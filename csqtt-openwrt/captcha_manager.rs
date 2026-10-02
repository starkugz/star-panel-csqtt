// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [OpenWrt-порт, M4d] Daemon CaptchaManager: lifecycle CAPTCHA-вызовов.
//!
//! Ядро (`captcha.rs`) сознательно не тронуто: Rust v2 solver и upstream
//! wire contract `CAPTCHA_SOLVE|mode|redirect_uri|session_token` /
//! `CAPTCHA_RESULT|result` сохранены как есть (request-id в wire-format не
//! добавлен — совместимость с внешним помощником не ломается).
//!
//! Этот модуль — daemon-уровень над тем же wire-каналом:
//! - перехватывает wire-запрос капчи ещё до файлового лог-синка;
//! - изолирует секреты (`redirect_uri` может содержать sensitive query,
//!   `session_token` и результат/ success token — SECRET): они живут только
//!   в памяти менеджера и никогда не попадают в status.json/обычный лог/QR;
//! - ведёт безопасный challenge-id (≥128 bit) с TTL и state-машиной;
//! - выдаёт одноразовую browser-capability (≥128 bit, TTL, single-use,
//!   привязана к challenge; хранится только SHA-256 от токена).
//!
//! ## Lifecycle (выбран минимальный вариант после аудита — см. history/M4d)
//!
//! В службе CSQTT одновременно ровно один клиент владеет фиксированным именем
//! `csqtt0`, а captcha-ожидание живёт внутри работающего клиента (фаза auth
//! воркера, `CaptchaSolver` блокирует задачу). Поэтому оставить auth-задачу
//! A ждать результат, не владея интерфейсом, и параллельно поднять B нельзя:
//! `A → CAPTCHA_REQUIRED → cancel A → B ACTIVE`. «Решить A» — повторная
//! попытка профиля через менеджер (новый challenge, новый session_token),
//! которая не трогает активный `csqtt0` до READY.

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::time::{Duration, Instant};

/// Размер случайных challenge-id: 32 байта = 256 bit (≥128 bit).
const CHALLENGE_ID_BYTES: usize = 32;
/// Размер одноразовой browser-capability: 32 байта = 256 bit (≥128 bit).
const CAPABILITY_BYTES: usize = 32;
/// Время жизни challenge по умолчанию: покрывает максимальную solver-цепочку
/// ядра (wv selected 120с; auto ≈ Rust v2 + WBV Auto 45с + Manual 60с) с
/// запасом. Меньше — риск убить живое окно, больше — профиль застрянет.
pub const CHALLENGE_TTL: Duration = Duration::from_secs(240);
/// Время жизни одноразовой capability по умолчанию: короче жизни challenge
/// (браузер должен успеть открыть окно, пока запрос ещё валиден).
pub const CAPABILITY_TTL: Duration = Duration::from_secs(180);
/// Сколько terminal-записей хранить в памяти для UI/истории.
const MAX_TERMINAL: usize = 16;

/// Состояние одного challenge (M4d требование 5).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChallengeState {
    /// Wire-запрос получен, capability выдана, ждём решения.
    Pending,
    /// Capability погашена — внешний помощник открыл браузер.
    Opened,
    /// Результат передан решателю, проверяется ядром.
    Verifying,
    /// Результат принят и доставлен решателю.
    Solved,
    /// Решатель/помощник ответил ошибкой, либо результат устарел.
    Failed,
    /// Истёк TTL без результата.
    Expired,
    /// Отменён оператором или службой CSQTT.
    Cancelled,
}

impl ChallengeState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Opened => "opened",
            Self::Verifying => "verifying",
            Self::Solved => "solved",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_live(self) -> bool {
        matches!(self, Self::Pending | Self::Opened | Self::Verifying)
    }

    fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Solved | Self::Failed | Self::Expired | Self::Cancelled
        )
    }
}

/// Секретный payload challenge. `redirect_uri` может содержать sensitive
/// query, `session_token` — SECRET. Классификация секретов (M4d требование 4):
/// значение хранится только в памяти менеджера — его нет в status.json,
/// обычном логе и QR. Debug никогда не выводит значения.
#[derive(Clone)]
pub struct SecretPayload {
    pub redirect_uri: String,
    pub session_token: String,
}

impl std::fmt::Debug for SecretPayload {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SecretPayload")
            .field("redirect_uri", &"<redacted>")
            .field("session_token", &"<redacted>")
            .finish()
    }
}

/// Разобранный upstream wire-запрос `CAPTCHA_SOLVE|mode|redirect_uri|session_token`.
/// Несёт секреты — поэтому Debug их не показывает.
#[derive(Clone)]
pub struct CaptchaSolveRequest {
    /// Режим окна, в котором ядро открыло WebView: auto/selected/manual
    /// (это не captcha_mode профиля, а wire-режим ядра).
    pub mode: String,
    pub secret: SecretPayload,
}

impl std::fmt::Debug for CaptchaSolveRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptchaSolveRequest")
            .field("mode", &self.mode)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Разбор wire-строки капчи. Никаких request-id (контракт CAPTCHA): один
/// активный запрос на профиль. Битые/неполные строки → None, секреты таких
/// строк никуда не сохраняются. Пустой режим отклоняется (ядро всегда
/// шлёт auto/selected/manual).
pub fn parse_captcha_solve_line(line: &str) -> Option<CaptchaSolveRequest> {
    let rest = line.trim().strip_prefix("CAPTCHA_SOLVE|")?;
    let mut fields = rest.splitn(3, '|');
    let mode = fields.next()?.to_string();
    let redirect_uri = fields.next()?.to_string();
    let session_token = fields.next()?.to_string();
    if mode.is_empty() || redirect_uri.is_empty() || session_token.is_empty() {
        return None;
    }
    Some(CaptchaSolveRequest {
        mode,
        secret: SecretPayload {
            redirect_uri,
            session_token,
        },
    })
}

/// [High-1 AUDIT] Fail-closed скраббер `session_token=<значение>` (в т.ч.
/// внутри URL в строках ошибок HTTP-клиента): значение секрета заменяется
/// на REDACTED целиком, до записи в файловый лог. Параметр ищется
/// регистронезависимо; значение — до следующего разделителя (пробел, `|`,
/// кавычки, запятая/точка с запятой, `&`, скобки/угловые скобки) или конца.
pub fn redact_session_token(line: &str) -> std::borrow::Cow<'_, str> {
    const KEY: &str = "session_token=";
    const VALUE_DELIMITERS: &[char] = &[
        ' ', '\t', '\n', '\r', '|', '"', '\'', ',', ';', '&', ')', '(', '>', '<',
    ];
    if !line.to_ascii_lowercase().contains(KEY) {
        return std::borrow::Cow::Borrowed(line);
    }
    let lower = line.to_ascii_lowercase();
    let mut result = String::with_capacity(line.len());
    let mut rest = line;
    let mut rest_lower = lower.as_str();
    while let Some(position) = rest_lower.find(KEY) {
        let key_end = position + KEY.len();
        result.push_str(&rest[..key_end]);
        let value_start = rest[key_end..]
            .char_indices()
            .find(|(_, character)| VALUE_DELIMITERS.contains(character))
            .map_or(rest.len(), |(offset, _)| key_end + offset);
        result.push_str(crate::uci::REDACTED);
        rest = &rest[value_start..];
        rest_lower = &rest_lower[value_start..];
    }
    result.push_str(rest);
    std::borrow::Cow::Owned(result)
}

/// Одноразовая browser-capability (M4d требование 6).
struct Capability {
    /// Хранится только SHA-256 от токена: сам токен выдаётся один раз
    /// вызывающей стороне и больше нигде не сохраняется.
    token_hash: [u8; 32],
    expires: Instant,
    used: bool,
}

/// Один challenge службы CSQTT. Секретный payload — отдельным полем.
pub struct CaptchaChallenge {
    id: String,
    profile_id: String,
    mode: String,
    created: Instant,
    expires: Instant,
    state: ChallengeState,
    secret: SecretPayload,
    capability: Option<Capability>,
}

impl CaptchaChallenge {
    fn view(&self) -> ChallengeView {
        ChallengeView {
            id: self.id.clone(),
            profile_id: self.profile_id.clone(),
            mode: self.mode.clone(),
            state: self.state,
            created: self.created,
            expires: self.expires,
        }
    }
}

/// Debug challenge: safe fields только, секреты — никогда.
impl std::fmt::Debug for CaptchaChallenge {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CaptchaChallenge")
            .field("id", &self.id)
            .field("profile_id", &self.profile_id)
            .field("mode", &self.mode)
            .field("state", &self.state)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// Безопасное представление challenge для status.json/UI: только safe fields.
/// Секретов (`session_token`, `redirect_uri`, result) здесь не бывает.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChallengeView {
    pub id: String,
    pub profile_id: String,
    pub mode: String,
    pub state: ChallengeState,
    pub created: Instant,
    pub expires: Instant,
}

/// Результат приёма решения капчи.
///
/// Debug — ручной: `Accepted` несёт success token (SECRET), в открытом виде
/// не выводится никогда.
pub enum SubmitOutcome {
    /// Результат валиден и принят (state → Verifying): вызывающий доставляет
    /// его решателю активного профиля и подтверждает исход через
    /// `confirm_solved`/`fail_stale` (двухфазная фиксация).
    Accepted(String),
    /// Результат невалиден (пустой или `error:…`) — state → Failed.
    InvalidResult,
    /// Нет такого живого challenge: replay/истёк/отменён — отклонено.
    NotLive,
}

impl std::fmt::Debug for SubmitOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Accepted(_) => formatter
                .debug_tuple("Accepted")
                .field(&"<redacted>")
                .finish(),
            Self::InvalidResult => formatter.write_str("InvalidResult"),
            Self::NotLive => formatter.write_str("NotLive"),
        }
    }
}

/// Менеджер challenges. Чистая логика: время — параметром, I/O и таймеров
/// нет (служба CSQTT гоняет `expire` на своих тиках). Память ограничена: terminal
/// записи вытесняются (`MAX_TERMINAL`).
pub struct CaptchaManager {
    challenges: Vec<CaptchaChallenge>,
    challenge_ttl: Duration,
    capability_ttl: Duration,
}

impl CaptchaManager {
    pub fn new() -> Self {
        Self {
            challenges: Vec::new(),
            challenge_ttl: CHALLENGE_TTL,
            capability_ttl: CAPABILITY_TTL,
        }
    }

    /// Тестовый/отладочный менеджер с короткими TTL (production — `new`).
    #[cfg(test)]
    fn with_ttls(challenge_ttl: Duration, capability_ttl: Duration) -> Self {
        Self {
            challenges: Vec::new(),
            challenge_ttl,
            capability_ttl,
        }
    }

    /// Зарегистрировать wire-запрос для профиля. Одно активное ожидание на
    /// профиль: предыдущий live-challenge того же профиля закрывается (Failed)
    /// — маппинг challenge-id на единственный активный upstream-wait.
    /// Возвращает safe view и raw capability-токен (выдаётся ровно один раз;
    /// доставляет его внешний транспорт — Web Helper, M4e).
    pub fn open_challenge(
        &mut self,
        profile_id: &str,
        mode: &str,
        request: CaptchaSolveRequest,
        now: Instant,
    ) -> (ChallengeView, String) {
        self.close_live(profile_id, ChallengeState::Failed);
        self.purge();
        let expires = now + self.challenge_ttl;
        let id = random_id(CHALLENGE_ID_BYTES);
        let token = random_id(CAPABILITY_BYTES);
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash = hasher.finalize().into();
        let challenge = CaptchaChallenge {
            id: id.clone(),
            profile_id: profile_id.to_string(),
            mode: mode.to_string(),
            created: now,
            expires,
            state: ChallengeState::Pending,
            secret: request.secret,
            capability: Some(Capability {
                token_hash,
                expires: now + self.capability_ttl,
                used: false,
            }),
        };
        let view = challenge.view();
        self.challenges.push(challenge);
        (view, token)
    }

    /// Все challenges (safe fields), свежие — в конце.
    pub fn views(&self) -> Vec<ChallengeView> {
        self.challenges.iter().map(CaptchaChallenge::view).collect()
    }

    pub fn view(&self, id: &str) -> Option<ChallengeView> {
        self.challenges
            .iter()
            .find(|challenge| challenge.id == id)
            .map(CaptchaChallenge::view)
    }

    /// Профиль challenge (для маппинга на активный upstream-wait).
    pub fn profile_of(&self, id: &str) -> Option<String> {
        self.challenges
            .iter()
            .find(|challenge| challenge.id == id)
            .map(|challenge| challenge.profile_id.clone())
    }

    /// Есть ли live-challenge (pending/opened/verifying) для профиля:
    /// клиент ждёт решения — службе CSQTT нельзя ронять его по health.
    pub fn live_for_profile(&self, profile_id: &str) -> bool {
        self.challenges
            .iter()
            .any(|challenge| challenge.profile_id == profile_id && challenge.state.is_live())
    }

    /// Погасить capability: одноразовая, с TTL, привязанная к challenge.
    /// Успех → state Opened, возвращает секреты payload для браузера (M4e).
    /// Повтор токеном → None (single-use, replay отклонён).
    pub fn consume_capability(
        &mut self,
        token: &str,
        now: Instant,
    ) -> Option<(String, SecretPayload)> {
        let mut hasher = Sha256::new();
        hasher.update(token.as_bytes());
        let token_hash: [u8; 32] = hasher.finalize().into();
        let challenge = self.challenges.iter_mut().find(|challenge| {
            challenge
                .capability
                .as_ref()
                .is_some_and(|capability| capability.token_hash == token_hash)
        })?;
        let capability = challenge.capability.as_mut()?;
        if capability.used || now > capability.expires || !challenge.state.is_live() {
            return None;
        }
        capability.used = true;
        challenge.state = ChallengeState::Opened;
        Some((challenge.id.clone(), challenge.secret.clone()))
    }

    /// Принять результат решения (одноразово). Результат-секрет: повторная
    /// отправка того же/чужого значения в другой challenge не принимается.
    /// Сразу переводит challenge в Verifying — доставка решателю
    /// подтверждается отдельно (`confirm_solved`/`fail_stale`).
    pub fn submit_result(&mut self, id: &str, result: &str, now: Instant) -> SubmitOutcome {
        let Some(challenge) = self.challenges.iter_mut().find(|c| c.id == id) else {
            return SubmitOutcome::NotLive;
        };
        if !challenge.state.is_live() {
            return SubmitOutcome::NotLive;
        }
        if now > challenge.expires {
            challenge.state = ChallengeState::Expired;
            return SubmitOutcome::NotLive;
        }
        let result = result.trim();
        if result.is_empty() {
            challenge.state = ChallengeState::Failed;
            return SubmitOutcome::InvalidResult;
        }
        if result.to_ascii_lowercase().starts_with("error:") {
            challenge.state = ChallengeState::Failed;
            return SubmitOutcome::InvalidResult;
        }
        // Upstream wire без request-id: результат встраивается службой CSQTT в
        // control-линию `CAPTCHA_RESULT|{result}` — управляющие символы
        // (включая NUL) и разделитель запрещены, иначе инъекция команд
        // решателю через разбиение строк.
        if result
            .bytes()
            .any(|byte| byte == b'|' || byte.is_ascii_control())
        {
            challenge.state = ChallengeState::Failed;
            return SubmitOutcome::InvalidResult;
        }
        challenge.state = ChallengeState::Verifying;
        SubmitOutcome::Accepted(result.to_string())
    }

    /// Подтвердить доставку результата решателю (state → Solved).
    pub fn confirm_solved(&mut self, id: &str) -> bool {
        if let Some(challenge) = self.challenges.iter_mut().find(|c| c.id == id)
            && challenge.state == ChallengeState::Verifying
        {
            challenge.state = ChallengeState::Solved;
            return true;
        }
        false
    }

    /// Провалить доставку: ждать некому/профиль не активен (state → Failed).
    pub fn fail_stale(&mut self, id: &str) {
        if let Some(challenge) = self.challenges.iter_mut().find(|c| c.id == id)
            && challenge.state == ChallengeState::Verifying
        {
            challenge.state = ChallengeState::Failed;
        }
    }

    /// Отменить challenge оператором/службой CSQTT. Возвращает false, если такого
    /// live-challenge нет.
    pub fn cancel(&mut self, id: &str) -> bool {
        self.close_live_by_id(id, ChallengeState::Cancelled)
    }

    /// Закрыть протухшие challenges. Возвращает profile_id свеже-протухших
    /// (служба CSQTT решает по ним судьбу профиля).
    pub fn expire(&mut self, now: Instant) -> Vec<String> {
        let mut expired = Vec::new();
        for challenge in &mut self.challenges {
            if challenge.state.is_live() && now > challenge.expires {
                challenge.state = ChallengeState::Expired;
                expired.push(challenge.profile_id.clone());
            }
        }
        expired
    }

    /// Удалить самые старые terminal-записи (bounded history).
    pub fn purge(&mut self) {
        let terminal = self
            .challenges
            .iter()
            .filter(|c| c.state.is_terminal())
            .count();
        if terminal <= MAX_TERMINAL {
            return;
        }
        let drop = terminal - MAX_TERMINAL;
        let mut dropped = 0;
        self.challenges.retain(|challenge| {
            if challenge.state.is_terminal() && dropped < drop {
                dropped += 1;
                false
            } else {
                true
            }
        });
    }

    /// Закрыть live-challenge профиля как Failed: клиент сдался, ждать
    /// результата больше некому (служба CSQTT использует на ClientFinished).
    pub fn fail_live(&mut self, profile_id: &str) {
        self.close_live(profile_id, ChallengeState::Failed);
    }

    /// Закрыть live-challenge профиля указанным terminal-состоянием.
    fn close_live(&mut self, profile_id: &str, state: ChallengeState) {
        for challenge in &mut self.challenges {
            if challenge.profile_id == profile_id && challenge.state.is_live() {
                challenge.state = state;
                challenge.capability = None;
            }
        }
    }

    fn close_live_by_id(&mut self, id: &str, state: ChallengeState) -> bool {
        let Some(challenge) = self.challenges.iter_mut().find(|c| c.id == id) else {
            return false;
        };
        if !challenge.state.is_live() {
            return false;
        }
        challenge.state = state;
        challenge.capability = None;
        true
    }

    /// Тестовый/диагностический доступ к секретам по id (проверка redaction).
    #[cfg(test)]
    fn secret_of(&self, id: &str) -> Option<&SecretPayload> {
        self.challenges
            .iter()
            .find(|challenge| challenge.id == id)
            .map(|challenge| &challenge.secret)
    }
}

impl Default for CaptchaManager {
    fn default() -> Self {
        Self::new()
    }
}

fn random_id(bytes: usize) -> String {
    let mut buffer = vec![0u8; bytes];
    // getrandom — криптографический источник ОС; fallback недопустим:
    // предсказуемый id/capability — это обход gate-логики службы CSQTT.
    getrandom::fill(&mut buffer).expect("getrandom: no entropy source available");
    URL_SAFE_NO_PAD.encode(buffer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn now() -> Instant {
        Instant::now()
    }

    fn request() -> CaptchaSolveRequest {
        CaptchaSolveRequest {
            mode: "auto".to_string(),
            secret: SecretPayload {
                redirect_uri: "https://vk.com/captcha?sid=42&secret-query=value".to_string(),
                session_token: "SESSION-TOKEN-SECRET-0123456789".to_string(),
            },
        }
    }

    #[test]
    fn wire_line_parsers_modes_and_keeps_secrets_out_of_debug() {
        let parsed =
            parse_captcha_solve_line("CAPTCHA_SOLVE|manual|https://vk.com/captcha?sid=1|TOKEN-ABC")
                .expect("valid wire line");
        assert_eq!(parsed.mode, "manual");
        assert_eq!(parsed.secret.session_token, "TOKEN-ABC");
        // Debug не содержит секретов.
        let debug = format!("{parsed:?}");
        assert!(!debug.contains("TOKEN-ABC"));
        assert!(!debug.contains("https://vk.com/captcha"));
    }

    #[test]
    fn wire_line_rejects_prefix_and_partial_lines() {
        assert!(parse_captcha_solve_line("[КАПЧА] не wire-строка").is_none());
        assert!(parse_captcha_solve_line("CAPTCHA_SOLVE|auto").is_none());
        assert!(parse_captcha_solve_line("CAPTCHA_SOLVE|auto|uri|").is_none());
        assert!(parse_captcha_solve_line("CAPTCHA_SOLVE|auto||token").is_none());
        // Пустой режим отклоняется (ядро всегда шлёт auto/selected/manual).
        assert!(parse_captcha_solve_line("CAPTCHA_SOLVE||uri|token").is_none());
    }

    /// [High-1 AUDIT] Регресс: session_token вырезается из диагностических
    /// строк (URL в ошибках HTTP-клиента) до записи в лог-файл.
    #[test]
    fn redact_session_token_scrubs_values_in_urls_and_pairs() {
        let scrubbed = redact_session_token(
            "[КАПЧА] send failed https://id.vk.com/captcha?session_token=LEAKED-9999&x=1 (retry)",
        );
        assert!(!scrubbed.contains("LEAKED-9999"));
        assert!(scrubbed.contains("session_token=***"));
        assert!(scrubbed.contains("&x=1"));
        // Несколько вхождений и другой регистр ключа.
        let scrubbed = redact_session_token("SESSION_TOKEN=AAA|session_token=BBB");
        assert!(!scrubbed.contains("AAA"));
        assert!(!scrubbed.contains("BBB"));
        // Обычные строки не мутируются (Cow::Borrowed).
        let plain = "[КЛИЕНТ] обычная строка лога";
        assert!(matches!(
            redact_session_token(plain),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(redact_session_token(plain), plain);
        // Непечатаемый секрет в конце строки без разделителя.
        let scrubbed = redact_session_token("error at session_token=TAILSECRET");
        assert_eq!(
            scrubbed,
            format!("error at session_token={}", crate::uci::REDACTED)
        );
    }

    #[test]
    fn challenge_id_and_capability_carry_at_least_128_bits() {
        let mut manager = CaptchaManager::new();
        let (view, token) = manager.open_challenge("p10", "auto", request(), now());
        // 32 байта энтропии в urlsafe-base64 = 43 символа; ≥128 bit = ≥16 байт.
        assert!(
            URL_SAFE_NO_PAD
                .decode(view.id)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
                >= 16,
            "challenge id must carry >=128 bits of entropy"
        );
        assert!(
            URL_SAFE_NO_PAD
                .decode(token)
                .map(|bytes| bytes.len())
                .unwrap_or(0)
                >= 16,
            "capability token must carry >=128 bits of entropy"
        );
    }

    #[test]
    fn secrets_never_appear_in_views_or_challenge_debug() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        let debug = format!("{view:?}");
        assert!(!debug.contains("SESSION-TOKEN-SECRET"));
        assert!(!debug.contains("secret-query"));
        // Сами секреты в памяти есть (иначе менеджер бесполезен), но
        // публичные представления их не содержат.
        assert!(
            manager
                .secret_of(&view.id)
                .is_some_and(|secret| secret.session_token == "SESSION-TOKEN-SECRET-0123456789")
        );
    }

    #[test]
    fn capability_is_single_use_and_ttl_bound() {
        let mut manager =
            CaptchaManager::with_ttls(Duration::from_secs(60), Duration::from_secs(5));
        let (view, token) = manager.open_challenge("p10", "auto", request(), now());
        let first = manager.consume_capability(&token, now());
        assert!(first.is_some(), "first capability use must succeed");
        assert_eq!(first.unwrap().0, view.id);
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Opened
        );
        // Replay того же токена — отклоняется (single-use).
        assert!(manager.consume_capability(&token, now()).is_none());
        // Новый challenge — новый токен; протухший (после TTL) — отклоняется.
        let (view2, token2) = manager.open_challenge("p10", "auto", request(), now());
        let later = now() + Duration::from_secs(6);
        assert!(manager.consume_capability(&token2, later).is_none());
        assert_eq!(
            manager.view(&view2.id).unwrap().state,
            ChallengeState::Pending
        );
    }

    #[test]
    fn submit_result_moves_pending_to_solved_exactly_once() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        let outcome = manager.submit_result(&view.id, "  success-token-value  ", now());
        match outcome {
            SubmitOutcome::Accepted(result) => assert_eq!(result, "success-token-value"),
            other => panic!("ожидался Accepted, получено {other:?}"),
        }
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Verifying
        );
        // Двухфазная фиксация: доставка подтверждена — Solved.
        assert!(manager.confirm_solved(&view.id));
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Solved
        );
        // Повторная отправка (replay) — отклонена.
        assert!(matches!(
            manager.submit_result(&view.id, "success-token-value", now()),
            SubmitOutcome::NotLive
        ));
    }

    #[test]
    fn stale_result_is_failed_not_substituted() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        assert!(matches!(
            manager.submit_result(&view.id, "result", now()),
            SubmitOutcome::Accepted(_)
        ));
        // Доставка не состоялась — результат не подставляется другому профилю.
        manager.fail_stale(&view.id);
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Failed
        );
    }

    #[test]
    fn submit_result_rejects_control_line_injection() {
        // Результат встраивается в `CAPTCHA_RESULT|{result}`: переводы строк
        // и разделитель отклонили бы линию/подставили команду решателю.
        for payload in [
            "ok\nCAPTCHA_RESULT|forged",
            "ok\r\nforged",
            "ok|forged",
            "ok\x00forged",
            "ok\x07forged",
            "ok\x1bforged",
            "ok\x7fforged",
        ] {
            let mut manager = CaptchaManager::new();
            let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
            assert!(
                matches!(
                    manager.submit_result(&view.id, payload, now()),
                    SubmitOutcome::InvalidResult
                ),
                "payload must be rejected: {payload:?}"
            );
            assert_eq!(
                manager.view(&view.id).unwrap().state,
                ChallengeState::Failed
            );
        }
    }

    #[test]
    fn submit_outcome_debug_hides_result_secret() {
        let debug = format!(
            "{:?}",
            SubmitOutcome::Accepted("SUCCESS-TOKEN-SECRET".to_string())
        );
        assert!(!debug.contains("SUCCESS-TOKEN-SECRET"));
        assert!(debug.contains("Accepted"));
    }

    #[test]
    fn submit_result_rejects_empty_and_error_payloads() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        assert!(matches!(
            manager.submit_result(&view.id, "", now()),
            SubmitOutcome::InvalidResult
        ));
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Failed
        );
        let (view2, _token2) = manager.open_challenge("p10", "auto", request(), now());
        assert!(matches!(
            manager.submit_result(&view2.id, "error:timeout", now()),
            SubmitOutcome::InvalidResult
        ));
        assert_eq!(
            manager.view(&view2.id).unwrap().state,
            ChallengeState::Failed
        );
    }

    #[test]
    fn submit_result_for_unknown_or_expired_challenge_is_rejected() {
        let mut manager = CaptchaManager::with_ttls(Duration::from_secs(1), Duration::from_secs(1));
        assert!(matches!(
            manager.submit_result("nope", "result", now()),
            SubmitOutcome::NotLive
        ));
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        let later = now() + Duration::from_secs(2);
        let expired = manager.expire(later);
        assert_eq!(expired, vec!["p10"]);
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Expired
        );
        assert!(matches!(
            manager.submit_result(&view.id, "result", later),
            SubmitOutcome::NotLive
        ));
    }

    #[test]
    fn expire_returns_profiles_of_newly_expired_challenges_only() {
        let mut manager =
            CaptchaManager::with_ttls(Duration::from_secs(10), Duration::from_secs(10));
        let (first, _token) = manager.open_challenge("p10", "auto", request(), now());
        let after = now() + Duration::from_secs(11);
        let (second, _token2) = manager.open_challenge("p20", "auto", request(), after);
        let expired = manager.expire(after + Duration::from_secs(1));
        assert_eq!(expired, vec!["p10"]);
        assert_eq!(
            manager.view(&first.id).unwrap().state,
            ChallengeState::Expired
        );
        assert_eq!(
            manager.view(&second.id).unwrap().state,
            ChallengeState::Pending
        );
        // Повторный вызов не возвращает уже закрытые.
        assert!(manager.expire(after + Duration::from_secs(2)).is_empty());
    }

    #[test]
    fn cancel_closes_live_challenge_only() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        assert!(manager.cancel(&view.id));
        assert_eq!(
            manager.view(&view.id).unwrap().state,
            ChallengeState::Cancelled
        );
        // Повторная отмена/отмена чужого — false.
        assert!(!manager.cancel(&view.id));
        assert!(!manager.cancel("unknown"));
    }

    #[test]
    fn open_challenge_replaces_previous_live_challenge_of_profile() {
        let mut manager = CaptchaManager::new();
        let (first, _token) = manager.open_challenge("p10", "auto", request(), now());
        let (second, _token2) = manager.open_challenge("p10", "manual", request(), now());
        assert_ne!(first.id, second.id);
        assert_eq!(
            manager.view(&first.id).unwrap().state,
            ChallengeState::Failed
        );
        assert_eq!(
            manager.view(&second.id).unwrap().state,
            ChallengeState::Pending
        );
        // Другой профиль не затронут.
        let (other, _token3) = manager.open_challenge("p20", "auto", request(), now());
        assert_eq!(
            manager.view(&other.id).unwrap().state,
            ChallengeState::Pending
        );
    }

    #[test]
    fn live_for_profile_tracks_pending_opened_and_verifying() {
        let mut manager =
            CaptchaManager::with_ttls(Duration::from_secs(60), Duration::from_secs(60));
        assert!(!manager.live_for_profile("p10"));
        let (view, token) = manager.open_challenge("p10", "auto", request(), now());
        assert!(manager.live_for_profile("p10"));
        assert!(!manager.live_for_profile("p20"));
        assert!(manager.consume_capability(&token, now()).is_some());
        assert!(manager.live_for_profile("p10"));
        assert!(matches!(
            manager.submit_result(&view.id, "ok", now()),
            SubmitOutcome::Accepted(_)
        ));
        assert!(manager.live_for_profile("p10"));
        assert!(manager.confirm_solved(&view.id));
        assert!(!manager.live_for_profile("p10"));
    }

    #[test]
    fn purge_keeps_terminal_history_bounded() {
        let mut manager =
            CaptchaManager::with_ttls(Duration::from_secs(60), Duration::from_secs(60));
        for index in 0..(MAX_TERMINAL + 5) {
            let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
            assert!(matches!(
                manager.submit_result(&view.id, &format!("tok-{index}"), now()),
                SubmitOutcome::Accepted(_)
            ));
            assert!(manager.confirm_solved(&view.id));
        }
        manager.purge();
        let terminal = manager
            .views()
            .into_iter()
            .filter(|view| view.state.is_terminal())
            .count();
        assert!(
            terminal <= MAX_TERMINAL,
            "terminal history must be bounded: {terminal}"
        );
    }

    #[test]
    fn submit_result_after_cancel_is_rejected() {
        let mut manager = CaptchaManager::new();
        let (view, _token) = manager.open_challenge("p10", "auto", request(), now());
        assert!(manager.cancel(&view.id));
        assert!(matches!(
            manager.submit_result(&view.id, "result", now()),
            SubmitOutcome::NotLive
        ));
    }
}
