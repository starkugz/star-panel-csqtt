# TESTING — слои тестов и как их запускать

Тесты многослойные: от чистых unit до живого e2e на роутере. Для релиза нужен
зелёный прогон всех доступных слоёв. См. также [`TEST_PLAN.md`](TEST_PLAN.md)
(детальный план M-модулей) и [`UPDATE.md`](UPDATE.md).

## Слой 0 — Rust: fmt + clippy + unit (быстрый, off-line)

```sh
cd csqtt-openwrt && bash scripts/run-tests.sh
```

Что делает:
- `cargo fmt --check`
- `cargo clippy --all-targets -- -D warnings`
- `cargo test --lib` (600+ тестов: state-machine пула, health/thresholds,
  CAPTCHA-менеджер, UCI-валидация, TURN/RTP-модели, redaction, workers и т.д.)
- M5 openwrt-тесты, если собран host-бинарник (иначе честный SKIP).

Требования: Rust + musl target (см. `docs/UPDATE.md` §0). Сеть не нужна.

## Слой 1 — M5 openwrt (shell + config)

```sh
sh openwrt/tests/openwrt/run.sh
```

Проверяет `sh -n`, `shellcheck`, содержимое `init.d` (procd command/respawn/
stdout/stderr/limits), ворота `doctor`, runtime-каталог, reload по SIGHUP,
`logrotate`, схему UCI, **изоляцию** (нет запрещённых команд/сетевых
procd-параметров). Ожидаемо: `PASS=40 FAIL=0`.

## Слой 2 — M6b LuCI (JS + QR + runtime)

```sh
sh openwrt/tests/luci/run.sh
```

Статический разбор вью (`_()` i18n, запрет кириллицы в JS, `L.dom`,
`uci.save().then(()=>uci.apply())`), QR round-trip, runtime-симуляция вью.
Опционально — live headless-браузер (Chrome for Testing + CDP) через WSL.

## Слой 3 — M7 packages (.apk)

```sh
sh openwrt/tests/packages/verify-apk.sh
```

Проверяет структуру собранных `.apk` (наличие нужных файлов, conffiles,
отсутствие лишнего), согласованность с `openwrt/etc`.

## Слой 4 — M8 smoke (живой роутер, e2e)

```sh
SSHHOST=192.168.1.1 SSHPASS=... CSQTT_SMOKE_WAIT=120 sh scripts/smoke-test.sh
```

Запускается **с хоста**, роутер не требует git. Шаги (PASS/FAIL/SKIP):
packages, version, UCI enable+start+procd, `doctor`, `csqtt0`, `status --json`,
logs+redaction, LuCI HTTP, respawn, **изоляция до/после**, `rp_filter`,
mihomo TCP/UDP (если `MIHOMO_BIN`), failover, failback=off, fatal auth,
CAPTCHA, workers/CPU/RAM/temp, stop/teardown, upgrade persistence.

Переменные: `SSHHOST`, `SSHPORT`, `SSHUSER`, `SSHPASS` (через `sshpass -e`) или
`SSHKEY`, `CSQTT_SMOKE_WAIT`, `CSQTT_SMOKE_RESTART`, `MIHOMO_BIN`, `MIHOMO_DIR`,
`KEEP_RUNNING`. Шаги 13–16 требуют нескольких профилей/живого VK-challenge —
без них честный **SKIP**, не FAIL.

## Что считается регрессом

- Любой FAIL в слоях 0–3.
- FAIL в smoke (SKIP допустим и обоснован окружением).
- Изменение сетевого поведения (появились route/rule/DNS-мутации) — нарушение
  M3X, даже если тесты «зелёные».

## Найденный M8-дефект (регресс-охранение)

Ранее клиент **не проходил** VK-auth на реальном роутере: пул набирал порог
отказов (первый тик `tokio::time::interval` срабатывает сразу + `fail_threshold=3`
при `health_interval=5`) уже к ~10с, тогда как медленный старт (таймаут
Яндекс.DNS ~5с + VK-auth) доходил до TURN только к ~11с. Покрыто:

- unit: `startup_grace_defers_health_fails_until_first_connection`,
  `startup_grace_still_counts_success_when_transport_ready` (в `pool/tests.rs`);
- константа `STARTUP_GRACE = 30s` в `pool.rs`.

Не удаляйте grace и эти тесты без замены на эквивалентную защиту.
