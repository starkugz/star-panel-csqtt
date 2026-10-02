#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M4c] Дымовой прогон CLI-подкоманд на собранном артефакте через qemu.
# Запуск: bash scripts/cli_smoke.sh (из csqtt-openwrt или откуда угодно —
# скрипт сам находит корень репозитория). Требует qemu-aarch64-static в PATH.
# doctor делает реальную UDP-пробу peer из фикстуры — WARN «нет ответа»
# на silent-сервере это нормальный результат.
#
# Каждая команда сравнивается с ожидаемым exit-кодом; итоговый exit скрипта
# ненулевой, если хотя бы одна команда разошлась с ожиданием.
set -uo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
BIN="$ROOT/../csqtt-aarch64"
if [ ! -x "$BIN" ]; then
    echo "артефакт не найден: $BIN" >&2
    echo "соберите: cargo build --release --target aarch64-unknown-linux-musl --bin csqtt" >&2
    exit 99
fi
Q="qemu-aarch64-static $BIN"
CF="$ROOT/tests/fixtures/config/valid/csqtt"
LINK="$(cat "$ROOT/tests/fixtures/links/current.txt")"
FAILURES=0

# <ожидаемый exit> <команда...>
run_case() {
    local expected="$1"; shift
    echo "=== $* ==="
    "$@"
    local rc=$?
    if [ "$rc" -ne "$expected" ]; then
        echo "MISMATCH: expected $expected, got $rc"
        FAILURES=$((FAILURES + 1))
    else
        echo "exit=$rc (ok)"
    fi
}

run_case 0 $Q version
run_case 2 $Q status
run_case 0 $Q profile list --config "$CF"
run_case 0 $Q profile show finland --config "$CF"

# import preview: конфиг не меняется, exit 0.
run_case 0 bash -c "$Q profile import '$LINK' --id newserver --config '$CF' >/dev/null"
# pipe-closed: тихий SIGPIPE (141), а не panic 101. Код берём у qemu
# (PIPESTATUS[0]), а не у head.
echo "=== $Q profile import ... | head -2 (SIGPIPE) ==="
$Q profile import "$LINK" --id newserver --config "$CF" | head -2
rc=${PIPESTATUS[0]}
if [ "$rc" -ne 141 ]; then
    echo "MISMATCH: expected 141, got $rc"
    FAILURES=$((FAILURES + 1))
else
    echo "exit=$rc (ok)"
fi
run_case 0 bash -c "$Q profile export finland --config '$CF' >/dev/null"

run_case 2 $Q captcha list
run_case 1 $Q captcha cancel
run_case 1 $Q log tail -n 3
# doctor: 1 WARN (peer-reachability на silent-сервере) → exit 1.
run_case 1 $Q doctor --config-dir "$ROOT/tests/fixtures/config/valid"

if [ "$FAILURES" -ne 0 ]; then
    echo "ИТОГ: $FAILURES несовпадений exit-кодов"
    exit 1
fi
echo "ИТОГ: все exit-коды совпали с ожиданием"
