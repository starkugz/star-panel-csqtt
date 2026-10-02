// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [M4a] Файловый лог-синк с ротацией по размеру и redaction секретов.
//!
//! Штатный logger ядра (logging.rs) сохранён без изменений; этот модуль —
//! ОПЦИОНАЛЬНЫЙ файловый слой поверх него: через `set_log_callback`
//! строки дополнительно пишутся в `/var/log/csqtt.log` (main.log_file),
//! с ротацией `csqtt.log → csqtt.log.1` при превышении main.log_size_kb
//! (PROMPTS.md M4a, задача 10). Права 0600: в логах могут оказаться
//! секреты до redaction — файл не должен быть читаем другим пользователям.
//!
//! Redaction: перед записью каждая строка проходит `redact_secrets`
//! (пароли/vk_js_token из ProfilePool::collect_secrets). Тесты
//! секрет-маскировки обязательны (промпт M4a, ТЕСТЫ).
//!
//! Модуль синхронный (файловый I/O в колбэке логгера уже в
//! writer-потоке logging.rs; для M4a этого достаточно).

use crate::uci::redact_secrets;
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Суффикс ротированного файла (csqtt.log.1).
const ROTATED_SUFFIX: &str = "1";

/// Файловый лог-синк: append + size rotation, права 0600.
pub struct FileLogSink {
    state: Mutex<SinkState>,
    secrets: Mutex<Vec<String>>,
}

struct SinkState {
    path: PathBuf,
    file: Option<File>,
    max_bytes: u64,
    written_bytes: u64,
    /// Отключён после ошибки I/O (как Sink в logging.rs): лог-файл
    /// не должен ломать процесс.
    disabled: bool,
}

impl FileLogSink {
    /// Создать синк. Файл открывается лениво при первой записи.
    /// `max_bytes` — порог ротации в байтах (main.log_size_kb * 1024).
    pub fn new(path: impl Into<PathBuf>, max_bytes: u64, secrets: Vec<String>) -> Self {
        Self {
            state: Mutex::new(SinkState {
                path: path.into(),
                file: None,
                max_bytes: max_bytes.max(1),
                written_bytes: 0,
                disabled: false,
            }),
            secrets: Mutex::new(secrets),
        }
    }

    /// Обновить набор секретов для redaction (M4b/M4c: после импорта
    /// профиля или смены пула; служба CSQTT живёт дольше одного конфига).
    pub fn set_secrets(&self, secrets: Vec<String>) {
        match self.secrets.lock() {
            Ok(mut current) => *current = secrets,
            Err(poisoned) => *poisoned.into_inner() = secrets,
        }
    }

    /// Записать строку лога: redaction → append (с `\n`) → при
    /// превышении лимита — ротация. Ошибка I/O отключает синк тихо
    /// (лог-файл не должен ронять службу CSQTT).
    pub fn write_line(&self, line: &str) {
        let redacted = {
            let secrets = match self.secrets.lock() {
                Ok(secrets) => secrets,
                Err(poisoned) => poisoned.into_inner(),
            };
            redact_secrets(line, &secrets)
        };
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if state.disabled {
            return;
        }
        if state.file.is_none() && !open_sink_file(&mut state) {
            return;
        }
        let bytes = redacted.as_bytes();
        let newline = b"\n";
        let Some(file) = state.file.as_mut() else {
            return;
        };
        let write = file.write_all(bytes).and_then(|()| file.write_all(newline));
        if write.is_err() {
            state.disabled = true;
            state.file = None;
            return;
        }
        state.written_bytes += (bytes.len() + newline.len()) as u64;
        if state.written_bytes >= state.max_bytes {
            rotate(&mut state);
        }
    }

    /// Сбросить буферы на диск (для тестов/корректного завершения).
    pub fn flush(&self) {
        let mut state = match self.state.lock() {
            Ok(state) => state,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let Some(file) = state.file.as_mut() {
            let _ = file.flush();
        }
    }
}

/// Открыть файл лога (append; создать каталог при необходимости).
/// Права 0600 — секреты до redaction могли бы попасть в файл.
fn open_sink_file(state: &mut SinkState) -> bool {
    if let Some(parent) = state.path.parent()
        && !parent.as_os_str().is_empty()
        && fs::create_dir_all(parent).is_err()
    {
        state.disabled = true;
        return false;
    }
    match OpenOptions::new()
        .create(true)
        .append(true)
        .open(&state.path)
    {
        Ok(file) => {
            state.written_bytes = file.metadata().map_or(0, |meta| meta.len());
            state.file = Some(file);
            restrict_permissions(&state.path);
            true
        }
        Err(_) => {
            state.disabled = true;
            false
        }
    }
}

/// Ротация: `csqtt.log → csqtt.log.1` (перезапись старого .1), затем
/// новый пустой `csqtt.log`. Хранится ОДИН предыдущий файл: главный
/// ротатор службы CSQTT (M5) может расширить, M4a фиксирует схему.
/// При неуспешном rename счётчик пересчитывается из метаданных —
/// иначе ротация замолчала бы и файл рос без ограничений.
fn rotate(state: &mut SinkState) {
    let path = state.path.clone();
    state.file = None;
    let rotated = rotated_path(&state.path);
    let _ = fs::rename(&path, &rotated);
    match OpenOptions::new().create(true).append(true).open(&path) {
        Ok(file) => {
            state.written_bytes = file.metadata().map_or(0, |meta| meta.len());
            state.file = Some(file);
        }
        Err(_) => {
            state.disabled = true;
        }
    }
}

fn rotated_path(path: &Path) -> PathBuf {
    let mut rotated = path.as_os_str().to_os_string();
    rotated.push(format!(".{ROTATED_SUFFIX}"));
    PathBuf::from(rotated)
}

/// Права 0600 (unix): лог содержит в т.ч. диагностику до redaction.
fn restrict_permissions(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o600));
    }
    #[cfg(not(unix))]
    let _ = path;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uci::REDACTED;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let index = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir()
            .join("csqtt-m4a-logsink")
            .join(format!("{tag}-{index}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn writes_lines_and_creates_file() {
        let dir = temp_dir("write");
        let log = dir.join("csqtt.log");
        let sink = FileLogSink::new(&log, 1024 * 1024, Vec::new());
        sink.write_line("первая строка");
        sink.write_line("вторая строка");
        sink.flush();
        let content = fs::read_to_string(&log).unwrap();
        assert_eq!(content, "первая строка\nвторая строка\n");
    }

    #[test]
    fn redacts_secrets_before_writing() {
        let dir = temp_dir("redact");
        let log = dir.join("csqtt.log");
        let secrets = vec!["examplePassword1".to_string()];
        let sink = FileLogSink::new(&log, 1024 * 1024, secrets);
        sink.write_line("connect password=examplePassword1 ok");
        sink.write_line("чистая строка");
        sink.flush();
        let content = fs::read_to_string(&log).unwrap();
        assert!(!content.contains("examplePassword1"));
        assert_eq!(
            content,
            format!("connect password={REDACTED} ok\nчистая строка\n")
        );
    }

    #[test]
    fn rotates_on_size_threshold() {
        let dir = temp_dir("rotate");
        let log = dir.join("csqtt.log");
        // Порог: 2 строки по 5 байт (aa\n = 3, но проверка после записи)
        let sink = FileLogSink::new(&log, 10, Vec::new());
        sink.write_line("aaaa");
        sink.write_line("bbbb");
        sink.write_line("cccc");
        sink.flush();
        // Текущий файл начинается заново после ротации
        assert_eq!(fs::read_to_string(&log).unwrap(), "cccc\n");
        // Ротированный сохранил старые строки
        assert_eq!(
            fs::read_to_string(rotated_path(&log)).unwrap(),
            "aaaa\nbbbb\n"
        );
    }

    #[test]
    fn broken_path_disables_sink_silently() {
        // Файл внутри несоздаваемого «каталога-файла»
        let dir = temp_dir("broken");
        let blocker = dir.join("blocker");
        fs::write(&blocker, b"file").unwrap();
        let sink = FileLogSink::new(blocker.join("csqtt.log"), 1024, Vec::new());
        sink.write_line("строка");
        sink.write_line("ещё строка");
        sink.flush();
        // Тихо: не паникует, не пишет
        assert!(!blocker.join("csqtt.log").exists());
    }

    #[test]
    fn existing_file_size_counts_toward_rotation() {
        let dir = temp_dir("existing");
        let log = dir.join("csqtt.log");
        fs::write(&log, "x".repeat(100)).unwrap();
        // Порог 101 байт: уже 100 — одна строка вызовет ротацию
        let sink = FileLogSink::new(&log, 101, Vec::new());
        sink.write_line("new");
        sink.flush();
        // Ротация по ПОСЛЕДНЕЙ записи: .1 = старые 100 байт + строка,
        // перевалившая порог; текущий файл пуст.
        assert_eq!(
            fs::read_to_string(rotated_path(&log)).unwrap(),
            format!("{}new\n", "x".repeat(100))
        );
        assert_eq!(fs::read_to_string(&log).unwrap(), "");
    }

    #[test]
    fn rotated_file_is_overwritten_on_second_rotation() {
        let dir = temp_dir("twice");
        let log = dir.join("csqtt.log");
        let sink = FileLogSink::new(&log, 5, Vec::new());
        sink.write_line("aa");
        sink.write_line("bb");
        sink.write_line("cc");
        sink.flush();
        // .1 всегда только последний слепок
        assert_eq!(fs::read_to_string(rotated_path(&log)).unwrap(), "aa\nbb\n");
        assert_eq!(fs::read_to_string(&log).unwrap(), "cc\n");
    }

    #[test]
    fn secrets_can_be_updated_after_creation() {
        let dir = temp_dir("secrets-update");
        let log = dir.join("csqtt.log");
        let sink = FileLogSink::new(&log, 1024 * 1024, Vec::new());
        sink.write_line("password=examplePassword1");
        sink.set_secrets(vec!["examplePassword1".to_string()]);
        sink.write_line("password=examplePassword1");
        sink.flush();
        let content = fs::read_to_string(&log).unwrap();
        let mut lines = content.lines();
        // Первая строка — до обновления секретов, вторая — замаскирована.
        assert_eq!(lines.next().unwrap(), "password=examplePassword1");
        assert_eq!(lines.next().unwrap(), format!("password={REDACTED}"));
    }

    #[test]
    fn failed_rename_keeps_sink_alive_with_true_counter() {
        let dir = temp_dir("rename-fail");
        let log = dir.join("csqtt.log");
        // Каталог вместо .1: rename упадёт (непустой каталог).
        let blocked = rotated_path(&log);
        fs::create_dir_all(&blocked).unwrap();
        fs::write(blocked.join("keep"), b"x").unwrap();
        let sink = FileLogSink::new(&log, 5, Vec::new());
        sink.write_line("aa");
        sink.write_line("bb");
        sink.write_line("cc");
        sink.flush();
        // Строки не потеряны, паники нет; ротация повторится позже.
        assert_eq!(fs::read_to_string(&log).unwrap(), "aa\nbb\ncc\n");
        assert!(blocked.is_dir());
    }
}
