# Установка CSQTT на OpenWrt (aarch64_cortex-a53)

Пакеты `.apk` собираются одной командой `sh scripts/build-all.sh` (M7) и
складываются в `dist/`. Версия пакета интеграции и панели — **1.0.0**;
встроенное ядро — **2.1.9** (см. [`RELEASE.md`](RELEASE.md)).

| Файл | Содержимое |
|---|---|
| `csqtt_1.0.0_aarch64_cortex-a53.apk` | служба CSQTT `/usr/bin/csqtt` (ядро 2.1.9) + `/etc/init.d/csqtt` + `/etc/config/csqtt` (CONFFILES) + `/etc/logrotate.d/csqtt` |
| `luci-app-csqtt_1.0.0_all.apk` | LuCI: вью, `menu.d`, `rpcd/ucode/csqtt`, `rpcd/acl.d` |
| `luci-i18n-csqtt-ru_all.apk` | русский перевод (`csqtt.ru.lmo`) — опционально |
| `SHA256SUMS` | контрольные суммы всех `.apk` |

Зависимости `csqtt`: `+kmod-tun` (модуль TUN). `ip-full` НЕ требуется —
routing layer отсутствует (итог M3X). `luci-app-csqtt`: `+csqtt +luci-base
+rpcd-mod-ucode +ucode-mod-socket +coreutils-timeout` (потянутся из
онлайн-репозитория OpenWrt при установке). `ucode-mod-socket` нужен rpcd-ucode
бекенду (модуль `socket` для loopback API Web Helper), `coreutils-timeout`
даёт `/usr/bin/timeout`, которым обёрнут каждый exec бекенда.

## 1. Копирование на роутер

```sh
cd dist
scp csqtt_1.0.0_aarch64_cortex-a53.apk \
    luci-app-csqtt_1.0.0_all.apk \
    luci-i18n-csqtt-ru_all.apk root@192.168.1.1:/tmp/
ssh root@192.168.1.1
```

Проверка целостности (на ПК): `sha256sum -c SHA256SUMS`.

## 2. Установка

```sh
# на роутере
apk add --allow-untrusted /tmp/csqtt_1.0.0_aarch64_cortex-a53.apk
apk add --allow-untrusted /tmp/luci-app-csqtt_1.0.0_all.apk
apk add --allow-untrusted /tmp/luci-i18n-csqtt-ru_all.apk   # русский UI

apk info csqtt            # версия, зависимости, размер
```

`--allow-untrusted` нужен, потому что пакеты собраны локально и не подписаны
ключом репозитория OpenWrt. Пакет `kmod-tun` (и `luci-base`, `rpcd-mod-ucode`)
`apk` подтянет из официального репозитория — при наличии `apk update`/доступа к
репо; иначе установите их отдельно.

После установки LuCI перезапустите `rpcd`/утилиту меню, если страница не
появилась сразу: `/etc/init.d/rpcd restart` (postinst приложения делает это
сам).

## 3. Включение

Служба CSQTT по умолчанию **не подключается**: UCI `enabled='0'` и профиль
выключен, поэтому туннель не поднимается. При установке OpenWrt сам включает
init-скрипт пакета (`default_postinst`), но пока `main.enabled='0'`, служба
работает вхолостую. Включите профиль и службу: вручную:

```sh
# профиль: import ссылки (расходует одноразовые VK-хеши на сервере)
csqtt profile import 'csqtt://connect?…' --commit
# или правка /etc/config/csqtt секцией config server

uci set csqtt.main.enabled='1'
uci commit csqtt
service csqtt start            # = /etc/init.d/csqtt start (doctor-ворота)
logread -e csqtt
```

Для режима `auto_js` задайте VK-токен: в LuCI (Profiles → **VK JS token**) или

```sh
uci set csqtt.<id>.vk_auth_mode='auto_js'
uci set csqtt.<id>.vk_hash_mode='auto_js'
uci set csqtt.<id>.vk_js_token='<VK access token или полный OAuth redirect-URL>'
uci commit csqtt
```

При импорте `csqtt://`-ссылки **без** VK-хешей (профиль под `auto_js`) не
включайте профиль сразу (`--activate` / «Activate»): сначала задайте токен, затем
включите профиль — иначе включённый профиль без хешей не проходит валидацию.

Поле `vk_js_token` принимает и сам токен, и полный redirect-URL implicit-flow:
и LuCI, и ядро извлекут параметр `access_token`/`token`, декодируют URL-кодирование
и уберут пробелы. Нераспознанный текст отклоняется с понятной ошибкой
(ядро вернёт отказ до обращения к серверу).

Автозапуск init-скрипта обычно включён пакетом; отключить — `/etc/init.d/csqtt disable`,
включить — `/etc/init.d/csqtt enable` (без перезагрузки роутера факт автозапуска
не проверить).
Через LuCI: **Службы → CSQTT → Status** → Start, и **Settings** → `enabled`.

После правки конфига: `service csqtt reload` (служба CSQTT перечитает UCI по SIGHUP,
`csqtt0` не рвётся) либо авто-триггер на `uci commit csqtt`.

## 4. Обновление

```sh
# новый .apk в /tmp, затем:
apk add --allow-untrusted /tmp/csqtt_<новая>_aarch64_cortex-a53.apk \
    /tmp/luci-app-csqtt_<новая>_all.apk /tmp/luci-i18n-csqtt-ru_all.apk
service csqtt restart
```

Версии пакета (1.0.0) и ядра (2.1.9) независимы. При переходе на **более
низкий** номер пакета (смена схемы версий, например 2.1.9 → 1.0) `apk` выполняет
понижение; если прямая замена не проходит — установите с удалением пакетов
(`apk del luci-app-csqtt csqtt`), конфигурация `/etc/config/csqtt` сохраняется
как conffile (при необходимости восстановите из резервной копии).
Автоустановщик `install-csqtt.sh` делает это автоматически.

`/etc/config/csqtt` помечен CONFFILES — при обновлении пользовательский конфиг
сохраняется (новая версия кладётся как `/etc/config/csqtt.apk-new`, если изменился
шаблон). Логи (`/var/log/csqtt.log`) и `/etc/csqtt/backups` не трогаются.

## 5. sysupgrade

При прошивке нового образа OpenWrt через `sysupgrade` сохраняются файлы,
перечисленные в `/etc/sysupgrade.conf` и стандартный список UCI. Чтобы
сохранить конфигурацию CSQTT, добавьте в `/etc/sysupgrade.conf`:

```
/etc/config/csqtt
/etc/csqtt/
```

Сам бинарник/пакет после `sysupgrade` ставить заново (в `/overlay` его нет) —
профилей и `enabled` из сохранённого UCI достаточно, затем `service csqtt
start`. Runtime-состояние `/var/run/csqtt/status.json` — tmpfs, не сохраняется
(восстанавливается службой CSQTT).

## 6. Откат / удаление

```sh
service csqtt stop
service csqtt disable          # убрать автозапуск
apk del luci-app-csqtt csqtt   # удалить пакеты
# конфиг остаётся (CONFFILES); полное удаление конфига:
rm -f /etc/config/csqtt
```

Удаление НЕ трогает маршруты/DNS/firewall — менять нечего (см. ниже).

## 7. ПРИНЦИП ИЗОЛЯЦИИ (важно)

CSQTT — независимое сетевое ядро. Ни пакет, ни служба CSQTT:

- не становятся default-шлюзом и не меняют маршруты/правила/таблицы WAN;
- не трогают системный DNS / `dnsmasq` / `uci dhcp`;
- не создают правил firewall/NAT/forwarding и не захватывают LAN;
- не добавляют `uci-defaults` и firewall-файлов (проверено тестом M7).

Поднимается только TUN-интерфейс `csqtt0` (фиксированное имя). Через него идёт
**только** трафик процессов, явно привязанных к интерфейсу. Control-plane
CSQTT/VK/TURN остаётся через WAN.

Связка с пользовательским прокси настраивается **самим пользователем**, это не
часть пакета. Пример mihomo/ssclash:

```yaml
# конфигурация mihomo (пользовательская)
tun:
  enable: true
  device: csqtt0        # использовать уже поднятый csqtt0
interface-name: csqtt0  # исходящие привязываются к csqtt0 (SO_BINDTODEVICE)
```

`SO_BINDTODEVICE` работает без каких-либо маршрутов/правил (доказано M3X). Если
на железе `rp_filter=1` (strict) режет downlink на `csqtt0` — это настройка
системы: `sysctl net.ipv4.conf.csqtt0.rp_filter=2` (per-interface), ядро порта
не трогать.

## 8. Диагностика

```sh
csqtt status            # 0 подключён / 1 нет / 2 служба CSQTT не запущена
csqtt doctor            # read-only: конфиг / TUN / WAN route / peer / rp_filter
csqtt log tail -n 50 -f # лог с маскировкой секретов
/etc/init.d/csqtt doctor
```

Секреты (пароли профилей, `vk_js_token`, `session_token`, device-id) через
`status`/LuCI/логи не выводятся. `device-id` привязан к паролю на сервере —
переносите как есть, не генерируйте новый.
