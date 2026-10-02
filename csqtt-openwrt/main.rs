// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Порт клиента CSQTT для OpenWrt (aarch64_cortex-a53).
//! Ядро — csqtt-core (форк focsq), CLI восстановлен по main.rs апстрима.
//! Нативный Linux TUN: бинарник сам создаёт интерфейс (по умолчанию
//! csqtt0) через /dev/net/tun и применяет TUNCONF от сервера.

use clap::{CommandFactory, Parser};
use csqtt_core::{
    ClientConfig,
    pool::{ClientRunner, CoreClientRunner, Daemon, DaemonOptions, install_event_pipeline},
    run_client, run_vk_hash_validation,
};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Parser)]
#[command(
    name = "csqtt",
    about = "CSQTT клиент для OpenWrt: туннель поверх TURN/RTP с нативным TUN-интерфейсом",
    disable_help_flag = true
)]
struct Arguments {
    /// TURN-сервер (host override), например turn:relay.example.com
    #[arg(long, default_value = "")]
    turn: String,
    /// Порт TURN-сервера (override)
    #[arg(long, default_value = "")]
    port: String,
    /// Локальный UDP-адрес (только без TUN-режима)
    #[arg(long, default_value = "127.0.0.1:9000")]
    listen: String,
    /// Хеши VK через запятую
    #[arg(long, default_value = "", allow_hyphen_values = true)]
    vk: String,
    /// manual | auto_js
    #[arg(long, default_value = "manual")]
    vk_hash_mode: String,
    /// Пир: ip:port сервера CSQTT
    #[arg(long, default_value = "")]
    peer: String,
    /// Количество воркеров (кратно 9)
    #[arg(short = 'n', long, default_value_t = 18)]
    workers: usize,
    #[arg(long, default_value_t = false)]
    allow_hash_redistribution: bool,
    /// Идентификатор устройства
    #[arg(long, default_value = "openwrt")]
    device_id: String,
    /// Пароль подключения (для WRAP-ключа)
    #[arg(long, default_value = "")]
    password: String,
    /// vkcalls | legacy | auto_js
    #[arg(long, default_value = "vkcalls")]
    vk_auth_mode: String,
    /// auto | wv | rjs
    #[arg(long, default_value = "auto")]
    captcha_mode: String,
    /// TLS-фингерпринт: chrome | firefox
    #[arg(long, default_value = "chrome")]
    fingerprint: String,
    /// Client IDs через запятую
    #[arg(long, default_value = "")]
    client_ids: String,
    /// audio | video | silent
    #[arg(long, default_value = "audio")]
    obfs: String,
    /// udp | tcp | tcp_tls
    #[arg(long, default_value = "udp")]
    turn_transport: String,
    #[arg(long = "gen", default_value_t = 0)]
    generation: u64,
    #[arg(long, default_value = "")]
    salt: String,
    /// Имя TUN-интерфейса (нативный режим Linux): csqtt0
    #[arg(long, default_value = "csqtt0")]
    tun: String,
    /// Отключить нативный TUN (UDP-режим как у апстримного CLI)
    #[arg(long, default_value_t = false)]
    no_tun: bool,
    /// [M3R, ручной режим] Ставить half-маршруты 0/1+128/1 и exclude
    /// VK/TURN («весь трафик через туннель»). По умолчанию OFF — принцип
    /// изоляции: csqtt0 только интерфейс, маршрутизацию делает прокси.
    #[arg(long, default_value_t = false)]
    routes: bool,
    /// [M3R, ручной режим] Применять системный DNS из TUNCONF.
    /// По умолчанию OFF — системный DNS роутера не меняется.
    #[arg(long, default_value_t = false)]
    apply_dns: bool,
    #[arg(long, default_value_t = false)]
    validate_vk_hashes: bool,
    /// Токен VK для режима auto_js
    #[arg(long, default_value = "")]
    vk_js_token: String,
    /// Показать справку
    #[arg(short = 'h', long)]
    help: bool,
}

impl Arguments {
    fn to_config(&self) -> ClientConfig {
        ClientConfig {
            turn: self.turn.clone(),
            port: self.port.clone(),
            listen: self.listen.clone(),
            vk: self.vk.clone(),
            vk_hash_mode: self.vk_hash_mode.clone(),
            peer: self.peer.clone(),
            workers: self.workers,
            allow_hash_redistribution: self.allow_hash_redistribution,
            device_id: self.device_id.clone(),
            password: self.password.clone(),
            vk_auth_mode: self.vk_auth_mode.clone(),
            captcha_mode: self.captcha_mode.clone(),
            fingerprint: self.fingerprint.clone(),
            client_ids: self.client_ids.clone(),
            obfs: self.obfs.clone(),
            turn_transport: self.turn_transport.clone(),
            generation: self.generation,
            salt: self.salt.clone(),
            // focsq-модель: непустое tun_uds включает нативный TUN-режим,
            // значение = имя интерфейса (csqtt0)
            tun_uds: if !self.no_tun {
                self.tun.clone()
            } else {
                String::new()
            },
            validate_vk_hashes: self.validate_vk_hashes,
            vk_js_token: self.vk_js_token.clone(),
            install_routes: self.routes,
            apply_dns: self.apply_dns,
        }
    }
}

fn main() {
    std::panic::set_hook(Box::new(|_| {
        #[cfg(unix)]
        unsafe {
            const MESSAGE: &[u8] = b"[PANIC] Rust client task failed\n";
            let _ = libc::write(libc::STDERR_FILENO, MESSAGE.as_ptr().cast(), MESSAGE.len());
        }
    }));
    // [OpenWrt-порт, M4b] `csqtt run` — profile pool daemon (UCI config,
    // failover/failback, status.json, SIGHUP-reload). procd (M5) зовёт его.
    // [OpenWrt-порт, M4c] Прочие подкоманды (status/doctor/profile/captcha/
    // log/version) — cli::handle. Всё остальное — legacy one-shot клиент
    // (ручной режим M3R): прямые флаги, как у апстримного CLI.
    let raw: Vec<String> = std::env::args().collect();
    if raw.get(1).map(String::as_str) == Some("run") {
        return run_daemon_entry(&raw[2..]);
    }
    if let Some(exit_code) = csqtt_core::cli::handle(&raw[1..]) {
        std::process::exit(exit_code);
    }
    let arguments = Arguments::parse();
    if arguments.help {
        println!("{}", Arguments::command().render_help());
        return;
    }
    let config = arguments.to_config();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_worker_threads(config.workers))
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[ФАТАЛ] {error:#}");
            std::process::exit(1);
        }
    };
    let failure = runtime.block_on(async {
        match tokio::spawn(async move {
            if config.validate_vk_hashes {
                run_vk_hash_validation(&config).await
            } else {
                run_client(config, None).await
            }
        })
        .await
        {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => Some(format!("паника верхнего уровня изолирована: {error}")),
        }
    });
    if let Some(failure) = failure {
        eprintln!("[ФАТАЛ] {failure}");
        let _ = csqtt_core::logging_shutdown(Duration::from_secs(1));
        std::process::exit(1);
    }
    let _ = csqtt_core::logging_shutdown(Duration::from_secs(1));
}

const STREAMS_PER_RUNTIME_WORKER: usize = 12;
const MAX_RUNTIME_WORKER_THREADS: usize = 4;

fn runtime_worker_threads(requested_workers: usize) -> usize {
    (requested_workers
        .clamp(9, 126)
        .div_ceil(STREAMS_PER_RUNTIME_WORKER))
    .clamp(1, MAX_RUNTIME_WORKER_THREADS)
}

// ===========================================================================
// [OpenWrt-порт, M4b] `csqtt run` — profile pool daemon
// ===========================================================================

/// Аргументы `csqtt run`: переопределение путей UCI/status/log для procd
/// и отладки. По умолчанию — production-пути контракта M3X/M4a.
#[derive(Parser)]
#[command(
    name = "csqtt run",
    about = "Profile pool daemon: UCI config, failover/failback, status.json",
    disable_help_flag = true
)]
struct RunArguments {
    /// Путь к UCI-конфигу (по умолчанию /etc/config/csqtt)
    #[arg(long, default_value = "/etc/config/csqtt")]
    config: String,
    /// Путь к status.json (по умолчанию /var/run/csqtt/status.json)
    #[arg(long, default_value = "/var/run/csqtt/status.json")]
    status: String,
    /// Лог-файл (по умолчанию main.log_file из UCI)
    #[arg(long, default_value = "")]
    log: String,
    /// [M4e] LAN-адрес:порт Web Helper капчи (только приватный/loopback).
    /// `auto` (по умолчанию) — определить LAN-адрес роутера из
    /// `/etc/config/network` (без shell); пусто/`off` — helper выключен.
    /// WAN-адрес и 0.0.0.0 отклоняются политикой.
    #[arg(long, default_value = "auto")]
    helper_listen: String,
    /// [M4e] Выключить Web Helper капчи (то же, что --helper-listen '').
    #[arg(long)]
    no_helper: bool,
    /// Показать справку
    #[arg(short = 'h', long)]
    help: bool,
}

fn run_daemon_entry(args: &[String]) {
    let mut run_args = RunArguments::try_parse_from(
        std::iter::once("csqtt-run".to_string()).chain(args.iter().cloned()),
    )
    .unwrap_or_else(|error| {
        eprintln!("{error:#}");
        std::process::exit(2);
    });
    if run_args.help {
        println!("{}", RunArguments::command().render_help());
        return;
    }
    let log_file = if run_args.log.trim().is_empty() {
        None
    } else {
        Some(std::path::PathBuf::from(&run_args.log))
    };
    // [M4e] Web Helper: адрес обязателен LAN-only (WAN/0.0.0.0 → фатально,
    // это политика изоляции, а не рантайм-неудача). Bind-ошибка позже —
    // только предупреждение: служба CSQTT работает и без helper.
    let helper = if run_args.no_helper {
        None
    } else {
        let value = run_args.helper_listen.trim().to_string();
        let addr = if value.is_empty() || value == "off" {
            None
        } else if value == "auto" {
            // LAN-адрес определяет сама служба CSQTT (без shell/ip/uci): WAN не открываем.
            csqtt_core::captcha_helper::detect_lan_listen(
                csqtt_core::captcha_helper::DEFAULT_HELPER_PORT,
            )
        } else {
            match value.parse::<std::net::SocketAddr>() {
                Ok(addr) => Some(addr),
                Err(error) => {
                    eprintln!("[ФАТАЛ] --helper-listen: не адрес:порт ({error}): {value}");
                    std::process::exit(2);
                }
            }
        };
        match addr {
            Some(addr) => {
                if let Err(reason) = csqtt_core::captcha_helper::validate_listen_addr(addr) {
                    eprintln!("[ФАТАЛ] --helper-listen {addr}: {reason}");
                    std::process::exit(2);
                }
                Some((
                    addr,
                    std::sync::Arc::new(csqtt_core::captcha_helper::HelperState::new(format!(
                        "http://{addr}"
                    ))),
                ))
            }
            None => None,
        }
    };
    let cancel = CancellationToken::new();
    let options = DaemonOptions {
        config_path: std::path::PathBuf::from(std::mem::take(&mut run_args.config)),
        status_path: std::path::PathBuf::from(std::mem::take(&mut run_args.status)),
        log_file,
        runner: std::sync::Arc::new(CoreClientRunner) as std::sync::Arc<dyn ClientRunner>,
        cancel: Some(cancel.clone()),
        helper: helper.as_ref().map(|(_, state)| state.clone()),
    };
    // Машино-читаемые события — единственный runtime state: форсируем
    // CSQTT_EVENTS и ставим лог-колбэк до старта службы CSQTT (события run_client
    // идут через него в канал команд).
    csqtt_core::set_events_enabled(true);
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .worker_threads(runtime_worker_threads(18))
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("[ФАТАЛ] {error:#}");
            std::process::exit(1);
        }
    };
    runtime.block_on(async move {
        let (daemon, commands, receiver) = match Daemon::new(options, std::time::Instant::now()) {
            Ok(triple) => triple,
            Err(error) => {
                eprintln!("[ФАТАЛ] {error:#}");
                std::process::exit(1);
            }
        };
        // Файловый лог-синк + парсинг событий в команды службы CSQTT (M4a+M4b).
        install_event_pipeline(daemon.log_sink(), commands.clone());
        #[cfg(unix)]
        csqtt_core::pool::install_signal_handlers(cancel.clone(), commands.clone());
        // [M4e] Поднять Web Helper: bind-ошибка (например, LAN-адреса ещё
        // нет) не валит службу CSQTT — предупреждение, helper просто недоступен.
        let mut helper_task = None;
        if let Some((addr, state)) = helper {
            match csqtt_core::captcha_helper::spawn_helper(
                addr,
                state,
                commands.clone(),
                cancel.clone(),
            )
            .await
            {
                Ok(task) => {
                    eprintln!("[СЛУЖБА CSQTT] [HELPER] Web Helper слушает {addr} (LAN-only)");
                    helper_task = Some(task);
                }
                Err(error) => eprintln!(
                    "[СЛУЖБА CSQTT] [HELPER] не удалось поднять на {addr}: {error} — служба CSQTT работает без helper"
                ),
            }
        }
        let result = daemon.run(receiver).await;
        if let Some(task) = helper_task {
            let _ = tokio::time::timeout(Duration::from_secs(2), task).await;
        }
        if let Err(error) = result {
            eprintln!("[ФАТАЛ] {error:#}");
            let _ = csqtt_core::logging_shutdown(Duration::from_secs(1));
            std::process::exit(1);
        }
        let _ = csqtt_core::logging_shutdown(Duration::from_secs(1));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tun_disabled_maps_to_empty_uds() {
        let arguments = Arguments::try_parse_from([
            "csqtt",
            "--peer",
            "127.0.0.1:9000",
            "--vk",
            "-Wabc",
            "--password",
            "secret",
            "--no-tun",
        ])
        .unwrap();
        let config = arguments.to_config();
        assert!(config.tun_uds.is_empty());
    }

    #[test]
    fn tun_enabled_by_default_uses_csqtt0() {
        let arguments = Arguments::try_parse_from([
            "csqtt",
            "--peer",
            "127.0.0.1:9000",
            "--vk",
            "-Wabc",
            "--password",
            "secret",
        ])
        .unwrap();
        let config = arguments.to_config();
        assert_eq!(config.tun_uds, "csqtt0");
    }

    /// [M3R] Дефолт — interface-only: ни маршрутов, ни DNS от службы CSQTT.
    #[test]
    fn isolation_flags_default_to_off() {
        let arguments = Arguments::try_parse_from([
            "csqtt",
            "--peer",
            "127.0.0.1:9000",
            "--vk",
            "-Wabc",
            "--password",
            "secret",
        ])
        .unwrap();
        let config = arguments.to_config();
        assert!(!config.install_routes);
        assert!(!config.apply_dns);
    }

    /// [M3R] Ручной режим: --routes/--apply-dns включают прежнее поведение.
    #[test]
    fn manual_flags_enable_full_capture() {
        let arguments = Arguments::try_parse_from([
            "csqtt",
            "--peer",
            "127.0.0.1:9000",
            "--vk",
            "-Wabc",
            "--password",
            "secret",
            "--routes",
            "--apply-dns",
        ])
        .unwrap();
        let config = arguments.to_config();
        assert!(config.install_routes);
        assert!(config.apply_dns);
    }

    /// [M3R] Флаги независимы: только --routes не включает DNS.
    #[test]
    fn routes_flag_alone_leaves_dns_isolated() {
        let arguments = Arguments::try_parse_from([
            "csqtt",
            "--peer",
            "127.0.0.1:9000",
            "--vk",
            "-Wabc",
            "--password",
            "secret",
            "--routes",
        ])
        .unwrap();
        let config = arguments.to_config();
        assert!(config.install_routes);
        assert!(!config.apply_dns);
    }
}
