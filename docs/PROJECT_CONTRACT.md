# PROJECT_CONTRACT — краткий неизменяемый контекст

## Проект

CSQTT → OpenWrt для Huasifei WH3000 Pro: MediaTek Filogic MT7981, ARM64 Cortex-A53 ×2, OpenWrt 25.12.x, `apk`, `aarch64_cortex-a53`, современный LuCI (JS + menu.d + rpcd-ucode).

Рабочий порт: `./csqtt-openwrt/` — Rust/tokio, core v2.1.9 на базе focsq + CLI. Референсы `./csqtt-main/` и `./focsq-main/` — READ-ONLY.

## Закрыто

M0–M3X закрыты. M3R/M3X baseline: 333 passed / 0 failed / 7 ignored.

M3X доказал:
- production = interface-only;
- `SO_BINDTODEVICE` / mihomo `interface-name: csqtt0` работает без route/rule/table;
- routing layer не нужен и в M4+ не добавляется;
- downlink: `rp_filter=0/2` OK, strict `1` DROP; route/rule это не лечит;
- live Mihomo gate на WH3000 Pro перенесён в M8 из-за SSH password-auth.

## Сетевая изоляция

Daemon не становится default gateway, не меняет WAN/main routes, system DNS/dnsmasq, firewall/NAT и не захватывает LAN. Через `csqtt0` идёт только трафик процессов, явно настроенных на интерфейс. Control-plane CSQTT/VK/TURN остаётся через WAN.

`csqtt0` — фиксированное имя. Native Linux TUN: `/dev/net/tun`, `IFF_TUN|IFF_NO_PI`.

## Зафиксированные технические решения

- `MAX_WORKERS=126`.
- musl-фиксы `tun_ioctl`, `udp_batch` mmsg/msghdr и clap `help` не откатывать.
- статическая сборка: `aarch64-unknown-linux-musl`; WSL+musl.cc или эквивалентный rust-musl-cross.
- UCI `/etc/config/csqtt` начинается в M4a; procd — M5; LuCI без Lua controllers.
- `CSQTT_EVENTS=1` machine events используются для runtime state; человеческие logs не парсить как state.
- SIGTERM → CancellationToken → clean teardown — задача M4b.
- Runtime state будущего daemon — `/var/run/csqtt/status.json`, не UCI.

## CAPTCHA

Upstream `CaptchaSolver` не переписывать с нуля. Wire contract:

```text
CAPTCHA_SOLVE|mode|redirect_uri|session_token
CAPTCHA_RESULT|result
```

В wire-format нет request-id. Внутренний daemon challenge-id допустим только как mapping. Secrets не логировать. Safari/iPhone — основной human fallback; Web Helper — M4e.

## Порядок

`M4a → M4b → M4c → M4d → M4e → M5 → M6a → M6b → M6c → M6d → M7 → M8`.

Один подмодуль = один новый чат. После PASS текущего модуля остановиться.
