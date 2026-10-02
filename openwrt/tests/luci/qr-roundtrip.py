#!/usr/bin/env python3
# -*- coding: utf-8 -*-
# [M6d] Независимый декодер QR для теста qr-roundtrip.js: читает JSON матриц
# (stdin), демаскирует/деинтерливит/парсит byte-mode и сверяет со строкой.
# Дополнительно (если установлен segno) сверяет function-область матрицы
# (finder/timing/format/version/alignment) с эталоном при том же маске.
# Выход: строки "ok - ..." / "FAIL - ..."; exit 1 при любом FAIL.
import json
import sys

ECM = [[10, 1, 16, 0, 0], [16, 1, 28, 0, 0], [26, 1, 44, 0, 0],
       [18, 2, 32, 0, 0], [24, 2, 43, 0, 0], [16, 4, 27, 0, 0],
       [18, 4, 31, 0, 0], [22, 2, 38, 2, 39], [22, 3, 36, 2, 37],
       [26, 4, 43, 1, 44]]
ALIGN = [[], [6, 18], [6, 22], [6, 26], [6, 30],
         [6, 34], [6, 22, 38], [6, 24, 42], [6, 26, 46], [6, 28, 50]]


def funcmap(size, ver):
    f = [[False] * size for _ in range(size)]

    def box(y0, y1, x0, x1):
        for y in range(max(y0, 0), min(y1, size - 1) + 1):
            for x in range(max(x0, 0), min(x1, size - 1) + 1):
                f[y][x] = True

    box(0, 7, 0, 7)
    box(0, 7, size - 8, size - 1)
    box(size - 8, size - 1, 0, 7)
    for i in range(size):
        f[6][i] = True
        f[i][6] = True
    for i in range(9):
        f[8][i] = True
        f[i][8] = True
    for i in range(8):
        f[8][size - 1 - i] = True
        f[size - 1 - i][8] = True
    if ver >= 7:
        box(size - 11, size - 9, 0, 5)
        box(0, 5, size - 11, size - 9)
    ac = ALIGN[ver - 1]
    for ay in ac:
        for ax in ac:
            if (ay == 6 and ax == 6) or (ay == 6 and ax == size - 7) \
                    or (ay == size - 7 and ax == 6):
                continue
            box(ay - 2, ay + 2, ax - 2, ax + 2)
    return f


def unmask(mod, f, size, mask):
    o = [[0] * size for _ in range(size)]
    for y in range(size):
        for x in range(size):
            v = mod[y][x]
            if not f[y][x]:
                inv = {0: (x + y) % 2 == 0, 1: y % 2 == 0, 2: x % 3 == 0,
                       3: (x + y) % 3 == 0, 4: (x // 3 + y // 2) % 2 == 0,
                       5: (x * y) % 2 + (x * y) % 3 == 0,
                       6: ((x * y) % 2 + (x * y) % 3) % 2 == 0,
                       7: ((x + y) % 2 + (x * y) % 3) % 2 == 0}[mask]
                if inv:
                    v ^= 1
            o[y][x] = v
    return o


def readcw(mod, f, size):
    bits = []
    right = size - 1
    while right >= 1:
        if right == 6:
            right = 5
        for vert in range(size):
            for j in range(2):
                x = right - j
                up = ((right + 1) & 2) == 0
                y = size - 1 - vert if up else vert
                if not f[y][x]:
                    bits.append(mod[y][x])
        right -= 2
    cw = []
    for i in range(0, len(bits) // 8 * 8, 8):
        b = 0
        for k in range(8):
            b = (b << 1) | bits[i + k]
        cw.append(b)
    return cw


def data_codewords(cw, ver):
    eclen, b1, d1, b2, d2 = ECM[ver - 1]
    nb = b1 + b2
    lens = [d1] * b1 + [d2] * b2
    blocks = [[] for _ in range(nb)]
    pos = 0
    for i in range(max(lens)):
        for bi in range(nb):
            if i < lens[bi]:
                blocks[bi].append(cw[pos])
                pos += 1
    data = bytearray()
    for bl in blocks:
        data.extend(bl)
    return data


def decode(rows, ver, mask):
    size = len(rows)
    mod = [[int(c) for c in r] for r in rows]
    f = funcmap(size, ver)
    cw = readcw(unmask(mod, f, size, mask), f, size)
    data = data_codewords(cw, ver)
    bits = []
    for byte in data:
        for k in range(7, -1, -1):
            bits.append((byte >> k) & 1)
    idx = [0]

    def take(n):
        v = 0
        for _ in range(n):
            v = (v << 1) | bits[idx[0]]
            idx[0] += 1
        return v

    mode = take(4)
    if mode != 4:
        raise ValueError("mode %d != byte(4)" % mode)
    cc = take(8 if ver < 10 else 16)
    payload = bytes(take(8) for _ in range(cc))
    return payload.decode("utf-8")


def main():
    items = json.load(sys.stdin)
    fails = 0
    try:
        import segno
        have_segno = True
    except ImportError:
        have_segno = False
        print("note - segno не установлен: сверка function-области пропущена")
    for it in items:
        try:
            got = decode(it["rows"], it["version"], it["mask"])
        except Exception as exc:
            print("FAIL - roundtrip v%s :: %r" % (it["version"], exc))
            fails += 1
            continue
        if got != it["text"]:
            print("FAIL - roundtrip mismatch: %r != %r" % (got[:30], it["text"][:30]))
            fails += 1
            continue
        print("ok - roundtrip v%s mask%s len%d" % (it["version"], it["mask"], len(it["text"])))
        if have_segno:
            q = segno.make(it["text"], error="m", micro=False, mode="byte",
                           version=it["version"], mask=it["mask"], boost_error=False)
            ref = [[int(c) for c in r] for r in q.matrix]
            mine = [[int(c) for c in r] for r in it["rows"]]
            f = funcmap(it["size"], it["version"])
            bad = [(y, x) for y in range(it["size"]) for x in range(it["size"])
                   if f[y][x] and ref[y][x] != mine[y][x]]
            if bad:
                print("FAIL - function-область отличается от segno в %d модулях (v%s)"
                      % (len(bad), it["version"]))
                fails += 1
            else:
                print("ok - function-область == segno (v%s mask%s)" % (it["version"], it["mask"]))
    return 1 if fails else 0


if __name__ == "__main__":
    sys.exit(main())
