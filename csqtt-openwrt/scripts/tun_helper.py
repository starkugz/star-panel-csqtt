#!/usr/bin/env python3
"""tun_helper.py — маленький test helper для M3X routing lab.

Использование:
  python3 tun_helper.py create  <ifname>            — создать TUN (IFF_TUN|IFF_NO_PI)
  python3 tun_helper.py check   <ifname> <test-ip>   — диагностика маршрутизации:
      1) ip route get <test-ip>                       (unbound view)
      2) ip route get <test-ip> oif <ifname>          (oif view)
      3) unbound socket → connect/sendto             (какой src/if)
      4) bound socket (SO_BINDTODEVICE) → connect/sendto
  python3 tun_helper.py cleanup  <ifname>             — удалить TUN
  python3 tun_helper.py table-cleanup <table>         — очистить таблицу маршрутов

Не является частью production-кода порта; только лабораторный инструмент M3X.
"""
import socket
import struct
import subprocess
import sys

TUNSETIFF = 0x400454CA  # _IOW('T', 202, 4)
IFF_TUN = 0x0001
IFF_NO_PI = 0x1000
DEFAULT_MTU = 1400


def run(cmd: list[str]) -> str:
    return subprocess.run(
        cmd, capture_output=True, text=True, check=False
    ).stdout.strip()


def create(ifname: str) -> int:
    fd = os_open("/dev/net/tun", os.O_RDWR)
    ifr = struct.pack("16sH", ifname.encode(), IFF_TUN | IFF_NO_PI)
    # len 18 < 40 байт ifreq; докладываем нулями до sizeof(struct ifreq)=40
    ifr += b"\x00" * (40 - len(ifr))
    r = fcntl.ioctl(fd, TUNSETIFF, ifr)
    # вернуть фактическое имя
    actual = r[:16].split(b"\x00")[0].decode()
    print(f"created:{actual}")
    return fd


def os_open(path: str, flags: int) -> int:
    return os.open(path, flags)


import os  # noqa: E402
import fcntl  # noqa: E402


def check(ifname: str, test_ip: str) -> None:
    print(f"== ip route get {test_ip} (unbound) ==")
    print(run(["ip", "route", "get", test_ip]))
    print(f"== ip route get {test_ip} oif {ifname} ==")
    out = run(["ip", "route", "get", test_ip, "oif", ifname])
    print(out if out else "(empty)")

    # unbound TCP socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.settimeout(3)
        s.connect((test_ip, 443))
        print(f"unbound-tcp: connected local={s.getsockname()}")
        s.close()
    except OSError as e:
        print(f"unbound-tcp: FAIL {e}")

    # bound TCP socket
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        s.setsockopt(socket.SOL_SOCKET, 25, ifname.encode())  # SO_BINDTODEVICE
        s.settimeout(3)
        s.connect((test_ip, 443))
        print(f"bound-tcp: connected local={s.getsockname()}")
        s.close()
    except OSError as e:
        print(f"bound-tcp: FAIL {e}")

    # unbound UDP
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.sendto(b"m3x", (test_ip, 53))
        print(f"unbound-udp: sent local={s.getsockname()}")
        s.close()
    except OSError as e:
        print(f"unbound-udp: FAIL {e}")

    # bound UDP
    try:
        s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
        s.setsockopt(socket.SOL_SOCKET, 25, ifname.encode())  # SO_BINDTODEVICE
        s.sendto(b"m3x", (test_ip, 53))
        print(f"bound-udp: sent local={s.getsockname()}")
        s.close()
    except OSError as e:
        print(f"bound-udp: FAIL {e}")


def cleanup(ifname: str) -> None:
    run(["ip", "link", "del", ifname])
    print(f"cleaned:{ifname}")


def table_cleanup(table: str) -> None:
    # flush table then delete table (если поддержка есть)
    run(["ip", "route", "flush", "table", table])
    print(f"table-cleaned:{table}")


if __name__ == "__main__":
    if len(sys.argv) < 3:
        print(__doc__)
        sys.exit(2)
    mode = sys.argv[1]
    if mode == "create":
        fd = create(sys.argv[2])
        # держать fd открытым, пока жив процесс — иначе интерфейс исчезнет.
        # НО: лабе нужно, чтобы интерфейс жил между запусками скрипта.
        # Поэтому переводим fd в daemon-режим: держим навсегда.
        import time

        while True:
            time.sleep(3600)
    elif mode == "check":
        check(sys.argv[2], sys.argv[3])
    elif mode == "cleanup":
        cleanup(sys.argv[2])
    elif mode == "table-cleanup":
        table_cleanup(sys.argv[2])
    else:
        print(__doc__)
        sys.exit(2)
