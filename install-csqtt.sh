#!/bin/sh
# ================================================================
#  star-panel-csqtt — auto-installer для OpenWrt 25.12.x и SNAPSHOT (apk)
#  Архитектура: aarch64_cortex-a53 (ARM64 Cortex-A53)
#  Пакеты берутся из GitHub Release.
#  https://github.com/starkugz/star-panel-csqtt
#
#  Запуск (последний выпуск):
#    wget --no-proxy -qO- https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/install-csqtt.sh | ash
#  Конкретный выпуск (тег):
#    wget --no-proxy -qO- https://github.com/starkugz/star-panel-csqtt/raw/refs/heads/main/install-csqtt.sh | CSQTT_VERSION=v1.0.1 ash
#
#  Принципы:
#   - поддерживаются ровно 25.12.x и SNAPSHOT (apk-tools 3); прочие релизы
#     и архитектуры отклоняются ДО изменений системы;
#   - набор пакетов выбирается автоматически по семейству прошивки
#     (stable/snapshot): сначала ищется вариант `<имя>-<family>.apk`, затем
#     общий; latest разрешается в конкретный тег ОДИН раз, до скачивания;
#   - имена пакетов берутся из SHA256SUMS выбранного выпуска (не конструируются);
#   - контрольная сумма проверяется для каждого из трёх APK;
#   - до установки выполняется сухой прогон всего набора (apk add --simulate):
#     проверяются arch, зависимости и совместимость kmod-tun с ядром;
#   - любая ошибка до установки (нет файла, суммы, платформа, apk-tools,
#     совместимость, место, зависимости, резервная копия) останавливает работу
#     ДО изменений пакетов; массовое обновление прошивки (apk upgrade) не
#     запускается — только обновление индексов (apk update);
#   - установщик НЕ удаляет пакеты при ошибке apk add; удаление старой версии
#     возможно только как осознанный переход при понижении версии и лишь при
#     наличии проверенных пакетов для восстановления.
# ================================================================
set -eu

REPO="starkugz/star-panel-csqtt"
ARCH_OK="aarch64_cortex-a53"
OW_REL_OK="25.12"
APK_TOOLS_MIN=3
REQUIRED_DEPS="kmod-tun luci-base rpcd-mod-ucode ucode-mod-socket coreutils-timeout"
NEED_KB_TMP=30000
NEED_KB_ROOT=10000

# --- переопределяемые пути (для локальных тестов; на роутере пусто) ----------
ROOT="${CSQTT_ROOT:-}"
OW_RELEASE="${ROOT}/etc/openwrt_release"
CONFIG="${ROOT}/etc/config/csqtt"
INITD="${ROOT}/etc/init.d/csqtt"
BIN="${ROOT}/usr/bin/csqtt"
RCD="${ROOT}/etc/rc.d"
TMPBASE="${CSQTT_TMPDIR:-${ROOT:+$ROOT/tmp}}"
[ -n "$TMPBASE" ] || TMPBASE=/tmp

VERSION="${CSQTT_VERSION:-}"
ROLLBACK_DIR="${CSQTT_ROLLBACK_DIR:-}"
WORK="$TMPBASE/csqtt-install.$$"
BACKUP=""

log()  { printf '[+] %s\n' "$*"; }
info() { printf '[i] %s\n' "$*"; }
warn() { printf '[!] %s\n' "$*" >&2; }
die()  { printf '[x] %s\n' "$*" >&2; exit 1; }

download() {
	# download <url> <out>  (не исполняет загруженные данные)
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

fetch() { download "$1" "$2"; }

# --- разрешение latest в конкретный тег (один раз) ---------------------------
resolve_latest_tag() {
	json="$WORK/latest.json"
	if fetch "https://api.github.com/repos/$REPO/releases/latest" "$json"; then
		tag=$(sed -n 's/.*"tag_name":[[:space:]]*"\([^"]*\)".*/\1/p' "$json" | head -n1)
		if [ -n "$tag" ]; then printf '%s' "$tag"; return 0; fi
	fi
	# запасной путь: редирект releases/latest -> /releases/tag/<tag>
	if command -v curl >/dev/null 2>&1; then
		url=$(curl -fsSL -o /dev/null -w '%{url_effective}' "https://github.com/$REPO/releases/latest" 2>/dev/null || true)
	else
		url=$(wget -q -S -O /dev/null "https://github.com/$REPO/releases/latest" 2>&1 | sed -n 's/.*[Ll]ocation: //p' | tail -n1 | tr -d '\r')
	fi
	tag="${url##*/}"
	case "$tag" in
		v[0-9]*) printf '%s' "$tag"; return 0 ;;
	esac
	return 1
}

# --- версии ----------------------------------------------------------------
installed_version() {
	apk list --installed 2>/dev/null | sed -n 's/^csqtt-\([0-9][^ ]*\) .*/\1/p' | head -n1
}

version_gt() {
	# 0, если $1 > $2 (сначала apk version -t, иначе числовой fallback)
	res=$(apk version -t "$1" "$2" 2>/dev/null || true)
	if [ "$res" = ">" ]; then return 0; fi
	if [ "$res" = "<" ] || [ "$res" = "=" ]; then return 1; fi
	[ "$(printf '%s\n%s\n' "$1" "$2" | sort -V | tail -n1)" = "$1" ] && [ "$1" != "$2" ]
}

# --- проверки до изменений --------------------------------------------------
check_platform() {
	[ -f "$OW_RELEASE" ] || die "не найден $OW_RELEASE — это OpenWrt?"
	# shellcheck disable=SC1090  # файл релиза OpenWrt, не пользовательские данные
	. "$OW_RELEASE"
	ARCH="${DISTRIB_ARCH:-}"
	OW_REL="${DISTRIB_RELEASE:-}"
	OW_REV="${DISTRIB_REVISION:-}"
	OW_TARGET="${DISTRIB_TARGET:-}"
	info "OpenWrt ${OW_REL:-?}${OW_REV:+ ($OW_REV)}, arch ${ARCH:-?}${OW_TARGET:+, target $OW_TARGET}"
	case "$ARCH" in
		"$ARCH_OK") : ;;
		*) die "поддерживается только $ARCH_OK (обнаружено: ${ARCH:-неизвестно})." ;;
	esac
	# Два поддерживаемых семейства прошивки на apk-tools 3:
	#   stable   — ровно 25.12.x (фиксированный выпуск);
	#   snapshot — rolling-сборка; конкретная ревизия не «угадывается», а
	#              проверяется по факту: ядро, kmod-tun и зависимости должны
	#              разрешаться из репозиториев самого устройства (см. ниже).
	OW_REL_LC=$(printf '%s' "$OW_REL" | tr '[:upper:]' '[:lower:]')
	case "$OW_REL_LC" in
		snapshot) FAMILY="snapshot" ;;
		"$OW_REL_OK".*) FAMILY="stable" ;;
		*) die "поддерживается OpenWrt ${OW_REL_OK}.x или SNAPSHOT (обнаружено: ${OW_REL:-неизвестно})." ;;
	esac
	info "семейство прошивки: $FAMILY."
	# apk-tools 3 обязателен: пакеты имеют v3-формат (apk v3). Если версию
	# определить не удалось — не блокируем (команда apk уже найдена), но
	# заведомо старые (2.x/opkg) отсекаем.
	APK_V=$(apk --version 2>/dev/null | sed -n 's/.*apk-tools[[:space:]]*\([0-9][0-9]*\).*/\1/p' | head -n1)
	case "$APK_V" in
		''|*[!0-9]*) : ;;
		*) [ "$APK_V" -ge "$APK_TOOLS_MIN" ] || die "нужен apk-tools ${APK_TOOLS_MIN}+ (обнаружено: apk-tools $APK_V)." ;;
	esac
}

check_space() {
	# check_space <path> <need_kb>
	avail=$(df -k "$1" 2>/dev/null | awk 'NR==2 {print $4}')
	[ -n "$avail" ] || return 0
	[ "$avail" -ge "$2" ] || die "мало места в $1: нужно ${2}KiB, свободно ${avail}KiB."
}

check_dependencies() {
	missing=""
	# shellcheck disable=SC2086  # REQUIRED_DEPS — список через пробел без спецсимволов
	for dep in $REQUIRED_DEPS; do
		apk info -e "$dep" >/dev/null 2>&1 || missing="$missing $dep"
	done
	if [ -z "$missing" ]; then
		info "обязательные зависимости на месте."
		return 0
	fi
	info "не хватает зависимостей:$missing — проверяю возможность установки..."
	# сухой прогон: разрешимость зависимостей без изменения установленных пакетов
	# shellcheck disable=SC2086
	if apk add --simulate $REQUIRED_DEPS >/dev/null 2>&1; then
		info "зависимости разрешаются (будут установлены вместе с пакетами)."
	else
		# На SNAPSHOT самая частая причина — kmod-tun для другого ядра:
		# модуль должен соответствовать именно running kernel.
		if ! apk add --simulate kmod-tun >/dev/null 2>&1; then
			die "не разрешается kmod-tun для ядра $(uname -r): модуль должен совпадать с ядром устройства. Обновите индексы (apk update) или прошивку и повторите; ничего не удалено и не изменено."
		fi
		die "не хватает зависимостей:$missing, и apk не может их разрешить. Установите их и повторите."
	fi
}

# --- резервная копия --------------------------------------------------------
backup_config() {
	[ -f "$CONFIG" ] || { info "конфигурация отсутствует (чистая установка)."; return 0; }
	BACKUP="${CONFIG}.bak.$(date +%Y%m%d-%H%M%S)"
	if cp -p "$CONFIG" "$BACKUP" 2>/dev/null; then
		chmod 600 "$BACKUP" 2>/dev/null || true
		info "резервная копия конфигурации: $BACKUP"
	else
		BACKUP=""
		die "не удалось создать резервную копию $CONFIG — обновление остановлено, пакеты не изменялись."
	fi
}

service_enabled() { [ -e "$RCD/S99csqtt" ]; }

# --- выбор пакетов из SHA256SUMS --------------------------------------------
# Возвращает "hash name" для первого файла, подходящего под расширенное
# регулярное выражение. Пусто — файла нет.
pick_asset() {
	awk -v pat="$1" '
		{
			name = $2
			sub(/^\.\//, "", name)
			if (name ~ pat) { print $1, name; exit }
		}' "$WORK/SHA256SUMS"
}

# --- выбор набора пакетов по семейству прошивки ------------------------------
# Если в выпуске опубликован отдельный вариант для семейства
# (`<имя>-<family>.apk`, family = stable|snapshot), он имеет приоритет;
# иначе используется общий пакет. Для проверенной сборки SNAPSHOT общий набор
# подходит (ядро статическое musl, панель/переводы — noarch), но механизм
# оставляет возможность развести наборы без смены команды установки.
pick_core() {
	a=$(pick_asset "^csqtt_[0-9][^/]*_aarch64_cortex-a53-${FAMILY}[.]apk$")
	[ -n "$a" ] || a=$(pick_asset '^csqtt_[0-9][^/]*_aarch64_cortex-a53[.]apk$')
	printf '%s' "$a"
}

pick_panel() {
	a=$(pick_asset "^luci-app-csqtt_[^/]*_all-${FAMILY}[.]apk$")
	[ -n "$a" ] || a=$(pick_asset '^luci-app-csqtt_[^/]*_all[.]apk$')
	printf '%s' "$a"
}

pick_i18n() {
	a=$(pick_asset "^luci-i18n-csqtt-ru[^/]*-${FAMILY}[.]apk$")
	[ -n "$a" ] || a=$(pick_asset '^luci-i18n-csqtt-ru[^/]*[.]apk$')
	printf '%s' "$a"
}

download_and_verify() {
	# download_and_verify <name> <expected_sha>
	fetch "$BASE/$1" "$WORK/$1" || die "не удалось скачать $1 из выпуска $TAG."
	actual=$(sha256sum "$WORK/$1" | awk '{print $1}')
	[ "$actual" = "$2" ] || die "SHA256 $1 не совпала (получено $actual, ожидалось $2) — установка остановлена."
}

# --- проверка совместимости всего набора ДО установки -----------------------
# Сухой прогон `apk add --simulate` по локальным файлам: apk проверяет arch,
# разрешает зависимости (включая kmod-tun для running kernel) и ничего не
# меняет в системе. Это и есть реальная проверка совместимости со SNAPSHOT
# конкретной ревизии — без «флага разрешения» и без установки модулей ядра от
# другой сборки.
verify_package_set() {
	# shellcheck disable=SC2086  # имена файлов без пробелов
	plan=$(apk add --simulate --allow-untrusted \
		"$WORK/$CORE_NAME" "$WORK/$PANEL_NAME" "$WORK/$I18N_NAME" 2>&1) && {
		info "набор совместим: arch, зависимости и kmod-tun разрешаются."
		return 0
	}
	if ! apk add --simulate kmod-tun >/dev/null 2>&1; then
		die "набор несовместим: kmod-tun не разрешается для ядра $(uname -r). На SNAPSHOT модуль должен соответствовать ядру и цели; обновите индексы (apk update) или прошивку. apk: $(printf '%s' "$plan" | tail -n1)"
	fi
	die "набор несовместим с этой системой (arch/зависимости). apk: $(printf '%s' "$plan" | tail -n1)"
}

# --- установка --------------------------------------------------------------
install_packages() {
	# shellcheck disable=SC2086  # PKG_* — имена файлов без пробелов
	apk add --allow-untrusted "$WORK/$CORE_NAME" "$WORK/$PANEL_NAME" "$WORK/$I18N_NAME"
}

# Пакеты для восстановления установленной версии: ищем в CSQTT_ROLLBACK_DIR
# или в $TMPBASE/csqtt-rollback. Без них переход (удаление) запрещён.
find_rollback() {
	dir="$ROLLBACK_DIR"
	[ -n "$dir" ] || dir="$TMPBASE/csqtt-rollback"
	[ -d "$dir" ] || return 1
	for f in csqtt luci-app-csqtt luci-i18n-csqtt-ru; do
		found=$(find "$dir" -maxdepth 1 -name "${f}_*.apk" 2>/dev/null | head -n1)
		[ -n "$found" ] || return 1
	done
	printf '%s' "$dir"
}

restore_rollback() {
	dir="$1"
	info "восстановление предыдущей установки из $dir ..."
	# shellcheck disable=SC2046  # имена файлов без пробелов
	apk add --allow-untrusted $(find "$dir" -maxdepth 1 -name '*.apk') \
		|| warn "восстановление пакетов не удалось — см. состояние ниже."
	if [ -n "$BACKUP" ] && [ -f "$BACKUP" ] && [ ! -f "$CONFIG" ]; then
		cp -p "$BACKUP" "$CONFIG" 2>/dev/null || true
	fi
}

# --- 1. окружение -----------------------------------------------------------
[ "$(id -u)" = 0 ] || die "нужны права root."
command -v apk >/dev/null 2>&1 || die "нужен apk-tools (OpenWrt 25.x). Пакетов .ipk для старых версий нет."
check_platform
mkdir -p "$WORK"
trap 'rm -rf "$WORK"' EXIT INT TERM

# --- 2. выбор выпуска (latest -> тег один раз) ------------------------------
if [ -n "$VERSION" ]; then
	TAG="$VERSION"
	case "$TAG" in v*) : ;; *) TAG="v$TAG" ;; esac
	info "выбран выпуск: $TAG"
else
	info "определяю последний выпуск..."
	TAG=$(resolve_latest_tag) || die "не удалось определить последний выпуск (нет сети или API недоступен). Укажите CSQTT_VERSION=<тег>."
	info "последний выпуск: $TAG"
fi
case "$TAG" in
	*[!A-Za-z0-9._-]*) die "некорректное имя тега: $TAG" ;;
esac
BASE="https://github.com/$REPO/releases/download/$TAG"

# --- 3. метаданные выпуска и имена файлов -----------------------------------
log "Читаю SHA256SUMS выпуска $TAG ..."
fetch "$BASE/SHA256SUMS" "$WORK/SHA256SUMS" \
	|| die "выпуск $TAG недоступен или не содержит SHA256SUMS — установка остановлена."

CORE=$(pick_core)
PANEL=$(pick_panel)
I18N=$(pick_i18n)
[ -n "$CORE" ] || die "в выпуске $TAG нет пакета csqtt (aarch64_cortex-a53, family=$FAMILY)."
[ -n "$PANEL" ] || die "в выпуске $TAG нет пакета luci-app-csqtt."
[ -n "$I18N" ] || die "в выпуске $TAG нет пакета luci-i18n-csqtt-ru."

CORE_NAME=${CORE#* }
CORE_SHA=${CORE%% *}
PANEL_NAME=${PANEL#* }
PANEL_SHA=${PANEL%% *}
I18N_NAME=${I18N#* }
I18N_SHA=${I18N%% *}
info "пакеты выпуска: $CORE_NAME, $PANEL_NAME, $I18N_NAME"

# --- 4. скачивание и проверка каждого APK -----------------------------------
log "Загрузка и проверка пакетов ..."
download_and_verify "$CORE_NAME" "$CORE_SHA"
download_and_verify "$PANEL_NAME" "$PANEL_SHA"
download_and_verify "$I18N_NAME" "$I18N_SHA"
info "контрольные суммы всех трёх пакетов совпали."

# --- 5. проверки до изменений -----------------------------------------------
log "Проверка свободного места ..."
check_space "$TMPBASE" "$NEED_KB_TMP"
check_space "${ROOT}/" "$NEED_KB_ROOT"
# Только индексы: apk upgrade (массовое обновление прошивки) НЕ вызывается.
apk update >/dev/null 2>&1 || warn "apk update не удался (нет сети?) — зависимости могут не установиться."
check_dependencies
log "Проверка совместимости набора (apk --simulate) ..."
verify_package_set
log "Резервная копия конфигурации ..."
backup_config
ENABLED_BEFORE=0
if service_enabled; then ENABLED_BEFORE=1; fi

CUR_VER=$(installed_version || true)
if [ -n "$CUR_VER" ]; then info "установлено сейчас: $CUR_VER"; else info "CSQTT ещё не установлен."; fi
NEW_VER=$(printf '%s' "$CORE_NAME" | sed -n 's/^csqtt_\([0-9][^_]*\)_.*/\1/p')
DOWNGRADE=0
if [ -n "$CUR_VER" ] && [ -n "$NEW_VER" ] && version_gt "$CUR_VER" "$NEW_VER"; then
	DOWNGRADE=1
	info "обнаружено понижение версии: $CUR_VER -> $NEW_VER."
fi

# --- 6. установка (без удаления при ошибке) ---------------------------------
log "Установка пакетов ..."
if install_packages; then
	info "пакеты установлены."
else
	add_rc=$?
	# НИКАКОГО удаления по общим ошибкам (зависимости/место/повреждение).
	if [ "$DOWNGRADE" != 1 ]; then
		die "apk add не удался (код $add_rc). Пакеты не изменялись. Проверьте место, зависимости и целостность пакетов."
	fi
	info "прямая замена не удалась при понижении версии."
	RBDIR=$(find_rollback || true)
	if [ -z "$RBDIR" ]; then
		die "для перехода $CUR_VER -> $NEW_VER нужны проверенные пакеты установленной версии. Поместите их в $TMPBASE/csqtt-rollback (или задайте CSQTT_ROLLBACK_DIR) и повторите. Пакеты не удалялись."
	fi
	log "Переход: удаление $CUR_VER и установка $NEW_VER (восстановление из $RBDIR) ..."
	apk del luci-i18n-csqtt-ru luci-app-csqtt csqtt >/dev/null 2>&1 || true
	if ! install_packages; then
		warn "установка нового выпуска не удалась — восстанавливаю предыдущую установку."
		restore_rollback "$RBDIR"
		warn "фактическое состояние: $(installed_version || echo 'пакеты отсутствуют')"
		[ -n "$BACKUP" ] && warn "резервная копия конфигурации: $BACKUP"
		die "переход не завершён; выполнено восстановление предыдущей установки."
	fi
fi
# Если apk удалил conffile при переходе — восстановить сохранённую копию.
if [ ! -f "$CONFIG" ] && [ -n "$BACKUP" ] && [ -f "$BACKUP" ]; then
	cp -p "$BACKUP" "$CONFIG" || true
	info "конфигурация восстановлена из $BACKUP"
fi

# --- 7. проверка результата -------------------------------------------------
missing=""
# shellcheck disable=SC2086  # REQUIRED_DEPS — список через пробел без спецсимволов
for dep in $REQUIRED_DEPS; do
	apk info -e "$dep" >/dev/null 2>&1 || missing="$missing $dep"
done
[ -z "$missing" ] || die "после установки не хватает зависимостей:$missing"
[ -x "$BIN" ] || die "после установки нет $BIN."
NEW_INSTALLED=$(installed_version || true)
info "установлено: ${NEW_INSTALLED:-csqtt}"
info "ядро: $("$BIN" version 2>/dev/null | head -n1 || echo '?')"
if [ "$ENABLED_BEFORE" = 1 ] && ! service_enabled; then
	if [ -x "$INITD" ]; then "$INITD" enable >/dev/null 2>&1 || true; fi
	info "автозапуск службы сохранён."
elif service_enabled; then
	info "автозапуск службы включён."
else
	info "автозапуск не включён (включите при необходимости)."
fi

# --- 8. дальнейшие шаги -----------------------------------------------------
cat <<EOF

[+] Готово: star-panel-csqtt установлен (пакет + панель LuCI + ядро CSQTT 2.1.9).
    OpenWrt ${OW_REL}${OW_REV:+ ($OW_REV)}, семейство ${FAMILY}, arch ${ARCH}.

Дальше в LuCI (Службы → star-panel-csqtt):
  1. Профили → Импортировать ссылку: вставьте ссылку csqtt://… (без отметки «Активировать»).
  2. Откройте профиль (Изменить): Режим авторизации VK = Auto JS, Режим хешей VK = Auto JS.
  3. В поле VK JS token вставьте сам токен или полный OAuth redirect-URL.
  4. Сохраните и включите профиль.
  5. Включите службу: Настройки → CSQTT включён (или uci set csqtt.main.enabled='1'; uci commit csqtt).
  6. Проверьте соединение: csqtt status; csqtt doctor.
  7. Трафик: направьте прокси на csqtt0 (Mihomo/ssclash, interface-name: csqtt0)
     либо выполните явный тест: curl --interface csqtt0 https://1.1.1.1/cdn-cgi/trace.

ВАЖНО: interface-only — установка сама по себе НЕ направляет весь трафик в
туннель; нужен пользовательский прокси, привязанный к csqtt0.

Обновление/откат:
  - конфигурация сохраняется; копия для этого запуска: ${BACKUP:-нет}
  - переустановить конкретный выпуск: CSQTT_VERSION=<тег> ash install-csqtt.sh
  - резервная копия одного конфига НЕ является откатом пакетов.
EOF
