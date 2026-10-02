# SECURITY — секреты, изоляция, модель угроз

Документ фиксирует правила безопасности порта CSQTT → OpenWrt. Нарушение этих
правил — блокирующий дефект. См. также [`PROJECT_CONTRACT.md`](PROJECT_CONTRACT.md),
[`ARCHITECTURE.md`](ARCHITECTURE.md), [`OPENWRT.md`](OPENWRT.md).

## 1. Категории секретов

| Секрет | Где хранится | Примечание |
|---|---|---|
| `password` (WRAP-ключ) | UCI `server.password` | не читается обратно в UI |
| `vk` (хеши/ссылки) | UCI `server.vk` | одноразовые; могут истекать |
| `vk_js_token` | UCI `server.vk_js_token` | **secret**, implicit-flow access_token |
| CAPTCHA `session_token` / `redirect_uri` / `result` | только `CaptchaManager` (память) | выдаются через одноразовую capability |
| `device_id` | UCI `server.device_id` | **привязан к паролю** — не регенерировать |

Дополнительно секретными считаются любые токены VK/OK/TURN, полученные в
рантайме.

## 2. Правила обращения

- **Никогда** не выводить секреты в: UI (кроме write-only полей), `status.json`,
  логи (файл/syslog), argv/ps, сообщения об ошибках.
- В CLI секретные параметры помечены как `SECRET_OPTION_KEYS` и не эхо-ятся.
- Лог-синк службы CSQTT (`logsink.rs`) прогоняет строки через **redaction** набора
  секретов профиля + fail-closed скраббер `session_token=`; строки
  CAPTCHA wire не пишутся в файл вовсе (`[High-1 AUDIT]`).
- `status.json` содержит только safe-поля (counters/state/коды), без секретов.
- `vk_hash_mode=auto_js` хранит `vk_js_token` только в UCI; поле в UI —
  write-only (пустое значение сохраняет прежнее).

## 3. Изоляция транспортного уровня (M3X)

Служба CSQTT всегда `install_routes=false`, `apply_dns=false`. Он **не**:

- становится default-шлюзом;
- ставит global route/rule/table, half-маршруты (0/1, 128/1) или exclude;
- меняет WAN/main routes, system DNS / dnsmasq;
- трогает firewall/NAT;
- захватывает LAN.

`csqtt0` — только TUN-интерфейс (`IFF_TUN|IFF_NO_PI`). В туннель идёт лишь
трафик процессов, явно привязанных к устройству (`SO_BINDTODEVICE`, у Mihomo —
`interface-name: csqtt0`). Control-plane CSQTT/VK/TURN — через WAN.

`rp_filter`: strict `net.ipv4.conf.csqtt0.rp_filter=1` режет downlink; штатное
значение — `2` (loose). Ядро не пересобирать.

## 4. Web Helper CAPTCHA (M4e)

- Bind **LAN-only**: WAN/`0.0.0.0` отклоняются политикой (`validate_listen_addr`),
  это фатальная ошибка конфигурации.
- Страница — для телефона оператора; loopback API — для LuCI/rpcd.
- Доступ по **одноразовой capability** c TTL; `redirect_uri` проверяется
  политикой доменов (`CaptchaUriPolicy`), replay/expiry/invalid — отклоняются.
- Wire-контракт: `CAPTCHA_SOLVE|mode|redirect_uri|session_token` /
  `CAPTCHA_RESULT|result`; секреты изолированы в `CaptchaManager`.

## 5. Файловые права

| Путь | Права | Содержимое |
|---|---|---|
| `/etc/config/csqtt` | `600` | секреты (password/vk/vk_js_token) |
| `/var/run/csqtt/` | `755` | каталог (читает rpcd/LuCI) |
| `/var/run/csqtt/status.json` | root | safe runtime state |
| `/var/log/csqtt.log` | `600` | логи с redaction |
| `/usr/bin/csqtt`, `/etc/init.d/csqtt` | `755` | исполняемые |

Проверяйте права после установки/бэкапов.

## 6. Модель угроз / non-goals

- Защита **локального** устройства и секретов профиля от утечки в логи/UI/status.
- Гарантия, что CSQTT не ломает сетевую конфигурацию роутера (M3X).
- **Не** является средством защиты от компрометации самого роутера/root или от
  анализа трафика на стороне peer'а/провайдера.
- Обфускация (OBFS/`audio`) — не криптографический контроль доступа.

## 7. Реакция на инцидент

1. Отвязать/сменить `password` и VK-хеши на сервере; `device_id` не переносить.
2. Ротировать `vk_js_token` (получить новый implicit-flow токен).
3. Проверить логи/`status.json`/UI на утечки; при находке — фиксить redaction и
   добавлять регресс-тест.
4. Пересобрать с инкрементом `PKG_RELEASE`, прогнать `TESTING.md` + smoke.
