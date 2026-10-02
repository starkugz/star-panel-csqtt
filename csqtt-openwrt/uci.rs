// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [M4a] UCI profile-pool: типизированная модель /etc/config/csqtt.
//!
//! Единая точка правды для M4–M8 (PROMPTS.md v5.1, UCI-СХЕМА M4a):
//! - `config csqtt 'main'` → `MainConfig`;
//! - `config csqtt 'routing'` → `RoutingConfig` (mode auto|none — итог
//!   M3X: interface-only, БЕЗ route/rule/table; других режимов нет);
//! - `config server '<id>'` → `ServerProfile` (пул профилей).
//!
//! ЖЁСТКИЕ ПРАВИЛА (PROMPTS.md M4a):
//! - `csqtt0` фиксирован (константа `TUN_INTERFACE`), не UCI-поле;
//! - runtime-значения (state/rx/tx/…) НИКОГДА не в UCI — только в
//!   /var/run/csqtt/status.json (M4b);
//! - secrets (password, vk_js_token) не выдаются через статусы/UI
//!   (`collect_secrets` + `redact_secrets`);
//! - device_id не меняется и не генерируется (переносится как есть);
//! - фактический порядок профилей — по числовому priority; одинаковые
//!   priority → deterministic tie-break по порядку в UCI-файле.
//!
//! VK input — семантика upstream-парсера ядра (`worker::parse_hashes`),
//! без придуманных лимитов/нормализации.

use crate::ClientConfig;
use crate::worker::{WORKERS_PER_GROUP, parse_hashes};
use crate::{MAX_VK_HASHES, MAX_WORKERS};
use std::collections::HashSet;

/// Имя TUN-интерфейса — фиксировано контрактом M3X (правило 1 UCI-схемы).
pub const TUN_INTERFACE: &str = "csqtt0";
/// Дефолтное число воркеров нового профиля (CLI main.rs).
pub const DEFAULT_PROFILE_WORKERS: usize = 18;
/// Дефолтный device_id, если в профиле пусто (CLI main.rs default; НЕ генерация).
pub const DEFAULT_DEVICE_ID: &str = "openwrt";
/// Дефолтный priority нового профиля при импорте.
pub const DEFAULT_PROFILE_PRIORITY: u32 = 10;

/// MTU: допустимый диапазон для csqtt0 (IPv4-минимум 576 … u16-максимум).
const MTU_MIN: u16 = 576;
const MTU_MAX: u16 = 65535;
/// Ротация файла лога: 16 KiB … 64 MiB.
const LOG_SIZE_KB_MIN: u64 = 16;
const LOG_SIZE_KB_MAX: u64 = 65536;

// ===========================================================================
// Raw UCI (синтаксис /etc/config/*)
// ===========================================================================

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UciOption {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UciListEntry {
    pub key: String,
    pub value: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UciSection {
    pub kind: String,
    pub name: Option<String>,
    pub options: Vec<UciOption>,
    pub lists: Vec<UciListEntry>,
}

impl UciSection {
    pub fn option(&self, key: &str) -> Option<&str> {
        self.options
            .iter()
            .find(|option| option.key == key)
            .map(|option| option.value.as_str())
    }

    pub fn set_option(&mut self, key: &str, value: &str) {
        if let Some(option) = self.options.iter_mut().find(|option| option.key == key) {
            option.value = value.to_string();
        } else {
            self.options.push(UciOption {
                key: key.to_string(),
                value: value.to_string(),
            });
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UciFile {
    pub sections: Vec<UciSection>,
}

impl UciFile {
    pub fn named_section(&self, kind: &str, name: &str) -> Option<&UciSection> {
        self.sections
            .iter()
            .find(|section| section.kind == kind && section.name.as_deref() == Some(name))
    }

    pub fn named_section_mut(&mut self, kind: &str, name: &str) -> Option<&mut UciSection> {
        self.sections
            .iter_mut()
            .find(|section| section.kind == kind && section.name.as_deref() == Some(name))
    }

    pub fn server_ids(&self) -> Vec<String> {
        self.sections
            .iter()
            .filter(|section| section.kind == "server")
            .filter_map(|section| section.name.clone())
            .collect()
    }
}

/// Синтаксическая ошибка UCI-файла (строка, причина).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UciSyntaxError {
    pub line: usize,
    pub message: String,
}

impl std::fmt::Display for UciSyntaxError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "строка {}: {}", self.line, self.message)
    }
}

/// Разбор текста UCI-конфига (синтаксис /etc/config/*).
/// Поддерживает: `config <type> ['<name>']`, `option <key> <value>`,
/// `list <key> <value>`, комментарии `#` (строчные), одинарные/двойные
/// кавычки и unquoted-значения.
pub fn parse_uci(text: &str) -> Result<UciFile, UciSyntaxError> {
    let mut file = UciFile::default();
    for (index, raw_line) in text.lines().enumerate() {
        let line_number = index + 1;
        let trimmed = raw_line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        let mut words = trimmed.split_ascii_whitespace();
        let directive = words.next().unwrap_or_default();
        match directive {
            "config" => {
                let kind = words.next().ok_or(UciSyntaxError {
                    line: line_number,
                    message: "config без типа секции".to_string(),
                })?;
                let name = words.next().map(strip_quotes);
                file.sections.push(UciSection {
                    kind: kind.to_string(),
                    name,
                    options: Vec::new(),
                    lists: Vec::new(),
                });
            }
            "option" | "list" => {
                let key = words.next().ok_or(UciSyntaxError {
                    line: line_number,
                    message: format!("{directive} без ключа"),
                })?;
                let value = extract_value(trimmed);
                let section = file.sections.last_mut().ok_or(UciSyntaxError {
                    line: line_number,
                    message: format!("{directive} вне секции config"),
                })?;
                if directive == "option" {
                    section.options.push(UciOption {
                        key: strip_quotes(key),
                        value,
                    });
                } else {
                    section.lists.push(UciListEntry {
                        key: strip_quotes(key),
                        value,
                    });
                }
            }
            other => {
                return Err(UciSyntaxError {
                    line: line_number,
                    message: format!("неизвестная директива {other:?}"),
                });
            }
        }
    }
    Ok(file)
}

/// Значение опции: `'…'` (до последней `'` в строке), `"…"` (с escape),
/// либо unquoted-токен до whitespace.
fn extract_value(line: &str) -> String {
    let value_part = words_next_position(line).trim();
    if let Some(inner) = value_part.strip_prefix('\'') {
        if let Some(last) = inner.rfind('\'') {
            return inner[..last].to_string();
        }
        return inner.to_string();
    }
    if let Some(inner) = value_part.strip_prefix('"') {
        return unescape_double_quoted(inner);
    }
    value_part
        .split_ascii_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// Позиция значения: после директивы и ключа.
fn words_next_position(line: &str) -> &str {
    let first = line.trim_start();
    // директива
    let directive_end = first.find(char::is_whitespace).unwrap_or(first.len());
    let after_directive = &first[directive_end..];
    let key_start = after_directive.trim_start();
    if key_start.is_empty() {
        return "";
    }
    // ключ: либо quoted-токен, либо слово
    let key_end = if key_start.starts_with('\'') || key_start.starts_with('"') {
        let quote = key_start.as_bytes()[0] as char;
        key_start[1..]
            .find(quote)
            .map(|at| at + 2)
            .unwrap_or(key_start.len())
    } else {
        key_start
            .find(char::is_whitespace)
            .unwrap_or(key_start.len())
    };
    &key_start[key_end..]
}

fn unescape_double_quoted(inner: &str) -> String {
    let mut result = String::with_capacity(inner.len());
    let mut chars = inner.chars().peekable();
    while let Some(character) = chars.next() {
        if character == '"' {
            break;
        }
        if character == '\\' {
            match chars.next() {
                Some('"') => result.push('"'),
                Some('\\') => result.push('\\'),
                Some('n') => result.push('\n'),
                Some('t') => result.push('\t'),
                Some(other) => {
                    result.push('\\');
                    result.push(other);
                }
                None => result.push('\\'),
            }
        } else {
            result.push(character);
        }
    }
    result
}

fn strip_quotes(token: &str) -> String {
    let value = token;
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|rest| rest.strip_suffix('\''))
    {
        return inner.to_string();
    }
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|rest| rest.strip_suffix('"'))
    {
        return inner.to_string();
    }
    value.to_string()
}

/// Каноническая сериализация UCI-файла (roundtrip parse→render→parse идемпотентен).
pub fn render_uci(file: &UciFile) -> String {
    let mut output = String::new();
    for (index, section) in file.sections.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        match &section.name {
            Some(name) => {
                output.push_str(&format!("config {} '{}'\n", section.kind, name));
            }
            None => {
                output.push_str(&format!("config {}\n", section.kind));
            }
        }
        for option in &section.options {
            output.push_str(&format!("\toption {} '{}'\n", option.key, option.value));
        }
        for list in &section.lists {
            output.push_str(&format!("\tlist {} '{}'\n", list.key, list.value));
        }
    }
    output
}

// ===========================================================================
// Typed модель
// ===========================================================================

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SelectionMode {
    #[default]
    Priority,
    Manual,
}

impl SelectionMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "priority" => Some(Self::Priority),
            "manual" => Some(Self::Manual),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Priority => "priority",
            Self::Manual => "manual",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HealthMode {
    Transport,
    Data,
    #[default]
    Both,
}

impl HealthMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "transport" => Some(Self::Transport),
            "data" => Some(Self::Data),
            "both" => Some(Self::Both),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Data => "data",
            Self::Both => "both",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaptchaPolicy {
    #[default]
    Failover,
    Wait,
}

impl CaptchaPolicy {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "failover" => Some(Self::Failover),
            "wait" => Some(Self::Wait),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Failover => "failover",
            Self::Wait => "wait",
        }
    }
}

/// Режим маршрутизации: `auto` = interface-only контракт M3X (БЕЗ
/// route/rule/table), `none` — отладочный алиас. Других production-режимов
/// нет: oif/main_metric опровергнуты M3X (PROMPTS.md v5.1).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RoutingMode {
    #[default]
    Auto,
    None,
}

impl RoutingMode {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "auto" => Some(Self::Auto),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::None => "none",
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LogLevel {
    Error,
    Warn,
    #[default]
    Info,
    Debug,
}

impl LogLevel {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "error" => Some(Self::Error),
            "warn" => Some(Self::Warn),
            "info" => Some(Self::Info),
            "debug" => Some(Self::Debug),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warn => "warn",
            Self::Info => "info",
            Self::Debug => "debug",
        }
    }
}

/// Ошибка конфигурации: секция + поле + человекочитаемая причина.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigIssue {
    pub section: String,
    pub field: String,
    pub message: String,
}

impl ConfigIssue {
    fn new(section: &str, field: &str, message: impl Into<String>) -> Self {
        Self {
            section: section.to_string(),
            field: field.to_string(),
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}: {}", self.section, self.field, self.message)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MainConfig {
    pub enabled: bool,
    pub selection_mode: SelectionMode,
    pub active_profile: String,
    pub failover: bool,
    pub failback: bool,
    pub health_interval: u64,
    pub fail_threshold: u32,
    pub success_threshold: u32,
    pub cooldown: u64,
    pub reconnect_delay: u64,
    pub failback_stable_time: u64,
    pub health_mode: HealthMode,
    pub health_target: String,
    pub captcha_policy: CaptchaPolicy,
    pub tun_address: String,
    pub tun_mtu: u16,
    pub dns: String,
    pub log_level: LogLevel,
    pub log_file: String,
    pub log_size_kb: u64,
}

impl Default for MainConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            selection_mode: SelectionMode::default(),
            active_profile: String::new(),
            failover: true,
            failback: false,
            health_interval: 5,
            fail_threshold: 3,
            success_threshold: 2,
            cooldown: 60,
            reconnect_delay: 5,
            failback_stable_time: 60,
            health_mode: HealthMode::default(),
            health_target: String::new(),
            captcha_policy: CaptchaPolicy::default(),
            tun_address: String::new(),
            tun_mtu: 1280,
            dns: String::new(),
            log_level: LogLevel::default(),
            log_file: "/var/log/csqtt.log".to_string(),
            log_size_kb: 512,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RoutingConfig {
    pub mode: RoutingMode,
}

/// Профиль сервера (секция `config server '<id>'`).
/// Secrets: `password`, `vk_js_token` — не выдавать через статусы/UI.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerProfile {
    pub section_id: String,
    /// Порядок в UCI-файле (tie-break одинаковых priority).
    pub order: usize,
    pub name: String,
    pub enabled: bool,
    pub priority: u32,
    pub peer: String,
    /// SECRET.
    pub password: String,
    pub vk: String,
    pub workers: usize,
    pub obfs: String,
    pub turn_transport: String,
    pub captcha_mode: String,
    pub fingerprint: String,
    pub client_ids: String,
    pub vk_auth_mode: String,
    pub vk_hash_mode: String,
    /// SECRET.
    pub vk_js_token: String,
    /// Привязан к паролю на сервере; НЕ менять/не генерировать.
    pub device_id: String,
    /// Per-profile override; `None` = использовать global (main).
    pub fail_threshold: Option<u32>,
    pub success_threshold: Option<u32>,
    pub cooldown: Option<u64>,
    pub captcha_policy: Option<CaptchaPolicy>,
    /// UI-only метаданные; транспорт не использует.
    pub note: String,
}

impl Default for ServerProfile {
    fn default() -> Self {
        Self {
            section_id: String::new(),
            order: 0,
            name: String::new(),
            enabled: true,
            priority: DEFAULT_PROFILE_PRIORITY,
            peer: String::new(),
            password: String::new(),
            vk: String::new(),
            workers: DEFAULT_PROFILE_WORKERS,
            obfs: "audio".to_string(),
            turn_transport: "udp".to_string(),
            captcha_mode: "auto".to_string(),
            fingerprint: "chrome".to_string(),
            client_ids: String::new(),
            vk_auth_mode: String::new(),
            vk_hash_mode: String::new(),
            vk_js_token: String::new(),
            device_id: String::new(),
            fail_threshold: None,
            success_threshold: None,
            cooldown: None,
            captcha_policy: None,
            note: String::new(),
        }
    }
}

/// Пул конфигурации: main + routing + профили (в порядке UCI-файла).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProfilePool {
    pub main: MainConfig,
    pub routing: RoutingConfig,
    pub servers: Vec<ServerProfile>,
}

impl ProfilePool {
    /// Типизация UCI-файла со сбором ВСЕХ ошибок (не первой).
    /// Пустые секции main/routing допустимы (используются дефолты);
    /// ошибки — только явный мусор.
    pub fn from_uci(file: &UciFile) -> Result<Self, Vec<ConfigIssue>> {
        let mut issues = Vec::new();
        let mut main: Option<MainConfig> = None;
        let mut routing: Option<RoutingConfig> = None;
        let mut servers: Vec<ServerProfile> = Vec::new();
        let mut seen_server_ids: HashSet<String> = HashSet::new();
        for (index, section) in file.sections.iter().enumerate() {
            match section.kind.as_str() {
                "csqtt" => {
                    let name = section.name.clone().unwrap_or_default();
                    match name.as_str() {
                        "main" => {
                            if main.is_some() {
                                issues.push(ConfigIssue::new(
                                    "main",
                                    "section",
                                    "секция csqtt 'main' объявлена повторно",
                                ));
                                continue;
                            }
                            main = Some(parse_main_section(section, &mut issues));
                        }
                        "routing" => {
                            if routing.is_some() {
                                issues.push(ConfigIssue::new(
                                    "routing",
                                    "section",
                                    "секция csqtt 'routing' объявлена повторно",
                                ));
                                continue;
                            }
                            routing = Some(parse_routing_section(section, &mut issues));
                        }
                        "" => issues.push(ConfigIssue::new(
                            "csqtt",
                            "section",
                            "секция csqtt без имени (допустимы только main/routing)",
                        )),
                        other => issues.push(ConfigIssue::new(
                            other,
                            "section",
                            "неизвестная именованная секция csqtt (только main/routing)",
                        )),
                    }
                }
                "server" => {
                    let id = match section.name.as_deref() {
                        Some(name) if !name.trim().is_empty() => name.to_string(),
                        _ => {
                            issues.push(ConfigIssue::new(
                                "server",
                                "section",
                                "секция server без имени — profile id обязателен",
                            ));
                            continue;
                        }
                    };
                    if !seen_server_ids.insert(id.clone()) {
                        issues.push(ConfigIssue::new(
                            &id,
                            "section",
                            "дубликат section id профиля",
                        ));
                        continue;
                    }
                    let (profile, mut section_issues) = parse_server_section(&id, section, index);
                    issues.append(&mut section_issues);
                    servers.push(profile);
                }
                other => issues.push(ConfigIssue::new(
                    other,
                    "section",
                    "неизвестный тип секции (допустимы только csqtt и server)",
                )),
            }
        }
        let main = main.unwrap_or_default();
        let routing = routing.unwrap_or_default();
        let pool = Self {
            main,
            routing,
            servers,
        };
        pool.validate(&mut issues);
        if issues.is_empty() {
            Ok(pool)
        } else {
            Err(issues)
        }
    }

    pub fn from_text(text: &str) -> Result<Self, Vec<ConfigIssue>> {
        let file = parse_uci(text)
            .map_err(|error| vec![ConfigIssue::new("uci", "syntax", error.to_string())])?;
        Self::from_uci(&file)
    }

    /// Семантическая валидация поверх типизации.
    pub fn validate(&self, issues: &mut Vec<ConfigIssue>) {
        let main = &self.main;
        // manual: active_profile существует и enabled.
        if main.selection_mode == SelectionMode::Manual && main.active_profile.trim().is_empty() {
            issues.push(ConfigIssue::new(
                "main",
                "active_profile",
                "в manual-режиме active_profile обязателен",
            ));
        }
        if !main.active_profile.trim().is_empty() {
            match self.server(&main.active_profile) {
                Some(profile) => {
                    if main.selection_mode == SelectionMode::Manual && !profile.enabled {
                        issues.push(ConfigIssue::new(
                            "main",
                            "active_profile",
                            format!(
                                "профиль «{}» выключен (manual-режим требует enabled)",
                                main.active_profile
                            ),
                        ));
                    }
                }
                None => issues.push(ConfigIssue::new(
                    "main",
                    "active_profile",
                    format!("профиль «{}» не существует", main.active_profile),
                )),
            }
        }
        for profile in &self.servers {
            validate_profile_semantics(profile, self, issues);
        }
    }

    pub fn server(&self, id: &str) -> Option<&ServerProfile> {
        self.servers.iter().find(|profile| profile.section_id == id)
    }

    /// Включённые профили в порядке фактического выбора: числовой priority
    /// по возрастанию; одинаковые priority — deterministic tie-break по
    /// порядку в UCI-файле (правило 2 UCI-схемы).
    pub fn ordered_enabled_servers(&self) -> Vec<&ServerProfile> {
        let mut enabled: Vec<&ServerProfile> = self
            .servers
            .iter()
            .filter(|profile| profile.enabled)
            .collect();
        enabled.sort_by(|left, right| {
            left.priority
                .cmp(&right.priority)
                .then(left.order.cmp(&right.order))
        });
        enabled
    }

    /// Профиль, выбранный текущей конфигурацией (priority → первый
    /// включённый; manual → active_profile). Для M4b.
    pub fn selected_profile(&self) -> Option<&ServerProfile> {
        match self.main.selection_mode {
            SelectionMode::Manual => self.server(&self.main.active_profile),
            SelectionMode::Priority => self.ordered_enabled_servers().first().copied(),
        }
    }

    // --- Effective overrides: per-profile поверх global (правило 8) ---

    pub fn effective_fail_threshold(&self, profile: &ServerProfile) -> u32 {
        profile.fail_threshold.unwrap_or(self.main.fail_threshold)
    }

    pub fn effective_success_threshold(&self, profile: &ServerProfile) -> u32 {
        profile
            .success_threshold
            .unwrap_or(self.main.success_threshold)
    }

    pub fn effective_cooldown(&self, profile: &ServerProfile) -> u64 {
        profile.cooldown.unwrap_or(self.main.cooldown)
    }

    pub fn effective_captcha_policy(&self, profile: &ServerProfile) -> CaptchaPolicy {
        profile.captcha_policy.unwrap_or(self.main.captcha_policy)
    }

    /// Секреты, недопустимые в логах/статусах/UI (правило 10 UCI-схемы).
    pub fn collect_secrets(&self) -> Vec<String> {
        let mut secrets: Vec<String> = self
            .servers
            .iter()
            .flat_map(|profile| [&profile.password, &profile.vk_js_token])
            .filter(|secret| !secret.is_empty())
            .cloned()
            .collect();
        secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
        secrets.dedup();
        secrets
    }

    /// Полный конфиг ядра для профиля. Правила M3X/M4a: interface-only
    /// (install_routes=false, apply_dns=false), csqtt0 фиксирован,
    /// пустые enum-поля → дефолты CLI main.rs, device_id НЕ генерируется.
    pub fn client_config_for(&self, profile: &ServerProfile) -> ClientConfig {
        ClientConfig {
            vk: profile.vk.clone(),
            vk_hash_mode: non_empty_or(&profile.vk_hash_mode, "manual"),
            peer: profile.peer.clone(),
            workers: crate::normalized_workers(profile.workers),
            device_id: non_empty_or(&profile.device_id, DEFAULT_DEVICE_ID),
            password: profile.password.clone(),
            vk_auth_mode: non_empty_or(&profile.vk_auth_mode, "vkcalls"),
            captcha_mode: non_empty_or(&profile.captcha_mode, "auto"),
            fingerprint: non_empty_or(&profile.fingerprint, "chrome"),
            client_ids: profile.client_ids.clone(),
            obfs: non_empty_or(&profile.obfs, "audio"),
            turn_transport: non_empty_or(&profile.turn_transport, "udp"),
            vk_js_token: profile.vk_js_token.clone(),
            tun_uds: TUN_INTERFACE.to_string(),
            ..ClientConfig::default()
        }
    }
}

fn non_empty_or(value: &str, fallback: &str) -> String {
    if value.trim().is_empty() {
        fallback.to_string()
    } else {
        value.to_string()
    }
}

// ===========================================================================
// Разбор секций
// ===========================================================================

fn parse_bool(raw: &str) -> Option<bool> {
    match raw.trim().to_ascii_lowercase().as_str() {
        "0" | "false" | "off" | "no" => Some(false),
        "1" | "true" | "on" | "yes" => Some(true),
        _ => None,
    }
}

fn bool_field(
    section: &UciSection,
    section_id: &str,
    field: &str,
    default: bool,
    issues: &mut Vec<ConfigIssue>,
) -> bool {
    match section.option(field) {
        None => default,
        Some(raw) => match parse_bool(raw) {
            Some(value) => value,
            None => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("значение {raw:?} не является 0/1"),
                ));
                default
            }
        },
    }
}

fn numeric_field<T: std::str::FromStr + std::fmt::Display + Copy>(
    section: &UciSection,
    section_id: &str,
    field: &str,
    default: T,
    check: impl Fn(T) -> bool,
    check_message: &str,
    issues: &mut Vec<ConfigIssue>,
) -> T {
    match section.option(field) {
        None => default,
        Some(raw) => match raw.trim().parse::<T>() {
            Ok(value) if check(value) => value,
            Ok(value) => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("значение {value} {check_message}"),
                ));
                default
            }
            Err(_) => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("значение {raw:?} не является числом"),
                ));
                default
            }
        },
    }
}

fn string_field(section: &UciSection, field: &str, default: &str) -> String {
    match section.option(field) {
        Some(raw) if !raw.trim().is_empty() => raw.to_string(),
        _ => default.to_string(),
    }
}

fn optional_string_field(section: &UciSection, field: &str) -> Option<String> {
    section
        .option(field)
        .and_then(|raw| (!raw.trim().is_empty()).then(|| raw.to_string()))
}

fn enum_field<T>(
    section: &UciSection,
    section_id: &str,
    field: &str,
    default: T,
    parse: impl Fn(&str) -> Option<T>,
    allowed: &str,
    issues: &mut Vec<ConfigIssue>,
) -> T {
    match section.option(field) {
        None => default,
        Some(raw) if raw.trim().is_empty() => default,
        Some(raw) => match parse(raw) {
            Some(value) => value,
            None => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("значение {raw:?} не входит в {allowed}"),
                ));
                default
            }
        },
    }
}

fn parse_main_section(section: &UciSection, issues: &mut Vec<ConfigIssue>) -> MainConfig {
    let default = MainConfig::default();
    let enabled = bool_field(section, "main", "enabled", default.enabled, issues);
    let selection_mode = enum_field(
        section,
        "main",
        "selection_mode",
        default.selection_mode,
        SelectionMode::parse,
        "priority|manual",
        issues,
    );
    let active_profile = string_field(section, "active_profile", "");
    let failover = bool_field(section, "main", "failover", default.failover, issues);
    let failback = bool_field(section, "main", "failback", default.failback, issues);
    let health_interval = numeric_field(
        section,
        "main",
        "health_interval",
        default.health_interval,
        |value| value > 0,
        "должно быть > 0 (секунды)",
        issues,
    );
    let fail_threshold = numeric_field(
        section,
        "main",
        "fail_threshold",
        default.fail_threshold,
        |value| value >= 1,
        "должно быть >= 1",
        issues,
    );
    let success_threshold = numeric_field(
        section,
        "main",
        "success_threshold",
        default.success_threshold,
        |value| value >= 1,
        "должно быть >= 1",
        issues,
    );
    let cooldown = numeric_field(
        section,
        "main",
        "cooldown",
        default.cooldown,
        |value| value > 0,
        "должно быть > 0 (секунды)",
        issues,
    );
    let reconnect_delay = numeric_field(
        section,
        "main",
        "reconnect_delay",
        default.reconnect_delay,
        |value| value > 0,
        "должно быть > 0 (секунды)",
        issues,
    );
    let failback_stable_time = numeric_field(
        section,
        "main",
        "failback_stable_time",
        default.failback_stable_time,
        |value| value > 0,
        "должно быть > 0 (секунды)",
        issues,
    );
    let health_mode = enum_field(
        section,
        "main",
        "health_mode",
        default.health_mode,
        HealthMode::parse,
        "transport|data|both",
        issues,
    );
    let health_target = string_field(section, "health_target", "");
    if !health_target.trim().is_empty()
        && let Err(message) = validate_health_target(&health_target)
    {
        issues.push(ConfigIssue::new("main", "health_target", message));
    }
    let captcha_policy = enum_field(
        section,
        "main",
        "captcha_policy",
        default.captcha_policy,
        CaptchaPolicy::parse,
        "failover|wait",
        issues,
    );
    let tun_address = string_field(section, "tun_address", "");
    if !tun_address.trim().is_empty() && validate_ip_or_cidr(&tun_address).is_err() {
        issues.push(ConfigIssue::new(
            "main",
            "tun_address",
            "ожидается IP или IP/префикс (например 10.66.67.7/24)",
        ));
    }
    let tun_mtu = numeric_field(
        section,
        "main",
        "tun_mtu",
        default.tun_mtu,
        |value: u16| (MTU_MIN..=MTU_MAX).contains(&value),
        &format!("должно быть в диапазоне {MTU_MIN}..={MTU_MAX}"),
        issues,
    );
    let dns = string_field(section, "dns", "");
    if !dns.trim().is_empty() {
        for entry in dns
            .split([',', ' ', ';', '\t'])
            .filter(|part| !part.is_empty())
        {
            if entry.parse::<std::net::IpAddr>().is_err() {
                issues.push(ConfigIssue::new(
                    "main",
                    "dns",
                    format!("«{entry}» не является IP-адресом (DNS ТУННЕЛЯ, не системный)"),
                ));
                break;
            }
        }
    }
    let log_level = enum_field(
        section,
        "main",
        "log_level",
        default.log_level,
        LogLevel::parse,
        "error|warn|info|debug",
        issues,
    );
    let log_file = string_field(section, "log_file", &default.log_file);
    if !log_file.starts_with('/') {
        issues.push(ConfigIssue::new(
            "main",
            "log_file",
            "путь должен быть абсолютным (начинаться с /)",
        ));
    }
    let log_size_kb = numeric_field(
        section,
        "main",
        "log_size_kb",
        default.log_size_kb,
        |value| (LOG_SIZE_KB_MIN..=LOG_SIZE_KB_MAX).contains(&value),
        &format!("должно быть в диапазоне {LOG_SIZE_KB_MIN}..={LOG_SIZE_KB_MAX} KiB"),
        issues,
    );
    MainConfig {
        enabled,
        selection_mode,
        active_profile,
        failover,
        failback,
        health_interval,
        fail_threshold,
        success_threshold,
        cooldown,
        reconnect_delay,
        failback_stable_time,
        health_mode,
        health_target,
        captcha_policy,
        tun_address,
        tun_mtu,
        dns,
        log_level,
        log_file,
        log_size_kb,
    }
}

fn parse_routing_section(section: &UciSection, issues: &mut Vec<ConfigIssue>) -> RoutingConfig {
    let mode = enum_field(
        section,
        "routing",
        "mode",
        RoutingMode::default(),
        RoutingMode::parse,
        "auto|none (контракт M3X: interface-only, других режимов нет)",
        issues,
    );
    RoutingConfig { mode }
}

fn parse_server_section(
    id: &str,
    section: &UciSection,
    order: usize,
) -> (ServerProfile, Vec<ConfigIssue>) {
    let mut issues = Vec::new();
    let default = ServerProfile::default();
    // Section id: charset-проверка (имена UCI-секций).
    if id
        .chars()
        .any(|character| !character.is_ascii_alphanumeric() && character != '_')
    {
        issues.push(ConfigIssue::new(
            id,
            "section",
            "id секции: только [A-Za-z0-9_]",
        ));
    }
    if matches!(id, "main" | "routing") {
        issues.push(ConfigIssue::new(
            id,
            "section",
            "id профиля не может называться main/routing",
        ));
    }
    let name = optional_string_field(section, "name").unwrap_or_else(|| id.to_string());
    let enabled = bool_field(section, id, "enabled", default.enabled, &mut issues);
    let priority = numeric_field(
        section,
        id,
        "priority",
        default.priority,
        |_| true,
        "",
        &mut issues,
    );
    let peer = string_field(section, "peer", "");
    let password = optional_string_field(section, "password").unwrap_or_default();
    let vk = optional_string_field(section, "vk").unwrap_or_default();
    let workers = numeric_field(
        section,
        id,
        "workers",
        default.workers,
        |value: usize| (WORKERS_PER_GROUP..=MAX_WORKERS).contains(&value),
        &format!(
            "в диапазоне {WORKERS_PER_GROUP}..={MAX_WORKERS} (лимит сервера); \
             будет использовано ближайшее кратное {WORKERS_PER_GROUP}"
        ),
        &mut issues,
    );
    let obfs = enum_field(
        section,
        id,
        "obfs",
        default.obfs.clone(),
        |raw| match raw {
            "audio" => Some("audio".to_string()),
            "video" => Some("video".to_string()),
            _ => None,
        },
        "audio|video",
        &mut issues,
    );
    let turn_transport = enum_field(
        section,
        id,
        "turn_transport",
        default.turn_transport.clone(),
        |raw| match raw.trim().to_ascii_lowercase().as_str() {
            "udp" => Some("udp".to_string()),
            "tcp" | "tcp_tls" | "tcp-tls" | "tcp/tls" => Some("tcp_tls".to_string()),
            _ => None,
        },
        "udp|tcp|tcp_tls",
        &mut issues,
    );
    let captcha_mode = enum_field(
        section,
        id,
        "captcha_mode",
        default.captcha_mode.clone(),
        |raw| match raw {
            "auto" => Some("auto".to_string()),
            "wv" => Some("wv".to_string()),
            "rjs" => Some("rjs".to_string()),
            _ => None,
        },
        "auto|wv|rjs",
        &mut issues,
    );
    let fingerprint = enum_field(
        section,
        id,
        "fingerprint",
        default.fingerprint.clone(),
        |raw| {
            matches!(raw, "chrome" | "firefox" | "safari" | "edge" | "opera")
                .then(|| raw.to_string())
        },
        "chrome|firefox|safari|edge|opera",
        &mut issues,
    );
    let client_ids = optional_string_field(section, "client_ids").unwrap_or_default();
    let vk_auth_mode = optional_string_field(section, "vk_auth_mode").unwrap_or_default();
    let vk_hash_mode = optional_string_field(section, "vk_hash_mode").unwrap_or_default();
    let vk_js_token = optional_string_field(section, "vk_js_token").unwrap_or_default();
    let device_id = optional_string_field(section, "device_id").unwrap_or_default();
    let fail_threshold = optional_override_u32(section, id, "fail_threshold", &mut issues);
    let success_threshold = optional_override_u32(section, id, "success_threshold", &mut issues);
    let cooldown = numeric_override(section, id, "cooldown", &mut issues);
    let captcha_policy = match optional_string_field(section, "captcha_policy") {
        None => None,
        Some(raw) => match CaptchaPolicy::parse(&raw) {
            Some(policy) => Some(policy),
            None => {
                issues.push(ConfigIssue::new(
                    id,
                    "captcha_policy",
                    format!("значение {raw:?} не входит в failover|wait"),
                ));
                None
            }
        },
    };
    let note = optional_string_field(section, "note").unwrap_or_default();
    (
        ServerProfile {
            section_id: id.to_string(),
            order,
            name,
            enabled,
            priority,
            peer,
            password,
            vk,
            workers,
            obfs,
            turn_transport,
            captcha_mode,
            fingerprint,
            client_ids,
            vk_auth_mode,
            vk_hash_mode,
            vk_js_token,
            device_id,
            fail_threshold,
            success_threshold,
            cooldown,
            captcha_policy,
            note,
        },
        issues,
    )
}

fn optional_override_u32(
    section: &UciSection,
    section_id: &str,
    field: &str,
    issues: &mut Vec<ConfigIssue>,
) -> Option<u32> {
    optional_override_numeric(
        section,
        section_id,
        field,
        |value| value >= 1,
        ">= 1",
        issues,
    )
}

fn numeric_override(
    section: &UciSection,
    section_id: &str,
    field: &str,
    issues: &mut Vec<ConfigIssue>,
) -> Option<u64> {
    optional_override_numeric(section, section_id, field, |value| value > 0, "> 0", issues)
}

fn optional_override_numeric<T: std::str::FromStr + std::fmt::Display + Copy>(
    section: &UciSection,
    section_id: &str,
    field: &str,
    check: impl Fn(T) -> bool,
    check_message: &str,
    issues: &mut Vec<ConfigIssue>,
) -> Option<T> {
    match optional_string_field(section, field) {
        None => None,
        Some(raw) => match raw.trim().parse::<T>() {
            Ok(value) if check(value) => Some(value),
            Ok(value) => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("override-значение {value} должно быть {check_message}"),
                ));
                None
            }
            Err(_) => {
                issues.push(ConfigIssue::new(
                    section_id,
                    field,
                    format!("override-значение {raw:?} не является числом"),
                ));
                None
            }
        },
    }
}

fn validate_profile_semantics(
    profile: &ServerProfile,
    pool: &ProfilePool,
    issues: &mut Vec<ConfigIssue>,
) {
    let id = profile.section_id.as_str();
    let vk_hash_mode = non_empty_or(&profile.vk_hash_mode, "manual");
    let vk_auth_mode = non_empty_or(&profile.vk_auth_mode, "vkcalls");
    // Зеркало bail ядра: auto_js-авторизация требует auto_js-режима хешей.
    if vk_auth_mode == "auto_js" && vk_hash_mode != "auto_js" {
        issues.push(ConfigIssue::new(
            id,
            "vk_auth_mode",
            "режим авторизации auto_js требует vk_hash_mode=auto_js",
        ));
    }
    if !profile.enabled {
        // Выключенный профиль — черновик: только формат полей (уже проверен).
        return;
    }
    if crate::link::parse_peer(&profile.peer).is_none() {
        issues.push(ConfigIssue::new(
            id,
            "peer",
            "ожидается host:port (или [IPv6]:port) сервера CSQTT",
        ));
    }
    if profile.password.is_empty() {
        issues.push(ConfigIssue::new(
            id,
            "password",
            "для включённого профиля пароль обязателен (WRAP-ключ)",
        ));
    }
    if vk_hash_mode != "auto_js" {
        // VK input — семантика upstream-парсера ядра (worker::parse_hashes),
        // без придуманных лимитов; проверяем только непустой результат и
        // серверный потолок MAX_VK_HASHES.
        let hashes = parse_hashes(&profile.vk);
        if hashes.is_empty() {
            issues.push(ConfigIssue::new(
                id,
                "vk",
                "нет ни одного VK-хеша (парсер ядра дал пустой список)",
            ));
        } else if hashes.len() > MAX_VK_HASHES {
            issues.push(ConfigIssue::new(
                id,
                "vk",
                format!("хешей {MAX_VK_HASHES} максимум, найдено {}", hashes.len()),
            ));
        }
    } else if profile.vk_js_token.trim().is_empty() {
        issues.push(ConfigIssue::new(
            id,
            "vk_js_token",
            "для vk_hash_mode=auto_js токен VK обязателен",
        ));
    }
    let _ = pool;
}

fn validate_health_target(target: &str) -> Result<(), String> {
    if target.contains("://")
        && let Ok(url) = url::Url::parse(target)
        && url.host_str().is_some_and(|host| !host.is_empty())
    {
        return Ok(());
    }
    if target.contains("://") {
        return Err("URL некорректен".to_string());
    }
    if crate::link::parse_peer(target).is_some() {
        return Ok(());
    }
    Err("ожидается host:port или http(s)://URL".to_string())
}

fn validate_ip_or_cidr(value: &str) -> Result<(), ()> {
    let mut parts = value.splitn(2, '/');
    let address = parts.next().ok_or(())?;
    let parsed: std::net::IpAddr = address.parse().map_err(|_| ())?;
    if let Some(prefix) = parts.next() {
        let prefix: u8 = prefix.parse().map_err(|_| ())?;
        let max = if parsed.is_ipv4() { 32 } else { 128 };
        if prefix > max {
            return Err(());
        }
    }
    Ok(())
}

// ===========================================================================
// Мутации UciFile (контракт profile-команд M4c)
// ===========================================================================

/// Создать/обновить главную секцию csqtt 'main'.
fn ensure_main_section(file: &mut UciFile) -> &mut UciSection {
    if file.named_section_mut("csqtt", "main").is_none() {
        file.sections.push(UciSection {
            kind: "csqtt".to_string(),
            name: Some("main".to_string()),
            options: Vec::new(),
            lists: Vec::new(),
        });
    }
    file.named_section_mut("csqtt", "main")
        .expect("секция main только что создана")
}

/// `profile use <id>`: manual + active_profile (правило 9 UCI-схемы).
pub fn uci_set_active_profile(file: &mut UciFile, id: &str) -> Result<(), String> {
    let exists = file
        .sections
        .iter()
        .any(|section| section.kind == "server" && section.name.as_deref() == Some(id));
    if !exists {
        return Err(format!("профиль «{id}» не найден"));
    }
    let main = ensure_main_section(file);
    main.set_option("selection_mode", "manual");
    main.set_option("active_profile", id);
    Ok(())
}

/// `profile auto`: priority-режим (правило 9 UCI-схемы).
pub fn uci_set_selection_auto(file: &mut UciFile) {
    let main = ensure_main_section(file);
    main.set_option("selection_mode", "priority");
    main.set_option("active_profile", "");
}

/// `profile enable/disable <id>`.
pub fn uci_set_server_enabled(file: &mut UciFile, id: &str, enabled: bool) -> Result<(), String> {
    let section = file
        .sections
        .iter_mut()
        .find(|section| section.kind == "server" && section.name.as_deref() == Some(id))
        .ok_or_else(|| format!("профиль «{id}» не найден"))?;
    section.set_option("enabled", if enabled { "1" } else { "0" });
    Ok(())
}

/// Удалить профиль по id.
pub fn uci_remove_server(file: &mut UciFile, id: &str) -> Result<(), String> {
    let before = file.sections.len();
    file.sections
        .retain(|section| !(section.kind == "server" && section.name.as_deref() == Some(id)));
    if file.sections.len() == before {
        return Err(format!("профиль «{id}» не найден"));
    }
    // active_profile, указывающий на удалённый профиль, сбрасываем;
    // manual без профиля — недопустимое состояние → priority (правило 3).
    if let Some(main) = file.named_section_mut("csqtt", "main")
        && main.option("active_profile") == Some(id)
    {
        main.set_option("active_profile", "");
        if main.option("selection_mode") == Some("manual") {
            main.set_option("selection_mode", "priority");
        }
    }
    Ok(())
}

/// Установить опцию профиля (для M4c profile edit).
pub fn uci_set_server_option(
    file: &mut UciFile,
    id: &str,
    key: &str,
    value: &str,
) -> Result<(), String> {
    let section = file
        .sections
        .iter_mut()
        .find(|section| section.kind == "server" && section.name.as_deref() == Some(id))
        .ok_or_else(|| format!("профиль «{id}» не найден"))?;
    section.set_option(key, value);
    Ok(())
}

// ===========================================================================
// Env-file fallback (миграция с openwrt/etc/csqtt.conf до M5)
// ===========================================================================

/// Разбор env-файла CSQTT_KEY="value" (формат openwrt/etc/csqtt.conf).
pub fn parse_env_file(text: &str) -> Vec<(String, String)> {
    let mut vars = Vec::new();
    for raw_line in text.lines() {
        let line = raw_line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let mut value = value.trim();
        if let Some(unquoted) = value
            .strip_prefix('"')
            .and_then(|rest| rest.strip_suffix('"'))
        {
            value = unquoted;
        }
        vars.push((key, value.to_string()));
    }
    vars
}

fn env_value<'a>(vars: &'a [(String, String)], key: &str) -> Option<&'a str> {
    vars.iter()
        .find(|(var_key, _)| var_key == key)
        .map(|(_, value)| value.as_str())
}

/// Конвертация env-file в ProfilePool (один профиль 'migrated';
/// device_id переносится как есть — НЕ менять/не генерировать).
/// UCI приоритет: env используется только пока /etc/config/csqtt нет.
pub fn migrate_env_to_pool(vars: &[(String, String)]) -> ProfilePool {
    let mut profile = ServerProfile {
        section_id: "migrated".to_string(),
        order: 0,
        name: "Migrated (env)".to_string(),
        ..ServerProfile::default()
    };
    profile.peer = env_value(vars, "CSQTT_PEER")
        .unwrap_or_default()
        .to_string();
    profile.password = env_value(vars, "CSQTT_PASSWORD")
        .unwrap_or_default()
        .to_string();
    profile.vk = env_value(vars, "CSQTT_VK").unwrap_or_default().to_string();
    profile.device_id = env_value(vars, "CSQTT_DEVICE_ID")
        .unwrap_or_default()
        .to_string();
    if let Some(fingerprint) =
        env_value(vars, "CSQTT_FINGERPRINT").filter(|value| !value.trim().is_empty())
    {
        profile.fingerprint = fingerprint.to_string();
    }
    if let Some(workers) = env_value(vars, "CSQTT_WORKERS").and_then(|raw| raw.parse().ok()) {
        profile.workers = workers;
    }
    if let Some(obfs) = env_value(vars, "CSQTT_OBFS").filter(|value| !value.trim().is_empty()) {
        profile.obfs = obfs.to_string();
    }
    if let Some(turn_transport) =
        env_value(vars, "CSQTT_TURN_TRANSPORT").filter(|value| !value.trim().is_empty())
    {
        profile.turn_transport = turn_transport.to_string();
    }
    // CSQTT_TUN игнорируется: csqtt0 фиксирован (правило 1).
    // CSQTT_EXTRA_ARGS не переносится: expert-режим CLI.
    ProfilePool {
        main: MainConfig::default(),
        routing: RoutingConfig::default(),
        servers: vec![profile],
    }
}

// ===========================================================================
// Redaction секретов (правило 10 UCI-схемы; тесты обязательны)
// ===========================================================================

pub const REDACTED: &str = "***";

/// Заменить все вхождения секретных значений на `***`.
/// Длинные секреты — первыми (корректная маскировка перекрытий).
pub fn redact_secrets(line: &str, secrets: &[String]) -> String {
    let mut result = line.to_string();
    for secret in secrets {
        if secret.is_empty() || secret.len() < 3 {
            continue;
        }
        if result.contains(secret.as_str()) {
            result = result.replace(secret.as_str(), REDACTED);
        }
    }
    result
}

/// Тест-хелпер: канонический UCI-вывод пула (main → routing → servers
/// в порядке файла) для round-trip проверок.
#[cfg(test)]
impl ProfilePool {
    fn to_uci_for_test(&self) -> UciFile {
        let mut file = UciFile::default();
        let mut main = UciSection {
            kind: "csqtt".to_string(),
            name: Some("main".to_string()),
            options: Vec::new(),
            lists: Vec::new(),
        };
        main.set_option("enabled", if self.main.enabled { "1" } else { "0" });
        main.set_option("selection_mode", self.main.selection_mode.as_str());
        main.set_option("active_profile", &self.main.active_profile);
        main.set_option("failover", if self.main.failover { "1" } else { "0" });
        main.set_option("failback", if self.main.failback { "1" } else { "0" });
        main.set_option("health_interval", &self.main.health_interval.to_string());
        main.set_option("fail_threshold", &self.main.fail_threshold.to_string());
        main.set_option(
            "success_threshold",
            &self.main.success_threshold.to_string(),
        );
        main.set_option("cooldown", &self.main.cooldown.to_string());
        main.set_option("reconnect_delay", &self.main.reconnect_delay.to_string());
        main.set_option(
            "failback_stable_time",
            &self.main.failback_stable_time.to_string(),
        );
        main.set_option("health_mode", self.main.health_mode.as_str());
        main.set_option("health_target", &self.main.health_target);
        main.set_option("captcha_policy", self.main.captcha_policy.as_str());
        main.set_option("tun_address", &self.main.tun_address);
        main.set_option("tun_mtu", &self.main.tun_mtu.to_string());
        main.set_option("dns", &self.main.dns);
        main.set_option("log_level", self.main.log_level.as_str());
        main.set_option("log_file", &self.main.log_file);
        main.set_option("log_size_kb", &self.main.log_size_kb.to_string());
        file.sections.push(main);
        let mut routing = UciSection {
            kind: "csqtt".to_string(),
            name: Some("routing".to_string()),
            options: Vec::new(),
            lists: Vec::new(),
        };
        routing.set_option("mode", self.routing.mode.as_str());
        file.sections.push(routing);
        for profile in &self.servers {
            let mut section = UciSection {
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
            if let Some(value) = profile.fail_threshold {
                section.set_option("fail_threshold", &value.to_string());
            } else {
                section.set_option("fail_threshold", "");
            }
            if let Some(value) = profile.success_threshold {
                section.set_option("success_threshold", &value.to_string());
            } else {
                section.set_option("success_threshold", "");
            }
            if let Some(value) = profile.cooldown {
                section.set_option("cooldown", &value.to_string());
            } else {
                section.set_option("cooldown", "");
            }
            if let Some(policy) = profile.captcha_policy {
                section.set_option("captcha_policy", policy.as_str());
            } else {
                section.set_option("captcha_policy", "");
            }
            section.set_option("note", &profile.note);
            file.sections.push(section);
        }
        file
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(relative: &str) -> String {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(relative);
        std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("fixture {path:?}: {error}"))
    }

    // --- UCI-синтаксис ---

    #[test]
    fn parses_basic_uci_syntax() {
        let text = "\
# комментарий
config csqtt 'main'
	option enabled '0'

config server 'finland'
	option name 'Finland'
	option peer '1.2.3.4:46000'
";
        let file = parse_uci(text).unwrap();
        assert_eq!(file.sections.len(), 2);
        assert_eq!(file.sections[0].kind, "csqtt");
        assert_eq!(file.sections[0].name.as_deref(), Some("main"));
        assert_eq!(file.sections[0].option("enabled"), Some("0"));
        assert_eq!(file.sections[1].option("peer"), Some("1.2.3.4:46000"));
    }

    #[test]
    fn parses_double_quoted_and_unquoted_values() {
        let text = "\
config server 'a'
	option key1 \"double value\"
	option key2 bare
	option key3 ''
";
        let file = parse_uci(text).unwrap();
        assert_eq!(file.sections[0].option("key1"), Some("double value"));
        assert_eq!(file.sections[0].option("key2"), Some("bare"));
        assert_eq!(file.sections[0].option("key3"), Some(""));
    }

    #[test]
    fn quoted_value_with_inner_single_quote_roundtrips() {
        let mut file = parse_uci("config server 'a'\n\toption note 'it's fine'\n").unwrap();
        assert_eq!(file.sections[0].option("note"), Some("it's fine"));
        let rendered = render_uci(&file);
        file = parse_uci(&rendered).unwrap();
        assert_eq!(file.sections[0].option("note"), Some("it's fine"));
    }

    #[test]
    fn render_parse_roundtrip_is_stable() {
        let text = fixture("config/valid/csqtt");
        let first = parse_uci(&text).unwrap();
        let rendered = render_uci(&first);
        let second = parse_uci(&rendered).unwrap();
        assert_eq!(first, second);
        assert_eq!(rendered, render_uci(&second));
    }

    #[test]
    fn option_outside_section_is_syntax_error() {
        let err = parse_uci("option enabled '1'").unwrap_err();
        assert!(err.message.contains("вне секции"));
    }

    #[test]
    fn unknown_directive_is_syntax_error() {
        let err = parse_uci("banana csqtt 'main'").unwrap_err();
        assert!(err.message.contains("неизвестная директива"));
    }

    #[test]
    fn lists_are_preserved() {
        let text = "config server 'a'\n\tlist vk 'h1'\n\tlist vk 'h2'\n";
        let file = parse_uci(text).unwrap();
        assert_eq!(file.sections[0].lists.len(), 2);
        assert_eq!(file.sections[0].lists[0].value, "h1");
    }

    // --- Fixtures: типизация + валидация ---

    #[test]
    fn valid_priority_pool_fixture_passes() {
        let pool = ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap();
        assert_eq!(pool.servers.len(), 2);
        assert!(pool.main.enabled);
        assert_eq!(pool.main.selection_mode, SelectionMode::Priority);
        assert_eq!(pool.routing.mode, RoutingMode::Auto);
        assert_eq!(pool.ordered_enabled_servers().len(), 2);
        assert_eq!(pool.ordered_enabled_servers()[0].section_id, "finland");
        assert_eq!(pool.ordered_enabled_servers()[1].section_id, "backup");
    }

    #[test]
    fn manual_active_fixture_passes() {
        let pool = ProfilePool::from_text(&fixture("config/manual_active/csqtt")).unwrap();
        assert_eq!(pool.main.selection_mode, SelectionMode::Manual);
        assert_eq!(pool.main.active_profile, "finland");
        assert_eq!(pool.selected_profile().unwrap().section_id, "finland");
    }

    #[test]
    fn overrides_fixture_passes_and_effective_values_work() {
        let pool = ProfilePool::from_text(&fixture("config/overrides/csqtt")).unwrap();
        let first = pool.server("first").unwrap();
        let second = pool.server("second").unwrap();
        // Пустые override → global (в fixture global: wait).
        assert_eq!(pool.effective_fail_threshold(first), 3);
        assert_eq!(pool.effective_cooldown(first), 90);
        assert_eq!(pool.effective_captcha_policy(first), CaptchaPolicy::Wait);
        // Заданные override поверх global.
        assert_eq!(pool.effective_fail_threshold(second), 5);
        assert_eq!(pool.effective_success_threshold(second), 1);
        assert_eq!(pool.effective_cooldown(second), 15);
        assert_eq!(
            pool.effective_captcha_policy(second),
            CaptchaPolicy::Failover,
        );
    }

    #[test]
    fn minimal_fixture_passes_with_defaults() {
        let pool = ProfilePool::from_text(&fixture("config/minimal/csqtt")).unwrap();
        assert!(!pool.main.enabled);
        assert_eq!(pool.main.fail_threshold, 3);
        assert_eq!(pool.main.tun_mtu, 1280);
        assert_eq!(pool.main.log_size_kb, 512);
        assert!(pool.servers.is_empty());
        assert_eq!(pool.routing.mode, RoutingMode::Auto);
    }

    #[test]
    fn broken_fixture_reports_specific_fields() {
        let issues = ProfilePool::from_text(&fixture("config/broken/csqtt")).unwrap_err();
        let fields: HashSet<(String, String)> = issues
            .iter()
            .map(|issue| (issue.section.clone(), issue.field.clone()))
            .collect();
        for expected in [
            ("main".to_string(), "enabled".to_string()),
            ("main".to_string(), "selection_mode".to_string()),
            ("main".to_string(), "failover".to_string()),
            ("main".to_string(), "health_interval".to_string()),
            ("main".to_string(), "fail_threshold".to_string()),
            ("main".to_string(), "reconnect_delay".to_string()),
            ("main".to_string(), "captcha_policy".to_string()),
            ("main".to_string(), "tun_mtu".to_string()),
            ("main".to_string(), "dns".to_string()),
            ("main".to_string(), "log_level".to_string()),
            ("main".to_string(), "log_file".to_string()),
            ("main".to_string(), "log_size_kb".to_string()),
            ("main".to_string(), "active_profile".to_string()),
            ("routing".to_string(), "mode".to_string()),
            ("bad1".to_string(), "priority".to_string()),
            ("bad1".to_string(), "peer".to_string()),
            ("bad1".to_string(), "password".to_string()),
            ("bad1".to_string(), "vk".to_string()),
            ("bad1".to_string(), "obfs".to_string()),
            ("bad1".to_string(), "turn_transport".to_string()),
            ("bad1".to_string(), "captcha_mode".to_string()),
            ("bad1".to_string(), "fingerprint".to_string()),
            ("bad1".to_string(), "vk_auth_mode".to_string()),
            ("bad1".to_string(), "section".to_string()),
            ("bad2".to_string(), "workers".to_string()),
        ] {
            assert!(
                fields.contains(&expected),
                "ожидалась ошибка {expected:?}, есть: {issues:#?}"
            );
        }
    }

    #[test]
    fn m3x_routing_modes_only_auto_and_none() {
        // Контракт M3X: routing.mode только auto|none; никаких oif/main_metric.
        assert_eq!(RoutingMode::parse("auto"), Some(RoutingMode::Auto));
        assert_eq!(RoutingMode::parse("none"), Some(RoutingMode::None));
        for rejected in ["oif", "main_metric", "table", "rule", "global"] {
            assert_eq!(RoutingMode::parse(rejected), None);
        }
        // В канонической сериализации нет route/rule-полей.
        let pool = ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap();
        let file = pool.to_uci_for_test();
        let rendered = render_uci(&file);
        for forbidden in ["oif", "main_metric", "table '", "rule '", "ip route"] {
            assert!(
                !rendered.contains(forbidden),
                "в UCI-выводе недопустимо {forbidden:?} (контракт M3X)"
            );
        }
    }

    #[test]
    fn m3x_client_config_is_interface_only() {
        let pool = ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap();
        let profile = pool.server("finland").unwrap();
        let config = pool.client_config_for(profile);
        assert!(!config.install_routes);
        assert!(!config.apply_dns);
        assert_eq!(config.tun_uds, "csqtt0");
    }

    // --- Selection / tie-break ---

    #[test]
    fn priority_tie_break_follows_uci_order() {
        let text = "\
config server 'a'
	option enabled '1'
	option priority '10'
	option peer '1.2.3.4:1'
	option password 'p'
	option vk 'abcdefghijklmnop1'

config server 'b'
	option enabled '1'
	option priority '10'
	option peer '1.2.3.4:2'
	option password 'p'
	option vk 'abcdefghijklmnop1'

config server 'c'
	option enabled '1'
	option priority '5'
	option peer '1.2.3.4:3'
	option password 'p'
	option vk 'abcdefghijklmnop1'

config server 'd'
	option enabled '0'
	option priority '1'
";
        let pool = ProfilePool::from_text(text).unwrap();
        let ordered: Vec<&str> = pool
            .ordered_enabled_servers()
            .iter()
            .map(|profile| profile.section_id.as_str())
            .collect();
        assert_eq!(ordered, vec!["c", "a", "b"]);
    }

    #[test]
    fn selected_profile_respects_mode() {
        let text = "\
config server 'a'
	option enabled '1'
	option priority '10'
	option peer '1.2.3.4:1'
	option password 'p'
	option vk 'abcdefghijklmnop1'

config server 'b'
	option enabled '1'
	option priority '20'
	option peer '1.2.3.4:2'
	option password 'p'
	option vk 'abcdefghijklmnop1'
";
        let pool = ProfilePool::from_text(text).unwrap();
        assert_eq!(
            pool.selected_profile()
                .map(|profile| profile.section_id.as_str()),
            Some("a")
        );
        let mut file = parse_uci(text).unwrap();
        uci_set_active_profile(&mut file, "b").unwrap();
        let pool = ProfilePool::from_uci(&file).unwrap();
        assert_eq!(
            pool.selected_profile()
                .map(|profile| profile.section_id.as_str()),
            Some("b")
        );
    }

    #[test]
    fn manual_active_missing_or_disabled_is_error() {
        let text = "\
config csqtt 'main'
	option selection_mode 'manual'
	option active_profile 'ghost'

config server 'off'
	option enabled '0'
";
        let issues = ProfilePool::from_text(text).unwrap_err();
        assert!(issues.iter().any(
            |issue| issue.field == "active_profile" && issue.message.contains("не существует")
        ));

        let text = "\
config csqtt 'main'
	option selection_mode 'manual'
	option active_profile 'off'

config server 'off'
	option enabled '0'
";
        let issues = ProfilePool::from_text(text).unwrap_err();
        assert!(
            issues
                .iter()
                .any(|issue| issue.field == "active_profile" && issue.message.contains("выключен"))
        );
    }

    // --- workers / enums / диапазоны ---

    #[test]
    fn workers_accept_any_value_in_range() {
        // [M8] Любое 9..=126 принимается; приведение к кратному 9 — в ядре.
        for valid in [9usize, 10, 18, 27, 50, 54, 117, 125, 126] {
            let text = format!(
                "config server 'a'\n\toption enabled '1'\n\toption peer '1.2.3.4:1'\n\toption password 'p'\n\toption vk 'abcdefghijklmnop1'\n\toption workers '{valid}'\n"
            );
            let pool = ProfilePool::from_text(&text).unwrap();
            assert_eq!(pool.servers[0].workers, valid);
        }
        for invalid in [0usize, 1, 8, 127, 135] {
            let text = format!(
                "config server 'a'\n\toption enabled '1'\n\toption peer '1.2.3.4:1'\n\toption password 'p'\n\toption vk 'abcdefghijklmnop1'\n\toption workers '{invalid}'\n"
            );
            let issues = ProfilePool::from_text(&text).unwrap_err();
            assert!(
                issues.iter().any(|issue| issue.field == "workers"),
                "workers={invalid} должен быть отвергнут"
            );
        }
    }

    #[test]
    fn turn_transport_accepts_core_forms() {
        for (raw, expected) in [
            ("udp", "udp"),
            ("tcp", "tcp_tls"),
            ("tcp_tls", "tcp_tls"),
            ("tcp-tls", "tcp_tls"),
        ] {
            let text = format!(
                "config server 'a'\n\toption enabled '0'\n\toption turn_transport '{raw}'\n"
            );
            let pool = ProfilePool::from_text(&text).unwrap();
            assert_eq!(pool.servers[0].turn_transport, expected);
        }
        let text =
            "config server 'a'\n\toption enabled '0'\n\toption turn_transport 'carrier-pigeon'\n";
        assert!(ProfilePool::from_text(text).is_err());
    }

    #[test]
    fn disabled_profile_allows_empty_fields() {
        let text = "config server 'draft'\n\toption enabled '0'\n";
        let pool = ProfilePool::from_text(text).unwrap();
        assert!(!pool.servers[0].enabled);
        assert!(pool.servers[0].peer.is_empty());
    }

    #[test]
    fn auto_js_profile_requires_token_and_hash_mode() {
        let ok = "\
config server 'js'
	option enabled '1'
	option peer '1.2.3.4:1'
	option password 'p'
	option vk_hash_mode 'auto_js'
	option vk_auth_mode 'auto_js'
	option vk_js_token 'tok'
";
        assert!(ProfilePool::from_text(ok).is_ok());
        let no_token = "\
config server 'js'
	option enabled '1'
	option peer '1.2.3.4:1'
	option password 'p'
	option vk_hash_mode 'auto_js'
";
        let issues = ProfilePool::from_text(no_token).unwrap_err();
        assert!(issues.iter().any(|issue| issue.field == "vk_js_token"));
    }

    // --- to_client_config ---

    #[test]
    fn client_config_defaults_for_empty_fields() {
        let pool = ProfilePool::from_text("config server 'a'\n\toption enabled '0'\n").unwrap();
        let config = pool.client_config_for(&pool.servers[0]);
        assert_eq!(config.vk_hash_mode, "manual");
        assert_eq!(config.vk_auth_mode, "vkcalls");
        assert_eq!(config.captcha_mode, "auto");
        assert_eq!(config.fingerprint, "chrome");
        assert_eq!(config.obfs, "audio");
        assert_eq!(config.turn_transport, "udp");
        assert_eq!(config.device_id, "openwrt");
        assert_eq!(config.workers, 18);
    }

    #[test]
    fn client_config_keeps_existing_device_id_verbatim() {
        let pool = ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap();
        let profile = pool.server("finland").unwrap();
        let config = pool.client_config_for(profile);
        assert_eq!(config.device_id, "11111111-2222-3333-4444-555555555555");
    }

    // --- Env-file fallback ---

    #[test]
    fn env_file_is_parsed_and_migrated() {
        let vars = parse_env_file(&fixture("env/csqtt.conf"));
        assert_eq!(env_value(&vars, "CSQTT_PEER"), Some("198.51.100.10:46000"));
        assert_eq!(
            env_value(&vars, "CSQTT_DEVICE_ID"),
            Some("11111111-2222-3333-4444-555555555555")
        );
        let pool = migrate_env_to_pool(&vars);
        assert_eq!(pool.servers.len(), 1);
        let profile = &pool.servers[0];
        assert_eq!(profile.section_id, "migrated");
        assert_eq!(profile.peer, "198.51.100.10:46000");
        assert_eq!(profile.password, "examplePassword1");
        assert_eq!(profile.device_id, "11111111-2222-3333-4444-555555555555");
        assert_eq!(profile.workers, 18);
        assert_eq!(profile.obfs, "audio");
        assert_eq!(profile.turn_transport, "udp");
        // Валидна как конфиг (enabled-профиль заполнен).
        let issues_text = render_pool_for_validation(&pool);
        assert!(ProfilePool::from_text(&issues_text).is_ok());
    }

    fn render_pool_for_validation(pool: &ProfilePool) -> String {
        let file = pool.to_uci_for_test();
        render_uci(&file)
    }

    // --- Мутации profile-команд (контракт M4c) ---

    #[test]
    fn uci_mutations_use_auto_enable_disable_remove() {
        let text = "\
config csqtt 'main'
	option selection_mode 'priority'

config server 'a'
	option enabled '1'
	option peer '1.2.3.4:1'
	option password 'p'
	option vk 'abcdefghijklmnop1'
";
        let mut file = parse_uci(text).unwrap();
        // use
        uci_set_active_profile(&mut file, "a").unwrap();
        assert!(uci_set_active_profile(&mut file, "ghost").is_err());
        let pool = ProfilePool::from_uci(&file).unwrap();
        assert_eq!(pool.main.selection_mode, SelectionMode::Manual);
        assert_eq!(pool.main.active_profile, "a");
        // auto
        uci_set_selection_auto(&mut file);
        let pool = ProfilePool::from_uci(&file).unwrap();
        assert_eq!(pool.main.selection_mode, SelectionMode::Priority);
        assert!(pool.main.active_profile.is_empty());
        // disable/enable
        uci_set_server_enabled(&mut file, "a", false).unwrap();
        let pool = ProfilePool::from_uci(&file).unwrap();
        assert!(!pool.server("a").unwrap().enabled);
        uci_set_server_enabled(&mut file, "a", true).unwrap();
        // remove сбрасывает active_profile
        uci_set_active_profile(&mut file, "a").unwrap();
        uci_remove_server(&mut file, "a").unwrap();
        let pool = ProfilePool::from_uci(&file).unwrap();
        assert!(pool.main.active_profile.is_empty());
        assert!(pool.servers.is_empty());
    }

    // --- Secrets redaction ---

    #[test]
    fn collect_secrets_gathers_passwords_and_tokens() {
        let pool = ProfilePool::from_text(&fixture("config/valid/csqtt")).unwrap();
        let secrets = pool.collect_secrets();
        assert!(secrets.contains(&"examplePassword1".to_string()));
        assert!(secrets.contains(&"backup-secret-password".to_string()));
        // Пустые не попадают (черновик без пароля/токена — валиден).
        let pool = ProfilePool::from_text("config server 'a'\n\toption enabled '0'\n").unwrap();
        assert!(pool.collect_secrets().is_empty());
    }

    #[test]
    fn redact_secrets_masks_all_occurrences() {
        let secrets = vec![
            "examplePassword1".to_string(),
            "verylongvkjstoken123".to_string(),
        ];
        let line =
            "connect password=examplePassword1 token verylongvkjstoken123 again examplePassword1";
        let redacted = redact_secrets(line, &secrets);
        assert!(!redacted.contains("examplePassword1"));
        assert!(!redacted.contains("verylongvkjstoken123"));
        assert_eq!(redacted.matches(REDACTED).count(), 3);
    }

    #[test]
    fn redact_secrets_skips_short_and_empty() {
        let line = "ab cd";
        assert_eq!(redact_secrets(line, &["ab".to_string()]), "ab cd");
        assert_eq!(redact_secrets(line, &[String::new()]), "ab cd");
    }
}
