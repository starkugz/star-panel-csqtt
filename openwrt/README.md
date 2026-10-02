# CSQTT клиент для OpenWrt (Huasifei WH3000 Pro / MT7981 aarch64_cortex-a53)

Порт клиента CSQTT (github.com/amurcanov/csqtt) для роутеров OpenWrt.
Файлы этого каталога — интеграция в систему (M5); сборка .apk-пакета — M7.

## Модель работы (контракт M3X/M4)

Служба CSQTT `csqtt run` поднимает TUN-интерфейс `csqtt0` (фиксированное имя) и
работает **только как интерфейс**: он НЕ становится default-шлюзом, НЕ
ставит маршрутов/правил/таблиц, НЕ меняет системный DNS/dnsmasq, файрвол и
NAT. Через `csqtt0` идёт трафик процессов, явно привязанных к интерфейсу
(например, mihomo `interface-name: csqtt0`). Control-plane (CSQTT/VK/TURN)
остаётся через WAN. Runtime-состояние — `/var/run/csqtt/status.json`
(не UCI); `csqtt status`/LuCI читают его.

## Файлы

| Файл | Назначение |
|---|---|
| `etc/config/csqtt` | UCI-конфиг (схема M4a): main + routing + профили `server`. Дефолт `enabled='0'` |
| `etc/init.d/csqtt` | procd-скрипт: создание runtime-каталога `/var/run/csqtt`, doctor-ворота, `csqtt run`, respawn, SIGHUP-reload, `status`/`doctor` команды |
| `etc/logrotate.d/csqtt` | запасной недельный слой ротации (размерную делает сама служба CSQTT, M4a) |
| `tests/openwrt/run.sh` | интеграционный тест файлов (моки procd/uci, shellcheck, негативная проверка изоляции) |

`etc/csqtt.conf` — переходный env-file (v0–M3R). С M5 больше НЕ читается
init-скриптом; оставлен как запись прежних значений (device-id, peer,
валидные VK-хеши) до переноса в UCI на устройстве.

## Установка (OpenWrt 25.12.x, apk)

```sh
apk update && apk add kmod-tun

scp csqtt-aarch64 root@192.168.1.1:/tmp/csqtt
scp etc/config/csqtt   root@192.168.1.1:/etc/config/csqtt
scp etc/init.d/csqtt   root@192.168.1.1:/etc/init.d/csqtt
scp etc/logrotate.d/csqtt root@192.168.1.1:/etc/logrotate.d/csqtt

install -m 755 /tmp/csqtt /usr/bin/csqtt
chmod +x /etc/init.d/csqtt
```

Профили: `csqtt profile import 'csqtt://connect?…' --commit` (одноразовые
VK-хеши расходует сервер при подключении) или вручную секцией
`config server '<id>'` в `/etc/config/csqtt`. device-id переносить как есть —
он привязан к паролю на сервере, НЕ генерировать новый.

Включение:

```sh
uci set csqtt.main.enabled='1'   # или правка etc/config/csqtt
/etc/init.d/csqtt enable
/etc/init.d/csqtt start
logread -e csqtt
```

После правки конфига: `/etc/init.d/csqtt reload` (служба CSQTT перечитает UCI по
SIGHUP, `csqtt0` не рвётся) или авто-триггер на `uci commit csqtt`.

## Обслуживание

```sh
/etc/init.d/csqtt status     # = csqtt status (0 подключён / 1 нет / 2 служба CSQTT не запущена)
/etc/init.d/csqtt doctor     # read-only диагностика конфига и окружения
csqtt log tail -n 50 -f      # лог с маскировкой секретов
```

`doctor` перед стартом: FAIL (невалидный конфиг / нет /dev/net/tun) —
запуск отклоняется; WARN (peer молчит, leftover `csqtt0`, rp_filter=1) —
старт разрешён, детали в выводе. rp_filter=1 лечится per-interface
sysctl (`net.ipv4.conf.csqtt0.rp_filter=2`) — это настройка системы, не службы CSQTT.

## Проверка изоляции

```sh
ip route          # default — по-прежнему через WAN; полумаршрутов нет
uci show network  # не менялся
logread -e csqtt  # Tunnel IP, статус воркеров
```

## Сборка из исходников

Каталог `../csqtt-openwrt/` — исходники (ядро csqtt-core v2.1.9 + CLI).

```sh
cargo build --release --target aarch64-unknown-linux-musl --bin csqtt
```

Требуется Rust >= 1.97.1 и тулчейн aarch64-linux-musl-gcc (для aws-lc-sys).
Тесты: `bash scripts/run-tests.sh` (fmt/clippy/unit + openwrt-интеграция).

## Лицензия

PolyForm-Noncommercial-1.0.0 (© amurcanov, © luminescq). Только
некоммерческое использование.
