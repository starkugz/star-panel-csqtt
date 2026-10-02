#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [REL] Устанавливает интерпретатор ucode на хост (Ubuntu CI) для M6-проверок
# LuCI-бекенда. Без ucode критичные RPC-проверки уходят в SKIP, что на релизе
# недопустимо. Идемпотентен: если ucode уже доступен — выходит сразу.
#
# Запуск:  sh scripts/install-ucode.sh
# Переменные: UCODE_REF — git-ref ucode (по умолчанию — master).
set -eu

if command -v ucode >/dev/null 2>&1; then
	echo "ucode уже установлен: $(ucode -V 2>/dev/null || echo 'версия неизвестна')"
	exit 0
fi

UCODE_REF="${UCODE_REF:-master}"

SUDO=""
[ "$(id -u)" -ne 0 ] && SUDO="sudo"

$SUDO apt-get update
$SUDO apt-get install -y --no-install-recommends build-essential cmake git libjson-c-dev ca-certificates

SRC=$(mktemp -d)
trap 'rm -rf "$SRC"' EXIT INT TERM

git clone --depth 1 --branch "$UCODE_REF" https://github.com/jow-/ucode.git "$SRC/ucode"

# Снимаем -Werror: сборка ucode не должна падать из-за предупреждений нового
# компилятора (не влияет на функциональность интерпретатора).
sed -i 's/-Werror[^ ]*//g' "$SRC/ucode/CMakeLists.txt"

cmake -S "$SRC/ucode" -B "$SRC/ucode/build" \
	-DCMAKE_BUILD_TYPE=Release -DBUILD_OPTIMIZE_SIZE=OFF
cmake --build "$SRC/ucode/build" -j"$(nproc)"
$SUDO cmake --install "$SRC/ucode/build"

command -v ucode >/dev/null 2>&1 || { echo "ucode не установился" >&2; exit 1; }
ucode -e 'print("ucode готов\n");'
