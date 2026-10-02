# star-panel-csqtt

Клиент **CSQTT** для роутеров на **OpenWrt 25.12.x** (`apk`), архитектура
`aarch64_cortex-a53` (ARM64 Cortex-A53). Ядро CSQTT v2.1.9, интерфейс — LuCI
(персональная редакция **star-panel-csqtt** 1.0.0).

`csqtt0` — изолированный **interface-only** туннель: служба CSQTT не становится
default-шлюзом и не меняет маршруты/DNS/firewall. Трафик в туннель направляет
пользовательский прокси (например, Mihomo или ssclash).

# Возможности

- Профильный пул: переключение при сбое (failover), возврат (failback),
  приоритеты, cooldown/reconnect.
- Health-контроль (`transport` / `data` / `both`), пороги, интервал проверки.
- Две схемы авторизации VK: `vkcalls` (хеши `vk.com/call/join`) и `auto_js`
  (по VK access-token — хеши не нужны).
- Воркеры `9..126` (кратно 9), обфускация `audio` / `video`, TURN `udp` / `tcp_tls`.
- CAPTCHA: встроенный solver + LAN-only Web Helper (страница для телефона).
- LuCI: 6 страниц — Status / Profiles / Settings / Logs / CAPTCHA / About,
  русский интерфейс, кнопка **Check hashes** (подсветка рабочих хешей).
- Секреты (`password`, `vk_js_token`) — write-only: в интерфейс, статус и логи
  не выводятся.
- Изоляция (M3X): только TUN `csqtt0`, без route/rule/table, DNS, firewall/NAT.

# Autoinstall script

Устанавливает пакеты CSQTT (ядро + LuCI + русский перевод) и зависимости.

```sh
wget --no-proxy -qO- https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/install-csqtt.sh | ash
```

# Manual install

## Step 1: Update Package List

```sh
apk update
```

## Step 2: Download and Install Packages

Пакеты — в каталоге [`dist/`](dist).

```sh
cd /tmp
BASE=https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/dist
wget --no-proxy -q $BASE/csqtt_2.1.9_aarch64_cortex-a53.apk
wget --no-proxy -q $BASE/luci-app-csqtt_2.1.9_all.apk
wget --no-proxy -q $BASE/luci-i18n-csqtt-ru_all.apk
apk add --allow-untrusted \
  csqtt_2.1.9_aarch64_cortex-a53.apk \
  luci-app-csqtt_2.1.9_all.apk \
  luci-i18n-csqtt-ru_all.apk
```

`--allow-untrusted` нужен, потому что пакеты собраны локально и не подписаны
ключом репозитория OpenWrt. Зависимости (`kmod-tun`, `luci-base`,
`rpcd-mod-ucode`, `ucode-mod-socket`, `coreutils-timeout`) `apk` подтянет из
официального репозитория.

## Step 3: Start

```sh
csqtt profile import 'csqtt://…' --commit --activate
uci set csqtt.main.enabled='1'
uci commit csqtt
/etc/init.d/csqtt enable
/etc/init.d/csqtt start
```

Служба **не включается автоматически**: дефолт `enabled='0'`.

# Настройка

Настройка доступна в LuCI (**Службы → star-panel-csqtt**) или через UCI.

## Через LuCI

- **Status** — состояние профиля и `csqtt0`, кнопки запуска/остановки.
- **Profiles** — добавить сервер (`peer`, `password`, VK) и включить профиль;
  импорт/экспорт ссылки `csqtt://`, QR, **Check hashes**.
- **Settings** — глобальные параметры: failover/failback, health, CAPTCHA.
- **Logs** — журнал службы и очистка.
- **CAPTCHA** — активные задачи и ссылка/QR для Web Helper.

## Через UCI

```sh
# профиль из ссылки csqtt:// (VK-хеши)
csqtt profile import 'csqtt://…' --commit --activate

# или вручную
uci set csqtt.p1=server
uci set csqtt.p1.peer='198.51.100.10:46000'
uci set csqtt.p1.password='<пароль>'
uci set csqtt.p1.vk='hash1,hash2'
uci set csqtt.p1.enabled='1'
uci commit csqtt
```

## Авторизация VK

- **`vkcalls`** (по умолчанию) — VK-хеши `vk.com/call/join` в поле `vk`;
  хеши одноразовые и могут истекать (кнопка **Check hashes**).
- **`auto_js`** — по VK access-token в поле `vk_js_token`, хеши не нужны:

```sh
uci set csqtt.p1.vk_auth_mode='auto_js'
uci set csqtt.p1.vk_hash_mode='auto_js'
uci set csqtt.p1.vk_js_token='<VK access token>'
uci commit csqtt
/etc/init.d/csqtt restart
```

Токен — секрет и имеет срок жизни; получают implicit-flow-авторизацией (ссылка
с токеном из redirect-URL), например через
[`vkhost.github.io`](https://vkhost.github.io/) — приложение с доступом к звонкам.

## Основные параметры

| Параметр | Значения | Дефолт |
|---|---|---|
| `main.enabled` | `0` / `1` | `0` |
| `main.selection_mode` | `priority` / `manual` | `priority` |
| `main.failover` / `main.failback` | авто-переключение / возврат | `1` / `0` |
| `main.health_interval` | сек | `5` |
| `main.health_mode` | `transport` / `data` / `both` | `both` |
| `main.captcha_policy` | `failover` / `wait` | `failover` |
| `server.peer` | `host:port` | — |
| `server.workers` | `9..126` (кратно 9) | `18` |
| `server.obfs` | `audio` / `video` | `audio` |
| `server.turn_transport` | `udp` / `tcp_tls` | `udp` |
| `server.fingerprint` | `chrome` / `firefox` | `chrome` |

Шаблон конфигурации — `/etc/config/csqtt`. Полный перечень параметров — на
странице **Настройки** в LuCI.

# LuCI

Интерфейс — **Службы → star-panel-csqtt**, 6 страниц:

- **Status** — состояние активного профиля и интерфейса `csqtt0`, счётчики
  трафика, кнопки Start / Stop / Restart.
- **Profiles** — список серверов, добавление и редактирование (в т.ч. advanced:
  `vk_auth_mode`, `vk_hash_mode`, `vk_js_token`), импорт/экспорт ссылки `csqtt://`,
  QR-код, кнопка **Check hashes** (валидация и подсветка рабочих хешей).
- **Settings** — глобальные параметры: failover/failback, health, CAPTCHA,
  логирование, `routing`.
- **Logs** — журнал `/var/log/csqtt.log`: хвост, обновление, очистка.
- **CAPTCHA** — активные задачи, отмена, ссылка и QR для Web Helper (решение
  на телефоне в LAN).
- **About** — версия редакции (`star-panel-csqtt`) и версия ядра (живая из
  `status.json`), автор редакции, исходный проект, лицензия и уведомления.

Секреты (`password`, `vk_js_token`) — write-only: в интерфейсе, статусе и логах
не отображаются; пустое поле сохраняет прежнее значение.

# Routing (Mihomo / ssclash)

CSQTT не трогает маршруты. Трафик в туннель направляется привязкой к устройству —
подходит для Mihomo и [ssclash](https://github.com/zerolabnet/SSClash/):

```yaml
proxies:
  - name: CSQTT
    type: direct
    interface-name: csqtt0
    udp: true
```

# Remove

```sh
/etc/init.d/csqtt stop
/etc/init.d/csqtt disable
apk del luci-app-csqtt csqtt
```

# License

PolyForm Noncommercial License 1.0.0 — см. [`LICENSE`](LICENSE).
Только некоммерческое использование.

> Required Notice: Copyright 2026 amurcanov
