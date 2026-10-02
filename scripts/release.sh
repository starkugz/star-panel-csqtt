#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M8] Релиз CSQTT: сборка .apk -> верификация -> бандл dist/RELEASE-<tag>
# (пакеты + SHA256SUMS + документация) -> git tag vX.Y.Z (если каталог — git-репо).
#
# Запуск (Linux/WSL):  sh scripts/release.sh v2.1.9
#
# Переменные: SKIP_BUILD=1 — не пересобирать (использовать готовый dist/).
#             SKIP_TESTS=1 — не запускать локальные регрессы.
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
DIST="$ROOT/dist"

TAG_IN="${1:-}"
[ -n "$TAG_IN" ] || { echo "usage: sh scripts/release.sh vX.Y.Z"; exit 2; }
case "$TAG_IN" in
	v*) TAG="$TAG_IN" ;;
	*) TAG="v$TAG_IN" ;;
esac
VERSION="${TAG#v}"

echo "==> CSQTT release $TAG (version $VERSION)"

# 1. Сборка (musl-ядро + SDK-пакеты) ----------------------------------------
if [ "${SKIP_BUILD:-0}" = 1 ]; then
	echo "==> SKIP_BUILD=1 — использую готовый $DIST"
else
	sh "$SCRIPT_DIR/build-all.sh"
fi

# 2. Верификация пакетов -----------------------------------------------------
sh "$ROOT/openwrt/tests/packages/verify-apk.sh"
( cd "$DIST" && sha256sum -c SHA256SUMS )

# 3. Локальные регрессы ------------------------------------------------------
if [ "${SKIP_TESTS:-0}" = 1 ]; then
	echo "==> SKIP_TESTS=1 — регрессы пропущены"
else
	sh "$ROOT/openwrt/tests/luci/run.sh"
	sh "$ROOT/openwrt/tests/openwrt/run.sh"
	( cd "$ROOT/csqtt-openwrt" && bash scripts/run-tests.sh )
fi

# 4. Бандл дистрибутива ------------------------------------------------------
OUT="$DIST/RELEASE-$TAG"
rm -rf "$OUT"
mkdir -p "$OUT/docs"
cp -f "$DIST"/csqtt_*_aarch64_cortex-a53.apk "$OUT"/
cp -f "$DIST"/luci-app-csqtt_*_all.apk "$OUT"/ 2>/dev/null || true
cp -f "$DIST"/luci-i18n-csqtt-ru_all.apk "$OUT"/ 2>/dev/null || true
cp -f "$DIST/SHA256SUMS" "$OUT"/
cp -f "$ROOT/docs/INSTALL.md" "$OUT/docs/"
cp -f "$ROOT/docs/TEST_PLAN.md" "$OUT/docs/" 2>/dev/null || true
cp -f "$ROOT/docs/CAPTCHA.md" "$OUT/docs/" 2>/dev/null || true
cp -f "$ROOT/README.md" "$OUT/docs/README.md" 2>/dev/null || true
( cd "$OUT" && sha256sum ./*.apk > SHA256SUMS )
echo "==> бандл: $OUT"
ls -la "$OUT"

# 5. git tag -----------------------------------------------------------------
if git -C "$ROOT" rev-parse --verify -q HEAD >/dev/null 2>&1; then
	if git -C "$ROOT" rev-parse -q --verify "refs/tags/$TAG" >/dev/null 2>&1; then
		echo "==> тег $TAG уже существует — пропускаю"
	else
		git -C "$ROOT" tag -a "$TAG" -m "CSQTT OpenWrt $TAG"
		echo "==> git tag $TAG создан ($(git -C "$ROOT" rev-parse --short HEAD))"
	fi
else
	echo "==> ВНИМАНИЕ: $ROOT не git-репозиторий (или нет HEAD) — git tag не создан (артефакты готовы в $OUT)"
fi

echo "==> релиз $TAG готов"
