# CAPTCHA — полный контракт (M4d + M4e)

Точка правды по протоколу: `csqtt-main/rust-client/captcha.rs` (upstream v2.1.9).
Детальные ranges референсов — `docs/reference/M4d.md`, `docs/reference/M4e.md`.

## 1. Wire contract (не меняется)

```text
stdout:  CAPTCHA_SOLVE|<mode>|<redirect_uri>|<session_token>   (mode = auto|selected|manual)
stdin:   CAPTCHA_RESULT|<result>                               (result = success_token или error:…)
```

Request-id в wire-format нет (контракт upstream). Внутренний daemon challenge-id —
только mapping службы CSQTT. `redirect_uri`/`session_token`/`result` — SECRET: не в лог,
не в status.json, не в QR/URL.

## 2. Rust v2 flow (авто-решатель, ядро `captcha.rs` — не тронуто)

`CaptchaSolver` AUTO-цепочка порта (focsq-вариант, сохранён M4d): Rust v2 ×1 →
WBV Auto 45с → Manual 60с. Rust v2 (`CaptchaSession::solve_once`):

1. GET `redirect_uri` (страница `id.vk.com/captcha?session_token=…`) → HTML;
2. `parse_page`: pow_input/pow_difficulty/script_url/debug_info/window.init
   (PoW-fallback через `captchaNotRobot.initSession` — focsq-улучшение);
3. PoW (`solve_pow`, cpu_task) → `captchaNotRobot.settings`;
4. `initSession` → show_captcha_type (checkbox|slider) + slider settings;
5. checkbox: `componentDone` → `perform_check` (живая cursor-траектория) →
   `response.status=ok` + `success_token`; slider: `captcha_slider::solve_slider`;
6. `endSession`; `success_token` → решатель возвращает его наверх →
   control-линия `CAPTCHA_RESULT|<success_token>`.

## 3. Android WebView host (upstream; фактический mechanism completion)

Хосты: `CaptchaWebViewManager.kt` (auto/selected WV) и `ManlCaptchaActivity.kt`
(manual). Цикл:

1. `TunnelManager.kt:1157–1185` ловит `CAPTCHA_SOLVE`, режет по `|` (3 части;
   legacy 2 части → mode="selected"), битая строка → `CAPTCHA_RESULT|error:invalid…`;
2. `CaptchaUriPolicy.isAllowed`: только https + vk.com/vk.ru/ok.ru/okcdn.ru;
3. WebView (JS+DOM storage) грузит `redirect_uri`; на каждой странице
   (`onPageStarted`/`onPageFinished`) инжектится **interceptor JS**:
   обёртки `fetch` и `XMLHttpRequest` ловят ответ URL, содержащего
   `captchaNotRobot.check`, и из JSON читают `data.response.success_token` →
   `window.CsqttCaptcha.onSuccess(token)`; slider → `onSliderDetected`;
   `data.error` → `onError`;
4. `@JavascriptInterface onSuccess(token)` → `writeCaptchaResult` →
   stdin Rust: `CAPTCHA_RESULT|<success_token>`;
5. навигация вне VK-доменов блокируется (`shouldOverrideUrlLoading`), и это НЕ
   мешает решению — **доказательство, что completion только JS-callback**,
   URL-redirect с токеном в VK flow отсутствует;
6. manual-хост: уведомление → Activity с WebView; клик крестика VK =
   `onCancelAndStop` (отмена + стоп туннеля). Auto-хост дополнительно сам
   кликает чекбокс (MotionEvent-симуляция) и детектит slider → `error:slider…`.

Cookies: WebView использует собственный профиль (domStorageEnabled); VK-сессия
пользователя Android не требуется — challenge привязан к `session_token` в URL.

## 4. Local Web Helper (M4e, `captcha_helper.rs`) — human fallback в обычном браузере

Поверх CaptchaManager (M4d); wire ядра не трогается.

### URL/QR

```text
http://<LAN-адрес роутера>:8443/c/<challenge_id>?cap=<capability>
```

Только: LAN URL + safe challenge_id + одноразовая capability (256-bit, TTL 180с).
НИ session_token, ни password, ни device-id, ни VK hashes. QR рисует UI (M6/LuCI)
из этой строки; сама служба CSQTT QR не генерирует.

### Flow

1. `GET /c/<id>?cap=…` → служба CSQTT гасит capability (`HelperConsume` →
   `consume_capability`: single-use/TTL/state; id из URL обязан совпасть с
   challenge токена) → страница **без секретов** с одноразовым ticket
   (256-bit, хранится SHA-256, TTL=CHALLENGE_TTL);
2. «Открыть проверку VK» → `GET /c/<id>/go?t=…` → **302 server-side** на штатный
   `redirect_uri` (секрет не попадает в HTML/лог; браузер видит его сам — как в
   WebView-хосте upstream). 302 защищён портом `CaptchaUriPolicy`
   (`captcha_uri_allowed`): только https + vk.com/vk.ru/ok.ru/okcdn.ru и
   поддомены, без управляющих символов — open-redirect и header-injection
   исключены даже при враждебном wire-источнике;
3. пользователь проходит чекбокс/slider в обычном браузере (реальный
   fingerprint Safari — именно в этом смысл human-fallback);
4. завершение (см. §5):
   - есть success_token (Android-helper/расширение/вставка) → `POST /c/<id>/result`
     (ticket+result в теле формы, не в URL) → `SubmitCaptchaResult` → обычный
     двухфазный мост M4d → `CAPTCHA_RESULT|…` решателю ядра;
   - «Я прошёл» без токена → `POST /c/<id>/done` → CancelCaptcha → профиль
     повторяет попытку (M4d lifecycle: новый challenge/session_token).
5. статус — `GET /c/<id>/status?t=…` (safe state JSON), страница опрашивает сама.

### Безопасность

- **LAN-only**: bind вне RFC1918/loopback/ULA отклоняется (`validate_listen_addr`);
  0.0.0.0, публичные IPv4/IPv6, link-local — отказ. WAN exposure невозможен.
- **CSRF/replay**: cookie нет вообще; авторизация только секретом в URL/теле;
  capability single-use (M4d); ticket одноразовый на submit/done, bound к
  challenge, TTL; повтор/чужой/просроченный → 403.
- **Secrets**: redirect_uri/session_token/result — только память; Debug-редакция
  (`HelperInner`, `DaemonCommand`); CSP `default-src 'none'`, `X-Frame-Options: DENY`,
  `Referrer-Policy: no-referrer`, `Cache-Control: no-store`, `form-action 'self'`.
- iframe НЕ используется как способ встраивания VK-страницы (CSP/X-Frame-Options/
  SameSite/CORS на id.vk.com + iOS Safari) — только top-level navigation.

### Интеграционный API для M6 (LuCI/rpcd, loopback-only)

```text
GET  /api/challenges                      → list (safe fields)
GET  /api/challenge/<id>                  → info (safe fields)
GET  /api/challenge/<id>/helper-url       → полный URL с capability (для QR)
POST /api/challenge/<id>/cancel           → отмена с подтверждением
```

С тем же контрактом внутри процесса: `DaemonCommand::{CaptchaSnapshot,
HelperStatus, CaptchaCancel, HelperConsume}`. С LAN-адреса API отклоняется (403).

## 5. Browser matrix и технический предел Safari (пункт 11)

| Браузер | Роль | Статус |
|---|---|---|
| Safari iOS | основной target | **PASS (инфраструктура)**: helper-URL/QR, capability, 302 на штатный VK flow, страница/опрос статуса — работают в любом браузере. **Автономный capture success_token — DOCUMENTED SKIP**: см. причину ниже. |
| Desktop Chrome/Firefox/Edge | минимум один PASS | PASS: то же + ручная вставка токена из DevTools Network (`captchaNotRobot.check` → response.success_token) → submit-endpoint. Матрица проверялась на loopback-стенде (юнит-тесты HTTP-цикла); живой desktop-прогон — M8. |
| Chrome Android | PASS или documented SKIP | documented SKIP (нет устройства в стенде M4e); код-path идентичен desktop. |
| Safari macOS | при наличии | documented SKIP (нет устройства); идентично iOS по helper-пути. |

**Точная техническая причина, почему обычный Safari не может сам вернуть токен:**

- upstream completion = чтение JSON-ответа `captchaNotRobot.check` **изнутри
  страницы vk.com** (JS-интерцептор WebView). В обычном браузере это запрещено
  same-origin policy: страница helper'а (LAN origin) не видит ни DOM, ни
  сетевые ответы cross-origin vk.com;
- инъекция JS в vk.com невозможна без расширения (Safari Content Blocker API
  не даёт читать тело ответов, а «обязательное приложение» запрещено контрактом);
- iframe-перехват невозможен: id.vk.com шлёт CSP/X-Frame-Options, плюс
  cross-origin, плюс iOS Safari ограничения (пункт 5 промпта);
- URL-redirect с токеном у VK captcha flow отсутствует — доказательство:
  WebView-хост upstream блокирует ВСЕ переходы вне VK-доменов
  (`shouldOverrideUrlLoading = !isAllowed`) и корректно получает результат
  только через JS-callback (§3.5).

**Evidence**: `csqtt-main/app/.../CaptchaWebViewManager.kt:60–118,243–246`,
`ManlCaptchaActivity.kt:28–76,201–217`, `CaptchaUriPolicy.kt:8–17`
(полные ranges — `docs/reference/M4e.md`).

**Следствие**: отдельное iOS-приложение НЕ делается (не доказана необходимость:
solve-in-browser + retry/done-путь закрывают human-fallback). Android WebView
helper остаётся OPTIONAL автоматизированным доводочным каналом (он умеет
перехватывать токен) — его submit идёт через тот же `/result` endpoint.
Финальная живая проверка VK flow на WH3000 Pro + iPhone — M8.

## 6. Failover-семантика (пункт 12)

Пока A в CAPTCHA_REQUIRED (слот свободен — M4d), B может быть ACTIVE. Результат
A, присланный helper, доставляется ядру только если профиль challenge активен;
иначе `fail_stale` — B не обрывается (тест
`helper_submit_for_non_active_profile_does_not_rip_active_backup`).

## 7. Ручной browser checklist (для M8/оператора)

1. `csqtt run --helper-listen 192.168.1.1:8443` (или UCI-конфиг M5);
2. дождаться `csqtt captcha list` / status.json: state=pending;
3. LuCI (M6d) или `GET /api/challenge/<id>/helper-url` (loopback) → QR/URL;
4. открыть на iPhone (тот же LAN), пройти 302, решить капчу в Safari;
5. вариант a: вернуться на страницу helper → «Я прошёл» (retry профиля);
   вариант b: вставить success_token в форму (Android-helper/расширение);
6. проверить: `csqtt captcha list` → solved; профиль A поднимется при
   следующей попытке; B (ACTIVE) не тронут.
