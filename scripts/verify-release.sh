#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [REL] Гейт выпуска: выполняется ПЕРЕД публикацией GitHub Release.
# Принцип: критичные проверки не должны «тихо» пропускаться. Отсутствие
# инструмента или build-дерева SDK — это FAIL, а не SKIP.
#
# Запуск (Linux/WSL, из корня репозитория):
#   sh scripts/verify-release.sh
# Переменные: CSQTT_SDK — путь к распакованному OpenWrt SDK.
set -eu

ROOT=$(cd "$(dirname "$0")/.." && pwd)
cd "$ROOT"

# cargo/rustup могут быть не в PATH неинтерактивного шелла (локальный WSL).
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
PATH="$HOME/.cargo/bin:$PATH"; export PATH

DIST="$ROOT/dist"
SDK="${CSQTT_SDK:-$HOME/csqtt-m7/sdk/openwrt-sdk}"

fail() { echo "REL-GATE FAIL: $*" >&2; exit 1; }
step() { echo ""; echo "==> $*"; }

# --- 1. Обязательные инструменты (иначе M6/M7 уйдут в SKIP) -------------------
step "проверка инструментов"
for t in ucode jq node python3 msgfmt shellcheck; do
	command -v "$t" >/dev/null 2>&1 || fail "не найден инструмент: $t (критичные проверки M6/M7 не должны пропускаться)"
done

# --- 2. Артефакты и контрольные суммы ----------------------------------------
step "контрольные суммы dist/SHA256SUMS"
[ -f "$DIST/SHA256SUMS" ] || fail "нет $DIST/SHA256SUMS"
( cd "$DIST" && sha256sum -c SHA256SUMS ) || fail "SHA256SUMS не совпадают с .apk"
for f in "$DIST"/csqtt_*_aarch64_cortex-a53.apk \
         "$DIST"/luci-app-csqtt_*_all.apk \
         "$DIST"/luci-i18n-csqtt-ru_all.apk; do
	[ -f "$f" ] || fail "нет пакета $f"
done

# --- 3. Build-дерево SDK (строгие .pkgdir проверки M7) ------------------------
step "build-дерево SDK"
CB="$SDK/build_dir/target-aarch64_cortex-a53_musl"
[ -d "$CB" ] || fail "нет build-дерева SDK ($CB) — M7 .pkgdir проверки будут пропущены"
export CSQTT_SDK="$SDK"

# --- 4. Пакетные тесты (M7) ---------------------------------------------------
step "M7 verify-apk"
sh openwrt/tests/packages/verify-apk.sh || fail "M7 verify-apk"

# --- 5. M5 (init.d/UCI/logrotate) --------------------------------------------
step "M5 openwrt"
sh openwrt/tests/openwrt/run.sh || fail "M5 openwrt"

# --- 6. M6 (LuCI: ucode-бекенд, ACL, i18n, QR) --------------------------------
step "M6 luci"
sh openwrt/tests/luci/run.sh || fail "M6 luci"

# --- 7. Rust: fmt + clippy + unit (+ M5 host) ---------------------------------
step "rust fmt/clippy/unit"
( cd csqtt-openwrt && cargo build --bin csqtt && bash scripts/run-tests.sh ) || fail "rust quality gate"

echo ""
echo "REL-GATE: OK — выпуск можно публиковать"
