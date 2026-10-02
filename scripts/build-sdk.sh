#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M7] Сборка .apk через OpenWrt SDK (aarch64_cortex-a53, mediatek/filogic).
# Запускать из Linux/WSL. Cargo ВНУТРЬ SDK не вызывается: musl-бинарник
# предварительно кладётся в openwrt/csqtt/staging/bin/csqtt (scripts/build-all.sh).
#
# SDK и сборка живут на нативной ext4 ($CSQTT_SDK_WORK, по умолчанию
# $HOME/csqtt-m7): на drvfs/9p SDK ломает симлинки feeds и права. Артефакты
# копируются обратно в <root>/dist/.
#
# Окружение: OPENWRT_VERSION (25.12.5), TARGET (mediatek/filogic),
# ARCH (aarch64_cortex-a53), CSQTT_SDK_WORK.
set -eu

SCRIPT_DIR=$(cd "$(dirname "$0")" && pwd)
ROOT=$(cd "$SCRIPT_DIR/.." && pwd)

OPENWRT_VERSION=${OPENWRT_VERSION:-25.12.5}
TARGET=${TARGET:-mediatek/filogic}
ARCH=${ARCH:-aarch64_cortex-a53}
BASE_URL="https://downloads.openwrt.org/releases/$OPENWRT_VERSION/targets/$TARGET"

WORK=${CSQTT_SDK_WORK:-$HOME/csqtt-m7}
DL="$WORK/dl"
SDK="$WORK/sdk"
DIST="$ROOT/dist"

mkdir -p "$WORK" "$DL" "$DIST"

# --- 1. Имя и sha256 SDK из release sha256sums (версионно-устойчиво) ---------
SHA_FILE="$DL/sha256sums.$OPENWRT_VERSION"
if [ ! -s "$SHA_FILE" ]; then
	echo "==> скачиваю sha256sums ($BASE_URL)"
	curl -fsSL "$BASE_URL/sha256sums" -o "$SHA_FILE"
fi
SDK_NAME=$(grep -oE 'openwrt-sdk-[^ ]*Linux-x86_64\.tar\.zst' "$SHA_FILE" | head -1)
[ -n "$SDK_NAME" ] || { echo "ERROR: SDK-архив не найден в sha256sums"; exit 1; }
SDK_SHA=$(awk -v f="$SDK_NAME" '$2 == "*"f || $2 == f {print $1; exit}' "$SHA_FILE")
echo "==> SDK: $SDK_NAME"
echo "==> sha256: $SDK_SHA"

# --- 2. Скачивание + проверка + распаковка -----------------------------------
SDK_TARBALL="$DL/$SDK_NAME"
if [ ! -s "$SDK_TARBALL" ]; then
	echo "==> скачиваю SDK (~250 МБ) ..."
	curl -fL --retry 3 -C - -o "$SDK_TARBALL.part" "$BASE_URL/$SDK_NAME"
	mv "$SDK_TARBALL.part" "$SDK_TARBALL"
fi
echo "$SDK_SHA  $SDK_TARBALL" | ( cd "$DL" && sha256sum -c - )

if [ ! -d "$SDK/openwrt-sdk" ]; then
	echo "==> распаковываю SDK в $SDK ..."
	rm -rf "$SDK"; mkdir -p "$SDK"
	tar --zstd -xf "$SDK_TARBALL" -C "$SDK" --strip-components=1
	mv "$SDK" "$SDK.tmp" 2>/dev/null || true
	mkdir -p "$SDK"; mv "$SDK.tmp" "$SDK/openwrt-sdk"
fi
SDK="$SDK/openwrt-sdk"
[ -f "$SDK/include/prereq.mk" ] || [ -d "$SDK/scripts" ] || { echo "ERROR: SDK не распакован"; exit 1; }
echo "==> SDK готов: $SDK"

# --- 2b. Неинтерактивный .config -------------------------------------------
# Без .config `make` в SDK входит в интерактивный menuconfig и подвешивает
# сборку (актуально для CI/чистого клона). Задаём целевой таргет и дефолты
# один раз; при уже существующем .config ничего не трогаем. Полный defconfig
# здесь безопасен: SDK таргет-специфичный, CONFIG_ALL_KMODS не включается.
if [ ! -f "$SDK/.config" ]; then
	echo "==> создаю .config (TARGET=$TARGET, ARCH=$ARCH) неинтерактивно"
	{
		echo "CONFIG_TARGET_${TARGET%%/*}=y"
		echo "CONFIG_TARGET_${TARGET%%/*}_${TARGET##*/}=y"
	} > "$SDK/.config"
	( cd "$SDK" && make defconfig </dev/null )
fi

# --- 3. luci feed (нужен luci.mk) + хостовый po2lmo для i18n -----------------
# SDK 25.12 НЕ поставляет локальное prebuilt-репо пакетов и dev-файлы ucode/
# rpcd/libubus. Полная target-сборка luci-base (lucihttp→ucode→lua) в SDK
# невозможна без клонирования огромного base-feed и не нужна: +luci-base/
# +rpcd-mod-ucode — рантайм-зависимости, которые apk подтянет с онлайн-репо
# при установке. Для сборки самого luci-app-csqtt нужен лишь luci.mk (логика
# установки root/) и хостовый po2lmo (компиляция .po→.lmo для i18n).
# Поэтому: ставим luci-base, собираем ТОЛЬКО его host-вариант (po2lmo/jsmin —
# чистый C, без lua/ucode), затем убираем luci-base из дерева, чтобы make не
# дёргал target-цепочку. po2lmo остаётся в staging_dir/hostpkg/bin.
FEEDS_CONF="$SDK/feeds.conf"
[ -f "$FEEDS_CONF" ] || cp "$SDK/feeds.conf.default" "$FEEDS_CONF"
if ! grep -qE '^[[:space:]]*src-git[[:space:]]+luci[[:space:]]' "$FEEDS_CONF"; then
	echo "src-git luci https://git.openwrt.org/project/luci.git" >> "$FEEDS_CONF"
fi
echo "==> feeds update luci (клонирование luci feed) ..."
if [ ! -f "$SDK/feeds/luci/luci.mk" ]; then
	( cd "$SDK" && ./scripts/feeds update luci )
else
	echo "    luci feed уже в $SDK/feeds/luci — пропускаю update"
fi

# po2lmo (host) нужен для i18n. Цикл install/host-compile/uninstall ТРОГАЕТ
# .config (refresh_config) и сбрасывает штампы kmod, поэтому выполняем его
# ровно один раз — пока po2lmo не появился. Дальнейшие прогоны его пропускают.
if [ -x "$SDK/staging_dir/hostpkg/bin/po2lmo" ]; then
	echo "==> po2lmo уже собран — пропускаю install/host-compile/uninstall luci-base"
else
	echo "==> feeds install luci-base (временно, ради host/po2lmo) ..."
	( cd "$SDK" && ./scripts/feeds install luci-base )
	echo "==> make package/feeds/luci/luci-base/host/compile (po2lmo) ..."
	( cd "$SDK" && make package/feeds/luci/luci-base/host/compile V=s )
	[ -x "$SDK/staging_dir/hostpkg/bin/po2lmo" ] || {
		echo "ERROR: po2lmo не собран (host-сборка luci-base)"; exit 1; }
	echo "==> feeds uninstall luci-base (не строим target-цепочку) ..."
	( cd "$SDK" && ./scripts/feeds uninstall luci-base )
fi

# --- 4. Копирование пакетов в SDK --------------------------------------------
# единый источник etc-файлов — openwrt/etc (уже покрыт M5-тестами) → files/
echo "==> синхронизирую openwrt/etc → openwrt/csqtt/files/etc ..."
mkdir -p "$ROOT/openwrt/csqtt/files/etc/init.d" \
         "$ROOT/openwrt/csqtt/files/etc/config" \
         "$ROOT/openwrt/csqtt/files/etc/logrotate.d"
cp -f "$ROOT/openwrt/etc/init.d/csqtt"      "$ROOT/openwrt/csqtt/files/etc/init.d/csqtt"
cp -f "$ROOT/openwrt/etc/config/csqtt"      "$ROOT/openwrt/csqtt/files/etc/config/csqtt"
cp -f "$ROOT/openwrt/etc/logrotate.d/csqtt" "$ROOT/openwrt/csqtt/files/etc/logrotate.d/csqtt"

[ -x "$ROOT/openwrt/csqtt/staging/bin/csqtt" ] || {
	echo "ERROR: openwrt/csqtt/staging/bin/csqtt отсутствует — выполните scripts/build-all.sh"; exit 1; }

echo "==> копирую пакеты в $SDK/package ..."
rm -rf "$SDK/package/csqtt" "$SDK/package/luci-app-csqtt"
cp -a "$ROOT/openwrt/csqtt" "$SDK/package/csqtt"
cp -a "$ROOT/openwrt/luci-app-csqtt" "$SDK/package/luci-app-csqtt"

# --- 5. Компиляция целевых пакетов -------------------------------------------
# make defconfig НЕ запускаем (раздувает сборку через CONFIG_ALL_KMODS).
# ОДНИМ make-вызовом: ядро/модули (DEPENDS +kmod-tun) упаковываются один раз
# и переиспользуются; повторные прогоны кэшируются штампами make.
echo "==> make package/csqtt/compile package/luci-app-csqtt/compile ..."
( cd "$SDK" && make package/csqtt/compile package/luci-app-csqtt/compile V=s )

# --- 6. Сбор артефактов в dist/ ----------------------------------------------
# Версии пакетов берём из Makefile — не дублируем числа в скрипте (ядро 2.1.9
# и пакет интеграции 1.0.0 версионируются раздельно).
PKGVER=$(sed -n 's/^PKG_VERSION:=//p' "$ROOT/openwrt/csqtt/Makefile" | tr -d '\r' | head -1)
LUCI_VER=$(sed -n 's/^PKG_VERSION:=//p' "$ROOT/openwrt/luci-app-csqtt/Makefile" | tr -d '\r' | head -1)
[ -n "$PKGVER" ] && [ -n "$LUCI_VER" ] || {
	echo "ERROR: не удалось прочитать PKG_VERSION из Makefile (csqtt=$PKGVER luci=$LUCI_VER)"; exit 1; }
echo "==> версии пакетов: csqtt=$PKGVER luci-app-csqtt=$LUCI_VER (ядро — из Cargo.toml)"
BIN="$SDK/bin/packages"
echo "==> собранные .apk (apk-формат имён: name-version-rrelease_arch.apk):"
find "$BIN" -name '*.apk' \( -name 'csqtt-*' -o -name 'luci-app-csqtt-*' -o -name 'luci-i18n-csqtt-*' \) -print

copy_apk() {
	# $1 = glob имени собранного apk, $2 = целевое имя в dist/.
	# [M8] Берём САМЫЙ СВЕЖИЙ файл (i18n-apk версионируется по PKG_PO_VERSION —
	# старые остаются в bin/packages и `find | head -1` давал устаревший пакет).
	src=$(find "$BIN" -name "$1" -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)
	[ -n "$src" ] || { echo "ERROR: не найден $1"; exit 1; }
	cp -f "$src" "$DIST/$2"
	echo "    $2  <-  $(basename "$src")"
}

copy_apk "csqtt-${PKGVER}-*.apk"            "csqtt_${PKGVER}_${ARCH}.apk"
copy_apk "luci-app-csqtt-${LUCI_VER}-*.apk" "luci-app-csqtt_${LUCI_VER}_all.apk"
# i18n-пакет (русский) — версия авто-генерируется luci.mk (PKG_PO_VERSION).
# [M8] Как и в copy_apk — берём САМЫЙ СВЕЖИЙ по mtime: `find | head -1`
# возвращал устаревший пакет (строки перевода не доезжали в dist/).
i18n=$(find "$BIN" -name 'luci-i18n-csqtt-ru-*.apk' -printf '%T@ %p\n' | sort -nr | head -1 | cut -d' ' -f2-)
[ -n "$i18n" ] && cp -f "$i18n" "$DIST/luci-i18n-csqtt-ru_all.apk" && \
	echo "    luci-i18n-csqtt-ru_all.apk  <-  $(basename "$i18n")"

echo "==> SHA256SUMS ..."
( cd "$DIST" && rm -f SHA256SUMS && sha256sum ./*.apk > SHA256SUMS )
cat "$DIST/SHA256SUMS"
echo "==> готово: $DIST"
