#!/usr/bin/env python3
"""Диагностика bound-сокета на dummy-интерфейсе без адреса."""
import socket
import sys

ifname = sys.argv[1]
test_ip = sys.argv[2]

# UDP bound на интерфейс БЕЗ адреса и без маршрута
s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
s.setsockopt(socket.SOL_SOCKET, 25, ifname.encode())  # SO_BINDTODEVICE
try:
    s.sendto(b"m3x", (test_ip, 53))
    print(f"bound-udp-noaddr: SENT local={s.getsockname()}")
except OSError as e:
    print(f"bound-udp-noaddr: FAIL errno={e.errno} {e}")
s.close()
