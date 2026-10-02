# RELEASE — релизы и публикация

Как выпускать новые версии и публиковать репозиторий/пакеты. См. также
[`UPDATE.md`](UPDATE.md), [`OPENWRT.md`](OPENWRT.md), [`CONTRIBUTING.md`](../CONTRIBUTING.md).

## Версионирование

Три независимые версии:

- **Пакет интеграции для OpenWrt** (`csqtt`): `openwrt/csqtt/Makefile` →
  `PKG_VERSION` (1.0.0) и `PKG_RELEASE`.
- **Панель LuCI** (`luci-app-csqtt`): `openwrt/luci-app-csqtt/Makefile` →
  `PKG_VERSION` (1.0.0) и `PKG_RELEASE`.
- **Ядро CSQTT**: `csqtt-openwrt/Cargo.toml` → `version` (2.1.9; видно в
  `csqtt version` и `status.json`). Версия ядра не подменяется версией пакета.

Инкрементируйте `PKG_RELEASE` при пересборке с теми же `PKG_VERSION` — иначе
apk-tools 3 не заменит пакет (см. `OPENWRT.md` §3). Тег git: `vX.Y.Z`
(например `v1.0.0`).

Переход на **более низкий** номер пакета (смена схемы версий) `apk` выполняет
как понижение; автоустановщик добавляет безопасный откат (удаление с
сохранением конфигурации и повторная установка).

## Чек-лист релиза

- [ ] Нет секретов/реальной инфраструктуры: `grep` по паролю, токену, хешам,
      `device_id`, peer IP (см. `.gitignore`, `SECURITY.md`).
- [ ] `run-tests.sh` зелёный; M5/M6/M7 зелёные.
- [ ] `PKG_RELEASE` инкрементирован.
- [ ] `CHANGELOG.md` обновлён.
- [ ] `docs/*` актуальны (Config/Architecture при изменениях).
- [ ] `sh scripts/build-all.sh` собирает `dist/*.apk` + `SHA256SUMS`.

## Сборка артефактов

```sh
sh scripts/build-all.sh
cat dist/SHA256SUMS
```

Артефакты: `csqtt_<ver>_aarch64_cortex-a53.apk`, `luci-app-csqtt_<ver>_all.apk`,
`luci-i18n-csqtt-ru_all.apk`.

## Публикация в GitHub

### Первый раз

```sh
git init
git add -A
git commit -m "CSQTT OpenWrt baseline"
git tag -a v1.0.0 -m "star-panel-csqtt 1.0.0 (core 2.1.9)"
git branch -M main
git remote add origin git@github.com:<owner>/<repo>.git
git push -u origin main --tags
```

### Новый релиз

```sh
# 1) поднять версию/ревизию и обновить CHANGELOG
# 2) собрать и проверить
sh scripts/build-all.sh && (cd csqtt-openwrt && bash scripts/run-tests.sh)
# 3) коммит + тег
git add -A && git commit -m "release: vX.Y.Z"
git tag -a vX.Y.Z -m "star-panel-csqtt vX.Y.Z"
git push origin main --tags
# 4) GitHub Release + ассеты
gh release create vX.Y.Z dist/*.apk dist/SHA256SUMS \
  --title "star-panel-csqtt vX.Y.Z" --notes-file CHANGELOG.md
```

Либо загрузите `.apk` вручную на странице Releases.

## GitHub Actions

`.github/workflows/release.yml` собирает ядро и `.apk` на каждый тег `v*` и
прикладывает артефакты к Release (Ubuntu, WSL не нужен — SDK host x86_64).
Если сборка в CI недоступна/слишком долгая, публикуйте артефакты, собранные
локально (`gh release create … dist/*.apk`).

## Скрытые локальные материалы

Следующее не публикуется (см. `.gitignore`): `csqtt-main/`, `focsq-main/`
(сторонние reference), `PROGRESS.md`, `docs/history/`, `docs/reference/`,
dev-артефакты (`_live-router-fix/`, `tools/`, devtools-копии JS) и снимки
`dist/RELEASE-*/`. Файлы остаются на диске.

## Обновление на устройстве

Автообновления нет (по замыслу). Новый релиз ставится пакетами:

```sh
scp dist/*.apk root@<router>:/tmp/
ssh root@<router> 'apk add --allow-untrusted /tmp/csqtt_*.apk /tmp/luci-app-csqtt_*.apk /tmp/luci-i18n-csqtt-ru_*.apk'
```

Сверяйте `sha256sum /usr/bin/csqtt` с собранным бинарём. После `sysupgrade`
пакеты нужно переустановить (см. `OPENWRT.md`).
