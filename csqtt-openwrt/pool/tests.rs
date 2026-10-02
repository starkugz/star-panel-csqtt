// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Тесты M4b: детерминированная state-machine пула + daemon loop.
//!
//! Все политики гоняются через чистый `PoolManager` (время — `Instant`),
//! без сети и таймеров. Отдельно — fake `ClientRunner` для daemon-loop.

use super::*;
use crate::uci::RoutingMode;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// Глобальный CONTROL_TX ядра — единственный на процесс. Тесты, которые его
/// устанавливают/проверяют доставку `CAPTCHA_RESULT|…`, сериализуются этим
/// замком (tokio::sync::Mutex — удерживается через .await), чтобы не мешать
/// `captcha_wait_policy_keeps_client_and_tracks_result` (он ожидает «нет
/// канала» → failed).
static CAPTCHA_CONTROL_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// Тестовый UCI: 3 профиля, priority 10/20/30, global thresholds.
/// health_interval=30 — чтобы health-ticker не мешал управляемым тестам.
const UCI_3: &str = "\
config csqtt 'main'
	option enabled '1'
	option selection_mode 'priority'
	option active_profile ''
	option failover '1'
	option failback '0'
	option health_interval '30'
	option fail_threshold '3'
	option success_threshold '2'
	option cooldown '60'
	option reconnect_delay '5'
	option failback_stable_time '30'
	option health_mode 'both'
	option captcha_policy 'failover'

config csqtt 'routing'
	option mode 'auto'

config server 'p10'
	option name 'Primary'
	option enabled '1'
	option priority '10'
	option peer '10.0.0.10:46000'
	option password 'secret-10'
	option vk 'hash10'
	option workers '18'

config server 'p20'
	option name 'Backup'
	option enabled '1'
	option priority '20'
	option peer '10.0.0.20:46000'
	option password 'secret-20'
	option vk 'hash20'
	option workers '9'

config server 'p30'
	option name 'Tertiary'
	option enabled '1'
	option priority '30'
	option peer '10.0.0.30:46000'
	option password 'secret-30'
	option vk 'hash30'
	option workers '9'
";

fn pool() -> ProfilePool {
    ProfilePool::from_text(UCI_3).expect("valid UCI fixture")
}

fn t0() -> Instant {
    Instant::now()
}

fn advance(base: Instant, secs: u64) -> Instant {
    base + Duration::from_secs(secs)
}

// --- Требование 2: Selection ---

/// Мутабельный доступ к профилю в пуле (тестовый helper: uci.rs
/// предоставляет только immutable `server`).
fn server_mut<'a>(pool: &'a mut ProfilePool, id: &str) -> &'a mut ServerProfile {
    pool.servers
        .iter_mut()
        .find(|profile| profile.section_id == id)
        .expect(id)
}

#[test]
fn priority_selects_lowest_priority_first() {
    let manager = PoolManager::new(pool(), t0());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p10"),
        other => panic!("ожидался Start p10, получено {other:?}"),
    }
}

#[test]
fn manual_pins_active_profile_only() {
    let mut pool = pool();
    pool.main.selection_mode = SelectionMode::Manual;
    pool.main.active_profile = "p30".to_string();
    let manager = PoolManager::new(pool, t0());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p30"),
        other => panic!("ожидался Start p30, получено {other:?}"),
    }
}

#[test]
fn manual_with_empty_active_profile_stops() {
    let mut pool = pool();
    pool.main.selection_mode = SelectionMode::Manual;
    let manager = PoolManager::new(pool, t0());
    assert_eq!(manager.next_action(t0()), Action::Stop);
}

#[test]
fn disabled_profile_is_never_selected() {
    let mut pool = pool();
    server_mut(&mut pool, "p10").enabled = false;
    let manager = PoolManager::new(pool, t0());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn selected_profile_matches_uci_logic() {
    let pool = pool();
    assert_eq!(
        pool.selected_profile().map(|p| p.section_id.as_str()),
        Some("p10")
    );
}

// --- Требование 3: Failover ---

/// transient 1/3 → NO SWITCH: профиль остаётся активным.
#[test]
fn transient_under_fail_threshold_does_not_switch() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(&HealthInput::NetworkSuspect, t0());
    assert_eq!(manager.active_id(), Some("p10"));
    assert_eq!(manager.next_action(t0()), Action::Keep);
}

/// threshold reached → следующий профиль.
#[test]
fn fail_threshold_moves_to_next_profile() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    for _ in 0..3 {
        manager.on_health(&HealthInput::NetworkSuspect, t0());
    }
    assert_ne!(manager.active_id(), Some("p10"));
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

/// FATAL_AUTH не крутим циклом: профиль уходит в AuthFailed навсегда.
#[test]
fn fatal_auth_marks_profile_and_moves_on() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("FATAL_AUTH: неверный пароль подключения"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::AuthFailed);
    assert!(manager.runtimes["p10"].permanent);
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
    // Даже после истечения cooldown p10 не eligible.
    assert!(!manager.eligible("p10", advance(t0(), 600)));
}

/// Конфигурационная ошибка профиля — permanent, без цикла.
#[test]
fn missing_peer_is_permanent_not_transient() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("[КЛИЕНТ] Нужны -peer и хеши VK"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::AuthFailed);
    assert!(manager.runtimes["p10"].permanent);
}

/// Отменённый клиент — clean stop, состояние не сбрасывается.
#[test]
fn cancelled_client_result_is_clean_stop() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    for _ in 0..3 {
        manager.on_health(&HealthInput::NetworkSuspect, t0());
    }
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Failed);
    let result: Result<()> = Err(anyhow::anyhow!("отменено"));
    manager.on_client_result("p10", &result, true, t0());
    // Failed/cooldown переживает отмену (cancel — следствие решения).
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Failed);
    assert!(manager.runtimes["p10"].cooldown_until.is_some());
}

/// Transient-ошибка клиента под threshold — retry того же профиля (NO SWITCH).
#[test]
fn transient_client_error_retries_same_profile() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("TURN transaction timeout"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Standby);
    assert!(manager.runtimes["p10"].retry_pending(t0()));
    // Профиль остался активной попыткой — переключения на backup нет.
    assert_eq!(manager.active_id(), Some("p10"));
    assert_eq!(manager.next_action(t0()), Action::Keep);
    // После reconnect_delay — старт того же профиля.
    assert!(matches!(
        manager.next_action(advance(t0(), 5)),
        Action::Start { id, .. } if id == "p10"
    ));
}

// --- Требование 3: success_threshold ---

#[test]
fn success_threshold_promotes_to_active() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    // health_mode=both: нужны transport и data сигналы.
    manager.on_health(
        &HealthInput::TunnelConfig {
            ip: "10.66.66.2".into(),
            dns: "1.1.1.1".into(),
        },
        t0(),
    );
    manager.on_health(
        &HealthInput::Stats {
            active: 9,
            bytes_up: 1,
            bytes_down: 2,
        },
        t0(),
    );
    manager.on_tick(t0()); // success #1 → Ready
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Ready);
    manager.on_health(&HealthInput::Ready { worker: 1 }, t0());
    manager.on_health(
        &HealthInput::Stats {
            active: 9,
            bytes_up: 3,
            bytes_down: 4,
        },
        t0(),
    );
    manager.on_tick(t0()); // success #2 → Active
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Active);
}

#[test]
fn sustained_active_connections_keep_health_without_repeated_ready() {
    // [M8] READY/TUNCONF приходят разово; последующие тики должны оставаться
    // здоровыми, пока в интервале есть живые соединения (Stats.active > 0).
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(&HealthInput::Ready { worker: 1 }, t0());
    for _ in 0..4 {
        manager.on_health(
            &HealthInput::Stats {
                active: 9,
                bytes_up: 10,
                bytes_down: 20,
            },
            t0(),
        );
        manager.on_tick(t0());
    }
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Active);
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 0);
}

#[test]
fn startup_grace_defers_health_fails_until_first_connection() {
    // [M8-fix] Пока профиль в Connecting и grace не истёк, тики без
    // положительного транспорта фейлы не копят — иначе первый immediate-тик
    // интервала + fail_threshold=3 отменяли попытку до готовности.
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_tick(t0());
    manager.on_tick(advance(t0(), 5));
    manager.on_tick(advance(t0(), 10));
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 0);
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Connecting);
    // grace истёк — обычная семантика: 3 фейла подряд → Failed.
    manager.on_tick(advance(t0(), 31));
    manager.on_tick(advance(t0(), 36));
    manager.on_tick(advance(t0(), 41));
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 3);
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Failed);
}

#[test]
fn startup_grace_still_counts_success_when_transport_ready() {
    // grace не блокирует успех: пришедшие READY/TUNCONF+Stats сразу считаются.
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(
        &HealthInput::TunnelConfig {
            ip: "10.66.66.2".into(),
            dns: "1.1.1.1".into(),
        },
        advance(t0(), 5),
    );
    manager.on_health(
        &HealthInput::Stats {
            active: 9,
            bytes_up: 1,
            bytes_down: 2,
        },
        advance(t0(), 5),
    );
    manager.on_tick(advance(t0(), 6));
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 0);
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Ready);
}

#[test]
fn success_counter_resets_on_failure() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(
        &HealthInput::TunnelConfig {
            ip: "10.66.66.2".into(),
            dns: "1.1.1.1".into(),
        },
        t0(),
    );
    manager.on_health(
        &HealthInput::Stats {
            active: 9,
            bytes_up: 1,
            bytes_down: 2,
        },
        t0(),
    );
    manager.on_tick(t0());
    assert_eq!(manager.runtimes["p10"].consecutive_successes, 1);
    manager.on_health(&HealthInput::NetworkSuspect, t0());
    assert_eq!(manager.runtimes["p10"].consecutive_successes, 0);
}

// --- Требование 3: cooldown ---

#[test]
fn cooldown_blocks_reselection_until_expired() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    for _ in 0..3 {
        manager.on_health(&HealthInput::NetworkSuspect, t0());
    }
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Failed);
    // До истечения cooldown (60с) p10 не eligible.
    assert!(!manager.eligible("p10", advance(t0(), 59)));
    assert!(manager.eligible("p10", advance(t0(), 60)));
}

#[test]
fn cooldown_remaining_is_reported_in_snapshot() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    for _ in 0..3 {
        manager.on_health(&HealthInput::NetworkSuspect, t0());
    }
    let snapshot = manager.snapshot(advance(t0(), 10), DaemonState::Running, Vec::new());
    let p10 = snapshot
        .profiles
        .iter()
        .find(|profile| profile.id == "p10")
        .unwrap();
    assert!(p10.cooldown_remaining_secs > 0);
    assert_eq!(p10.state, "failed");
}

// --- Требование 3: reconnect_delay ---

#[test]
fn reconnect_delay_gaps_attempts() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("TURN timeout"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Standby);
    assert!(manager.runtimes["p10"].reconnect_after.is_some());
    // reconnect_delay=5с: раньше не eligible.
    assert!(!manager.eligible("p10", t0()));
    assert!(!manager.eligible("p10", advance(t0(), 4)));
    assert!(manager.eligible("p10", advance(t0(), 5)));
}

// --- Требование 4: Failback ---

#[test]
fn failback_off_stays_on_backup() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p20", t0()); // активный — backup
    // p10 доступен (standby), но failback=0 — остаёмся на p20,
    // даже когда он здоров и стабилен.
    make_healthy(&mut manager, "p20", t0());
    let later = advance(t0(), 3600);
    positive_signals(&mut manager, later);
    manager.on_tick(later);
    assert_eq!(manager.next_action(later), Action::Keep);
    assert_eq!(manager.active_id(), Some("p20"));
}

/// Подать положительные сигналы здоровья (перед каждым тиком).
fn positive_signals(manager: &mut PoolManager, now: Instant) {
    manager.on_health(&HealthInput::Ready { worker: 1 }, now);
    manager.on_health(
        &HealthInput::Stats {
            active: 9,
            bytes_up: 3,
            bytes_down: 4,
        },
        now,
    );
}

/// Здоровый профиль: transport + data перед каждым тиком + success_threshold.
fn make_healthy(manager: &mut PoolManager, id: &str, now: Instant) {
    manager.on_health(
        &HealthInput::TunnelConfig {
            ip: "10.66.66.2".into(),
            dns: "1.1.1.1".into(),
        },
        now,
    );
    positive_signals(manager, now);
    manager.on_tick(now);
    positive_signals(manager, now);
    manager.on_tick(now);
    assert_eq!(manager.runtimes[id].state, ProfileState::Active);
}

#[test]
fn failback_on_returns_to_better_priority_after_stable_window() {
    let mut pool = pool();
    pool.main.failback = true;
    pool.main.failback_stable_time = 30;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p20", t0());
    make_healthy(&mut manager, "p20", t0());
    // До stable window — остаёмся на p20 (no flapping).
    let early = advance(t0(), 29);
    positive_signals(&mut manager, early);
    manager.on_tick(early);
    assert_eq!(manager.next_action(early), Action::Keep);
    // После stable window — возвращаемся на p10 (лучший priority, eligible).
    let later = advance(t0(), 30);
    positive_signals(&mut manager, later);
    manager.on_tick(later);
    match manager.next_action(later) {
        Action::Start { id, .. } => assert_eq!(id, "p10"),
        other => panic!("ожидался Start p10, получено {other:?}"),
    }
}

#[test]
fn failback_requires_healthy_active() {
    let mut pool = pool();
    pool.main.failback = true;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p20", t0());
    // p20 не healthy — failback не сработает даже после окна.
    let later = advance(t0(), 3600);
    positive_signals(&mut manager, later);
    manager.on_tick(later);
    assert_eq!(manager.next_action(later), Action::Keep);
}

#[test]
fn failback_oscillates_between_profiles() {
    // Защита от осцилляции: после возврата на p10 окно стабильности
    // отсчитывается заново (healthy_since сброшен), p20 не отбирает
    // управление обратно до нового stable window.
    let mut pool = pool();
    pool.main.failback = true;
    pool.main.failback_stable_time = 30;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p20", t0());
    make_healthy(&mut manager, "p20", t0());
    let later = advance(t0(), 30);
    positive_signals(&mut manager, later);
    manager.on_tick(later);
    match manager.next_action(later) {
        Action::Start { id, .. } => assert_eq!(id, "p10"),
        other => panic!("ожидался Start p10, получено {other:?}"),
    }
    // Переключились на p10 — он ещё не здоров, failback-target нет.
    manager.on_client_started("p10", advance(t0(), 31));
    manager.on_tick(advance(t0(), 32));
    assert_eq!(manager.next_action(advance(t0(), 32)), Action::Keep);
}

// --- Требование 5: profile overrides ---

/// Регресс no-flapping: метрики здоровья должны сбрасываться на каждом старте
/// попытки, иначе «вторая жизнь» профиля зачислит success_threshold из прошлой
/// жизни и failback-target увидит стайлый healthy_since (обход stable window).
#[test]
fn restart_resets_health_metrics() {
    let mut pool = pool();
    pool.main.failback = true;
    pool.main.failback_stable_time = 30;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p20", t0());
    make_healthy(&mut manager, "p20", t0());
    // Отмена (failover на другой профиль) — состояние не меняется,
    // но метрики должны быть сброшены следующим on_client_started.
    let p20 = manager.runtimes["p20"].clone();
    assert!(p20.healthy_since.is_some());
    assert_eq!(p20.consecutive_successes, 2);
    manager.on_client_started("p20", t0());
    let p20 = &manager.runtimes["p20"];
    assert_eq!(p20.state, ProfileState::Connecting);
    assert_eq!(p20.healthy_since, None);
    assert_eq!(p20.consecutive_successes, 0);
    assert!(!p20.saw_transport_positive);
    assert!(!p20.saw_data_positive);
}

#[test]
fn profile_override_replaces_global_thresholds() {
    let mut pool = pool();
    server_mut(&mut pool, "p10").fail_threshold = Some(1);
    server_mut(&mut pool, "p10").cooldown = Some(10);
    let manager = PoolManager::new(pool, t0());
    assert_eq!(manager.effective_fail_threshold_for("p10"), 1);
    assert_eq!(
        manager.effective_cooldown_for("p10"),
        Duration::from_secs(10)
    );
    assert_eq!(manager.effective_fail_threshold_for("p20"), 3);
}

#[test]
fn override_changes_failover_behaviour() {
    let mut pool = pool();
    server_mut(&mut pool, "p10").fail_threshold = Some(1);
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p10", t0());
    // Одна неудача при override fail_threshold=1 → сразу switch.
    manager.on_health(&HealthInput::NetworkSuspect, t0());
    assert_ne!(manager.active_id(), Some("p10"));
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn captcha_policy_override_is_effective() {
    let mut pool = pool();
    pool.main.captcha_policy = CaptchaPolicy::Failover;
    server_mut(&mut pool, "p10").captcha_policy = Some(CaptchaPolicy::Wait);
    let manager = PoolManager::new(pool, t0());
    assert_eq!(
        manager.effective_captcha_policy_for("p10"),
        CaptchaPolicy::Wait
    );
    assert_eq!(
        manager.effective_captcha_policy_for("p20"),
        CaptchaPolicy::Failover
    );
}

// --- Требование 6: CAPTCHA policy ---

#[test]
fn captcha_failover_policy_moves_to_next_profile() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("CAPTCHA_WAIT_REQUIRED"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::CaptchaRequired);
    assert_eq!(manager.captcha_pending(), 1);
    // failover-политика: p10 не eligible, следующий — p20.
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn captcha_wait_policy_stays_and_keeps_daemon_alive() {
    // [M4d] Lifecycle переопределён: wait больше не держит активный слот —
    // CAPTCHA_REQUIRED не блокирует переход к B (требование 8). Политика
    // wait влияет только на момент отмены ждущего клиента (служба CSQTT ждёт TTL
    // решения), а не на освобождение слота.
    let mut pool = pool();
    pool.main.captcha_policy = CaptchaPolicy::Wait;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("CAPTCHA_WAIT_REQUIRED"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::CaptchaRequired);
    assert_eq!(
        manager.active_id(),
        None,
        "captcha must free the active slot"
    );
    assert_eq!(manager.captcha_pending(), 1);
    // wait: слот свободен — стартует следующий профиль.
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn mark_captcha_required_frees_slot_immediately() {
    // [M4d] Точка входа службы CSQTT: wire-запрос при policy=failover → профиль
    // сразу в CaptchaRequired, слот свободен, следующий профиль стартует.
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.mark_captcha_required("p10", t0());
    assert_eq!(manager.active_id(), None);
    assert_eq!(manager.runtimes["p10"].state, ProfileState::CaptchaRequired);
    assert!(manager.runtimes["p10"].cooldown_until.is_some());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn captcha_required_recovers_to_standby_after_cooldown() {
    // [M4d] Профиль не застревает в CaptchaRequired навсегда: после cooldown
    // on_tick возвращает его в Standby (M4b placeholder закрыт).
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("captcha needed"));
    manager.on_client_result("p10", &result, false, t0());
    assert_eq!(
        manager.runtimes["p10"].effective_state(advance(t0(), 10)),
        ProfileState::CaptchaRequired
    );
    manager.on_tick(advance(t0(), 61));
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Standby);
}

#[test]
fn solving_pending_captcha_does_not_rip_active_profile_at_failback_zero() {
    // [M4d] «решить A» не должно рвать активный B: A снова eligible, но
    // failback=0 и B работает — выбор остаётся за B (no flapping).
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.mark_captcha_required("p10", t0());
    assert_eq!(manager.active_id(), None);
    // B стартовал и работает.
    manager.on_client_started("p20", advance(t0(), 1));
    assert_eq!(manager.active_id(), Some("p20"));
    // «Решаем A»: профиль возвращается в Standby.
    manager.resolve_captcha("p10");
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Standby);
    // B активен — A не вытесняет (failback=0 в UCI_3).
    match manager.next_action(advance(t0(), 2)) {
        Action::Keep => assert_eq!(manager.active_id(), Some("p20")),
        other => panic!("ожидался Keep для p20, получено {other:?}"),
    }
}

#[test]
fn resolve_captcha_returns_profile_to_standby() {
    let mut pool = pool();
    pool.main.captcha_policy = CaptchaPolicy::Wait;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("CAPTCHA_WAIT_REQUIRED"));
    manager.on_client_result("p10", &result, false, t0());
    manager.resolve_captcha("p10");
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Standby);
    assert_eq!(manager.captcha_pending(), 0);
}

// --- Требование 7: Health modes ---

#[test]
fn transport_only_health_ignores_data_signals() {
    let mut pool = pool();
    pool.main.health_mode = HealthMode::Transport;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p10", t0());
    // Положительный транспорт нужен перед каждым тиком (data не нужен).
    manager.on_health(&HealthInput::Ready { worker: 3 }, t0());
    manager.on_tick(t0());
    manager.on_health(&HealthInput::Ready { worker: 4 }, t0());
    manager.on_tick(t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Active);
}

#[test]
fn data_only_health_ignores_transport_signals() {
    let mut pool = pool();
    pool.main.health_mode = HealthMode::Data;
    let mut manager = PoolManager::new(pool, t0());
    manager.on_client_started("p10", t0());
    // Положительный data нужен перед каждым тиком (транспорт не нужен).
    manager.on_health(
        &HealthInput::Stats {
            active: 1,
            bytes_up: 1,
            bytes_down: 1,
        },
        t0(),
    );
    manager.on_tick(t0());
    manager.on_health(
        &HealthInput::Stats {
            active: 2,
            bytes_up: 2,
            bytes_down: 2,
        },
        t0(),
    );
    manager.on_tick(t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Active);
}

#[test]
fn icmp_ping_alone_does_not_mean_active() {
    // health_target-зонд (ProbeOk) не копит success и не делает профиль
    // ACTIVE: только transport/data сигналы (M4b требование 7).
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(&HealthInput::ProbeOk, t0());
    manager.on_health(&HealthInput::ProbeOk, t0());
    manager.on_tick(t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Connecting);
}

#[test]
fn probe_failure_counts_as_transient() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    for _ in 0..3 {
        manager.on_health(&HealthInput::ProbeFailed, t0());
    }
    assert_ne!(manager.active_id(), Some("p10"));
}

#[test]
fn active_zero_is_data_health_failure() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    make_healthy(&mut manager, "p10", t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Active);
    // Все соединения упали → transient fail.
    manager.on_health(&HealthInput::ActiveZero, t0());
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 1);
}

// --- Требование 8/9: Routing + stable interface (M3X regression) ---

#[test]
fn pool_config_is_interface_only() {
    let manager = PoolManager::new(pool(), t0());
    match manager.next_action(t0()) {
        Action::Start { config, .. } => {
            assert!(!config.install_routes);
            assert!(!config.apply_dns);
            assert_eq!(config.tun_uds, crate::uci::TUN_INTERFACE);
        }
        other => panic!("ожидался Start, получено {other:?}"),
    }
}

#[test]
fn routing_mode_auto_in_status_summary() {
    let manager = PoolManager::new(pool(), t0());
    let snapshot = manager.snapshot(t0(), DaemonState::Running, Vec::new());
    assert_eq!(snapshot.routing.mode, "auto");
    assert!(!snapshot.routing.install_routes);
    assert!(!snapshot.routing.apply_dns);
    assert_eq!(snapshot.tunnel.interface, "csqtt0");
}

#[test]
fn routing_none_mode_reports_safe_summary() {
    let mut pool = pool();
    pool.routing.mode = RoutingMode::None;
    let manager = PoolManager::new(pool, t0());
    let snapshot = manager.snapshot(t0(), DaemonState::Running, Vec::new());
    assert_eq!(snapshot.routing.mode, "none");
    assert!(snapshot.routing.summary.contains("disabled"));
}

// --- Требование 10: SIGHUP reconcile ---

#[test]
fn reload_removes_active_profile_and_reselects() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let mut reloaded = pool();
    reloaded
        .servers
        .retain(|profile| profile.section_id != "p10");
    manager.reload(reloaded, t0());
    assert!(manager.pool.server("p10").is_none());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn reload_changed_priority_reorders() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let mut reloaded = pool();
    server_mut(&mut reloaded, "p20").priority = 5;
    manager.reload(reloaded, t0());
    // p20 теперь лучший — активный p10 больше не лучший, пересчёт выбора.
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn reload_switches_selection_mode_to_manual() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let mut reloaded = pool();
    reloaded.main.selection_mode = SelectionMode::Manual;
    reloaded.main.active_profile = "p30".to_string();
    manager.reload(reloaded, t0());
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p30"),
        other => panic!("ожидался Start p30, получено {other:?}"),
    }
}

#[test]
fn reload_preserves_counters_for_kept_profiles() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(&HealthInput::NetworkSuspect, t0());
    manager.reload(pool(), t0());
    assert_eq!(manager.runtimes["p10"].consecutive_fails, 1);
}

#[test]
fn reload_disabled_profile_goes_to_disabled() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let mut reloaded = pool();
    server_mut(&mut reloaded, "p10").enabled = false;
    manager.reload(reloaded, t0());
    assert_eq!(manager.runtimes["p10"].state, ProfileState::Disabled);
    match manager.next_action(t0()) {
        Action::Start { id, .. } => assert_eq!(id, "p20"),
        other => panic!("ожидался Start p20, получено {other:?}"),
    }
}

#[test]
fn reload_changed_settings_apply_immediately() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let mut reloaded = pool();
    reloaded.main.fail_threshold = 1;
    manager.reload(reloaded, t0());
    assert_eq!(manager.effective_fail_threshold_for("p10"), 1);
}

// --- Требование 12: all profiles unavailable ---

#[test]
fn all_profiles_unavailable_stops_without_infinite_loop() {
    let mut manager = PoolManager::new(pool(), t0());
    for id in ["p10", "p20", "p30"] {
        manager.on_client_started(id, t0());
        let result: Result<()> = Err(anyhow::anyhow!("FATAL_AUTH"));
        manager.on_client_result(id, &result, false, t0());
    }
    assert_eq!(manager.next_action(t0()), Action::Stop);
}

#[test]
fn all_in_cooldown_waits_for_expiry() {
    let mut manager = PoolManager::new(pool(), t0());
    for id in ["p10", "p20", "p30"] {
        manager.on_client_started(id, t0());
        for _ in 0..3 {
            manager.on_health(&HealthInput::NetworkSuspect, t0());
        }
    }
    // Все в cooldown — Stop.
    assert_eq!(manager.next_action(t0()), Action::Stop);
    // После истечения — снова eligible.
    let later = advance(t0(), 60);
    assert!(manager.eligible("p10", later));
    match manager.next_action(later) {
        Action::Start { id, .. } => assert_eq!(id, "p10"),
        other => panic!("ожидался Start p10, получено {other:?}"),
    }
}

// --- Требование 11: status snapshot ---

#[test]
fn snapshot_has_no_secrets() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    let result: Result<()> = Err(anyhow::anyhow!("password=secret-10 rejected"));
    manager.on_client_result("p10", &result, false, t0());
    let snapshot = manager.snapshot(t0(), DaemonState::Running, Vec::new());
    let json = snapshot.to_json().unwrap();
    assert!(!json.contains("secret-10"));
    assert!(json.contains("password=***"));
}

#[test]
fn snapshot_reports_tunnel_and_workers_and_uptime() {
    let mut manager = PoolManager::new(pool(), t0());
    manager.on_client_started("p10", t0());
    manager.on_health(
        &HealthInput::TunnelConfig {
            ip: "10.66.66.2".into(),
            dns: "1.1.1.1".into(),
        },
        t0(),
    );
    manager.on_health(
        &HealthInput::Stats {
            active: 7,
            bytes_up: 100,
            bytes_down: 200,
        },
        t0(),
    );
    let snapshot = manager.snapshot(advance(t0(), 120), DaemonState::Running, Vec::new());
    assert_eq!(snapshot.tunnel.address.as_deref(), Some("10.66.66.2"));
    assert_eq!(snapshot.tunnel.dns.as_deref(), Some("1.1.1.1"));
    assert_eq!(snapshot.workers.configured, 18);
    assert_eq!(snapshot.workers.active, 7);
    assert_eq!(snapshot.rx_bytes, 200);
    assert_eq!(snapshot.tx_bytes, 100);
    assert_eq!(snapshot.uptime_secs, 120);
    assert_eq!(snapshot.active_profile.as_deref(), Some("p10"));
}

#[test]
fn empty_pool_status_is_safe() {
    // Пул без профилей — daemon останавливается, snapshot не паникует.
    let pool = ProfilePool::default();
    let manager = PoolManager::new(pool, t0());
    assert_eq!(manager.next_action(t0()), Action::Stop);
    let snapshot = manager.snapshot(t0(), DaemonState::Disabled, Vec::new());
    assert!(snapshot.profiles.is_empty());
    assert!(snapshot.active_profile.is_none());
}

// --- Event parsing pipeline ---

#[test]
fn event_pipeline_maps_events_to_health_inputs() {
    use crate::events::parse_event_line;
    let record =
        parse_event_line("__CSQTT_EVENT__|STATS|{\"active\":3,\"bytes_up\":10,\"bytes_down\":20}")
            .unwrap();
    match event_to_health(&record) {
        Some(HealthInput::Stats {
            active,
            bytes_up,
            bytes_down,
        }) => assert_eq!((active, bytes_up, bytes_down), (3, 10, 20)),
        other => panic!("ожидался Stats, получено {other:?}"),
    }
    let record =
        parse_event_line("__CSQTT_EVENT__|CONFIG|{\"config\":\"TUNCONF:10.66.66.2:1.1.1.1\"}")
            .unwrap();
    match event_to_health(&record) {
        Some(HealthInput::TunnelConfig { ip, dns }) => {
            assert_eq!(ip, "10.66.66.2");
            assert_eq!(dns, "1.1.1.1");
        }
        other => panic!("ожидался TunnelConfig, получено {other:?}"),
    }
    // Не-TUNCONF CONFIG-строка игнорируется.
    let record = parse_event_line("__CSQTT_EVENT__|CONFIG|{\"config\":\"READY_OK\"}").unwrap();
    assert!(event_to_health(&record).is_none());
    // NETWORK_SUSPECT → transient signal.
    let record = parse_event_line("__CSQTT_EVENT__|NETWORK_SUSPECT|{}").unwrap();
    assert_eq!(event_to_health(&record), Some(HealthInput::NetworkSuspect));
}

// --- Daemon loop с fake runner ---

/// Fake runner: сохраняет конфиги, «работает» до отмены токена или
/// возвращает запрограммированную ошибку (после короткого ожидания,
/// давая службе CSQTT время получить события). Ok-результат через None+cancel.
struct FakeRunner {
    started: Mutex<Vec<ClientConfig>>,
    /// None — работает до отмены; Some(message) — Err(message).
    error: Option<String>,
}

impl FakeRunner {
    /// Работает до отмены токена (чистый путь).
    fn running() -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::new(Vec::new()),
            error: None,
        })
    }

    /// Возвращает Err(message) спустя 150мс.
    fn failing(message: &str) -> Arc<Self> {
        Arc::new(Self {
            started: Mutex::new(Vec::new()),
            error: Some(message.to_string()),
        })
    }

    fn started_peers(&self) -> Vec<String> {
        self.started
            .lock()
            .unwrap()
            .iter()
            .map(|config| config.peer.clone())
            .collect()
    }
}

impl ClientRunner for FakeRunner {
    fn run(
        &self,
        config: ClientConfig,
        cancel: CancellationToken,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + Send>> {
        self.started.lock().unwrap().push(config);
        let error = self.error.clone();
        Box::pin(async move {
            match error {
                Some(message) => {
                    // Даём драйверу шанс обработать старт, затем ошибку.
                    let _ =
                        tokio::time::timeout(Duration::from_millis(150), cancel.cancelled()).await;
                    Err(anyhow::anyhow!("{message}"))
                }
                None => {
                    cancel.cancelled().await;
                    Ok(())
                }
            }
        })
    }
}

fn tokio_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
}

struct DaemonFixture {
    options: DaemonOptions,
    cancel: CancellationToken,
}

fn fixture(runner: Arc<dyn ClientRunner>, uci: &str, tag: &str) -> DaemonFixture {
    fixture_with_helper(runner, uci, tag, None)
}

fn fixture_with_helper(
    runner: Arc<dyn ClientRunner>,
    uci: &str,
    tag: &str,
    helper: Option<Arc<crate::captcha_helper::HelperState>>,
) -> DaemonFixture {
    let dir = std::env::temp_dir().join(format!("csqtt-m4b-{tag}"));
    std::fs::create_dir_all(&dir).unwrap();
    let config_path = dir.join("csqtt");
    std::fs::write(&config_path, uci).unwrap();
    let cancel = CancellationToken::new();
    DaemonFixture {
        options: DaemonOptions {
            config_path,
            status_path: dir.join("status.json"),
            log_file: Some(dir.join("csqtt.log")),
            runner,
            cancel: Some(cancel.clone()),
            helper,
        },
        cancel,
    }
}

/// Драйверит службу CSQTT до отмены, возвращая статус и список пиров клиентов.
/// `settle_ms` — сколько ждать после инъекции команд (даём клиенту
/// отработать ошибку/переключение до отмены).
fn drive(
    fixture: DaemonFixture,
    settle_ms: u64,
    drive: impl Fn(&mpsc::UnboundedSender<DaemonCommand>),
) -> DaemonStatus {
    let runtime = tokio_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        drive(&commands);
        tokio::time::sleep(Duration::from_millis(settle_ms)).await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
    let text = std::fs::read_to_string(&status_path).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn daemon_starts_best_profile_and_stops_cleanly() {
    let runner = FakeRunner::running();
    let fixture = fixture(runner.clone(), UCI_3, "basic");
    let status = drive(fixture, 120, |_| {});
    assert_eq!(status.active_profile.as_deref(), Some("p10"));
    assert_eq!(runner.started_peers(), vec!["10.0.0.10:46000"]);
    assert!(status.tunnel.interface == "csqtt0");
    assert!(!text_contains_secret(&status));
}

#[test]
fn daemon_failovers_on_health_failures() {
    let runner = FakeRunner::running();
    let fixture = fixture(runner.clone(), UCI_3, "health-failover");
    let status = drive(fixture, 120, |commands| {
        // 3 transient-сигнала → fail_threshold → switch на p20.
        for _ in 0..3 {
            let _ = commands.send(DaemonCommand::Health(HealthInput::NetworkSuspect));
        }
    });
    assert_eq!(status.active_profile.as_deref(), Some("p20"));
    assert_eq!(
        runner.started_peers(),
        vec!["10.0.0.10:46000", "10.0.0.20:46000"]
    );
}

#[test]
fn daemon_failovers_on_fatal_auth() {
    let runner = FakeRunner::failing("FATAL_AUTH: неверный пароль подключения");
    let fixture = fixture(runner.clone(), UCI_3, "auth-failover");
    // Каждая попытка падает через 150мс: ждём 3×150 + запас на переключения.
    let status = drive(fixture, 700, |_| {});
    assert_eq!(
        runner.started_peers(),
        vec!["10.0.0.10:46000", "10.0.0.20:46000", "10.0.0.30:46000"]
    );
    // Все в AuthFailed — активного нет.
    assert!(status.active_profile.is_none());
    assert_eq!(status.daemon_state, "stopped");
}

#[test]
fn daemon_disabled_main_starts_no_client() {
    let runner = FakeRunner::running();
    let uci = UCI_3.replacen("option enabled '1'", "option enabled '0'", 1);
    let fixture = fixture(runner.clone(), &uci, "disabled");
    let status = drive(fixture, 120, |_| {});
    assert!(runner.started_peers().is_empty());
    assert_eq!(status.daemon_state, "stopped");
}

#[test]
fn daemon_sighup_reload_reconciles() {
    let runner = FakeRunner::running();
    let fixture = fixture(runner.clone(), UCI_3, "reload");
    let config_path = fixture.options.config_path.clone();
    let status = drive(fixture, 200, |commands| {
        // SIGHUP → reload c изменённым priority (p20 становится лучшим).
        let reloaded = UCI_3.replace(
                "config server 'p20'\n	option name 'Backup'\n	option enabled '1'\n	option priority '20'",
                "config server 'p20'\n	option name 'Backup'\n	option enabled '1'\n	option priority '5'",
            );
        std::fs::write(&config_path, reloaded).unwrap();
        let _ = commands.send(DaemonCommand::Reload);
    });
    // p20 теперь лучший: служба CSQTT переключилась на него.
    assert_eq!(status.active_profile.as_deref(), Some("p20"));
    assert_eq!(
        runner.started_peers(),
        vec!["10.0.0.10:46000", "10.0.0.20:46000"]
    );
}

#[test]
fn daemon_manual_mode_pins_profile() {
    let runner = FakeRunner::running();
    let uci = UCI_3
        .replace(
            "option selection_mode 'priority'",
            "option selection_mode 'manual'",
        )
        .replace("option active_profile ''", "option active_profile 'p30'");
    let fixture = fixture(runner.clone(), &uci, "manual");
    let status = drive(fixture, 150, |commands| {
        // Health-failover в manual не переключает на другие профили:
        // крутится только pinned p30.
        for _ in 0..3 {
            let _ = commands.send(DaemonCommand::Health(HealthInput::NetworkSuspect));
        }
    });
    // Активный — p30 (после cooldown он же перезапускается); другие
    // профили в manual не стартуют.
    let peers = runner.started_peers();
    assert!(!peers.is_empty());
    assert!(
        peers.iter().all(|peer| peer == "10.0.0.30:46000"),
        "manual mode must not start other profiles: {peers:?}"
    );
    let _ = status;
}

fn text_contains_secret(status: &DaemonStatus) -> bool {
    let json = status.to_json().unwrap();
    json.contains("secret-10") || json.contains("secret-20") || json.contains("secret-30")
}

// --- Требование M4d: wire-перехват, секреты, failover gate ---

/// `expires_at` обязан лежать в будущем: saturating_sub схлопывал его в now
/// (rust-review MAJOR). Проверяем чистую конвертацию challenge_status.
#[test]
fn challenge_status_epoch_conversion_keeps_future_expiry() {
    use crate::captcha_manager::{ChallengeState, ChallengeView};
    let now = t0();
    let view = ChallengeView {
        id: "chg-test".to_string(),
        profile_id: "p10".to_string(),
        mode: "auto".to_string(),
        state: ChallengeState::Pending,
        created: now,
        expires: now + Duration::from_secs(240),
    };
    let status = challenge_status(&view, now, 1_700_000_000);
    assert_eq!(status.created_at, 1_700_000_000);
    assert_eq!(status.expires_at, 1_700_000_240);
    assert!(status.expires_at > status.created_at);
    assert_eq!(status.state, "pending");
    assert_eq!(status.mode, "auto");
    // Протухший challenge: expires_at не уходит в прошлое (saturate в now).
    let stale = ChallengeView {
        expires: now,
        ..view
    };
    let stale_status = challenge_status(&stale, now + Duration::from_secs(10), 1_700_000_010);
    assert_eq!(stale_status.expires_at, 1_700_000_010);
}

/// Debug команд службы CSQTT никогда не выводит success token (rust-review MAJOR).
#[test]
fn daemon_command_debug_never_leaks_captcha_result() {
    let command = DaemonCommand::SubmitCaptchaResult {
        challenge_id: "chg-1".to_string(),
        result: "SUCCESS-TOKEN-SECRET-XYZ".to_string(),
    };
    let debug = format!("{command:?}");
    assert!(
        !debug.contains("SUCCESS-TOKEN-SECRET-XYZ"),
        "result secret must not leak via Debug"
    );
    assert!(debug.contains("chg-1"));
}

/// Wire-строка капчи перехватывается ДО файлового синка: session_token и
/// redirect_uri — секреты, в обычный лог они не попадают (требование 4).
#[test]
fn wire_captcha_request_never_reaches_log_sink() {
    let dir = std::env::temp_dir().join("csqtt-m4d-wire-redaction");
    std::fs::create_dir_all(&dir).unwrap();
    let log_path = dir.join("csqtt.log");
    let sink = Arc::new(crate::logsink::FileLogSink::new(
        log_path.clone(),
        1024 * 1024,
        vec![],
    ));
    let (tx, mut rx) = mpsc::unbounded_channel();
    process_log_line(
        "CAPTCHA_SOLVE|auto|https://vk.com/captcha?sid=1|SESSION-SECRET-XYZ",
        Some(&sink),
        &tx,
    );
    let text = std::fs::read_to_string(&log_path).unwrap_or_default();
    assert!(
        !text.contains("SESSION-SECRET-XYZ"),
        "session_token must not land in the log file"
    );
    assert!(matches!(
        rx.try_recv(),
        Ok(DaemonCommand::CaptchaRequest { .. })
    ));
    // Обычный лог пишется в синк как есть.
    process_log_line("[КЛИЕНТ] обычная строка лога", Some(&sink), &tx);
    assert!(
        std::fs::read_to_string(&log_path)
            .unwrap()
            .contains("обычная строка лога")
    );
    // [High-1 AUDIT] session_token внутри диагностической строки (URL в
    // ошибке HTTP-клиента) вырезается до записи в лог-файл.
    process_log_line(
        "[КАПЧА] solveOnce failed https://id.vk.com/captcha?session_token=LEAKED-SINK-9",
        Some(&sink),
        &tx,
    );
    sink.flush();
    let text = std::fs::read_to_string(&log_path).unwrap();
    assert!(
        !text.contains("LEAKED-SINK-9"),
        "session_token in error strings must not land in the log file"
    );
    assert!(text.contains("session_token=***"));
}

/// При policy=failover wire-запрос капчи: p10 → captcha_required, клиент
/// отменён, стартует p20; challenge зарегистрирован, секреты изолированы.
#[test]
fn daemon_failovers_on_captcha_wire_request() {
    let runner = FakeRunner::running();
    let fixture = fixture(runner.clone(), UCI_3, "captcha-failover");
    let status = drive(fixture, 300, |commands| {
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|auto|https://vk.com/captcha?sid=1|SESSION-SECRET-FAIL",
        )
        .expect("valid wire line");
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
    });
    assert_eq!(status.active_profile.as_deref(), Some("p20"));
    assert_eq!(
        runner.started_peers(),
        vec!["10.0.0.10:46000", "10.0.0.20:46000"]
    );
    let p10 = status
        .profiles
        .iter()
        .find(|profile| profile.id == "p10")
        .expect("p10 in status");
    assert_eq!(p10.state, "captcha_required");
    // Challenge зарегистрирован, но ждущего клиента уже нет → failed.
    assert_eq!(status.captcha_challenges.len(), 1);
    assert_eq!(status.captcha_challenges[0].profile_id, "p10");
    assert_eq!(status.captcha_challenges[0].mode, "auto");
    assert_eq!(status.captcha_challenges[0].state, "failed");
    // Секреты не утекли в status.json.
    let json = status.to_json().unwrap();
    assert!(
        !json.contains("SESSION-SECRET-FAIL"),
        "session_token must not leak into status.json"
    );
    assert!(!text_contains_secret(&status));
}

/// При policy=wait служба CSQTT не рвёт клиента, ждущего решения: challenge жив,
/// слот занят; результат принимается менеджером (доставка — в ядро).
#[test]
fn captcha_wait_policy_keeps_client_and_tracks_result() {
    let runner = FakeRunner::running();
    let uci = UCI_3.replace(
        "option captcha_policy 'failover'",
        "option captcha_policy 'wait'",
    );
    let fixture = fixture(runner.clone(), &uci, "captcha-wait");
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    let mid_status_path = status_path.clone();
    // Multi-thread runtime: драйвер теста спит асинхронно, а служба CSQTT в это
    // время обрабатывает команды. На current-thread runtime блокирующий
    // sleep морил бы служба CSQTT голоданием (status.json не успел бы появиться).
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    runtime.block_on(async move {
        // Сериализация с bridge-тестом M4e: он временно ставит глобальный
        // CONTROL_TX, а этот тест проверяет путь «канала нет → failed».
        let _control_guard = CAPTCHA_CONTROL_TEST_LOCK.lock().await;
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|manual|https://vk.com/captcha?sid=7|SESSION-SECRET-WAIT",
        )
        .expect("valid wire line");
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Берём свежий challenge-id из status.json (служба CSQTT пишет его сразу).
        let current: DaemonStatus =
            serde_json::from_str(&std::fs::read_to_string(&mid_status_path).unwrap()).unwrap();
        let challenge_id = current.captcha_challenges[0].id.clone();
        // Клиент всё ещё активен (wait) — результат уходит менеджеру.
        assert_eq!(current.active_profile.as_deref(), Some("p10"));
        let _ = commands.send(DaemonCommand::SubmitCaptchaResult {
            challenge_id,
            result: "solve-token-value".to_string(),
        });
        tokio::time::sleep(Duration::from_millis(150)).await;
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
    let status: DaemonStatus =
        serde_json::from_str(&std::fs::read_to_string(&status_path).unwrap()).unwrap();
    // Клиент не был отменён ради капчи.
    assert_eq!(status.active_profile.as_deref(), Some("p10"));
    assert_eq!(runner.started_peers(), vec!["10.0.0.10:46000"]);
    // Результат доставить некому (FakeRunner не держит control-канал ядра) —
    // challenge помечен failed, но профиль не был вырван.
    assert_eq!(status.captcha_challenges[0].state, "failed");
    let json = status.to_json().unwrap();
    assert!(
        !json.contains("SESSION-SECRET-WAIT"),
        "session_token must not leak into status.json"
    );
}

// --- M4e: Web Helper поверх реальной службы CSQTT (auth/capability/bridge) ---

fn wait_uci() -> String {
    UCI_3.replace(
        "option captcha_policy 'failover'",
        "option captcha_policy 'wait'",
    )
}

fn multi_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap()
}

fn read_status(path: &std::path::Path) -> DaemonStatus {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn capability_from_url(url: &str) -> String {
    url.rsplit("?cap=")
        .next()
        .expect("cap param in helper url")
        .to_string()
}

fn ticket_from_page(html: &str) -> String {
    let start = html.find("go?t=").expect("go link with ticket") + 5;
    html[start..start + 43].to_string()
}

/// Wire-запрос капчи регистрирует challenge и capability в helper-состоянии:
/// helper-url содержит ТОЛЬКО LAN-URL + id + capability (без секретов VK).
#[test]
fn helper_url_recorded_after_wire_request_has_no_secrets() {
    let helper = Arc::new(crate::captcha_helper::HelperState::new(
        "http://192.168.1.1:8443".to_string(),
    ));
    let runner = FakeRunner::running();
    let fixture = fixture_with_helper(runner, &wait_uci(), "m4e-url", Some(helper.clone()));
    let runtime = multi_thread_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|auto|https://id.vk.com/captcha?session_token=SECRET-M4E-URL|SESSION-TOKEN-SECRET",
        )
        .unwrap();
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        let id = status.captcha_challenges[0].id.clone();
        let url = helper.helper_url(&id).expect("capability recorded by daemon");
        assert!(url.starts_with(&format!("http://192.168.1.1:8443/c/{id}?cap=")));
        assert_eq!(capability_from_url(&url).len(), 43, "32 bytes url-safe base64");
        assert!(!url.contains("SECRET-M4E-URL"));
        assert!(!url.contains("SESSION-TOKEN-SECRET"));
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
}

/// Полный мост M4e: capability → страница (ticket) → submit mock-результата →
/// служба CSQTT доставляет `CAPTCHA_RESULT|…` в control-канал ядра → challenge solved.
#[test]
fn helper_bridge_delivers_mock_result_to_core_control_channel() {
    let helper = Arc::new(crate::captcha_helper::HelperState::new(
        "http://127.0.0.1:8443".to_string(),
    ));
    let runner = FakeRunner::running();
    let fixture = fixture_with_helper(
        runner.clone(),
        &wait_uci(),
        "m4e-bridge",
        Some(helper.clone()),
    );
    let runtime = multi_thread_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let _control_guard = CAPTCHA_CONTROL_TEST_LOCK.lock().await;
        let (control_tx, mut control_rx) =
            mpsc::unbounded_channel::<String>();
        crate::set_control_channel_for_test(control_tx);
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|manual|https://id.vk.com/captcha?session_token=SECRET-BRIDGE|SESSION-TOKEN-BRIDGE",
        )
        .unwrap();
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        let id = status.captcha_challenges[0].id.clone();
        assert_eq!(status.captcha_challenges[0].state, "pending");
        // 1. Browser открывает helper-URL: capability гасится службой CSQTT.
        let cap = capability_from_url(&helper.helper_url(&id).unwrap());
        let page = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Open { id: id.clone(), cap: cap.clone() },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(page.status, 200);
        let html = String::from_utf8(page.body).unwrap();
        assert!(!html.contains("SECRET-BRIDGE"), "redirect_uri stays server-side");
        assert!(!html.contains("SESSION-TOKEN-BRIDGE"));
        assert!(!html.contains(&cap), "capability not echoed back");
        let ticket = ticket_from_page(&html);
        // Replay capability (повторный Open тем же токеном) — reject.
        let replay = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Open { id: id.clone(), cap },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(replay.status, 403, "capability is single-use");
        // 2. /go — server-side redirect на штатный VK flow (секрет не в HTML).
        let go = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Go { id: id.clone(), ticket: ticket.clone() },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(go.status, 302);
        assert_eq!(go.location.unwrap(), "https://id.vk.com/captcha?session_token=SECRET-BRIDGE");
        // 3. Submit mock-результата с ticket → полный мост до control-канала.
        let submit = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Submit {
                id: id.clone(),
                ticket: ticket.clone(),
                result: "mock-success-token-42".to_string(),
            },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(submit.status, 200);
        let line = tokio::time::timeout(Duration::from_secs(2), control_rx.recv())
            .await
            .expect("core control channel must receive the result")
            .expect("channel open");
        assert_eq!(line, "CAPTCHA_RESULT|mock-success-token-42");
        // Replay submit тем же ticket — reject (одноразовая сессия).
        let replay = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Submit {
                id: id.clone(),
                ticket: ticket.clone(),
                result: "mock-success-token-42".to_string(),
            },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(replay.status, 403, "second submit must be rejected");
        // Чужой ticket — reject.
        let forged = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Submit {
                id,
                ticket: "x".repeat(43),
                result: "forged".to_string(),
            },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(forged.status, 403);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        assert_eq!(status.captcha_challenges[0].state, "solved");
        assert_eq!(status.active_profile.as_deref(), Some("p10"));
        crate::clear_control_channel_for_test();
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
    let _ = runner;
}

/// Failover (требование 12): пока A ждёт капчу (policy=failover → B ACTIVE),
/// результат A не доставляется (профиль не активен) и B не обрывается.
#[test]
fn helper_submit_for_non_active_profile_does_not_rip_active_backup() {
    let helper = Arc::new(crate::captcha_helper::HelperState::new(
        "http://192.168.1.1:8443".to_string(),
    ));
    let runner = FakeRunner::running();
    let fixture = fixture_with_helper(runner.clone(), UCI_3, "m4e-failover", Some(helper.clone()));
    let runtime = multi_thread_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let _control_guard = CAPTCHA_CONTROL_TEST_LOCK.lock().await;
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|auto|https://id.vk.com/captcha?session_token=SECRET-FO|SESSION-TOKEN-FO",
        )
        .unwrap();
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(200)).await;
        let status = read_status(&status_path);
        // failover: p10 → captcha_required, активен p20; challenge p10 закрыт
        // (ждать некому) — helper-URL для него уже не выдаётся.
        assert_eq!(status.active_profile.as_deref(), Some("p20"));
        let id = status.captcha_challenges[0].id.clone();
        assert_eq!(status.captcha_challenges[0].state, "failed");
        // Даже по ещё не выметенной capability открыть браузер нельзя:
        // challenge не живой → consume отказывает (403).
        if let Some(url) = helper.helper_url(&id) {
            let denied = crate::captcha_helper::dispatch(
                crate::captcha_helper::Route::Open {
                    id: id.clone(),
                    cap: capability_from_url(&url),
                },
                std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
                &helper,
                &commands,
            )
            .await;
            assert_eq!(
                denied.status, 403,
                "dead challenge capability must be rejected"
            );
        }
        let submit = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::Submit {
                id,
                ticket: "y".repeat(43),
                result: "late-result".to_string(),
            },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(submit.status, 403, "no session → no delivery");
        tokio::time::sleep(Duration::from_millis(100)).await;
        let status = read_status(&status_path);
        assert_eq!(
            status.active_profile.as_deref(),
            Some("p20"),
            "B stays ACTIVE"
        );
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
    let _ = runner;
}

/// Интеграционный API M6 (loopback-only): snapshot/cancel через реальную службу CSQTT.
#[test]
fn helper_api_snapshot_and_cancel_via_daemon() {
    let helper = Arc::new(crate::captcha_helper::HelperState::new(
        "http://192.168.1.1:8443".to_string(),
    ));
    let runner = FakeRunner::running();
    let fixture = fixture_with_helper(runner.clone(), &wait_uci(), "m4e-api", Some(helper.clone()));
    let runtime = multi_thread_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|auto|https://id.vk.com/captcha?session_token=SECRET-API|SESSION-TOKEN-API",
        )
        .unwrap();
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        let id = status.captcha_challenges[0].id.clone();
        // list — safe fields.
        let list = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::ApiList,
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        let body = String::from_utf8(list.body).unwrap();
        assert!(body.contains(&id));
        assert!(body.contains("\"state\":\"pending\""));
        assert!(!body.contains("SECRET-API"));
        // helper-url — только с loopback.
        let lan = std::net::IpAddr::V4(std::net::Ipv4Addr::new(192, 168, 1, 77));
        let denied = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::ApiHelperUrl { id: id.clone() },
            lan,
            &helper,
            &commands,
        )
        .await;
        assert_eq!(denied.status, 403);
        // cancel через API → профиль возвращается в Standby-очередь (wait),
        // challenge — cancelled.
        let cancelled = crate::captcha_helper::dispatch(
            crate::captcha_helper::Route::ApiCancel { id: id.clone() },
            std::net::IpAddr::V4(std::net::Ipv4Addr::LOCALHOST),
            &helper,
            &commands,
        )
        .await;
        assert_eq!(cancelled.status, 200);
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        assert_eq!(status.captcha_challenges[0].state, "cancelled");
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
    let _ = runner;
}

/// redirect_uri вне штатного VK/OK flow не получает 302 (порт CaptchaUriPolicy).
#[test]
fn helper_consume_rejects_non_vk_redirect_uri() {
    let helper = Arc::new(crate::captcha_helper::HelperState::new(
        "http://192.168.1.1:8443".to_string(),
    ));
    let runner = FakeRunner::running();
    let fixture = fixture_with_helper(runner, &wait_uci(), "m4e-uri-policy", Some(helper.clone()));
    let runtime = multi_thread_runtime();
    let status_path = fixture.options.status_path.clone();
    let cancel = fixture.cancel.clone();
    runtime.block_on(async move {
        let (daemon, commands, receiver) = Daemon::new(fixture.options, Instant::now()).unwrap();
        let task = tokio::spawn(async move { daemon.run(receiver).await });
        let request = parse_captcha_solve_line(
            "CAPTCHA_SOLVE|auto|https://evil.example.com/phish|SESSION-TOKEN-EVIL",
        )
        .unwrap();
        let _ = commands.send(DaemonCommand::CaptchaRequest { request });
        tokio::time::sleep(Duration::from_millis(150)).await;
        let status = read_status(&status_path);
        let id = status.captcha_challenges[0].id.clone();
        let cap = capability_from_url(&helper.helper_url(&id).unwrap());
        let (tx, rx) = tokio::sync::oneshot::channel();
        let _ = commands.send(DaemonCommand::HelperConsume {
            challenge_id: id,
            token: cap,
            reply: tx,
        });
        assert_eq!(rx.await.unwrap(), None, "non-VK redirect must be rejected");
        cancel.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
    });
}

/// Debug новых команд не печатает capability-токен.
#[test]
fn helper_command_debug_redacts_capability_token() {
    let command = DaemonCommand::HelperConsume {
        challenge_id: "chg-1".to_string(),
        token: "CAPABILITY-SECRET-XYZ".to_string(),
        reply: tokio::sync::oneshot::channel().0,
    };
    let debug = format!("{command:?}");
    assert!(!debug.contains("CAPABILITY-SECRET-XYZ"));
    assert!(debug.contains("chg-1"));
}
