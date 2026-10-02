#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M7] Полный конвейер: musl-сборка ядра → staging → OpenWrt SDK → dist/.
# Запуск из Linux/WSL:  sh scripts/build-all.sh
#
# Ядро собирается ВНЕ SDK (статический musl-бинарник, PROJECT_CONTRACT);
# musl-фиксы (tun_ioctl, udp_batch, clap help) не трогаются. crates.io →
# rsproxy.cn задаётся в ~/.cargo/config.toml; target/линкер — там же.
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

SRC="$ROOT/csqtt-openwrt"
STAGE_BIN="$ROOT/openwrt/csqtt/staging/bin"
TARGET=aarch64-unknown-linux-musl

# cargo/rustup могут не быть в PATH неинтерактивного шелла
# shellcheck disable=SC1091  # ~/.cargo/env генерируется rustup, вне дерева
if [ -f "$HOME/.cargo/env" ]; then . "$HOME/.cargo/env"; fi
PATH="$HOME/.cargo/bin:$PATH"; export PATH

echo "==> [1/3] cargo build --release --target $TARGET --bin csqtt"
( cd "$SRC" && cargo build --release --target "$TARGET" --bin csqtt )

BIN="$SRC/target/$TARGET/release/csqtt"
[ -x "$BIN" ] || { echo "ERROR: $BIN не найден после сборки"; exit 1; }
echo "==> артефакт ядра:"
file "$BIN" || true

echo "==> [2/3] staging → openwrt/csqtt/staging/bin/csqtt"
mkdir -p "$STAGE_BIN"
cp -f "$BIN" "$STAGE_BIN/csqtt"
chmod 0755 "$STAGE_BIN/csqtt"

echo "==> [3/3] SDK → dist/"
sh "$SCRIPT_DIR/build-sdk.sh"

echo "==> M7 build-all завершён. Артефакты: $ROOT/dist"
ls -la "$ROOT/dist"
