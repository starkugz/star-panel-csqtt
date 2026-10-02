#!/bin/bash
# SPDX-FileCopyrightText: 2026 amurcanov
# SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0
#
# [M7] Верификация собранных .apk.
# Формат apk-tools v3 — упакованный ("ADBd" + raw-deflate -> "ADB.pckg"), не
# POSIX-tar, поэтому прямой `tar -t` к нему неприменим. Проверяем двумя
# взаимодополняющими способами:
#   (A) .pkgdir/<name> — авторитетное дерево+права, из которых `apk mkpkg`
#       собрал пакет (SDK build_dir);
#   (B) декомпрессия САМОГО .apk из dist/ и поиск встроенных путей/depends —
#       доказательство, что именно этот файл содержит нужное и НЕ содержит
#       firewall/uci-defaults/dnsmasq/ip-full.
# Запуск (WSL/Linux):  sh openwrt/tests/packages/verify-apk.sh
# shellcheck disable=SC2015  # A && ok || bad — ok/bad всегда exit 0 (паттерн M5/M6)
set -u
ROOT=$(cd "$(dirname "$0")/../../.." && pwd)
DIST="$ROOT/dist"
SDK="${CSQTT_SDK:-$HOME/csqtt-m7/sdk/openwrt-sdk}"
CB="$SDK/build_dir/target-aarch64_cortex-a53_musl"
PY=python3

PASS=0; FAIL=0; SKIP=0
ok(){ echo "ok   - $1"; PASS=$((PASS+1)); }
bad(){ echo "FAIL - $1"; FAIL=$((FAIL+1)); }
sk(){ echo "SKIP - $1"; SKIP=$((SKIP+1)); }

[ -d "$DIST" ] || { echo "ERROR: нет $DIST (сначала scripts/build-all.sh)"; exit 2; }
command -v "$PY" >/dev/null 2>&1 || { echo "ERROR: нужен python3"; exit 2; }

# Версии пакетов и ядра раздельны (ядро 2.1.9, пакет интеграции/панель 1.0.0),
# поэтому имена файлов в dist/ и build-деревья SDK определяем по маске, а не
# по зашитому номеру версии.
APK_CORE=$(find "$DIST" -maxdepth 1 -name 'csqtt_*_aarch64_cortex-a53.apk' | head -1)
APK_LUCI=$(find "$DIST" -maxdepth 1 -name 'luci-app-csqtt_*_all.apk' | head -1)
APK_I18N=$(find "$DIST" -maxdepth 1 -name 'luci-i18n-csqtt-ru*.apk' | head -1)
[ -n "$APK_CORE" ] || { echo "ERROR: нет csqtt_*_aarch64_cortex-a53.apk в $DIST"; exit 2; }
[ -n "$APK_LUCI" ] || { echo "ERROR: нет luci-app-csqtt_*_all.apk в $DIST"; exit 2; }
[ -n "$APK_I18N" ] || { echo "ERROR: нет luci-i18n-csqtt-ru*.apk в $DIST"; exit 2; }
CS_DIR=$(find "$CB" -maxdepth 1 -type d -name 'csqtt-*' 2>/dev/null | head -1)
LS_DIR=$(find "$CB" -maxdepth 1 -type d -name 'luci-app-csqtt*' 2>/dev/null | head -1)

# декомпрессия apk -> stdout (raw-deflate после 4-байтного тега ADBd)
apkz(){ "$PY" - "$1" <<'PY'
import sys,zlib
d=open(sys.argv[1],'rb').read()
assert d[:4]==b'ADBd', "не ADBd"
sys.stdout.buffer.write(zlib.decompressobj(-15).decompress(d[4:]))
PY
}

# первые N байт распакованного apk = control-секция (name/version/arch/depends);
# важно проверять depends ЗДЕСЬ, а не по всему payload: комментарии в файлах
# пакета (напр. ucode-бекенда) могут содержать имена зависимостей и давать
# ложный PASS. [M8]
apkz_head(){ "$PY" - "$1" "$2" <<'PY'
import sys,zlib
d=open(sys.argv[1],'rb').read()
assert d[:4]==b'ADBd', "не ADBd"
raw=zlib.decompressobj(-15).decompress(d[4:])
sys.stdout.buffer.write(raw[:int(sys.argv[2])])
PY
}

echo "############ csqtt.apk ############"
CS="$CS_DIR/.pkgdir/csqtt"
# [CI] Дерево .pkgdir существует только после полной локальной сборки SDK.
# В CI его нет — это SKIP, а не FAIL: проверки самого apk (ниже, разбор блоба из
# dist/) остаются строгими. При наличии SDK пропускать нельзя.
[ -d "$CB" ] || sk "нет build-дерева SDK ($CB) — проверки csqtt .pkgdir пропущены"
[ -d "$CB" ] && [ ! -d "$CS" ] && bad "нет IDIR csqtt ($CS) — пересоберите"
if [ -d "$CS" ]; then
  for f in usr/bin/csqtt etc/init.d/csqtt etc/config/csqtt etc/logrotate.d/csqtt; do
    [ -f "$CS/$f" ] && ok "csqtt pkgdir: $f" || bad "csqtt pkgdir: нет $f"
  done
  m(){ stat -c '%a' "$CS/$1" 2>/dev/null; }
  [ "$(m usr/bin/csqtt)" = 755 ] && ok "csqtt: usr/bin/csqtt 0755" || bad "csqtt: bin perms=$(m usr/bin/csqtt)"
  [ "$(m etc/init.d/csqtt)" = 755 ] && ok "csqtt: init.d 0755" || bad "csqtt: init.d perms=$(m etc/init.d/csqtt)"
  [ "$(m etc/config/csqtt)" = 644 ] && ok "csqtt: config 0644" || bad "csqtt: config perms=$(m etc/config/csqtt)"
  [ "$(m etc/logrotate.d/csqtt)" = 644 ] && ok "csqtt: logrotate 0644" || bad "csqtt: logrotate perms=$(m etc/logrotate.d/csqtt)"
  head -1 "$CS/etc/init.d/csqtt" | grep -q '/etc/rc.common' && ok "csqtt: init.d shebang rc.common" || bad "csqtt: init.d shebang"
  file "$CS/usr/bin/csqtt" | grep -qi 'ELF' && file "$CS/usr/bin/csqtt" | grep -qi 'aarch64' && ok "csqtt: бинарь ELF aarch64" || bad "csqtt: бинарь не ELF aarch64"
  # изоляция в дереве
  if find "$CS" | grep -qiE 'firewall|uci-defaults|dnsmasq'; then bad "csqtt pkgdir: запрещённые файлы"; else ok "csqtt pkgdir: нет firewall/uci-defaults/dnsmasq"; fi
fi
# (B) встроенные пути/depends в самом apk
BLOB=$(apkz "$APK_CORE" | LC_ALL=C tr -c '[:print:]' '\n')
CCTRL=$(apkz_head "$APK_CORE" 4096 | LC_ALL=C tr -c '[:print:]' '\n')
echo "$BLOB" | grep -q 'usr/bin/csqtt' && ok "csqtt apk: содержит usr/bin/csqtt" || bad "csqtt apk: нет usr/bin/csqtt"
echo "$BLOB" | grep -q 'etc/init.d/csqtt' && ok "csqtt apk: содержит etc/init.d/csqtt" || bad "csqtt apk: нет etc/init.d/csqtt"
echo "$CCTRL" | grep -q 'kmod-tun' && ok "csqtt apk: depends kmod-tun" || bad "csqtt apk: нет kmod-tun"
echo "$CCTRL" | grep -qi 'ip-full' && bad "csqtt apk: есть ip-full (запрещено)" || ok "csqtt apk: нет ip-full"
# firewall/dnsmasq/uci-defaults как ФАЙЛЫ проверяются по дереву .pkgdir выше;
# по blob не ищем — эти слова встречаются внутри Rust-бинарника (doctor).
echo "$BLOB" | grep -q 'etc/config/csqtt' && ok "csqtt apk: config (conffile) присутствует" || bad "csqtt apk: нет config"

echo "############ luci-app-csqtt.apk ############"
LS="$LS_DIR/.pkgdir/luci-app-csqtt"
if [ -d "$LS" ]; then
  for f in \
    usr/share/luci/menu.d/luci-app-csqtt.json \
    usr/share/rpcd/acl.d/luci-app-csqtt.json \
    usr/share/rpcd/ucode/csqtt \
    www/luci-static/resources/view/csqtt/status.js \
    www/luci-static/resources/view/csqtt/profiles.js \
    www/luci-static/resources/view/csqtt/captcha.js \
    www/luci-static/resources/view/csqtt/settings.js \
    www/luci-static/resources/view/csqtt/logs.js ; do
    [ -f "$LS/$f" ] && ok "luci pkgdir: $f" || bad "luci pkgdir: нет $f"
  done
  # [M8] регресс дефекта M7: world-writable ucode rpcd игнорирует
  # ("Ignoring ucode script ... because it is world writable"), а вью .js
  # попадали в пакет с 0777 из drvfs-исходников.
  ml(){ stat -c '%a' "$LS/$1" 2>/dev/null; }
  [ "$(ml usr/share/rpcd/ucode/csqtt)" = 644 ] && ok "luci: ucode/csqtt 0644" || bad "luci: ucode perms=$(ml usr/share/rpcd/ucode/csqtt)"
  [ "$(ml www/luci-static/resources/view/csqtt/status.js)" = 644 ] && ok "luci: view status.js 0644" || bad "luci: view perms=$(ml www/luci-static/resources/view/csqtt/status.js)"
  [ "$(ml www/luci-static/resources/view/csqtt/captcha.js)" = 644 ] && ok "luci: view captcha.js 0644" || bad "luci: view perms=$(ml www/luci-static/resources/view/csqtt/captcha.js)"
  [ "$(ml usr/share/rpcd/acl.d/luci-app-csqtt.json)" = 644 ] && ok "luci: acl 0644" || bad "luci: acl perms=$(ml usr/share/rpcd/acl.d/luci-app-csqtt.json)"
  find "$LS" -type f -perm -0002 | grep -q . && bad "luci pkgdir: есть world-writable файлы" || ok "luci pkgdir: нет world-writable файлов"
  find "$LS" | grep -q 'etc/uci-defaults' && bad "luci-app pkgdir: есть uci-defaults" || ok "luci-app pkgdir: нет uci-defaults"
  find "$LS" | grep -qiE 'firewall|dnsmasq' && bad "luci-app pkgdir: firewall/dnsmasq" || ok "luci-app pkgdir: нет firewall/dnsmasq"
elif [ -d "$CB" ]; then bad "нет IDIR luci-app-csqtt"; else sk "нет build-дерева SDK — проверки luci .pkgdir пропущены"; fi
LBLOB=$(apkz "$APK_LUCI" | LC_ALL=C tr -c '[:print:]' '\n')
LCTRL=$(apkz_head "$APK_LUCI" 4096 | LC_ALL=C tr -c '[:print:]' '\n')
echo "$LBLOB" | grep -q 'view/csqtt/captcha.js' && ok "luci apk: captcha.js" || bad "luci apk: нет captcha.js"
echo "$LBLOB" | grep -q 'rpcd/ucode/csqtt' && ok "luci apk: rpcd ucode" || bad "luci apk: нет rpcd ucode"
echo "$LCTRL" | grep -q 'ucode-mod-socket' && ok "luci apk: depends ucode-mod-socket" || bad "luci apk: нет ucode-mod-socket"
echo "$LCTRL" | grep -q 'coreutils-timeout' && ok "luci apk: depends coreutils-timeout" || bad "luci apk: нет coreutils-timeout"

echo "############ luci-i18n-csqtt-ru.apk ############"
IS="$LS_DIR/.pkgdir/luci-i18n-csqtt-ru"
if [ -d "$IS" ]; then
  find "$IS" -name 'csqtt.ru.lmo' | grep -q . && ok "i18n pkgdir: csqtt.ru.lmo" || bad "i18n pkgdir: нет .lmo"
elif [ -d "$CB" ]; then bad "нет IDIR i18n"; else sk "нет build-дерева SDK — проверка i18n .pkgdir пропущена"; fi
IBLOB=$(apkz "$APK_I18N" | LC_ALL=C tr -c '[:print:]' '\n')
echo "$IBLOB" | grep -q 'csqtt.ru.lmo' && ok "i18n apk: .lmo встроен" || bad "i18n apk: нет .lmo"

echo
echo "M7 verify: PASS=$PASS FAIL=$FAIL SKIP=$SKIP"
if [ "$FAIL" -ne 0 ]; then
  echo "RESULT: FAIL"
  exit 1
fi
echo "RESULT: PASS"
