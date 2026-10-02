// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [OpenWrt-порт, M4b] Profile pool daemon: `csqtt run`.
//!
//! Слои:
//! 1. [`ProfileState`] — state machine профилей (M4b требование 1).
//! 2. [`PoolManager`] — ЧИСТАЯ, детерминированная логика выбора/failover/
//!    failback/health. Время подаётся снаружы (`Instant`), I/O нет — все
//!    политики покрыты unit-тестами без сети и без реального времени.
//! 3. [`Daemon`] — драйвер на tokio: грузит UCI, ставит signal handlers
//!    (SIGTERM/SIGINT/SIGHUP), забирает события из лог-колбэка, дёргает
//!    менеджер, пишет status.json, запускает клиент через [`ClientRunner`].
//!
//! Контракт изоляции M3X (НЕ нарушается): служба CSQTT interface-only —
//! `install_routes=false`, `apply_dns=false` из `client_config_for`,
//! НИ ОДНОГО route/rule/table, WAN/DNS/firewall не трогаются. См. тест
//! `pool_config_is_interface_only`.

use crate::ClientConfig;
use crate::captcha_helper::HelperState;
use crate::captcha_manager::{
    CaptchaManager, CaptchaSolveRequest, SubmitOutcome, parse_captcha_solve_line,
    redact_session_token,
};
use crate::events::parse_event_line;
use crate::status::{
    CaptchaChallengeStatus, DEFAULT_STATUS_PATH, DaemonState, DaemonStatus, ProfileStatus,
    RoutingSummary, STATUS_WRITE_INTERVAL_SECS, TunnelStatus, WorkersStatus, epoch_secs,
    health_mode_str, selection_mode_str, write_status_atomic,
};
use crate::uci::{CaptchaPolicy, HealthMode, ProfilePool, SelectionMode, ServerProfile};
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub const DAEMON_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Минимальный graceful-период при SIGTERM/SIGINT (M4b требование 10).
pub const GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(3);

/// [M8-fix] Стартовая пауза для активного профиля: пока клиент в `Connecting`
/// меньше этого времени, отсутствие положительного транспорта НЕ считается
/// health-фейлом. Без паузы первый же тик интервала (`tokio::time::interval`
/// срабатывает сразу) вместе с `fail_threshold` отменял попытку на ~10с —
/// ровно до того, как медленный DNS (таймаут Yandex ~5с) и VK-auth успевали
/// выдать TURN-креды (~11с). Реальный дефект, найденный M8 e2e на WH3000 Pro.
pub const STARTUP_GRACE: Duration = Duration::from_secs(30);

// ===========================================================================
// 1. State machine профилей (M4b требование 1)
// ===========================================================================

/// Состояние профиля:
///
/// ```text
/// Disabled ──(reload/enabled=1)──► Standby ──(selected)──► Connecting
///                                       ▲                     │
///                                       │                     │ transport+data signal
///                                  Cooldown ◄── Failed          ▼
///                                       ▲        (fails≥threshold)   Ready ──success_threshold──► Active
///                                       │                              │            │
///                                       │                              ▼            ▼ FATAL_AUTH
///                                       └─── (captcha_policy=failover) CaptchaRequired, AuthFailed
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileState {
    /// `enabled=0` — никогда не выбирается.
    Disabled,
    /// Включён, ждёт выбора или повтора (reconnect_delay).
    #[default]
    Standby,
    /// Клиент запущен, положительный health-сигнал ещё не приходил.
    Connecting,
    /// Транспорт/данные есть (по health_mode), но success_threshold
    /// ещё не накоплен — ACTIVE это не означает (M4b требование 7).
    Ready,
    /// Здоров: накоплен success_threshold (M4b требование 3).
    Active,
    /// Достигнут fail_threshold (transient) — переход в Cooldown.
    Failed,
    /// Временная пауза после Failed; по истечении — Standby.
    Cooldown,
    /// FATAL_AUTH / битый профиль — не крутим циклом (M4b требование 3).
    AuthFailed,
    /// Требуется капча: failover переключает на следующий профиль,
    /// wait — остаёмся (daemon/status/UI живы). Точный lifecycle — M4d.
    CaptchaRequired,
}

impl ProfileState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Standby => "standby",
            Self::Connecting => "connecting",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::Failed => "failed",
            Self::Cooldown => "cooldown",
            Self::AuthFailed => "auth_failed",
            Self::CaptchaRequired => "captcha_required",
        }
    }

    fn runs_client(self) -> bool {
        matches!(self, Self::Connecting | Self::Ready | Self::Active)
    }
}

/// Runtime-состояние одного профиля.
#[derive(Clone, Debug)]
struct ProfileRuntime {
    state: ProfileState,
    /// Transient-неудачи подряд (network/TURN/timeout/data-health/client exit).
    consecutive_fails: u32,
    /// Здоровые health-тики подряд (для success_threshold).
    consecutive_successes: u32,
    /// Помечен permanent-ошибкой (FATAL_AUTH/битый профиль): не повторять.
    permanent: bool,
    cooldown_until: Option<Instant>,
    reconnect_after: Option<Instant>,
    /// Непрерывно здоров с этого момента (failback stable window).
    healthy_since: Option<Instant>,
    last_started_at: Option<Instant>,
    last_error: Option<String>,
    saw_transport_positive: bool,
    saw_data_positive: bool,
    stats_active: i64,
    stats_up: u64,
    stats_down: u64,
}

impl ProfileRuntime {
    fn new(profile: &ServerProfile) -> Self {
        Self {
            state: if profile.enabled {
                ProfileState::Standby
            } else {
                ProfileState::Disabled
            },
            consecutive_fails: 0,
            consecutive_successes: 0,
            permanent: false,
            cooldown_until: None,
            reconnect_after: None,
            healthy_since: None,
            last_started_at: None,
            last_error: None,
            saw_transport_positive: false,
            saw_data_positive: false,
            stats_active: 0,
            stats_up: 0,
            stats_down: 0,
        }
    }

    fn start_transient_failure(&mut self, now: Instant, reconnect: Duration) {
        self.consecutive_successes = 0;
        self.healthy_since = None;
        self.consecutive_fails += 1;
        self.reconnect_after = Some(now + reconnect);
    }

    fn finish_failure(&mut self, now: Instant, cooldown: Duration) {
        self.state = ProfileState::Failed;
        self.consecutive_successes = 0;
        self.healthy_since = None;
        self.cooldown_until = Some(now + cooldown);
    }

    fn record_permanent(&mut self, now: Instant, cooldown: Duration, message: String) {
        self.consecutive_successes = 0;
        self.healthy_since = None;
        self.permanent = true;
        self.last_error = Some(message);
        self.state = ProfileState::AuthFailed;
        self.cooldown_until = Some(now + cooldown);
    }

    fn record_success(&mut self, now: Instant) {
        self.consecutive_fails = 0;
        self.consecutive_successes += 1;
        if self.healthy_since.is_none() {
            self.healthy_since = Some(now);
        }
    }

    fn cooldown_remaining(&self, now: Instant) -> Duration {
        self.cooldown_until
            .map(|deadline| deadline.saturating_duration_since(now))
            .unwrap_or_default()
    }

    /// Состояние после учёта истёкших cooldownов (просмотр без мутации).
    fn effective_state(&self, now: Instant) -> ProfileState {
        match self.state {
            ProfileState::Failed | ProfileState::Cooldown
                if self.cooldown_until.is_some_and(|deadline| now >= deadline) =>
            {
                ProfileState::Standby
            }
            other => other,
        }
    }

    /// Ждёт повтора того же профиля после transient-ошибки (no switch):
    /// клиент не крутится в цикле, но и другие профили не перехватывают выбор.
    fn retry_pending(&self, now: Instant) -> bool {
        !self.permanent
            && self.effective_state(now) == ProfileState::Standby
            && self.reconnect_after.is_some()
    }
}

// ===========================================================================
// 2. Health inputs и классификация результатов клиента
// ===========================================================================

/// Сигналы здоровья от транспорта (через `CSQTT_EVENTS` из лог-колбэка)
/// и от health_target-зонда. Никаких секретов — только счётчики/коды.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HealthInput {
    /// Worker поднял транспорт (transport signal).
    Ready { worker: usize },
    /// Периодическая статистика (data signal: active/bytes).
    Stats {
        active: i64,
        bytes_up: i64,
        bytes_down: i64,
    },
    /// Подозрение на сетевой сбой (burst TURN timeouts).
    NetworkSuspect,
    /// Все соединения упали (data signal).
    ActiveZero,
    /// VK-вызов недоступен (data signal).
    CallUnavailable,
    /// TUNCONF от сервера: туннельный IP/DNS (transport signal).
    TunnelConfig { ip: String, dns: String },
    /// health_target-зонд прошёл (НЕ означает ACTIVE — M4b требование 7):
    /// сбрасывает подозрение, но не копит success.
    ProbeOk,
    /// health_target-зонд упал (transient signal).
    ProbeFailed,
}

impl HealthInput {
    fn is_negative(&self) -> bool {
        matches!(
            self,
            Self::NetworkSuspect | Self::ActiveZero | Self::CallUnavailable | Self::ProbeFailed
        )
    }
}

/// Классификация результата run_client для активного профиля.
#[derive(Clone, Debug, PartialEq, Eq)]
enum ClientOutcome {
    /// Отмена токеном/clean stop — не неудача.
    Stopped,
    /// FATAL_AUTH: пароль/устройство отклонены — не повторять в цикле.
    AuthFailed,
    /// Профиль требует капчу: политика решает, переключаться ли.
    CaptchaRequired,
    /// Прочая ошибка: transient (сеть/TURN/timeout/data) или
    /// ошибка конфигурации профиля (permanent — тоже не крутим).
    Failed { permanent: bool },
}

fn classify_client_result(result: &Result<()>, cancelled: bool) -> ClientOutcome {
    if cancelled {
        // Отмена токеном — clean shutdown даже при ошибке.
        return ClientOutcome::Stopped;
    }
    match result {
        Ok(()) => ClientOutcome::Stopped,
        Err(error) => {
            let message = format!("{error:#}");
            let lower = message.to_lowercase();
            if lower.contains("fatal_auth") {
                ClientOutcome::AuthFailed
            } else if lower.contains("captcha_wait_required") || lower.contains("captcha") {
                ClientOutcome::CaptchaRequired
            } else if lower.contains("нужны -peer")
                || lower.contains("нужен -password")
                || lower.contains("нет хешей vk")
            {
                ClientOutcome::Failed { permanent: true }
            } else {
                ClientOutcome::Failed { permanent: false }
            }
        }
    }
}

/// Команда менеджеру/драйверу. Внешние: Reload (SIGHUP), Health (events),
/// ResolveCaptcha, CaptchaRequest/SubmitCaptchaResult/CancelCaptcha (M4d),
/// HelperConsume/HelperStatus/CaptchaSnapshot/CaptchaCancel (M4e Web Helper).
/// Внутренние: HealthTick, WriteStatus, ClientFinished.
///
/// Debug — ручной: `SubmitCaptchaResult.result` (success token) и
/// `HelperConsume.token` (capability) — SECRET, в открытом виде не выводятся.
pub enum DaemonCommand {
    /// SIGHUP: перечитать UCI и сделать reconcile.
    Reload,
    /// Сигнал здоровья (из лог-колбэка или зонда).
    Health(HealthInput),
    /// M4d hook: капча решена/протухла — профиль обратно в Standby.
    ResolveCaptcha(String),
    /// M4d: перехваченный wire-запрос CAPTCHA_SOLVE от ядра (секреты —
    /// в CaptchaManager, в лог они не пишутся).
    CaptchaRequest { request: CaptchaSolveRequest },
    /// M4d: внешний помощник/оператор прислал результат challenge.
    SubmitCaptchaResult {
        challenge_id: String,
        result: String,
    },
    /// M4d: отменить challenge (оператор).
    CancelCaptcha(String),
    /// M4e Web Helper: погасить capability challenge (id+token связаны —
    /// сверяются в менеджере). Ответ — redirect_uri (SECRET) либо None
    /// (недействительная/просроченная/повторная capability).
    HelperConsume {
        challenge_id: String,
        token: String,
        reply: oneshot::Sender<Option<String>>,
    },
    /// M4e Web Helper: safe-состояние challenge для статус-опроса страницы.
    HelperStatus {
        challenge_id: String,
        reply: oneshot::Sender<Option<String>>,
    },
    /// M4e интеграционный API (M6/LuCI): снимок всех challenges (safe fields).
    CaptchaSnapshot {
        reply: oneshot::Sender<Vec<CaptchaSnapshotView>>,
    },
    /// M4e интеграционный API: отмена с подтверждением.
    CaptchaCancel {
        challenge_id: String,
        reply: oneshot::Sender<bool>,
    },
    /// Внутренний: health_interval тик.
    HealthTick,
    /// Внутренний: интервал записи status.json (~5с).
    WriteStatus,
    /// Внутренний: клиент завершился (supervisor-задача шлёт результат).
    ClientFinished {
        id: String,
        result: Result<()>,
        cancelled: bool,
    },
}

impl std::fmt::Debug for DaemonCommand {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Reload => formatter.write_str("Reload"),
            Self::Health(input) => formatter.debug_tuple("Health").field(input).finish(),
            Self::ResolveCaptcha(id) => formatter.debug_tuple("ResolveCaptcha").field(id).finish(),
            Self::CaptchaRequest { request } => formatter
                .debug_struct("CaptchaRequest")
                .field("request", request)
                .finish(),
            Self::SubmitCaptchaResult {
                challenge_id,
                result: _,
            } => formatter
                .debug_struct("SubmitCaptchaResult")
                .field("challenge_id", challenge_id)
                .field("result", &"<redacted>")
                .finish(),
            Self::CancelCaptcha(id) => formatter.debug_tuple("CancelCaptcha").field(id).finish(),
            Self::HelperConsume { challenge_id, .. } => formatter
                .debug_struct("HelperConsume")
                .field("challenge_id", challenge_id)
                .field("token", &"<redacted>")
                .finish(),
            Self::HelperStatus { challenge_id, .. } => formatter
                .debug_struct("HelperStatus")
                .field("challenge_id", challenge_id)
                .finish(),
            Self::CaptchaSnapshot { .. } => formatter.write_str("CaptchaSnapshot"),
            Self::CaptchaCancel { challenge_id, .. } => formatter
                .debug_struct("CaptchaCancel")
                .field("challenge_id", challenge_id)
                .finish(),
            Self::HealthTick => formatter.write_str("HealthTick"),
            Self::WriteStatus => formatter.write_str("WriteStatus"),
            Self::ClientFinished {
                id,
                result,
                cancelled,
            } => formatter
                .debug_struct("ClientFinished")
                .field("id", id)
                .field("result", result)
                .field("cancelled", cancelled)
                .finish(),
        }
    }
}

/// Safe-представление challenge для интеграционного API M4e (list/info):
/// только non-secret поля. `redirect_uri`/`session_token`/result здесь не
/// бывают — они живут в CaptchaManager и выдаются исключительно через
/// `HelperConsume` (по одноразовой capability).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptchaSnapshotView {
    pub id: String,
    pub profile_id: String,
    pub mode: String,
    pub state: String,
}

/// Команда драйверу: детерминирована (одинаковые входы → одинаковый выход),
/// основа no-flapping (M4b DoD). `config` в Box — enum не раздувается
/// (clippy::large_enum_variant).
#[derive(Clone, Debug)]
pub enum Action {
    /// Текущий активный профиль корректен — ничего не менять.
    Keep,
    /// Запустить клиент для профиля (драйвер остановит предыдущий).
    Start {
        id: String,
        config: Box<ClientConfig>,
    },
    /// Подходящих кандидатов нет (все в cooldown/AuthFailed/captcha-wait).
    /// Служба CSQTT живёт, пишет status, ждёт истечения cooldown.
    Stop,
}

/// Сравнение по профилю (конфиг не сравнивается — ClientConfig без Eq).
impl PartialEq for Action {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Keep, Self::Keep) | (Self::Stop, Self::Stop) => true,
            (Self::Start { id: left, .. }, Self::Start { id: right, .. }) => left == right,
            _ => false,
        }
    }
}

impl Eq for Action {}

// ===========================================================================
// 3. PoolManager — чистая логика
// ===========================================================================

/// Менеджер пула профилей. Все методы чистые: время — параметр, I/O,
/// таймеров и рандома нет — политики failover/failback/captcha покрыты
/// unit-тестами детерминированно.
pub struct PoolManager {
    pool: ProfilePool,
    runtimes: HashMap<String, ProfileRuntime>,
    /// Профиль, которым владеет текущая попытка (даже если клиент не
    /// работает — например, retry-pending или captcha-wait).
    active: Option<String>,
    tunnel_ip: Option<String>,
    tunnel_dns: Option<String>,
    started_at: Instant,
    reconnects: u64,
}

impl PoolManager {
    pub fn new(pool: ProfilePool, now: Instant) -> Self {
        let runtimes = pool
            .servers
            .iter()
            .map(|profile| (profile.section_id.clone(), ProfileRuntime::new(profile)))
            .collect();
        Self {
            pool,
            runtimes,
            active: None,
            tunnel_ip: None,
            tunnel_dns: None,
            started_at: now,
            reconnects: 0,
        }
    }

    pub fn selection_mode(&self) -> SelectionMode {
        self.pool.main.selection_mode
    }

    pub fn active_id(&self) -> Option<&str> {
        self.active.as_deref()
    }

    /// Кандидаты в порядке выбора: priority по возрастанию, tie-break —
    /// UCI order (правило 2 UCI-схемы). Disabled отфильтрованы.
    fn candidate_profiles(&self) -> Vec<&ServerProfile> {
        self.pool.ordered_enabled_servers()
    }

    /// Можно ли запустить профиль прямо сейчас: состояние выбираемое,
    /// cooldown и reconnect_delay истекли, не permanent.
    fn eligible(&self, id: &str, now: Instant) -> bool {
        let Some(runtime) = self.runtimes.get(id) else {
            return false;
        };
        if runtime.permanent {
            return false;
        }
        if !matches!(
            runtime.effective_state(now),
            ProfileState::Standby | ProfileState::Ready | ProfileState::Active
        ) {
            return false;
        }
        if let Some(deadline) = runtime.reconnect_after
            && now < deadline
        {
            return false;
        }
        true
    }

    /// Главный выбор. Детерминирован: при одинаковых (pool, runtimes, now)
    /// всегда одно и то же решение — основа no-flapping.
    pub fn next_action(&self, now: Instant) -> Action {
        if !self.pool.main.enabled {
            return Action::Stop;
        }
        match self.selection_mode() {
            SelectionMode::Manual => self.manual_action(now),
            SelectionMode::Priority => self.priority_action(now),
        }
    }

    /// Manual: только active_profile. Failover на другие профили НЕ уходит —
    /// оператор явно закрепил профиль (M4b требование 2); retry того же
    /// профиля после transient-ошибки работает. CAPTCHA_REQUIRED (M4d) также
    /// освобождает слот: профиль ждёт в CaptchaRequired до истечения
    /// cooldown, потом перезапускается (другие профили не трогает).
    fn manual_action(&self, now: Instant) -> Action {
        let target = self.pool.main.active_profile.as_str();
        if target.is_empty() {
            return Action::Stop;
        }
        let Some(runtime) = self.runtimes.get(target) else {
            return Action::Stop;
        };
        let state = runtime.effective_state(now);
        if self.active.as_deref() == Some(target) {
            if state.runs_client() {
                return Action::Keep;
            }
            if runtime.retry_pending(now) {
                // Тот же профиль, повтор после reconnect_delay — NO SWITCH.
                return if self.eligible(target, now) {
                    self.start_action(target)
                } else {
                    Action::Keep
                };
            }
        }
        if self.eligible(target, now) {
            self.start_action(target)
        } else {
            Action::Stop
        }
    }

    /// Priority: включённые по priority; retry того же профиля под threshold;
    /// failover после threshold; failback — на лучший приоритет при stable.
    fn priority_action(&self, now: Instant) -> Action {
        // Активный профиль проверяем ПЕРВЫМ: клиент на нём уже работает —
        // никаких переключений на кандидатов с лучшим priority (no flapping).
        if let Some(active) = self.active.as_deref()
            && let Some(runtime) = self.runtimes.get(active)
        {
            let state = runtime.effective_state(now);
            if state.runs_client() {
                // Failback (требование 4) имеет приоритет над Keep:
                // активный backup здоров непрерывно failback_stable_time И
                // есть лучший priority-профиль eligible — возвращаемся.
                // failback=0 — failback_target() вернёт None → остаёмся.
                if self.pool.main.failback
                    && let Some(target_id) = self.failback_target(now)
                    && self.eligible(target_id, now)
                {
                    return self.start_action(target_id);
                }
                return Action::Keep;
            }
            if runtime.retry_pending(now) {
                // NO SWITCH: переподключаем тот же профиль, не backup.
                return if self.eligible(active, now) {
                    self.start_action(active)
                } else {
                    Action::Keep
                };
            }
            // CaptchaRequired/Failed/Cooldown/AuthFailed — берём следующего
            // кандидата: CAPTCHA_REQUIRED не блокирует переход к B (M4d
            // требование 8 — политика wait влияет только на момент отмены
            // ждущего клиента, не на освобождение слота).
        }
        for profile in &self.candidate_profiles() {
            let id = profile.section_id.as_str();
            if self.eligible(id, now) {
                return self.start_action(id);
            }
        }
        // Никого не выбрали: активный мог остаться (captcha-wait) — Keep;
        // иначе кандидатов нет — Stop (все cooldown/AuthFailed).
        if self.active.is_some() {
            Action::Keep
        } else {
            Action::Stop
        }
    }

    fn start_action(&self, id: &str) -> Action {
        let Some(profile) = self.pool.server(id) else {
            return Action::Stop;
        };
        Action::Start {
            id: id.to_string(),
            config: Box::new(self.pool.client_config_for(profile)),
        }
    }

    /// Лучший priority-кандидат для failback: активный профиль здоров
    /// непрерывно failback_stable_time, и есть eligible профиль с лучшим
    /// priority. Возвращает id только если активный не лучший по приоритету.
    fn failback_target(&self, now: Instant) -> Option<&str> {
        let active = self.active.as_deref()?;
        let active_runtime = self.runtimes.get(active)?;
        if !matches!(
            active_runtime.state,
            ProfileState::Ready | ProfileState::Active
        ) {
            return None;
        }
        let stable_since = active_runtime.healthy_since?;
        if now.saturating_duration_since(stable_since) < self.stable_window() {
            return None;
        }
        let active_key = self
            .pool
            .server(active)
            .map(|profile| (profile.priority, profile.order))?;
        self.candidate_profiles()
            .iter()
            .find(|profile| {
                profile.section_id != active && (profile.priority, profile.order) < active_key
            })
            .map(|profile| profile.section_id.as_str())
    }

    fn stable_window(&self) -> Duration {
        Duration::from_secs(self.pool.main.failback_stable_time.max(1))
    }

    // --- События ---

    /// Сигнал здоровья для активного профиля.
    pub fn on_health(&mut self, input: &HealthInput, now: Instant) {
        let active = match self.active.clone() {
            Some(id) => id,
            None => return,
        };
        // Эффективные параметры вычисляем ДО мутабельного borrow runtime.
        let cooldown = self.effective_cooldown_for(&active);
        let reconnect = self.effective_reconnect_delay_for(&active);
        let threshold = self.effective_fail_threshold_for(&active);
        let Some(runtime) = self.runtimes.get_mut(&active) else {
            return;
        };
        match input {
            HealthInput::TunnelConfig { ip, dns } => {
                self.tunnel_ip = Some(ip.clone());
                self.tunnel_dns = Some(dns.clone());
                runtime.saw_transport_positive = true;
            }
            HealthInput::Stats {
                active,
                bytes_up,
                bytes_down,
            } => {
                runtime.stats_active = *active;
                runtime.stats_up = runtime.stats_up.saturating_add((*bytes_up).max(0) as u64);
                runtime.stats_down = runtime
                    .stats_down
                    .saturating_add((*bytes_down).max(0) as u64);
                if *active > 0 || *bytes_down > 0 {
                    runtime.saw_data_positive = true;
                }
            }
            HealthInput::Ready { .. } => runtime.saw_transport_positive = true,
            HealthInput::ProbeOk => {}
            _ => {}
        }
        let mut finished = false;
        if input.is_negative()
            && !matches!(
                runtime.state,
                ProfileState::AuthFailed | ProfileState::CaptchaRequired
            )
        {
            runtime.start_transient_failure(now, reconnect);
            if runtime.consecutive_fails >= threshold {
                runtime.finish_failure(now, cooldown);
                finished = true;
            }
        }
        // Порог достигнут — активная попытка больше не валидна: следующий
        // выбор возьмёт другой профиль (no switch под порогом).
        if finished {
            self.active = None;
        }
    }

    /// Результат run_client для профиля (драйвер зовёт после завершения).
    pub fn on_client_result(
        &mut self,
        id: &str,
        result: &Result<()>,
        cancelled: bool,
        now: Instant,
    ) {
        let outcome = classify_client_result(result, cancelled);
        // Эффективные параметры — до мутабельного borrow runtime.
        let cooldown = self.effective_cooldown_for(id);
        let reconnect = self.effective_reconnect_delay_for(id);
        let threshold = self.effective_fail_threshold_for(id);
        let outcome = outcome.clone();
        let Some(runtime) = self.runtimes.get_mut(id) else {
            return;
        };
        match outcome {
            ClientOutcome::Stopped => {
                // Отмена/clean stop: состояние не сбрасываем (cancel —
                // следствие решения менеджера, не новая информация).
                // Clean exit без отмены — профиль свободен, retry после
                // reconnect_delay (тот же профиль, без switch).
                if !cancelled && !runtime.permanent {
                    runtime.state = ProfileState::Standby;
                    runtime.reconnect_after = Some(now + reconnect);
                }
            }
            ClientOutcome::AuthFailed => {
                runtime.record_permanent(
                    now,
                    cooldown,
                    "FATAL_AUTH: сервер отклонил пароль/устройство".to_string(),
                );
            }
            ClientOutcome::CaptchaRequired => {
                // [M4d] lifecycle: CAPTCHA_REQUIRED всегда освобождает активный
                // слот — следующий профиль стартует (требование 8). Политика
                // wait/failover отличается только моментом отмены ждущего
                // клиента (служба CSQTT отменяет сразу при failover и ждёт TTL при
                // wait), а не тем, держим ли мы слот.
                runtime.state = ProfileState::CaptchaRequired;
                runtime.last_error =
                    Some("требуется капча (challenge в CaptchaManager)".to_string());
                runtime.cooldown_until = Some(now + cooldown);
                self.active = None;
                return;
            }
            ClientOutcome::Failed { permanent } => {
                let message = match result.as_ref() {
                    Ok(()) => "клиент завершился без ошибки".to_string(),
                    Err(error) => format!("{error:#}"),
                };
                if permanent {
                    runtime.record_permanent(now, cooldown, message);
                } else {
                    runtime.start_transient_failure(now, reconnect);
                    runtime.last_error = Some(message);
                    if runtime.consecutive_fails >= threshold {
                        runtime.finish_failure(now, cooldown);
                        self.active = None;
                        return;
                    } else {
                        // Под threshold: retry того же профиля — NO SWITCH.
                        runtime.state = ProfileState::Standby;
                    }
                }
            }
        }
        if self.active.as_deref() == Some(id)
            && !matches!(runtime.state, ProfileState::Ready | ProfileState::Active)
            && !runtime.retry_pending(now)
        {
            self.active = None;
        }
    }

    /// Health-тик (раз в health_interval): оцениваем здоровье активного
    /// профиля, копим success/fail, сбрасываем истёкшие cooldowns.
    pub fn on_tick(&mut self, now: Instant) {
        for (id, runtime) in self.runtimes.iter_mut() {
            // Сброс истёкших cooldownов для неактивных профилей.
            if Some(id.as_str()) == self.active.as_deref() {
                continue;
            }
            if matches!(
                runtime.state,
                ProfileState::Failed | ProfileState::Cooldown | ProfileState::CaptchaRequired
            ) && runtime
                .cooldown_until
                .is_some_and(|deadline| now >= deadline)
            {
                runtime.state = ProfileState::Standby;
                runtime.cooldown_until = None;
                runtime.reconnect_after = None;
            }
        }
        let active = match self.active.clone() {
            Some(id) => id,
            None => return,
        };
        // Эффективные параметры — до мутабельного borrow runtime.
        let mode = self.pool.main.health_mode;
        let success_threshold = self.effective_success_threshold_for(&active);
        let cooldown = self.effective_cooldown_for(&active);
        let reconnect = self.effective_reconnect_delay_for(&active);
        let threshold = self.effective_fail_threshold_for(&active);
        let Some(runtime) = self.runtimes.get_mut(&active) else {
            return;
        };
        if !runtime.state.runs_client() {
            return;
        }
        // [M8] `saw_transport_positive` выставляется разовым READY/TUNCONF, а
        // тик его сбрасывает. Живые соединения в интервале (Stats.active > 0)
        // — тоже положительный транспортный сигнал; иначе health_mode=both
        // навсегда застревает в transient-fail при живом туннеле с трафиком
        // (живой дефект, найденный M8 e2e на WH3000 Pro).
        let transport_ok =
            runtime.saw_transport_positive || runtime.stats_active > 0 || mode == HealthMode::Data;
        let data_ok = runtime.saw_data_positive || mode == HealthMode::Transport;
        runtime.saw_transport_positive = false;
        runtime.saw_data_positive = false;
        let mut finished = false;
        if transport_ok && data_ok {
            runtime.record_success(now);
            if runtime.consecutive_successes >= success_threshold {
                runtime.state = ProfileState::Active;
                runtime.last_error = None;
            } else {
                runtime.state = ProfileState::Ready;
            }
        } else if runtime.state == ProfileState::Connecting
            && runtime
                .last_started_at
                .is_some_and(|started| now.saturating_duration_since(started) < STARTUP_GRACE)
        {
            // [M8-fix] Стартовое окно: клиент ещё резолвит DNS и проходит
            // VK-auth (на практике до ~11с). Пока grace не истёк, тик без
            // положительного транспорта — не фейл, иначе попытка отменяется
            // до готовности (см. STARTUP_GRACE).
        } else if runtime.state == ProfileState::Connecting
            && mode != HealthMode::Transport
            && !data_ok
        {
            // Connecting, а данных по-прежнему нет — data-health fail.
            runtime.start_transient_failure(now, reconnect);
            if runtime.consecutive_fails >= threshold {
                runtime.finish_failure(now, cooldown);
                finished = true;
            }
        } else {
            // Ready/Active деградировал — transient fail (без cooldown,
            // пока не достигнут threshold).
            runtime.start_transient_failure(now, reconnect);
            if runtime.consecutive_fails >= threshold {
                runtime.finish_failure(now, cooldown);
                finished = true;
            }
        }
        if finished {
            self.active = None;
        }
    }

    /// SIGHUP: reload config + reconcile (M4b требование 10):
    /// removed active profile, changed priority, selection_mode/active_profile,
    /// settings changed. Счётчики переносятся на совпадающие id.
    pub fn reload(&mut self, pool: ProfilePool, now: Instant) {
        let mut new_runtimes = HashMap::new();
        for profile in &pool.servers {
            let mut runtime = ProfileRuntime::new(profile);
            if let Some(previous) = self.runtimes.get(&profile.section_id) {
                runtime.consecutive_fails = previous.consecutive_fails;
                runtime.consecutive_successes = previous.consecutive_successes;
                runtime.cooldown_until = previous.cooldown_until;
                runtime.reconnect_after = previous.reconnect_after;
                runtime.stats_active = previous.stats_active;
                runtime.stats_up = previous.stats_up;
                runtime.stats_down = previous.stats_down;
                runtime.permanent = previous.permanent;
                runtime.last_error = previous.last_error.clone();
                runtime.healthy_since = previous.healthy_since;
                runtime.state = if !profile.enabled {
                    ProfileState::Disabled
                } else if previous.permanent {
                    ProfileState::AuthFailed
                } else {
                    previous.effective_state(now)
                };
            }
            new_runtimes.insert(profile.section_id.clone(), runtime);
        }
        // Активный профиль удалён/выключен — остановим, выбор пересчитается.
        if let Some(active) = self.active.as_ref()
            && !pool.server(active).is_some_and(|profile| profile.enabled)
        {
            self.active = None;
        }
        // Reconcile выбора: если активный профиль больше не является
        // результатом выбора по новому конфигу (changed priority /
        // selection_mode / active_profile) — останавливаем его,
        // next_action запустит новый лучший. Если он остался лучшим —
        // клиент не дёргается (no unnecessary switch).
        if let Some(active) = self.active.as_ref() {
            let still_selected = match pool.main.selection_mode {
                SelectionMode::Manual => active.as_str() == pool.main.active_profile.as_str(),
                SelectionMode::Priority => pool
                    .ordered_enabled_servers()
                    .first()
                    .is_some_and(|profile| profile.section_id.as_str() == active.as_str()),
            };
            if !still_selected {
                self.active = None;
            }
        }
        self.pool = pool;
        self.runtimes = new_runtimes;
    }

    /// Отметить старт клиента для профиля (драйвер — после spawn).
    /// Метрики здоровья сбрасываются: новая попытка должна заново накопить
    /// success_threshold и stable window (no-flapping, M4b DoD).
    /// `consecutive_fails` НЕ трогаем — retry-threshold семантика опирается на него.
    pub fn on_client_started(&mut self, id: &str, now: Instant) {
        self.active = Some(id.to_string());
        self.reconnects += 1;
        if let Some(runtime) = self.runtimes.get_mut(id) {
            runtime.state = ProfileState::Connecting;
            runtime.last_started_at = Some(now);
            runtime.saw_transport_positive = false;
            runtime.saw_data_positive = false;
            runtime.reconnect_after = None;
            runtime.healthy_since = None;
            runtime.consecutive_successes = 0;
        }
    }

    /// M4b API (требование 6): сброс captcha-состояния профиля —
    /// капча решена/протухла. Возвращает профиль в Standby.
    pub fn resolve_captcha(&mut self, id: &str) {
        if let Some(runtime) = self.runtimes.get_mut(id)
            && runtime.state == ProfileState::CaptchaRequired
        {
            runtime.state = ProfileState::Standby;
            runtime.cooldown_until = None;
            runtime.last_error = None;
        }
    }

    /// [M4d] Профиль требует капчу, которую сейчас не решить: переводит в
    /// CaptchaRequired с cooldown и ОСВОБОЖДАЕТ активный слот — следующий
    /// профиль стартует (требование 8). Используется службой CSQTT в момент
    /// wire-запроса при policy=failover, при истечении challenge и при
    /// отмене. Idempotent: не сбрасывает уже назначенный cooldown.
    pub fn mark_captcha_required(&mut self, id: &str, now: Instant) {
        let cooldown = self.effective_cooldown_for(id);
        let Some(runtime) = self.runtimes.get_mut(id) else {
            return;
        };
        runtime.state = ProfileState::CaptchaRequired;
        runtime.consecutive_successes = 0;
        runtime.healthy_since = None;
        runtime.last_error = Some("требуется капча (challenge в CaptchaManager)".to_string());
        if runtime.cooldown_until.is_none() {
            runtime.cooldown_until = Some(now + cooldown);
        }
        if self.active.as_deref() == Some(id) {
            self.active = None;
        }
    }

    pub fn captcha_pending(&self) -> u64 {
        self.runtimes
            .values()
            .filter(|runtime| runtime.state == ProfileState::CaptchaRequired)
            .count() as u64
    }

    // --- Effective overrides (M4b требование 5) ---

    fn effective_fail_threshold_for(&self, id: &str) -> u32 {
        self.pool
            .server(id)
            .map(|profile| self.pool.effective_fail_threshold(profile))
            .unwrap_or(self.pool.main.fail_threshold)
    }

    fn effective_success_threshold_for(&self, id: &str) -> u32 {
        self.pool
            .server(id)
            .map(|profile| self.pool.effective_success_threshold(profile))
            .unwrap_or(self.pool.main.success_threshold)
    }

    fn effective_cooldown_for(&self, id: &str) -> Duration {
        Duration::from_secs(
            self.pool
                .server(id)
                .map(|profile| self.pool.effective_cooldown(profile))
                .unwrap_or(self.pool.main.cooldown),
        )
    }

    fn effective_reconnect_delay_for(&self, _id: &str) -> Duration {
        Duration::from_secs(self.pool.main.reconnect_delay)
    }

    fn effective_captcha_policy_for(&self, id: &str) -> CaptchaPolicy {
        self.pool
            .server(id)
            .map(|profile| self.pool.effective_captcha_policy(profile))
            .unwrap_or(self.pool.main.captcha_policy)
    }

    // --- Снимок для status.json ---

    pub fn snapshot(
        &self,
        now: Instant,
        daemon_state: DaemonState,
        captcha_challenges: Vec<CaptchaChallengeStatus>,
    ) -> DaemonStatus {
        let secrets = self.pool.collect_secrets();
        let profiles = self
            .pool
            .servers
            .iter()
            .map(|profile| {
                let runtime = self.runtimes.get(&profile.section_id);
                ProfileStatus {
                    id: profile.section_id.clone(),
                    name: profile.name.clone(),
                    state: runtime
                        .map(|runtime| runtime.effective_state(now).as_str())
                        .unwrap_or(ProfileState::Disabled.as_str())
                        .to_string(),
                    priority: profile.priority,
                    enabled: profile.enabled,
                    consecutive_fails: runtime.map(|r| r.consecutive_fails).unwrap_or_default(),
                    success_streak: runtime.map(|r| r.consecutive_successes).unwrap_or_default(),
                    cooldown_remaining_secs: runtime
                        .map(|r| r.cooldown_remaining(now).as_secs())
                        .unwrap_or_default(),
                }
            })
            .collect();
        let active_runtime = self.active.as_ref().and_then(|id| self.runtimes.get(id));
        // Последняя ошибка любого профиля (обезвреженная от секретов).
        let last_error = self
            .runtimes
            .values()
            .collect::<Vec<_>>()
            .iter()
            .rev()
            .find_map(|runtime| runtime.last_error.clone())
            .map(|message| crate::uci::redact_secrets(&message, &secrets));
        DaemonStatus {
            version: DAEMON_VERSION.to_string(),
            daemon_state: daemon_state.as_str().to_string(),
            started_at: epoch_secs().saturating_sub(now.duration_since(self.started_at).as_secs()),
            generated_at: epoch_secs(),
            uptime_secs: now.duration_since(self.started_at).as_secs(),
            selection_mode: selection_mode_str(self.selection_mode()).to_string(),
            active_profile: self.active.clone(),
            profiles,
            tunnel: TunnelStatus {
                interface: crate::uci::TUN_INTERFACE.to_string(),
                address: self.tunnel_ip.clone(),
                local_address: if self.pool.main.tun_address.is_empty() {
                    None
                } else {
                    Some(self.pool.main.tun_address.clone())
                },
                mtu: self.pool.main.tun_mtu,
                dns: self.tunnel_dns.clone(),
            },
            routing: RoutingSummary::from_mode(self.pool.routing.mode),
            workers: WorkersStatus {
                configured: self
                    .active
                    .as_deref()
                    .and_then(|id| self.pool.server(id))
                    .map(|profile| crate::normalized_workers(profile.workers))
                    .unwrap_or_default(),
                active: active_runtime.map(|r| r.stats_active).unwrap_or_default(),
            },
            rx_bytes: active_runtime.map(|r| r.stats_down).unwrap_or_default(),
            tx_bytes: active_runtime.map(|r| r.stats_up).unwrap_or_default(),
            reconnects: self.reconnects,
            captcha_pending: self.captcha_pending(),
            captcha_challenges,
            last_error,
            health_mode: health_mode_str(self.pool.main.health_mode).to_string(),
            health_target: self.pool.main.health_target.clone(),
        }
    }
}

// ===========================================================================
// 4. Daemon — драйвер
// ===========================================================================

/// Абстракция запуска клиента: реальная реализация зовёт `run_client`,
/// тестовая — имитация. Позволяет гонять daemon-loop без сети/TUN.
pub trait ClientRunner: Send + Sync {
    fn run(
        &self,
        config: ClientConfig,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>>;
}

/// Реальный runner: ядро csqtt, interface-only конфиг из UCI.
pub struct CoreClientRunner;

impl ClientRunner for CoreClientRunner {
    fn run(
        &self,
        config: ClientConfig,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
        Box::pin(async move { crate::run_client(config, Some(cancel)).await })
    }
}

/// Клиент под наблюдением: токен отмены + supervisor-задача.
struct ClientHandle {
    token: CancellationToken,
    supervisor: tokio::task::JoinHandle<()>,
}

#[derive(Clone)]
pub struct DaemonOptions {
    /// Путь к UCI-конфигу (`/etc/config/csqtt`).
    pub config_path: PathBuf,
    /// Путь к status.json (`/var/run/csqtt/status.json`).
    pub status_path: PathBuf,
    /// Лог-файл (по умолчанию main.log_file, иначе /var/log/csqtt.log).
    pub log_file: Option<PathBuf>,
    /// Тестовый/альтернативный runner.
    pub runner: Arc<dyn ClientRunner>,
    /// CancellationToken родителя (procd). None → создаётся свой.
    pub cancel: Option<CancellationToken>,
    /// [M4e] Состояние Web Helper (LAN-сервер капчи). None — helper выключен.
    pub helper: Option<Arc<HelperState>>,
}

impl std::fmt::Debug for DaemonOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DaemonOptions")
            .field("config_path", &self.config_path)
            .field("status_path", &self.status_path)
            .field("log_file", &self.log_file)
            .finish_non_exhaustive()
    }
}

impl Default for DaemonOptions {
    fn default() -> Self {
        Self {
            config_path: PathBuf::from("/etc/config/csqtt"),
            status_path: PathBuf::from(DEFAULT_STATUS_PATH),
            log_file: None,
            runner: Arc::new(CoreClientRunner),
            cancel: None,
            helper: None,
        }
    }
}

/// Драйвер службы CSQTT. Живёт, пока не отменят или пока конфиг не скажет стоп.
pub struct Daemon {
    manager: PoolManager,
    options: DaemonOptions,
    cancel: CancellationToken,
    state: DaemonState,
    pool: ProfilePool,
    log_sink: Option<Arc<crate::logsink::FileLogSink>>,
    /// [M4d] Challenges капчи: секреты изолированы в памяти менеджера.
    captcha: CaptchaManager,
    /// [M4e] Web Helper (LAN-сервер): push capability для helper-url API.
    helper: Option<Arc<HelperState>>,
    /// Sender для supervisor-задач (ClientFinished) и внутренних команд.
    sender: mpsc::UnboundedSender<DaemonCommand>,
}

impl Daemon {
    /// Создать драйвер: читает UCI, строит менеджер и лог-синк.
    /// Возвращает драйвер, канал команд (внешние: SIGHUP/events/M4d) и
    /// приёмник, который надо передать в [`Daemon::run`].
    pub fn new(
        options: DaemonOptions,
        now: Instant,
    ) -> Result<(
        Self,
        mpsc::UnboundedSender<DaemonCommand>,
        mpsc::UnboundedReceiver<DaemonCommand>,
    )> {
        let text = std::fs::read_to_string(&options.config_path)
            .with_context(|| format!("чтение {}", options.config_path.display()))?;
        let pool = Self::parse_pool(&text)?;
        let cancel = options.cancel.clone().unwrap_or_default();
        let helper = options.helper.clone();
        let (tx, rx) = mpsc::unbounded_channel::<DaemonCommand>();
        let mut daemon = Self {
            manager: PoolManager::new(pool.clone(), now),
            options,
            cancel,
            state: DaemonState::Starting,
            pool,
            log_sink: None,
            captcha: CaptchaManager::new(),
            helper,
            sender: tx.clone(),
        };
        daemon.install_log_sink();
        Ok((daemon, tx, rx))
    }

    fn parse_pool(text: &str) -> Result<ProfilePool> {
        match ProfilePool::from_text(text) {
            Ok(pool) => Ok(pool),
            Err(issues) => Err(anyhow::anyhow!(
                "ошибки конфигурации ({}): {}",
                issues.len(),
                issues
                    .first()
                    .map(|issue| issue.to_string())
                    .unwrap_or_default()
            )),
        }
    }

    fn install_log_sink(&mut self) {
        let path = self
            .options
            .log_file
            .clone()
            .or_else(|| {
                if self.pool.main.log_file.is_empty() {
                    None
                } else {
                    Some(PathBuf::from(&self.pool.main.log_file))
                }
            })
            .unwrap_or_else(|| PathBuf::from("/var/log/csqtt.log"));
        let sink = Arc::new(crate::logsink::FileLogSink::new(
            path,
            self.pool.main.log_size_kb.saturating_mul(1024),
            self.pool.collect_secrets(),
        ));
        self.log_sink = Some(sink);
    }

    /// Файловый лог-синк для `install_event_pipeline` (redaction секретов
    /// + парсинг событий) — вызывается до `run`.
    pub fn log_sink(&self) -> Option<Arc<crate::logsink::FileLogSink>> {
        self.log_sink.clone()
    }

    /// Главный цикл. SIGTERM/SIGINT → cancel (graceful ≤3с), SIGHUP → reload.
    /// Завершение клиента приходит через тот же канал команд (supervisor
    /// задача шлёт ClientFinished) — никаких join-handle borrow в select.
    pub async fn run(mut self, mut commands: mpsc::UnboundedReceiver<DaemonCommand>) -> Result<()> {
        let cancel = self.cancel.clone();
        let mut health_ticker =
            tokio::time::interval(Duration::from_secs(self.pool.main.health_interval.max(1)));
        let mut status_ticker =
            tokio::time::interval(Duration::from_secs(STATUS_WRITE_INTERVAL_SECS));
        let mut client: Option<ClientHandle> = None;
        self.state = DaemonState::Running;
        self.apply_action(Instant::now(), &mut client).await;
        loop {
            let command = tokio::select! {
                biased;
                _ = cancel.cancelled() => break,
                cmd = commands.recv() => match cmd {
                    Some(cmd) => cmd,
                    None => break,
                },
                _ = health_ticker.tick() => DaemonCommand::HealthTick,
                _ = status_ticker.tick() => DaemonCommand::WriteStatus,
            };
            match command {
                DaemonCommand::Reload => {
                    self.state = DaemonState::Reloading;
                    match self.reload_from_file(Instant::now()) {
                        Ok(()) => {
                            crate::log_error!(
                                "[ПУЛ] Конфигурация перезагружена, reconcile выполнен"
                            )
                        }
                        Err(error) => {
                            crate::log_error!("[ПУЛ] Ошибка reload: {error:#} — работаю со старой")
                        }
                    }
                    self.state = DaemonState::Running;
                    let now = Instant::now();
                    self.apply_action(now, &mut client).await;
                }
                DaemonCommand::Health(input) => {
                    let now = Instant::now();
                    // [M4d] Активный профиль ждёт решения капчи (live
                    // challenge): отрицательные health-сигналы не копим —
                    // клиент не сломан, он ждёт человека/помощника.
                    let active = self.manager.active_id().map(str::to_owned);
                    let captcha_live = active
                        .as_deref()
                        .is_some_and(|profile| self.captcha.live_for_profile(profile));
                    if !(captcha_live && input.is_negative()) {
                        self.manager.on_health(&input, now);
                    }
                    if input.is_negative() && !captcha_live {
                        self.apply_action(now, &mut client).await;
                    }
                }
                DaemonCommand::HealthTick => {
                    let now = Instant::now();
                    let active = self.manager.active_id().map(str::to_owned);
                    let captcha_live = active
                        .as_deref()
                        .is_some_and(|profile| self.captcha.live_for_profile(profile));
                    if !captcha_live {
                        self.manager.on_tick(now);
                    }
                    self.sweep_captchas(now, &mut client);
                    self.apply_action(now, &mut client).await;
                }
                DaemonCommand::ResolveCaptcha(id) => {
                    self.manager.resolve_captcha(&id);
                    self.apply_action(Instant::now(), &mut client).await;
                }
                DaemonCommand::CaptchaRequest { request } => {
                    self.handle_captcha_request(request, &mut client).await;
                }
                DaemonCommand::SubmitCaptchaResult {
                    challenge_id,
                    result,
                } => {
                    self.handle_captcha_result(&challenge_id, &result, &mut client)
                        .await;
                }
                DaemonCommand::CancelCaptcha(challenge_id) => {
                    if self.captcha.cancel(&challenge_id) {
                        crate::log_error!("[КАПЧА] challenge {challenge_id} отменён");
                        if let Some(profile) = self.captcha.profile_of(&challenge_id) {
                            self.give_up_captcha(&profile, Instant::now(), &mut client);
                        }
                        self.apply_action(Instant::now(), &mut client).await;
                        self.write_status(Instant::now());
                    } else {
                        crate::log_error!(
                            "[КАПЧА] отмена невозможна: нет живого challenge {challenge_id}"
                        );
                    }
                }
                DaemonCommand::HelperConsume {
                    challenge_id,
                    token,
                    reply,
                } => {
                    // Capability одноразовая (M4d): consume_capability находит
                    // challenge по SHA-256 токена и проверяет used/TTL/state.
                    // id из URL обязан совпасть с id challenge — иначе это
                    // попытка подставить чужой challenge.
                    let now = Instant::now();
                    let redirect = match self.captcha.consume_capability(&token, now) {
                        Some((id, secret))
                            if id == challenge_id
                                && crate::captcha_helper::captcha_uri_allowed(
                                    &secret.redirect_uri,
                                ) =>
                        {
                            Some(secret.redirect_uri)
                        }
                        Some((id, _)) if id == challenge_id => {
                            // redirect_uri вне штатного VK/OK flow (порт
                            // CaptchaUriPolicy) — 302 запрещён, capability
                            // уже погашена (fail-closed).
                            crate::log_error!(
                                "[HELPER] challenge {challenge_id}: redirect_uri отклонён политикой доменов"
                            );
                            None
                        }
                        Some((id, _)) => {
                            crate::log_error!(
                                "[HELPER] capability challenge {challenge_id} не совпал с токеном ({id}) — отклонено"
                            );
                            None
                        }
                        None => None,
                    };
                    if redirect.is_none() {
                        crate::log_error!(
                            "[HELPER] capability challenge {challenge_id} отклонена (replay/expiry/invalid)"
                        );
                    }
                    let _ = reply.send(redirect);
                }
                DaemonCommand::HelperStatus {
                    challenge_id,
                    reply,
                } => {
                    let state = self
                        .captcha
                        .view(&challenge_id)
                        .map(|view| view.state.as_str().to_string());
                    let _ = reply.send(state);
                }
                DaemonCommand::CaptchaSnapshot { reply } => {
                    let views = self
                        .captcha
                        .views()
                        .iter()
                        .map(|view| CaptchaSnapshotView {
                            id: view.id.clone(),
                            profile_id: view.profile_id.clone(),
                            mode: view.mode.clone(),
                            state: view.state.as_str().to_string(),
                        })
                        .collect();
                    let _ = reply.send(views);
                }
                DaemonCommand::CaptchaCancel {
                    challenge_id,
                    reply,
                } => {
                    let now = Instant::now();
                    let cancelled = self.captcha.cancel(&challenge_id);
                    if cancelled {
                        if let Some(profile) = self.captcha.profile_of(&challenge_id) {
                            self.give_up_captcha(&profile, now, &mut client);
                        }
                        self.apply_action(now, &mut client).await;
                        self.write_status(Instant::now());
                    }
                    let _ = reply.send(cancelled);
                }
                DaemonCommand::WriteStatus => {
                    self.write_status(Instant::now());
                }
                DaemonCommand::ClientFinished {
                    id,
                    result,
                    cancelled,
                } => {
                    let now = Instant::now();
                    let outcome = classify_client_result(&result, cancelled);
                    let message = match result.as_ref() {
                        Ok(()) => "ok".to_string(),
                        Err(error) => format!("{error:#}"),
                    };
                    self.manager.on_client_result(&id, &result, cancelled, now);
                    crate::log_error!(
                        "[ПУЛ] Профиль {id} завершён (cancelled={cancelled}): {message}"
                    );
                    // [M4d] Клиент сдался на капче — live-challenge профиля
                    // закрывается (Failed): ждать результата больше некому.
                    if outcome == ClientOutcome::CaptchaRequired {
                        self.captcha.fail_live(&id);
                    }
                    if !cancelled {
                        self.apply_action(now, &mut client).await;
                    }
                }
            }
        }
        self.state = DaemonState::Stopping;
        if let Some(handle) = client.take()
            && !handle.token.is_cancelled()
        {
            handle.token.cancel();
            let _ = tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, handle.supervisor).await;
        }
        self.state = DaemonState::Stopped;
        self.write_status(Instant::now());
        if let Some(sink) = self.log_sink.take() {
            sink.flush();
        }
        Ok(())
    }

    /// Применить решение менеджера: остановить/запустить клиент.
    async fn apply_action(&mut self, now: Instant, client: &mut Option<ClientHandle>) {
        let action = self.manager.next_action(now);
        match action {
            Action::Keep => {}
            Action::Stop => {
                self.stop_client(client);
            }
            Action::Start { id, config } => {
                if self.manager.active_id() == Some(id.as_str())
                    && client
                        .as_ref()
                        .is_some_and(|handle| !handle.supervisor.is_finished())
                {
                    return;
                }
                // TUN-downtime при switch: предыдущий клиент обязан освободить
                // csqtt0 (фиксированное имя) до запуска следующего.
                self.stop_client(client);
                if let Some(handle) = client.take() {
                    if !handle.token.is_cancelled() {
                        handle.token.cancel();
                    }
                    let _ =
                        tokio::time::timeout(GRACEFUL_SHUTDOWN_TIMEOUT, handle.supervisor).await;
                }
                let token = CancellationToken::new();
                let task = spawn_client(
                    self.options.runner.clone(),
                    *config,
                    token.clone(),
                    id.clone(),
                    self.sender.clone(),
                );
                *client = Some(ClientHandle {
                    token,
                    supervisor: task,
                });
                self.manager.on_client_started(&id, now);
                crate::log_error!("[ПУЛ] Запущен профиль {id}");
            }
        }
    }

    /// Отменить клиент (токен); финальный await — в apply_action/завершении.
    fn stop_client(&self, client: &mut Option<ClientHandle>) {
        if let Some(handle) = client.as_ref()
            && !handle.token.is_cancelled()
        {
            handle.token.cancel();
        }
    }

    fn reload_from_file(&mut self, now: Instant) -> Result<()> {
        let text = std::fs::read_to_string(&self.options.config_path)?;
        let pool = Self::parse_pool(&text)?;
        self.manager.reload(pool.clone(), now);
        self.pool = pool;
        // Секреты могли измениться — обновляем redaction-набор лог-синка.
        if let Some(sink) = &self.log_sink {
            sink.set_secrets(self.pool.collect_secrets());
        }
        Ok(())
    }

    fn write_status(&self, now: Instant) {
        let now_epoch = epoch_secs();
        // [M4d] Safe fields только: session_token/redirect_uri/result —
        // секреты, они остаются в памяти CaptchaManager.
        let captcha_challenges = self
            .captcha
            .views()
            .iter()
            .map(|view| challenge_status(view, now, now_epoch))
            .collect();
        let status = self.manager.snapshot(now, self.state, captcha_challenges);
        if let Err(error) = write_status_atomic(&self.options.status_path, &status) {
            crate::log_error!("[ПУЛ] Ошибка записи status.json: {error}");
        }
    }

    /// [M4d] Перехваченный wire-запрос CAPTCHA_SOLVE от ядра: регистрируем
    /// challenge для активного профиля, секреты изолируем в менеджере.
    /// При policy=failover ждать некому (headless-служба CSQTT без Web Helper) —
    /// профиль в CaptchaRequired, ждущий клиент отменяется, стартует B.
    async fn handle_captcha_request(
        &mut self,
        request: CaptchaSolveRequest,
        client: &mut Option<ClientHandle>,
    ) {
        let now = Instant::now();
        let mode = request.mode.clone();
        let Some(profile) = self.manager.active_id().map(str::to_owned) else {
            crate::log_error!(
                "[КАПЧА] wire-запрос ({mode}) без активного профиля — секреты не сохранены, запрос отклонён"
            );
            return;
        };
        let (view, capability) = self.captcha.open_challenge(&profile, &mode, request, now);
        // [M4e] raw capability не пишется ни в лог, ни в status: она
        // передаётся Web Helper (память процесса) для построения helper-URL и
        // отдачи через loopback API. В лог/QR/CLI попадает только challenge-id.
        if let Some(helper) = &self.helper {
            helper.remember_capability(&view.id, &capability, now);
        }
        crate::log_error!(
            "[КАПЧА] challenge {} для профиля {profile}: режим «{mode}», истекает через {}с; секреты изолированы (лог/status не содержат)",
            view.id,
            view.expires.saturating_duration_since(now).as_secs()
        );
        // Оператор/UI должен видеть challenge сразу, а не через status-тик.
        self.write_status(now);
        if self.manager.effective_captcha_policy_for(&profile) == CaptchaPolicy::Failover {
            self.give_up_captcha(&profile, now, client);
            self.apply_action(now, client).await;
        }
    }

    /// [M4d] Результат решения от внешнего помощника/оператора. Маппинг на
    /// единственный активный upstream-wait: результат доставляется только
    /// challenge текущего активного профиля (ячейного канала ядра), иначе
    /// помечается устаревшим — чужому профилю его не подставляем.
    async fn handle_captcha_result(
        &mut self,
        challenge_id: &str,
        result: &str,
        client: &mut Option<ClientHandle>,
    ) {
        let now = Instant::now();
        match self.captcha.submit_result(challenge_id, result, now) {
            SubmitOutcome::Accepted(result) => {
                let active = self.manager.active_id().map(str::to_owned);
                let belongs = active.as_deref() == self.captcha.profile_of(challenge_id).as_deref();
                if !belongs {
                    self.captcha.fail_stale(challenge_id);
                    crate::log_error!(
                        "[КАПЧА] результат challenge {challenge_id} устарел: профиль больше не активен"
                    );
                } else {
                    // Upstream wire (без request-id): один результат — один
                    // активный ожидатель в ядре.
                    let line = format!("CAPTCHA_RESULT|{result}");
                    if crate::submit_control_line(line) {
                        self.captcha.confirm_solved(challenge_id);
                        crate::log_error!(
                            "[КАПЧА] результат challenge {challenge_id} доставлен решателю"
                        );
                    } else {
                        self.captcha.fail_stale(challenge_id);
                        crate::log_error!(
                            "[КАПЧА] результат challenge {challenge_id} принять некому: клиент не принимает команды"
                        );
                    }
                }
            }
            SubmitOutcome::InvalidResult => crate::log_error!(
                "[КАПЧА] результат challenge {challenge_id} отклонён: пустой или error-ответ"
            ),
            SubmitOutcome::NotLive => {
                let state = self
                    .captcha
                    .view(challenge_id)
                    .map(|view| view.state.as_str())
                    .unwrap_or("missing");
                crate::log_error!(
                    "[КАПЧА] результат отклонён: challenge {challenge_id} не живой ({state})"
                );
            }
        }
        self.apply_action(now, client).await;
        // [M4e] Оператор/Web Helper видят исход (solved/failed) сразу,
        // не дожидаясь status-тика (~5с).
        self.write_status(Instant::now());
    }

    /// [M4d] Профиль отдаётся капче: CaptchaRequired + свободный слот; если
    /// клиент всё ещё ждёт результата — отменяем его (solver прервётся по
    /// токену — безопасная отмена, см. captcha.rs webview()).
    fn give_up_captcha(&mut self, profile: &str, now: Instant, client: &mut Option<ClientHandle>) {
        let stuck = self.manager.active_id() == Some(profile)
            && client
                .as_ref()
                .is_some_and(|handle| !handle.supervisor.is_finished());
        self.manager.mark_captcha_required(profile, now);
        if stuck {
            self.stop_client(client);
            // Клиент отменён — результат капчи принимать некому: challenge
            // закрывается (Failed), ждать больше некого.
            self.captcha.fail_live(profile);
            crate::log_error!(
                "[КАПЧА] профиль {profile}: капча не решена — клиент отменён, challenge закрыт, слот свободен"
            );
        } else {
            crate::log_error!(
                "[КАПЧА] профиль {profile}: капча не решена — CaptchaRequired, слот свободен"
            );
        }
    }

    /// [M4d] Вымести протухшие challenges (служба CSQTT гоняет на health-тиках).
    /// Застоявшийся ждущий клиент отменяется; остальные профили возвращаются
    /// в Standby (будут выбраны, когда дойдёт очередь — без отрыва активного).
    fn sweep_captchas(&mut self, now: Instant, client: &mut Option<ClientHandle>) {
        for profile in self.captcha.expire(now) {
            self.give_up_captcha(&profile, now, client);
        }
        self.captcha.purge();
        // [M4e] capability-записи helper живут ровно пока challenge живой.
        if let Some(helper) = &self.helper {
            let live_ids = self
                .captcha
                .views()
                .iter()
                .filter(|view| {
                    matches!(
                        view.state,
                        crate::captcha_manager::ChallengeState::Pending
                            | crate::captcha_manager::ChallengeState::Opened
                            | crate::captcha_manager::ChallengeState::Verifying
                    )
                })
                .map(|view| view.id.clone())
                .collect::<Vec<_>>();
            helper.prune_capabilities(&live_ids, now);
        }
    }
}

/// [M4d] Safe-представление challenge для status.json: секретов нет.
/// `created`/`expires` (Instant) переводятся в epoch относительно `now`.
/// `expires` в будущем → `expires_at` в будущем (не схлопывается в now).
fn challenge_status(
    view: &crate::captcha_manager::ChallengeView,
    now: Instant,
    now_epoch: u64,
) -> CaptchaChallengeStatus {
    CaptchaChallengeStatus {
        id: view.id.clone(),
        profile_id: view.profile_id.clone(),
        mode: view.mode.clone(),
        state: view.state.as_str().to_string(),
        created_at: now_epoch.saturating_sub(now.saturating_duration_since(view.created).as_secs()),
        expires_at: now_epoch.saturating_add(view.expires.saturating_duration_since(now).as_secs()),
    }
}

/// Supervisor-задача: бежит клиент, по завершении шлёт результат в канал
/// команд службы CSQTT (id нужен менеджеру для классификации по профилю).
/// `cancelled` определяется по токену после завершения: отмена токеном —
/// clean stop (следствие решения менеджера, а не новая информация).
fn spawn_client(
    runner: Arc<dyn ClientRunner>,
    config: ClientConfig,
    token: CancellationToken,
    id: String,
    commands: mpsc::UnboundedSender<DaemonCommand>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let result = runner.run(config, token.clone()).await;
        let cancelled = token.is_cancelled();
        let _ = commands.send(DaemonCommand::ClientFinished {
            id,
            result,
            cancelled,
        });
    })
}

// ===========================================================================
// Лог-колбэк: события -> DaemonCommand::Health
// ===========================================================================

/// Установить глобальный лог-колбэк: файловый синк (M4a, redaction секретов)
/// + парсинг `__CSQTT_EVENT__|...` строк в канал команд службы CSQTT.
///
/// События — единственный machine-readable runtime state (CSQTT_EVENTS=1):
/// человеческие логи как state не парсить (PROJECT_CONTRACT).
pub fn install_event_pipeline(
    sink: Option<Arc<crate::logsink::FileLogSink>>,
    commands: mpsc::UnboundedSender<DaemonCommand>,
) {
    crate::set_log_callback(Box::new(move |line: String| {
        process_log_line(&line, sink.as_deref(), &commands);
    }));
}

/// Одна строка лога: файловый синк + разбор событий + [M4d] перехват
/// wire-запроса капчи. Wire-строка НЕ пишется в лог-файл: её секреты
/// (redirect_uri, session_token) изолированы в CaptchaManager — требование
/// классификации секретов (не в обычный лог/status/QR). [High-1 AUDIT]
/// обычные строки дополнительно проходят fail-closed скраббер
/// `session_token=` — секрет мог попасть в текст ошибки HTTP-клиента.
fn process_log_line(
    line: &str,
    sink: Option<&crate::logsink::FileLogSink>,
    commands: &mpsc::UnboundedSender<DaemonCommand>,
) {
    if let Some(request) = parse_captcha_solve_line(line) {
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        return;
    }
    if let Some(sink) = sink {
        sink.write_line(&redact_session_token(line));
    }
    if let Some(record) = parse_event_line(line)
        && let Some(input) = event_to_health(&record)
    {
        let _ = commands.send(DaemonCommand::Health(input));
    }
}

fn event_to_health(record: &crate::events::EventRecord) -> Option<HealthInput> {
    match record.kind.as_str() {
        "READY" => Some(HealthInput::Ready {
            worker: record.payload["worker"].as_u64().unwrap_or_default() as usize,
        }),
        "STATS" => Some(HealthInput::Stats {
            active: record.payload["active"].as_i64().unwrap_or_default(),
            bytes_up: record.payload["bytes_up"].as_i64().unwrap_or_default(),
            bytes_down: record.payload["bytes_down"].as_i64().unwrap_or_default(),
        }),
        "NETWORK_SUSPECT" => Some(HealthInput::NetworkSuspect),
        "ACTIVE_ZERO" => Some(HealthInput::ActiveZero),
        "CALL_UNAVAILABLE" => Some(HealthInput::CallUnavailable),
        "CONFIG" => parse_tunnel_config(record.payload["config"].as_str().unwrap_or_default()),
        _ => None,
    }
}

/// `TUNCONF:ip:dns` → TunnelConfig; прочие CONFIG-строки игнорируются.
fn parse_tunnel_config(config: &str) -> Option<HealthInput> {
    let tunconf = config.strip_prefix("TUNCONF:")?;
    let mut fields = tunconf.splitn(2, ':');
    let ip = fields.next()?.to_string();
    let dns = fields.next().unwrap_or_default().to_string();
    Some(HealthInput::TunnelConfig { ip, dns })
}

/// Unix signal handlers: SIGTERM/SIGINT → cancel (graceful), SIGHUP → reload.
/// Windows-сборка службы CSQTT не нужна; здесь unix-only тонкая обёртка.
#[cfg(unix)]
pub fn install_signal_handlers(
    cancel: CancellationToken,
    commands: mpsc::UnboundedSender<DaemonCommand>,
) {
    use tokio::signal::unix::{SignalKind, signal};
    let cancel = cancel.clone();
    tokio::spawn(async move {
        let mut term = signal(SignalKind::terminate()).expect("SIGTERM handler");
        let mut int = signal(SignalKind::interrupt()).expect("SIGINT handler");
        tokio::select! {
            _ = term.recv() => crate::log_error!("[ПУЛ] Получен SIGTERM"),
            _ = int.recv() => crate::log_error!("[ПУЛ] Получен SIGINT"),
        }
        cancel.cancel();
    });
    tokio::spawn(async move {
        let mut hangup = signal(SignalKind::hangup()).expect("SIGHUP handler");
        while hangup.recv().await.is_some() {
            if commands.send(DaemonCommand::Reload).is_err() {
                break;
            }
        }
    });
}

#[cfg(test)]
mod tests;
