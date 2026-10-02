// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [OpenWrt-порт, M4b] Runtime state службы CSQTT: `/var/run/csqtt/status.json`.
//!
//! Контракт (PROJECT_CONTRACT): runtime state живёт ТОЛЬКО здесь — не в UCI
//! и не в логах. Файл пишется атомарно (temp-сиблинг + rename, права 0644 —
//! секретов в нём нет, его читает LuCI/rpcd от root) примерно каждые 5с.
//!
//! В статусе НИКОГДА не бывает password/vk/vk_js_token/captcha-секретов:
//! модель собирается из публичных полей состояний профилей, а ошибки
//! пропускаются через `uci::redact_secrets`.

use crate::uci::{HealthMode, RoutingMode, SelectionMode};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

/// Путь по умолчанию (procd/resolv: /var/run — tmpfs, survives reload).
pub const DEFAULT_STATUS_PATH: &str = "/var/run/csqtt/status.json";
/// Интервал записи статус-райтером (служба CSQTT M4b, требование «~каждые 5с»).
pub const STATUS_WRITE_INTERVAL_SECS: u64 = 5;

/// Состояние службы CSQTT в целом (жизненный цикл процесса, не профилей).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DaemonState {
    /// Конфиг разобран, клиент стартует.
    Starting,
    /// Профиль работает (или службе CSQTT есть что делать).
    Running,
    /// main.enabled = 0: служба CSQTT живёт (procd), но туннель не поднимает.
    Disabled,
    /// Идёт SIGHUP-reload и reconcile.
    Reloading,
    /// Получен SIGTERM/SIGINT, graceful teardown ≤3с.
    Stopping,
    /// Завершён (финальный статус перед выходом).
    Stopped,
}

impl DaemonState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Starting => "starting",
            Self::Running => "running",
            Self::Disabled => "disabled",
            Self::Reloading => "reloading",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
        }
    }
}

/// Один профиль в status.json (state machine M4b, требование 11).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProfileStatus {
    pub id: String,
    pub name: String,
    pub state: String,
    pub priority: u32,
    pub enabled: bool,
    /// Сколько неудач подряд (health/client) накоплено.
    pub consecutive_fails: u32,
    /// Сколько здоровых health-тиков подряд накоплено.
    pub success_streak: u32,
    /// Секунд до повторной попытки (cooldown) — 0 если доступен сейчас.
    pub cooldown_remaining_secs: u64,
}

/// Туннель: фиксированное имя csqtt0, IP/DNS из TUNCONF, MTU/DNS из конфига.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TunnelStatus {
    /// Фиксированное имя интерфейса (других не бывает — M3X/M4b правило 9).
    pub interface: String,
    /// IP из TUNCONF сервера (data-plane адрес).
    pub address: Option<String>,
    /// Локальный адрес TUN из main.tun_address (если задан).
    pub local_address: Option<String>,
    pub mtu: u16,
    /// DNS из TUNCONF сервера.
    pub dns: Option<String>,
}

/// Безопасная сводка маршрутизации: служба CSQTT interface-only, он НЕ добавляет
/// ни одного route/rule/table и не трогает WAN/DNS/firewall (M3X contract).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutingSummary {
    /// routing.mode UCI: "auto" (interface-only) | "none".
    pub mode: String,
    /// Всегда false в службе CSQTT: half/exclude-маршруты не ставятся.
    pub install_routes: bool,
    /// Всегда false в службе CSQTT: системный DNS не меняется.
    pub apply_dns: bool,
    /// Человекочитаемая строка для UI.
    pub summary: String,
}

impl RoutingSummary {
    pub fn from_mode(mode: RoutingMode) -> Self {
        Self {
            mode: mode.as_str().to_string(),
            install_routes: false,
            apply_dns: false,
            summary: match mode {
                RoutingMode::Auto => {
                    "interface-only: no routes/rules/tables, WAN/DNS/firewall untouched"
                }
                RoutingMode::None => "tunnel interface only (routing disabled)",
            }
            .to_string(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkersStatus {
    /// Сконфигурировано воркеров в активном профиле.
    pub configured: usize,
    /// Активных соединений из событий STATS (data-plane).
    pub active: i64,
}

/// Один CAPTCHA-challenge службы CSQTT (M4d). ТОЛЬКО safe fields: session_token,
/// redirect_uri и результат решения — секреты, их здесь не бывает никогда
/// (они живут в памяти CaptchaManager).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptchaChallengeStatus {
    /// Безопасный случайный id (≥128 bit), не зависимый от секретов VK.
    pub id: String,
    /// Профиль, которому нужен challenge.
    pub profile_id: String,
    /// Режим открытого окна (auto/selected/manual) — не секрет.
    pub mode: String,
    /// pending / opened / verifying / solved / failed / expired / cancelled.
    pub state: String,
    pub created_at: u64,
    pub expires_at: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub daemon_state: String,
    pub started_at: u64,
    pub generated_at: u64,
    pub uptime_secs: u64,
    pub selection_mode: String,
    pub active_profile: Option<String>,
    pub profiles: Vec<ProfileStatus>,
    pub tunnel: TunnelStatus,
    pub routing: RoutingSummary,
    pub workers: WorkersStatus,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    /// Счётчик переключений/переподключений профиля.
    pub reconnects: u64,
    /// Pending CAPTCHA (сколько профилей ждут решения).
    pub captcha_pending: u64,
    /// Активные/недавние challenges — safe fields только (M4d).
    #[serde(default)]
    pub captcha_challenges: Vec<CaptchaChallengeStatus>,
    /// Последняя ошибка, обезвреженная от секретов (правило 10).
    pub last_error: Option<String>,
    /// Режим проверки здоровья (health_mode UCI).
    pub health_mode: String,
    /// health_target, если задан (зонд — только дополнительный сигнал).
    pub health_target: String,
}

impl DaemonStatus {
    /// Сериализовать в JSON. Не падает на секретах — их здесь не бывает.
    pub fn to_json(&self) -> serde_json::Result<String> {
        serde_json::to_string_pretty(self)
    }
}

/// Атомарная запись статуса: temp-сиблинг + rename. Ошибка I/O не должна
/// ронять службу CSQTT — вызывающий логирует и продолжает.
///
/// [High-2 AUDIT] Runtime-каталог (`/var/run/csqtt` — tmpfs, на свежей
/// загрузке отсутствует) создаётся гарантированно до первой записи:
/// create_dir_all идемпотентен, права созданного каталога 0755 (status.json
/// не содержит секретов и читается LuCI/rpcd; секретные файлы это не
/// затрагивается).
pub fn write_status_atomic(path: &Path, status: &DaemonStatus) -> std::io::Result<()> {
    let json = status
        .to_json()
        .map_err(|error| std::io::Error::other(format!("status serialize: {error}")))?;
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
        && !parent.exists()
    {
        fs::create_dir_all(parent)?;
        restrict_runtime_dir(parent);
    }
    let mut temp_path = PathBuf::from(path);
    {
        let name = temp_path
            .file_name()
            .ok_or_else(|| std::io::Error::other("status path has no file name"))?
            .to_owned();
        temp_path.set_file_name(format!("{}.tmp", name.to_string_lossy()));
    }
    {
        let mut file = fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&temp_path)?;
        file.write_all(json.as_bytes())?;
        // tmpfs/смысловое устройство: статус читается на лету, fsync
        // не нужен, но flush — обязательно до rename.
        file.flush()?;
    }
    set_world_readable(&temp_path)?;
    // rename атомарен на одной файловой системе (tmpfs /var/run).
    fs::rename(&temp_path, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_world_readable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o644);
    fs::set_permissions(path, permissions)
}

#[cfg(not(unix))]
fn set_world_readable(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

/// [High-2 AUDIT] Права созданного runtime-каталога: 0755 (rwx для root,
/// каталог читаем rpcd/LuCI). Секретных файлов здесь нет — status.json 0644,
/// конфиг/лог (0600) живут в других каталогах и их права не меняются.
#[cfg(unix)]
fn restrict_runtime_dir(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
}

#[cfg(not(unix))]
fn restrict_runtime_dir(_path: &Path) {}

/// Серверное время в секундах (для started_at/generated_at/uptime).
pub fn epoch_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or_default()
}

/// selection_mode для статуса (строка UCI-значения).
pub fn selection_mode_str(mode: SelectionMode) -> &'static str {
    mode.as_str()
}

/// health_mode для статуса.
pub fn health_mode_str(mode: HealthMode) -> &'static str {
    mode.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    fn sample_status() -> DaemonStatus {
        DaemonStatus {
            version: "2.1.9".to_string(),
            daemon_state: DaemonState::Running.as_str().to_string(),
            started_at: 1_700_000_000,
            generated_at: 1_700_000_005,
            uptime_secs: 5,
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
            rx_bytes: 1024,
            tx_bytes: 512,
            reconnects: 0,
            captcha_pending: 0,
            captcha_challenges: Vec::new(),
            last_error: None,
            health_mode: "both".to_string(),
            health_target: String::new(),
        }
    }

    #[test]
    fn status_json_is_atomic_and_readable() {
        let directory = std::env::temp_dir().join("csqtt-m4b-status-atomic");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        let mut text = String::new();
        fs::File::open(&path)
            .unwrap()
            .read_to_string(&mut text)
            .unwrap();
        assert!(text.contains("\"active_profile\": \"finland\""));
        // temp-сиблинг исчез после rename.
        assert!(!directory.join("status.json.tmp").exists());
        // Roundtrip через модель.
        let parsed: DaemonStatus = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed.active_profile.as_deref(), Some("finland"));
        assert_eq!(parsed.tunnel.interface, "csqtt0");
    }

    #[test]
    fn status_never_contains_secrets() {
        // Redaction применяется в PoolManager::snapshot через
        // uci::redact_secrets ДО сборки DaemonStatus (см. pool::tests::
        // snapshot_has_no_secrets). Здесь — регресс писателя: атомарный
        // файл содержит ровно то, что пришло, без внедрения секретов.
        let secrets = ["examplePassword1".to_string(), "exampleHashOne".to_string()];
        let raw = "connect password=examplePassword1 vk=exampleHashOne failed";
        let redacted = crate::uci::redact_secrets(raw, &secrets);
        assert!(!redacted.contains("examplePassword1"));
        assert!(!redacted.contains("exampleHashOne"));
        let mut status = sample_status();
        status.last_error = Some(redacted);
        let json = status.to_json().unwrap();
        assert!(!json.contains("examplePassword1"));
        assert!(!json.contains("exampleHashOne"));
        assert!(json.contains("***"));
    }

    #[test]
    fn routing_summary_is_interface_only() {
        let summary = RoutingSummary::from_mode(RoutingMode::Auto);
        assert!(!summary.install_routes);
        assert!(!summary.apply_dns);
        assert!(summary.summary.contains("interface-only"));
    }

    #[test]
    #[cfg(unix)]
    fn status_file_permissions_are_world_readable() {
        use std::os::unix::fs::PermissionsExt;
        let directory = std::env::temp_dir().join("csqtt-m4b-status-perms");
        fs::create_dir_all(&directory).unwrap();
        let path = directory.join("status.json");
        write_status_atomic(&path, &sample_status()).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o644);
    }

    /// [High-2 AUDIT] Регресс fresh boot: runtime-каталог изначально
    /// отсутствует — первая запись создаёт его (0755) и status.json (0644);
    /// повторные записи идемпотентны.
    #[test]
    fn write_creates_missing_runtime_directory() {
        let base =
            std::env::temp_dir().join(format!("csqtt-m4b-status-freshboot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        let path = base.join("var-run").join("csqtt").join("status.json");
        assert!(!path.parent().unwrap().exists());
        write_status_atomic(&path, &sample_status()).expect("fresh boot write");
        assert!(path.parent().unwrap().is_dir());
        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains("\"daemon_state\": \"running\""));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let dir_mode = fs::metadata(path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(dir_mode & 0o777, 0o755);
            let file_mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(file_mode & 0o777, 0o644);
        }
        // Идемпотентность: вторая запись поверх существующего каталога.
        write_status_atomic(&path, &sample_status()).expect("idempotent rewrite");
        let _ = fs::remove_dir_all(&base);
    }

    /// Каталог нельзя создать (блокирует файл) — ошибка без паники.
    #[test]
    fn write_into_uncreatable_directory_fails_without_panic() {
        let base =
            std::env::temp_dir().join(format!("csqtt-m4b-status-blocked-{}", std::process::id()));
        let _ = fs::remove_dir_all(&base);
        fs::create_dir_all(&base).unwrap();
        let blocker = base.join("blocker");
        fs::write(&blocker, b"file").unwrap();
        let path = blocker.join("csqtt").join("status.json");
        assert!(write_status_atomic(&path, &sample_status()).is_err());
        let _ = fs::remove_dir_all(&base);
    }

    #[test]
    fn daemon_state_serializes_as_snake_case() {
        let status = sample_status();
        let json = status.to_json().unwrap();
        assert!(json.contains("\"daemon_state\": \"running\""));
    }
}
