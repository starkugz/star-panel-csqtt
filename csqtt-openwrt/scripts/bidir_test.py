#!/usr/bin/env python3
"""bidir_test.py — M3X фаза B: доказательство двунаправленного потока через TUN
для bound (SO_BINDTODEVICE) сокета при разных схемах маршрутизации и rp_filter.

Модельрует пару «Mihomo ↔ служба CSQTT»:
  OUT: bound-сокет sendto → пакет появляется в TUN fd (его читает служба CSQTT);
  IN:  служба CSQTT пишет в TUN fd ответный IP-пакет (src=внешний IP, dst=адрес TUN)
       → bound-сокет получает его через recvfrom.

Матрица: scheme × rp_filter:
  A — interface-only, НЕТ route (production-режим v4 по умолчанию);
  B — отдельная table + `ip rule oif <if> lookup <table>` (кандидат-M3X-исключение).

Эффективный rp_filter интерфейса = max(conf.all, conf.<if>) — в каждом случае
ставим ОБА значения, после теста восстанавливаем исходные.

Ожидания (кодированы в EXPECTED, расхождение = exit 1):
  OUT (kernel on-link fallback при oif) работает в обеих схемах;
  IN при rp_filter=1 (strict): DROP в схеме A (нет обратного маршрута через if)
        И в схеме B (реверс rp_filter — fib_lookup(iif=...), rule oif не влияет);
  IN при rp_filter=0/2 — OK в обеих схемах;
  unbound-сокет всегда выбирает WAN (eth0) — изоляция.

Только для лаборатории (WSL root). НЕ является production-кодом порта.
Никогда не трогает default WSL. Использование:
  sudo python3 bidir_test.py [--ifname m3xlab0] [--addr 10.77.0.2] \
      [--test-ip 1.1.1.1] [--table 760] [--priority 760]
"""
import argparse
import fcntl
import os
import select
import socket
import struct
import subprocess
import sys

TUNSETIFF = 0x400454CA  # _IOW('T', 202, 4)
IFF_TUN = 0x0001
IFF_NO_PI = 0x1000
SOL_BINDTODEVICE = 25  # Linux SO_BINDTODEVICE
PROBE_PORT = 44444  # порт «внешнего» UDP-сервиса в тесте
WAN_IF = "eth0"  # аплинк WSL (для проверки изоляции unbound-трафика)

PROBE_OUT = b"m3x-probe-outbound"
PROBE_IN = b"m3x-reply-inbound"


def ip_cmd(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(("ip",) + args, capture_output=True, text=True, check=False)


def sysctl_read(name: str) -> str:
    path = "/proc/sys/net/ipv4/conf/" + name + "/rp_filter"
    with open(path, "r", encoding="ascii") as f:
        return f.read().strip()


def sysctl_write(name: str, value: str) -> None:
    path = "/proc/sys/net/ipv4/conf/" + name + "/rp_filter"
    with open(path, "w", encoding="ascii") as f:
        f.write(value + "\n")


def tun_create(ifname: str) -> tuple[int, str]:
    fd = os.open("/dev/net/tun", os.O_RDWR)
    ifr = struct.pack("16sH", ifname.encode(), IFF_TUN | IFF_NO_PI)
    ifr += b"\x00" * (40 - len(ifr))  # докладываем до sizeof(struct ifreq)
    res = fcntl.ioctl(fd, TUNSETIFF, ifr)
    actual = res[:16].split(b"\x00")[0].decode()
    return fd, actual


def checksum(data: bytes) -> int:
    if len(data) % 2:
        data += b"\x00"
    s = 0
    for i in range(0, len(data), 2):
        s += (data[i] << 8) | data[i + 1]
    while s >> 16:
        s = (s & 0xFFFF) + (s >> 16)
    return (~s) & 0xFFFF


def build_udp_ip(src: str, dst: str, sport: int, dport: int, payload: bytes) -> bytes:
    """IPv4 + UDP пакет (checksum UDP = 0 — легально для IPv4)."""
    udp = struct.pack("!HHHH", sport, dport, 8 + len(payload), 0) + payload
    total = 20 + len(udp)
    src_b = socket.inet_aton(src)
    dst_b = socket.inet_aton(dst)
    hdr = struct.pack("!BBHHHBBH4s4s", 0x45, 0, total, 1, 0, 64, 17, 0, src_b, dst_b)
    csum = checksum(hdr)
    hdr = struct.pack("!BBHHHBBH4s4s", 0x45, 0, total, 1, 0, 64, 17, csum, src_b, dst_b)
    return hdr + udp


def parse_ip(pkt: bytes) -> tuple[int, str, str, bytes]:
    ihl = (pkt[0] & 0xF) * 4
    proto = pkt[9]
    src = socket.inet_ntoa(pkt[12:16])
    dst = socket.inet_ntoa(pkt[16:20])
    return proto, src, dst, pkt[ihl:]


def tun_read(fd: int, timeout: float) -> bytes | None:
    ready, _, _ = select.select([fd], [], [], timeout)
    if not ready:
        return None
    return os.read(fd, 65536)


def bound_socket(ifname: str) -> socket.socket:
    s = socket.socket(socket.AF_INET, socket.SOCK_DGRAM)
    s.setsockopt(socket.SOL_SOCKET, SOL_BINDTODEVICE, ifname.encode())
    return s


def case(scheme: str, rp: str, ifname: str, if_addr: str,
         test_ip: str, table: int, priority: int) -> dict:
    """Один прогон матрицы: свой TUN, свои table/rule, teardown, честные метки."""
    res = {"out": False, "in": False, "tcp_syn": False, "unbound_wan": False,
           "route_get_oif": "", "err": ""}

    ip_cmd("link", "del", ifname)  # best-effort очистка от прошлого прогона
    fd, actual = tun_create(ifname)
    if actual != ifname:
        res["err"] = f"tun actual name {actual}"
        os.close(fd)
        return res
    ip_cmd("addr", "add", f"{if_addr}/32", "dev", ifname)
    ip_cmd("link", "set", ifname, "up")

    if scheme == "B":
        ip_cmd("route", "add", "default", "dev", ifname, "table", str(table))
        rule = ip_cmd("rule", "add", "priority", str(priority),
                      "oif", ifname, "lookup", str(table))
        if rule.returncode != 0:
            res["err"] = "ip rule add failed: " + rule.stderr.strip()
            os.close(fd)
            return res
    elif scheme != "A":
        res["err"] = f"unknown scheme {scheme}"
        os.close(fd)
        return res

    sysctl_write("all", rp)
    sysctl_write(ifname, rp)

    # --- изоляция: unbound lookup обязан идти через WAN ---
    un = ip_cmd("route", "get", test_ip)
    res["unbound_wan"] = ("dev " + WAN_IF) in un.stdout
    rg = ip_cmd("route", "get", test_ip, "oif", ifname)
    res["route_get_oif"] = rg.stdout.strip().replace("\n", " | ")

    # --- OUT: bound UDP sendto → пакет в TUN fd ---
    s = bound_socket(ifname)
    try:
        s.sendto(PROBE_OUT, (test_ip, PROBE_PORT))
        pkt = tun_read(fd, 3.0)
        while pkt is not None:
            proto, src, dst, _ = parse_ip(pkt)
            if proto == 17 and src == if_addr and dst == test_ip:
                res["out"] = True
                break
            pkt = tun_read(fd, 1.0)
        # --- IN: крафтим ответ в TUN fd → bound recvfrom ---
        if res["out"]:
            local_port = s.getsockname()[1]
            reply = build_udp_ip(test_ip, if_addr, PROBE_PORT, local_port, PROBE_IN)
            os.write(fd, reply)
            s.settimeout(3.0)
            try:
                data, peer = s.recvfrom(2048)
                res["in"] = (data == PROBE_IN and peer[0] == test_ip)
            except OSError:
                res["in"] = False
    except OSError as e:
        res["err"] += f" udp-stage: {e}"
    finally:
        s.close()

    # --- TCP: bound connect → SYN виден в TUN (без ответа, только OUT-доказательство) ---
    try:
        t = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        t.setsockopt(socket.SOL_SOCKET, SOL_BINDTODEVICE, ifname.encode())
        t.setblocking(False)
        try:
            t.connect((test_ip, 443))
        except (BlockingIOError, OSError):
            pass  # EINPROGRESS — SYN уже отправлен ядром
        deadline_end = 3.0
        while deadline_end > 0 and not res["tcp_syn"]:
            pkt = tun_read(fd, 1.0)
            if pkt is None:
                deadline_end -= 1.0
                continue
            proto, src, dst, _ = parse_ip(pkt)
            if proto == 6 and src == if_addr and dst == test_ip:
                res["tcp_syn"] = True
            deadline_end -= 1.0
        t.close()
    except OSError as e:
        res["err"] += f" tcp-stage: {e}"

    # --- teardown только своих артефактов ---
    if scheme == "B":
        ip_cmd("rule", "del", "priority", str(priority))
        ip_cmd("route", "flush", "table", str(table))
    os.close(fd)  # закрытие fd уничтожает TUN-интерфейс
    ip_cmd("link", "del", ifname)  # best-effort, если интерфейс ещё жив
    return res


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("--ifname", default="m3xlab0")
    ap.add_argument("--addr", default="10.77.0.2")
    ap.add_argument("--test-ip", default="1.1.1.1")
    ap.add_argument("--table", type=int, default=760)
    ap.add_argument("--priority", type=int, default=760)
    args = ap.parse_args()

    if os.geteuid() != 0:
        print("нужен root (TUN + sysctl): sudo python3 bidir_test.py")
        return 2
    orig_all = sysctl_read("all")
    orig_def = sysctl_read("default")

    print(f"ifname={args.ifname} addr={args.addr} test_ip={args.test_ip} "
          f"table={args.table} priority={args.priority}")
    print(f"rp_filter orig: all={orig_all} default={orig_def}")
    print("--- ip rule show before ---")
    print(ip_cmd("rule", "show").stdout.strip())

    # Ожидания (ядро Linux 6.6, проверено M3X): OUT работает всегда (on-link
    # fallback при oif-lookup); downlink режется strict rp_filter=1 в ОБЕИХ
    # схемах (реверс fib_lookup(iif=...), rule oif не влияет) — см. REFERENCE_MAP.md.
    expected = {
        ("A", "1"): {"out": True, "in": False},
        ("A", "0"): {"out": True, "in": True},
        ("A", "2"): {"out": True, "in": True},
        ("B", "1"): {"out": True, "in": False},
        ("B", "0"): {"out": True, "in": True},
        ("B", "2"): {"out": True, "in": True},
    }

    results = {}
    for scheme in ("A", "B"):
        for rp in ("1", "2", "0"):
            r = case(scheme, rp, args.ifname, args.addr,
                     args.test_ip, args.table, args.priority)
            results[(scheme, rp)] = r
            in_str = "OK" if r["in"] else "DROP"
            out_str = "OK" if r["out"] else "FAIL"
            print(f"CASE scheme={scheme} rp_filter={rp}: out={out_str} in={in_str} "
                  f"tcp_syn={'OK' if r['tcp_syn'] else 'FAIL'} "
                  f"unbound_wan={'OK' if r['unbound_wan'] else 'FAIL'}"
                  + (f" ERR={r['err']}" if r["err"] else ""))
            print(f"  route_get_oif: {r['route_get_oif']}")

    # восстановление окружения
    sysctl_write("all", orig_all)
    sysctl_write("default", orig_def)
    ip_cmd("link", "del", args.ifname)
    print("--- ip rule show after (должно совпадать с before) ---")
    after = ip_cmd("rule", "show").stdout.strip()
    print(after)
    ip_cmd("route", "flush", "table", str(args.table))

    mismatch = []
    for (scheme, rp), exp in expected.items():
        got = results[(scheme, rp)]
        for key in ("out", "in"):
            if got[key] != exp[key]:
                mismatch.append(f"scheme={scheme} rp={rp} {key}: "
                               f"получено {got[key]}, ожидалось {exp[key]}")
    for (scheme, rp), got in results.items():
        if not got["tcp_syn"]:
            mismatch.append(f"scheme={scheme} rp={rp} tcp_syn: не увиден SYN в TUN")
        if not got["unbound_wan"]:
            mismatch.append(f"scheme={scheme} rp={rp}: unbound-сокет ушёл не в {WAN_IF}")

    print()
    if mismatch:
        print("MISMATCH (отклонение от ожиданий — разобрать вручную):")
        for m in mismatch:
            print("  " + m)
        return 1
    print("ALL-CASES: соответствуют ожиданиям (OUT ok всегда; IN DROP только при "
          "strict rp_filter=1 в A и B; unbound всегда " + WAN_IF + "; teardown чистый)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
