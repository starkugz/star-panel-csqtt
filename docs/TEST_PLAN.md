# TEST_PLAN — CSQTT для OpenWrt (WH3000 Pro / MT7981)

Актуально для M8 (2026-09-17). План описывает все уровни проверок: офлайн
(юнит/интеграция/пакеты) и живой e2e на железе. Общий принцип — **честный
PASS/FAIL/SKIP**: каждая проверка либо подтверждена, либо помечена SKIP с
причиной (например, нет второго сервера/VK-challenge).

## 0. Окружение

| Компонент | Значение |
|---|---|
| Роутер | Huasifei WH3000 Pro, MediaTek Filogic MT7981, aarch64_cortex-a53 |
| ОС | OpenWrt 25.12.5, kernel 6.12.94, `apk` 3.0.5, LuCI (JS + menu.d + rpcd-ucode) |
| Пакеты | `csqtt_2.1.9_aarch64_cortex-a53.apk`, `luci-app-csqtt_2.1.9_all.apk`, `luci-i18n-csqtt-ru_all.apk` |
| Runtime-зависимости | `kmod-tun`, `luci-base`, `rpcd-mod-ucode`, `ucode-mod-socket`, `coreutils-timeout` |
| SSH | `SSHHOST` (192.168.1.1), `SSHPORT` (22), `SSHUSER` (root), `SSHPASS`/key |

## 1. Офлайн-тесты (без роутера)

| # | Команда | Ожидание |
|---|---|---|
| 1.1 | `cd csqtt-openwrt && bash scripts/run-tests.sh` | fmt OK, clippy `-D warnings` 0, `cargo test --lib` **601 passed / 0 failed / 7 ignored** |
| 1.2 | `sh openwrt/tests/openwrt/run.sh` | M5 init.d/uci: **40 / 0 / 0** (моки, shellcheck 0) |
| 1.3 | `sh openwrt/tests/luci/run.sh` | M6a–M6d rpcd-ucode/ACL/menu.d/i18n/QR + runtime вью: **145 / 0 / 1** (SKIP = msgfmt отсутствует) |
| 1.4 | `sh openwrt/tests/packages/verify-apk.sh` | apk-пакеты: **PASS 37 / FAIL 0** (дерево/права/ELF/depends/изоляция) |
| 1.5 | `sh scripts/build-all.sh` | musl-бинарь + оба `.apk` + `SHA256SUMS` в `dist/` |
| 1.6 | `sh scripts/release.sh vX.Y.Z` | бандл `dist/RELEASE-vX.Y.Z/` + git tag (если git-репо) |

Обязательное условие зелёного результата: секреты не утекают (profiles/логи),
изоляция M3X не нарушена (нет route/rule/table/DNS/firewall от службы CSQTT).

## 2. Живой smoke-test на железе

```sh
SSHHOST=192.168.1.1 SSHPASS='…' \
MIHOMO_BIN=/opt/clash/bin/clash MIHOMO_DIR=/opt/clash \
	sh scripts/smoke-test.sh
```

Скрипт печатает шаги `PASS/FAIL/SKIP` и завершается `RESULT: PASS` при `FAIL=0`.
Эталон M8 на WH3000 Pro: **PASS=29 FAIL=0 SKIP=1** (SKIP — шаги 13–16, см. §5).

Шаги: 1 пакеты · 2 версия · 3 UCI `enabled=1` + `service start` + procd ·
4 `doctor --json` · 5 `ip link csqtt0` · 6 `status --json` · 7 логи+redaction ·
8 LuCI HTTP · 9 respawn procd ≤10с · 10 изоляция (before/after) · 11 `rp_filter` ·
12 Mihomo TCP/UDP · 13–16 failover/failback/fatal/CAPTCHA · 17 workers/CPU/RAM/temp ·
18 stop/teardown · 19 upgrade persistence.

## 3. Контракт изоляции (шаг 10, M3X)

Перед стартом и после поднятия туннеля снимаются и сравниваются:
`ip route show`, `ip rule show`, `ip -6 route show`, `uci show dhcp`,
`nft list ruleset`, `/etc/resolv.conf`. Требования:

- WAN/modem default и metrics не заменены (`default via 192.168.1.1 dev eth1 …`);
- `dnsmasq`/system DNS/`resolv.conf` не изменены;
- в `nft` нет правил CSQTT; `ip rule` без записей CSQTT; IPv6-маршруты не тронуты;
- служба CSQTT не ставит route/rule/table (M3X: interface-only).

**Уточнение M8 (честно):** при назначении адреса `csqtt0` ядро автоматически
создаёт connected-route своей подсети — `10.66.67.0/24 dev csqtt0 proto kernel`.
Это не маршрут, установленный службой CSQTT (`plan_system_commands` в interface-only
возвращает 0 команд), а штатный артефакт `set_address(/24)`; WAN/default/DNS/
firewall/rule он не затрагивает. На `stop` интерфейс и эта запись исчезают.
Если требуется строго нулевая запись, ядру порта нужно выставлять
`IFA_F_NOPREFIXROUTE` (не делается, чтобы не менять M3X-поведение).

## 4. Mihomo gate (шаг 12)

Пакет **не настраивает** пользовательский прокси. Проверка выполняется
отдельным экземпляром mihomo/clash:

```yaml
mixed-port: 17890
proxies:
  - name: CSQTT
    type: direct
    interface-name: csqtt0
    udp: true
rules:
  - MATCH,CSQTT
```

- TCP: `curl -x http://127.0.0.1:17890 http://api.ipify.org` → внешний IP
  туннеля (эталон M8: `198.51.100.10`), отличный от WAN;
- `csqtt status --json` → `active_profile` не null, `state=active`,
  `rx_bytes`/`tx_bytes` растут при user-traffic;
- UDP: `udp: true` в прокси + UDP-нагрузка через TUN (полный UDP-ассерт —
  ручной: в прошивке нет `dig`/UDP-инструмента для автоматизации);
- control-plane CSQTT/VK/TURN остаётся через WAN (служба CSQTT не привязана к csqtt0).

## 5. Failover / failback / fatal-auth / CAPTCHA (шаги 13–16)

Автоматизации подлежат только при наличии **второго живого профиля/сервера** и
управляемого VK-challenge; иначе — честный SKIP (эталон M8: 1 профиль).
Ручные сценарии:

1. **13 failover**: профиль A недоступен → B становится ACTIVE в
   `fail_threshold × health_interval`; `csqtt0`/mihomo-конфиг не меняются.
2. **14 failback=off**: A восстановился → текущий B не рвётся.
3. **15 fatal auth/device mismatch**: A даёт FATAL_AUTH → переход на B;
   `device-id` не меняется сам.
4. **16 CAPTCHA**: A → `CAPTCHA_REQUIRED`, служба CSQTT жива, поднимает B; iPhone
   Safari проходит VK-капчу через Web Helper, результат возвращается
   `CaptchaSolver`; `failback=off` не рвёт B.

## 6. LuCI — ручной визуальный чек-лист

- `/cgi-bin/luci/admin/services/csqtt/status` — карточки, CAPTCHA-badge;
- `…/profiles` — таблица/редактор/импорт(preview→commit)/экспорт;
- `…/captcha` — пусто/таблица, «Решить CAPTCHA» (QR → iPhone Safari),
  «Скопировать ссылку», «Отменить»; токены не видны;
- `…/settings` — форма main + doctor-отчёт + read-only routing (M3X);
- `…/logs` — file/syslog, grep, refresh, download, clear.

Скрипт (шаг 8) проверяет HTTP: JS-вью `200`, страница — не `404` (без авторизации
`403`). Полный визуальный прогон — под root в браузере.

## 7. Известные SKIP и предпосылки

| Шаг | Причина SKIP |
|---|---|
| 13–16 | нет второго профиля/сервера и управляемого VK-challenge |
| 12 UDP | нет прибора для детерминированного UDP-ассерта в прошивке |
| 1.3 i18n | `msgfmt` не установлен (SKIP синтаксиса po) |
| CAPTCHA live | одноразовые VK-хеши; challenge не воспроизводится детерминированно |

## 8. Дефекты, найденные/исправленные в M8

| # | Дефект | Исправление |
|---|---|---|
| 1 | M7: JS-вью ставились в `/htdocs`, LuCI не видел (нужно `/www`) | перенос `root/htdocs` → top-level `htdocs/` + verify-apk 37 |
| 2 | M6a/M7: rpcd-ucode 0777 (drvfs) → rpcd игнорирует «world writable» | `Build/Prepare/<LUCI_NAME>` chmod 0644 перед include luci.mk |
| 3 | M6a/M7: нет зависимости `ucode-mod-socket` (модуль `socket`) | `LUCI_DEPENDS += ucode-mod-socket` |
| 4 | M6a: `/usr/bin/timeout` отсутствует на ванильном OpenWrt | `LUCI_DEPENDS += coreutils-timeout` |
| 5 | M6a: `fs.popen([argv])` (array-exec) нет в ucode 2023.07.11 | строковый `popen` + `shellquote()` (как в LuCI) |
| 6 | M6a: `uci show -j/--json` не поддерживается | парсинг текстового `uci show csqtt` в ucode |
| 7 | M4b: `saw_transport_positive` только разовый READY → health_mode=both всегда transient-fail при живом туннеле | учитывать `Stats.active > 0` как transport-сигнал (+регресс-тест) |
| 8 | M6b/c/d: вью полагались на `poll()`-хук (LuCI его не вызывает) и несуществующий `L.poll` → вечный «loading…», пустой журнал; profiles.js к тому же вызывал `renderTable()` до attach DOM → «нету профиля» | первый запрос + `L.Poll.add` в `render`, post-attach `update`, `unload()` снимает poller; регресс §4e + runtime `view-runtime.js` |
| 9 | M6a: `run()` читал только stdout, ошибки CLI (идут в stderr) терялись → LuCI показывал «ERROR (code N)» без причины | `fs.popen(cmd + " 2>&1")` |
| 10 | M6a/M6c: `isId` (backend) и `ID_RE` (profiles.js) пускали дефис в id профиля, а uci.rs требует `[A-Za-z0-9_]`; id был редактируем → мог создать дубль секции | `PROFILE_ID_RE`/`isProfileId` в бекенде; `ID_RE` без дефиса в UI; id readonly при редактировании |
| 11 | Вью использовали bare-глобал `dom` (в LuCI 24.10 отсутствует, есть `L.dom`) → `ReferenceError` в `setNode`/`showError` → Status и Logs навсегда «loading…»/«…» (найдено в живом браузере, headless Chromium+CDP) | `dom.content` → `L.dom.content` (12 мест, 5 вью); harness больше не инжектит `dom`; статический гвард §4e |

## 9. Критерии приёмки M8 (DoD)

- все обязательные шаги smoke-test PASS (эталон: 29 PASS / 0 FAIL);
- M3X routing подтверждён на железе: interface-only, `ip rule` без CSQTT,
  `nft` без CSQTT, `rp_filter≠1` (на WH3000 — 0), WAN/DNS/firewall не тронуты;
- Mihomo TCP через `csqtt0` уводит трафик в туннель, RX/TX растут;
- локальные регрессы (Rust/LuCI/OpenWrt/пакеты) зелёные;
- релизные артефакты собираются `scripts/release.sh`.
