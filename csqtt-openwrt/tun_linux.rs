// SPDX-FileCopyrightText: 2026 amurcanov
// SPDX-FileCopyrightText: 2026 luminescq
// SPDX-License-Identifier: PolyForm-Noncommercial-1.0.0

//! Нативный Linux TUN: `/dev/net/tun` + ioctl-адреса + netlink-маршруты.
//!
//! [FOCSQ] Сетевая модель зеркалит Windows-версию (tun_win.rs):
//! 1. исходный шлюз запоминается ДО перехвата таблицы;
//! 2. подсети VK/TURN/DNS-роутера уходят через исходный шлюз — иначе
//!    петля (пакеты до них снова попадают в туннель). Пир НЕ
//!    исключается: транспорт всегда TURN, а хостинг на пиру должен
//!    открываться через туннель;
//! 3. перехват — half-маршрутами 0/1 + 128/1: бьют любой дефолт,
//!    но оставляют место для более конкретных исключений.
//!
//! Отличия от Windows: интерфейс живёт ровно столько, сколько открыт fd
//! (стейл-маршруты физически невозможны), DNS — systemd-resolved
//! (resolvectl) с fallback на /etc/resolv.conf. Маршруты — netlink
//! RTM_NEWROUTE, адреса — ioctl SIOCSIF*: никаких дочерних ip-процессов,
//! всё нативно.
//!
//! [OpenWrt-порт, M3R] ПРИНЦИП ИЗОЛЯЦИИ: режим interface-only по умолчанию
//! (RoutingPolicy{install_routes:false, apply_dns:false}) — csqtt0 поднимается
//! (адрес/MTU из TUNCONF), но маршруты и системный DNS НЕ трогаются вовсе.
//! Половинные/exclude-маршруты и set_dns включаются только в ручном режиме
//! (флаги CLI --routes/--apply-dns); служба CSQTT OpenWrt всегда работает
//! interface-only. dnsmasq/uci dhcp не затрагиваются никогда.

use std::{
    collections::HashSet,
    fs::File,
    net::{IpAddr, Ipv4Addr},
    os::fd::{AsRawFd, FromRawFd, OwnedFd},
    sync::{
        Arc, Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};

/// MTU как в оригинале (Constants.Vpn.DEFAULT_MTU)
const MTU: usize = 1300;

const TUNSETIFF: libc::c_ulong = 0x4004_54ca; // _IOW('t', 202, int)
const IFF_TUN: i16 = 0x0001;
const IFF_NO_PI: i16 = 0x1000;

/// [OpenWrt-порт] glibc принимает ioctl-реквест как c_ulong, musl — как
/// c_int. Обёртка приводит вызов к сигнатуре каждой libc, значение
/// реквеста одно и то же (aarch64 little-endian, умещается в int).
#[cfg(target_env = "musl")]
unsafe fn tun_ioctl(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: *mut libc::c_void,
) -> libc::c_int {
    unsafe { libc::ioctl(fd, request as libc::c_int, arg) }
}

#[cfg(not(target_env = "musl"))]
unsafe fn tun_ioctl(
    fd: libc::c_int,
    request: libc::c_ulong,
    arg: *mut libc::c_void,
) -> libc::c_int {
    unsafe { libc::ioctl(fd, request, arg) }
}

/// struct ifreq: ifr_name[16] + union 16 байт
const IFREQ_SIZE: usize = 32;

/// Подсети VK/TURN целиком через исходный шлюз (PWDTT-style): закрывают
/// ротацию CDN без гонок динамического резолва. Совпадает с tun_win.rs.
const VK_EXCLUDE_CIDRS: [&str; 13] = [
    "87.240.128.0/18",
    "87.240.192.0/19",
    "90.156.0.0/16",
    "93.186.224.0/21",
    "95.142.192.0/21",
    "95.163.0.0/16",
    "95.213.0.0/18",
    "155.212.192.0/20",
    "185.16.28.0/22",
    "194.67.64.0/18",
    "195.82.146.0/23",
    "213.180.193.0/24",
    "77.88.0.0/18",
];

// ===========================================================================
// Разбор CIDR
// ===========================================================================

fn parse_cidr(cidr: &str) -> Result<(Ipv4Addr, u8)> {
    let (address, prefix) = cidr.split_once('/').context("CIDR без префикса")?;
    let address: Ipv4Addr = address
        .parse()
        .with_context(|| format!("некорректный IP: {address}"))?;
    let prefix: u8 = prefix
        .parse()
        .with_context(|| format!("некорректный префикс: {prefix}"))?;
    if prefix > 32 {
        bail!("префикс /{prefix} больше 32");
    }
    Ok((address, prefix))
}

fn parse_addr(text: &str) -> Result<Ipv4Addr> {
    text.parse()
        .with_context(|| format!("некорректный IP: {text}"))
}

// ===========================================================================
// [M3R] Политика маршрутизации/DNS: принцип изоляции
// ===========================================================================

/// Политика перехвата трафика: по умолчанию (служба CSQTT OpenWrt) csqtt0 —
/// ТОЛЬКО интерфейс, ни одной записи в таблицу маршрутов и системный DNS
/// не пишется. Ручной режим CLI (--routes/--apply-dns) возвращает прежнее
/// поведение focsq: half-маршруты 0/1+128/1 + exclude VK/TURN + set_dns.
#[derive(Clone, Copy, Debug, Default)]
pub struct RoutingPolicy {
    /// Ставить half-маршруты 0/1+128/1 и exclude-маршруты VK/TURN
    /// (перехват всего трафика системы). false = interface-only.
    pub install_routes: bool,
    /// Применять системный DNS из TUNCONF (resolvectl / resolv.conf).
    /// false = системный DNS не меняется вообще.
    pub apply_dns: bool,
}

impl RoutingPolicy {
    /// Восстановить полное focsq-поведение (ручной десктоп-режим).
    pub const fn full_capture() -> Self {
        Self {
            install_routes: true,
            apply_dns: true,
        }
    }
}

/// Активная политика сессии. Дефолт — interface-only; run_client задаёт
/// из ClientConfig до поднятия интерфейса.
static POLICY: Mutex<RoutingPolicy> = Mutex::new(RoutingPolicy {
    install_routes: false,
    apply_dns: false,
});

/// Установить политику маршрутизации/DNS на сессию (вызывается ядром
/// из run_client по ClientConfig до создания интерфейса).
pub fn set_routing_policy(policy: RoutingPolicy) {
    *POLICY.lock().unwrap() = policy;
}

fn current_policy() -> RoutingPolicy {
    *POLICY.lock().unwrap()
}

/// Политика, применённая последней успешной apply_tunconf (для teardown).
static APPLIED_POLICY: Mutex<Option<RoutingPolicy>> = Mutex::new(None);

// ===========================================================================
// [M3R] Системные операции: слой над ioctl/netlink/resolv
// (единая точка перехвата для юнит-тестов маршрутов без root)
// ===========================================================================

/// Системная команда, которую политика планирует к выполнению.
/// Планировщик — чистая функция от (политика, входные данные), что
/// даёт тестируемость без root и реального /dev/net/tun.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SystemCommand {
    /// half-маршрут перехвата через TUN-шлюз (0/1, 128/1)
    AddHalfRoute {
        destination: String,
        gateway: Ipv4Addr,
        interface: i32,
    },
    /// exclude-маршрут через исходный аплинк (VK-подсети, TURN /32)
    AddUplinkRoute {
        destination: String,
        gateway: Ipv4Addr,
        interface: i32,
    },
    /// снятие стейл peer-/32 от прошлых сессий (идемпотентно)
    DeletePeerHost { destination: String },
    /// применить системный DNS (resolvectl либо /etc/resolv.conf)
    ApplyDns {
        servers: Vec<String>,
        interface: String,
    },
}

/// Подтверждение перехвата в таблице маршрутов (route_exists).
const VERIFY_HALF_ROUTE: &str = "0.0.0.0/1";

/// Планировщик системных команд для apply_tunconf: чистая функция.
/// Возвращает команды ПОРЯДКА выполнения (half → peer-чистка → exclude → DNS)
/// и нужные данные для подтверждения перехвата.
pub fn plan_system_commands(policy: RoutingPolicy, input: &TunconfInput) -> Vec<SystemCommand> {
    let mut plan = Vec::new();
    if !policy.install_routes {
        return plan;
    }
    let tun_gateway = tun_gateway(input.address);
    for destination in ["0.0.0.0/1", "128.0.0.0/1"] {
        plan.push(SystemCommand::AddHalfRoute {
            destination: destination.to_string(),
            gateway: tun_gateway,
            interface: input.tun_index,
        });
    }
    plan.push(SystemCommand::DeletePeerHost {
        destination: format!("{}/32", input.peer),
    });
    for cidr in VK_EXCLUDE_CIDRS {
        plan.push(SystemCommand::AddUplinkRoute {
            destination: (*cidr).to_string(),
            gateway: input.gateway,
            interface: input.uplink_index,
        });
    }
    for address in &input.turn_excludes {
        plan.push(SystemCommand::AddUplinkRoute {
            destination: format!("{address}/32"),
            gateway: input.gateway,
            interface: input.uplink_index,
        });
    }
    if policy.apply_dns {
        plan.push(SystemCommand::ApplyDns {
            servers: input.dns_servers.clone(),
            interface: input.interface_name.clone(),
        });
    }
    plan
}

/// Входные данные apply_tunconf для планировщика (тестируемая модель).
#[derive(Clone, Debug)]
pub struct TunconfInput {
    pub address: Ipv4Addr,
    pub gateway: Ipv4Addr,
    pub uplink_index: i32,
    pub tun_index: i32,
    pub interface_name: String,
    pub peer: Ipv4Addr,
    pub dns_servers: Vec<String>,
    /// TURN-IP, накопленные exclude_host_ip до перехвата
    pub turn_excludes: Vec<Ipv4Addr>,
}

/// Отдельный планировщик команды exclude_host_ip (динамические TURN /32).
pub fn plan_host_exclude(
    policy: RoutingPolicy,
    address: Ipv4Addr,
    gateway: Ipv4Addr,
    uplink_index: i32,
) -> Option<SystemCommand> {
    if !policy.install_routes {
        return None;
    }
    Some(SystemCommand::AddUplinkRoute {
        destination: format!("{address}/32"),
        gateway,
        interface: uplink_index,
    })
}

/// Маска префикса как u32 в «старшем-первый» представлении (255.255.255.0 → 0xFFFFFF00).
fn prefix_mask(prefix: u8) -> u32 {
    if prefix == 0 {
        0
    } else {
        u32::MAX << (32 - prefix as u32)
    }
}

// ===========================================================================
// Netlink: маршруты
// ===========================================================================

/// RTM_*-константы libc: RTM_NEWROUTE=8, NLM_F_*, NETLINK_ROUTE — есть в libc crate.
/// Последовательности netlink должны быть уникальными на сокет.
static NL_SEQUENCE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

#[repr(C)]
struct NlMsgHeader {
    length: u32,
    type_: u16,
    flags: u16,
    sequence: u32,
    port: u32,
}

/// struct rtmsg (linux/route.h).
#[repr(C)]
struct RtMsg {
    family: u8,
    dst_len: u8,
    src_len: u8,
    tos: u8,
    table: u8,
    protocol: u8,
    scope: u8,
    kind: u8,
    flags: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct RtAttr {
    length: u16,
    kind: u16,
}

const RTA_DST: u16 = 1;
const RTA_GATEWAY: u16 = 5;
const RTA_OIF: u16 = 4;

const RT_SCOPE_UNIVERSE: u8 = 0;
const RT_PROTOCOL_STATIC: u8 = 4;
const RTN_UNICAST: u8 = 1;
const RT_TABLE_MAIN: u8 = 254;

const NLMSG_ERROR: u16 = 2;

fn nlmsg_align(length: usize) -> usize {
    (length + 3) & !3
}

fn nl_header(length: u32, kind: u16, flags: u16) -> NlMsgHeader {
    NlMsgHeader {
        length,
        type_: kind,
        flags,
        sequence: NL_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        port: 0,
    }
}

fn push_bytes<T>(buffer: &mut Vec<u8>, value: &T) {
    let bytes =
        unsafe { std::slice::from_raw_parts((value as *const T).cast(), std::mem::size_of::<T>()) };
    buffer.extend_from_slice(bytes);
}

fn push_rta(buffer: &mut Vec<u8>, kind: u16, payload: &[u8]) {
    push_bytes(
        buffer,
        &RtAttr {
            length: (std::mem::size_of::<RtAttr>() + payload.len()) as u16,
            kind,
        },
    );
    buffer.extend_from_slice(payload);
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
}

/// Открыть netlink-сокет routing-таблицы. Приём — с таймаутом SO_RCVTIMEO:
/// если ACK не пришёл за отведённое время, считаем тихий успех (успешный
/// RTM_NEWROUTE с NLM_F_ACK отвечает мгновенно; тишина дольше — аномалия).
fn open_rtnetlink() -> Result<File> {
    let descriptor = unsafe {
        libc::socket(
            libc::AF_NETLINK,
            libc::SOCK_RAW | libc::SOCK_CLOEXEC,
            libc::NETLINK_ROUTE,
        )
    };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error()).context("netlink socket");
    }
    // 2с на приём ответа — как обычный таймаут обращения к ядру.
    let timeout = libc::timeval {
        tv_sec: 2,
        tv_usec: 0,
    };
    if unsafe {
        libc::setsockopt(
            descriptor,
            libc::SOL_SOCKET,
            libc::SO_RCVTIMEO,
            (&timeout as *const libc::timeval).cast(),
            std::mem::size_of::<libc::timeval>() as libc::socklen_t,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error()).context("SO_RCVTIMEO");
    }
    // SAFETY: дескриптор только что создан и ещё ничей
    let file = unsafe { File::from_raw_fd(descriptor) };
    let mut address: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
    address.nl_family = libc::AF_NETLINK as libc::sa_family_t;
    if unsafe {
        libc::bind(
            file.as_raw_fd(),
            (&mut address as *mut libc::sockaddr_nl).cast(),
            std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
        )
    } < 0
    {
        return Err(std::io::Error::last_os_error()).context("netlink bind");
    }
    Ok(file)
}

/// Добавить IPv4-маршрут в главную таблицу. gateway=None — on-link
/// (для TUN-маршрутов через фиктивный шлюз .1, как WireGuard).
///
/// [FOCSQ] ensure=true: сначала удалить существующий маршрут с тем же
/// назначением (ESRCH = не было — не ошибка), затем добавить. Нужно
/// для маршрутов ЧЕРЕЗ аплинк (TURN-исключения, VK-подсети):
/// half-маршруты 0/1+128/1 исчезают вместе с TUN-интерфейсом, а эти —
/// нет и переживают teardown. Без ensure повторный коннект получал
/// EEXIST («Файл существует», живая Manjaro 2026-09-04) — и весь
/// TUNCONF падал, второй коннект не поднимался вовсе.
fn route_add(
    destination: &str,
    gateway: Option<Ipv4Addr>,
    interface: i32,
    ensure: bool,
) -> Result<()> {
    let (network, prefix) = parse_cidr(destination)?;
    if prefix == 0 {
        bail!("маршрут 0/0 не поддерживается — используйте half-маршруты 0/1 + 128/1");
    }
    if ensure {
        route_del(destination);
    }

    let mut buffer: Vec<u8> = Vec::with_capacity(128);
    // NLM_F_ACK обязателен: без него ядро на УСПЕШНЫЙ маршрут ничего не
    // отвечает, и цикл приёма навсегда зависает в recv.
    push_bytes(
        &mut buffer,
        &nl_header(
            0, // дозаполним после сборки
            libc::RTM_NEWROUTE,
            (libc::NLM_F_REQUEST | libc::NLM_F_CREATE | libc::NLM_F_EXCL | libc::NLM_F_ACK) as u16,
        ),
    );
    push_bytes(
        &mut buffer,
        &RtMsg {
            family: libc::AF_INET as u8,
            dst_len: prefix,
            src_len: 0,
            tos: 0,
            table: RT_TABLE_MAIN,
            protocol: RT_PROTOCOL_STATIC,
            scope: if gateway.is_some() {
                RT_SCOPE_UNIVERSE
            } else {
                libc::RT_SCOPE_LINK
            },
            kind: RTN_UNICAST,
            flags: 0,
        },
    );
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }

    push_rta(&mut buffer, RTA_DST, &network.octets());
    if let Some(gateway) = gateway {
        push_rta(&mut buffer, RTA_GATEWAY, &gateway.octets());
    }
    push_rta(&mut buffer, RTA_OIF, &(interface as u32).to_ne_bytes());

    let length = buffer.len() as u32;
    buffer[0..4].copy_from_slice(&length.to_ne_bytes());

    let socket = open_rtnetlink()?;
    send_all(&socket, &buffer)?;
    if let Some(error) = receive_netlink_error(&socket)? {
        bail!("route add {destination}: {error}");
    }
    Ok(())
}

/// Удалить маршрут из главной таблицы по назначению (и интерфейсу, если
/// задан). Отсутствие маршрута (ESRCH) — не ошибка: идемпотентность.
/// Ошибки только логируем: удаление — «лучшее усилие» (запускать
/// туннель нельзя держать на чистке мусора прошлой сессии).
fn route_del(destination: &str) {
    let Ok((network, prefix)) = parse_cidr(destination) else {
        return;
    };
    if prefix == 0 {
        return;
    }
    let mut buffer: Vec<u8> = Vec::with_capacity(128);
    push_bytes(
        &mut buffer,
        &nl_header(
            0,
            libc::RTM_DELROUTE,
            (libc::NLM_F_REQUEST | libc::NLM_F_ACK) as u16,
        ),
    );
    push_bytes(
        &mut buffer,
        &RtMsg {
            family: libc::AF_INET as u8,
            dst_len: prefix,
            src_len: 0,
            tos: 0,
            table: RT_TABLE_MAIN,
            protocol: RT_PROTOCOL_STATIC,
            scope: libc::RT_SCOPE_NOWHERE,
            kind: RTN_UNICAST,
            flags: 0,
        },
    );
    while !buffer.len().is_multiple_of(4) {
        buffer.push(0);
    }
    push_rta(&mut buffer, RTA_DST, &network.octets());
    let length = buffer.len() as u32;
    buffer[0..4].copy_from_slice(&length.to_ne_bytes());

    let Ok(socket) = open_rtnetlink() else {
        return;
    };
    if send_all(&socket, &buffer).is_err() {
        return;
    }
    // Ответ не ждём всерьёз: даже ESRCH/ENOENT — норма (маршрута не было).
    let _ = receive_netlink_error(&socket);
}

/// Отправить буфер целиком (send может уйти частями).
fn send_all(socket: &File, buffer: &[u8]) -> Result<()> {
    let mut sent = 0;
    while sent < buffer.len() {
        let written = unsafe {
            libc::send(
                socket.as_raw_fd(),
                buffer[sent..].as_ptr().cast(),
                buffer.len() - sent,
                libc::MSG_NOSIGNAL,
            )
        };
        if written <= 0 {
            return Err(std::io::Error::last_os_error()).context("netlink send");
        }
        sent += written as usize;
    }
    Ok(())
}

/// Дождаться ответа: Ok(None) — ACK/тишина (успех), Ok(Some(text)) — ошибка ядра.
fn receive_netlink_error(socket: &File) -> Result<Option<String>> {
    let mut buffer = vec![0u8; 8192];
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if Instant::now() > deadline {
            return Ok(None);
        }
        let received = unsafe {
            libc::recv(
                socket.as_raw_fd(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                0,
            )
        };
        if received < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() == std::io::ErrorKind::WouldBlock {
                continue;
            }
            return Err(error).context("netlink recv");
        }
        let length = received as usize;
        let mut offset = 0;
        while offset + std::mem::size_of::<NlMsgHeader>() <= length {
            // SAFETY: буфер выровнен по u32 (выделён Vec<u8> с выравниванием >4,
            // заголовок netlink требует 4-байтового выравнивания — соблюдено)
            let header: NlMsgHeader =
                unsafe { std::ptr::read_unaligned(buffer.as_ptr().add(offset).cast()) };
            if header.type_ == NLMSG_ERROR
                && header.length as usize >= std::mem::size_of::<NlMsgHeader>() + 4
                && offset + std::mem::size_of::<NlMsgHeader>() + 4 <= length
            {
                // struct nlmsgerr: nlmsghdr(16) + error(4)
                let code = unsafe {
                    std::ptr::read_unaligned(
                        buffer
                            .as_ptr()
                            .add(offset + std::mem::size_of::<NlMsgHeader>())
                            .cast::<i32>(),
                    )
                };
                if code == 0 {
                    return Ok(None); // чистый ACK
                }
                return Ok(Some(format!(
                    "ошибка ядра: {}",
                    std::io::Error::from_raw_os_error(-code)
                )));
            }
            offset += nlmsg_align(header.length as usize);
        }
    }
}

// ===========================================================================
// Таблица маршрутов: /proc/net/route
// ===========================================================================

/// Строка таблицы маршрутов из /proc/net/route.
struct RouteRow {
    interface: String,
    /// Сеть в сетевом порядке (u32 «старший-первый»: 10.66.67.0 → 0x0A424300)
    destination: u32,
    mask: u32,
    gateway: u32,
    /// RTF_UP=1, RTF_GATEWAY=2
    flags: u32,
}

/// Разбор /proc/net/route. Колонки: Iface Destination Gateway Flags
/// RefCnt Use Metric Mask. Числа — hex little-endian представления u32,
/// где байты IP лежат в сетевом порядке: 192.168.8.1 → «0108A8C0».
/// Переводим к «старшему-первому» через swap_bytes.
fn parse_route_rows() -> Result<Vec<RouteRow>> {
    let table = std::fs::read_to_string("/proc/net/route").context("/proc/net/route")?;
    let mut rows = Vec::new();
    for line in table.lines().skip(1) {
        let mut fields = line.split_whitespace();
        let (
            Some(interface),
            Some(destination),
            Some(gateway),
            Some(flags),
            Some(_refcnt),
            Some(_use),
            Some(_metric),
            Some(mask),
        ) = (
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
            fields.next(),
        )
        else {
            continue;
        };
        let (Ok(destination), Ok(gateway), Ok(flags), Ok(mask)) = (
            u32::from_str_radix(destination, 16),
            u32::from_str_radix(gateway, 16),
            u32::from_str_radix(flags, 16),
            u32::from_str_radix(mask, 16),
        ) else {
            continue;
        };
        rows.push(RouteRow {
            interface: interface.to_owned(),
            destination: destination.swap_bytes(),
            mask: mask.swap_bytes(),
            gateway: gateway.swap_bytes(),
            flags,
        });
    }
    Ok(rows)
}

/// Исходный default-маршрут: сеть 0.0.0.0/0 через шлюз (RTF_GATEWAY).
fn find_default_gateway() -> Result<(Ipv4Addr, i32)> {
    for row in parse_route_rows()? {
        if row.destination == 0 && row.mask == 0 && row.flags & 2 != 0
        // RTF_GATEWAY
        {
            return Ok((
                Ipv4Addr::from(row.gateway),
                interface_index(&row.interface)?,
            ));
        }
    }
    bail!("default-маршрут не найден — нет сети?")
}

/// Подтверждение перехвата (аналог verify_half_routes в tun_win.rs):
/// маршрут стоит на ожидаемом интерфейсе с ожидаемым шлюзом.
fn route_exists(destination: &str, gateway: Ipv4Addr, interface: &str) -> Result<bool> {
    let (network, prefix) = parse_cidr(destination)?;
    let wanted_mask = prefix_mask(prefix);
    for row in parse_route_rows()? {
        if row.destination == u32::from(network)
            && row.mask == wanted_mask
            && row.interface == interface
            && Ipv4Addr::from(row.gateway) == gateway
        {
            return Ok(true);
        }
    }
    Ok(false)
}

// ===========================================================================
// Устройство
// ===========================================================================

/// Устройство TUN сессии. Интерфейс живёт, пока открыт дескриптор: drop
/// устройства = исчезновение интерфейса и всех его маршрутов из системы.
pub struct TunDevice {
    file: File,
    name: String,
}

/// Активное устройство сессии (для teardown).
static DEVICE: Mutex<Option<Arc<TunDevice>>> = Mutex::new(None);
/// Исходный шлюз и индекс интерфейса (до перехвата) — для exclude-маршрутов.
static GATEWAY_ROUTE: Mutex<Option<(Ipv4Addr, i32)>> = Mutex::new(None);
/// TURN-IP, исключённые динамически: дедуп от всех воркеров [FOCSQ]
static DYNAMIC_EXCLUDES: OnceLock<Mutex<HashSet<Ipv4Addr>>> = OnceLock::new();

/// Маршруты, добавленные нами ЧЕРЕЗ аплинк (TURN-исключения,
/// VK-подсети). В отличие от half-маршрутов 0/1+128/1, уходящих вместе
/// с TUN-интерфейсом, эти переживают teardown — убираем сами.
static UPLINK_ROUTES: OnceLock<Mutex<Vec<String>>> = OnceLock::new();

fn uplink_routes() -> &'static Mutex<Vec<String>> {
    UPLINK_ROUTES.get_or_init(|| Mutex::new(Vec::new()))
}

fn dynamic_excludes() -> &'static Mutex<HashSet<Ipv4Addr>> {
    DYNAMIC_EXCLUDES.get_or_init(|| Mutex::new(HashSet::new()))
}

impl TunDevice {
    /// Открыть /dev/net/tun и создать интерфейс (имя ≤ 15 символов, IFNAMSIZ).
    pub fn open(name: &str) -> Result<Arc<Self>> {
        if name.len() >= 16 {
            bail!("имя TUN-интерфейса {name:?} длиннее 15 символов (IFNAMSIZ)");
        }
        let descriptor = unsafe {
            libc::open(
                c"/dev/net/tun".as_ptr().cast(),
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if descriptor < 0 {
            return Err(std::io::Error::last_os_error())
                .context("не удалось открыть /dev/net/tun (нужен root или setcap cap_net_admin)");
        }
        // SAFETY: дескриптор только что создан и ещё ничей
        let file = unsafe { File::from_raw_fd(descriptor) };

        // TUNSETIFF: ifreq { ifr_name[16], ifr_flags }
        let mut ifreq = [0u8; IFREQ_SIZE];
        ifreq[..name.len()].copy_from_slice(name.as_bytes());
        ifreq[16..18].copy_from_slice(&((IFF_TUN | IFF_NO_PI) as u16).to_ne_bytes());
        if unsafe { tun_ioctl(file.as_raw_fd(), TUNSETIFF, ifreq.as_mut_ptr().cast()) } < 0 {
            return Err(std::io::Error::last_os_error())
                .context("TUNSETIFF не удался — нет CAP_NET_ADMIN?");
        }
        let name_end = ifreq[..16].iter().position(|byte| *byte == 0).unwrap_or(16);
        let actual_name = String::from_utf8_lossy(&ifreq[..name_end]).into_owned();

        let device = Arc::new(Self {
            file,
            name: actual_name,
        });
        *DEVICE.lock().unwrap() = Some(device.clone());
        crate::log_error!("[КЛИЕНТ] TUN-интерфейс создан: {}", device.name);
        Ok(device)
    }

    /// Имя интерфейса в системе.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Дублированный дескриптор для poll-циклов (AsyncFd). Владение
    /// устройством остаётся у TunDevice: закрытие дубля цикл не роняет.
    pub fn poll_fd(&self) -> Result<OwnedFd> {
        let copy = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: дескриптор только что создан дублированием и ещё ничей
        Ok(unsafe { OwnedFd::from_raw_fd(copy) })
    }

    /// [FOCSQ] Дубликат fd как File в неблокирующем режиме — формат,
    /// который ждёт read_tun/write_tun диспетчера v2.1.9: там TUN приходит
    /// готовым File. O_NONBLOCK ставится на общее описание открытого
    /// файла (дубликаты делят его) — блокирующих читателей исходного fd
    /// нет, так что эффект безопасен.
    pub fn nonblocking_file(&self) -> Result<File> {
        use std::os::fd::{AsRawFd, FromRawFd};
        let copy = unsafe { libc::fcntl(self.file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) };
        if copy < 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        let status = unsafe { libc::fcntl(copy, libc::F_GETFL) };
        if status < 0 || unsafe { libc::fcntl(copy, libc::F_SETFL, status | libc::O_NONBLOCK) } < 0
        {
            return Err(std::io::Error::last_os_error().into());
        }
        // SAFETY: дублированный дескриптор, владение передаётся File
        Ok(unsafe { File::from_raw_fd(copy) })
    }
}

// ===========================================================================
// ioctl-хелперы
// ===========================================================================

fn ioctl_socket() -> Result<i32> {
    let descriptor =
        unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if descriptor < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(descriptor)
}

/// Индекс интерфейса по имени (SIOCGIFINDEX).
fn interface_index(name: &str) -> Result<i32> {
    if name.len() >= 16 {
        bail!("имя интерфейса {name:?} длиннее 15 символов");
    }
    let descriptor = ioctl_socket()?;
    let mut ifreq = [0u8; IFREQ_SIZE];
    ifreq[..name.len()].copy_from_slice(name.as_bytes());
    let result = unsafe {
        tun_ioctl(
            descriptor,
            libc::SIOCGIFINDEX as libc::c_ulong,
            ifreq.as_mut_ptr().cast(),
        )
    };
    let error = std::io::Error::last_os_error();
    unsafe { libc::close(descriptor) };
    if result < 0 {
        return Err(error).context("SIOCGIFINDEX");
    }
    // ifr_ifindex лежит в начале union (offset 16)
    let index = unsafe { std::ptr::read_unaligned(ifreq[16..].as_ptr().cast::<i32>()) };
    Ok(index)
}

/// sockaddr_in байтами: family(2) + port(2) + addr(4) + pad(8).
/// sin_addr кладётся байтами IP как есть — порядок памяти (урок из
/// S_addr-бага, fixes-2026-08-30.md §1: from_ne_bytes, НЕ from_be).
fn sockaddr_in_bytes(address: Ipv4Addr) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..2].copy_from_slice(&(libc::AF_INET as u16).to_ne_bytes());
    bytes[4..8].copy_from_slice(&address.octets());
    bytes
}

/// Адрес, маска и MTU интерфейса через SIOCSIF*.
fn set_address(name: &str, address: Ipv4Addr, mtu: usize) -> Result<()> {
    let descriptor = ioctl_socket()?;
    let result = (|| -> Result<()> {
        let mut ifreq_addr = [0u8; IFREQ_SIZE];
        ifreq_addr[..name.len()].copy_from_slice(name.as_bytes());
        let sockaddr = sockaddr_in_bytes(address);
        ifreq_addr[16..32].copy_from_slice(&sockaddr);

        let mut ifreq_mask = [0u8; IFREQ_SIZE];
        ifreq_mask[..name.len()].copy_from_slice(name.as_bytes());
        let mask = sockaddr_in_bytes(Ipv4Addr::from(prefix_mask(24)));
        ifreq_mask[16..32].copy_from_slice(&mask);

        let mut ifreq_mtu = [0u8; IFREQ_SIZE];
        ifreq_mtu[..name.len()].copy_from_slice(name.as_bytes());
        let mtu = mtu as i32;
        ifreq_mtu[16..20].copy_from_slice(&mtu.to_ne_bytes());

        for (request, ifreq, what) in [
            (libc::SIOCSIFADDR, &ifreq_addr, "адрес"),
            (libc::SIOCSIFNETMASK, &ifreq_mask, "маска"),
            (libc::SIOCSIFMTU, &ifreq_mtu, "MTU"),
        ] {
            if unsafe {
                tun_ioctl(
                    descriptor,
                    request as libc::c_ulong,
                    ifreq.as_ptr() as *mut libc::c_void,
                )
            } < 0
            {
                bail!("SIOCSIF-{what}: {}", std::io::Error::last_os_error());
            }
        }

        // [FOCSQ] Поднять интерфейс. SIOCSIFADDR этого НЕ делает (в
        // отличие от Windows, где Wintun-адаптер становится Up сам),
        // а маршруты через лежащий интерфейс ядро отклоняет
        // ENETUNREACH: «route add 0.0.0.0/1: ошибка ядра: Network is
        // unreachable», живая Manjaro 2026-09-03.
        let mut ifreq_flags = [0u8; IFREQ_SIZE];
        ifreq_flags[..name.len()].copy_from_slice(name.as_bytes());
        if unsafe {
            tun_ioctl(
                descriptor,
                libc::SIOCGIFFLAGS as libc::c_ulong,
                ifreq_flags.as_mut_ptr().cast(),
            )
        } < 0
        {
            bail!("SIOCGIFFLAGS: {}", std::io::Error::last_os_error());
        }
        // ifr_flags — начало union (offset 16)
        let mut flags =
            unsafe { std::ptr::read_unaligned(ifreq_flags[16..].as_ptr().cast::<libc::c_short>()) };
        flags |= libc::IFF_UP as libc::c_short | libc::IFF_RUNNING as libc::c_short;
        ifreq_flags[16..18].copy_from_slice(&flags.to_ne_bytes());
        if unsafe {
            tun_ioctl(
                descriptor,
                libc::SIOCSIFFLAGS as libc::c_ulong,
                ifreq_flags.as_mut_ptr().cast(),
            )
        } < 0
        {
            bail!("SIOCSIFFLAGS: {}", std::io::Error::last_os_error());
        }
        Ok(())
    })();
    unsafe { libc::close(descriptor) };
    result
}

// ===========================================================================
// TUNCONF
// ===========================================================================

/// Шлюз TUN — .1 от адреса (модель WireGuard: точка-в-точку через .1).
fn tun_gateway(ip: Ipv4Addr) -> Ipv4Addr {
    let octets = ip.octets();
    Ipv4Addr::new(octets[0], octets[1], octets[2], 1)
}

/// Применить TUNCONF. Модель — tun_win.rs::apply_tunconf.
/// [M3R] ПРИНЦИП ИЗОЛЯЦИИ: адрес/маска/MTU и подъём интерфейса — всегда;
/// half-маршруты + exclude и системный DNS — только по RoutingPolicy
/// (по умолчанию OFF: csqtt0 — только интерфейс для прокси пользователя).
pub async fn apply_tunconf(ip: &str, dns: &str, peer_ip: &str) -> Result<()> {
    let address = parse_addr(ip)?;
    let device = DEVICE
        .lock()
        .unwrap()
        .clone()
        .context("TUN-устройство не создано — адрес выставить некому")?;
    let peer = parse_addr(peer_ip)?;

    let dns_servers: Vec<String> = dns
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .collect();
    if dns_servers.is_empty() {
        bail!("TUNCONF не содержит DNS");
    }

    let policy = current_policy();

    // 1. Исходный шлюз — ДО перехвата таблицы. В interface-only не нужен
    //    для exclude, но источник ошибок не создаём: резолвим только когда
    //    действительно будет ставить маршруты через аплинк.
    let (gateway, uplink_index) = if policy.install_routes {
        find_default_gateway()?
    } else {
        (Ipv4Addr::UNSPECIFIED, -1)
    };
    let tun_index = interface_index(&device.name)?;

    // 2. Адрес/маска/MTU на TUN-интерфейсе — ВСЕГДА (это и есть интерфейс)
    set_address(&device.name, address, MTU)?;

    if !policy.install_routes {
        // [M3R] Interface-only: точка остановки. Ни одной записи в таблицу
        // маршрутов, системный DNS не меняется. DNS из TUNCONF живёт
        // внутри туннеля (его знают воркеры), роутер остаётся при своём.
        *GATEWAY_ROUTE.lock().unwrap() = None;
        *APPLIED_POLICY.lock().unwrap() = Some(policy);
        crate::log_error!(
            "[TUN] Настроен: ip={address}/24 mtu={MTU} | interface-only: маршруты и DNS не тронуты"
        );
        return Ok(());
    }

    *GATEWAY_ROUTE.lock().unwrap() = Some((gateway, uplink_index));
    let tun_gateway = tun_gateway(address);

    // 3. Перехват. Пир НЕ исключаем: транспорт всегда TURN (клиент на
    //    IP пира напрямую не стучится — только ChannelBind через релей),
    //    а исключение выкидывало весь хостинг на пиру на прямой путь
    //    провайдера. Через туннель трафик до собственного IP сервер
    //    доставляет себе локально (loopback).
    //    Exclude — через аплинк, ensure=true: half-маршруты умирают
    //    вместе с TUN-интерфейсом, а эти переживают teardown прошлой
    //    сессии — чистим перед добавлением.
    route_add("0.0.0.0/1", Some(tun_gateway), tun_index, false)?;
    route_add("128.0.0.0/1", Some(tun_gateway), tun_index, false)?;
    if !route_exists(VERIFY_HALF_ROUTE, tun_gateway, &device.name)? {
        bail!("half-маршрут 0.0.0.0/1 не подтверждён в таблице маршрутов");
    }
    // [FOCSQ] Стейл peer-/32 от прошлой сессии (когда пир исключался):
    // в uplink_routes() он уже не попадает, а пережил бы teardown и мешал
    // бы хостингу на пиру ходить через туннель. route_del идемпотентен.
    route_del(&format!("{peer}/32"));

    // 4. Подсети VK/TURN через исходный шлюз (PWDTT-style)
    for cidr in VK_EXCLUDE_CIDRS {
        match route_add(cidr, Some(gateway), uplink_index, true) {
            Ok(()) => uplink_routes().lock().unwrap().push(cidr.to_string()),
            Err(error) => {
                crate::log_error!("[TUN] Exclude-подсеть VK {cidr} не добавлена: {error:#}");
            }
        }
    }
    apply_deferred_excludes();

    // 5. DNS системы — только при apply_dns (принцип изоляции: по
    //    умолчанию системный DNS роутера/LAN не меняется)
    if policy.apply_dns {
        set_dns(&dns_servers, &device)?;
    }

    *APPLIED_POLICY.lock().unwrap() = Some(policy);
    crate::log_error!(
        "[TUN] Настроен: ip={address}/24 gw={tun_gateway} dns={dns} | исходный шлюз {gateway} (iface {uplink_index}), пир {peer} — через туннель"
    );
    Ok(())
}

/// [FOCSQ] Динамический exclude-маршрут для TURN-хостов (мимо туннеля):
/// и STUN :19302, и relay-порты на том же IP. До apply_tunconf IP
/// запоминается и исключается сразу после перехвата.
/// [M3R] В interface-only не выполняет НИ ОДНОЙ команды: IP просто не
/// попадает в отложенный набор (план пуст), шлюз остаётся None.
pub fn exclude_host_ip(ip: IpAddr) {
    let IpAddr::V4(address) = ip else { return };
    let policy = current_policy();
    if !policy.install_routes {
        // Тихо игнорируем: маршрутов не будет, дедуп-набор не растёт.
        return;
    }
    if !dynamic_excludes().lock().unwrap().insert(address) {
        return;
    }
    let Some((gateway, uplink_index)) = *GATEWAY_ROUTE.lock().unwrap() else {
        crate::log_error!("[TUN] TURN {address}: перехвата ещё нет, исключим при TUNCONF");
        return;
    };
    if plan_host_exclude(policy, address, gateway, uplink_index).is_none() {
        return;
    }
    match route_add(&format!("{address}/32"), Some(gateway), uplink_index, true) {
        Ok(()) => {
            uplink_routes()
                .lock()
                .unwrap()
                .push(format!("{address}/32"));
            crate::log_error!("[TUN] Exclude-маршрут TURN {address} добавлен");
        }
        Err(error) => crate::log_error!("[TUN] Exclude-маршрут TURN {address}: {error:#}"),
    }
}

/// Отложенные TURN-исключения (IP, увиденные до перехвата).
/// Вызывается только из ветки install_routes=true — в interface-only
/// набор динамики остаётся пустым, сюда не доходим.
fn apply_deferred_excludes() {
    let pending: Vec<Ipv4Addr> = dynamic_excludes().lock().unwrap().iter().copied().collect();
    let Some((gateway, uplink_index)) = *GATEWAY_ROUTE.lock().unwrap() else {
        return;
    };
    for address in pending {
        match route_add(&format!("{address}/32"), Some(gateway), uplink_index, true) {
            Ok(()) => {
                uplink_routes()
                    .lock()
                    .unwrap()
                    .push(format!("{address}/32"));
                crate::log_error!("[TUN] Отложенный Exclude-маршрут TURN {address} добавлен")
            }
            Err(error) => crate::log_error!("[TUN] Отложенный exclude TURN {address}: {error:#}"),
        }
    }
}

/// DNS системы — на туннельные серверы. systemd-resolved (resolvectl) на
/// большинстве современных систем, прямой resolv.conf — как fallback.
fn set_dns(servers: &[String], device: &TunDevice) -> Result<()> {
    let list = servers.join(" ");
    if std::path::Path::new("/run/systemd/resolve").exists() {
        let status = std::process::Command::new("resolvectl")
            .arg("dns")
            .arg(&device.name)
            .args(servers.iter().map(String::as_str))
            .status();
        match status {
            Ok(status) if status.success() => {
                crate::log_error!("[TUN] DNS направлен в {list} (systemd-resolved)");
                return Ok(());
            }
            Ok(status) => bail!("resolvectl dns: код выхода {status}"),
            Err(error) => bail!("resolvectl недоступен: {error}"),
        }
    }
    let content = servers
        .iter()
        .map(|server| format!("nameserver {server}\n"))
        .collect::<String>();
    std::fs::write("/etc/resolv.conf", content).context("/etc/resolv.conf")?;
    crate::log_error!("[TUN] DNS направлен в {list} (/etc/resolv.conf)");
    Ok(())
}

/// Teardown: закрыть устройство — интерфейс и его маршруты исчезают
/// ядром автоматически. Системный DNS systemd-resolved вернёт сам
/// (наш интерфейс исчез из его списка).
pub fn teardown() {
    *GATEWAY_ROUTE.lock().unwrap() = None;
    dynamic_excludes().lock().unwrap().clear();
    // [FOCSQ] Аплинк-маршруты (TURN/VK-исключения) не исчезают
    // вместе с TUN-интерфейсом — снимаем сами, иначе они копятся между
    // сессиями, а повторный коннект падал на EEXIST.
    let routes: Vec<String> = uplink_routes().lock().unwrap().drain(..).collect();
    if !routes.is_empty() {
        for route in &routes {
            route_del(route);
        }
        crate::log_error!("[TUN] Снято аплинк-маршрутов: {}", routes.len());
    }
    if DEVICE.lock().unwrap().take().is_some() {
        crate::log_error!("[TUN] Интерфейс опущен (быстрое закрытие)");
    }
}

/// Мгновенное опускание интерфейса при закрытии — закрыть fd.
pub fn drop_interface_now() {
    teardown();
}

// ===========================================================================
// [M3R] Мок-TUN для тестов без root
// ===========================================================================

/// Эмуляция TunDevice::open/nonblocking_file без /dev/net/tun: пара
/// DGRAM-сокетов (socketpair). «Сетевая» сторона — инжекция/чтение
/// пакетов, «диспетчерская» — File в неблокирующем режиме, в точности
/// как выдаёт настоящий TunDevice::nonblocking_file (тот же формат,
/// который читают read_tun/write_tun через AsyncFd<StdUnixDatagram>).
pub struct MockTun {
    /// Сторона диспетчера: пакеты, записанные сюда, читает mock.read_packet()
    dispatcher_file: File,
    /// Сторона «ядра»: пакеты, записанные сюда, читает read_tun
    kernel_socket: std::os::unix::net::UnixDatagram,
    /// Счётчики обмена в обе стороны
    pub sent_to_tunnel: Arc<AtomicU64>,
    pub received_from_tunnel: Arc<AtomicU64>,
    /// Счётчик записанных диспетчером байтов (write_tun → туннель)
    pub tunnel_bytes_written: Arc<AtomicU64>,
    /// Счётчик прочитанных диспетчером байтов (туннель → read_tun)
    pub tunnel_bytes_read: Arc<AtomicU64>,
}

impl MockTun {
    /// Создать пару сокетов с буфером, вмещающим тестовый burst.
    pub fn new() -> Result<Self> {
        let buffer: libc::c_int = 1 << 20; // 1 MiB: burst 32×1300 без блокировки
        unsafe {
            let mut pair = [-1 as libc::c_int, -1 as libc::c_int];
            let pair_result = libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_DGRAM | libc::SOCK_CLOEXEC,
                0,
                pair.as_mut_ptr(),
            );
            if pair_result < 0 {
                return Err(std::io::Error::last_os_error()).context("socketpair");
            }
            let [first, second] = pair;
            // SAFETY: second только что создан socketpair-ом, ещё ничей
            let kernel = std::os::unix::net::UnixDatagram::from_raw_fd(second);
            let status = libc::fcntl(first, libc::F_GETFL);
            if status < 0
                || libc::fcntl(first, libc::F_SETFL, status | libc::O_NONBLOCK) < 0
                || libc::setsockopt(
                    first,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&buffer as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                ) < 0
                || libc::setsockopt(
                    first,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&buffer as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                ) < 0
                || libc::setsockopt(
                    second,
                    libc::SOL_SOCKET,
                    libc::SO_SNDBUF,
                    (&buffer as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                ) < 0
                || libc::setsockopt(
                    second,
                    libc::SOL_SOCKET,
                    libc::SO_RCVBUF,
                    (&buffer as *const libc::c_int).cast(),
                    std::mem::size_of::<libc::c_int>() as libc::socklen_t,
                ) < 0
            {
                let error = std::io::Error::last_os_error();
                libc::close(first);
                let _ = kernel;
                return Err(error).context("mock tun fcntl/setsockopt");
            }
            // SAFETY: first только что создан, fcntl успешен, владеем File
            let dispatcher_file = File::from_raw_fd(first);
            // Неблокирующее чтение: пустая очередь → WouldBlock, не висит
            kernel.set_nonblocking(true)?;
            Ok(Self {
                dispatcher_file,
                kernel_socket: kernel,
                sent_to_tunnel: Arc::new(AtomicU64::new(0)),
                received_from_tunnel: Arc::new(AtomicU64::new(0)),
                tunnel_bytes_written: Arc::new(AtomicU64::new(0)),
                tunnel_bytes_read: Arc::new(AtomicU64::new(0)),
            })
        }
    }

    /// File для диспетчера (как TunDevice::nonblocking_file).
    pub fn dispatcher_file(&self) -> File {
        unsafe {
            let copy = libc::fcntl(self.dispatcher_file.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0);
            if copy < 0 {
                panic!("mock tun dup: {}", std::io::Error::last_os_error());
            }
            File::from_raw_fd(copy)
        }
    }

    /// [Ядро→туннель] Инжекция пакета, как будто пришёл из сети в csqtt0:
    /// read_tun диспетчера его прочитает.
    pub fn inject_inbound(&self, packet: &[u8]) -> std::io::Result<usize> {
        self.kernel_socket.send(packet)?;
        self.sent_to_tunnel.fetch_add(1, Ordering::Relaxed);
        Ok(packet.len())
    }

    /// [Туннель→ядро] Прочитать пакет, записанный write_tun (в «сеть»).
    /// DGRAM-сокеты сохраняют границы пакетов.
    pub fn read_outbound(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        let length = self.kernel_socket.recv(buffer)?;
        if length > 0 {
            self.received_from_tunnel.fetch_add(1, Ordering::Relaxed);
        }
        Ok(length)
    }

    /// [Ядро→туннель, тесты] Прочитать пакет с диспетчерской стороны —
    /// то, что read_tun возьмёт после inject_inbound. Без root.
    #[cfg(test)]
    pub fn read_dispatcher_side(&self, buffer: &mut [u8]) -> std::io::Result<usize> {
        use std::io::Read;
        let mut file = self.dispatcher_file();
        file.read(buffer)
    }
}

#[cfg(test)]
mod mock_tun_tests {
    use super::*;

    /// Round-trip мока без root — обе стороны пары.
    /// uplink: inject_inbound кладёт в очередь диспетчерского fd — там её
    /// читает read_tun; здесь проверяем read_dispatcher_side (dequeuing
    /// с диспетчерской стороны). downlink: write диспетчера читается
    /// read_outbound со стороны «ядра». Границы пакетов сохраняются.
    #[test]
    fn mock_tun_round_trip_preserves_packet_boundaries() {
        let mock = MockTun::new().unwrap();

        // downlink-направление: диспетчер (write_tun) → «ядро»
        use std::io::Write;
        let mut dispatcher = mock.dispatcher_file();
        let first = [0x45u8, 1, 2, 3, 4, 5];
        let second = [0x06u8, 9, 8, 7];
        assert_eq!(dispatcher.write(&first).unwrap(), first.len());
        assert_eq!(dispatcher.write(&second).unwrap(), second.len());
        let mut buffer = [0u8; 64];
        assert_eq!(mock.read_outbound(&mut buffer).unwrap(), first.len());
        assert_eq!(&buffer[..first.len()], &first);
        assert_eq!(mock.read_outbound(&mut buffer).unwrap(), second.len());
        assert_eq!(&buffer[..second.len()], &second);
        assert_eq!(mock.received_from_tunnel.load(Ordering::Relaxed), 2);

        // uplink-направление: «ядро» (inject_inbound) → диспетчер (read_tun)
        let inbound = [0x45u8, 9, 9, 9];
        assert_eq!(mock.inject_inbound(&inbound).unwrap(), inbound.len());
        assert_eq!(
            mock.read_dispatcher_side(&mut buffer).unwrap(),
            inbound.len()
        );
        assert_eq!(&buffer[..inbound.len()], &inbound);
        assert_eq!(mock.sent_to_tunnel.load(Ordering::Relaxed), 1);
    }

    /// Счётчики байтов живут как AtomicU64 и растут независимо.
    #[test]
    fn mock_tun_byte_counters_advance_independently() {
        let mock = MockTun::new().unwrap();
        mock.tunnel_bytes_written.fetch_add(1300, Ordering::Relaxed);
        mock.tunnel_bytes_read.fetch_add(700, Ordering::Relaxed);
        mock.tunnel_bytes_read.fetch_add(600, Ordering::Relaxed);
        assert_eq!(mock.tunnel_bytes_written.load(Ordering::Relaxed), 1300);
        assert_eq!(mock.tunnel_bytes_read.load(Ordering::Relaxed), 1300);
    }

    /// DGRAM-пара: write диспетчера читается ядром ровно одним пактом.
    #[test]
    fn mock_tun_dispatcher_writes_reach_kernel_side() {
        let mock = MockTun::new().unwrap();
        use std::io::Write;
        let mut file = mock.dispatcher_file();
        let payload = [0x99u8; 32];
        assert_eq!(file.write(&payload).unwrap(), 32);
        let mut buffer = [0u8; 64];
        assert_eq!(mock.read_outbound(&mut buffer).unwrap(), 32);
        assert_eq!(buffer[..32], payload);
        mock.tunnel_bytes_written.fetch_add(32, Ordering::Relaxed);
        assert_eq!(mock.tunnel_bytes_written.load(Ordering::Relaxed), 32);
    }
}

#[cfg(all(test, unix))]
mod policy_tests {
    use super::*;

    fn input() -> TunconfInput {
        TunconfInput {
            address: Ipv4Addr::new(10, 66, 67, 7),
            gateway: Ipv4Addr::new(192, 168, 8, 1),
            uplink_index: 3,
            tun_index: 8,
            interface_name: "csqtt0".to_string(),
            peer: Ipv4Addr::new(198, 51, 100, 10),
            dns_servers: vec!["77.88.8.8".to_string(), "77.88.8.1".to_string()],
            turn_excludes: vec![Ipv4Addr::new(198, 51, 100, 11)],
        }
    }

    /// ПРИНЦИП ИЗОЛЯЦИИ: interface-only — мок системы не получает НИ ОДНОЙ
    /// команды (ни half, ни exclude, ни DNS).
    #[test]
    fn interface_only_policy_plans_zero_system_commands() {
        let plan = plan_system_commands(RoutingPolicy::default(), &input());
        assert!(plan.is_empty());
        let mixed = RoutingPolicy {
            install_routes: false,
            apply_dns: true,
        };
        assert!(plan_system_commands(mixed, &input()).is_empty());
        assert!(plan_host_exclude(mixed, Ipv4Addr::new(1, 2, 3, 4), input().gateway, 3).is_none());
    }

    /// Ручной режим: прежний набор focsq — half ×2, peer-чистка, VK ×13,
    /// отложенные TURN, DNS. Порядок стабильный.
    #[test]
    fn full_capture_policy_plans_half_peer_vk_and_dns() {
        let plan = plan_system_commands(RoutingPolicy::full_capture(), &input());
        assert_eq!(plan.len(), 2 + 1 + VK_EXCLUDE_CIDRS.len() + 1 + 1);
        assert_eq!(
            plan[0],
            SystemCommand::AddHalfRoute {
                destination: "0.0.0.0/1".to_string(),
                gateway: Ipv4Addr::new(10, 66, 67, 1),
                interface: 8
            }
        );
        assert_eq!(
            plan[1],
            SystemCommand::AddHalfRoute {
                destination: "128.0.0.0/1".to_string(),
                gateway: Ipv4Addr::new(10, 66, 67, 1),
                interface: 8
            }
        );
        assert_eq!(
            plan[2],
            SystemCommand::DeletePeerHost {
                destination: "198.51.100.10/32".to_string()
            }
        );
        assert_eq!(
            plan[3],
            SystemCommand::AddUplinkRoute {
                destination: "87.240.128.0/18".to_string(),
                gateway: Ipv4Addr::new(192, 168, 8, 1),
                interface: 3
            }
        );
        let uplink_count = plan
            .iter()
            .filter(|command| matches!(command, SystemCommand::AddUplinkRoute { .. }))
            .count();
        assert_eq!(uplink_count, VK_EXCLUDE_CIDRS.len() + 1);
        assert!(plan
            .iter()
            .any(|command| matches!(command, SystemCommand::AddUplinkRoute { destination, .. } if destination == "198.51.100.11/32")));
        assert_eq!(
            plan.last(),
            Some(&SystemCommand::ApplyDns {
                servers: vec!["77.88.8.8".to_string(), "77.88.8.1".to_string()],
                interface: "csqtt0".to_string()
            })
        );
    }

    /// routes=true, dns=false: DNS-команды нет, маршруты есть.
    #[test]
    fn routes_without_dns_omits_dns_command() {
        let policy = RoutingPolicy {
            install_routes: true,
            apply_dns: false,
        };
        let plan = plan_system_commands(policy, &input());
        assert!(!plan.is_empty());
        assert!(
            plan.iter()
                .all(|command| !matches!(command, SystemCommand::ApplyDns { .. }))
        );
    }

    /// Динамический TURN-exclude планирует /32 через аплинк только при
    /// install_routes=true.
    #[test]
    fn host_exclude_plans_u32_route_only_with_routes_enabled() {
        let address = Ipv4Addr::new(203, 0, 113, 5);
        let command =
            plan_host_exclude(RoutingPolicy::full_capture(), address, input().gateway, 3).unwrap();
        assert_eq!(
            command,
            SystemCommand::AddUplinkRoute {
                destination: "203.0.113.5/32".to_string(),
                gateway: Ipv4Addr::new(192, 168, 8, 1),
                interface: 3
            }
        );
        assert!(plan_host_exclude(RoutingPolicy::default(), address, input().gateway, 3).is_none());
    }

    /// exclude_host_ip в interface-only не пополняет дедуп-набор динамики
    /// (план бы остался пустым — проверяем состояние через API мока политики).
    #[test]
    fn exclude_host_ip_interface_only_touches_no_state() {
        set_routing_policy(RoutingPolicy::default());
        exclude_host_ip(IpAddr::V4(Ipv4Addr::new(1, 2, 3, 4)));
        assert!(dynamic_excludes().lock().unwrap().is_empty());
        set_routing_policy(RoutingPolicy::full_capture());
        exclude_host_ip(IpAddr::V4(Ipv4Addr::new(5, 6, 7, 8)));
        assert!(
            dynamic_excludes()
                .lock()
                .unwrap()
                .contains(&Ipv4Addr::new(5, 6, 7, 8))
        );
        dynamic_excludes().lock().unwrap().clear();
        set_routing_policy(RoutingPolicy::default());
    }

    /// Дефолт ClientConfig-политики: оба флага false (проверка константы
    /// дефолта, чтобы принцип изоляции не «уехал» через Default).
    #[test]
    fn default_policy_is_isolated() {
        let policy = RoutingPolicy::default();
        assert!(!policy.install_routes);
        assert!(!policy.apply_dns);
    }
}

#[cfg(all(test, unix))]
mod pump_tests {
    use super::*;
    use crate::dispatcher::Dispatcher;
    use crate::packet::PacketPool;
    use crate::stats::Stats;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    /// Кадр «CQF1» + TCP-пакет: гейт frame_outbound_packet в read_tun
    /// требует распознаваемый TCP-флоу, иначе пакеты молча отбрасываются.
    fn tun_packet(source_port: u16, sequence: u32) -> Vec<u8> {
        let mut packet = [0u8; 1_200];
        packet[0] = 0x45;
        packet[2..4].copy_from_slice(&(1_200u16).to_be_bytes());
        packet[8] = 64;
        packet[9] = 6;
        packet[12..16].copy_from_slice(&[10, 66, 67, 2]);
        packet[16..20].copy_from_slice(&[1, 1, 1, 1]);
        packet[20..22].copy_from_slice(&source_port.to_be_bytes());
        packet[22..24].copy_from_slice(&443u16.to_be_bytes());
        packet[24..28].copy_from_slice(&sequence.to_be_bytes());
        packet[32] = 5 << 4;
        packet[33] = 0x18;
        packet.to_vec()
    }

    /// Pump-цикл: инжекция в мок → read_tun кадрирует и раскидывает по
    /// воркерам; return-канал → write_tun пишет в мок; чтение со стороны
    /// «ядра» возвращает кадры CQF1 с теми же данными.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn mock_tun_pump_moves_packets_both_directions_and_stops() {
        let mock = MockTun::new().unwrap();
        let dispatcher_file = mock.dispatcher_file();
        let cancel = CancellationToken::new();
        let pool = PacketPool::new(1_024);
        let stats = Arc::new(Stats::default());
        let dispatcher = Dispatcher::start_test_tun(
            dispatcher_file,
            pool.clone(),
            stats.clone(),
            cancel.clone(),
        )
        .await;

        // Зарегистрировать воркера, чтобы dispatch() нашёл адресата;
        // bulk-приёмник остаётся снаружи для чтения uplink-пакетов.
        use crate::dispatcher::{WorkerChannels, packet_channel};
        let (latency, _latency_rx) = packet_channel(64, true);
        let (priority, _priority_rx) = packet_channel(64, true);
        let (bulk, worker_bulk) = packet_channel(64, true);
        dispatcher.register(WorkerChannels {
            id: 1,
            incarnation_id: 1,
            turn_path: Arc::from("mock"),
            latency,
            priority,
            bulk,
        });

        // uplink: инжекция → read_tun → воркер. Пакеты кадрируются (CQF1+seq).
        let uplink_packets: Vec<Vec<u8>> = (0..12)
            .map(|sequence| tun_packet(50_000, sequence))
            .collect();
        for packet in &uplink_packets {
            mock.inject_inbound(packet).unwrap();
        }
        let mut received_uplink = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while received_uplink < uplink_packets.len() {
            assert!(std::time::Instant::now() < deadline, "uplink pump stalled");
            match tokio::time::timeout(
                std::time::Duration::from_millis(200),
                worker_bulk.recv(&cancel),
            )
            .await
            {
                Ok(Some(packet)) => {
                    // кадр CQF1 + исходный IP-пакет без потерь
                    assert!(packet.len() > crate::flow_frame::FRAME_LEN);
                    assert_eq!(packet.as_slice()[..4], *b"CQF1");
                    assert_eq!(packet.as_slice()[crate::flow_frame::FRAME_LEN], 0x45);
                    received_uplink += 1;
                }
                Ok(None) => panic!("worker channel closed"),
                Err(_) => continue,
            }
        }
        assert_eq!(
            stats.total_bytes_up.load(Ordering::Relaxed),
            (uplink_packets.len() * 1_200) as i64
        );

        // downlink: return-канал → write_tun → мок. Raw-пакеты (без CQF1)
        // ReturnReorder пропускает как есть — write_tun пишет их в
        // интерфейс без кадра, граница сохраняется DGRAM-парой.
        for sequence in 0..12u32 {
            let mut packet = pool.acquire();
            packet.set_read_len(1_200).unwrap();
            packet
                .as_mut_slice()
                .copy_from_slice(&tun_packet(50_000, sequence));
            dispatcher.return_packet(packet);
        }
        let mut buffer = vec![0u8; 4_096];
        let mut received_downlink = 0;
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while received_downlink < 12 {
            assert!(
                std::time::Instant::now() < deadline,
                "downlink pump stalled"
            );
            match mock.read_outbound(&mut buffer) {
                Ok(0) => continue,
                Ok(length) => {
                    assert_eq!(length, 1_200);
                    assert_eq!(buffer[0], 0x45);
                    received_downlink += 1;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => continue,
                Err(error) => panic!("mock read failed: {error}"),
            }
        }
        assert_eq!(received_downlink, 12);
        assert_eq!(stats.total_bytes_down.load(Ordering::Relaxed), 12 * 1_200);

        // Остановка: shutdown завершает pump-задачи, пул возвращён целиком.
        dispatcher.shutdown().await;
        assert_eq!(pool.available(), pool.capacity());
    }
}
