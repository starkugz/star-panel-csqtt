#!/bin/sh
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M8] Сквозной smoke-test CSQTT на реальном роутере (Huasifei WH3000 Pro,
# OpenWrt 25.12.x). Запускается С ХОСТА по SSH; сам роутер не требует git/курьезов.
#
# Шаги (PASS/FAIL/SKIP), см. docs/TEST_PLAN.md:
#   1 packages  2 version  3 UCI enable+start+procd  4 doctor
#   5 ip link csqtt0  6 status --json  7 logs+redaction  8 LuCI HTTP
#   9 respawn (procd)  10 ИЗОЛЯЦИЯ (before/after)  11 rp_filter
#   12 Mihomo TCP/UDP (опционально, MIHOMO_BIN)  13 failover  14 failback=off
#   15 fatal auth  16 CAPTCHA  17 workers/CPU/RAM/temp  18 stop/teardown
#   19 upgrade persistence
# 13-16 требуют нескольких профилей/живого VK-challenge — без них честный SKIP.
#
# Доступ (env):
#   SSHHOST (по умолчанию 192.168.1.1)  SSHPORT (22)  SSHUSER (root)
#   SSHPASS (пароль; используется sshpass -e)  либо SSHKEY (файл ключа)
# Опции:
#   CSQTT_SMOKE_WAIT=90     ожидание поднятия csqtt0
#   CSQTT_SMOKE_RESTART=1   перезапустить сервис на шаге 3 (по умолчанию 1)
#   MIHOMO_BIN=/opt/clash/bin/clash   включить шаг 12 (иначе SKIP)
#   MIHOMO_DIR=/opt/clash             каталог ресурсов mihomo (geodata и т.п.)
#   KEEP_RUNNING=1          не останавливать службу CSQTT в конце (шаг 18)
#
# Запуск: SSHHOST=192.168.1.1 SSHPASS=... sh scripts/smoke-test.sh

# shellcheck disable=SC2015
#   SC2015: A && ok || bad — ok/bad/sk всегда exit 0 (паттерн M5/M6).
set -u

SSHHOST="${SSHHOST:-192.168.1.1}"
SSHPORT="${SSHPORT:-22}"
SSHUSER="${SSHUSER:-root}"
WAIT_UP="${CSQTT_SMOKE_WAIT:-90}"
RESTART="${CSQTT_SMOKE_RESTART:-1}"
MIHOMO_BIN="${MIHOMO_BIN:-}"
MIHOMO_DIR="${MIHOMO_DIR:-/opt/clash}"
KEEP_RUNNING="${KEEP_RUNNING:-0}"
WORK="${TMPDIR:-/tmp}/csqtt-smoke"
mkdir -p "$WORK" 2>/dev/null || true

SSHOPTS="-o StrictHostKeyChecking=accept-new -o ConnectTimeout=10 -o ServerAliveInterval=15"
[ -n "${SSHKEY:-}" ] && SSHOPTS="$SSHOPTS -i $SSHKEY"
SSHAGENT="ssh"
if [ -n "${SSHPASS:-}" ] && command -v sshpass >/dev/null 2>&1; then
	SSHAGENT="sshpass -e ssh"
fi
# shellcheck disable=SC2086
RSSH_CMD="$SSHAGENT $SSHOPTS -p $SSHPORT $SSHUSER@$SSHHOST"

rsh() { $RSSH_CMD "$@"; }                # одна команда
rscript() { $RSSH_CMD sh -s; }           # скрипт со stdin

PASS=0; FAIL=0; SKIP=0
ok()   { PASS=$((PASS + 1)); echo "PASS - $1"; }
bad()  { FAIL=$((FAIL + 1)); echo "FAIL - $1"; }
sk()   { SKIP=$((SKIP + 1)); echo "SKIP - $1"; }
sec()  { echo; echo "=== $* ==="; }
redact() { sed -E 's/(password|session_token|success_token|vk_js_token|hashes|device_id)[=: ]+[^ ,"'"'"'}]*/\1=***/gi'; }

rsh_rc() { rsh "$@" >/dev/null 2>&1; }
has_remote() { rsh "command -v $1 >/dev/null 2>&1 && echo yes || echo no" 2>/dev/null | tr -d '\r'; }

echo "CSQTT M8 smoke-test: $SSHUSER@$SSHHOST:$SSHPORT (OpenWrt)"
if ! rsh 'echo SMOKE_OK' 2>"$WORK/ssh.err" | grep -q SMOKE_OK; then
	echo "FATAL: нет SSH-доступа к роутеру: $(head -1 "$WORK/ssh.err")"
	exit 3
fi
echo "SSH OK"

# Базовый снимок изоляции ДО любых действий (шаг 10/18).
snapshot() {
	rscript <<'EOS'
echo "## route"; ip route show | grep -v 'csqtt0' | sed 's/  */ /g' | sort
echo "## rule"; ip rule show
echo "## route6"; ip -6 route show | grep -v 'csqtt0' | sort
echo "## dhcp"; uci show dhcp 2>/dev/null | sed -E 's/(key|password)=.*/\1=***/'
echo "## resolv"; cat /etc/resolv.conf
echo "## nft_csqtt"; nft list ruleset 2>/dev/null | grep -ic csqtt
EOS
}
SNAP_BEFORE=$(snapshot)
printf '%s\n' "$SNAP_BEFORE" > "$WORK/before.snap"

sec "1. Пакеты"
for p in csqtt luci-app-csqtt; do
	if [ "$(rsh "apk info -e $p >/dev/null 2>&1 && echo yes || echo no" | tr -d '\r')" = yes ]; then
		ok "установлен пакет $p ($(rsh "apk version $p 2>/dev/null | tail -1" | tr -d '\r' | tr -s ' '))"
	else
		bad "пакет $p не установлен (apk add --allow-untrusted dist/$p...apk)"
	fi
done
[ "$(rsh 'apk info -e luci-i18n-csqtt-ru >/dev/null 2>&1 && echo yes || echo no' | tr -d '\r')" = yes ] \
	&& ok "установлен luci-i18n-csqtt-ru" || sk "luci-i18n-csqtt-ru не установлен (опционально)"

sec "2. Версия бинарника"
VER=$(rsh '/usr/bin/csqtt version 2>&1' | tr -d '\r' | head -1)
case "$VER" in *2.1.9*) ok "csqtt version: $VER" ;; *) bad "csqtt version неожиданна: $VER" ;; esac

sec "3. UCI enabled + service start + procd"
rsh 'uci set csqtt.main.enabled=1 && uci commit csqtt' >/dev/null 2>&1
if [ "$RESTART" = 1 ]; then
	DOCTOR_RC=$(rsh '/usr/bin/csqtt doctor >/dev/null 2>&1; echo $?' | tr -d '\r')
	if [ "${DOCTOR_RC:-2}" -ge 2 ]; then
		bad "doctor exit $DOCTOR_RC (>=2) — старт будет отменён (см. шаг 4)"
	else
		rsh '/etc/init.d/csqtt restart >/dev/null 2>&1; echo started' >/dev/null 2>&1
		ok "uci enabled=1, /etc/init.d/csqtt restart (doctor gate exit $DOCTOR_RC)"
	fi
fi
i=0; UP=0
while [ "$i" -lt "$WAIT_UP" ]; do
	if rsh 'pidof csqtt >/dev/null 2>&1 && echo yes || echo no' | grep -q yes; then UP=1; break; fi
	i=$((i + 2)); sleep 2
done
if [ "$UP" = 1 ]; then
	TUN_UP=$(rsh 'ip link show csqtt0 >/dev/null 2>&1 && echo yes || echo no' | tr -d '\r')
	ok "procd запустил службу CSQTT (pid $(rsh 'pidof csqtt' | tr -d '\r')); csqtt0=$TUN_UP"
else
	bad "служба CSQTT не поднялась за ${WAIT_UP}s (procd/doctor)"
fi

sec "4. csqtt doctor --json"
DOC=$(rsh '/usr/bin/csqtt doctor --json 2>/dev/null')
DOC_RC=$?
if echo "$DOC" | grep -q '"checks"'; then
	OKN=$(echo "$DOC" | grep -c '"status": "ok"')
	WN=$(echo "$DOC" | grep -c '"status": "warn"')
	FN=$(echo "$DOC" | grep -c '"status": "fail"')
	ok "doctor --json: ok=$OKN warn=$WN fail=$FN (exit $DOC_RC)"
else
	bad "doctor --json не вернул checks"
fi

sec "5. ip link csqtt0"
LINK=$(rsh 'ip -d link show csqtt0 2>&1' | tr -d '\r')
if echo "$LINK" | grep -q csqtt0; then
	ADDR=$(rsh "ip -4 -o addr show csqtt0 2>/dev/null | awk '{print \$4}'" | tr -d '\r')
	MTU=$(rsh "ip -o link show csqtt0 | sed -n 's/.*mtu \\([0-9]*\\).*/\\1/p'" | tr -d '\r')
	ok "csqtt0 UP: addr=$ADDR mtu=$MTU"
else
	sk "csqtt0 отсутствует (профиль не подключился — см. шаги 4/6)"
fi

sec "6. csqtt status --json"
ST=$(rsh '/usr/bin/csqtt status --json 2>/dev/null')
if echo "$ST" | grep -q '"daemon_state"'; then
	ACTIVE=$(echo "$ST" | sed -n 's/.*"active_profile": *"\([^"]*\)".*/\1/p' | head -1)
	STATE=$(echo "$ST" | sed -n 's/.*"state": *"\([^"]*\)".*/\1/p' | head -1)
	RX=$(echo "$ST" | sed -n 's/.*"rx_bytes": *\([0-9]*\).*/\1/p' | head -1)
	TX=$(echo "$ST" | sed -n 's/.*"tx_bytes": *\([0-9]*\).*/\1/p' | head -1)
	if [ -n "$ACTIVE" ]; then
		ok "status JSON: active_profile=$ACTIVE state=$STATE rx=$RX tx=$TX"
	else
		sk "status JSON валиден, но active_profile=null (нет доступного сервера/VK-хешей)"
	fi
else
	bad "status --json невалиден"
fi

sec "7. Логи (file+syslog) и маскировка секретов"
LOGF=/var/log/csqtt.log
S1=$(rsh "wc -c < $LOGF 2>/dev/null || echo 0" | tr -d '\r')
sleep 6
S2=$(rsh "wc -c < $LOGF 2>/dev/null || echo 0" | tr -d '\r')
[ "${S2:-0}" -gt "${S1:-0}" ] && ok "лог-файл растёт ($S1 -> $S2 байт)" \
	|| sk "лог-файл не растёт ($S1 -> $S2); служба CSQTT могла не логировать"
SYS=$(rsh 'logread 2>/dev/null | grep -c csqtt' | tr -d '\r')
[ "${SYS:-0}" -gt 0 ] && ok "syslog содержит записи csqtt ($SYS строк)" \
	|| sk "syslog без записей csqtt (logread buffer)"
if rsh "grep -qE 'password=[^ ]|session_token' $LOGF 2>/dev/null && echo leak || echo clean" | grep -q leak; then
	bad "в логе найдены признаки секретов (password=/session_token)"
else
	ok "секреты в логе не видны (redaction)"
fi

sec "8. LuCI HTTP"
if [ "$(has_remote curl)" = yes ]; then
	for js in status profiles captcha settings logs; do
		CODE=$(rsh "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1/luci-static/resources/view/csqtt/$js.js" | tr -d '\r')
		[ "$CODE" = 200 ] && ok "LuCI view $js.js → HTTP $CODE" || bad "LuCI view $js.js → HTTP $CODE"
	done
	PCODE=$(rsh "curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1/cgi-bin/luci/admin/services/csqtt/status" | tr -d '\r')
	case "$PCODE" in 200|302|401|403) ok "LuCI route /admin/services/csqtt/status → HTTP $PCODE (не 404)" ;; *) bad "LuCI route → HTTP $PCODE" ;; esac
	echo "     Ручной визуальный чек-лист: docs/TEST_PLAN.md §LuCI (5 вью, RU, captcha QR)."
else
	sk "curl недоступен на роутере — LuCI HTTP не проверен"
fi

sec "9. Respawn (procd)"
if rsh 'pidof csqtt >/dev/null 2>&1 && echo yes' | grep -q yes; then
	P1=$(rsh 'pidof csqtt' | tr -d '\r' | awk '{print $1}')
	rsh 'kill -9 '"$P1" >/dev/null 2>&1
	i=0; P2=""
	while [ "$i" -lt 15 ]; do sleep 1; P2=$(rsh 'pidof csqtt 2>/dev/null' | tr -d '\r' | awk '{print $1}'); [ -n "$P2" ] && [ "$P2" != "$P1" ] && break; i=$((i + 1)); done
	[ -n "$P2" ] && [ "$P2" != "$P1" ] && ok "procd respawn: $P1 -> $P2 за ≤${i}s" || bad "procd не перезапустил службу CSQTT за 15s"
else
	sk "служба CSQTT не запущена — respawn не проверить"
fi

sec "10. Изоляция (снимок до/после)"
SNAP_AFTER=$(snapshot)
printf '%s\n' "$SNAP_AFTER" > "$WORK/after.snap"
if [ "$SNAP_BEFORE" = "$SNAP_AFTER" ]; then
	ok "WAN/rules/routes6/dhcp/resolv/nft не изменились (snapshot before == after)"
else
	echo "--- diff (before -> after) ---"
	diff "$WORK/before.snap" "$WORK/after.snap" 2>/dev/null || true
	bad "сетевая конфигурация изменилась вне csqtt0 (см. diff)"
fi
CSQTT_RULES=$(rsh "ip rule show | grep -c csqtt" | tr -d '\r')
[ "${CSQTT_RULES:-0}" = 0 ] && ok "route rules от CSQTT отсутствуют (M3X: interface-only)" || bad "найдены ip rule, связанные с csqtt"
CSQTT_NFT=$(rsh "nft list ruleset 2>/dev/null | grep -ci csqtt" | tr -d '\r')
[ "${CSQTT_NFT:-0}" = 0 ] && ok "nft ruleset без правил CSQTT" || bad "в nft найдены правила CSQTT"

sec "11. rp_filter"
ALL=$(rsh 'cat /proc/sys/net/ipv4/conf/all/rp_filter 2>/dev/null' | tr -d '\r')
DEF=$(rsh 'cat /proc/sys/net/ipv4/conf/default/rp_filter 2>/dev/null' | tr -d '\r')
C0=$(rsh 'cat /proc/sys/net/ipv4/conf/csqtt0/rp_filter 2>/dev/null' | tr -d '\r')
if [ -z "$C0" ]; then
	sk "csqtt0 не поднят — rp_filter csqtt0=n/a (all=$ALL default=$DEF)"
elif [ "$C0" = 1 ]; then
	bad "csqtt0 rp_filter=1 (strict) — downlink будет резаться; лечить: sysctl net.ipv4.conf.csqtt0.rp_filter=2"
else
	ok "rp_filter: all=$ALL default=$DEF csqtt0=$C0 (не strict)"
fi

sec "12. Mihomo / user-traffic через csqtt0"
if [ -z "$MIHOMO_BIN" ]; then
	sk "MIHOMO_BIN не задан — шаг 12 пропущен (см. docs/TEST_PLAN.md: direct+interface-name:csqtt0, udp:true)"
elif [ "$(rsh "test -x $MIHOMO_BIN && echo yes || echo no" | tr -d '\r')" != yes ]; then
	bad "MIHOMO_BIN=$MIHOMO_BIN не найден на роутере"
else
	WAN_IP=$(rsh "curl -s --max-time 8 http://api.ipify.org" | tr -d '\r')
	rsh "cat > /tmp/csqtt-smoke-mihomo.yaml <<'YAML'
mixed-port: 17890
allow-lan: false
mode: rule
log-level: warning
proxies:
  - name: CSQTT
    type: direct
    interface-name: csqtt0
    udp: true
rules:
  - MATCH,CSQTT
YAML
pkill -f 'csqtt-smoke-mihomo.yaml' 2>/dev/null
$MIHOMO_BIN -d $MIHOMO_DIR -f /tmp/csqtt-smoke-mihomo.yaml >/tmp/csqtt-smoke-mihomo.log 2>&1 &
sleep 3" >/dev/null 2>&1
	TUN_IP=$(rsh "for i in 1 2 3 4 5 6 7 8; do r=\$(curl -s --max-time 6 -x http://127.0.0.1:17890 http://api.ipify.org); [ -n \"\$r\" ] && { echo \$r; break; }; sleep 1; done" | tr -d '\r' | tail -1)
	if [ -n "$TUN_IP" ] && [ "$TUN_IP" != "$WAN_IP" ]; then
		ok "Mihomo TCP через csqtt0: exit IP=$TUN_IP (WAN=$WAN_IP)"
	else
		bad "Mihomo TCP через csqtt0 не дал внешний IP (WAN=$WAN_IP tunnel='$TUN_IP')"
	fi
	tunnel_counters() {
		rscript <<'EOS'
r=$(sed -n 's/.*"rx_bytes": *\([0-9]*\).*/\1/p' /var/run/csqtt/status.json 2>/dev/null)
t=$(sed -n 's/.*"tx_bytes": *\([0-9]*\).*/\1/p' /var/run/csqtt/status.json 2>/dev/null)
echo "${r:-0}/${t:-0}"
EOS
	}
	P2=$(tunnel_counters | tr -d '\r')
	sleep 8
	P3=$(tunnel_counters | tr -d '\r')
	[ "$P2" != "$P3" ] && ok "CSQTT RX/TX растут под user-traffic (${P2} -> ${P3} байт)" \
		|| sk "CSQTT RX/TX не выросли (${P2} -> ${P3}) — трафик mihomo не дошёл/туннель idle"
	rsh "pkill -f 'csqtt-smoke-mihomo.yaml' 2>/dev/null" >/dev/null 2>&1
	echo "     UDP: mihomo udp:true + CSQTT TUN несёт UDP; полный UDP-пробник — ручной (нет dig/nc UDP-ассерта в sh)."
fi

sec "13-16. Failover / failback / fatal-auth / CAPTCHA"
NPROF=$(rsh 'uci show csqtt 2>/dev/null | grep -c "=server"' | tr -d '\r')
if [ "${NPROF:-0}" -ge 2 ]; then
	sk "13-16 требуют управляемых тестовых профилей/challenge — выполнять вручную (docs/TEST_PLAN.md §13-16)"
else
	sk "13-16: в конфиге $NPROF профиль(ей) — failover/failback/CAPTCHA-сценарии не автоматизируются без второго сервера/VK-challenge"
fi

sec "17. Workers / MT7981 (CPU/RAM/temp)"
INFO=$(rsh 'cat /proc/loadavg; free -m | sed -n "2p"; cat /sys/class/thermal/thermal_zone0/temp 2>/dev/null; /usr/bin/csqtt status --json 2>/dev/null | grep -E "\"configured\"|\"active\""' | tr -d '\r')
echo "     loadavg/free/temp: $(echo "$INFO" | tr '\n' ' ')"
WK=$(rsh '/etc/init.d/csqtt status 2>/dev/null | grep -i workers' | tr -d '\r')
[ -n "$WK" ] && ok "$(echo "$WK" | tr -s ' ')" || sk "workers недоступны (служба CSQTT без активного профиля)"

sec "18. Stop / teardown"
if [ "$KEEP_RUNNING" = 1 ]; then
	sk "KEEP_RUNNING=1 — служба CSQTT оставлена запущенной"
else
	rsh '/etc/init.d/csqtt stop >/dev/null 2>&1' >/dev/null 2>&1
	sleep 2
	[ "$(rsh 'ip link show csqtt0 >/dev/null 2>&1 && echo yes || echo no' | tr -d '\r')" = no ] \
		&& ok "stop: csqtt0 удалён" || bad "stop: csqtt0 остался"
	SNAP_STOP=$(snapshot)
	printf '%s\n' "$SNAP_STOP" > "$WORK/stop.snap"
	[ "$SNAP_BEFORE" = "$SNAP_STOP" ] && ok "после stop сеть вернулась к исходному снимку (удалять больше нечего — M3X)" \
		|| { echo "--- diff (before -> stop) ---"; diff "$WORK/before.snap" "$WORK/stop.snap" 2>/dev/null || true; bad "после stop сеть отличается от исходной"; }
fi

sec "19. Upgrade persistence"
PID_DEV=$(rsh 'csqtt profile list 2>/dev/null | grep -c .' | tr -d '\r')
if [ "${PID_DEV:-0}" -gt 1 ]; then
	BEFORE_CFG=$(rsh 'uci show csqtt 2>/dev/null | sed -E "s/(password|vk|device_id)=.*/\1=***/" | sort' | tr -d '\r')
	rsh 'apk fix csqtt >/dev/null 2>&1' >/dev/null 2>&1
	AFTER_CFG=$(rsh 'uci show csqtt 2>/dev/null | sed -E "s/(password|vk|device_id)=.*/\1=***/" | sort' | tr -d '\r')
	[ "$BEFORE_CFG" = "$AFTER_CFG" ] && ok "apk fix csqtt: UCI-конфиг (профили/device-id/enabled) сохранён" \
		|| bad "конфиг изменился после apk fix"
else
	sk "конфиг без профилей — upgrade persistence не на чем проверить"
fi

echo
echo "M8 smoke-test: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
[ "$FAIL" -eq 0 ] && { echo "RESULT: PASS"; exit 0; } || { echo "RESULT: FAIL"; exit 1; }
