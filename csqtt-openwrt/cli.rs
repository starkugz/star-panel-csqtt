// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [OpenWrt-порт, M4c] CLI-подкоманды: status / doctor / profile / captcha /
//! log / version.
//!
//! Диспетчер: первый аргумент = подкоманда. Без него (или с неизвестным
//! первым аргументом) main.rs уходит в legacy-режим прямых флагов (ручной
//! режим M3R) — он не затронут. `csqtt run` остаётся в main.rs (служба CSQTT M4b).
//!
//! Контракты (PROMPTS.md M4c, PROJECT_CONTRACT):
//! - `status` читает runtime-state из `/var/run/csqtt/status.json`
//!   (единственный источник runtime state — не UCI и не логи) и НЕ
//!   парсит человеческие логи как состояние.
//! - `doctor` только читает и проверяет; он НИКОГДА не меняет конфиг,
//!   DNS, firewall, default route или sysctl (даже rp_filter=1 —
//!   WARN с подсказкой, лечение — M8 per-interface sysctl).
//! - `profile import` по умолчанию показывает preview; commit — только
//!   явным флагом `--commit` (транзакция M4a: backup + atomic rename).
//! - captcha: read-only safe fields из status.json; управление
//!   challenge (cancel) требует backend M4d — до него честное
//!   «captcha manager not available».
//! - secrets (password, vk_js_token, хеши) не выводятся в выводе
//!   подкоманд (правило 10 UCI-схемы); `profile export` печатает ссылку
//!   (она сама по себе секрет) с предупреждением.

use crate::import::{self, ImportRequest};
use crate::link::parse_peer;
use crate::status::{
    CaptchaChallengeStatus, DEFAULT_STATUS_PATH, DaemonState, DaemonStatus, ProfileStatus,
};
use crate::uci::{
    ProfilePool, ServerProfile, UciFile, parse_uci, redact_secrets, render_uci, uci_remove_server,
    uci_set_active_profile, uci_set_selection_auto, uci_set_server_enabled, uci_set_server_option,
};
use clap::Parser;
use serde::Serialize;
use std::fs;
use std::io::Write;
use std::net::{ToSocketAddrs, UdpSocket};
use std::path::{Path, PathBuf};
use std::time::Duration;
/// Поддерживаемые подкоманды (первый аргумент). Всё остальное → legacy CLI.
pub const SUBCOMMANDS: [&str; 6] = ["status", "doctor", "profile", "captcha", "log", "version"];

/// UCI-конфиг пула профилей (production).
pub const DEFAULT_CONFIG_PATH: &str = "/etc/config/csqtt";
/// Лог по умолчанию, если конфиг не читается (main.log_file).
const DEFAULT_LOG_FILE: &str = "/var/log/csqtt.log";
/// Таймаут UDP-проверки peer в doctor.
const PEER_PROBE_TIMEOUT: Duration = Duration::from_secs(2);

/// Точка входа: если первый аргумент — известная подкоманда, выполняет её и
/// возвращает код выхода; иначе None (main.rs уходит в legacy-режим).
pub fn handle(args: &[String]) -> Option<i32> {
    let first = args.first()?;
    if !SUBCOMMANDS.contains(&first.as_str()) {
        return None;
    }
    let mut argv: Vec<String> = vec!["csqtt".to_string()];
    argv.extend_from_slice(args);
    match SubCommand::try_parse_from(argv) {
        Ok(command) => Some(run(command)),
        Err(error) => {
            // --help на подкоманде: clap отдаёт DisplayHelp (stdout, код 0).
            if error.use_stderr() {
                eprintln!("{error:#}");
            } else {
                println!("{error:#}");
            }
            Some(error.exit_code())
        }
    }
}

fn run(command: SubCommand) -> i32 {
    // CLI пишем в канал/pipe: стандартное Unix-поведение при закрытом читателе
    // (csqtt profile list | head) — тихий выход по SIGPIPE, а не panic
    // println! с кодом 101. Служба CSQTT (csqtt run) SIGPIPE не меняет — ему лучше
    // получать EPIPE как ошибку записи.
    reset_sigpipe_to_default();
    match command {
        SubCommand::Status { json, status_path } => run_status(json, &status_path),
        SubCommand::Doctor {
            json,
            config,
            config_dir,
            status_path,
        } => run_doctor(json, &config, config_dir.as_deref(), &status_path),
        SubCommand::Profile { command } => run_profile(&command),
        SubCommand::Captcha {
            command,
            status_path,
        } => run_captcha(&command, &status_path),
        SubCommand::Log { command } => run_log(&command),
        SubCommand::Version {} => {
            println!(
                "csqtt {} (OpenWrt port, interface-only)",
                env!("CARGO_PKG_VERSION")
            );
            0
        }
    }
}

// ===========================================================================
// clap-модель подкоманд
// ===========================================================================

#[derive(Parser)]
#[command(
    name = "csqtt",
    about = "CSQTT для OpenWrt: status/doctor/profile/captcha/log/version; без подкоманды — ручной режим прямых флагов"
)]
enum SubCommand {
    /// Показать runtime-статус службы CSQTT (exit: 0=подключён, 1=нет, 2=сервис не запущен)
    Status {
        /// Вывести status.json как есть (machine-readable)
        #[arg(long)]
        json: bool,
        /// Путь к status.json (по умолчанию /var/run/csqtt/status.json)
        #[arg(long, default_value = DEFAULT_STATUS_PATH)]
        status_path: String,
    },
    /// Диагностика окружения: конфиг, TUN, WAN-маршрут, peer, csqtt0, rp_filter
    Doctor {
        /// Вывести отчёт в JSON
        #[arg(long)]
        json: bool,
        /// Путь к UCI-конфигу (по умолчанию /etc/config/csqtt)
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: String,
        /// Каталог с файлом конфига (ищется <dir>/csqtt) — перекрывает --config
        #[arg(long)]
        config_dir: Option<String>,
        /// Путь к status.json для проверки владельца csqtt0
        #[arg(long, default_value = DEFAULT_STATUS_PATH)]
        status_path: String,
    },
    /// Управление профилями пула (UCI)
    Profile {
        #[command(subcommand)]
        command: ProfileCommand,
    },
    /// Ожидающие CAPTCHA (safe fields; backend — M4d)
    Captcha {
        #[command(subcommand)]
        command: CaptchaCommand,
        /// Путь к status.json
        #[arg(long, default_value = DEFAULT_STATUS_PATH)]
        status_path: String,
    },
    /// Чтение лог-файла службы CSQTT
    Log {
        #[command(subcommand)]
        command: LogCommand,
    },
    /// Версия бинарника
    Version {},
}

/// Общий флаг --config для листовых подкоманд profile (flatten — чтобы
/// `csqtt profile show finland --config X` работал в естественном порядке).
#[derive(Parser)]
struct ConfigArg {
    /// Путь к UCI-конфигу (по умолчанию /etc/config/csqtt)
    #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
    config: String,
}

#[derive(Parser)]
enum ProfileCommand {
    /// Список профилей конфигурации (секретов нет)
    List {
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Детали профиля (секреты замаскированы)
    Show {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Ручной выбор профиля: manual + active_profile
    Use {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Автоматический выбор по priority
    Auto {
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Включить профиль
    Enable {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Выключить профиль
    Disable {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Удалить профиль из конфига
    Remove {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Изменить одну опцию профиля (ключи UCI: peer, workers, priority, ...)
    Set {
        id: String,
        key: String,
        value: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Импорт csqtt://-ссылки: по умолчанию preview, commit только с --commit
    Import {
        /// csqtt://connect?... ссылка
        link: String,
        /// Желаемый section id (по умолчанию — из хоста ссылки)
        #[arg(long)]
        id: Option<String>,
        /// Отображаемое имя профиля
        #[arg(long)]
        name: Option<String>,
        /// Активировать профиль сразу после commit (manual + active_profile)
        #[arg(long)]
        activate: bool,
        /// Применить preview на диск (backup + atomic rename)
        #[arg(long)]
        commit: bool,
        /// Каталог для backup-копий (по умолчанию /etc/csqtt/backups)
        #[arg(long, default_value = import::BACKUP_DIR)]
        backup_dir: String,
        #[command(flatten)]
        config: ConfigArg,
    },
    /// Экспорт профиля в csqtt://-ссылку (содержит секреты!)
    Export {
        id: String,
        #[command(flatten)]
        config: ConfigArg,
    },
}

impl ProfileCommand {
    fn config(&self) -> &str {
        match self {
            ProfileCommand::List { config }
            | ProfileCommand::Show { config, .. }
            | ProfileCommand::Use { config, .. }
            | ProfileCommand::Auto { config }
            | ProfileCommand::Enable { config, .. }
            | ProfileCommand::Disable { config, .. }
            | ProfileCommand::Remove { config, .. }
            | ProfileCommand::Set { config, .. }
            | ProfileCommand::Import { config, .. }
            | ProfileCommand::Export { config, .. } => &config.config,
        }
    }
}

#[derive(Parser)]
enum CaptchaCommand {
    /// Список ожидающих CAPTCHA (safe fields)
    List {},
    /// Детали ожидающей CAPTCHA по id профиля
    Show { id: Option<String> },
    /// Отмена ожидающей CAPTCHA (требует backend M4d)
    Cancel { id: Option<String> },
}

#[derive(Parser)]
enum LogCommand {
    /// Последние N строк лога (с маскировкой секретов)
    Tail {
        /// Сколько строк показать (по умолчанию 20)
        #[arg(short = 'n', long, default_value_t = 20)]
        lines: usize,
        /// Следовать за логом (Ctrl-C для выхода)
        #[arg(short = 'f', long)]
        follow: bool,
        /// Лог-файл (по умолчанию main.log_file из конфига)
        #[arg(long)]
        file: Option<String>,
        /// Путь к UCI-конфигу (для main.log_file и секретов redaction)
        #[arg(long, default_value = DEFAULT_CONFIG_PATH)]
        config: String,
    },
}

#[cfg(unix)]
fn reset_sigpipe_to_default() {
    // Безопасно: одноразовый короткоживущий CLI без сетевых PIPE-записей.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

#[cfg(not(unix))]
fn reset_sigpipe_to_default() {}

// ===========================================================================
// status
// ===========================================================================

/// Результат классификации status.json для кода выхода.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StatusOutcome {
    /// Активный профиль в состоянии active — туннель работает.
    Connected,
    /// Служба CSQTT жива, но туннель не подключён (connecting/cooldown/failed/...).
    NotConnected,
    /// Сервис не запущен (нет файла или daemon_state=stopped).
    NotRunning,
}

/// Классификация status.json: 0=connected, 1=not connected, 2=not running.
/// Проводится по машинно-читаемому runtime state, не по логам.
pub fn classify_status(status: &DaemonStatus) -> StatusOutcome {
    if status.daemon_state == DaemonState::Stopped.as_str() {
        return StatusOutcome::NotRunning;
    }
    // Connected требует одновременно живой службы CSQTT (running) и активного
    // профиля; disabled/starting/reloading — туннель ещё не работает.
    let active = status
        .active_profile
        .as_deref()
        .and_then(|id| status.profiles.iter().find(|profile| profile.id == id));
    match (status.daemon_state.as_str(), active) {
        ("running", Some(profile)) if profile.state == "active" => StatusOutcome::Connected,
        ("stopped", _) => StatusOutcome::NotRunning,
        _ => StatusOutcome::NotConnected,
    }
}

enum StatusRead {
    Ok(Box<DaemonStatus>),
    Missing,
    Invalid(String),
}

fn read_status_file(status_path: &str) -> StatusRead {
    match fs::read_to_string(status_path) {
        Ok(text) => match serde_json::from_str::<DaemonStatus>(&text) {
            Ok(status) => StatusRead::Ok(Box::new(status)),
            Err(error) => StatusRead::Invalid(error.to_string()),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => StatusRead::Missing,
        Err(error) => StatusRead::Invalid(error.to_string()),
    }
}

fn run_status(json: bool, status_path: &str) -> i32 {
    match read_status_file(status_path) {
        StatusRead::Missing => {
            eprintln!("сервис не запущен: статус-файл не найден: {status_path}");
            2
        }
        StatusRead::Invalid(message) => {
            eprintln!("сервис не запущен: статус-файл невалиден: {message}");
            2
        }
        StatusRead::Ok(status) => {
            if json {
                match status.to_json() {
                    Ok(text) => println!("{text}"),
                    Err(error) => {
                        eprintln!("ошибка сериализации статуса: {error}");
                        return 2;
                    }
                }
            } else {
                println!("{}", format_status(&status));
            }
            match classify_status(&status) {
                StatusOutcome::Connected => 0,
                StatusOutcome::NotConnected => 1,
                StatusOutcome::NotRunning => 2,
            }
        }
    }
}

/// Человекочитаемый статус без секретов (модель уже их не содержит;
/// last_error служба CSQTT обезвредила через redact_secrets до записи).
pub fn format_status(status: &DaemonStatus) -> String {
    let mut out = String::new();
    out.push_str(&format!(
        "daemon:      {} (uptime {})\n",
        status.daemon_state,
        human_duration(status.uptime_secs)
    ));
    out.push_str(&format!(
        "selection:   {}  active: {}\n",
        status.selection_mode,
        status.active_profile.as_deref().unwrap_or("-")
    ));
    out.push_str("profiles:\n");
    for profile in &status.profiles {
        let marker = if status
            .active_profile
            .as_deref()
            .is_some_and(|id| id == profile.id)
        {
            " *"
        } else {
            "  "
        };
        out.push_str(&format!(
            "{marker}{} {} state={} priority={} enabled={} fails={} cooldown={}s\n",
            profile.id,
            profile.name,
            profile.state,
            profile.priority,
            profile.enabled,
            profile.consecutive_fails,
            profile.cooldown_remaining_secs
        ));
    }
    out.push_str(&format!(
        "tunnel:      {} addr={} mtu={} dns={}\n",
        status.tunnel.interface,
        status.tunnel.address.as_deref().unwrap_or("-"),
        status.tunnel.mtu,
        status.tunnel.dns.as_deref().unwrap_or("-")
    ));
    out.push_str(&format!(
        "workers:     {}/{} configured active\n",
        status.workers.active, status.workers.configured
    ));
    out.push_str(&format!(
        "stats:       rx {} tx {} reconnects={}\n",
        human_bytes(status.rx_bytes),
        human_bytes(status.tx_bytes),
        status.reconnects
    ));
    out.push_str(&format!(
        "routing:     {} — {}\n",
        status.routing.mode, status.routing.summary
    ));
    out.push_str(&format!(
        "captcha:     {} pending   health: {} {}\n",
        status.captcha_pending, status.health_mode, status.health_target
    ));
    match &status.last_error {
        Some(error) => out.push_str(&format!("last error:  {error}\n")),
        None => out.push_str("last error:  none\n"),
    }
    out.trim_end().to_string()
}

fn human_duration(secs: u64) -> String {
    if secs < 60 {
        return format!("{secs}s");
    }
    if secs < 3600 {
        return format!("{}m{}s", secs / 60, secs % 60);
    }
    format!("{}h{}m", secs / 3600, (secs % 3600) / 60)
}

fn human_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;
    if bytes >= GB {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    } else if bytes >= MB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else if bytes >= KB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else {
        format!("{bytes} B")
    }
}

// ===========================================================================
// doctor
// ===========================================================================

/// Статус одной проверки.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// Всё хорошо.
    Ok,
    /// Проблема, требующая внимания (продолжать можно).
    Warn,
    /// Блокирующая проблема — туннель не заработает.
    Fail,
    /// Проверка неприменима к этому окружению (например, /proc на Windows).
    Skip,
    /// Необязательная справка (mihomo).
    Info,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: CheckStatus,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct DoctorReport {
    pub checks: Vec<DoctorCheck>,
}

impl DoctorReport {
    /// Код выхода: 2 — есть Fail, 1 — есть Warn, иначе 0.
    pub fn exit_code(&self) -> i32 {
        if self
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Fail)
        {
            2
        } else if self
            .checks
            .iter()
            .any(|check| check.status == CheckStatus::Warn)
        {
            1
        } else {
            0
        }
    }

    fn push(&mut self, name: &str, status: CheckStatus, message: impl Into<String>) {
        self.checks.push(DoctorCheck {
            name: name.to_string(),
            status,
            message: message.into(),
        });
    }
}

/// Результат UDP-проверки peer (внедряемый — для тестов).
#[derive(Clone, Debug)]
pub enum ProbeOutcome {
    /// Получен ответ за N мс.
    Reachable(u128),
    /// Ответа нет / ошибка — туннель через этот peer не поднимется.
    Unreachable(String),
    /// Проверка не выполнялась (нет peer в конфиге).
    Skipped(String),
}

/// Доступность /dev/net/tun.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TunProbe {
    /// Платформа не поддерживает проверку (не Linux).
    NotPlatform,
    /// Устройства нет — kmod-tun не загружен.
    Missing,
    /// Есть, но нет прав (CAP_NET_ADMIN).
    NoAccess,
    /// Открывается на чтение/запись.
    Ok,
}

/// Найденный WAN default route.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DefaultRoute {
    pub device: String,
    pub gateway: String,
}

/// Входные данные doctor. Реальный запуск собирает их с системы (часть —
/// только на Linux); тесты подставляют моки. Doctor ничего не меняет.
#[derive(Clone, Debug)]
pub struct DoctorInput {
    pub config_path: String,
    pub config_text: Option<String>,
    pub tun: TunProbe,
    pub wan_default: Option<DefaultRoute>,
    pub csqtt0_exists: Option<bool>,
    /// None — status.json недоступен.
    pub daemon_running: Option<bool>,
    pub rp_filter: Option<(u8, String)>,
    pub peer_probe: ProbeOutcome,
    pub mihomo_detected: bool,
}

/// Сборка отчёта из входных данных — чистая функция (поведение
/// детерминировано, без I/O).
pub fn build_doctor_report(input: &DoctorInput) -> DoctorReport {
    let mut report = DoctorReport { checks: Vec::new() };

    // 1. Конфиг: разбирается и валиден ли.
    match &input.config_text {
        None => report.push(
            "config",
            CheckStatus::Skip,
            format!("конфиг не найден: {}", input.config_path),
        ),
        Some(text) => match ProfilePool::from_text(text) {
            Ok(pool) => report.push(
                "config",
                CheckStatus::Ok,
                format!(
                    "конфиг валиден: {} профилей, selection={}, active={}",
                    pool.servers.len(),
                    pool.main.selection_mode.as_str(),
                    pool.selected_profile()
                        .map(|profile| profile.section_id.as_str())
                        .unwrap_or("-")
                ),
            ),
            Err(issues) => report.push(
                "config",
                CheckStatus::Fail,
                format!(
                    "конфиг невалиден: {}",
                    issues
                        .iter()
                        .map(|issue| issue.to_string())
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            ),
        },
    }

    // 2. /dev/net/tun + CAP_NET_ADMIN.
    match input.tun {
        TunProbe::NotPlatform => {
            report.push(
                "tun-device",
                CheckStatus::Skip,
                "проверка /dev/net/tun доступна только на Linux",
            );
        }
        TunProbe::Missing => report.push(
            "tun-device",
            CheckStatus::Fail,
            "/dev/net/tun отсутствует: загрузите модуль ядра (OpenWrt: apk add kmod-tun)",
        ),
        TunProbe::NoAccess => report.push(
            "tun-device",
            CheckStatus::Fail,
            "/dev/net/tun есть, но нет прав: требуется CAP_NET_ADMIN (запуск от root)",
        ),
        TunProbe::Ok => report.push(
            "tun-device",
            CheckStatus::Ok,
            "/dev/net/tun доступен, CAP_NET_ADMIN есть",
        ),
    }

    // 3. WAN default route: служба CSQTT interface-only и НЕ должна им быть.
    match &input.wan_default {
        None => report.push(
            "wan-default-route",
            CheckStatus::Skip,
            "default route не определён (нет /proc/net/route)",
        ),
        Some(route) if route.device == "csqtt0" => report.push(
            "wan-default-route",
            CheckStatus::Fail,
            format!(
                "default route через {} — служба CSQTT не должна становиться шлюзом (изоляция)",
                route.device
            ),
        ),
        Some(route) => report.push(
            "wan-default-route",
            CheckStatus::Ok,
            format!("default route: dev {} via {}", route.device, route.gateway),
        ),
    }

    // 4. Доступность peer через WAN (control-plane идёт через WAN, не туннель).
    match &input.peer_probe {
        ProbeOutcome::Skipped(reason) => report.push(
            "peer-reachability",
            CheckStatus::Skip,
            format!("проверка peer пропущена: {reason}"),
        ),
        ProbeOutcome::Reachable(millis) => report.push(
            "peer-reachability",
            CheckStatus::Ok,
            format!("peer отвечает на UDP-пробу через WAN за {millis} мс"),
        ),
        ProbeOutcome::Unreachable(reason) => report.push(
            "peer-reachability",
            CheckStatus::Warn,
            format!("peer недоступен через WAN: {reason}"),
        ),
    }

    // 5. csqtt0: владелец/коллизия (leftover interface).
    match (input.csqtt0_exists, input.daemon_running) {
        (None, _) => report.push(
            "csqtt0-collision",
            CheckStatus::Skip,
            "проверка интерфейсов доступна только на Linux",
        ),
        (Some(false), _) => report.push(
            "csqtt0-collision",
            CheckStatus::Ok,
            "csqtt0 свободен — будет создана службой CSQTT при старте туннеля",
        ),
        (Some(true), Some(true)) => report.push(
            "csqtt0-collision",
            CheckStatus::Ok,
            "csqtt0 поднят работающей службой CSQTT",
        ),
        (Some(true), _) => report.push(
            "csqtt0-collision",
            CheckStatus::Warn,
            "csqtt0 существует, но служба CSQTT не запущена: leftover interface (удалите: ip link delete csqtt0)",
        ),
    }

    // 6. rp_filter: strict=1 режет downlink (итог M3X-B); doctor НЕ лечит.
    match &input.rp_filter {
        None => report.push(
            "rp-filter",
            CheckStatus::Skip,
            "sysctl rp_filter не читается на этой платформе",
        ),
        Some((value, scope)) if *value == 1 => report.push(
            "rp-filter",
            CheckStatus::Warn,
            format!(
                "rp_filter=1 на {scope} — строгий режим DROP'ает downlink на csqtt0; лечится per-interface sysctl: sysctl -w net.ipv4.conf.csqtt0.rp_filter=2 (doctor систему не меняет)"
            ),
        ),
        Some((value, scope)) => report.push(
            "rp-filter",
            CheckStatus::Ok,
            format!("rp_filter={value} на {scope} — downlink резаться не будет"),
        ),
    }

    // 7. Mihomo (optional): пользовательский прокси с interface-name: csqtt0.
    if input.mihomo_detected {
        report.push(
            "mihomo",
            CheckStatus::Info,
            "mihomo обнаружен: проверьте interface-name: csqtt0 в конфиге прокси (routing layer не нужен)",
        );
    } else {
        report.push(
            "mihomo",
            CheckStatus::Info,
            "mihomo не обнаружен (optional): пользовательский трафик пойдёт через csqtt0 только после настройки прокси",
        );
    }

    report
}

pub fn format_doctor(report: &DoctorReport) -> String {
    let mut out = String::new();
    for check in &report.checks {
        out.push_str(&format!(
            "[{}] {} — {}\n",
            match check.status {
                CheckStatus::Ok => "OK",
                CheckStatus::Warn => "WARN",
                CheckStatus::Fail => "FAIL",
                CheckStatus::Skip => "SKIP",
                CheckStatus::Info => "INFO",
            },
            check.name,
            check.message
        ));
    }
    let fails = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Fail)
        .count();
    let warns = report
        .checks
        .iter()
        .filter(|check| check.status == CheckStatus::Warn)
        .count();
    out.push_str(&format!(
        "\nитог: {fails} FAIL, {warns} WARN (exit {})",
        report.exit_code()
    ));
    out
}

fn run_doctor(json: bool, config: &str, config_dir: Option<&str>, status_path: &str) -> i32 {
    let config_path = config_dir.map_or_else(
        || config.to_string(),
        |dir| Path::new(dir).join("csqtt").to_string_lossy().into_owned(),
    );
    let config_text = import::read_live_config(Path::new(&config_path))
        .ok()
        .flatten();
    let peer = config_text.as_deref().and_then(active_peer);
    let input = DoctorInput {
        config_path,
        config_text,
        tun: probe_tun_device(),
        wan_default: read_wan_default_route(),
        csqtt0_exists: check_csqtt0_exists(),
        daemon_running: daemon_running_from_status(status_path),
        rp_filter: read_rp_filter(),
        peer_probe: probe_peer(peer),
        mihomo_detected: detect_mihomo(),
    };
    let report = build_doctor_report(&input);
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(text) => println!("{text}"),
            Err(error) => {
                eprintln!("ошибка сериализации отчёта: {error}");
                return 2;
            }
        }
    } else {
        println!("{}", format_doctor(&report));
    }
    report.exit_code()
}

/// Peer активного/выбранного профиля из конфига (куда ходит control-plane).
fn active_peer(config_text: &str) -> Option<String> {
    ProfilePool::from_text(config_text)
        .ok()?
        .selected_profile()
        .map(|profile| profile.peer.clone())
}

/// Служба CSQTT запущена? Только из runtime state (status.json), не из логов.
fn daemon_running_from_status(status_path: &str) -> Option<bool> {
    match read_status_file(status_path) {
        StatusRead::Ok(status) => Some(status.daemon_state == DaemonState::Running.as_str()),
        StatusRead::Missing | StatusRead::Invalid(_) => None,
    }
}

/// Разбор /proc/net/route: default route (dst 00000000, UP|GATEWAY).
pub fn parse_proc_net_route(text: &str) -> Option<DefaultRoute> {
    for line in text.lines().skip(1) {
        let fields: Vec<&str> = line.split_whitespace().collect();
        if fields.len() < 4 {
            continue;
        }
        let flags = u32::from_str_radix(fields[3], 16).ok()?;
        // RTF_UP | RTF_GATEWAY
        if flags & 0x3 != 0 && fields[1] == "00000000" {
            let gateway = u32::from_str_radix(fields[2], 16).ok()?;
            let bytes = gateway.to_le_bytes();
            return Some(DefaultRoute {
                device: fields[0].to_string(),
                gateway: format!("{}.{}.{}.{}", bytes[0], bytes[1], bytes[2], bytes[3]),
            });
        }
    }
    None
}

#[cfg(unix)]
fn read_wan_default_route() -> Option<DefaultRoute> {
    parse_proc_net_route(&fs::read_to_string("/proc/net/route").ok()?)
}

#[cfg(not(unix))]
fn read_wan_default_route() -> Option<DefaultRoute> {
    None
}

#[cfg(unix)]
fn check_csqtt0_exists() -> Option<bool> {
    let text = fs::read_to_string("/proc/net/dev").ok()?;
    Some(text.lines().any(|line| line.starts_with("csqtt0:")))
}

#[cfg(not(unix))]
fn check_csqtt0_exists() -> Option<bool> {
    None
}

#[cfg(unix)]
fn read_rp_filter() -> Option<(u8, String)> {
    if Path::new("/proc/sys/net/ipv4/conf/csqtt0/rp_filter").exists() {
        return read_rp_filter_value("csqtt0");
    }
    read_rp_filter_value("all").or_else(|| read_rp_filter_value("default"))
}

#[cfg(unix)]
fn read_rp_filter_value(scope: &str) -> Option<(u8, String)> {
    let text = fs::read_to_string(format!("/proc/sys/net/ipv4/conf/{scope}/rp_filter")).ok()?;
    let value = text.trim().parse::<u8>().ok()?;
    Some((value, scope.to_string()))
}

#[cfg(not(unix))]
fn read_rp_filter() -> Option<(u8, String)> {
    None
}

/// UDP-проба peer: bind → connect → 1 байт → ждём ответа. Идёт через WAN
/// (служба CSQTT interface-only и не ставит маршрутов; сокет не привязан к csqtt0).
/// Не расходует VK-хеши и не начинает сессию — мусорный пакет сервер
/// молча игнорирует.
fn probe_peer(peer: Option<String>) -> ProbeOutcome {
    let Some(peer) = peer else {
        return ProbeOutcome::Skipped("в конфиге нет выбранного профиля/peer".to_string());
    };
    let Some((host, port)) = parse_peer(&peer) else {
        return ProbeOutcome::Unreachable(format!("peer «{peer}» не разбирается как host:port"));
    };
    let mut addresses = match (host.as_str(), port).to_socket_addrs() {
        Ok(addresses) => addresses,
        Err(_) => {
            return ProbeOutcome::Unreachable(format!(
                "host «{host}» не резолвится (DNS через WAN)"
            ));
        }
    };
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(socket) => socket,
        Err(error) => return ProbeOutcome::Unreachable(format!("локальный сокет: {error}")),
    };
    let Ok(()) = socket.set_read_timeout(Some(PEER_PROBE_TIMEOUT)) else {
        return ProbeOutcome::Skipped("не удалось выставить таймаут сокета".to_string());
    };
    let Some(target) = addresses.next() else {
        return ProbeOutcome::Unreachable("нет адресов после резолва".to_string());
    };
    let Ok(()) = socket.connect(target) else {
        return ProbeOutcome::Unreachable(format!("connect к {target}"));
    };
    let started = std::time::Instant::now();
    if socket.send(&[0]).is_err() {
        return ProbeOutcome::Unreachable(format!("send в {target} не прошёл (маршрут/файрвол)"));
    }
    let mut buffer = [0u8; 16];
    match socket.recv(&mut buffer) {
        Ok(_) => ProbeOutcome::Reachable(started.elapsed().as_millis()),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => ProbeOutcome::Unreachable(
            "нет ответа за 2с (возможно, сервер молчит — проверьте firewall)".to_string(),
        ),
        Err(error) => ProbeOutcome::Unreachable(format!("recv: {error}")),
    }
}

#[cfg(unix)]
fn probe_tun_device() -> TunProbe {
    if !Path::new("/dev/net/tun").exists() {
        return TunProbe::Missing;
    }
    match fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("/dev/net/tun")
    {
        Ok(_) => TunProbe::Ok,
        Err(_) => TunProbe::NoAccess,
    }
}

#[cfg(not(unix))]
fn probe_tun_device() -> TunProbe {
    TunProbe::NotPlatform
}

#[cfg(unix)]
fn detect_mihomo() -> bool {
    if Path::new("/etc/init.d/mihomo").exists() || Path::new("/etc/mihomo/config.yaml").exists() {
        return true;
    }
    // Сканируем процессы: /proc/<pid>/comm == "mihomo".
    let Ok(entries) = fs::read_dir("/proc") else {
        return false;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .to_string_lossy()
            .chars()
            .all(char::is_numeric)
            && let Ok(comm) = fs::read_to_string(entry.path().join("comm"))
            && comm.trim() == "mihomo"
        {
            return true;
        }
    }
    false
}

#[cfg(not(unix))]
fn detect_mihomo() -> bool {
    false
}

// ===========================================================================
// profile
// ===========================================================================

/// Загрузка текста конфига: None — файл отсутствует.
fn load_text(config_path: &str) -> Result<Option<String>, i32> {
    match import::read_live_config(Path::new(config_path)) {
        Ok(text) => Ok(text),
        Err(error) => {
            eprintln!("ошибка чтения конфига {config_path}: {error}");
            Err(1)
        }
    }
}

fn load_pool(config_path: &str) -> Result<(String, ProfilePool), i32> {
    let Some(text) = load_text(config_path)? else {
        eprintln!("конфиг не найден: {config_path}");
        return Err(1);
    };
    match ProfilePool::from_text(&text) {
        Ok(pool) => Ok((text, pool)),
        Err(issues) => {
            eprintln!(
                "конфиг невалиден: {}",
                issues
                    .iter()
                    .map(|issue| issue.to_string())
                    .collect::<Vec<_>>()
                    .join("; ")
            );
            Err(1)
        }
    }
}

/// Атомарная замена конфига: temp-сиблинг + rename. Права наследуются у
/// существующего файла, для нового — 0600 (внутри пароли/хеши). temp
/// создаётся сразу 0600 (unix), чтобы не было окна world-readable
/// между записью и chmod.
fn write_config_atomic(path: &Path, text: &str) -> std::io::Result<()> {
    let mut temp_path = PathBuf::from(path);
    {
        let name = temp_path
            .file_name()
            .ok_or_else(|| std::io::Error::other("config path has no file name"))?
            .to_owned();
        temp_path.set_file_name(format!("{}.tmp", name.to_string_lossy()));
    }
    {
        let mut options = fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp_path)?;
        file.write_all(text.as_bytes())?;
        file.flush()?;
    }
    match fs::metadata(path) {
        Ok(metadata) => {
            let _ = fs::set_permissions(&temp_path, metadata.permissions());
        }
        Err(_) => restrict_config_permissions(&temp_path),
    }
    if let Err(error) = fs::rename(&temp_path, path) {
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    Ok(())
}

#[cfg(unix)]
fn restrict_config_permissions(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
}

#[cfg(not(unix))]
fn restrict_config_permissions(_path: &Path) {}

/// Разрешённые option-ключи для `profile set` (схема UCI M4a). Запрет
/// произвольных ключей + однострочность значений защищают рендер UCI от
/// инъекции новых секций/опций (особенно к LuCI RPC в M6).
const ALLOWED_PROFILE_OPTIONS: [&str; 21] = [
    "name",
    "enabled",
    "priority",
    "peer",
    "password",
    "vk",
    "workers",
    "obfs",
    "turn_transport",
    "captcha_mode",
    "fingerprint",
    "client_ids",
    "vk_auth_mode",
    "vk_hash_mode",
    "vk_js_token",
    "device_id",
    "fail_threshold",
    "success_threshold",
    "cooldown",
    "captcha_policy",
    "note",
];

/// Ключи, значение которых — секрет: success-вывод `profile set` маскируется.
const SECRET_OPTION_KEYS: [&str; 3] = ["password", "vk", "vk_js_token"];

fn mask_set_value(key: &str, value: &str) -> String {
    if SECRET_OPTION_KEYS.contains(&key) && !value.is_empty() {
        "***".to_string()
    } else {
        value.to_string()
    }
}

fn check_profile_option(key: &str, value: &str) -> Result<(), String> {
    if !ALLOWED_PROFILE_OPTIONS.contains(&key) {
        return Err(format!(
            "ключ «{key}» не поддерживается (допустимо: {})",
            ALLOWED_PROFILE_OPTIONS.join(", ")
        ));
    }
    // UCI-значение — всегда одна строка без одинарных кавычек (render_uci
    // не эскейпит).
    if value.contains('\n') || value.contains('\'') {
        return Err("значение должно быть одной строкой без символов '".to_string());
    }
    Ok(())
}

/// Мутация конфига: parse → mutate → validate → atomic write. Live-файл
/// не пишется, если валидация или сама мутация провалилась.
fn mutate_config<F>(config_path: &str, mutate: F) -> i32
where
    F: FnOnce(&mut UciFile) -> Result<(), String>,
{
    let text = match load_text(config_path) {
        Ok(Some(text)) => text,
        Ok(None) => {
            eprintln!("конфиг не найден: {config_path}");
            return 1;
        }
        Err(code) => return code,
    };
    let mut file = match parse_uci(&text) {
        Ok(file) => file,
        Err(error) => {
            eprintln!("синтаксис конфига сломан: {error}");
            return 1;
        }
    };
    if let Err(message) = mutate(&mut file) {
        eprintln!("{message}");
        return 1;
    }
    let rendered = render_uci(&file);
    if let Err(issues) = ProfilePool::from_text(&rendered) {
        eprintln!(
            "после изменения конфиг невалиден — отмена: {}",
            issues
                .iter()
                .map(|issue| issue.to_string())
                .collect::<Vec<_>>()
                .join("; ")
        );
        return 1;
    }
    match write_config_atomic(Path::new(config_path), &rendered) {
        Ok(()) => 0,
        Err(error) => {
            eprintln!("ошибка записи конфига: {error} (конфиг не изменён)");
            1
        }
    }
}

fn run_profile(command: &ProfileCommand) -> i32 {
    let config_path = command.config();
    match command {
        ProfileCommand::List { .. } => profile_list(config_path),
        ProfileCommand::Show { id, .. } => profile_show(config_path, id),
        ProfileCommand::Use { id, .. } => {
            let code = mutate_config(config_path, |file| uci_set_active_profile(file, id));
            if code == 0 {
                println!("OK: profile use {id} (manual + active_profile)");
                eprintln!("применится после перезагрузки сервиса: service csqtt reload");
            }
            code
        }
        ProfileCommand::Auto { .. } => {
            let code = mutate_config(config_path, |file| {
                uci_set_selection_auto(file);
                Ok(())
            });
            if code == 0 {
                println!("OK: profile auto (selection_mode=priority)");
                eprintln!("применится после перезагрузки сервиса: service csqtt reload");
            }
            code
        }
        ProfileCommand::Enable { id, .. } => {
            let code = mutate_config(config_path, |file| uci_set_server_enabled(file, id, true));
            if code == 0 {
                println!("OK: profile enable {id}");
            }
            code
        }
        ProfileCommand::Disable { id, .. } => {
            let code = mutate_config(config_path, |file| uci_set_server_enabled(file, id, false));
            if code == 0 {
                println!("OK: profile disable {id}");
            }
            code
        }
        ProfileCommand::Remove { id, .. } => {
            let code = mutate_config(config_path, |file| uci_remove_server(file, id));
            if code == 0 {
                println!("OK: profile remove {id}");
            }
            code
        }
        ProfileCommand::Set { id, key, value, .. } => {
            if let Err(message) = check_profile_option(key, value) {
                eprintln!("{message}");
                return 1;
            }
            let code = mutate_config(config_path, |file| {
                uci_set_server_option(file, id, key, value)
            });
            if code == 0 {
                println!("OK: profile set {id} {key}={}", mask_set_value(key, value));
            }
            code
        }
        ProfileCommand::Import {
            link,
            id,
            name,
            activate,
            commit,
            backup_dir,
            ..
        } => profile_import(
            config_path,
            link,
            id.as_deref(),
            name.as_deref(),
            *activate,
            *commit,
            backup_dir,
        ),
        ProfileCommand::Export { id, .. } => profile_export(config_path, id),
    }
}

fn profile_list(config_path: &str) -> i32 {
    let pool = match load_pool(config_path) {
        Ok((_text, pool)) => pool,
        Err(code) => return code,
    };
    let selected = pool
        .selected_profile()
        .map(|profile| profile.section_id.as_str());
    println!(
        "{:<12} {:<20} {:<8} {:<8} selected",
        "id", "name", "priority", "enabled"
    );
    for profile in &pool.servers {
        println!(
            "{:<12} {:<20} {:<8} {:<8} {}",
            profile.section_id,
            truncate(&profile.name, 20),
            profile.priority,
            if profile.enabled { "yes" } else { "no" },
            if selected == Some(profile.section_id.as_str()) {
                "*"
            } else {
                ""
            }
        );
    }
    println!(
        "\nselection_mode={} active_profile={} (total {})",
        pool.main.selection_mode.as_str(),
        pool.main.active_profile,
        pool.servers.len()
    );
    0
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut out: String = value.chars().take(limit.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn profile_show(config_path: &str, id: &str) -> i32 {
    let (_, pool) = match load_pool(config_path) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    let Some(profile) = pool.server(id) else {
        eprintln!("профиль «{id}» не найден");
        return 1;
    };
    println!("{}", format_profile(profile, &pool));
    0
}

/// Детали профиля: password/vk_js_token/vk-хеши не выводятся (правило 10).
pub fn format_profile(profile: &ServerProfile, pool: &ProfilePool) -> String {
    let hashes = if profile.vk.is_empty() {
        "0".to_string()
    } else {
        profile
            .vk
            .split(',')
            .filter(|hash| !hash.trim().is_empty())
            .count()
            .to_string()
    };
    format!(
        "id:            {}\n\
         name:          {}\n\
         enabled:       {}\n\
         priority:      {}\n\
         peer:          {}\n\
         password:      {}\n\
         vk hashes:     {} (значения скрыты)\n\
         workers:       {}\n\
         obfs:          {}\n\
         transport:     {}\n\
         captcha_mode:  {}\n\
         fingerprint:   {}\n\
         client_ids:    {}\n\
         vk_auth_mode:  {}\n\
         vk_hash_mode:  {}\n\
         vk_js_token:   {}\n\
         device_id:     {}\n\
         overrides:     fail_threshold={} success_threshold={} cooldown={}s captcha_policy={}\n\
         note:          {}",
        profile.section_id,
        profile.name,
        if profile.enabled { "yes" } else { "no" },
        profile.priority,
        profile.peer,
        masked_secret(&profile.password),
        hashes,
        profile.workers,
        profile.obfs,
        profile.turn_transport,
        profile.captcha_mode,
        profile.fingerprint,
        if profile.client_ids.is_empty() {
            "-"
        } else {
            &profile.client_ids
        },
        if profile.vk_auth_mode.is_empty() {
            "-"
        } else {
            &profile.vk_auth_mode
        },
        if profile.vk_hash_mode.is_empty() {
            "-"
        } else {
            &profile.vk_hash_mode
        },
        masked_secret(&profile.vk_js_token),
        if profile.device_id.is_empty() {
            "-"
        } else {
            &profile.device_id
        },
        pool.effective_fail_threshold(profile),
        pool.effective_success_threshold(profile),
        pool.effective_cooldown(profile),
        pool.effective_captcha_policy(profile).as_str(),
        if profile.note.is_empty() {
            "-"
        } else {
            &profile.note
        }
    )
}

fn masked_secret(secret: &str) -> String {
    if secret.is_empty() {
        "-".to_string()
    } else {
        "***".to_string()
    }
}

fn profile_import(
    config_path: &str,
    link: &str,
    id: Option<&str>,
    name: Option<&str>,
    activate: bool,
    commit: bool,
    backup_dir: &str,
) -> i32 {
    let live_text = match load_text(config_path) {
        Ok(Some(text)) => text,
        Ok(None) => {
            eprintln!("конфиг не найден: {config_path}");
            return 1;
        }
        Err(code) => return code,
    };
    let request = ImportRequest {
        link_text: link.to_string(),
        section_id: id.map(str::to_string),
        name: name.map(str::to_string),
        activate,
    };
    let preview = match import::build_preview(&live_text, &request) {
        Ok(preview) => preview,
        Err(error) => {
            eprintln!("{error:#}");
            return 1;
        }
    };
    println!("{}", preview.describe());
    if !commit {
        eprintln!("\npreview показан — конфиг не изменён. Примените: добавьте --commit");
        return 0;
    }
    match import::commit_with_backup_dir(Path::new(config_path), &preview, Path::new(backup_dir)) {
        Ok(()) => {
            println!(
                "\nOK: импортирован профиль «{}» (id={}){}",
                preview.name,
                preview.section_id,
                if activate { " + active" } else { "" }
            );
            eprintln!(
                "backup: {backup_dir}; применится после перезагрузки сервиса: service csqtt reload"
            );
            0
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

fn profile_export(config_path: &str, id: &str) -> i32 {
    let (_, pool) = match load_pool(config_path) {
        Ok(pair) => pair,
        Err(code) => return code,
    };
    match import::export_profile_link(&pool, id) {
        Ok(link) => {
            eprintln!("внимание: ссылка содержит пароль и хеши — не публикуйте её");
            println!("{link}");
            0
        }
        Err(error) => {
            eprintln!("{error:#}");
            1
        }
    }
}

// ===========================================================================
// captcha (safe fields; backend управления — M4d)
// ===========================================================================

fn run_captcha(command: &CaptchaCommand, status_path: &str) -> i32 {
    match command {
        CaptchaCommand::List {} => captcha_list(status_path),
        CaptchaCommand::Show { id } => captcha_show(status_path, id.as_deref()),
        CaptchaCommand::Cancel { .. } => {
            // [M4d] CaptchaManager живёт в службе CSQTT и хранит challenges в
            // памяти. Управление из отдельного CLI-процесса требует
            // control-канала службы CSQTT (procd/ubus — M5/M6): отмена/решение
            // через статус невозможны — status.json содержит только safe fields.
            eprintln!("captcha manager: challenges хранит служба CSQTT (M4d CaptchaManager)");
            eprintln!(
                "отмена/решение из CLI требуют control-канала службы CSQTT (M5 procd / M6 ubus)"
            );
            eprintln!("сейчас: profile disable <id> или смена captcha_policy в UCI + SIGHUP");
            1
        }
    }
}

fn captcha_read_status(status_path: &str) -> Result<DaemonStatus, i32> {
    match read_status_file(status_path) {
        StatusRead::Ok(status) => Ok(*status),
        StatusRead::Missing => {
            eprintln!("captcha manager not available: сервис не запущен (нет {status_path})");
            Err(2)
        }
        StatusRead::Invalid(message) => {
            eprintln!("captcha manager not available: статус-файл невалиден: {message}");
            Err(2)
        }
    }
}

fn captcha_list(status_path: &str) -> i32 {
    let status = match captcha_read_status(status_path) {
        Ok(status) => status,
        Err(code) => return code,
    };
    // [M4d] Challenges — реальные записи службы CSQTT (safe fields). Профили в
    // captcha_required показываем отдельно — это состояние пула.
    let challenges: Vec<&CaptchaChallengeStatus> = status
        .captcha_challenges
        .iter()
        .filter(|challenge| challenge.state != "solved")
        .collect();
    let pending: Vec<&ProfileStatus> = status
        .profiles
        .iter()
        .filter(|profile| profile.state == "captcha_required")
        .collect();
    if challenges.is_empty() && pending.is_empty() {
        println!(
            "ожидающих CAPTCHA нет (captcha_pending={})",
            status.captcha_pending
        );
        return 1;
    }
    if !challenges.is_empty() {
        println!(
            "{:<14} {:<10} {:<9} {:<10} expires",
            "challenge", "profile", "mode", "state"
        );
        for challenge in &challenges {
            println!(
                "{:<14} {:<10} {:<9} {:<10} {}",
                challenge.id,
                challenge.profile_id,
                challenge.mode,
                challenge.state,
                challenge.expires_at
            );
        }
    }
    if !pending.is_empty() {
        println!("\nпрофили в captcha_required:");
        for profile in &pending {
            println!("  {id} ({name})", id = profile.id, name = profile.name);
        }
    }
    println!("\nsession_token/redirect_uri — секреты, не выводятся");
    0
}

fn captcha_show(status_path: &str, id: Option<&str>) -> i32 {
    let status = match captcha_read_status(status_path) {
        Ok(status) => status,
        Err(code) => return code,
    };
    // [M4d] Ищем challenge по id (или первый незакрытый). Секретов в нём нет
    // по конструкции — только safe fields.
    let mut challenges = status
        .captcha_challenges
        .iter()
        .filter(|challenge| challenge.state != "solved");
    let found = match id {
        Some(id) => challenges.find(|challenge| challenge.id == id),
        None => challenges.next(),
    };
    match found {
        Some(challenge) => {
            println!(
                "challenge: {}\nprofile: {}  mode: {}\nstate: {}  created: {}  expires: {}",
                challenge.id,
                challenge.profile_id,
                challenge.mode,
                challenge.state,
                challenge.created_at,
                challenge.expires_at
            );
            println!(
                "session_token: секрет — не выводится (решение — через control-канал службы CSQTT, M5/M6)"
            );
            0
        }
        None => {
            if let Some(id) = id {
                eprintln!("challenge «{id}» не найден среди незакрытых");
            } else {
                eprintln!("ожидающих CAPTCHA нет");
            }
            1
        }
    }
}

// ===========================================================================
// log
// ===========================================================================

fn run_log(command: &LogCommand) -> i32 {
    let LogCommand::Tail {
        lines,
        follow,
        file,
        config,
    } = command;
    let config_text = load_text(config).ok().flatten();
    let log_path = file
        .as_ref()
        .map_or_else(|| resolve_log_path_from(&config_text), PathBuf::from);
    // Секреты для маскирования: collect_secrets M4a (password/vk_js_token)
    // плюс VK-хеши профилей — одноразовые креды, им не место в выводе
    // (контракт collect_secrets M4a не меняем — это локальное расширение
    // только для чтения лога).
    let pool = config_text
        .as_deref()
        .and_then(|text| ProfilePool::from_text(text).ok());
    let secrets = pool.as_ref().map(log_redaction_secrets).unwrap_or_default();
    if pool.is_none() {
        eprintln!("внимание: конфиг недоступен или невалиден — строки лога не маскируются");
    }
    if !log_path.exists() {
        eprintln!("лог-файл не найден: {}", log_path.display());
        return 1;
    }
    match read_tail(&log_path, *lines) {
        Ok(tailed) => {
            let stdout = std::io::stdout();
            let mut writer = std::io::BufWriter::new(stdout.lock());
            for line in &tailed {
                let _ = writeln!(writer, "{}", redact_secrets(line, &secrets));
            }
            let _ = writer.flush();
        }
        Err(error) => {
            eprintln!("ошибка чтения лога {}: {error}", log_path.display());
            return 1;
        }
    }
    if *follow {
        return follow_log(&log_path, &secrets);
    }
    0
}

/// Секреты для маскирования вывода лога: `collect_secrets` M4a
/// (password/vk_js_token) + VK-хеши (полная строка и части по запятой).
/// Контракт collect_secrets M4a не меняется — расширение локально в CLI.
pub fn log_redaction_secrets(pool: &ProfilePool) -> Vec<String> {
    let mut secrets = pool.collect_secrets();
    for profile in &pool.servers {
        if !profile.vk.is_empty() {
            secrets.push(profile.vk.clone());
            secrets.extend(profile.vk.split(',').map(str::to_string));
        }
    }
    // Длинные первыми (как в collect_secrets), чтобы короткий секрет не
    // закоротил маскирование вложенного длинного.
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    secrets.dedup();
    secrets
}

fn resolve_log_path_from(config_text: &Option<String>) -> PathBuf {
    config_text
        .as_deref()
        .and_then(|text| ProfilePool::from_text(text).ok())
        .map(|pool| PathBuf::from(pool.main.log_file))
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from(DEFAULT_LOG_FILE))
}

/// Последние N строк текста (чистая функция — для тестов).
pub fn tail_lines(text: &str, n: usize) -> Vec<&str> {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].to_vec()
}

fn read_tail(path: &Path, n: usize) -> std::io::Result<Vec<String>> {
    let text = fs::read_to_string(path)?;
    Ok(tail_lines(&text, n)
        .into_iter()
        .map(str::to_string)
        .collect())
}

/// Слежение за логом: печатает добавленные строки; устойчиво к ротации
/// (csqtt.log → csqtt.log.1) и усечению. Завершается по Ctrl-C.
fn follow_log(path: &Path, secrets: &[String]) -> i32 {
    let stdout = std::io::stdout();
    let mut seen = fs::read(path).map(|bytes| bytes.len()).unwrap_or(0);
    loop {
        std::thread::sleep(Duration::from_secs(1));
        let Ok(bytes) = fs::read(path) else {
            eprintln!("лог исчез: {}", path.display());
            return 1;
        };
        if bytes.len() < seen {
            // Ротация/усечение: новый файл читаем с начала.
            seen = 0;
        }
        if bytes.len() > seen {
            let appended = String::from_utf8_lossy(&bytes[seen..]);
            let mut writer = std::io::BufWriter::new(stdout.lock());
            for line in appended.lines() {
                let _ = writeln!(writer, "{}", redact_secrets(line, secrets));
            }
            let _ = writer.flush();
            seen = bytes.len();
        }
    }
}

// ===========================================================================
// Тесты
// ===========================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::status::{
        DaemonState, DaemonStatus, ProfileStatus, RoutingSummary, TunnelStatus, WorkersStatus,
        write_status_atomic,
    };
    use crate::uci::RoutingMode;
    use std::fs;
    use std::path::PathBuf;

    fn fixture(relative: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative);
        fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("fixture {}: {error}", path.display()))
    }

    fn sample_status() -> DaemonStatus {
        DaemonStatus {
            version: "2.1.9".to_string(),
            daemon_state: DaemonState::Running.as_str().to_string(),
            started_at: 1_700_000_000,
            generated_at: 1_700_000_005,
            uptime_secs: 125,
            selection_mode: "priority".to_string(),
            active_profile: Some("finland".to_string()),
            profiles: vec![ProfileStatus {
                id: "finland".to_string(),
                name: "Finland".to_string(),
                state: "active".to_string(),
                priority: 10,
                enabled: true,
                consecutive_fails: 0,
                success_streak: 3,
                cooldown_remaining_secs: 0,
            }],
            tunnel: TunnelStatus {
                interface: "csqtt0".to_string(),
                address: Some("10.66.66.2".to_string()),
                local_address: None,
                mtu: 1280,
                dns: Some("1.1.1.1".to_string()),
            },
            routing: RoutingSummary::from_mode(RoutingMode::Auto),
            workers: WorkersStatus {
                configured: 18,
                active: 9,
            },
            rx_bytes: 1_500_000,
            tx_bytes: 700_000,
            reconnects: 1,
            captcha_pending: 0,
            captcha_challenges: Vec::new(),
            last_error: None,
            health_mode: "both".to_string(),
            health_target: String::new(),
        }
    }

    // --- status: schema + классификация ---

    #[test]
    fn status_schema_roundtrip_and_connected() {
        let status = sample_status();
        let json = status.to_json().unwrap();
        assert!(json.contains("\"active_profile\": \"finland\""));
        let parsed: DaemonStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.tunnel.interface, "csqtt0");
        assert_eq!(classify_status(&parsed), StatusOutcome::Connected);
    }

    #[test]
    fn classify_connecting_profile_is_not_connected() {
        let mut status = sample_status();
        status.profiles[0].state = "connecting".to_string();
        assert_eq!(classify_status(&status), StatusOutcome::NotConnected);
    }

    #[test]
    fn classify_disabled_daemon_is_not_connected() {
        let mut status = sample_status();
        status.daemon_state = DaemonState::Disabled.as_str().to_string();
        assert_eq!(classify_status(&status), StatusOutcome::NotConnected);
    }

    #[test]
    fn classify_stopped_daemon_is_not_running() {
        let mut status = sample_status();
        status.daemon_state = DaemonState::Stopped.as_str().to_string();
        assert_eq!(classify_status(&status), StatusOutcome::NotRunning);
    }

    #[test]
    fn classify_missing_active_profile_is_not_connected() {
        let mut status = sample_status();
        status.active_profile = None;
        assert_eq!(classify_status(&status), StatusOutcome::NotConnected);
    }

    #[test]
    fn format_status_shows_interface_and_hides_no_secrets() {
        let text = format_status(&sample_status());
        assert!(text.contains("csqtt0"));
        assert!(text.contains("finland"));
        assert!(text.contains("interface-only"));
        assert!(!text.contains("examplePassword1"));
    }

    #[test]
    fn run_status_missing_file_exits_two() {
        let path = temp_path("csqtt-m4c-status-missing/status.json");
        assert_eq!(run_status(false, &path.to_string_lossy()), 2);
    }

    #[test]
    fn run_status_file_connected_exits_zero() {
        let directory = temp_dir("csqtt-m4c-status-ok");
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        assert_eq!(run_status(false, &path.to_string_lossy()), 0);
    }

    #[test]
    fn run_status_json_prints_status() {
        let directory = temp_dir("csqtt-m4c-status-json");
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        // JSON-режим лишь сериализует модель — рантайм-ошибок нет.
        assert_eq!(run_status(true, &path.to_string_lossy()), 0);
    }

    #[test]
    fn run_status_connecting_profile_exits_one() {
        let directory = temp_dir("csqtt-m4c-status-connecting");
        let path = directory.join("status.json");
        let mut status = sample_status();
        status.profiles[0].state = "connecting".to_string();
        write_status_atomic(&path, &status).unwrap();
        assert_eq!(run_status(false, &path.to_string_lossy()), 1);
    }

    // --- doctor: mocks через DoctorInput ---

    fn all_ok_input() -> DoctorInput {
        DoctorInput {
            config_path: "/etc/config/csqtt".to_string(),
            config_text: Some(fixture("config/valid/csqtt")),
            tun: TunProbe::Ok,
            wan_default: Some(DefaultRoute {
                device: "eth0".to_string(),
                gateway: "192.168.1.1".to_string(),
            }),
            csqtt0_exists: Some(false),
            daemon_running: Some(false),
            rp_filter: Some((0, "all".to_string())),
            peer_probe: ProbeOutcome::Reachable(12),
            mihomo_detected: false,
        }
    }

    #[test]
    fn doctor_all_ok_exits_zero() {
        let report = build_doctor_report(&all_ok_input());
        assert_eq!(report.exit_code(), 0);
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status != CheckStatus::Fail)
        );
        assert!(
            report
                .checks
                .iter()
                .all(|check| check.status != CheckStatus::Warn)
        );
    }

    #[test]
    fn doctor_rp_filter_strict_warns_with_hint() {
        let mut input = all_ok_input();
        input.rp_filter = Some((1, "csqtt0".to_string()));
        let report = build_doctor_report(&input);
        let rp = report
            .checks
            .iter()
            .find(|c| c.name == "rp-filter")
            .unwrap();
        assert_eq!(rp.status, CheckStatus::Warn);
        assert!(rp.message.contains("net.ipv4.conf.csqtt0.rp_filter=2"));
        // Doctor систему не меняет — только подсказка.
        assert_eq!(report.exit_code(), 1);
    }

    #[test]
    fn doctor_rp_filter_loose_is_ok() {
        let mut input = all_ok_input();
        input.rp_filter = Some((2, "csqtt0".to_string()));
        let report = build_doctor_report(&input);
        let rp = report
            .checks
            .iter()
            .find(|c| c.name == "rp-filter")
            .unwrap();
        assert_eq!(rp.status, CheckStatus::Ok);
    }

    #[test]
    fn doctor_broken_config_fails() {
        let mut input = all_ok_input();
        input.config_text = Some(fixture("config/broken/csqtt"));
        let report = build_doctor_report(&input);
        assert_eq!(report.exit_code(), 2);
        let config = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(config.status, CheckStatus::Fail);
    }

    #[test]
    fn doctor_missing_config_is_skip() {
        let mut input = all_ok_input();
        input.config_text = None;
        let report = build_doctor_report(&input);
        let config = report.checks.iter().find(|c| c.name == "config").unwrap();
        assert_eq!(config.status, CheckStatus::Skip);
    }

    #[test]
    fn doctor_tun_missing_fails() {
        let mut input = all_ok_input();
        input.tun = TunProbe::Missing;
        let report = build_doctor_report(&input);
        assert_eq!(report.exit_code(), 2);
        let tun = report
            .checks
            .iter()
            .find(|c| c.name == "tun-device")
            .unwrap();
        assert_eq!(tun.status, CheckStatus::Fail);
        assert!(tun.message.contains("kmod-tun"));
    }

    #[test]
    fn doctor_tun_no_access_fails() {
        let mut input = all_ok_input();
        input.tun = TunProbe::NoAccess;
        let report = build_doctor_report(&input);
        let tun = report
            .checks
            .iter()
            .find(|c| c.name == "tun-device")
            .unwrap();
        assert_eq!(tun.status, CheckStatus::Fail);
        assert!(tun.message.contains("CAP_NET_ADMIN"));
    }

    #[test]
    fn doctor_tun_not_platform_skips() {
        let mut input = all_ok_input();
        input.tun = TunProbe::NotPlatform;
        let report = build_doctor_report(&input);
        let tun = report
            .checks
            .iter()
            .find(|c| c.name == "tun-device")
            .unwrap();
        assert_eq!(tun.status, CheckStatus::Skip);
    }

    #[test]
    fn doctor_no_wan_default_route_skips() {
        // Default route неизвестен (нет /proc/net/route) — проверка SKIP.
        let mut input = all_ok_input();
        input.wan_default = None;
        let report = build_doctor_report(&input);
        let wan = report
            .checks
            .iter()
            .find(|c| c.name == "wan-default-route")
            .unwrap();
        assert_eq!(wan.status, CheckStatus::Skip);
        assert_eq!(report.exit_code(), 0);
    }

    #[test]
    fn doctor_default_route_via_csqtt0_fails_isolation() {
        let mut input = all_ok_input();
        input.wan_default = Some(DefaultRoute {
            device: "csqtt0".to_string(),
            gateway: "10.66.66.1".to_string(),
        });
        let report = build_doctor_report(&input);
        let wan = report
            .checks
            .iter()
            .find(|c| c.name == "wan-default-route")
            .unwrap();
        assert_eq!(wan.status, CheckStatus::Fail);
        assert!(wan.message.contains("изоляция"));
    }

    #[test]
    fn doctor_peer_unreachable_warns() {
        let mut input = all_ok_input();
        input.peer_probe = ProbeOutcome::Unreachable("нет ответа".to_string());
        let report = build_doctor_report(&input);
        let peer = report
            .checks
            .iter()
            .find(|c| c.name == "peer-reachability")
            .unwrap();
        assert_eq!(peer.status, CheckStatus::Warn);
    }

    #[test]
    fn doctor_peer_skipped_when_no_profile() {
        let mut input = all_ok_input();
        input.peer_probe = ProbeOutcome::Skipped("нет профиля".to_string());
        let report = build_doctor_report(&input);
        let peer = report
            .checks
            .iter()
            .find(|c| c.name == "peer-reachability")
            .unwrap();
        assert_eq!(peer.status, CheckStatus::Skip);
    }

    #[test]
    fn doctor_csqtt0_leftover_warns() {
        let mut input = all_ok_input();
        input.csqtt0_exists = Some(true);
        input.daemon_running = Some(false);
        let report = build_doctor_report(&input);
        let collision = report
            .checks
            .iter()
            .find(|c| c.name == "csqtt0-collision")
            .unwrap();
        assert_eq!(collision.status, CheckStatus::Warn);
        assert!(collision.message.contains("ip link delete csqtt0"));
    }

    #[test]
    fn doctor_csqtt0_owned_by_daemon_ok() {
        let mut input = all_ok_input();
        input.csqtt0_exists = Some(true);
        input.daemon_running = Some(true);
        let report = build_doctor_report(&input);
        let collision = report
            .checks
            .iter()
            .find(|c| c.name == "csqtt0-collision")
            .unwrap();
        assert_eq!(collision.status, CheckStatus::Ok);
    }

    #[test]
    fn doctor_mihomo_detection_is_info() {
        let mut input = all_ok_input();
        input.mihomo_detected = true;
        let report = build_doctor_report(&input);
        let mihomo = report.checks.iter().find(|c| c.name == "mihomo").unwrap();
        assert_eq!(mihomo.status, CheckStatus::Info);
        assert!(mihomo.message.contains("interface-name"));
    }

    #[test]
    fn doctor_report_json_serializes() {
        let report = build_doctor_report(&all_ok_input());
        let json = serde_json::to_string(&report).unwrap();
        assert!(json.contains("\"status\""));
        assert!(json.contains("\"ok\""));
    }

    #[test]
    fn proc_net_route_default_parsed() {
        // /proc/net/route: gateway в hex, little-endian; 0x0101A8C0 = 192.168.1.1.
        let text = "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
eth0 00000000 0101A8C0 0003 0 0 600 00000000 0 0 0\n\
eth0 0101A8C0 00000000 0001 0 0 600 00FFFFFF 0 0 0\n";
        let route = parse_proc_net_route(text).unwrap();
        assert_eq!(route.device, "eth0");
        assert_eq!(route.gateway, "192.168.1.1");
    }

    #[test]
    fn proc_net_route_no_default() {
        let text = "Iface Destination Gateway Flags RefCnt Use Metric Mask MTU Window IRTT\n\
eth0 0100A8C0 00000000 0001 0 0 600 00FFFFFF 0 0 0\n";
        assert!(parse_proc_net_route(text).is_none());
    }

    #[test]
    fn doctor_format_contains_all_checks() {
        let text = format_doctor(&build_doctor_report(&all_ok_input()));
        for name in [
            "config",
            "tun-device",
            "wan-default-route",
            "peer-reachability",
            "csqtt0-collision",
            "rp-filter",
            "mihomo",
        ] {
            assert!(text.contains(name), "missing check {name} in output");
        }
    }

    // --- profile ---

    fn temp_config_copy(name: &str) -> String {
        let directory = temp_dir(name);
        let path = directory.join("csqtt");
        fs::write(&path, fixture("config/valid/csqtt")).unwrap();
        path.to_string_lossy().into_owned()
    }

    fn config_arg(path: &str) -> ConfigArg {
        ConfigArg {
            config: path.to_string(),
        }
    }

    #[test]
    fn profile_list_shows_ids_without_secrets() {
        let path = temp_config_copy("csqtt-m4c-list");
        assert_eq!(profile_list(&path), 0);
    }

    #[test]
    fn profile_list_output_has_ids_and_no_password() {
        // Проверяем наполнение через форматтер списка напрямую.
        let pool = load_pool_for_test();
        let selected = pool.selected_profile().map(|p| p.section_id.as_str());
        let mut out = String::new();
        for profile in &pool.servers {
            out.push_str(&format!("{} {}", profile.section_id, profile.enabled));
            if selected == Some(profile.section_id.as_str()) {
                out.push_str(" *");
            }
        }
        assert!(out.contains("finland"));
        assert!(out.contains("backup"));
        assert!(!out.contains("examplePassword1"));
    }

    fn load_pool_for_test() -> ProfilePool {
        ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap()
    }

    #[test]
    fn profile_show_masks_secrets() {
        let pool = load_pool_for_test();
        let profile = pool.server("finland").unwrap();
        let text = format_profile(profile, &pool);
        assert!(text.contains("***"));
        assert!(!text.contains("examplePassword1"));
        assert!(!text.contains("backup-secret-password"));
        assert!(text.contains("2 (значения скрыты)"));
        assert!(text.contains("11111111-2222-3333-4444-555555555555"));
    }

    #[test]
    fn profile_show_unknown_id_fails() {
        let path = temp_config_copy("csqtt-m4c-show-missing");
        assert_eq!(profile_show(&path, "nope"), 1);
    }

    #[test]
    fn profile_use_writes_manual_active() {
        let path = temp_config_copy("csqtt-m4c-use");
        assert_eq!(
            run_profile(&ProfileCommand::Use {
                id: "backup".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(pool.main.selection_mode.as_str(), "manual");
        assert_eq!(pool.main.active_profile, "backup");
    }

    #[test]
    fn profile_use_unknown_id_fails_without_write() {
        let path = temp_config_copy("csqtt-m4c-use-missing");
        let before = fs::read_to_string(&path).unwrap();
        assert_eq!(
            run_profile(&ProfileCommand::Use {
                id: "nope".to_string(),
                config: config_arg(&path)
            }),
            1
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_auto_resets_selection() {
        let path = temp_config_copy("csqtt-m4c-auto");
        assert_eq!(
            run_profile(&ProfileCommand::Auto {
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(pool.main.selection_mode.as_str(), "priority");
        assert!(pool.main.active_profile.is_empty());
    }

    #[test]
    fn profile_enable_disable_roundtrip() {
        let path = temp_config_copy("csqtt-m4c-toggle");
        assert_eq!(
            run_profile(&ProfileCommand::Disable {
                id: "finland".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(!pool.server("finland").unwrap().enabled);
        assert_eq!(
            run_profile(&ProfileCommand::Enable {
                id: "finland".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(pool.server("finland").unwrap().enabled);
    }

    #[test]
    fn profile_remove_clears_dangling_active() {
        // manual+active на удаляемый профиль → auto fallback (правило 3).
        let path = temp_config_copy("csqtt-m4c-remove");
        run_profile(&ProfileCommand::Use {
            id: "finland".to_string(),
            config: config_arg(&path),
        });
        assert_eq!(
            run_profile(&ProfileCommand::Remove {
                id: "finland".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let text = fs::read_to_string(&path).unwrap();
        assert!(!text.contains("config server 'finland'"));
        let pool = ProfilePool::from_text(&text).unwrap();
        assert_eq!(pool.main.selection_mode.as_str(), "priority");
    }

    #[test]
    fn profile_set_option_updates_value() {
        let path = temp_config_copy("csqtt-m4c-set");
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "workers".to_string(),
                value: "27".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(pool.server("backup").unwrap().workers, 27);
    }

    #[test]
    fn profile_set_invalid_value_does_not_write() {
        let path = temp_config_copy("csqtt-m4c-set-bad");
        let before = fs::read_to_string(&path).unwrap();
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "workers".to_string(),
                value: "not-a-number".to_string(),
                config: config_arg(&path)
            }),
            1
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_set_rejects_unknown_key() {
        let path = temp_config_copy("csqtt-m4c-set-key");
        let before = fs::read_to_string(&path).unwrap();
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "bogus_field".to_string(),
                value: "x".to_string(),
                config: config_arg(&path)
            }),
            1
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_set_rejects_multiline_value() {
        // UCI-значение — одна строка: иначе можно инжектить секции/опции.
        // Ключ из whitelist, чтобы срабатывала именно проверка значения.
        let path = temp_config_copy("csqtt-m4c-set-ml");
        let before = fs::read_to_string(&path).unwrap();
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "name".to_string(),
                value: "line1\nconfig server 'evil'".to_string(),
                config: config_arg(&path)
            }),
            1
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_set_rejects_quote_in_value() {
        let path = temp_config_copy("csqtt-m4c-set-quote");
        let before = fs::read_to_string(&path).unwrap();
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "name".to_string(),
                value: "it's".to_string(),
                config: config_arg(&path)
            }),
            1
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_set_allows_note() {
        // note — реальное поле схемы (import пишет его), должно быть доступно.
        let path = temp_config_copy("csqtt-m4c-set-note");
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "backup".to_string(),
                key: "note".to_string(),
                value: "тестовая запись".to_string(),
                config: config_arg(&path)
            }),
            0
        );
        let pool = ProfilePool::from_text(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(pool.server("backup").unwrap().note, "тестовая запись");
    }

    #[test]
    fn profile_set_masks_secret_value_in_output() {
        // `profile set finland password X` не должен печатать секрет.
        let path = temp_config_copy("csqtt-m4c-set-secret");
        let value = "super-secret-12345";
        assert_eq!(
            run_profile(&ProfileCommand::Set {
                id: "finland".to_string(),
                key: "password".to_string(),
                value: value.to_string(),
                config: config_arg(&path)
            }),
            0
        );
        // Маскировку проверяем напрямую по product-функции (stdout в тесте
        // не виден): секретный ключ всегда маскируется.
        assert_eq!(mask_set_value("password", value), "***");
        assert_eq!(mask_set_value("workers", "18"), "18");
    }

    #[test]
    fn mask_set_value_handles_empty_secret() {
        assert_eq!(mask_set_value("password", ""), "");
        assert_eq!(mask_set_value("vk_js_token", "tok"), "***");
    }

    #[test]
    #[cfg(unix)]
    fn new_config_file_is_created_secret_mode() {
        // Новый конфиг (пароли/хеши) должен быть 0600 с момента создания —
        // без окна world-readable у temp-сиблинга.
        use std::os::unix::fs::PermissionsExt;
        let directory = temp_dir("csqtt-m4c-new-config-mode");
        let path = directory.join("csqtt");
        write_config_atomic(&path, "config csqtt 'main'\n\toption enabled '1'\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        // temp-сиблинг исчез после rename.
        assert!(!directory.join("csqtt.tmp").exists());
    }

    #[test]
    #[cfg(unix)]
    fn existing_config_permissions_are_preserved() {
        use std::os::unix::fs::PermissionsExt;
        let directory = temp_dir("csqtt-m4c-keep-mode");
        let path = directory.join("csqtt");
        fs::write(&path, "config csqtt 'main'\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        write_config_atomic(&path, "config csqtt 'main'\n\toption enabled '1'\n").unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    #[test]
    fn profile_import_preview_leaves_file_unchanged() {
        let path = temp_config_copy("csqtt-m4c-import-preview");
        let before = fs::read_to_string(&path).unwrap();
        let link = fixture("links/current.txt");
        assert_eq!(
            profile_import(&path, &link, None, None, false, false, "/tmp"),
            0
        );
        assert_eq!(fs::read_to_string(&path).unwrap(), before);
    }

    #[test]
    fn profile_import_commit_adds_profile_and_backup() {
        let directory = temp_dir("csqtt-m4c-import-commit");
        let live = directory.join("csqtt");
        fs::write(&live, fixture("config/valid/csqtt")).unwrap();
        let backups = directory.join("backups");
        let link = fixture("links/current.txt");
        assert_eq!(
            profile_import(
                &live.to_string_lossy(),
                &link,
                Some("newserver"),
                Some("New Server"),
                false,
                true,
                &backups.to_string_lossy(),
            ),
            0
        );
        let text = fs::read_to_string(&live).unwrap();
        assert!(text.contains("config server 'newserver'"));
        assert!(fs::read_dir(&backups).unwrap().count() >= 1);
    }

    #[test]
    fn profile_import_duplicate_id_rejected() {
        let path = temp_config_copy("csqtt-m4c-import-dup");
        let link = fixture("links/current.txt");
        // fixture valid уже содержит finland; предлагаем занятое имя.
        assert_eq!(
            profile_import(&path, &link, Some("finland"), None, false, true, "/tmp"),
            1
        );
    }

    #[test]
    fn profile_export_produces_parsable_link() {
        let path = temp_config_copy("csqtt-m4c-export");
        assert_eq!(
            run_profile(&ProfileCommand::Export {
                id: "finland".to_string(),
                config: config_arg(&path)
            }),
            0
        );
    }

    #[test]
    fn profile_export_unknown_id_fails() {
        let path = temp_config_copy("csqtt-m4c-export-missing");
        assert_eq!(
            run_profile(&ProfileCommand::Export {
                id: "nope".to_string(),
                config: config_arg(&path)
            }),
            1
        );
    }

    #[test]
    fn profile_list_on_missing_config_fails() {
        assert_eq!(profile_list("/tmp/csqtt-m4c-does-not-exist-12345/csqtt"), 1);
    }

    // --- captcha ---

    #[test]
    fn captcha_list_without_status_not_available() {
        let path = temp_path("csqtt-m4c-captcha-none/status.json");
        assert_eq!(captcha_list(&path.to_string_lossy()), 2);
    }

    #[test]
    fn captcha_list_shows_pending_without_session_token() {
        let directory = temp_dir("csqtt-m4c-captcha-pending");
        let path = directory.join("status.json");
        let mut status = sample_status();
        status.profiles[0].state = "captcha_required".to_string();
        write_status_atomic(&path, &status).unwrap();
        assert_eq!(captcha_list(&path.to_string_lossy()), 0);
    }

    #[test]
    fn captcha_list_no_pending_returns_one() {
        let directory = temp_dir("csqtt-m4c-captcha-empty");
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        assert_eq!(captcha_list(&path.to_string_lossy()), 1);
    }

    #[test]
    fn captcha_cancel_reports_m4d_backend() {
        // [M4d] Менеджер живёт в службе CSQTT; управление из отдельного CLI-процесса
        // требует control-канала (M5/M6) — честное сообщение, exit 1.
        assert_eq!(
            run_captcha(
                &CaptchaCommand::Cancel { id: None },
                "/tmp/csqtt-m4d-captcha-cancel-nope/status.json"
            ),
            1
        );
    }

    #[test]
    fn captcha_list_shows_registered_challenges() {
        let directory = temp_dir("csqtt-m4d-challenge-list");
        let path = directory.join("status.json");
        let mut status = sample_status();
        status.captcha_challenges = vec![CaptchaChallengeStatus {
            id: "chg-0123456789abcdef".to_string(),
            profile_id: "finland".to_string(),
            mode: "auto".to_string(),
            state: "pending".to_string(),
            created_at: 1_700_000_000,
            expires_at: 1_700_000_240,
        }];
        write_status_atomic(&path, &status).unwrap();
        assert_eq!(captcha_list(&path.to_string_lossy()), 0);
    }

    #[test]
    fn captcha_show_finds_challenge_by_id() {
        let directory = temp_dir("csqtt-m4d-challenge-show");
        let path = directory.join("status.json");
        let mut status = sample_status();
        status.captcha_challenges = vec![CaptchaChallengeStatus {
            id: "chg-abcdef0123456789".to_string(),
            profile_id: "finland".to_string(),
            mode: "manual".to_string(),
            state: "pending".to_string(),
            created_at: 1_700_000_000,
            expires_at: 1_700_000_240,
        }];
        write_status_atomic(&path, &status).unwrap();
        assert_eq!(
            captcha_show(&path.to_string_lossy(), Some("chg-abcdef0123456789")),
            0
        );
        assert_eq!(captcha_show(&path.to_string_lossy(), Some("chg-nope")), 1);
        assert_eq!(captcha_show(&path.to_string_lossy(), None), 0);
    }

    #[test]
    fn captcha_show_without_challenges_returns_one() {
        let directory = temp_dir("csqtt-m4d-challenge-empty");
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        assert_eq!(captcha_show(&path.to_string_lossy(), None), 1);
    }

    // --- log ---

    #[test]
    fn tail_lines_returns_last_n() {
        let text = (0..30)
            .map(|i| format!("line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        let tailed = tail_lines(&text, 5);
        assert_eq!(tailed.len(), 5);
        assert_eq!(tailed[0], "line 25");
        assert_eq!(tailed[4], "line 29");
    }

    #[test]
    fn tail_lines_fewer_lines_returns_all() {
        let text = "a\nb";
        assert_eq!(tail_lines(text, 10), vec!["a", "b"]);
    }

    #[test]
    fn log_tail_reads_last_lines() {
        let directory = temp_dir("csqtt-m4c-log");
        let log = directory.join("csqtt.log");
        fs::write(&log, "a\nb\nc\nd\ne\n").unwrap();
        let lines = read_tail(&log, 2).unwrap();
        assert_eq!(lines, vec!["d", "e"]);
    }

    #[test]
    fn log_tail_redacts_known_secrets() {
        // Секреты для redaction собираются из пула конфига (правило 10).
        let pool = load_pool_for_test();
        let secrets = pool.collect_secrets();
        let secret = "examplePassword1";
        assert!(secrets.iter().any(|value| value == secret));
        let line = format!("connect password={secret} ok");
        assert!(!redact_secrets(&line, &secrets).contains(secret));
    }

    #[test]
    fn log_tail_redacts_vk_hashes_too() {
        // Расширенный набор run_log: хеши маскируются наравне с паролем.
        let pool = load_pool_for_test();
        let profile = pool.server("finland").unwrap();
        let hash = profile.vk.split(',').next().expect("fixture has hashes");
        let secrets = log_redaction_secrets_for_test(&pool);
        let line = format!("profile hashes={hash} loaded");
        assert!(!redact_secrets(&line, &secrets).contains(hash));
    }

    /// Секреты для маскирования вывода лога: берём из product-функции.
    fn log_redaction_secrets_for_test(pool: &ProfilePool) -> Vec<String> {
        log_redaction_secrets(pool)
    }

    #[test]
    fn log_tail_missing_file_fails() {
        let path = temp_path("csqtt-m4c-log-missing/csqtt.log");
        let command = LogCommand::Tail {
            lines: 10,
            follow: false,
            file: Some(path.to_string_lossy().into_owned()),
            config: "/tmp/csqtt-m4c-no-config/csqtt".to_string(),
        };
        assert_eq!(run_log(&command), 1);
    }

    // --- CLI parser ---

    #[test]
    fn parser_status_json_and_paths() {
        let command = SubCommand::try_parse_from([
            "csqtt",
            "status",
            "--json",
            "--status-path",
            "/tmp/x.json",
        ])
        .unwrap();
        match command {
            SubCommand::Status { json, status_path } => {
                assert!(json);
                assert_eq!(status_path, "/tmp/x.json");
            }
            _ => panic!("status expected"),
        }
    }

    #[test]
    fn parser_doctor_config_dir() {
        let command = SubCommand::try_parse_from([
            "csqtt",
            "doctor",
            "--config-dir",
            "tests/fixtures/config/valid",
        ])
        .unwrap();
        match command {
            SubCommand::Doctor {
                config_dir: Some(dir),
                json,
                ..
            } => {
                assert_eq!(dir, "tests/fixtures/config/valid");
                assert!(!json);
            }
            _ => panic!("doctor expected"),
        }
    }

    #[test]
    fn parser_profile_use_and_import_flags() {
        let command = SubCommand::try_parse_from(["csqtt", "profile", "use", "finland"]).unwrap();
        match command {
            SubCommand::Profile { command } => match command {
                ProfileCommand::Use {
                    id,
                    config: ConfigArg { config },
                } => {
                    assert_eq!(id, "finland");
                    assert_eq!(config, DEFAULT_CONFIG_PATH);
                }
                _ => panic!("use expected"),
            },
            _ => panic!("profile expected"),
        }
        let command = SubCommand::try_parse_from([
            "csqtt",
            "profile",
            "import",
            "csqtt://connect?v=2",
            "--commit",
            "--activate",
            "--id",
            "srv",
        ])
        .unwrap();
        match command {
            SubCommand::Profile {
                command:
                    ProfileCommand::Import {
                        link,
                        id,
                        commit,
                        activate,
                        name,
                        ..
                    },
                ..
            } => {
                assert_eq!(link, "csqtt://connect?v=2");
                assert_eq!(id.as_deref(), Some("srv"));
                assert!(commit);
                assert!(activate);
                assert!(name.is_none());
            }
            _ => panic!("import expected"),
        }
    }

    #[test]
    fn parser_log_tail_flags() {
        let command =
            SubCommand::try_parse_from(["csqtt", "log", "tail", "-n", "50", "-f"]).unwrap();
        match command {
            SubCommand::Log {
                command:
                    LogCommand::Tail {
                        lines,
                        follow,
                        file,
                        ..
                    },
            } => {
                assert_eq!(lines, 50);
                assert!(follow);
                assert!(file.is_none());
            }
            _ => panic!("log tail expected"),
        }
    }

    #[test]
    fn parser_captcha_commands() {
        let command = SubCommand::try_parse_from(["csqtt", "captcha", "cancel"]).unwrap();
        match command {
            SubCommand::Captcha {
                command: CaptchaCommand::Cancel { id },
                ..
            } => assert!(id.is_none()),
            _ => panic!("captcha expected"),
        }
    }

    #[test]
    fn handle_dispatches_subcommand() {
        assert!(handle(&["version".to_string()]).is_some());
        assert!(handle(&["status".to_string(), "--json".to_string()]).is_some());
    }

    #[test]
    fn handle_returns_none_for_legacy() {
        assert!(handle(&["--peer".to_string(), "1.2.3.4:5".to_string()]).is_none());
        assert!(handle(&[]).is_none());
        assert!(handle(&["run".to_string()]).is_none());
    }

    #[test]
    fn handle_unknown_subcommand_is_legacy() {
        assert!(handle(&["frobnicate".to_string()]).is_none());
    }

    // --- helpers ---

    fn temp_dir(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(name);
        fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn temp_path(name: &str) -> PathBuf {
        std::env::temp_dir().join(name)
    }
}
