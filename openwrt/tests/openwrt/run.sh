#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M5] Интеграционный тест openwrt-файлов (etc/config/csqtt, etc/init.d/csqtt,
# etc/logrotate.d/csqtt). Чистый POSIX sh, root НЕ требуется: мок-окружение —
# fake rootfs + моки procd_*/uci/modprobe/pidof/kill. Запуск:
#   sh openwrt/tests/openwrt/run.sh
# Опционально: CSQTT_HOST_BIN=/path/to/csqtt — host-бинарник для парсинга
# дефолтного конфига (иначе авто-поиск в ../csqtt-openwrt/target, без него SKIP).

# shellcheck disable=SC2015,SC2317
#   SC2015: A && ok || bad — ok/bad всегда exit 0, ветка bad ложно не срабатывает;
#   SC2317: моки pidof/kill/modprobe/uci вызываются косвенно из функций init.d.
set -u

TESTS_DIR=$(cd "$(dirname "$0")" && pwd)
OPENWRT_DIR=$(cd "$TESTS_DIR/../.." && pwd)
INIT="$OPENWRT_DIR/etc/init.d/csqtt"
CONF="$OPENWRT_DIR/etc/config/csqtt"
LOGROTATE="$OPENWRT_DIR/etc/logrotate.d/csqtt"

WORK=$(mktemp -d) || exit 1
trap 'rm -rf "$WORK"' EXIT HUP INT TERM

PASS=0
FAIL=0
SKIP=0
ok() { PASS=$((PASS + 1)); echo "ok   - $1"; }
bad() { FAIL=$((FAIL + 1)); echo "FAIL - $1"; }
sk() { SKIP=$((SKIP + 1)); echo "SKIP - $1"; }

# --- 1. Синтаксис всех shell-скриптов ---------------------------------------
if sh -n "$INIT" 2>"$WORK/shn.err"; then
	ok "sh -n etc/init.d/csqtt"
else
	bad "sh -n etc/init.d/csqtt: $(cat "$WORK/shn.err")"
fi
if sh -n "$0" 2>"$WORK/shn2.err"; then
	ok "sh -n tests/openwrt/run.sh"
else
	bad "sh -n tests/openwrt/run.sh: $(cat "$WORK/shn2.err")"
fi

# --- 2. shellcheck -----------------------------------------------------------
if command -v shellcheck >/dev/null 2>&1; then
	if shellcheck -s sh "$INIT" >"$WORK/sc.out" 2>&1; then
		ok "shellcheck etc/init.d/csqtt — 0 замечаний"
	else
		bad "shellcheck etc/init.d/csqtt: $(grep -o 'SC[0-9]*' "$WORK/sc.out" | sort -u | tr '\n' ' ')"
	fi
	if shellcheck -s sh "$0" >"$WORK/sc2.out" 2>&1; then
		ok "shellcheck tests/openwrt/run.sh — 0 замечаний"
	else
		bad "shellcheck tests/openwrt/run.sh: $(grep -o 'SC[0-9]*' "$WORK/sc2.out" | sort -u | tr '\n' ' ')"
	fi
else
	sk "shellcheck не установлен"
fi

# --- 3. Мок-окружение: fake rootfs + моки procd/uci --------------------------
mkdir -p "$WORK/rootfs/etc" "$WORK/bin"
cp "$CONF" "$WORK/rootfs/etc/csqtt-uci"
PROCD_LOG="$WORK/procd.log"
ARGV_LOG="$WORK/argv.log"
: >"$PROCD_LOG"
: >"$ARGV_LOG"

cat >"$WORK/bin/csqtt" <<'MOCK'
#!/bin/sh
# мок бинарника csqtt: пишет argv, doctor/status отдают управляемые коды выхода
echo "argv: $*" >>"$ARGV_LOG"
case "$1" in
doctor) exit "${MOCK_DOCTOR_RC:-0}" ;;
status) exit "${MOCK_STATUS_RC:-0}" ;;
esac
exit 0
MOCK
chmod +x "$WORK/bin/csqtt"

procd_open_instance() { echo "open_instance $*" >>"$PROCD_LOG"; }
procd_set_param() { echo "set_param $*" >>"$PROCD_LOG"; }
procd_close_instance() { echo "close_instance" >>"$PROCD_LOG"; }
procd_add_reload_trigger() { echo "reload_trigger $*" >>"$PROCD_LOG"; }
extra_command() { echo "extra_command $1" >>"$PROCD_LOG"; }
uci() { echo "uci $*" >>"$ARGV_LOG"; }
modprobe() { echo "modprobe $*" >>"$ARGV_LOG"; }

export CSQTT_BIN="$WORK/bin/csqtt"
export CSQTT_UCI="$WORK/rootfs/etc/csqtt-uci"
# [High-2 AUDIT] Fresh boot: runtime-каталог изначально отсутствует.
export CSQTT_RUN_DIR="$WORK/rootfs/var-run/csqtt"
export ARGV_LOG
export MOCK_DOCTOR_RC=0 MOCK_STATUS_RC=0
# shellcheck disable=SC1090,SC1091  # source временного init-скрипта из моков — намеренно
. "$INIT"

# --- 4. start_service: ожидаемые procd_set_param ------------------------------
MOCK_DOCTOR_RC=0
start_service
rc=$?
[ "$rc" -eq 0 ] && ok "start_service: doctor OK → старт (exit 0)" || bad "start_service вернул $rc"
grep -qx "open_instance main" "$PROCD_LOG" && ok "instance main" || bad "нет procd_open_instance main"
grep -qx "set_param command $WORK/bin/csqtt run --config $WORK/rootfs/etc/csqtt-uci" "$PROCD_LOG" \
	&& ok "command = csqtt run --config <UCI>" || bad "неверный procd_set_param command"
grep -qx "set_param respawn 3600 5 0" "$PROCD_LOG" && ok "respawn 3600 5 0" || bad "нет respawn"
grep -qx "set_param stdout 1" "$PROCD_LOG" && ok "stdout 1 (syslog)" || bad "нет stdout"
grep -qx "set_param stderr 1" "$PROCD_LOG" && ok "stderr 1 (syslog)" || bad "нет stderr"
grep -qx "set_param limits core=0 0" "$PROCD_LOG" && ok "limits core=0" || bad "нет limits core=0"
grep -qx "close_instance" "$PROCD_LOG" && ok "close_instance" || bad "нет close_instance"
grep -qx "extra_command doctor" "$PROCD_LOG" && ok "extra_command doctor зарегистрирован" || bad "нет extra_command doctor"
service_triggers
grep -qx "reload_trigger csqtt" "$PROCD_LOG" && ok "reload-триггер на UCI csqtt" || bad "нет procd_add_reload_trigger csqtt"
grep -qx "argv: doctor --config $WORK/rootfs/etc/csqtt-uci" "$ARGV_LOG" \
	&& ok "start_service вызывает doctor" || bad "doctor не вызывался"

# --- 4b. [High-2 AUDIT] Runtime-каталог создаётся на fresh boot ---------------
if [ -d "$CSQTT_RUN_DIR" ]; then
	ok "start_service создал runtime-каталог (fresh boot)"
else
	bad "start_service не создал $CSQTT_RUN_DIR"
fi
RUN_MODE=$(stat -c '%a' "$CSQTT_RUN_DIR" 2>/dev/null || echo none)
[ "$RUN_MODE" = "755" ] && ok "runtime-каталог 0755" || bad "права runtime-каталога: $RUN_MODE"
start_service
rc=$?
[ "$rc" -eq 0 ] && [ -d "$CSQTT_RUN_DIR" ] \
	&& ok "повторный start идемпотентен (каталог на месте, exit 0)" || bad "второй start_service: rc=$rc"

# --- 5. doctor FAIL (exit 2) → отказ; WARN (exit 1) → старт ------------------
: >"$PROCD_LOG"
MOCK_DOCTOR_RC=2
start_service
rc=$?
[ "$rc" -ne 0 ] && ok "doctor exit 2 → отказ старта" || bad "старт не отклонён при doctor FAIL"
grep -q "open_instance" "$PROCD_LOG" && bad "инстанс создан при doctor FAIL" || ok "при отказе инстанс не создан"
: >"$PROCD_LOG"
MOCK_DOCTOR_RC=1
start_service
rc=$?
[ "$rc" -eq 0 ] && grep -qx "open_instance main" "$PROCD_LOG" \
	&& ok "doctor exit 1 (WARN) → старт разрешён" || bad "WARN-поведение start_service"

# --- 6. reload_service → kill -HUP (без stop/start) ---------------------------
: >"$PROCD_LOG"
KILL_LOG="$WORK/kill.log"
: >"$KILL_LOG"
pidof() { echo "4242"; }
kill() { echo "kill $*" >>"$KILL_LOG"; }
reload_service
grep -qx "kill -HUP 4242" "$KILL_LOG" && ok "reload: SIGHUP процессу" || bad "reload не выставил kill -HUP"
grep -q "open_instance" "$PROCD_LOG" && bad "reload лезет в procd-инстанс" || ok "reload не пересоздаёт инстанс"
: >"$KILL_LOG"
pidof() { return 0; }
reload_service
rc=$?
[ "$rc" -ne 0 ] && [ ! -s "$KILL_LOG" ] && ok "reload при остановленном службе CSQTT → не-0 без kill" || bad "reload без pidof"

# --- 7. status_service / doctor ------------------------------------------------
MOCK_STATUS_RC=1
status_service
rc=$?
[ "$rc" -eq 1 ] && grep -qx "argv: status" "$ARGV_LOG" && ok "status → csqtt status (exit 1 = не подключён)" || bad "status_service"
doctor >/dev/null 2>&1
grep -qx "argv: doctor --config $WORK/rootfs/etc/csqtt-uci" "$ARGV_LOG" && ok "init-команда doctor" || bad "doctor() не вызывает бинарник"

# --- 8. Мок uci/modprobe: init-скрипт их не дёргает (кроме modprobe tun) ------
grep -q "^uci " "$ARGV_LOG" && bad "init.d вызывает uci (мутации вне допустимы)" || ok "init.d не вызывает uci"
grep "^modprobe " "$ARGV_LOG" | grep -qv "^modprobe tun$" && bad "init.d грузит посторонние модули" || ok "modprobe только tun (если вызывался)"

# --- 9. Дефолтный конфиг: схема M4a, enabled='0' ------------------------------
grep -q "^config csqtt 'main'$" "$CONF" && ok "секция main по схеме M4a" || bad "нет config csqtt 'main'"
grep -A2 "^config csqtt 'main'$" "$CONF" | grep -q "option enabled '0'" \
	&& ok "дефолт main.enabled='0'" || bad "main.enabled != 0"
grep -q "^config csqtt 'routing'$" "$CONF" && ok "секция routing" || bad "нет config csqtt 'routing'"
grep -A1 "^config csqtt 'routing'$" "$CONF" | grep -q "option mode 'auto'" \
	&& ok "routing mode='auto' (interface-only M3X)" || bad "routing.mode != auto"
grep -q "log_file '/var/log/csqtt.log'" "$CONF" && ok "log_file совпадает с logrotate" || bad "log_file/logrotate разъезжаются"
grep -q "^/var/log/csqtt.log {" "$LOGROTATE" && ok "logrotate: путь лога" || bad "logrotate: нет пути"
for kw in weekly copytruncate "rotate 4"; do
	grep -q "$kw" "$LOGROTATE" && ok "logrotate: $kw" || bad "logrotate: нет $kw"
done

# --- 10. Дефолтный конфиг парсится host-бинарником ----------------------------
HOST_BIN=${CSQTT_HOST_BIN:-}
if [ -z "$HOST_BIN" ]; then
	for cand in "$OPENWRT_DIR/../csqtt-openwrt/target/debug/csqtt" "$OPENWRT_DIR/../csqtt-openwrt/target/release/csqtt"; do
		[ -x "$cand" ] && HOST_BIN=$cand && break
	done
fi
if [ -n "$HOST_BIN" ]; then
	mkdir -p "$WORK/confdir"
	cp "$CONF" "$WORK/confdir/csqtt"
	if "$HOST_BIN" doctor --config-dir "$WORK/confdir" >"$WORK/doctor.out" 2>&1; then
		DOCTOR_RC=0
	else
		DOCTOR_RC=$?
	fi
	grep -q '^\[OK\] config' "$WORK/doctor.out" \
		&& ok "бинарник: дефолтный конфиг валиден (doctor)" || bad "бинарник: config-чек не OK (rc=$DOCTOR_RC): $(head -3 "$WORK/doctor.out")"
else
	sk "host-бинарник не найден (cargo build --bin csqtt) — парсинг конфига пропущен"
fi

# --- 11. НЕГАТИВНАЯ ПРОВЕРКА ИЗОЛЯЦИИ ----------------------------------------
# init/package files: никаких маршрутизации/DNS/локальной защиты/uci-defaults
# и подмены default-шлюза (контракты M3X + PROJECT_CONTRACT).
ISOLATION_PATTERNS='ip route|ip rule|ip -[0-9]|route add|route del|ifconfig|iptables|ip6tables|nft |nft-|fw4|firewall|tc |dnsmasq|resolv|sysctl|ubus call network|netifd|udhcpc|uci add|uci set|uci batch|uci commit|uci delete'
ISOLATION_HITS=$(grep -nE "$ISOLATION_PATTERNS" "$INIT" "$CONF" "$LOGROTATE" 2>/dev/null || true)
if [ -z "$ISOLATION_HITS" ]; then
	ok "изоляция: init/config/logrotate без запрещённых команд"
else
	bad "изоляция: запрещённые токены: $(echo "$ISOLATION_HITS" | tr '\n' ' | ')"
fi
# init.d не должен содержать и проcd-параметров, поднимающих сеть
if grep -Eq "procd_set_param (net|interface|watchprocd)" "$INIT"; then
	bad "изоляция: procd сетевые параметры"
else
	ok "изоляция: нет procd сетевых параметров"
fi

# --- Итог ----------------------------------------------------------------------
echo
echo "M5 openwrt tests: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
if [ "$FAIL" -ne 0 ]; then
	echo "RESULT: FAIL"
	exit 1
fi
echo "RESULT: PASS"
