#!/bin/sh
# probe_router.sh — быстрая проверка доступности роутера по SSH.
LOG=/tmp/probe_router.log
: > "$LOG"
timeout 12 ssh -o ConnectTimeout=8 -o BatchMode=yes root@192.168.1.1 'echo ROUTER_OK; cat /etc/openwrt_version 2>/dev/null; sysctl net.ipv4.conf.all.rp_filter net.ipv4.conf.default.rp_filter 2>/dev/null; nft list ruleset >/dev/null 2>&1 && echo nft_ok' >> "$LOG" 2>&1
echo "ssh_rc=$?" >> "$LOG"
cat "$LOG"
