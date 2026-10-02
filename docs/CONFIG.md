# CONFIG — настройки CSQTT (UCI `/etc/config/csqtt`)

Единицы/диапазоны сверены с `csqtt-openwrt/uci.rs` и `pool.rs`. Internal
значения и их единицы **не переопределять** ради UI.

## Глобальные (`config csqtt 'main'`)

| UCI | UI (RU) | Тип | Ед. | Дефолт | Влияние |
|---|---|---|---|---|---|
| `enabled` | CSQTT включён | bool | — | `0` | Служба/автозапуск туннеля |
| `selection_mode` | Режим выбора профиля | `priority\|manual` | — | `priority` | выбор профиля |
| `active_profile` | Активный профиль | section id | — | `''` | только для `manual` |
| `failover` | Автоматическое переключение | bool | — | `1` | Надёжность |
| `failback` | Автоматический возврат | bool | — | `0` | Надёжность |
| `health_interval` | Интервал проверки | u64 | **сек** | `5` | как часто health-тик |
| `fail_threshold` | Порог отказов | u32 | **раз** | `3` | неудач до unhealthy |
| `success_threshold` | Порог успеха | u32 | **раз** | `2` | успехов до восстановления |
| `cooldown` | Пауза после отказа | u64 | **сек** | `60` | пауза перед новой попыткой |
| `reconnect_delay` | Задержка переподключения | u64 | **сек** | `5` | пауза между попытками |
| `failback_stable_time` | Время стабильности перед возвратом | u64 | **сек** | `60` | стабильность перед failback |
| `health_mode` | Режим проверки состояния | `transport\|data\|both` | — | `both` | сигналы здоровья |
| `health_target` | Доп. цель проверки | string (IP/host) | — | `''` | probe (не делает ACTIVE) |
| `captcha_policy` | Политика CAPTCHA | `failover\|wait` | — | `failover` | при CAPTCHA_REQUIRED |
| `tun_address` | Адрес TUN | string | — | `''` | override адреса `csqtt0` |
| `tun_mtu` | TUN MTU | u16 | **байт** | `1280` | MTU `csqtt0` |
| `dns` | DNS туннеля | string | — | `''` | DNS внутри туннеля |
| `log_level` | Уровень журналирования | `error\|warn\|info\|debug` | — | `info` | детализация лога |
| `log_file` | Файл журнала | path | — | `/var/log/csqtt.log` | куда писать |
| `log_size_kb` | Максимальный размер журнала | u64 | **KiB** | `512` | порог ротации (×1024 байт) |

> `log_level`: значения ровно `error|warn|info|debug` (**trace отсутствует**).
> `dns`: CSQTT **не** меняет системный DNS OpenWrt/dnsmasq.
> Human-readable (UI): `60 сек · 1 мин`, `512 KiB ≈ 0.5 MiB`.

## Маршрутизация (`config csqtt 'routing'`)

| UCI | Значения | Дефолт | Примечание |
|---|---|---|---|
| `mode` | `auto\|none` | `auto` | M3X: interface-only, без route/rule/table; `none` — отладочный алиас |

## Профиль (`config server '<id>'`)

| UCI | UI (RU) | Значения/ед. | Дефолт |
|---|---|---|---|
| `name` | Имя профиля | строка | host |
| `enabled` | Профиль включён | `0\|1` | `0` |
| `priority` | Приоритет | число (**меньше = выше**) | `10` |
| `peer` | Сервер / Peer | `host:port` | — |
| `password` | Пароль | **secret** | — |
| `vk` | VK хеши/ссылки | хеши через запятую или `vk.com/call/join` | — |
| `workers` | Воркеры | **9..126** (эффективно — ближайшее кратное 9) | `18` |
| `obfs` | Обфускация (OBFS) | `audio\|video` | `audio` |
| `turn_transport` | TURN транспорт | `udp\|tcp_tls` (`tcp`→`tcp_tls`) | `udp` |
| `captcha_mode` | Режим CAPTCHA | `auto\|wv\|rjs` | `auto` |
| `fingerprint` | Fingerprint | `chrome\|firefox` | `chrome` |
| `client_ids` | Client IDs | строка (advanced) | `''` |
| `vk_auth_mode` | VK auth mode | `vkcalls\|auto_js` | `vkcalls` |
| `vk_hash_mode` | VK hash mode | `manual\|auto_js` | `manual` |
| `vk_js_token` | VK JS token | **secret** | `''` |
| `device_id` | Device ID | **привязан к паролю** | — |
| `fail_threshold` | Порог отказов (override) | раз; пусто = global | `''` |
| `success_threshold` | Порог успеха (override) | раз; пусто = global | `''` |
| `cooldown` | Пауза (override) | сек; пусто = global | `''` |
| `captcha_policy` | CAPTCHA policy (override) | пусто = global | `''` |
| `note` | Заметка | строка | `''` |

> `workers`: можно выбрать любое число **9..126**; клиент работает группами по 9,
> поэтому эффективное значение округляется к ближайшему кратному 9 (50 → 54).
> Больше воркеров может повысить пропускную способность, но растит CPU/RAM/
> латентность. **Не ставьте MAX без измерений** (throughput/CPU/RAM/temp/ping).
> `device_id`: не менять/не регенерировать без отвязки на сервере.
> `password`/`vk_js_token`: не считываются обратно; пустое поле сохраняет значение.
> `auto_js` (`vk_hash_mode=auto_js` + `vk_auth_mode=auto_js`, обязательны вместе):
> креды берутся из `vk_js_token` (VK user access_token), поле `vk` игнорируется —
> протухшие/невалидные хеши перестают влиять. Токен — **secret**; получают
> implicit-flow-авторизацией (например, https://vkhost.github.io/). В LuCI
> редакторе профиля есть кнопка **Check hashes** (валидация + подсветка рабочих).

## Приоритет/наследование

- `selection_mode=priority`: активный профиль выбирается автоматически по
  `priority` (tie-break — порядок секций).
- `selection_mode=manual`: используется `active_profile`.
- Overrides профиля (`fail_threshold`/`success_threshold`/`cooldown`/
  `captcha_policy`) имеют приоритет над глобальными; пустые — берут глобальные.
