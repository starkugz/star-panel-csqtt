# ARCHITECTURE — устройство проекта CSQTT → OpenWrt

## Поток данных

```
                 LuCI JS (htdocs/.../view/csqtt/*.js, i18n po/)
                        │  rpc.declare → ubus
                        ▼
        rpcd-ucode backend  (root/usr/share/rpcd/ucode/csqtt)  [ubus: csqtt]
                        │  exec (popen + shellquote, timeout)
                        ▼
        /usr/bin/csqtt  (Rust/tokio, focsq core v2.1.9)
           ├─ profile pool / selection / failover / failback / health
           ├─ CAPTCHA manager + Web Helper
           └─ TUN csqtt0 (interface-only)
                        │
                        ▼
        csqtt0 ──► Mihomo / user proxy (SO_BINDTODEVICE, interface-name: csqtt0)
```

Runtime state: `/var/run/csqtt/status.json` (atomic write). Конфиг:
`/etc/config/csqtt` (UCI). Логи: `/var/log/csqtt.log` + syslog.

## Компоненты и ответственность

| Компонент | Путь | Ответственность |
|---|---|---|
| Rust core/daemon | `csqtt-openwrt/*.rs` | туннель, профили, failover/health, CAPTCHA, CLI, status.json |
| TUN-слой | `csqtt-openwrt/tun_linux.rs` | `/dev/net/tun`, адрес/MTU/UP; `RoutingPolicy` (в службе CSQTT OFF) |
| Пул/failover | `csqtt-openwrt/pool.rs` | selection, thresholds, cooldown, status |
| UCI-схема | `csqtt-openwrt/uci.rs` | типы/валидация main/routing/server, client_config |
| CAPTCHA/Helper | `captcha*.rs` | solver (upstream), manager, LAN-only Web Helper |
| LuCI frontend | `openwrt/luci-app-csqtt/htdocs/.../view/csqtt/*.js` | 5 вью, только представление + `_()` |
| rpcd backend | `openwrt/luci-app-csqtt/root/usr/share/rpcd/ucode/csqtt` | ubus-объект `csqtt`, bridge к CLI/helper |
| ACL/menu | `.../root/usr/share/rpcd/acl.d`, `.../usr/share/luci/menu.d` | права и меню |
| UCI шаблон | `openwrt/etc/config/csqtt` | дефолты `main`/`routing` |
| init.d | `openwrt/etc/init.d/csqtt` | procd, doctor-ворота, SIGHUP-reload, статус |
| i18n | `openwrt/luci-app-csqtt/po/{templates/csqtt.pot,ru/csqtt.po}` | EN source → RU |
| Пакеты | `openwrt/csqtt/Makefile`, `openwrt/luci-app-csqtt/Makefile` | .apk (prebuilt-staging) |
| Тесты | `openwrt/tests/{openwrt,luci,packages}` | M5/M6/M7 |
| Сборка/релиз | `scripts/{build-all,build-sdk,release,pack,smoke-test}.sh` | конвейер |
| Артефакты | `dist/*.apk`, `dist/SHA256SUMS` | релизы |

## Ключевые подсистемы

- **selection_mode**: `priority` — автоматический выбор лучшего enabled по
  priority; `manual` — только `active_profile`.
- **failover/failback**: при деградации активного профиля — переход на
  следующий; при восстановлении более приоритетного и `failback=1` — возврат
  после `failback_stable_time`.
- **health**: `health_mode` = `transport` | `data` | `both`; сигналы из событий
  службы CSQTT (`__CSQTT_EVENT__|…`), тики `health_interval`; пороги
  `fail_threshold`/`success_threshold`, `cooldown`.
- **status.json**: единственный runtime state; читается CLI `csqtt status`,
  LuCI и rpcd. Секретов не содержит.
- **CAPTCHA**: upstream `CaptchaSolver` (wire `CAPTCHA_SOLVE`/`CAPTCHA_RESULT`),
  поверх — `CaptchaManager` (challenge-id/capability/TTL) и LAN-only Web Helper
  (страница для телефона + loopback API для LuCI/rpcd).
- **i18n**: JS-строки только через `_()`; EN source + `po/ru`. Кириллица вне
  комментариев в JS запрещена.

## Изоляция (M3X)

Служба CSQTT всегда `install_routes=false`, `apply_dns=false`. Никаких
route/rule/table/DNS/firewall изменений; `csqtt0` — только TUN-интерфейс
(`IFF_TUN|IFF_NO_PI`). Связка с прокси — на стороне пользователя.
