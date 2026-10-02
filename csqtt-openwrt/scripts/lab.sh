#!/usr/bin/env bash
# lab.sh — M3X фаза B: routing lab в WSL (root).
#
# Последовательность: схема A (interface-only, без route) → B (table+ip rule oif)
# → C (high-metric main default). После каждой схемы — teardown и сравнение
# unbound vs bound socket. НИКОГДА не трогает реальный default WSL.
#
# Использование: sudo bash lab.sh   (из csqtt-openwrt/scripts/)
set -u
IF="m3xlab0"
TABLE="760"
PRIORITY="760"
TEST_IP="1.1.1.1"          # внешний IP (не VK/TURN!)
LAB_ADDR="10.77.0.2/32"
DIR="$(cd "$(dirname "$0")" && pwd)"
HELPER="$DIR/tun_helper.py"

fail() { echo "LAB-FAIL: $*"; exit 1; }

section() { echo; echo "=== $* ==="; }

snapshot() {
  echo "--- ip route show (main) ---"
  ip route show
  echo "--- ip rule show ---"
  ip rule show
}

check() {
  local label="$1"
  section "CHECK $label"
  python3 "$HELPER" check "$IF" "$TEST_IP"
}

teardown_table() {
  section "TEARDOWN table/rule"
  ip rule del priority "$PRIORITY" 2>/dev/null || true
  ip route flush table "$TABLE" 2>/dev/null || true
  ip route del table "$TABLE" 2>/dev/null || true
  echo "rules now:"; ip rule show
}

# ---------- Старт ----------
section "SNAPSHOT BEFORE"
snapshot
ip link show "$IF" >/dev/null 2>&1 && { echo "m3xlab0 уже существует — удаляю"; ip link del "$IF"; }
ip rule show | grep -q "priority $PRIORITY" && fail "rule $PRIORITY уже существует — коллизия"

# создаём TUN (fd держит фоновый python-процесс)
section "CREATE TUN $IF"
python3 "$HELPER" create "$IF" > /tmp/lab_create.out 2>&1 &
CREATE_PID=$!
sleep 1
cat /tmp/lab_create.out
ip link show "$IF" >/dev/null 2>&1 || fail "TUN не создан"
ip addr add "$LAB_ADDR" dev "$IF"
ip link set "$IF" up
ip addr show "$IF" | grep inet

# ---------- Схема A: interface-only, без route ----------
check "A: interface-only (нет route)"

# ---------- Схема B: отдельная table + ip rule oif ----------
section "B: add table $TABLE + rule oif"
ip route add default dev "$IF" table "$TABLE" || fail "table default add"
ip rule add priority "$PRIORITY" oif "$IF" lookup "$TABLE" || fail "ip rule oif add"
ip route show table "$TABLE"
ip rule show
check "B: table+rule(oif)"

# ---------- Схема B: UDP/дополнительные проверки через кернел ----------
section "B: ip route get с oif (после rule)"
ip route get "$TEST_IP" oif "$IF" || true

# ---------- Схема C: high-metric main default (fallback) ----------
section "C: high-metric default в main"
# (снимаем B перед C, чтобы сравнивать изолированно)
teardown_table
ip route add default dev "$IF" metric 32000 || fail "high-metric default add"
ip route show | grep m3xlab
check "C: high-metric main default 32000"
ip route del default dev "$IF" metric 32000 || true

# ---------- Финальный teardown ----------
section "FINAL TEARDOWN"
ip route del default dev "$IF" metric 32000 2>/dev/null || true
teardown_table
kill $CREATE_PID 2>/dev/null || true
sleep 0.5
ip link show "$IF" >/dev/null 2>&1 && ip link del "$IF"
section "SNAPSHOT AFTER"
snapshot
echo
echo "LAB-DONE"
