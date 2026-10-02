// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [M4a] Транзакционный импорт csqtt://-ссылок в UCI profile-pool.
//!
//! Pipeline (промпт M4a, задача 6): parse → normalize → validate →
//! preview → temp config → doctor/test_conf → commit. Импортированный
//! профиль НИКОГДА не пишется сразу в live-конфиг: до commit работают
//! только с текстом temp-конфига; любая ошибка этапа оставляет
//! live `/etc/config/csqtt` нетронутым.
//!
//! Backup/rollback (задача 7): перед user-driven commit — копия live
//! конфига с правами 0600; хранение ограничено (BACKUP_KEEP последних);
//! runtime status в backup не попадает (только UCI-текст). Путь
//! backup-каталога зафиксирован для M5 (uci::BACKUP_DIR).
//!
//! Команды профиля (задача 8 — контракт для M4c CLI): build_preview /
//! commit / rollback реализованы здесь; list/show/use/auto/enable/
//! disable/import/export — мутации uci.rs + ссылки link.rs.

use crate::link::{CsqttLink, build_csqtt_link, parse_csqtt_link};
use crate::uci::{
    ConfigIssue, DEFAULT_PROFILE_PRIORITY, DEFAULT_PROFILE_WORKERS, ProfilePool, ServerProfile,
    UciFile, parse_uci, redact_secrets, render_uci, uci_set_active_profile,
};
use std::{
    fs, io,
    path::{Path, PathBuf},
};

/// Каталог backup-копий live-конфига (зафиксирован для M5/procd).
pub const BACKUP_DIR: &str = "/etc/csqtt/backups";
/// Сколько последних backup хранить (задача 7: «хранить ограниченно»).
pub const BACKUP_KEEP: usize = 3;

/// Ошибка транзакционного импорта.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// Ссылка не разбирается (parse-этап).
    Link(String),
    /// Нормализованный профиль не проходит валидацию UCI-схемы.
    Validation(Vec<ConfigIssue>),
    /// Имя секции занято существующим профилем.
    SectionIdTaken(String),
    /// I/O при backup/commit (live-конфиг НЕ изменён).
    Io(String),
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Link(message) => write!(f, "ссылка не импортируется: {message}"),
            Self::Validation(issues) => write!(
                f,
                "конфиг после импорта невалиден: {}",
                issues
                    .iter()
                    .map(std::string::ToString::to_string)
                    .collect::<Vec<_>>()
                    .join("; ")
            ),
            Self::SectionIdTaken(id) => {
                write!(f, "имя профиля «{id}» уже занято")
            }
            Self::Io(message) => write!(f, "ошибка записи: {message} (live-конфиг не изменён)"),
        }
    }
}

/// Нормализованный импорт: ссылка + выбранное имя секции.
pub struct ImportRequest {
    pub link_text: String,
    /// Желаемый section id; None → сгенерировать из хоста.
    pub section_id: Option<String>,
    /// Отображаемое имя профиля.
    pub name: Option<String>,
    /// Активировать сразу после commit (manual + active_profile).
    pub activate: bool,
}

/// Результат parse+normalize+validate+preview: показывает, ЧТО будет
/// записано при commit. Ничего не трогает на диске.
#[derive(Clone, Debug, PartialEq)]
pub struct ImportPreview {
    /// Полный текст будущего /etc/config/csqtt.
    pub config_text: String,
    /// Итоговый section id импортированного профиля.
    pub section_id: String,
    /// Отображаемое имя профиля.
    pub name: String,
    /// Существующие секреты live-конфига + новые (для logsink после commit).
    pub secrets: Vec<String>,
}

impl ImportPreview {
    /// Публичный показ preview (CLI/LuCI, M4c/M6): структура конфига,
    /// секреты замаскированы (правило 10 UCI-схемы: password/vk_js_token
    /// не выдавать через CLI/LuCI/status).
    pub fn describe(&self) -> String {
        let safe_text = redact_secrets(&self.config_text, &self.secrets);
        format!(
            "импорт профиля «{}» (id={}); конфиг после commit:\n{safe_text}",
            self.name, self.section_id
        )
    }
}

/// Этап 1-4: parse → normalize → validate → preview.
/// Live-конфиг читается, но не пишется. Любая ошибка — live не тронут.
pub fn build_preview(
    live_text: &str,
    request: &ImportRequest,
) -> Result<ImportPreview, ImportError> {
    // parse
    let link: CsqttLink = parse_csqtt_link(&request.link_text)
        .map_err(|error| ImportError::Link(error.to_string()))?;
    // normalize: ссылка → ServerProfile
    let section_id = match &request.section_id {
        Some(id) => id.clone(),
        None => suggest_section_id(&link),
    };
    let mut file = parse_uci(live_text).map_err(|error| {
        ImportError::Validation(vec![ConfigIssue {
            section: "uci".to_string(),
            field: "syntax".to_string(),
            message: error.to_string(),
        }])
    })?;
    if file.sections.iter().any(|section| {
        section.kind == "server" && section.name.as_deref() == Some(section_id.as_str())
    }) {
        return Err(ImportError::SectionIdTaken(section_id));
    }
    let profile = profile_from_link(&section_id, &link, request);
    // temp config (in-memory) — полный будущий файл
    append_profile(&mut file, &profile);
    let config_text = render_uci(&file);
    // validate (doctor/test_conf): будущий файл должен проходить схему M4a
    let pool = ProfilePool::from_text(&config_text).map_err(ImportError::Validation)?;
    // activate: manual + active_profile должен быть валиден
    if request.activate {
        let mut activated = parse_uci(&config_text).map_err(|error| {
            ImportError::Validation(vec![ConfigIssue {
                section: "uci".to_string(),
                field: "syntax".to_string(),
                message: error.to_string(),
            }])
        })?;
        uci_set_active_profile(&mut activated, &section_id).map_err(ImportError::Link)?;
        let text = render_uci(&activated);
        ProfilePool::from_text(&text).map_err(ImportError::Validation)?;
        return Ok(ImportPreview {
            config_text: text,
            section_id,
            name: profile.name.clone(),
            secrets: pool.collect_secrets(),
        });
    }
    Ok(ImportPreview {
        config_text,
        section_id,
        name: profile.name.clone(),
        secrets: pool.collect_secrets(),
    })
}

/// Этап commit: backup live → атомарная замена (temp + rename).
/// Вызывается ТОЛЬКО после одобренного preview (user-driven).
pub fn commit(live_path: &Path, preview: &ImportPreview) -> Result<(), ImportError> {
    commit_with_backup_dir(live_path, preview, Path::new(BACKUP_DIR))
}

/// Ядро commit с явным каталогом backup: production вызывает с
/// BACKUP_DIR, тесты — с temp-каталогом.
pub fn commit_with_backup_dir(
    live_path: &Path,
    preview: &ImportPreview,
    backup_dir: &Path,
) -> Result<(), ImportError> {
    fs::create_dir_all(backup_dir).map_err(|error| ImportError::Io(error.to_string()))?;
    // backup текущего live (если уже существует)
    if live_path.exists() {
        let backup_path = next_backup_path(backup_dir)?;
        fs::copy(live_path, &backup_path).map_err(|error| ImportError::Io(error.to_string()))?;
        restrict_permissions(&backup_path);
        prune_backups(backup_dir)?;
    }
    // Атомарная запись: temp — СИБЛИНГ live (тот же каталог = та же ФС;
    // temp в другом каталоге дал бы EXDEV на rename), затем rename
    // поверх live. При ошибке rename temp удаляется, live не тронут.
    let temp_path = sibling_temp_path(live_path);
    fs::write(&temp_path, &preview.config_text)
        .map_err(|error| ImportError::Io(error.to_string()))?;
    restrict_permissions(&temp_path);
    if let Err(error) = fs::rename(&temp_path, live_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ImportError::Io(error.to_string()));
    }
    Ok(())
}

/// Temp-файл рядом с live (`<имя>.import-tmp`) — тот же каталог,
/// та же файловая система, атомарный rename.
fn sibling_temp_path(live_path: &Path) -> PathBuf {
    let file_name = live_path.file_name().map_or_else(
        || std::ffi::OsString::from("csqtt.import-tmp"),
        |name| {
            let mut temp = name.to_os_string();
            temp.push(".import-tmp");
            temp
        },
    );
    match live_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        Some(parent) => parent.join(file_name),
        None => PathBuf::from(file_name),
    }
}

/// Откат последнего коммита: восстановить live из самого свежего backup.
pub fn rollback(live_path: &Path) -> Result<PathBuf, ImportError> {
    rollback_with_backup_dir(live_path, Path::new(BACKUP_DIR))
}

/// Ядро rollback с явным каталогом backup (production — BACKUP_DIR,
/// тесты — temp-каталог).
pub fn rollback_with_backup_dir(
    live_path: &Path,
    backup_dir: &Path,
) -> Result<PathBuf, ImportError> {
    let candidates: Vec<PathBuf> = match fs::read_dir(backup_dir) {
        Ok(entries) => entries
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("csqtt.") && name.ends_with(".bak"))
            })
            .collect(),
        Err(error) => return Err(ImportError::Io(error.to_string())),
    };
    // Самый свежий — по счётчику в имени (больше = новее); mtime
    // ненадёжен (одинаковые метки в пределах commit-цикла одного теста).
    let mut newest: Option<(u64, PathBuf)> = None;
    for path in &candidates {
        let Some(counter) = backup_counter(path) else {
            continue;
        };
        if newest.as_ref().is_none_or(|(best, _)| counter >= *best) {
            newest = Some((counter, path.clone()));
        }
    }
    let Some((_, backup)) = newest else {
        return Err(ImportError::Io("backup-копий нет".to_string()));
    };
    let text = fs::read_to_string(&backup).map_err(|error| ImportError::Io(error.to_string()))?;
    // Восстанавливаемый конфиг обязан быть валиден — не ломаем live мусором
    ProfilePool::from_text(&text).map_err(ImportError::Validation)?;
    let temp_path = sibling_temp_path(live_path);
    fs::write(&temp_path, &text).map_err(|error| ImportError::Io(error.to_string()))?;
    if let Err(error) = fs::rename(&temp_path, live_path) {
        let _ = fs::remove_file(&temp_path);
        return Err(ImportError::Io(error.to_string()));
    }
    Ok(backup)
}

/// Сгенерировать имя секции из хоста ссылки (`vps.example.com` → `vps`).
/// Свободность НЕ гарантируется: занятое имя отклоняется ошибкой
/// SectionIdTaken (explicit error — caller выбирает другое имя сам,
/// суффиксы не придумываем, чтобы id оставался предсказуемым).
fn suggest_section_id(link: &CsqttLink) -> String {
    let mut base: String = link
        .host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '_' {
                character
            } else {
                '_'
            }
        })
        .collect();
    if base.is_empty() {
        base = "server".to_string();
    }
    base
}

/// Нормализация ссылки в профиль: transport-поля из ссылки, обвязка —
/// дефолты M4a (enabled=0 — включение отдельной командой M4c;
/// устройство не выдумываем: device_id пустой, M4b/M8 переносит
/// существующий при миграции). Исключение — activate=true: manual-режим
/// требует enabled-профиль (правило 3 UCI-схемы), импорт сразу включает.
fn profile_from_link(section_id: &str, link: &CsqttLink, request: &ImportRequest) -> ServerProfile {
    ServerProfile {
        section_id: section_id.to_string(),
        order: usize::MAX,
        name: request.name.clone().unwrap_or_else(|| link.host.clone()),
        enabled: request.activate,
        priority: DEFAULT_PROFILE_PRIORITY,
        peer: link.peer_address(),
        password: link.password.clone(),
        vk: link.hashes.join(","),
        workers: DEFAULT_PROFILE_WORKERS,
        ..ServerProfile::default()
    }
}

/// Вставить профиль в UciFile: секция server добавляется в конец
/// (порядок сортировки определяется priority, не позицией).
fn append_profile(file: &mut UciFile, profile: &ServerProfile) {
    let mut section = crate::uci::UciSection {
        kind: "server".to_string(),
        name: Some(profile.section_id.clone()),
        options: Vec::new(),
        lists: Vec::new(),
    };
    section.set_option("name", &profile.name);
    section.set_option("enabled", if profile.enabled { "1" } else { "0" });
    section.set_option("priority", &profile.priority.to_string());
    section.set_option("peer", &profile.peer);
    section.set_option("password", &profile.password);
    section.set_option("vk", &profile.vk);
    section.set_option("workers", &profile.workers.to_string());
    section.set_option("obfs", &profile.obfs);
    section.set_option("turn_transport", &profile.turn_transport);
    section.set_option("captcha_mode", &profile.captcha_mode);
    section.set_option("fingerprint", &profile.fingerprint);
    section.set_option("client_ids", &profile.client_ids);
    section.set_option("vk_auth_mode", &profile.vk_auth_mode);
    section.set_option("vk_hash_mode", &profile.vk_hash_mode);
    section.set_option("vk_js_token", &profile.vk_js_token);
    section.set_option("device_id", &profile.device_id);
    section.set_option("note", &profile.note);
    file.sections.push(section);
}

/// Счётчик backup-имени `csqtt.<n>.bak` (None — чужой файл).
fn backup_counter(path: &Path) -> Option<u64> {
    path.file_name()
        .and_then(|name| name.to_str())
        .and_then(|name| {
            name.strip_prefix("csqtt.")
                .and_then(|rest| rest.strip_suffix(".bak"))
                .and_then(|digits| digits.parse::<u64>().ok())
        })
}

/// Следующее имя backup: csqtt.<counter>.bak; счётчик — max+1.
fn next_backup_path(backup_dir: &Path) -> Result<PathBuf, ImportError> {
    let mut max = 0u64;
    for entry in fs::read_dir(backup_dir).map_err(|error| ImportError::Io(error.to_string()))? {
        let Ok(entry) = entry else { continue };
        if let Some(counter) = backup_counter(&entry.path()) {
            max = max.max(counter);
        }
    }
    Ok(backup_dir.join(format!("csqtt.{}.bak", max + 1)))
}

/// Хранить только BACKUP_KEEP свежих backup (по счётчику: большие
/// номера новее).
fn prune_backups(backup_dir: &Path) -> Result<(), ImportError> {
    let mut counters: Vec<(u64, PathBuf)> = Vec::new();
    for entry in fs::read_dir(backup_dir).map_err(|error| ImportError::Io(error.to_string()))? {
        let Ok(entry) = entry else { continue };
        if let Some(counter) = backup_counter(&entry.path()) {
            counters.push((counter, entry.path()));
        }
    }
    counters.sort_by_key(|(counter, _)| std::cmp::Reverse(*counter));
    for (_, path) in counters.into_iter().skip(BACKUP_KEEP) {
        let _ = fs::remove_file(path);
    }
    Ok(())
}

/// Права 0600 на backup (внутри — пароль профиля).
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

/// Экспорт профиля в csqtt://-ссылку (задача 4: current-форма;
/// сборка из UCI-профиля для profile export M4c).
pub fn export_profile_link(pool: &ProfilePool, section_id: &str) -> Result<String, ImportError> {
    let profile = pool
        .server(section_id)
        .ok_or_else(|| ImportError::Link(format!("профиль «{section_id}» не найден")))?;
    let Some((host, port)) = crate::link::parse_peer(&profile.peer) else {
        return Err(ImportError::Link(format!(
            "peer «{}» не разбирается как host:port",
            profile.peer
        )));
    };
    let hashes = crate::worker::parse_hashes(&profile.vk);
    Ok(build_csqtt_link(&host, port, &profile.password, &hashes))
}

/// Утилита чтения live-конфига для CLI (M4c): env-fallback возвращает
/// None, если /etc/config/csqtt отсутствует (миграция env → M5).
pub fn read_live_config(live_path: &Path) -> io::Result<Option<String>> {
    match fs::read_to_string(live_path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn fixture(relative: &str) -> String {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("fixture {path:?}: {error}"))
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let index = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("csqtt-m4a-import")
            .join(format!("{tag}-{index}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    // Гарантия транзакции: все ошибки ниже НЕ меняют live-текст.
    fn assert_live_untouched(live_path: &Path, original: &str) {
        assert_eq!(
            fs::read_to_string(live_path).unwrap(),
            original,
            "live-конфиг изменён — транзакция нарушена"
        );
    }

    // --- parse ---

    #[test]
    fn preview_parses_current_and_legacy_link_forms() {
        let live = fixture("config/minimal/csqtt");
        for link in [fixture("links/current.txt"), fixture("links/legacy.txt")] {
            let request = ImportRequest {
                link_text: link.trim().to_string(),
                section_id: None,
                name: None,
                activate: false,
            };
            let preview = build_preview(&live, &request).unwrap_or_else(|error| panic!("{error}"));
            let pool = ProfilePool::from_text(&preview.config_text).unwrap();
            let profile = pool.server(&preview.section_id).unwrap();
            assert!(!profile.enabled, "импорт не включает профиль сам");
            assert_eq!(profile.peer, "198.51.100.10:46000");
            assert_eq!(profile.password, "examplePassword1");
        }
    }

    #[test]
    fn preview_rejects_broken_link_without_touching_live() {
        let live = fixture("config/minimal/csqtt");
        let request = ImportRequest {
            link_text: "https://not-a-link".to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        match build_preview(&live, &request) {
            Err(ImportError::Link(message)) => {
                assert!(message.contains("csqtt://"), "{message}")
            }
            other => panic!("ожидали Link-ошибку, получили {other:?}"),
        }
    }

    #[test]
    fn preview_rejects_unknown_version_explicitly() {
        let live = fixture("config/minimal/csqtt");
        let request = ImportRequest {
            link_text: "csqtt://connect?v=3&host=h&peer=1&password=p".to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        match build_preview(&live, &request) {
            Err(ImportError::Link(message)) => {
                assert!(
                    message.contains('3'),
                    "ожидали упоминание версии: {message}"
                )
            }
            other => panic!("ожидали version-error, получили {other:?}"),
        }
    }

    #[test]
    fn preview_rejects_taken_section_id() {
        let live = fixture("config/valid/csqtt");
        let request = ImportRequest {
            link_text: fixture("links/legacy.txt").trim().to_string(),
            section_id: Some("finland".to_string()),
            name: None,
            activate: false,
        };
        assert_eq!(
            build_preview(&live, &request),
            Err(ImportError::SectionIdTaken("finland".to_string()))
        );
    }

    #[test]
    fn suggested_id_from_host_avoids_collisions() {
        let live = fixture("config/valid/csqtt"); // содержит finland + backup
        let request = ImportRequest {
            link_text: fixture("links/legacy.txt").trim().to_string(), // host 198.51.100.10
            section_id: None,
            name: None,
            activate: false,
        };
        let preview = build_preview(&live, &request).unwrap();
        // базовое имя — из хоста, без конфликта с finland/backup
        assert_eq!(preview.section_id, "198_51_100_10");
        // повторный импорт той же ссылки: имя занято → ошибка
        let second = build_preview(&preview.config_text, &request);
        assert_eq!(
            second,
            Err(ImportError::SectionIdTaken("198_51_100_10".to_string()))
        );
    }

    // --- validate (doctor/test_conf на temp-конфиге) ---

    #[test]
    fn preview_validates_resulting_config() {
        let live = fixture("config/minimal/csqtt");
        // Ссылка без хешей: профиль с vk='' и enabled=0 — валиден как черновик.
        let request = ImportRequest {
            link_text: "csqtt://secret@203.0.113.7:46000".to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        let preview = build_preview(&live, &request).unwrap();
        assert!(preview.config_text.contains("203.0.113.7:46000"));
        // validate-этап реально ловит мусор: битая ссылка-пароль не проходит
        let broken = ImportRequest {
            link_text: "csqtt://secret@203.0.113.7:0".to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        assert!(matches!(
            build_preview(&live, &broken),
            Err(ImportError::Link(_))
        ));
    }

    #[test]
    fn activate_option_switches_to_manual() {
        let live = fixture("config/minimal/csqtt");
        // current-ссылка с хешами: включённый manual-профиль валиден
        let request = ImportRequest {
            link_text: fixture("links/current.txt").trim().to_string(),
            section_id: None,
            name: None,
            activate: true,
        };
        let preview = build_preview(&live, &request).unwrap();
        let pool = ProfilePool::from_text(&preview.config_text).unwrap();
        assert_eq!(pool.main.selection_mode, crate::uci::SelectionMode::Manual);
        assert_eq!(pool.main.active_profile, preview.section_id);
        assert_eq!(
            pool.selected_profile().unwrap().section_id,
            preview.section_id
        );
        assert!(pool.server(&preview.section_id).unwrap().enabled);
    }

    // --- commit / rollback: файловая транзакция ---

    #[test]
    fn commit_writes_preview_and_creates_backup() {
        let dir = temp_dir("commit");
        let live_path = dir.join("csqtt");
        let live = fixture("config/valid/csqtt");
        fs::write(&live_path, &live).unwrap();
        let backup_dir = dir.join("backups");
        let request = ImportRequest {
            link_text: fixture("links/legacy.txt").trim().to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        let preview = build_preview(&live, &request).unwrap();
        // Реальный commit в temp-каталоги (ядро commit_with_backup_dir).
        commit_with_backup_dir(&live_path, &preview, &backup_dir).unwrap();
        // live содержит preview-текст целиком
        assert_eq!(fs::read_to_string(&live_path).unwrap(), preview.config_text);
        let pool = ProfilePool::from_text(&preview.config_text).unwrap();
        assert!(pool.server(&preview.section_id).is_some());
        assert!(pool.servers.len() == 3);
        // backup исходного live создан
        let backups: Vec<PathBuf> = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.ends_with(".bak"))
            })
            .collect();
        assert_eq!(backups.len(), 1);
        assert_eq!(fs::read_to_string(&backups[0]).unwrap(), live);
        // temp-сиблинг не остался лежать рядом с live
        assert!(
            fs::read_dir(&dir)
                .unwrap()
                .filter_map(|entry| entry.ok())
                .all(|entry| {
                    !entry
                        .file_name()
                        .to_str()
                        .is_some_and(|name| name.contains("import-tmp"))
                }),
            "temp-файл должен исчезать после rename"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&backups[0]).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600, "backup обязан быть 0600");
        }
    }

    #[test]
    fn rollback_restores_latest_backup() {
        let dir = temp_dir("rollback");
        let live_path = dir.join("csqtt");
        let backup_dir = dir.join("backups");
        let original = fixture("config/valid/csqtt");
        fs::write(&live_path, &original).unwrap();
        // commit1: original → preview1 (backup1 = original)
        let first = ImportRequest {
            link_text: fixture("links/legacy.txt").trim().to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        let preview1 = build_preview(&original, &first).unwrap();
        commit_with_backup_dir(&live_path, &preview1, &backup_dir).unwrap();
        // commit2: preview1 → preview2 (backup2 = preview1)
        let second = ImportRequest {
            link_text: fixture("links/current.txt").trim().to_string(),
            section_id: Some("second_import".to_string()),
            name: None,
            activate: false,
        };
        let live1 = fs::read_to_string(&live_path).unwrap();
        let preview2 = build_preview(&live1, &second).unwrap();
        commit_with_backup_dir(&live_path, &preview2, &backup_dir).unwrap();
        // Порча live вне транзакции: откат вернёт предпоследний commit.
        fs::write(&live_path, "config csqtt 'main'\n").unwrap();
        let restored_from = rollback_with_backup_dir(&live_path, &backup_dir).unwrap();
        assert_eq!(
            fs::read_to_string(&live_path).unwrap(),
            preview1.config_text
        );
        assert_eq!(
            fs::read_to_string(&restored_from).unwrap(),
            preview1.config_text
        );
        // Детерминированно — по счётчику в имени, не по mtime.
        assert!(restored_from.ends_with("csqtt.2.bak"));
    }

    #[test]
    fn rollback_refuses_invalid_backup() {
        let dir = temp_dir("rollback-bad");
        let live_path = dir.join("csqtt");
        let backup_dir = dir.join("backups");
        let original = "config csqtt 'main'\n";
        fs::write(&live_path, original).unwrap();
        fs::create_dir_all(&backup_dir).unwrap();
        // Битый backup: live трогать запрещено.
        fs::write(backup_dir.join("csqtt.1.bak"), "config banana 'x'\n").unwrap();
        match rollback_with_backup_dir(&live_path, &backup_dir) {
            Err(ImportError::Validation(_)) => {}
            other => panic!("ожидали Validation-ошибку, получили {other:?}"),
        }
        assert_live_untouched(&live_path, original);
    }

    #[test]
    fn rollback_without_backups_is_io_error() {
        let dir = temp_dir("rollback-empty");
        let live_path = dir.join("csqtt");
        fs::write(&live_path, "config csqtt 'main'\n").unwrap();
        let backup_dir = dir.join("backups");
        fs::create_dir_all(&backup_dir).unwrap();
        match rollback_with_backup_dir(&live_path, &backup_dir) {
            Err(ImportError::Io(message)) => assert!(message.contains("backup")),
            other => panic!("ожидали Io-ошибку, получили {other:?}"),
        }
    }

    #[test]
    fn describe_masks_secrets() {
        let live = fixture("config/valid/csqtt");
        let request = ImportRequest {
            link_text: fixture("links/legacy.txt").trim().to_string(),
            section_id: None,
            name: None,
            activate: false,
        };
        let preview = build_preview(&live, &request).unwrap();
        let shown = preview.describe();
        // Пароль импортированного профиля и существующие секреты live
        // в публичном описании замаскированы.
        assert!(!shown.contains("examplePassword1"));
        assert!(!shown.contains("backup-secret-password"));
        assert!(shown.contains("***"));
        // Но структура видна: id и peer.
        assert!(shown.contains(&preview.section_id));
        assert!(shown.contains("198.51.100.10:46000"));
        // А внутренний config_text — полный (пишется в файл, не на экран).
        assert!(preview.config_text.contains("examplePassword1"));
    }

    #[test]
    fn backup_files_are_pruned_to_keep_limit() {
        let dir = temp_dir("prune");
        let backup_dir = dir.join("backups");
        fs::create_dir_all(&backup_dir).unwrap();
        for counter in 1..=6 {
            fs::write(backup_dir.join(format!("csqtt.{counter}.bak")), "x").unwrap();
        }
        prune_backups(&backup_dir).unwrap();
        let mut remaining: Vec<u64> = fs::read_dir(&backup_dir)
            .unwrap()
            .filter_map(|entry| entry.ok())
            .filter_map(|entry| {
                entry
                    .file_name()
                    .to_str()
                    .and_then(|name| name.strip_prefix("csqtt."))
                    .and_then(|rest| rest.strip_suffix(".bak"))
                    .and_then(|digits| digits.parse::<u64>().ok())
            })
            .collect();
        remaining.sort_unstable();
        assert_eq!(remaining, [4, 5, 6]);
    }

    #[test]
    fn next_backup_counter_increments() {
        let dir = temp_dir("counter");
        let backup_dir = dir.join("backups");
        fs::create_dir_all(&backup_dir).unwrap();
        fs::write(backup_dir.join("csqtt.2.bak"), "x").unwrap();
        fs::write(backup_dir.join("csqtt.7.bak"), "x").unwrap();
        assert_eq!(
            next_backup_path(&backup_dir).unwrap(),
            backup_dir.join("csqtt.8.bak")
        );
    }

    // --- export ---

    #[test]
    fn export_builds_current_form_link_from_profile() {
        let live = fixture("config/valid/csqtt");
        let pool = ProfilePool::from_text(&live).unwrap();
        let link = export_profile_link(&pool, "finland").unwrap();
        let parsed = parse_csqtt_link(&link).unwrap();
        assert_eq!(parsed.host, "198.51.100.10");
        assert_eq!(parsed.port, 46000);
        assert_eq!(parsed.password, "examplePassword1");
        assert_eq!(parsed.hashes.len(), 2);
        // roundtrip: экспорт → импорт даёт тот же transport-профиль
        let request = ImportRequest {
            link_text: link,
            section_id: None,
            name: None,
            activate: false,
        };
        let preview = build_preview(fixture("config/minimal/csqtt").as_str(), &request).unwrap();
        let imported = ProfilePool::from_text(&preview.config_text).unwrap();
        let profile = imported.server(&preview.section_id).unwrap();
        assert_eq!(profile.peer, "198.51.100.10:46000");
        assert_eq!(profile.password, "examplePassword1");
    }

    #[test]
    fn export_rejects_unknown_profile_and_bad_peer() {
        let live = fixture("config/valid/csqtt");
        let pool = ProfilePool::from_text(&live).unwrap();
        assert!(matches!(
            export_profile_link(&pool, "ghost"),
            Err(ImportError::Link(_))
        ));
        // Peer из disabled-профиля может быть битым — export честно откажется
        let minimal = "config server 'draft'\n\toption enabled '0'\n\toption peer 'garbage'\n";
        let pool = ProfilePool::from_text(minimal).unwrap();
        assert!(matches!(
            export_profile_link(&pool, "draft"),
            Err(ImportError::Link(_))
        ));
    }

    // --- live reader ---

    #[test]
    fn read_live_config_none_when_missing() {
        let dir = temp_dir("readlive");
        assert_eq!(
            read_live_config(&dir.join("nope")).unwrap(),
            None,
            "отсутствующий конфиг = None (env-fallback миграции)"
        );
        let path = dir.join("csqtt");
        fs::write(&path, "config csqtt 'main'\n").unwrap();
        assert_eq!(
            read_live_config(&path).unwrap().as_deref(),
            Some("config csqtt 'main'\n")
        );
    }
}
