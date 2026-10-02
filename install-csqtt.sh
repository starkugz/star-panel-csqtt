#!/bin/sh
# ================================================================
#  star-panel-csqtt — auto-installer для OpenWrt 25.12.x (apk)
#  Архитектура: aarch64_cortex-a53 (ARM64 Cortex-A53)
#  Пакеты берутся из GitHub Release (pinned-версия).
#  https://github.com/starkugz/star-panel-csqtt
#
#  Запуск (последний релиз):
#    wget --no-proxy -qO- https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/install-csqtt.sh | ash
#  Конкретная версия:
#    wget --no-proxy -qO- https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/install-csqtt.sh | CSQTT_VERSION=v2.1.9-openwrt ash
# ================================================================
set -eu

REPO="starkugz/star-panel-csqtt"
VERSION="${CSQTT_VERSION:-}"
# Версия пакета интеграции/панели — 1.0.0; встроенное ядро — 2.1.9.
PKG1="csqtt_1.0.0_aarch64_cortex-a53.apk"
PKG2="luci-app-csqtt_1.0.0_all.apk"
PKG3="luci-i18n-csqtt-ru_all.apk"
WORK="/tmp/csqtt-install.$$"
BACKUP=""
REQUIRED_DEPS="kmod-tun luci-base rpcd-mod-ucode ucode-mod-socket coreutils-timeout"

log()  { printf '[+] %s\n' "$*"; }
info() { printf '[i] %s\n' "$*"; }
warn() { printf '[!] %s\n' "$*" >&2; }
die()  { printf '[x] %s\n' "$*" >&2; exit 1; }

download() {
	# download <url> <out>
	if command -v uclient-fetch >/dev/null 2>&1; then
		uclient-fetch -q -O "$2" "$1"
	elif command -v wget >/dev/null 2>&1; then
		wget --no-proxy -q -O "$2" "$1" 2>/dev/null || wget -q -O "$2" "$1"
	elif command -v curl >/dev/null 2>&1; then
		curl -fsSL "$1" -o "$2"
	else
		return 1
	fi
}

check_space() {
	# check_space <path> <need_kb>
	avail=$(df -k "$1" 2>/dev/null | awk 'NR==2 {print $4}')
	[ -n "$avail" ] || return 0
	[ "$avail" -ge "$2" ] || die "мало места в $1: нужно ${2}KiB, свободно ${avail}KiB"
}

installed_version() {
	v=$(apk info -v csqtt 2>/dev/null | head -n1 || true)
	[ -n "$v" ] && { printf '%s' "$v"; return; }
	[ -x /usr/bin/csqtt ] && /usr/bin/csqtt --version 2>/dev/null | head -n1 || true
}

# --- 1. Проверки окружения ---------------------------------------------------
[ "$(id -u)" = 0 ] || die "нужны права root."
[ -f /etc/openwrt_release ] || die "не найден /etc/openwrt_release — это OpenWrt?"
command -v apk >/dev/null 2>&1 || die "нужен apk-tools (OpenWrt >= 25). Для старых версий пакетов .ipk нет."

. /etc/openwrt_release
ARCH="${DISTRIB_ARCH:-}"
OW_REL="${DISTRIB_RELEASE:-?}"
OW_MAJOR=$(printf '%s' "$OW_REL" | cut -d. -f1)
info "OpenWrt ${OW_REL}, arch ${ARCH:-?}"

case "$ARCH" in
	aarch64_cortex-a53) : ;;
	*) die "поддерживается только aarch64_cortex-a53 (обнаружено: ${ARCH:-неизвестно})." ;;
esac
[ "${OW_MAJOR:-0}" -ge 25 ] 2>/dev/null || die "нужен OpenWrt 25.x (apk); обнаружено ${OW_REL}."

log "Проверка свободного места..."
check_space /tmp 30000
check_space / 10000

# Индекс пакетов нужен для резолва зависимостей (kmod-tun, luci-base,
# rpcd-mod-ucode, ucode-mod-socket, coreutils-timeout). Без него apk add
# локального файла может не найти зависимости на «чистом» устройстве.
log "Обновление индекса пакетов (apk update)..."
apk update >/dev/null 2>&1 || warn "apk update не удался (нет сети?) — зависимости могут не установиться"

# --- 2. Версия и URL релиза --------------------------------------------------
if [ -n "$VERSION" ]; then
	BASE="https://github.com/${REPO}/releases/download/${VERSION}"
	info "версия: $VERSION"
else
	BASE="https://github.com/${REPO}/releases/latest/download"
	info "версия: последний релиз (latest)"
fi

# --- 3. Что уже установлено --------------------------------------------------
CUR_VER=$(installed_version)
[ -n "$CUR_VER" ] && info "установлено сейчас: $CUR_VER" || info "CSQTT ещё не установлен."

mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT INT TERM

# --- 4. Резервная копия конфигурации ----------------------------------------
if [ -f /etc/config/csqtt ]; then
	BACKUP="/etc/config/csqtt.bak.$(date +%Y%m%d-%H%M%S)"
	if cp -p /etc/config/csqtt "$BACKUP" 2>/dev/null; then
		info "резервная копия: $BACKUP"
	else
		BACKUP=""
		warn "не удалось создать резервную копию /etc/config/csqtt"
	fi
fi

# --- 5. Загрузка и проверка контрольных сумм --------------------------------
log "Загрузка пакетов из релиза..."
download "$BASE/SHA256SUMS" "$WORK/SHA256SUMS" || die "не удалось скачать SHA256SUMS (релиз $BASE доступен?)."
for p in "$PKG1" "$PKG2" "$PKG3"; do
	download "$BASE/$p" "$WORK/$p" || die "не удалось скачать $p."
done

log "Проверка SHA256..."
( cd "$WORK" && sha256sum -c SHA256SUMS ) || die "контрольные суммы не совпали — загрузка повреждена."

# --- 6. Установка -----------------------------------------------------------
# apk может отказаться менять пакеты напрямую при смене схемы версий
# (2.1.9-r1 -> 1.0.0 = понижение). Тогда — безопасный переход: резервная
# копия конфигурации уже сделана, удаляем пакеты и ставим новые.
install_packages() {
	apk add --allow-untrusted "$WORK/$PKG1" "$WORK/$PKG2" "$WORK/$PKG3"
}

log "Установка пакетов..."
if ! install_packages; then
	info "прямая замена не удалась (возможно, смена схемы версий 2.1.9 -> 1.0.0)."
	info "Переход: удаление пакетов с сохранением конфигурации и повторная установка..."
	apk del luci-i18n-csqtt-ru luci-app-csqtt csqtt >/dev/null 2>&1 || true
	if ! install_packages; then
		[ -n "$BACKUP" ] && warn "конфигурация сохранена: $BACKUP"
		die "apk add не удался после перехода."
	fi
fi
# Если apk удалил conffile при переходе — восстановить сохранённую копию.
if [ ! -f /etc/config/csqtt ] && [ -n "$BACKUP" ] && [ -f "$BACKUP" ]; then
	cp -p "$BACKUP" /etc/config/csqtt
	info "конфигурация восстановлена из $BACKUP"
fi

# --- 7. Проверка результата -------------------------------------------------
missing=""
for dep in $REQUIRED_DEPS; do
	apk info -e "$dep" >/dev/null 2>&1 || missing="$missing $dep"
done
if [ -n "$missing" ]; then
	warn "не установлены обязательные зависимости:$missing"
	die "установка не завершена: установите зависимости и повторите."
fi
[ -x /usr/bin/csqtt ] || die "после установки нет /usr/bin/csqtt."
NEW_VER=$(installed_version)
info "установлено: ${NEW_VER:-csqtt}"
info "ядро: $(/usr/bin/csqtt version 2>/dev/null | head -n1 || echo '?')"

if [ -x /etc/init.d/csqtt ] && /etc/init.d/csqtt enabled 2>/dev/null; then
	info "автозапуск службы включён."
else
	info "автозапуск не включён (включите при необходимости)."
fi

# --- 8. Дальнейшие шаги и откат ---------------------------------------------
cat <<EOF

[+] Готово: CSQTT 1.0 установлен (пакет для OpenWrt + панель LuCI + ядро 2.1.9).

Дальше (обязательно включить профиль и службу):
  1. LuCI → Службы → star-panel-csqtt
  2. Профиль (ВКЛЮЧИТЕ его): csqtt profile import 'csqtt://…' --commit --activate
  3. VK: вставьте VK access token или полный OAuth redirect-URL (поле VK JS token)
  4. Служба: uci set csqtt.main.enabled='1'; uci commit csqtt
  5. Запуск: /etc/init.d/csqtt enable; /etc/init.d/csqtt start
  6. Проверка: csqtt status; csqtt doctor
  7. Трафик: привязать прокси к csqtt0 (Mihomo/ssclash, interface-name: csqtt0)

ВАЖНО: interface-only — установка сама по себе НЕ направляет весь трафик в
туннель; нужен пользовательский прокси, привязанный к csqtt0.

Откат к предыдущей версии:
  /etc/init.d/csqtt stop
  apk del luci-i18n-csqtt-ru luci-app-csqtt csqtt
  CSQTT_VERSION=<тег> sh -c "\$(wget --no-proxy -qO- https://github.com/${REPO}/raw/refs/heads/main/install-csqtt.sh)"
EOF
if [ -n "$BACKUP" ]; then
	echo "  Восстановить конфигурацию: cp '$BACKUP' /etc/config/csqtt"
fi
