#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [SAVE] Компактный архив проекта CSQTT для сохранения/переноса.
#
# Исключает тяжёлое и воспроизводимое:
#   - csqtt-openwrt/target (сборка, ~4.7 ГБ)
#   - распакованные reference-архивы (*.zip) и временные файлы
#   - .opencode
# Сохраняет исходники, пакеты, скрипты, docs, dist (готовые .apk).
#
# Запуск (Linux/WSL):  sh scripts/pack.sh [out-dir]
# Результат: <out-dir>/csqtt-compact-YYYYMMDD-HHMMSS.tar.gz (+ .sha256)
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd)
OUT_DIR=${1:-$(cd "$ROOT/.." && pwd)}
TS=$(date +%Y%m%d-%H%M%S)
ARCHIVE="$OUT_DIR/csqtt-compact-$TS.tar.gz"

mkdir -p "$OUT_DIR"
( cd "$ROOT" && tar -czf "$ARCHIVE" \
	--exclude='./csqtt-openwrt/target' \
	--exclude='./.opencode' \
	--exclude='./*.zip' \
	--exclude='./uc.out' \
	--exclude='./*.log' \
	. )

sha256sum "$ARCHIVE" > "$ARCHIVE.sha256"
echo "==> компактный архив: $ARCHIVE"
ls -lh "$ARCHIVE"
cat "$ARCHIVE.sha256"
