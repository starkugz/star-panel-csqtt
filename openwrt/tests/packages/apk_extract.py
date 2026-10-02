#!/usr/bin/env python3
# M7: распаковка apk-tools v3 (.apk). Формат — чанки с 4-байтным magic
# (ADBk/ADBd/...) + big-endian длина + payload (gzip-поток). Надёжно: сканируем
# на gzip-magic (1f 8b 08) и декодируем каждый zlib-поток, затем tar-листим.
import sys, io, gzip, zlib, tarfile

def gunzip_stream(data, start):
    do = zlib.decompressobj(16 + zlib.MAX_WBITS)
    out = do.decompress(data[start:])
    return out

def find_gzip_offsets(d):
    offs, i = [], 0
    while True:
        j = d.find(b'\x1f\x8b\x08', i)
        if j < 0:
            break
        offs.append(j)
        i = j + 3
    return offs

def main():
    d = open(sys.argv[1], 'rb').read()
    pkginfo = None
    files = []
    for off in find_gzip_offsets(d):
        try:
            raw = gunzip_stream(d, off)
        except Exception:
            continue
        try:
            tf = tarfile.open(fileobj=io.BytesIO(raw))
        except Exception:
            continue
        for m in tf.getmembers():
            name = m.name[2:] if m.name.startswith('./') else m.name
            if name in ('.', ''):
                continue
            if name == '.PKGINFO' and not m.isdir():
                pkginfo = tf.extractfile(m).read().decode('utf-8', 'replace')
            elif not m.isdir() and not name.startswith('.'):
                files.append((name, oct(m.mode)[-3:]))
    print("=== .PKGINFO ===")
    print(pkginfo or "(нет)")
    print("=== FILES (mode path) ===")
    for name, mode in sorted(set(files)):
        print(f"{mode} {name}")

if __name__ == '__main__':
    main()
