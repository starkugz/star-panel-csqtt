// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [M4a] Разбор и кодирование csqtt://-ссылок.
//!
//! Семантика — точный перенос Android-парсера
//! `csqtt-main/app/src/main/java/com/csqtt/client/ui/utils/UiUtils.kt`
//! (`parseCsqttLink` / `parseCsqttV2` / `parseRawQuery` / `parseLinkHashes` /
//! `stripVkUrlStatic` / `decodeQueryComponent`), оракулы —
//! `CsqttLinkTest.kt`. Кодировщик — перенос `buildCsqttLink` из
//! `csqtt-main/rust-server/web_panel.rs` (`encodeURIComponent`-семантика JS).
//!
//! Поддерживаются обе реальные формы:
//! - current v2: `csqtt://connect?v=2&host=<host>&peer=<port>&password=<pass>[&hashes=h1+h2]`
//!   (в т.ч. «склеенные» параметры без `&` — regex-fallback UiUtils.kt);
//! - legacy: `csqtt://<password>@<host>:<port>`.
//!
//! Отличие от Android: вместо Kotlin `null` для всех ошибок промпт M4a
//! требует explicit error — `LinkError` различает неизвестную версию
//! (`UnsupportedVersion`), мусор (`Malformed`) и конкретные причины;
//! итоговое множество валидных ссылок совпадает с upstream.

/// Минимальная длина VK-хеша в ссылке (UiUtils.kt `parseLinkHashes`).
const MIN_HASH_CHARS: usize = 16;
/// Максимум хешей в ссылке (UiUtils.kt `parseLinkHashes`: 1..6).
const MAX_LINK_HASHES: usize = 6;

/// Известные ключи query v2-ссылки (UiUtils.kt `parseRawQuery` regex).
const QUERY_KEYS: [&str; 5] = ["v", "host", "peer", "password", "hashes"];

/// Префиксы VK call-ссылок (UiUtils.kt `stripVkUrlStatic`), срезаются
/// с хеша при импорте из ссылки.
const VK_CALL_PREFIXES: [&str; 12] = [
    "https://vk.com/call/join/",
    "http://vk.com/call/join/",
    "https://m.vk.com/call/join/",
    "http://m.vk.com/call/join/",
    "m.vk.com/call/join/",
    "vk.com/call/join/",
    "https://vk.ru/call/join/",
    "http://vk.ru/call/join/",
    "https://m.vk.ru/call/join/",
    "http://m.vk.ru/call/join/",
    "m.vk.ru/call/join/",
    "vk.ru/call/join/",
];

/// Разобранная csqtt://-ссылка (аналог Kotlin `CsqttLink`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CsqttLink {
    pub host: String,
    pub port: u16,
    pub password: String,
    pub hashes: Vec<String>,
}

impl CsqttLink {
    /// Адрес пира `host:port`; IPv6-хост берётся в скобки
    /// (Kotlin `peerAddress()`).
    pub fn peer_address(&self) -> String {
        if self.host.contains(':') && !self.host.starts_with('[') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

/// Причина отказа разбора ссылки. `Display` — человекочитаемое сообщение
/// для CLI (M4c) и preview импорта.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkError {
    /// Строка не начинается с `csqtt://`.
    NotCsqttLink,
    /// Строка вообще не парсится как URI (вкл. незакодированные пробелы).
    InvalidUri(String),
    /// Фрагмент `#...` запрещён (UiUtils: `rawFragment != null → null`).
    FragmentNotAllowed,
    /// В v2-форме не допускаются userinfo/port/path в authority.
    UnexpectedAuthority,
    /// v2 без query (`rawQuery == null`).
    MissingQuery,
    /// Нет обязательного параметра query (v/host/peer/password).
    MissingParameter(&'static str),
    /// Значение параметра не декодируется (битый %XX / не UTF-8).
    DecodeError(&'static str),
    /// Параметр `v` присутствует, но не равен «2» — explicit error
    /// по промпту M4a (unknown version), не silent parse.
    UnsupportedVersion(String),
    /// Значение пустое или из одних пробелов.
    BlankValue(&'static str),
    /// Значение содержит пробелы внутри.
    WhitespaceInValue(&'static str),
    /// `peer` вне 1..=65535.
    BadPort,
    /// `hashes` невалиден (пустой элемент, <16 символов, пробелы,
    /// дубликаты, больше 6).
    InvalidHashes(String),
    /// Legacy-ссылка без password/userinfo.
    LegacyMissingCredentials,
    /// Legacy-ссылка без порта или с невалидным портом.
    LegacyBadPort,
}

impl std::fmt::Display for LinkError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotCsqttLink => {
                write!(f, "не csqtt://-ссылка (ожидается префикс csqtt://)")
            }
            Self::InvalidUri(reason) => write!(f, "ссылка не разбирается как URI: {reason}"),
            Self::FragmentNotAllowed => write!(f, "фрагмент (#...) в ссылке не допускается"),
            Self::UnexpectedAuthority => write!(
                f,
                "в форме v2 (host=connect) не допускаются userinfo, порт или путь"
            ),
            Self::MissingQuery => write!(f, "в форме v2 (host=connect) нет строки параметров"),
            Self::MissingParameter(name) => {
                write!(f, "в ссылке нет обязательного параметра «{name}»")
            }
            Self::DecodeError(name) => {
                write!(f, "параметр «{name}» содержит некорректное %-кодирование")
            }
            Self::UnsupportedVersion(value) => write!(
                f,
                "неподдерживаемая версия ссылки: v={value:?} (поддерживается только v=2)"
            ),
            Self::BlankValue(name) => write!(f, "параметр «{name}» пуст"),
            Self::WhitespaceInValue(name) => {
                write!(f, "параметр «{name}» содержит пробельные символы")
            }
            Self::BadPort => write!(f, "параметр «peer» вне диапазона 1..=65535"),
            Self::InvalidHashes(reason) => write!(f, "параметр «hashes» невалиден: {reason}"),
            Self::LegacyMissingCredentials => {
                write!(f, "в legacy-ссылке нет пароля (password@host:port)")
            }
            Self::LegacyBadPort => write!(f, "в legacy-ссылке нет валидного порта 1..=65535"),
        }
    }
}

/// Разобранная authority-часть URI (между `csqtt://` и `?`/`/`).
struct AuthorityParts<'a> {
    /// Вся authority до path (для строгой v2-проверки).
    auth: &'a str,
    userinfo: Option<&'a str>,
    host: &'a str,
    /// Сырая порт-строка (пустая — порт не задан).
    port_raw: &'a str,
    /// Часть после authority до `?` (пустая или `/...`).
    path: &'a str,
    /// Сырая query-часть (без `?`), None — вопросительного знака не было.
    query: Option<&'a str>,
}

fn split_uri(trimmed: &str) -> Result<AuthorityParts<'_>, LinkError> {
    // Java URI запрещает незакодированные пробелы: глобальный reject.
    if trimmed.chars().any(char::is_whitespace) {
        return Err(LinkError::InvalidUri("пробельные символы".to_string()));
    }
    let rest = &trimmed["csqtt://".len()..];
    // UiUtils: rawFragment != null → null. Сырой `#` — всегда фрагмент;
    // пароль с `#` обязан идти как %23 (семантика java.net.URI).
    let before_fragment = match rest.split_once('#') {
        Some(_) => return Err(LinkError::FragmentNotAllowed),
        None => rest,
    };
    let (before_query, query) = match before_fragment.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (before_fragment, None),
    };
    let auth_end = before_query.find('/').unwrap_or(before_query.len());
    let auth = &before_query[..auth_end];
    let path = &before_query[auth_end..];
    let (userinfo, hostport) = match auth.split_once('@') {
        Some((userinfo, hostport)) => (Some(userinfo), hostport),
        None => (None, auth),
    };
    let (host, port_raw) = if let Some(bracketed) = hostport.strip_prefix('[') {
        let (host, tail) = bracketed
            .split_once(']')
            .ok_or_else(|| LinkError::InvalidUri("незакрытая IPv6-скобка".to_string()))?;
        let port_raw = tail.strip_prefix(':').unwrap_or("");
        (host, port_raw)
    } else {
        match hostport.split_once(':') {
            Some((host, port_raw)) => (host, port_raw),
            None => (hostport, ""),
        }
    };
    Ok(AuthorityParts {
        auth,
        userinfo,
        host,
        port_raw,
        path,
        query,
    })
}

/// Разбор csqtt://-ссылки (v2 + legacy). Возвращает `Ok` ровно там,
/// где Android `parseCsqttLink` возвращает non-null.
pub fn parse_csqtt_link(raw: &str) -> Result<CsqttLink, LinkError> {
    let trimmed = raw.trim();
    if !trimmed.to_ascii_lowercase().starts_with("csqtt://") {
        return Err(LinkError::NotCsqttLink);
    }
    let parts = split_uri(trimmed)?;
    if parts.host.eq_ignore_ascii_case("connect") {
        return parse_v2(&parts);
    }
    parse_legacy(&parts)
}

fn parse_v2(parts: &AuthorityParts<'_>) -> Result<CsqttLink, LinkError> {
    // UiUtils.parseCsqttV2: rawUserInfo/port/rawPath обязаны отсутствовать.
    if parts.userinfo.is_some()
        || !parts.port_raw.is_empty()
        || !parts.path.is_empty()
        || !parts.auth.eq_ignore_ascii_case("connect")
    {
        return Err(LinkError::UnexpectedAuthority);
    }
    let raw_query = parts.query.ok_or(LinkError::MissingQuery)?;
    let pairs = parse_raw_query(raw_query).ok_or(LinkError::MissingQuery)?;
    let host = required_value(&pairs, "host")?;
    let port_raw = required_value(&pairs, "peer")?;
    let password = required_value(&pairs, "password")?;
    let version = required_value(&pairs, "v")?;
    if version != "2" {
        return Err(LinkError::UnsupportedVersion(version));
    }
    if host.chars().any(char::is_whitespace) {
        return Err(LinkError::WhitespaceInValue("host"));
    }
    if password.chars().any(char::is_whitespace) {
        return Err(LinkError::WhitespaceInValue("password"));
    }
    let port: u16 = port_raw
        .parse()
        .ok()
        .filter(|port| (1..=65535).contains(port))
        .ok_or(LinkError::BadPort)?;
    let hashes = match lookup(&pairs, "hashes") {
        Some(raw) => parse_link_hashes(raw).map_err(LinkError::InvalidHashes)?,
        None => Vec::new(),
    };
    Ok(CsqttLink {
        host: host.to_string(),
        port,
        password: password.to_string(),
        hashes,
    })
}

fn parse_legacy(parts: &AuthorityParts<'_>) -> Result<CsqttLink, LinkError> {
    let password = parts
        .userinfo
        .filter(|userinfo| !userinfo.trim().is_empty())
        .ok_or(LinkError::LegacyMissingCredentials)?;
    if parts.host.trim().is_empty() {
        return Err(LinkError::LegacyMissingCredentials);
    }
    let port: u16 = parts
        .port_raw
        .parse()
        .ok()
        .filter(|port| (1..=65535).contains(port))
        .ok_or(LinkError::LegacyBadPort)?;
    Ok(CsqttLink {
        host: parts.host.to_string(),
        port,
        password: password.to_string(),
        hashes: Vec::new(),
    })
}

/// Извлечь и декодировать обязательный параметр v2.
fn required_value(pairs: &[(String, String)], key: &'static str) -> Result<String, LinkError> {
    let raw = lookup(pairs, key).ok_or(LinkError::MissingParameter(key))?;
    let decoded = decode_query_component(raw).ok_or(LinkError::DecodeError(key))?;
    if decoded.trim().is_empty() {
        return Err(LinkError::BlankValue(key));
    }
    Ok(decoded)
}

/// Последнее вхождение ключа выигрывает (map-семантика UiUtils).
fn lookup<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .rev()
        .find(|(pair_key, _)| pair_key == key)
        .map(|(_, value)| value.as_str())
}

fn has_required_keys(pairs: &[(String, String)]) -> bool {
    ["v", "host", "peer", "password"]
        .iter()
        .all(|key| pairs.iter().any(|(pair_key, _)| pair_key == key))
}

/// Перенос UiUtils.parseRawQuery: split-режим по `&`/`;` с fallback на
/// «склеенные» параметры (regex UiUtils.kt:66). Ключи нормализуются к
/// нижнему регистру (в upstream это делает regex-ветка; split-ветка
/// чувствительна к регистру, но неполный набор всегда уходит в regex —
/// итоговое множество валидных ссылок совпадает).
fn parse_raw_query(raw_query: &str) -> Option<Vec<(String, String)>> {
    if raw_query.is_empty() {
        return None;
    }
    let sanitized = raw_query.replace("&amp;", "&");
    let mut split_pairs: Vec<(String, String)> = Vec::new();
    if sanitized.contains('&') || sanitized.contains(';') {
        for part in sanitized.split(['&', ';']) {
            if part.trim().is_empty() {
                continue;
            }
            let Some(separator) = part.find('=') else {
                continue;
            };
            if separator == 0 {
                continue;
            }
            let Some(key) = decode_query_component(&part[..separator]) else {
                continue;
            };
            if key.trim().is_empty() {
                continue;
            }
            split_pairs.push((key.to_ascii_lowercase(), part[separator + 1..].to_string()));
        }
        if has_required_keys(&split_pairs) {
            return Some(split_pairs);
        }
    }
    let scanned = scan_known_keys(&sanitized);
    if has_required_keys(&scanned) {
        return Some(scanned);
    }
    // UiUtils: финальный fallback — вернуть неполный split-результат.
    (!split_pairs.is_empty()).then_some(split_pairs)
}

/// Ручная замена regex UiUtils.kt:66
/// `(?:^|[&;]|(?<=[a-zA-Z0-9_]))(v|host|peer|password|hashes)=([^&?]*?)(?=(?:v|host|peer|password|hashes)=|$)`
/// (IGNORE_CASE; regex-lite не поддерживает look-around, поэтому сканер).
/// Значение обрезается на первом `&`/`?` (класс `[^&?]*?`) или на старте
/// следующего известного ключа с корректной границей.
fn scan_known_keys(sanitized: &str) -> Vec<(String, String)> {
    let bytes = sanitized.as_bytes();
    // to_ascii_lowercase байт-в-байт сохраняет длины и позиции.
    let lower = sanitized.to_ascii_lowercase();
    let lower_bytes = lower.as_bytes();
    // (ключ_старт, старт_значения, длина_ключа)
    let mut hits: Vec<(usize, usize, usize)> = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(key_len) = match_query_key_at(lower_bytes, index) {
            let boundary_ok = index == 0
                || matches!(bytes[index - 1], b'&' | b';')
                || bytes[index - 1].is_ascii_alphanumeric()
                || bytes[index - 1] == b'_';
            if boundary_ok {
                let value_start = index + key_len + 1;
                hits.push((index, value_start, key_len));
                index = value_start;
                continue;
            }
        }
        index += 1;
    }
    let mut pairs = Vec::with_capacity(hits.len());
    for (position, &(start, value_start, key_len)) in hits.iter().enumerate() {
        let next_hit_start = hits.get(position + 1).map_or(bytes.len(), |hit| hit.0);
        let special_cut = sanitized[value_start..]
            .find(['&', '?'])
            .map_or(bytes.len(), |offset| value_start + offset);
        let value_end = special_cut.min(next_hit_start).max(value_start);
        let key = &sanitized[start..start + key_len];
        pairs.push((
            key.to_ascii_lowercase(),
            sanitized[value_start..value_end].to_string(),
        ));
    }
    pairs
}

fn match_query_key_at(lower: &[u8], position: usize) -> Option<usize> {
    for key in QUERY_KEYS {
        let key_bytes = key.as_bytes();
        let after = position + key_bytes.len() + 1;
        if after <= lower.len()
            && lower[after - 1] == b'='
            && lower[position..after - 1].eq_ignore_ascii_case(key_bytes)
        {
            return Some(key_bytes.len());
        }
    }
    None
}

/// Перенос UiUtils.decodeQueryComponent: `+` остаётся литералом
/// (Kotlin заранее заменяет `+` на `%2B`), `%XX` декодируется, битые
/// последовательности и не-UTF-8 — ошибка (Kotlin `runCatching … null`).
fn decode_query_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex_value(*bytes.get(index + 1)?)?;
            let low = hex_value(*bytes.get(index + 2)?)?;
            decoded.push(high * 16 + low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Перенос UiUtils.parseLinkHashes: split по `+` (до декодирования),
/// 1..=6 частей, каждая непустая, после декодирования+stripVkUrlStatic —
/// ≥16 символов, без пробелов, без дубликатов. `distinct()` в Kotlin
/// сохраняет порядок первого вхождения — сортировка недопустима.
fn parse_link_hashes(raw_hashes: &str) -> Result<Vec<String>, String> {
    let parts: Vec<&str> = raw_hashes.split('+').collect();
    if parts.len() > MAX_LINK_HASHES {
        return Err(format!("хешей больше {MAX_LINK_HASHES}"));
    }
    if parts.iter().any(|part| part.is_empty()) {
        return Err("пустой элемент в списке хешей".to_string());
    }
    let hashes: Vec<String> = parts
        .iter()
        .map(|encoded| {
            decode_query_component(encoded)
                .map(|decoded| strip_vk_url_static(&decoded))
                .unwrap_or_default()
        })
        .collect();
    if hashes.is_empty() {
        return Err("список хешей пуст".to_string());
    }
    if hashes
        .iter()
        .any(|hash| hash.chars().count() < MIN_HASH_CHARS)
    {
        return Err(format!("хеш короче {MIN_HASH_CHARS} символов"));
    }
    if hashes
        .iter()
        .any(|hash| hash.chars().any(char::is_whitespace))
    {
        return Err("хеш содержит пробельные символы".to_string());
    }
    let unique_count = {
        let mut unique = hashes.clone();
        unique.sort();
        unique.dedup();
        unique.len()
    };
    if unique_count != hashes.len() {
        return Err("дубликаты хешей".to_string());
    }
    Ok(hashes)
}

/// Перенос UiUtils.stripVkUrlStatic: срезает префикс VK call-страницы
/// (регистронезависимо), затем всё после первого `?`/`#`, затем хвостовые `/`.
fn strip_vk_url_static(input: &str) -> String {
    let mut value = input.trim();
    let lower = value.to_ascii_lowercase();
    for prefix in VK_CALL_PREFIXES {
        if lower.starts_with(prefix) {
            value = &value[prefix.len()..];
            break;
        }
    }
    let cut = value.find(['?', '#']).unwrap_or(value.len());
    value[..cut].trim_end_matches('/').to_string()
}

/// JS `encodeURIComponent`: не кодируются `A-Za-z0-9 - _ . ! ~ * ' ( )`,
/// остальные байты UTF-8 — `%XX` (верхний регистр).
fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let keep = byte.is_ascii_alphanumeric()
            || matches!(
                byte,
                b'-' | b'_' | b'.' | b'!' | b'~' | b'*' | b'\'' | b'(' | b')'
            );
        if keep {
            encoded.push(*byte as char);
        } else {
            encoded.push('%');
            encoded.push_str(&format!("{byte:02X}"));
        }
    }
    encoded
}

/// Кодирование csqtt://-ссылки (перенос `buildCsqttLink` из
/// web_panel.rs): v2-форма; `hashes` опускаются при пустом списке.
pub fn build_csqtt_link(host: &str, port: u16, password: &str, hashes: &[String]) -> String {
    let mut url = format!(
        "csqtt://connect?v=2&host={}&peer={}&password={}",
        encode_uri_component(host),
        encode_uri_component(&port.to_string()),
        encode_uri_component(password),
    );
    if !hashes.is_empty() {
        let joined = hashes
            .iter()
            .map(|hash| encode_uri_component(hash))
            .collect::<Vec<_>>()
            .join("+");
        url.push_str("&hashes=");
        url.push_str(&joined);
    }
    url
}

/// Разбор `host:port` (или `[v6]:port`) без обращения к сети.
/// Используется UCI-валидацией и экспортом профиля.
pub fn parse_peer(peer: &str) -> Option<(String, u16)> {
    let peer = peer.trim();
    if peer.is_empty() || peer.chars().any(char::is_whitespace) {
        return None;
    }
    if let Some(bracketed) = peer.strip_prefix('[') {
        let (host, tail) = bracketed.split_once(']')?;
        if host.is_empty() {
            return None;
        }
        let port_raw = tail.strip_prefix(':')?;
        let port: u16 = port_raw.parse().ok()?;
        return (port > 0).then(|| (host.to_string(), port));
    }
    let mut split = peer.split(':');
    let host = split.next()?;
    if host.is_empty() {
        return None;
    }
    let port_raw = split.next()?;
    if split.next().is_some() {
        // Голый IPv6 без скобок невалиден (как и в java.net.URI).
        return None;
    }
    let port: u16 = port_raw.parse().ok()?;
    (port > 0).then(|| (host.to_string(), port))
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Оракулы CsqttLinkTest.kt (UiUtils.kt) ---

    #[test]
    fn parses_v2_without_hashes() {
        let link =
            parse_csqtt_link("csqtt://connect?v=2&host=203.0.113.7&peer=46000&password=p%40ss")
                .unwrap();
        assert_eq!(link.host, "203.0.113.7");
        assert_eq!(link.port, 46000);
        assert_eq!(link.password, "p@ss");
        assert!(link.hashes.is_empty());
    }

    #[test]
    fn parses_v2_concatenated_parameters() {
        let link = parse_csqtt_link(
            "csqtt://connect?v=2host=203.0.113.88peer=46000password=dummy_secret_pass",
        )
        .unwrap();
        assert_eq!(link.host, "203.0.113.88");
        assert_eq!(link.port, 46000);
        assert_eq!(link.password, "dummy_secret_pass");
        assert!(link.hashes.is_empty());
    }

    #[test]
    fn parses_v2_with_six_plus_separated_hashes() {
        let hashes: Vec<String> = (1..=6)
            .map(|index| format!("abcdefghijklmnop{index}"))
            .collect();
        let raw = format!(
            "csqtt://connect?v=2&host=vps.example&peer=46000&password=secret&hashes={}",
            hashes.join("+"),
        );
        let link = parse_csqtt_link(&raw).unwrap();
        assert_eq!(link.hashes, hashes);
    }

    #[test]
    fn preserves_encoded_plus_inside_hash() {
        let link = parse_csqtt_link(
            "csqtt://connect?v=2&host=vps.example&peer=46000&password=secret&hashes=abcdefghijklmno%2Bp",
        )
        .unwrap();
        assert_eq!(link.hashes, vec!["abcdefghijklmno+p".to_string()]);
    }

    #[test]
    fn retains_legacy_links() {
        let link = parse_csqtt_link("csqtt://secret@203.0.113.7:46000").unwrap();
        assert_eq!(link.peer_address(), "203.0.113.7:46000");
        assert_eq!(link.password, "secret");
        assert!(link.hashes.is_empty());
    }

    #[test]
    fn formats_ipv6_peer_with_brackets() {
        let link = parse_csqtt_link(
            "csqtt://connect?v=2&host=2001%3Adb8%3A%3A1&peer=46000&password=secret",
        )
        .unwrap();
        assert_eq!(link.peer_address(), "[2001:db8::1]:46000");
    }

    #[test]
    fn rejects_malformed_v2_links() {
        // v=3: unknown version — explicit error по промпту M4a.
        assert_eq!(
            parse_csqtt_link("csqtt://connect?v=3&host=1.2.3.4&peer=46000&password=secret"),
            Err(LinkError::UnsupportedVersion("3".to_string()))
        );
        assert_eq!(
            parse_csqtt_link("csqtt://connect?v=2&host=1.2.3.4&peer=0&password=secret"),
            Err(LinkError::BadPort)
        );
        assert!(
            parse_csqtt_link("csqtt://connect?v=2&host=1.2.3.4&peer=46000&password=secret&hashes=")
                .is_err()
        );
        assert!(
            parse_csqtt_link(
                "csqtt://connect?v=2&host=1.2.3.4&peer=46000&password=secret&hashes=short"
            )
            .is_err()
        );
        let seven_hashes = (1..=7)
            .map(|index| format!("abcdefghijklmnop{index}"))
            .collect::<Vec<_>>()
            .join("+");
        assert!(
            parse_csqtt_link(&format!(
                "csqtt://connect?v=2&host=1.2.3.4&peer=46000&password=secret&hashes={seven_hashes}"
            ))
            .is_err()
        );
    }

    // --- Дополнительные кейсы семантики UiUtils.kt ---

    #[test]
    fn v2_requires_query() {
        assert_eq!(
            parse_csqtt_link("csqtt://connect"),
            Err(LinkError::MissingQuery)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://connect?"),
            Err(LinkError::MissingQuery)
        );
    }

    #[test]
    fn v2_rejects_userinfo_port_and_path() {
        assert_eq!(
            parse_csqtt_link("csqtt://user@connect?v=2&host=h&peer=1&password=p"),
            Err(LinkError::UnexpectedAuthority)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://connect:99?v=2&host=h&peer=1&password=p"),
            Err(LinkError::UnexpectedAuthority)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://connect/x?v=2&host=h&peer=1&password=p"),
            Err(LinkError::UnexpectedAuthority)
        );
        // Kotlin: host=connect → parseCsqttV2 → rawUserInfo != null → null.
        assert_eq!(
            parse_csqtt_link("csqtt://secret@connect:46000"),
            Err(LinkError::UnexpectedAuthority)
        );
    }

    #[test]
    fn fragment_is_rejected() {
        let err = parse_csqtt_link("csqtt://connect?v=2&host=h&peer=1&password=p#fragment");
        assert_eq!(err, Err(LinkError::FragmentNotAllowed));
        assert!(parse_csqtt_link("csqtt://secret@h:1#x").is_err());
    }

    #[test]
    fn scheme_prefix_is_case_insensitive() {
        let link =
            parse_csqtt_link("CSQTT://connect?v=2&host=203.0.113.7&peer=46000&password=secret");
        assert!(link.is_ok());
    }

    #[test]
    fn not_a_link_prefix_is_explicit_error() {
        assert_eq!(
            parse_csqtt_link("https://connect?v=2"),
            Err(LinkError::NotCsqttLink)
        );
        assert_eq!(parse_csqtt_link(""), Err(LinkError::NotCsqttLink));
    }

    #[test]
    fn raw_whitespace_is_rejected_like_java_uri() {
        assert!(matches!(
            parse_csqtt_link("csqtt://connect?v=2&host=bad host&peer=1&password=p"),
            Err(LinkError::InvalidUri(_))
        ));
    }

    #[test]
    fn amp_entity_is_sanitized() {
        let link = parse_csqtt_link(
            "csqtt://connect?v=2&amp;host=example.net&amp;peer=46000&amp;password=secret",
        )
        .unwrap();
        assert_eq!(link.host, "example.net");
        assert_eq!(link.port, 46000);
    }

    #[test]
    fn vk_call_url_prefix_is_stripped_from_hash() {
        let link = parse_csqtt_link(
            "csqtt://connect?v=2&host=h&peer=1&password=p&hashes=https%3A%2F%2Fvk.com%2Fcall%2Fjoin%2Fabcdefghijklmnop1%3Fx%3D1",
        )
        .unwrap();
        assert_eq!(link.hashes, vec!["abcdefghijklmnop1".to_string()]);
    }

    #[test]
    fn hash_with_whitespace_is_rejected() {
        let err = parse_csqtt_link(
            "csqtt://connect?v=2&host=h&peer=1&password=p&hashes=abcdefghij%20klmnop",
        );
        assert!(matches!(err, Err(LinkError::InvalidHashes(_))));
    }

    #[test]
    fn duplicate_hashes_are_rejected() {
        let err = parse_csqtt_link(
            "csqtt://connect?v=2&host=h&peer=1&password=p&hashes=abcdefghijklmnop1+abcdefghijklmnop1",
        );
        assert!(matches!(err, Err(LinkError::InvalidHashes(_))));
    }

    #[test]
    fn broken_percent_encoding_is_rejected() {
        let err = parse_csqtt_link("csqtt://connect?v=2&host=h%2&peer=1&password=p");
        assert!(matches!(err, Err(LinkError::DecodeError("host"))));
    }

    #[test]
    fn legacy_ipv6_in_brackets_is_parsed() {
        let link = parse_csqtt_link("csqtt://secret@[2001:db8::1]:46000").unwrap();
        assert_eq!(link.host, "2001:db8::1");
        assert_eq!(link.port, 46000);
        assert_eq!(link.password, "secret");
        assert_eq!(link.peer_address(), "[2001:db8::1]:46000");
    }

    #[test]
    fn legacy_requires_password_host_and_port() {
        assert_eq!(
            parse_csqtt_link("csqtt://203.0.113.7:46000"),
            Err(LinkError::LegacyMissingCredentials)
        );
        assert_eq!(
            parse_csqtt_link("csqtt:// @203.0.113.7:46000"),
            Err(LinkError::InvalidUri("пробельные символы".to_string()))
        );
        assert_eq!(
            parse_csqtt_link("csqtt://secret@203.0.113.7"),
            Err(LinkError::LegacyBadPort)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://secret@203.0.113.7:0"),
            Err(LinkError::LegacyBadPort)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://secret@203.0.113.7:65536"),
            Err(LinkError::LegacyBadPort)
        );
        assert_eq!(
            parse_csqtt_link("csqtt://secret@:46000"),
            Err(LinkError::LegacyMissingCredentials)
        );
    }

    // --- Кодировщик (web_panel.rs buildCsqttLink) ---

    #[test]
    fn encode_omits_empty_hashes() {
        let url = build_csqtt_link("198.51.100.10", 46000, "secret", &[]);
        assert_eq!(
            url,
            "csqtt://connect?v=2&host=198.51.100.10&peer=46000&password=secret"
        );
    }

    #[test]
    fn encode_escapes_like_js_encode_uri_component() {
        assert_eq!(encode_uri_component("a+b c:d/e"), "a%2Bb%20c%3Ad%2Fe");
        assert_eq!(encode_uri_component("-_.!~*'()"), "-_.!~*'()");
        assert_eq!(encode_uri_component("2001:db8::1"), "2001%3Adb8%3A%3A1");
    }

    #[test]
    fn encode_parse_roundtrip_current_form() {
        let hashes: Vec<String> = [
            "vOBHrOBk_KTZyF_QC_q8QzIRlaLc5y-KCvzoRX9Bt4GQ",
            "ExampleHashOne00000000000000000000000000",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        let url = build_csqtt_link("198.51.100.10", 46000, "examplePassword1", &hashes);
        let parsed = parse_csqtt_link(&url).unwrap();
        assert_eq!(parsed.host, "198.51.100.10");
        assert_eq!(parsed.port, 46000);
        assert_eq!(parsed.password, "examplePassword1");
        assert_eq!(parsed.hashes, hashes);
    }

    #[test]
    fn encode_parse_roundtrip_plus_inside_hash() {
        let hashes = vec!["abcdefghijklmno+p".to_string()];
        let url = build_csqtt_link("h", 1, "p", &hashes);
        let parsed = parse_csqtt_link(&url).unwrap();
        assert_eq!(parsed.hashes, hashes);
    }

    #[test]
    fn encode_parse_roundtrip_ipv6_and_special_password() {
        // Пробелы в password недопустимы парсером upstream
        // (WhitespaceInValue) — как и в Android; roundtrip без них.
        let url = build_csqtt_link("2001:db8::1", 46000, "p@ss&w0rd", &[]);
        let parsed = parse_csqtt_link(&url).unwrap();
        assert_eq!(parsed.host, "2001:db8::1");
        assert_eq!(parsed.port, 46000);
        assert_eq!(parsed.password, "p@ss&w0rd");
        assert_eq!(parsed.peer_address(), "[2001:db8::1]:46000");
    }

    // --- parse_peer (для UCI-валидации и экспорта) ---

    #[test]
    fn parse_peer_accepts_ipv4_ipv6_and_hostnames() {
        assert_eq!(
            parse_peer("198.51.100.10:46000"),
            Some(("198.51.100.10".to_string(), 46000))
        );
        assert_eq!(
            parse_peer("[2001:db8::1]:46000"),
            Some(("2001:db8::1".to_string(), 46000))
        );
        assert_eq!(
            parse_peer("vps.example.com:46000"),
            Some(("vps.example.com".to_string(), 46000))
        );
    }

    #[test]
    fn parse_peer_rejects_garbage() {
        assert_eq!(parse_peer(""), None);
        assert_eq!(parse_peer("host"), None);
        assert_eq!(parse_peer(":46000"), None);
        assert_eq!(parse_peer("host:"), None);
        assert_eq!(parse_peer("host:0"), None);
        assert_eq!(parse_peer("host:65536"), None);
        assert_eq!(parse_peer("host:46000:9"), None);
        assert_eq!(parse_peer("bare::v6:1"), None);
        assert_eq!(parse_peer("[::1]46000"), None);
        assert_eq!(parse_peer("[::1]:"), None);
    }
}
