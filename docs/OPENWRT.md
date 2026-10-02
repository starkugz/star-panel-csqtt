# OPENWRT — установка, обновление, откат, интеграция

Целевое устройство: **Huasifei WH3000 Pro** (MediaTek MT7981, ARM64 Cortex-A53 ×2),
**OpenWrt 25.12.x**, пакетный менеджер **apk** (apk-tools 3), `aarch64_cortex-a53`.
См. также [`ARCHITECTURE.md`](ARCHITECTURE.md), [`CONFIG.md`](CONFIG.md),
[`SECURITY.md`](SECURITY.md).

## 1. Пакеты

| Файл в `dist/` | Устанавливает |
|---|---|
| `csqtt_2.1.9_aarch64_cortex-a53.apk` | `/usr/bin/csqtt`, `/etc/init.d/csqtt`, `/etc/config/csqtt`, logrotate |
| `luci-app-csqtt_2.1.9_all.apk` | LuCI-вью, menu.d, ACL, rpcd-ucode backend |
| `luci-i18n-csqtt-ru_all.apk` | русский перевод (`.lmo`) |

Рантайм-зависимости (`+luci-base`, `+rpcd-mod-ucode`, `+kmod-tun`) apk подтянет
с онлайн-репозитория. Перед установкой на роутере должен быть интернет (WAN).

## 2. Установка

```sh
# 1) передать артефакты
scp dist/*.apk root@192.168.1.1:/tmp/

# 2) установить
ssh root@192.168.1.1
apk add --allow-untrusted \
  /tmp/csqtt_2.1.9_aarch64_cortex-a53.apk \
  /tmp/luci-app-csqtt_2.1.9_all.apk \
  /tmp/luci-i18n-csqtt-ru_all.apk

# 3) профиль и запуск (enabled по умолчанию = 0)
csqtt profile import 'csqtt://…' --commit --activate
uci set csqtt.main.enabled='1'
uci commit csqtt
/etc/init.d/csqtt enable
/etc/init.d/csqtt start
```

Проверка:

```sh
csqtt doctor                 # конфиг/TUN/peer (read-only)
ubus call csqtt status | grep -E 'connected|active|configured'
ip -4 addr show csqtt0
```

## 3. ВАЖНО: переустановка той же версии в apk-tools 3

`apk add <file>.apk` при **совпадающей версии** (`2.1.9-r1`) считает пакет уже
установленным и **не перезаписывает файлы**. `apk del` + `apk add` тоже может
подхватить **закэшированный** пакет той же версии и оставить старый бинарь
(реально наблюдалось на WH3000 Pro: дата/размер `/usr/bin/csqtt` не менялись).

Рабочие способы:

- **Рекомендуется:** поднять `PKG_RELEASE` в `openwrt/csqtt/Makefile` и
  `openwrt/luci-app-csqtt/Makefile` (`1` → `2`, …) и пересобрать — apk видит
  апгрейд и корректно заменяет файлы.
- Либо принудительно из файла с обновлением кэша:
  ```sh
  rm -rf /var/cache/apk/* 2>/dev/null
  apk add --allow-untrusted --force-overwrite --force-refresh /tmp/csqtt_*.apk
  ```
- Либо (быстрая проверка, без пакетного учёта): залить бинарь напрямую:
  ```sh
  /etc/init.d/csqtt stop
  scp csqtt-openwrt/target/aarch64-unknown-linux-musl/release/csqtt \
      root@192.168.1.1:/usr/bin/csqtt
  chmod 0755 /usr/bin/csqtt
  /etc/init.d/csqtt start
  ```

Всегда сверяйте `sha256sum /usr/bin/csqtt` с `sha256sum` собранного бинаря.

## 4. Интеграция с Mihomo (interface-only)

CSQTT **не** ставит маршруты и не меняет DNS/firewall (контракт M3X). Трафик в
туннель направляет пользовательский прокси. Для Mihomo — привязка исходящих
сокетов к устройству:

```yaml
# config.yaml (фрагмент)
interface-name: csqtt0
```

Проверка egress-IP на роутере:

```sh
curl -s --interface csqtt0 https://api.ipify.org   # должен вернуть IP peer'а
```

`rp_filter`: при strict `net.ipv4.conf.csqtt0.rp_filter=1` downlink режется.
Решение — `=2` (loose); ядро не пересобирать. См. `M3X`.

## 5. Обновление и откат

- **Обновление:** см. [`UPDATE.md`](UPDATE.md) (runbook на месяцы вперёд).
- **Откат:** держите предыдущий `dist/` (или `dist/RELEASE-vX/`). Установка
  предыдущего набора `.apk` возвращает поведение; UCI-конфиг сохраняется
  (`Package/csqtt/conffiles`).
- **Сброс профиля:** `csqtt profile remove <id>` либо правка `/etc/config/csqtt`.
  `device_id` **не** регенерировать без отвязки на сервере.

## 6. Диагностика

| Симптом | Что проверить |
|---|---|
| `connected:false`, профиль `failed` | `csqtt doctor`, валидность VK-хешей, `fail_threshold`/`health_interval` |
| Пул убивает попытку до готовности | стартовое окно `STARTUP_GRACE` (30с) в `pool.rs`; медленный DNS/VK-auth не должен считаться фейлом сразу |
| `/usr/bin/csqtt` не обновляется | см. §3 (кэш apk / совпадение версии) |
| XHR/LuCI пусто | rpcd-ucode + ACL, `logread \| grep rpcd` |
| Нет трафика в туннеле | привязка прокси к `csqtt0`, `rp_filter`, `curl --interface csqtt0` |

Логи: `/var/log/csqtt.log` (redaction секретов), плюс `logread | grep csqtt`.
Runtime-состояние: `/var/run/csqtt/status.json`.
