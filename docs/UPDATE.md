# UPDATE — как обновлять проект через месяцы

Runbook для возврата к проекту спустя время: собрать, протестировать,
задеплоить, откатить. Все команды — из **Linux/WSL** (SDK требует ext4,
см. `scripts/build-sdk.sh`). См. также [`OPENWRT.md`](OPENWRT.md),
[`TESTING.md`](TESTING.md).

## 0. Быстрый вход

```sh
# зависимости хоста
sudo apt-get update && sudo apt-get install -y build-essential pkg-config \
    cmake python3 curl file zstd unzip git shellcheck

# Rust + musl target
. "$HOME/.cargo/env"
rustup target add aarch64-unknown-linux-musl
# (по проекту сборочный линкер/реестр заданы в ~/.cargo/config.toml)
```

## 1. Соблюдайте контракт (не «улучшать» по пути)

- M3X: `csqtt0` — **interface-only**; служба CSQTT не ставит route/rule/table, не
  трогает WAN/DNS/dnsmasq/firewall/NAT.
- `device-id` привязан к паролю — **не регенерировать**.
- `MAX_WORKERS=126`; `workers` — любое `9..126`, эффективно округляется к
  ближайшему кратному 9.
- musl-фиксы (`tun_ioctl`, `udp_batch`, clap `help`) не откатывать.
- Секреты не выводить в UI/логи/status. Подробно — [`SECURITY.md`](SECURITY.md).

## 2. Изменение версии

Версии разделены: пакет интеграции и панель — 1.0.0, ядро — 2.1.9.

- `openwrt/csqtt/Makefile`: `PKG_VERSION`, `PKG_RELEASE`
- `openwrt/luci-app-csqtt/Makefile`: `PKG_VERSION`, `PKG_RELEASE`
- `csqtt-openwrt/Cargo.toml` → `version` — только при изменении **ядра**
  (2.1.9 не переименовывать в 1.0)
- `dist/RELEASE-vX/` — положите текущий набор артефактов перед новой сборкой

**Почему `PKG_RELEASE` важен:** apk-tools 3 не переустанавливает пакет с той же
версией; инкремент `PKG_RELEASE` заставляет apk заменить файлы (см.
`docs/OPENWRT.md` §3). При переходе на более низкий номер пакета apk выполняет
понижение (см. `docs/RELEASE.md`).

## 3. Сборка и полный прогон тестов

```sh
sh scripts/build-all.sh                     # musl-ядро → staging → SDK → dist/*.apk
(cd csqtt-openwrt && bash scripts/run-tests.sh)   # fmt + clippy + 600+ unit
sh openwrt/tests/openwrt/run.sh             # M5 (init.d/UCI/logrotate)
sh openwrt/tests/luci/run.sh                # M6b (LuCI вью, QR, runtime)
sh openwrt/tests/packages/verify-apk.sh     # M7 (структура .apk)
```

Артефакты и контрольные суммы:

```sh
cat dist/SHA256SUMS
```

## 4. Деплой

```sh
SSHHOST=192.168.1.1 SSHPASS=... scp root@$SSHHOST ...   # или scp напрямую
```

Далее — по [`OPENWRT.md`](OPENWRT.md) §2–3. После установки обязательно:

```sh
# целостность пакетов — по контрольным суммам релиза:
( cd /tmp && sha256sum -c SHA256SUMS )
# версия ядра (пакет 1.0.0 может нести ядро 2.1.9):
csqtt version
```

> `/usr/bin/csqtt` при упаковке в `.apk` освобождается от отладочных символов,
> поэтому его sha256 не совпадает с сырым `target/.../release/csqtt`. Сверяйте
> целостность по `SHA256SUMS` пакетов, а не по этому файлу.

## 5. Живой smoke-test (M8, реальный роутер)

```sh
SSHHOST=192.168.1.1 SSHPASS=... CSQTT_SMOKE_WAIT=120 sh scripts/smoke-test.sh
```

Покрывает: пакеты/версию, procd, `doctor`, `csqtt0`, status, redaction логов,
LuCI HTTP, respawn, **изоляцию до/после**, `rp_filter`, mihomo (если задан
`MIHOMO_BIN`), failover/failback, fatal auth, CAPTCHA, workers/CPU/RAM,
stop/teardown, upgrade persistence.

## 6. Периодические зависимости

- **OpenWrt SDK:** версия в `scripts/build-sdk.sh` (`OPENWRT_VERSION=25.12.5`).
  При переходе OpenWrt поднимите и там; SDK скачается/проверится по sha256.
- **crates.io:** фиксируются `Cargo.lock`. Обновление — осознанное:
  `cargo update`, затем полный `run-tests.sh` и smoke-test.
- **luci feed:** подтягивается автоматически при сборке SDK (для `luci.mk` и
  host `po2lmo`).

## 7. Откат

1. Взять предыдущий набор `.apk` из `dist/RELEASE-vX/`.
2. Установить его (учитывая §3 по версиям; при равной версии — см. способы
   принудительной переустановки/заливки бинаря).
3. `/etc/config/csqtt` сохраняется как conffile; при необходимости верните
   профиль из бэкапа.

## 8. Компактное сохранение и Git

```sh
sh scripts/pack.sh                 # csqtt-compact-YYYYMMDD-HHMMSS.tar.gz (+ .sha256)
git init && git add -A && git commit -m "CSQTT OpenWrt baseline"
git tag v2.1.9-openwrt-baseline
```

`.gitignore` и `pack.sh` исключают `csqtt-openwrt/target` (~4.7 ГБ), `.opencode`,
`*.zip`, `*.log`.

## 9. Чек-лист перед релизом

- [ ] `run-tests.sh` зелёный (fmt/clippy/unit).
- [ ] M5/M6b/M7 тесты зелёные.
- [ ] `PKG_RELEASE` инкрементирован.
- [ ] `dist/SHA256SUMS` совпадает с залитыми файлами.
- [ ] На роутере `sha256sum /usr/bin/csqtt` совпадает со сборкой.
- [ ] `smoke-test.sh` PASS/SKIP без FAIL.
- [ ] `docs/CURRENT.md` и `CHANGELOG.md` обновлены.
