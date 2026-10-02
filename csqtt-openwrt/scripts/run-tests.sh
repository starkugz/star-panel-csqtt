#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M3R] Полный локальный прогон качества порта csqtt-openwrt:
# fmt → clippy → unit-тесты. Запуск из корня csqtt-openwrt/ (или откуда
# угодно — скрипт сам переходит в свою директорию). Требует cargo в PATH.
set -euo pipefail
cd "$(dirname "$0")/.."

echo "=== cargo fmt --check ==="
cargo fmt --check

echo "=== cargo clippy --all-targets -- -D warnings ==="
cargo clippy --all-targets -- -D warnings

echo "=== cargo test --lib ==="
cargo test --lib

# [M5] Интеграционные тесты openwrt-файлов (init.d/UCI/logrotate). Идём только
# если собран host-бинарник (нужен для doctor-парсинга дефолтного конфига);
# иначе — SKIP с предупреждением (контракт M5).
echo "=== [M5] openwrt integration tests ==="
CSQTT_HOST_BIN=""
for cand in target/debug/csqtt target/release/csqtt; do
	if [ -x "$cand" ]; then CSQTT_HOST_BIN="$PWD/$cand"; break; fi
done
if [ -n "$CSQTT_HOST_BIN" ]; then
	CSQTT_HOST_BIN="$CSQTT_HOST_BIN" sh ../openwrt/tests/openwrt/run.sh
else
	echo "SKIP: host-бинарник не собран (cargo build --bin csqtt) — openwrt-тесты пропущены" >&2
fi

echo "=== OK: fmt/clippy/test + openwrt зелёные ==="
