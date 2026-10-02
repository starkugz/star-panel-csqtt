// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! [OpenWrt-порт, M4e] Local Web Helper: human-fallback CAPTCHA через обычный
//! браузер (главный target — Safari на iPhone без обязательного приложения).
//!
//! Контракт (поверх CaptchaManager M4d, wire ядра не меняется):
//! - helper-URL содержит ТОЛЬКО LAN-адрес роутера + safe challenge_id +
//!   одноразовую capability: `http://<lan>:<port>/c/<id>?cap=<token>`.
//!   session_token/password/device-id/VK hashes сюда не попадают никогда.
//! - `GET /c/<id>?cap=…` гасит capability через службу CSQTT
//!   ([`DaemonCommand::HelperConsume`]) и отдаёт страницу без секретов;
//!   штатный VK flow открывается server-side redirect'ом (`/go`) — секретный
//!   `redirect_uri` живёт только в памяти helper-сессии.
//! - Completion воспроизводит фактический upstream mechanism (see
//!   docs/reference/M4e.md): результат — success_token из перехвата
//!   `captchaNotRobot.check` в WebView-хосте Android. В обычном браузере
//!   инъекция JS в vk.com невозможна, поэтому helper принимает результат
//!   через аутентифицированный submit-endpoint (ticket из capability-сессии),
//!   а «прошёл в браузере без токена» = отмена challenge и повторная попытка
//!   профиля (M4d lifecycle). Никакого самодельного token parser.
//! - LAN-only: bind вне приватных/loopback-адресов отклоняётся
//!   ([`validate_listen_addr`]); WAN exposure запрещён конструкцией.
//! - CSRF/replay: capability single-use (M4d), ticket выдаётся один раз на
//!   сессию, хранится только SHA-256, результат принимается однократно;
//!   никаких cookie — авторизация только через секрет в URL/теле формы.
//! - Интеграционный API для M6 (LuCI/rpcd, loopback-only):
//!   `GET /api/challenges`, `GET /api/challenge/<id>`,
//!   `GET /api/challenge/<id>/helper-url`, `POST /api/challenge/<id>/cancel`.

use crate::captcha_manager::{CAPABILITY_TTL, CHALLENGE_TTL};
use crate::pool::DaemonCommand;
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

/// Максимум на заголовок запроса (request-line + headers).
const MAX_HEADER_BYTES: usize = 16 * 1024;
/// Максимум тела формы.
const MAX_BODY_BYTES: usize = 8 * 1024;
/// Время жизни helper-сессии (ticket): покрывает полный TTL challenge.
pub const SESSION_TTL: Duration = CHALLENGE_TTL;
/// Время жизни capability-записи для helper-url API (не дольше capability).
pub const CAPABILITY_RECORD_TTL: Duration = CAPABILITY_TTL;

/// Порт upstream `CaptchaUriPolicy.kt:8–17`: переход разрешён только на
/// https-домены VK/OK (и поддомены). Без этого проверки 302 helper мог бы
/// стать open-redirect'ом (в т.ч. при скомпрометированном wire-источнике),
/// а управляющие символы — сломать заголовок Location.
pub fn captcha_uri_allowed(uri: &str) -> bool {
    if uri
        .bytes()
        .any(|byte| byte.is_ascii_control() || byte == b' ' || byte == b'|')
    {
        return false;
    }
    let Ok(url) = url::Url::parse(uri) else {
        return false;
    };
    if url.scheme() != "https" {
        return false;
    }
    let Some(host) = url.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    ["vk.com", "vk.ru", "ok.ru", "okcdn.ru"]
        .iter()
        .any(|domain| host == *domain || host.ends_with(&format!(".{domain}")))
}

/// Политика прослушивания: только приватные (RFC1918) и loopback адреса.
/// `0.0.0.0`/`::` (unspecified), link-local (169.254/fe80) и публичные адреса
/// запрещены (WAN exposure).
pub fn validate_listen_addr(addr: SocketAddr) -> Result<(), &'static str> {
    match addr.ip() {
        IpAddr::V4(v4) => {
            if v4.is_unspecified() {
                return Err("helper не может слушать 0.0.0.0: только конкретный LAN-адрес");
            }
            let octets = v4.octets();
            let private = octets[0] == 10 || (octets[0] == 172 && (16..=31).contains(&octets[1]));
            let lan = private || v4.is_loopback() || (octets[0] == 192 && octets[1] == 168);
            if lan {
                Ok(())
            } else {
                Err("helper слушает только LAN (RFC1918/loopback): WAN exposure запрещён")
            }
        }
        IpAddr::V6(v6) => {
            if v6.is_unspecified() {
                return Err("helper не может слушать [::]: только конкретный LAN-адрес");
            }
            let octets = v6.octets();
            let ula = octets[0] & 0xfe == 0xfc;
            if v6.is_loopback() || ula {
                Ok(())
            } else {
                Err("helper слушает только loopback/ULA IPv6: WAN exposure запрещён")
            }
        }
    }
}

/// Порт Web Helper по умолчанию (LAN-only).
pub const DEFAULT_HELPER_PORT: u16 = 8443;

/// LAN-адрес роутера для Web Helper (`--helper-listen auto`).
///
/// OpenWrt-native и без shell: разбираем `/etc/config/network`, секцию
/// `interface 'lan'`, опцию `ipaddr`. Никаких `ip`/`uci` вызовов (контракт
/// M5/M3X). Если адрес не найден — `None`: helper выключается, WAN не
/// открывается.
pub fn detect_lan_listen(port: u16) -> Option<SocketAddr> {
    let text = std::fs::read_to_string("/etc/config/network").ok()?;
    parse_lan_ip(&text).map(|ip| SocketAddr::new(ip, port))
}

/// Разбор `lan`-секции UCI `/etc/config/network` → IPv4 адрес.
fn parse_lan_ip(text: &str) -> Option<IpAddr> {
    let mut in_lan = false;
    for raw in text.lines() {
        let line = raw.trim();
        if line.starts_with("config interface") {
            in_lan = line.contains("'lan'") || line.contains("\"lan\"");
        } else if in_lan && line.starts_with("option ipaddr") {
            let value = line
                .trim_start_matches("option ipaddr")
                .trim()
                .trim_matches(|c| c == '\'' || c == '"');
            // OpenWrt хранит адрес с префиксом (192.168.1.1/24) — берём IP.
            let value = value.split('/').next().unwrap_or(value);
            if let Ok(ip) = value.parse::<IpAddr>() {
                return Some(ip);
            }
        }
    }
    None
}

/// Разобранный HTTP-запрос (минимальный подмножество: метод, path, query,
/// заголовок Content-Type, тело формы).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HttpRequest {
    pub method: String,
    pub path: String,
    pub query: String,
    pub body: String,
}

/// Маршрут helper-эндпоинта (чистый разбор — тестируется без сети).
#[derive(Debug, PartialEq, Eq)]
pub enum Route {
    Index,
    Open {
        id: String,
        cap: String,
    },
    Go {
        id: String,
        ticket: String,
    },
    Status {
        id: String,
        ticket: String,
    },
    Submit {
        id: String,
        ticket: String,
        result: String,
    },
    Done {
        id: String,
        ticket: String,
    },
    ApiList,
    ApiInfo {
        id: String,
    },
    ApiHelperUrl {
        id: String,
    },
    ApiCancel {
        id: String,
    },
    NotFound,
    BadRequest,
    MethodNotAllowed,
}

fn query_param(query: &str, name: &str) -> Option<String> {
    query.split('&').find_map(|pair| {
        let (key, value) = pair.split_once('=')?;
        (key == name).then(|| percent_decode(value))
    })
}

fn form_param(body: &str, name: &str) -> Option<String> {
    query_param(body, name)
}

/// Минимальный percent-decoding для form-urlencoded (`+` → пробел).
pub fn percent_decode(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'+' => {
                out.push(b' ');
                index += 1;
            }
            b'%' if index + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).ok();
                match hex.and_then(|pair| u8::from_str_radix(pair, 16).ok()) {
                    Some(byte) => {
                        out.push(byte);
                        index += 3;
                    }
                    None => {
                        out.push(b'%');
                        index += 1;
                    }
                }
            }
            byte => {
                out.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Разбор request-line (`METHOD PATH?QUERY HTTP/1.x`).
pub fn parse_request_line(line: &str) -> Option<HttpRequest> {
    let mut parts = line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    if !parts.next()?.starts_with("HTTP/") {
        return None;
    }
    let (path, query) = target.split_once('?').unwrap_or((target, ""));
    Some(HttpRequest {
        method,
        path: path.to_string(),
        query: query.to_string(),
        body: String::new(),
    })
}

/// Маршрутизация запроса. Методы: GET — страница/redirect/API-чтение,
/// POST — submit/done/cancel. Тело submit читается как form-urlencoded.
pub fn route(request: &HttpRequest) -> Route {
    let path = request.path.as_str();
    if request.method == "GET" && (path == "/" || path == "/index.html") {
        return Route::Index;
    }
    if request.method == "GET" && path == "/api/challenges" {
        return Route::ApiList;
    }
    let get = request.method == "GET";
    let post = request.method == "POST";
    if let Some(rest) = path.strip_prefix("/c/") {
        let mut segments = rest.split('/');
        let id = segments.next().unwrap_or("");
        if id.is_empty() {
            return Route::BadRequest;
        }
        return match segments.next() {
            None if get => match query_param(&request.query, "cap") {
                Some(cap) if !cap.is_empty() => Route::Open {
                    id: id.to_string(),
                    cap,
                },
                _ => Route::BadRequest,
            },
            Some("go") if get => match query_param(&request.query, "t") {
                Some(ticket) => Route::Go {
                    id: id.to_string(),
                    ticket,
                },
                None => Route::BadRequest,
            },
            Some("status") if get => match query_param(&request.query, "t") {
                Some(ticket) => Route::Status {
                    id: id.to_string(),
                    ticket,
                },
                None => Route::BadRequest,
            },
            Some("result") if post => {
                let Some(ticket) = form_param(&request.body, "t") else {
                    return Route::BadRequest;
                };
                let Some(result) = form_param(&request.body, "result") else {
                    return Route::BadRequest;
                };
                Route::Submit {
                    id: id.to_string(),
                    ticket,
                    result,
                }
            }
            Some("done") if post => match form_param(&request.body, "t") {
                Some(ticket) => Route::Done {
                    id: id.to_string(),
                    ticket,
                },
                None => Route::BadRequest,
            },
            _ => Route::MethodNotAllowed,
        };
    }
    if let Some(rest) = path.strip_prefix("/api/challenge/") {
        let mut segments = rest.split('/');
        let id = segments.next().unwrap_or("");
        if id.is_empty() {
            return Route::BadRequest;
        }
        return match segments.next() {
            None if get => Route::ApiInfo { id: id.to_string() },
            Some("helper-url") if get => Route::ApiHelperUrl { id: id.to_string() },
            Some("cancel") if post => Route::ApiCancel { id: id.to_string() },
            _ => Route::NotFound,
        };
    }
    Route::NotFound
}

/// Сессия helper после успешного гашения capability. `redirect_uri` — SECRET:
/// только память, Debug redacted.
struct HelperSession {
    ticket_hash: [u8; 32],
    redirect_uri: String,
    expires: Instant,
    submitted: bool,
    done: bool,
}

impl std::fmt::Debug for HelperSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperSession")
            .field("expires", &self.expires)
            .field("submitted", &self.submitted)
            .field("done", &self.done)
            .finish_non_exhaustive()
    }
}

/// Общая состояние helper: база-URL, capability-записи (для helper-url API),
/// сессии с тикетами. Служба CSQTT кладёт сюда raw capability при open_challenge,
/// HTTP-слой читает/обновляет.
pub struct HelperState {
    inner: Mutex<HelperInner>,
}

struct HelperInner {
    base_url: String,
    capabilities: HashMap<String, (String, Instant)>,
    sessions: HashMap<String, HelperSession>,
}

/// Debug — ручной: значения capability и redirect_uri (SECRET) не печатаются.
impl std::fmt::Debug for HelperInner {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HelperInner")
            .field("base_url", &self.base_url)
            .field(
                "capabilities",
                &self.capabilities.keys().collect::<Vec<_>>(),
            )
            .field("sessions", &self.sessions.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl HelperState {
    pub fn new(base_url: String) -> Self {
        Self {
            inner: Mutex::new(HelperInner {
                base_url,
                capabilities: HashMap::new(),
                sessions: HashMap::new(),
            }),
        }
    }

    /// Служба CSQTT вызывает при open_challenge: raw capability нужна только для
    /// helper-url API (M6/LuCI), в consume участвует менеджер (M4d).
    pub fn remember_capability(&self, id: &str, token: &str, now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        inner.capabilities.insert(
            id.to_string(),
            (token.to_string(), now + CAPABILITY_RECORD_TTL),
        );
    }

    /// Вымести capability-записи не-live/протухших challenges (служба CSQTT на тиках).
    pub fn prune_capabilities(&self, live_ids: &[String], now: Instant) {
        let mut inner = self.inner.lock().unwrap();
        inner
            .capabilities
            .retain(|id, (_, expires)| live_ids.iter().any(|live| live == id) && now <= *expires);
        inner.sessions.retain(|_, session| now <= session.expires);
    }

    /// Полный helper-URL (LAN + challenge_id + capability) — без секретов VK.
    pub fn helper_url(&self, id: &str) -> Option<String> {
        let inner = self.inner.lock().unwrap();
        let (token, expires) = inner.capabilities.get(id)?;
        if Instant::now() > *expires {
            return None;
        }
        Some(format!("{}/c/{}?cap={}", inner.base_url, id, token))
    }

    /// Открыть сессию после успешного consume: новый одноразовый ticket.
    pub fn open_session(&self, id: &str, redirect_uri: &str, now: Instant) -> String {
        let ticket = random_token();
        let mut hasher = Sha256::new();
        hasher.update(ticket.as_bytes());
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.insert(
            id.to_string(),
            HelperSession {
                ticket_hash: hasher.finalize().into(),
                redirect_uri: redirect_uri.to_string(),
                expires: now + SESSION_TTL,
                submitted: false,
                done: false,
            },
        );
        ticket
    }

    fn with_session<T>(
        &self,
        id: &str,
        ticket: &str,
        now: Instant,
        f: impl FnOnce(&mut HelperSession) -> T,
    ) -> Option<T> {
        let mut hasher = Sha256::new();
        hasher.update(ticket.as_bytes());
        let ticket_hash: [u8; 32] = hasher.finalize().into();
        let mut inner = self.inner.lock().unwrap();
        let session = inner.sessions.get_mut(id)?;
        if session.ticket_hash != ticket_hash || now > session.expires {
            return None;
        }
        Some(f(session))
    }

    /// redirect_uri для /go (после проверки ticket).
    pub fn redirect_for(&self, id: &str, ticket: &str, now: Instant) -> Option<String> {
        self.with_session(id, ticket, now, |session| session.redirect_uri.clone())
    }

    /// Одноразовый приём результата: второй submit той же сессии — false.
    pub fn claim_submit(&self, id: &str, ticket: &str, now: Instant) -> bool {
        self.with_session(id, ticket, now, |session| {
            if session.submitted {
                false
            } else {
                session.submitted = true;
                true
            }
        }) == Some(true)
    }

    /// Одноразовый «прошёл в браузере» (cancel + retry).
    pub fn claim_done(&self, id: &str, ticket: &str, now: Instant) -> bool {
        self.with_session(id, ticket, now, |session| {
            if session.done || session.submitted {
                false
            } else {
                session.done = true;
                true
            }
        }) == Some(true)
    }

    /// Закрыть сессию (challenge умер/решён).
    pub fn close_session(&self, id: &str) {
        let mut inner = self.inner.lock().unwrap();
        inner.sessions.remove(id);
        inner.capabilities.remove(id);
    }
}

fn random_token() -> String {
    let mut buffer = [0u8; 32];
    getrandom::fill(&mut buffer).expect("getrandom: no entropy source available");
    URL_SAFE_NO_PAD.encode(buffer)
}

/// Ответ helper-сервера.
pub struct HttpResponse {
    pub status: u16,
    pub content_type: &'static str,
    pub body: Vec<u8>,
    pub location: Option<String>,
}

impl HttpResponse {
    fn html(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "text/html; charset=utf-8",
            body: body.into_bytes(),
            location: None,
        }
    }

    fn json(status: u16, body: String) -> Self {
        Self {
            status,
            content_type: "application/json",
            body: body.into_bytes(),
            location: None,
        }
    }

    fn redirect(location: String) -> Self {
        Self {
            status: 302,
            content_type: "text/plain; charset=utf-8",
            body: Vec::new(),
            location: Some(location),
        }
    }

    /// Сериализация ответа с заголовками, блокирующими framing/кэш.
    pub fn to_bytes(&self) -> Vec<u8> {
        let reason = match self.status {
            200 => "OK",
            302 => "Found",
            400 => "Bad Request",
            403 => "Forbidden",
            404 => "Not Found",
            405 => "Method Not Allowed",
            408 => "Request Timeout",
            413 => "Payload Too Large",
            _ => "Server Error",
        };
        let mut head = format!(
            "HTTP/1.1 {} {}\r\n\
             Content-Type: {}\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\
             Cache-Control: no-store\r\n\
             Referrer-Policy: no-referrer\r\n\
             X-Frame-Options: DENY\r\n\
             Content-Security-Policy: default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; connect-src 'self'; base-uri 'none'; form-action 'self'\r\n\r\n",
            self.status,
            reason,
            self.content_type,
            self.body.len()
        );
        if let Some(location) = &self.location {
            head = head.replace(
                "Connection: close\r\n",
                &format!("Location: {location}\r\nConnection: close\r\n"),
            );
        }
        let mut out = head.into_bytes();
        out.extend_from_slice(&self.body);
        out
    }
}

fn escape_html(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Страница challenge: только safe fields + ticket. Секретов (session_token,
/// redirect_uri, success token) здесь не бывает — redirect идёт server-side.
pub fn render_page(id: &str, ticket: &str) -> String {
    let id = escape_html(id);
    let ticket = escape_html(ticket);
    format!(
        "<!doctype html><html lang=\"ru\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
         <title>CSQTT — подтверждение</title><style>body{{font-family:system-ui;\
         margin:2em;max-width:38em}}a.button,button{{display:inline-block;padding:.6em\
         1em;margin:.4em 0;border-radius:.5em;text-decoration:none}}</style></head>\
         <body><h1>CSQTT: требуется подтверждение</h1>\
         <p>Challenge: <code>{id}</code></p>\
         <p><a class=\"button\" href=\"/c/{id}/go?t={ticket}\">1. Открыть проверку VK \
         (штатный flow)</a></p>\
         <p>2. Пройдите проверку в браузере. Если получили токен успеха — отправьте \
         его ниже; иначе нажмите «Я прошёл» (профиль повторит попытку).</p>\
         <form method=\"post\" action=\"/c/{id}/result\">\
         <input type=\"hidden\" name=\"t\" value=\"{ticket}\">\
         <textarea name=\"result\" rows=\"2\" cols=\"40\" placeholder=\"токен \
         успеха (опционально)\"></textarea><br>\
         <button type=\"submit\">Отправить результат</button></form>\
         <form method=\"post\" action=\"/c/{id}/done\">\
         <input type=\"hidden\" name=\"t\" value=\"{ticket}\">\
         <button type=\"submit\">Я прошёл капчу в браузере</button></form>\
         <p id=\"state\">Статус: <span id=\"s\">…</span></p>\
         <script>async function poll(){{try{{const r=await fetch('/c/{id}/status?t={ticket}');\
         const j=await r.json();document.getElementById('s').textContent=j.state;}}\
         catch(e){{}}}}setInterval(poll,3000);poll();</script></body></html>"
    )
}

fn error_page(status: u16, message: &str) -> HttpResponse {
    HttpResponse::html(
        status,
        format!(
            "<!doctype html><meta charset=\"utf-8\"><title>CSQTT</title>\
             <h1>{}</h1>",
            escape_html(message)
        ),
    )
}

/// Ответ на маршрут. `peer` — адрес подключившегося (для loopback-only API).
pub async fn dispatch(
    route: Route,
    peer: IpAddr,
    state: &Arc<HelperState>,
    sender: &mpsc::UnboundedSender<DaemonCommand>,
) -> HttpResponse {
    let now = Instant::now();
    match route {
        Route::Index => HttpResponse::html(
            200,
            "<!doctype html><meta charset=\"utf-8\"><title>CSQTT Web Helper</title>\
             <p>CSQTT CAPTCHA Web Helper. Откройте ссылку из уведомления/QR.</p>"
                .to_string(),
        ),
        Route::Open { id, cap } => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if sender
                .send(DaemonCommand::HelperConsume {
                    challenge_id: id.clone(),
                    token: cap,
                    reply: reply_tx,
                })
                .is_err()
            {
                return error_page(500, "служба CSQTT недоступен");
            }
            match reply_rx.await {
                Ok(Some(redirect_uri)) => {
                    let ticket = state.open_session(&id, &redirect_uri, now);
                    HttpResponse::html(200, render_page(&id, &ticket))
                }
                Ok(None) => error_page(
                    403,
                    "capability отклонена: недействительна, просрочена или уже использована",
                ),
                Err(_) => error_page(500, "служба CSQTT не ответил"),
            }
        }
        Route::Go { id, ticket } => match state.redirect_for(&id, &ticket, now) {
            Some(location) => HttpResponse::redirect(location),
            None => error_page(403, "сессия не найдена или просрочена"),
        },
        Route::Status { id, ticket } => {
            if state.redirect_for(&id, &ticket, now).is_none() {
                return error_page(403, "сессия не найдена или просрочена");
            }
            let (reply_tx, reply_rx) = oneshot::channel();
            if sender
                .send(DaemonCommand::HelperStatus {
                    challenge_id: id.clone(),
                    reply: reply_tx,
                })
                .is_err()
            {
                return HttpResponse::json(500, "{\"error\":\"daemon unavailable\"}".to_string());
            }
            let state_str = reply_rx
                .await
                .unwrap_or(None)
                .unwrap_or_else(|| "missing".to_string());
            if matches!(
                state_str.as_str(),
                "solved" | "failed" | "expired" | "cancelled"
            ) {
                state.close_session(&id);
            }
            HttpResponse::json(
                200,
                format!(
                    "{{\"id\":\"{}\",\"state\":\"{}\"}}",
                    escape_json(&id),
                    escape_json(&state_str)
                ),
            )
        }
        Route::Submit { id, ticket, result } => {
            if !state.claim_submit(&id, &ticket, now) {
                return error_page(
                    403,
                    "недействительный, просроченный или использованный ticket",
                );
            }
            if sender
                .send(DaemonCommand::SubmitCaptchaResult {
                    challenge_id: id.clone(),
                    result,
                })
                .is_err()
            {
                return HttpResponse::json(500, "{\"error\":\"daemon unavailable\"}".to_string());
            }
            HttpResponse::json(200, "{\"accepted\":true}".to_string())
        }
        Route::Done { id, ticket } => {
            if !state.claim_done(&id, &ticket, now) {
                return error_page(
                    403,
                    "недействительный, просроченный или использованный ticket",
                );
            }
            // «Прошёл в браузере» без токена: challenge закрывается, профиль
            // повторит попытку (M4d lifecycle). Результат не выдумывается.
            let _ = sender.send(DaemonCommand::CancelCaptcha(id.clone()));
            state.close_session(&id);
            HttpResponse::json(200, "{\"cancelled\":true}".to_string())
        }
        Route::ApiList
        | Route::ApiInfo { .. }
        | Route::ApiHelperUrl { .. }
        | Route::ApiCancel { .. } => {
            // Интеграционный API (M6): loopback-only — helper-url отдаёт
            // raw capability, чужим хостам на LAN он не показывается.
            if !peer.is_loopback() {
                return error_page(403, "API доступен только с loopback (LuCI/rpcd)");
            }
            match route {
                Route::ApiList => snapshot_json(state, sender, None).await,
                Route::ApiInfo { id } => snapshot_json(state, sender, Some(&id)).await,
                Route::ApiHelperUrl { id } => match state.helper_url(&id) {
                    Some(url) => HttpResponse::json(
                        200,
                        format!(
                            "{{\"id\":\"{}\",\"url\":\"{}\"}}",
                            escape_json(&id),
                            escape_json(&url)
                        ),
                    ),
                    None => {
                        HttpResponse::json(404, "{\"error\":\"no capability record\"}".to_string())
                    }
                },
                Route::ApiCancel { id } => {
                    let (reply_tx, reply_rx) = oneshot::channel();
                    if sender
                        .send(DaemonCommand::CaptchaCancel {
                            challenge_id: id.clone(),
                            reply: reply_tx,
                        })
                        .is_err()
                    {
                        return HttpResponse::json(
                            500,
                            "{\"error\":\"daemon unavailable\"}".to_string(),
                        );
                    }
                    let cancelled = reply_rx.await.unwrap_or(false);
                    if cancelled {
                        state.close_session(&id);
                    }
                    HttpResponse::json(
                        if cancelled { 200 } else { 409 },
                        format!("{{\"cancelled\":{cancelled}}}"),
                    )
                }
                _ => unreachable!(),
            }
        }
        Route::NotFound => error_page(404, "не найдено"),
        Route::BadRequest => error_page(400, "некорректный запрос"),
        Route::MethodNotAllowed => error_page(405, "метод не поддерживается"),
    }
}

async fn snapshot_json(
    _state: &Arc<HelperState>,
    sender: &mpsc::UnboundedSender<DaemonCommand>,
    only_id: Option<&str>,
) -> HttpResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    if sender
        .send(DaemonCommand::CaptchaSnapshot { reply: reply_tx })
        .is_err()
    {
        return HttpResponse::json(500, "{\"error\":\"daemon unavailable\"}".to_string());
    }
    let Ok(views) = reply_rx.await else {
        return HttpResponse::json(500, "{\"error\":\"daemon not answered\"}".to_string());
    };
    let items: Vec<String> = views
        .iter()
        .filter(|view| only_id.is_none_or(|id| view.id == id))
        .map(|view| {
            format!(
                "{{\"id\":\"{}\",\"profile\":\"{}\",\"mode\":\"{}\",\"state\":\"{}\"}}",
                escape_json(&view.id),
                escape_json(&view.profile_id),
                escape_json(&view.mode),
                escape_json(&view.state),
            )
        })
        .collect();
    if only_id.is_some() {
        return match items.first() {
            Some(item) => HttpResponse::json(200, item.clone()),
            None => HttpResponse::json(404, "{\"error\":\"no such challenge\"}".to_string()),
        };
    }
    HttpResponse::json(200, format!("[{}]", items.join(",")))
}

fn escape_json(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Прочитать один запрос (заголовки до \\r\\n\\r\\n + тело по Content-Length).
pub async fn read_request(
    stream: &mut tokio::net::TcpStream,
) -> std::io::Result<Option<HttpRequest>> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 2048];
    let head_end = loop {
        if let Some(pos) = find_double_crlf(&buffer) {
            break pos;
        }
        if buffer.len() >= MAX_HEADER_BYTES {
            return Ok(None);
        }
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        buffer.extend_from_slice(&chunk[..read]);
    };
    let head = String::from_utf8_lossy(&buffer[..head_end]).into_owned();
    let mut lines = head.lines();
    let Some(mut request) = parse_request_line(lines.next().unwrap_or_default()) else {
        return Ok(None);
    };
    let mut content_length = 0usize;
    for line in lines {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("content-length") {
            content_length = value.trim().parse().unwrap_or(usize::MAX);
        }
    }
    if content_length > MAX_BODY_BYTES {
        return Ok(None);
    }
    let mut body: Vec<u8> = buffer[head_end + 4..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(None);
        }
        body.extend_from_slice(&chunk[..read]);
    }
    request.body = String::from_utf8_lossy(&body[..content_length.min(body.len())]).into_owned();
    Ok(Some(request))
}

fn find_double_crlf(buffer: &[u8]) -> Option<usize> {
    buffer.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Обработка одного соединения: прочитать, смаршрутизировать, ответить, закрыть.
pub async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    peer: IpAddr,
    state: Arc<HelperState>,
    sender: mpsc::UnboundedSender<DaemonCommand>,
) {
    let response =
        match tokio::time::timeout(Duration::from_secs(10), read_request(&mut stream)).await {
            Ok(Ok(Some(request))) => dispatch(route(&request), peer, &state, &sender).await,
            Ok(Ok(None)) => error_page(400, "некорректный HTTP-запрос"),
            Ok(Err(_)) => error_page(400, "ошибка чтения"),
            Err(_) => error_page(408, "таймаут запроса"),
        };
    let bytes = response.to_bytes();
    let _ = stream.write_all(&bytes).await;
    let _ = stream.flush().await;
    let _ = stream.shutdown().await;
}

/// Accept-цикл helper-сервера. Завершается по cancel.
pub async fn serve(
    listener: TcpListener,
    state: Arc<HelperState>,
    sender: mpsc::UnboundedSender<DaemonCommand>,
    cancel: CancellationToken,
) {
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            accepted = listener.accept() => match accepted {
                Ok((stream, peer)) => {
                    let state = Arc::clone(&state);
                    let sender = sender.clone();
                    tokio::spawn(async move {
                        handle_connection(stream, peer.ip(), state, sender).await;
                    });
                }
                Err(error) => {
                    crate::log_error!("[HELPER] accept: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            },
        }
    }
}

/// Поднять helper-сервер: политика LAN → bind → accept-цикл в фоне.
/// Ошибка bind не валит службу CSQTT: helper просто недоступен (в лог пишет служба CSQTT).
///
/// [M8] При bind на LAN-адрес дополнительно поднимаем loopback:<port>:
/// пользовательская страница должна быть доступна из LAN, а интеграционный
/// `/api/...` — только с loopback (LuCI/rpcd). Без второго слушателя API был бы
/// недостижим при LAN-bind, а с loopback-only — недостижима страница для телефона.
pub async fn spawn_helper(
    listen: SocketAddr,
    state: Arc<HelperState>,
    sender: mpsc::UnboundedSender<DaemonCommand>,
    cancel: CancellationToken,
) -> std::io::Result<tokio::task::JoinHandle<()>> {
    validate_listen_addr(listen).map_err(std::io::Error::other)?;
    let listener = TcpListener::bind(listen).await?;
    let mut tasks = vec![tokio::spawn(serve(
        listener,
        Arc::clone(&state),
        sender.clone(),
        cancel.clone(),
    ))];
    if !listen.ip().is_loopback() {
        let loopback = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), listen.port());
        match TcpListener::bind(loopback).await {
            Ok(loopback_listener) => tasks.push(tokio::spawn(serve(
                loopback_listener,
                state,
                sender,
                cancel.clone(),
            ))),
            Err(error) => crate::log_error!("[HELPER] loopback {loopback} bind: {error}"),
        }
    }
    Ok(tokio::spawn(async move {
        for task in tasks {
            let _ = task.await;
        }
    }))
}

/// Заголовок для unit-тестов: пишет raw-запрос в сокет и читает ответ.
#[cfg(test)]
async fn request_raw(addr: SocketAddr, request: &str) -> String {
    use tokio::io::AsyncReadExt;
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.shutdown().await.unwrap();
    let mut response = Vec::new();
    stream.read_to_end(&mut response).await.unwrap();
    String::from_utf8_lossy(&response).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::captcha_manager::ChallengeView;
    use crate::pool::CaptchaSnapshotView;

    fn base_state() -> Arc<HelperState> {
        Arc::new(HelperState::new("http://192.168.1.1:8443".to_string()))
    }

    /// Мок-служба CSQTT: отвечает на команды helper так же, как Daemon.
    fn mock_daemon(
        challenges: Vec<ChallengeView>,
        consume_ok: bool,
        redirect: &str,
    ) -> mpsc::UnboundedSender<DaemonCommand> {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let redirect = redirect.to_string();
        tokio::spawn(async move {
            while let Some(command) = rx.recv().await {
                match command {
                    DaemonCommand::HelperConsume { reply, .. } => {
                        let _ = reply.send(if consume_ok {
                            Some(redirect.clone())
                        } else {
                            None
                        });
                    }
                    DaemonCommand::HelperStatus { reply, .. } => {
                        let _ = reply.send(Some("opened".to_string()));
                    }
                    DaemonCommand::CaptchaSnapshot { reply } => {
                        let _ = reply.send(
                            challenges
                                .iter()
                                .map(|view| CaptchaSnapshotView {
                                    id: view.id.clone(),
                                    profile_id: view.profile_id.clone(),
                                    mode: view.mode.clone(),
                                    state: view.state.as_str().to_string(),
                                })
                                .collect(),
                        );
                    }
                    DaemonCommand::CaptchaCancel { reply, .. } => {
                        let _ = reply.send(true);
                    }
                    _ => {}
                }
            }
        });
        tx
    }

    fn view(id: &str) -> ChallengeView {
        let now = Instant::now();
        ChallengeView {
            id: id.to_string(),
            profile_id: "p10".to_string(),
            mode: "manual".to_string(),
            state: crate::captcha_manager::ChallengeState::Opened,
            created: now,
            expires: now + Duration::from_secs(200),
        }
    }

    /// Порт CaptchaUriPolicy: 302 только на штатные VK/OK https-домены.
    #[test]
    fn redirect_policy_blocks_open_redirect() {
        for ok in [
            "https://vk.com/captcha?sid=1",
            "https://id.vk.com/captcha?session_token=x",
            "https://oauth.vk.ru/authorize",
            "https://ok.ru/captcha",
            "https://cdn.okcdn.ru/c",
        ] {
            assert!(captcha_uri_allowed(ok), "must allow {ok}");
        }
        for bad in [
            "http://vk.com/captcha",
            "https://evil.com/",
            "https://vk.com.evil.com/c",
            "https://evil.com/?u=https://vk.com/",
            "https://vk.com@evil.com/c",
            "https://notvk.com/c",
            "https://vk.com/captcha\r\nSet-Cookie: x=1",
            "https://vk.com/captcha\nx",
            "https://192.168.1.1/c",
            "",
        ] {
            assert!(!captcha_uri_allowed(bad), "must reject {bad:?}");
        }
    }

    #[test]
    fn listen_policy_rejects_wan_and_unspecified() {
        for bad in [
            "0.0.0.0:8443",
            "8.8.8.8:8443",
            "169.254.1.1:80",
            "[::]:8443",
            "[2001:db8::1]:8443",
        ] {
            let addr: SocketAddr = bad.parse().unwrap();
            assert!(validate_listen_addr(addr).is_err(), "must reject {bad}");
        }
        for good in [
            "192.168.1.1:8443",
            "10.0.0.5:8443",
            "172.16.9.9:8443",
            "127.0.0.1:8443",
            "[::1]:8443",
            "[fd12:3456::1]:8443",
        ] {
            let addr: SocketAddr = good.parse().unwrap();
            assert!(validate_listen_addr(addr).is_ok(), "must allow {good}");
        }
    }

    #[test]
    fn parse_lan_ip_reads_openwrt_network_section() {
        let cfg = "\
config interface 'loopback'
\toption device 'lo'
\toption ipaddr '127.0.0.1'

config interface 'lan'
\toption device 'br-lan'
\toption proto 'static'
\toption ipaddr '192.168.1.1/24'
\toption netmask '255.255.255.0'

config interface 'wan'
\toption proto 'dhcp'
";
        assert_eq!(
            parse_lan_ip(cfg),
            Some(IpAddr::V4(Ipv4Addr::new(192, 168, 1, 1)))
        );
        // Нет lan-секции → None (helper выключится, WAN не открываем).
        assert_eq!(
            parse_lan_ip("config interface 'wan'\n\toption ipaddr '1.2.3.4'\n"),
            None
        );
    }

    #[test]
    fn routes_parse_all_endpoints() {
        let get =
            |target: &str| route(&parse_request_line(&format!("GET {target} HTTP/1.1")).unwrap());
        assert!(matches!(get("/"), Route::Index));
        assert!(matches!(
            get("/c/abc?cap=tok123"),
            Route::Open { id, cap } if id == "abc" && cap == "tok123"
        ));
        assert_eq!(get("/c/abc"), Route::BadRequest);
        assert!(matches!(
            get("/c/abc/go?t=tt"),
            Route::Go { id, ticket } if id == "abc" && ticket == "tt"
        ));
        assert!(matches!(get("/c/abc/status?t=tt"), Route::Status { .. }));
        assert!(matches!(get("/api/challenges"), Route::ApiList));
        assert!(matches!(
            get("/api/challenge/xyz"),
            Route::ApiInfo { id } if id == "xyz"
        ));
        assert!(matches!(
            get("/api/challenge/xyz/helper-url"),
            Route::ApiHelperUrl { .. }
        ));
        assert!(matches!(get("/nope"), Route::NotFound));
        let mut post = HttpRequest {
            method: "POST".to_string(),
            path: "/c/abc/result".to_string(),
            query: String::new(),
            body: "t=tt&result=token%20value".to_string(),
        };
        assert!(matches!(
            route(&post),
            Route::Submit { id, ticket, result }
                if id == "abc" && ticket == "tt" && result == "token value"
        ));
        post.path = "/c/abc/done".to_string();
        post.body = "t=tt".to_string();
        assert!(matches!(route(&post), Route::Done { .. }));
        post.path = "/api/challenge/abc/cancel".to_string();
        post.body = String::new();
        assert!(matches!(route(&post), Route::ApiCancel { id } if id == "abc"));
        let put = parse_request_line("PUT /c/abc HTTP/1.1").unwrap();
        assert_eq!(route(&put), Route::MethodNotAllowed);
    }

    #[test]
    fn percent_decode_handles_plus_and_hex() {
        assert_eq!(percent_decode("a+b"), "a b");
        assert_eq!(percent_decode("%7Ctok"), "|tok");
        assert_eq!(percent_decode("bad%zz"), "bad%zz");
    }

    #[test]
    fn page_and_url_contain_no_secrets() {
        let page = render_page("CH-1", "TICKET-9");
        assert!(page.contains("CH-1"));
        assert!(page.contains("/c/CH-1/go?t=TICKET-9"));
        for secret in ["session_token", "https://id.vk.com", "success"] {
            assert!(!page.contains(secret), "page must not contain {secret}");
        }
        let state = base_state();
        state.remember_capability("CH-1", "CAP-TOKEN", Instant::now());
        let url = state.helper_url("CH-1").unwrap();
        assert_eq!(url, "http://192.168.1.1:8443/c/CH-1?cap=CAP-TOKEN");
        assert!(!url.contains("session"));
    }

    #[test]
    fn html_escaping_blocks_injection() {
        let page = render_page("<script>x</script>", "t\"t");
        assert!(!page.contains("<script>x</script>"));
        assert!(page.contains("&lt;script&gt;"));
        assert!(page.contains("t&quot;t"));
    }

    #[test]
    fn ticket_is_single_use_per_action_and_expiry_bound() {
        let state = base_state();
        let now = Instant::now();
        let ticket = state.open_session("CH-1", "https://id.vk.com/captcha?session_token=S", now);
        assert_eq!(
            state.redirect_for("CH-1", &ticket, now).unwrap(),
            "https://id.vk.com/captcha?session_token=S"
        );
        assert!(state.claim_submit("CH-1", &ticket, now));
        assert!(!state.claim_submit("CH-1", &ticket, now), "replay rejected");
        assert!(
            !state.claim_done("CH-1", &ticket, now),
            "done after submit rejected"
        );
        assert!(
            state
                .redirect_for("CH-1", &ticket, now + SESSION_TTL + Duration::from_secs(1))
                .is_none(),
            "expired ticket rejected"
        );
        assert!(state.redirect_for("CH-1", "wrong", now).is_none());
        assert!(state.redirect_for("CH-2", &ticket, now).is_none());
    }

    #[test]
    fn capability_record_pruned_when_not_live() {
        let state = base_state();
        let now = Instant::now();
        state.remember_capability("CH-1", "CAP", now);
        state.remember_capability("CH-2", "CAP2", now);
        state.prune_capabilities(&["CH-1".to_string()], now);
        assert!(state.helper_url("CH-1").is_some());
        assert!(state.helper_url("CH-2").is_none());
    }

    #[test]
    fn debug_never_leaks_redirect_uri() {
        let state = base_state();
        state.open_session(
            "CH-1",
            "https://id.vk.com/captcha?session_token=SECRET-REDIRECT",
            Instant::now(),
        );
        let debug = format!("{:?}", state.inner.lock().unwrap());
        assert!(!debug.contains("SECRET-REDIRECT"));
    }

    #[tokio::test]
    async fn dispatch_open_rejects_invalid_capability() {
        let state = base_state();
        let sender = mock_daemon(vec![], false, "");
        let response = dispatch(
            Route::Open {
                id: "CH-1".to_string(),
                cap: "bad".to_string(),
            },
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &state,
            &sender,
        )
        .await;
        assert_eq!(response.status, 403);
    }

    #[tokio::test]
    async fn dispatch_full_flow_open_go_submit() {
        let state = base_state();
        let sender = mock_daemon(
            vec![view("CH-1")],
            true,
            "https://id.vk.com/captcha?session_token=SECRET-XYZ",
        );
        let loopback = IpAddr::V4(std::net::Ipv4Addr::LOCALHOST);
        let page = dispatch(
            Route::Open {
                id: "CH-1".to_string(),
                cap: "cap".to_string(),
            },
            loopback,
            &state,
            &sender,
        )
        .await;
        assert_eq!(page.status, 200);
        let html = String::from_utf8(page.body).unwrap();
        assert!(
            !html.contains("SECRET-XYZ"),
            "redirect_uri must stay server-side"
        );
        let ticket = {
            let start = html.find("go?t=").unwrap() + 5;
            html[start..start + 43].to_string()
        };
        let go = dispatch(
            Route::Go {
                id: "CH-1".to_string(),
                ticket: ticket.clone(),
            },
            loopback,
            &state,
            &sender,
        )
        .await;
        assert_eq!(go.status, 302);
        assert_eq!(
            go.location.unwrap(),
            "https://id.vk.com/captcha?session_token=SECRET-XYZ"
        );
        let submit = dispatch(
            Route::Submit {
                id: "CH-1".to_string(),
                ticket: ticket.clone(),
                result: "mock-success-token".to_string(),
            },
            loopback,
            &state,
            &sender,
        )
        .await;
        assert_eq!(submit.status, 200);
        let replay = dispatch(
            Route::Submit {
                id: "CH-1".to_string(),
                ticket,
                result: "mock-success-token".to_string(),
            },
            loopback,
            &state,
            &sender,
        )
        .await;
        assert_eq!(replay.status, 403, "second submit must be rejected");
    }

    #[tokio::test]
    async fn api_is_loopback_only() {
        let state = base_state();
        let sender = mock_daemon(vec![view("CH-1")], true, "");
        let lan = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50));
        let response = dispatch(Route::ApiList, lan, &state, &sender).await;
        assert_eq!(response.status, 403);
        let response = dispatch(
            Route::ApiList,
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &state,
            &sender,
        )
        .await;
        assert_eq!(response.status, 200);
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("\"id\":\"CH-1\""));
        assert!(body.contains("\"state\":\"opened\""));
    }

    #[tokio::test]
    async fn api_helper_url_returns_full_url_only_on_loopback() {
        let state = base_state();
        state.remember_capability("CH-1", "CAP-ABC", Instant::now());
        let sender = mock_daemon(vec![], true, "");
        let lan = IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 50));
        let response = dispatch(
            Route::ApiHelperUrl {
                id: "CH-1".to_string(),
            },
            lan,
            &state,
            &sender,
        )
        .await;
        assert_eq!(response.status, 403);
        let response = dispatch(
            Route::ApiHelperUrl {
                id: "CH-1".to_string(),
            },
            IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &state,
            &sender,
        )
        .await;
        let body = String::from_utf8(response.body).unwrap();
        assert!(body.contains("http://192.168.1.1:8443/c/CH-1?cap=CAP-ABC"));
    }

    #[tokio::test]
    async fn http_server_round_trip_over_loopback() {
        let state = base_state();
        let sender = mock_daemon(vec![], true, "https://id.vk.com/captcha?session_token=S");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server = tokio::spawn(serve(listener, Arc::clone(&state), sender, cancel.clone()));
        let response = request_raw(addr, "GET / HTTP/1.1\r\nHost: x\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("X-Frame-Options: DENY"));
        let response = request_raw(addr, "GET /c/CH-1?cap=zz HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(!response.contains("session_token=S"));
        let response = request_raw(addr, "GET /unknown HTTP/1.1\r\n\r\n").await;
        assert!(response.starts_with("HTTP/1.1 404"));
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
    }

    #[tokio::test]
    async fn oversized_body_is_rejected() {
        let state = base_state();
        let sender = mock_daemon(vec![], true, "");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let cancel = CancellationToken::new();
        let server = tokio::spawn(serve(listener, Arc::clone(&state), sender, cancel.clone()));
        let request = format!(
            "POST /c/CH-1/result HTTP/1.1\r\nContent-Length: {}\r\n\r\nt=x&result=y",
            MAX_BODY_BYTES + 1
        );
        let response = request_raw(addr, &request).await;
        assert!(response.starts_with("HTTP/1.1 400"), "got: {response}");
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(2), server).await;
    }
}
