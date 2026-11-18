//! Ağ yığını: Ethernet, ARP, IPv4, ICMP, UDP, TCP, DHCP istemcisi ve DNS çözücü.
//!
//! Tüm protokol işleme `netd` thread'inde yapılır; NIC kesmesi ya da zamanlayıcı
//! onu uyandırır. Diğer thread'ler (kabuk, syscall'lar) yığını kilitleyip paket
//! gönderir ve yanıtları bekleme kuyruğunda bekler.

pub mod dhcp;
pub mod dns;
pub mod ntp;
pub mod tcp;
pub mod wire;

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::{pit, tsc};
use alloc::boxed::Box;

use crate::drivers::nic::{self, Nic};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::sched::{self, Mutex, WaitQueue};
use wire::{Ipv4, Mac};

const ARP_TIMEOUT_MS: u64 = 5 * 60 * 1000;
const ARP_RETRY_MS: u64 = 1000;
const ARP_GIVE_UP_MS: u64 = 3000;
const PENDING_LIMIT: usize = 64;
const UDP_QUEUE_LIMIT: usize = 32;

#[derive(Clone, Debug, Default)]
pub struct Config {
    pub address: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Vec<Ipv4>,
    pub domain: String,
    pub server: Ipv4,
    pub lease_seconds: u32,
    pub acquired_ms: u64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Stats {
    pub rx_packets: u64,
    pub tx_packets: u64,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub rx_dropped: u64,
    pub tx_dropped: u64,
    pub arp_requests: u64,
    pub link_changes: u64,
}

struct ArpEntry {
    mac: [u8; 6],
    updated_ms: u64,
}

struct Pending {
    next_hop: Ipv4,
    packet: Vec<u8>,
    since_ms: u64,
    last_request_ms: u64,
}

pub struct Datagram {
    pub source: Ipv4,
    pub source_port: u16,
    pub data: Vec<u8>,
}

struct Stack {
    nic: Box<dyn Nic>,
    link_up: bool,
    speed: u32,
    full_duplex: bool,
    config: Option<Config>,
    dhcp: dhcp::Client,
    arp: BTreeMap<Ipv4, ArpEntry>,
    pending: Vec<Pending>,
    ip_id: u16,
    udp: BTreeMap<u16, VecDeque<Datagram>>,
    echo_replies: BTreeMap<(u16, u16), u64>,
    next_port: u16,
    tcp: tcp::Tcp,
    stats: Stats,
}

static STACK: Mutex<Option<Stack>> = Mutex::new(None);
static WAKE: WaitQueue = WaitQueue::new();
static PENDING_WORK: AtomicBool = AtomicBool::new(false);
static REPLIES: WaitQueue = WaitQueue::new();
static REPLY_GENERATION: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn notify_replies() {
    REPLY_GENERATION.fetch_add(1, Ordering::Release);
    REPLIES.wake_all();
}

/// `condition` (yığın kilitliyken) doğru olana ya da süre dolana kadar bekler.
fn wait_for(condition: impl Fn(&mut Stack) -> bool, timeout_ms: u64) -> bool {
    let deadline = now_ms() + timeout_ms;
    loop {
        let generation = REPLY_GENERATION.load(Ordering::Acquire);
        if with_stack(&condition).unwrap_or(false) {
            return true;
        }
        let now = now_ms();
        if now >= deadline {
            return false;
        }
        REPLIES.wait_timeout(
            || REPLY_GENERATION.load(Ordering::Acquire) != generation,
            deadline - now,
        );
    }
}
static PRESENT: AtomicBool = AtomicBool::new(false);
static STARTED: AtomicBool = AtomicBool::new(false);

/// Ağ yığınının başlatma adımı geçti (sonradan gelen kartlar `attach` ile bağlanır).
pub fn started() -> bool {
    STARTED.load(Ordering::Acquire)
}

pub fn wake() {
    PENDING_WORK.store(true, Ordering::Release);
    WAKE.wake_all();
}

pub fn now_ms() -> u64 {
    pit::uptime_ms()
}

/// Rastgele 32 bit (RDRAND varsa, yoksa TSC karışımı).
pub fn random_u32() -> u32 {
    if crate::arch::cpu::has_feature(1, crate::arch::cpu::CpuidRegister::Ecx, 30) {
        let mut value: u32 = 0;
        let mut ok: u8 = 0;
        for _ in 0..10 {
            unsafe {
                core::arch::asm!("rdrand {0:e}", "setc {1}", out(reg) value, out(reg_byte) ok, options(nomem, nostack));
            }
            if ok != 0 {
                return value;
            }
        }
    }
    let t = tsc::read();
    (t ^ (t >> 29) ^ (t << 11)) as u32
}

impl Stack {
    fn transmit(&mut self, frame: Vec<u8>) -> bool {
        if !self.link_up {
            self.stats.tx_dropped += 1;
            return false;
        }
        let ok = self.nic.send(&frame);
        if ok {
            self.stats.tx_packets += 1;
            self.stats.tx_bytes += frame.len() as u64;
        } else {
            self.stats.tx_dropped += 1;
        }
        ok
    }

    fn our_address(&self) -> Ipv4 {
        self.config
            .as_ref()
            .map(|c| c.address)
            .unwrap_or(Ipv4::UNSPECIFIED)
    }

    fn send_arp_request(&mut self, target: Ipv4, sender: Ipv4) {
        self.stats.arp_requests += 1;
        let payload = wire::arp(1, self.nic.mac(), sender, [0; 6], target);
        let frame = wire::ethernet(
            wire::BROADCAST_MAC,
            self.nic.mac(),
            wire::ETHERTYPE_ARP,
            &payload,
        );
        self.transmit(frame);
    }

    fn next_hop(&self, destination: Ipv4) -> Option<Ipv4> {
        let config = self.config.as_ref()?;
        if destination.same_subnet(config.address, config.netmask) {
            Some(destination)
        } else if !config.gateway.is_unspecified() {
            Some(config.gateway)
        } else {
            None
        }
    }

    /// IPv4 paketi gönderir; MAC bilinmiyorsa ARP sorar ve paketi bekletir.
    fn send_ip(&mut self, destination: Ipv4, protocol: u8, payload: &[u8]) -> bool {
        let source = self.our_address();
        self.ip_id = self.ip_id.wrapping_add(1);
        let packet = wire::ipv4(source, destination, protocol, self.ip_id, payload);
        if destination == Ipv4::BROADCAST {
            let frame = wire::ethernet(
                wire::BROADCAST_MAC,
                self.nic.mac(),
                wire::ETHERTYPE_IPV4,
                &packet,
            );
            return self.transmit(frame);
        }
        let Some(hop) = self.next_hop(destination) else {
            self.stats.tx_dropped += 1;
            return false;
        };
        if let Some(entry) = self.arp.get(&hop) {
            let frame = wire::ethernet(entry.mac, self.nic.mac(), wire::ETHERTYPE_IPV4, &packet);
            return self.transmit(frame);
        }
        if self.pending.len() >= PENDING_LIMIT {
            self.stats.tx_dropped += 1;
            return false;
        }
        let now = now_ms();
        let already_asking = self.pending.iter().any(|p| p.next_hop == hop);
        self.pending.push(Pending {
            next_hop: hop,
            packet,
            since_ms: now,
            last_request_ms: now,
        });
        if !already_asking {
            self.send_arp_request(hop, source);
        }
        true
    }

    fn flush_pending(&mut self, ip: Ipv4, mac: [u8; 6]) {
        let mut ready = Vec::new();
        self.pending.retain_mut(|p| {
            if p.next_hop == ip {
                ready.push(core::mem::take(&mut p.packet));
                false
            } else {
                true
            }
        });
        for packet in ready {
            let frame = wire::ethernet(mac, self.nic.mac(), wire::ETHERTYPE_IPV4, &packet);
            self.transmit(frame);
        }
    }

    fn handle_arp(&mut self, payload: &[u8]) {
        let Some(arp) = wire::parse_arp(payload) else {
            return;
        };
        let now = now_ms();
        self.dhcp
            .observe_arp(arp.sender_ip, arp.sender_mac, self.nic.mac());
        if !arp.sender_ip.is_unspecified() {
            let known = self.arp.contains_key(&arp.sender_ip);
            let ours = arp.target_ip == self.our_address() && !self.our_address().is_unspecified();
            if known || ours {
                self.arp.insert(
                    arp.sender_ip,
                    ArpEntry {
                        mac: arp.sender_mac,
                        updated_ms: now,
                    },
                );
                self.flush_pending(arp.sender_ip, arp.sender_mac);
            }
        }
        if arp.operation == 1
            && arp.target_ip == self.our_address()
            && !self.our_address().is_unspecified()
        {
            let reply = wire::arp(
                2,
                self.nic.mac(),
                self.our_address(),
                arp.sender_mac,
                arp.sender_ip,
            );
            let frame = wire::ethernet(arp.sender_mac, self.nic.mac(), wire::ETHERTYPE_ARP, &reply);
            self.transmit(frame);
        }
    }

    fn handle_ipv4(&mut self, payload: &[u8]) {
        let Some(packet) = wire::parse_ipv4(payload) else {
            self.stats.rx_dropped += 1;
            return;
        };
        let ours = self.our_address();
        let for_us = packet.destination == ours
            || packet.destination == Ipv4::BROADCAST
            || (ours.is_unspecified() && packet.protocol == wire::PROTO_UDP)
            || self.config.as_ref().is_some_and(|c| {
                packet.destination.to_u32() == (c.address.to_u32() | !c.netmask.to_u32())
            });
        if !for_us {
            return;
        }
        match packet.protocol {
            wire::PROTO_ICMP => self.handle_icmp(packet.source, packet.payload),
            wire::PROTO_TCP if packet.destination == ours => {
                if let Some((header, data)) =
                    wire::parse_tcp(packet.source, packet.destination, packet.payload)
                {
                    self.tcp.input(now_ms(), packet.source, &header, data);
                    self.flush_tcp();
                }
            }
            wire::PROTO_UDP => {
                if let Some(udp) = wire::parse_udp(packet.payload) {
                    if udp.destination_port == dhcp::CLIENT_PORT {
                        let now = now_ms();
                        let change = self.dhcp.receive(udp.payload, now, self.nic.mac());
                        self.apply_dhcp(change);
                    } else if let Some(queue) = self.udp.get_mut(&udp.destination_port)
                        && queue.len() < UDP_QUEUE_LIMIT
                    {
                        queue.push_back(Datagram {
                            source: packet.source,
                            source_port: udp.source_port,
                            data: udp.payload.to_vec(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    fn flush_tcp(&mut self) {
        let source = self.our_address();
        for out in core::mem::take(&mut self.tcp.output) {
            let segment = wire::tcp(source, out.destination, &out.header, &out.payload);
            self.send_ip(out.destination, wire::PROTO_TCP, &segment);
        }
    }

    fn handle_icmp(&mut self, source: Ipv4, payload: &[u8]) {
        if payload.len() < 8 || wire::checksum(&[payload]) != 0 {
            return;
        }
        match payload[0] {
            8 => {
                let id = wire::be16(payload, 4).unwrap_or(0);
                let seq = wire::be16(payload, 6).unwrap_or(0);
                let reply = wire::icmp_echo(0, id, seq, &payload[8..]);
                self.send_ip(source, wire::PROTO_ICMP, &reply);
            }
            0 => {
                let id = wire::be16(payload, 4).unwrap_or(0);
                let seq = wire::be16(payload, 6).unwrap_or(0);
                self.echo_replies.insert((id, seq), tsc::now_ns());
            }
            _ => {}
        }
    }

    fn handle_frame(&mut self, frame: &[u8]) {
        let Some(eth) = wire::parse_ethernet(frame) else {
            self.stats.rx_dropped += 1;
            return;
        };
        if eth.destination != self.nic.mac() && eth.destination != wire::BROADCAST_MAC {
            return;
        }
        self.stats.rx_packets += 1;
        self.stats.rx_bytes += frame.len() as u64;
        match eth.ethertype {
            wire::ETHERTYPE_ARP => self.handle_arp(eth.payload),
            wire::ETHERTYPE_IPV4 => self.handle_ipv4(eth.payload),
            _ => {}
        }
    }

    fn apply_dhcp(&mut self, change: dhcp::Change) {
        match change {
            dhcp::Change::None => {}
            dhcp::Change::Probe(address) => {
                let probe = wire::arp(1, self.nic.mac(), Ipv4::UNSPECIFIED, [0; 6], address);
                let frame = wire::ethernet(
                    wire::BROADCAST_MAC,
                    self.nic.mac(),
                    wire::ETHERTYPE_ARP,
                    &probe,
                );
                self.transmit(frame);
            }
            dhcp::Change::Send(frame) => {
                self.transmit(frame);
                if self.config.is_none()
                    && let Some(line) = self.nic.diagnostics()
                {
                    crate::log!("{}", line);
                }
            }
            dhcp::Change::SendUnicast(server, payload) => {
                let udp = wire::udp(
                    self.our_address(),
                    server,
                    dhcp::CLIENT_PORT,
                    dhcp::SERVER_PORT,
                    &payload,
                );
                self.send_ip(server, wire::PROTO_UDP, &udp);
            }
            dhcp::Change::Bound(config) => {
                let renewal = self
                    .config
                    .as_ref()
                    .is_some_and(|c| c.address == config.address);
                crate::log!(
                    "net: {} {}/{} gateway {} dns {:?} lease {} s",
                    if renewal { "renewed" } else { "configured" },
                    config.address,
                    config.netmask.prefix_len(),
                    config.gateway,
                    config.dns,
                    config.lease_seconds
                );
                if !renewal {
                    self.arp.clear();
                }
                let announce = wire::arp(1, self.nic.mac(), config.address, [0; 6], config.address);
                self.config = Some(config);
                let frame = wire::ethernet(
                    wire::BROADCAST_MAC,
                    self.nic.mac(),
                    wire::ETHERTYPE_ARP,
                    &announce,
                );
                self.transmit(frame);
                notify_replies();
            }
            dhcp::Change::Lost(reason) => {
                if let Some(old) = self.config.take() {
                    crate::log!("net: address {} released ({})", old.address, reason);
                }
                self.arp.clear();
                self.pending.clear();
            }
        }
    }

    fn check_link(&mut self, now: u64) {
        let (up, speed, full_duplex) = self.nic.link();
        if up == self.link_up {
            return;
        }
        self.link_up = up;
        self.speed = speed;
        self.full_duplex = full_duplex;
        self.stats.link_changes += 1;
        if up {
            crate::log!(
                "net: link up, {} Mbit/s {} duplex",
                speed,
                if full_duplex { "full" } else { "half" }
            );
            let change = self.dhcp.link_up(now);
            self.apply_dhcp(change);
        } else {
            crate::log!("net: link down");
            self.pending.clear();
            let change = self.dhcp.link_down();
            self.apply_dhcp(change);
        }
    }

    fn timers(&mut self, now: u64) {
        let mac = self.nic.mac();
        loop {
            let change = self.dhcp.poll(now, mac);
            if matches!(change, dhcp::Change::None) {
                break;
            }
            self.apply_dhcp(change);
        }
        self.arp
            .retain(|_, e| now.saturating_sub(e.updated_ms) < ARP_TIMEOUT_MS);
        let before = self.pending.len();
        self.pending
            .retain(|p| now.saturating_sub(p.since_ms) < ARP_GIVE_UP_MS);
        self.stats.tx_dropped += (before - self.pending.len()) as u64;
        let source = self.our_address();
        let mut retry = Vec::new();
        for p in self.pending.iter_mut() {
            if now.saturating_sub(p.last_request_ms) >= ARP_RETRY_MS && !retry.contains(&p.next_hop)
            {
                p.last_request_ms = now;
                retry.push(p.next_hop);
            }
        }
        for hop in retry {
            self.send_arp_request(hop, source);
        }
        if self.tcp.poll(now) {
            notify_replies();
        }
        self.flush_tcp();
    }
}

/// Trafik sürerken kart bu sıklıkla yoklanır: kesmesi gelmeyen kartlarda (MSI/INTx
/// yönlendirilemediyse) da ACK'ler gecikmez.
const BUSY_POLL_MS: u64 = 1;
const IDLE_POLL_MS: u64 = 20;
const BUSY_WINDOW_MS: u64 = 250;

fn netd() -> i64 {
    let mut last_traffic = 0u64;
    loop {
        PENDING_WORK.store(false, Ordering::Release);
        let received = {
            let mut guard = STACK.lock();
            let Some(stack) = guard.as_mut() else {
                return 0;
            };
            let now = now_ms();
            let (link_changed, _) = stack.nic.take_cause();
            if link_changed || now % 250 < 25 || !stack.link_up {
                stack.check_link(now);
            }
            let mut received = 0;
            while received < 64 {
                let Some(frame) = stack.nic.receive() else {
                    break;
                };
                stack.handle_frame(&frame);
                received += 1;
            }
            stack.timers(now);
            received
        };
        if received > 0 {
            last_traffic = now_ms();
            notify_replies();
        }
        // Bütçe dolduysa halkada hâlâ çerçeve olabilir: beklemeden devam et.
        if received == 64 {
            sched::yield_now();
            continue;
        }
        let poll = if now_ms().saturating_sub(last_traffic) < BUSY_WINDOW_MS {
            BUSY_POLL_MS
        } else {
            IDLE_POLL_MS
        };
        WAKE.wait_timeout(|| PENDING_WORK.load(Ordering::Acquire), poll);
    }
}

fn with_stack<R>(f: impl FnOnce(&mut Stack) -> R) -> Option<R> {
    STACK.lock().as_mut().map(f)
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn config() -> Option<Config> {
    with_stack(|s| s.config.clone()).flatten()
}

pub fn status_text() -> String {
    if !present() {
        return String::from("offline");
    }
    match with_stack(|s| {
        (
            s.link_up,
            s.config.as_ref().map(|c| c.address),
            s.dhcp.state_name(),
        )
    }) {
        Some((false, _, _)) => String::from("eth: link down"),
        Some((true, Some(address), _)) => format!("eth: {}", address),
        Some((true, None, state)) => format!("eth: {}", state),
        None => String::from("offline"),
    }
}

pub struct Report {
    pub model: &'static str,
    pub mac: [u8; 6],
    pub link_up: bool,
    pub speed: u32,
    pub full_duplex: bool,
    pub config: Option<Config>,
    pub dhcp_state: &'static str,
    pub stats: Stats,
    pub rx_errors: u64,
    pub crc_errors: u32,
    pub missed: u32,
    pub arp: Vec<(Ipv4, [u8; 6], u64)>,
}

pub fn report() -> Option<Report> {
    let now = now_ms();
    with_stack(|s| {
        let (crc, missed) = s.nic.hardware_errors();
        Report {
            model: s.nic.model(),
            mac: s.nic.mac(),
            link_up: s.link_up,
            speed: s.speed,
            full_duplex: s.full_duplex,
            config: s.config.clone(),
            dhcp_state: s.dhcp.state_name(),
            stats: s.stats,
            rx_errors: s.nic.rx_errors(),
            crc_errors: crc,
            missed,
            arp: s
                .arp
                .iter()
                .map(|(ip, e)| (*ip, e.mac, (now - e.updated_ms) / 1000))
                .collect(),
        }
    })
}

/// Yapılandırma (DHCP) tamamlanana kadar en fazla `timeout_ms` bekler.
pub fn wait_configured(timeout_ms: u64) -> bool {
    wait_for(|s| s.config.is_some(), timeout_ms)
}

/// ICMP echo gönderir; yanıt gelirse gidiş-dönüş süresini (µs) döndürür.
pub fn ping(destination: Ipv4, sequence: u16, timeout_ms: u64) -> Result<u64, &'static str> {
    let id = 0xba5e;
    let data: Vec<u8> = (0..32u8).collect();
    let sent = tsc::now_ns();
    let ok = with_stack(|s| {
        s.echo_replies.remove(&(id, sequence));
        let packet = wire::icmp_echo(8, id, sequence, &data);
        s.send_ip(destination, wire::PROTO_ICMP, &packet)
    })
    .ok_or("no network interface")?;
    if !ok {
        return Err("cannot send (no route or link down)");
    }
    let got = wait_for(|s| s.echo_replies.contains_key(&(id, sequence)), timeout_ms);
    if !got {
        return Err("timeout");
    }
    let at = with_stack(|s| s.echo_replies.remove(&(id, sequence)))
        .flatten()
        .unwrap_or(sent);
    Ok(at.saturating_sub(sent) / 1000)
}

pub fn udp_bind() -> Option<u16> {
    with_stack(|s| {
        for _ in 0..16384 {
            s.next_port = if s.next_port < 49152 {
                49152
            } else {
                s.next_port.wrapping_add(1).max(49152)
            };
            if !s.udp.contains_key(&s.next_port) {
                s.udp.insert(s.next_port, VecDeque::new());
                return Some(s.next_port);
            }
        }
        None
    })
    .flatten()
}

pub fn udp_unbind(port: u16) {
    with_stack(|s| s.udp.remove(&port));
}

pub fn udp_send(destination: Ipv4, source_port: u16, destination_port: u16, data: &[u8]) -> bool {
    with_stack(|s| {
        let packet = wire::udp(
            s.our_address(),
            destination,
            source_port,
            destination_port,
            data,
        );
        s.send_ip(destination, wire::PROTO_UDP, &packet)
    })
    .unwrap_or(false)
}

pub fn udp_receive(port: u16, timeout_ms: u64) -> Option<Datagram> {
    let take = || with_stack(|s| s.udp.get_mut(&port).and_then(|q| q.pop_front())).flatten();
    if let Some(d) = take() {
        return Some(d);
    }
    wait_for(
        |s| s.udp.get(&port).is_some_and(|q| !q.is_empty()),
        timeout_ms,
    );
    take()
}

pub fn dhcp_renew() -> bool {
    with_stack(|s| {
        let change = s.dhcp.force_renew(now_ms());
        s.apply_dhcp(change);
    })
    .is_some()
}

pub fn dhcp_release() -> bool {
    with_stack(|s| {
        let mac = s.nic.mac();
        if let Some(payload) = s.dhcp.release(mac) {
            let server = s
                .config
                .as_ref()
                .map(|c| c.server)
                .unwrap_or(Ipv4::BROADCAST);
            let udp = wire::udp(
                s.our_address(),
                server,
                dhcp::CLIENT_PORT,
                dhcp::SERVER_PORT,
                &payload,
            );
            s.send_ip(server, wire::PROTO_UDP, &udp);
        }
        s.apply_dhcp(dhcp::Change::Lost("released by user"));
    })
    .is_some()
}

/// Bağlantı kurulana, reddedilene ya da süre dolana kadar bekler.
pub fn tcp_connect(
    destination: Ipv4,
    port: u16,
    timeout_ms: u64,
) -> Result<tcp::SocketId, tcp::Error> {
    let id = with_stack(|s| {
        if s.config.is_none() {
            return Err(tcp::Error::Closed);
        }
        let id = s.tcp.connect(now_ms(), destination, port, random_u32())?;
        s.flush_tcp();
        Ok(id)
    })
    .ok_or(tcp::Error::NoSocket)??;
    wait_for(|s| s.tcp.state(id) != Some(tcp::State::SynSent), timeout_ms);
    let result = with_stack(|s| match (s.tcp.state(id), s.tcp.error(id)) {
        (_, Some(e)) => Err(e),
        (Some(tcp::State::SynSent), _) => Err(tcp::Error::TimedOut),
        (Some(_), None) => Ok(id),
        (None, None) => Err(tcp::Error::Closed),
    })
    .unwrap_or(Err(tcp::Error::NoSocket));
    if result.is_err() {
        tcp_abort(id);
    }
    result
}

pub fn tcp_listen(port: u16) -> Result<tcp::SocketId, tcp::Error> {
    with_stack(|s| s.tcp.listen(port)).unwrap_or(Err(tcp::Error::NoSocket))
}

pub fn tcp_accept(listener: tcp::SocketId, timeout_ms: u64) -> Result<tcp::SocketId, tcp::Error> {
    wait_for(|s| s.tcp.has_pending(listener), timeout_ms);
    with_stack(|s| s.tcp.accept(listener)).unwrap_or(Err(tcp::Error::NoSocket))
}

/// Tüm veri tampona girene kadar (gerekirse bekleyerek) gönderir.
pub fn tcp_send(id: tcp::SocketId, data: &[u8]) -> Result<usize, tcp::Error> {
    let mut done = 0;
    while done < data.len() {
        let r = with_stack(|s| {
            let r = s.tcp.send(now_ms(), id, &data[done..]);
            s.flush_tcp();
            r
        })
        .unwrap_or(Err(tcp::Error::NoSocket));
        match r {
            Ok(n) => done += n,
            Err(tcp::Error::WouldBlock) => {
                wait_for(|s| s.tcp.can_send(id), 1000);
            }
            Err(_) if done > 0 => break,
            Err(e) => return Err(e),
        }
    }
    Ok(done)
}

/// Veri gelene kadar bekler; bağlantı karşıdan kapandıysa boş vektör döner.
pub fn tcp_receive(id: tcp::SocketId, max: usize, timeout_ms: u64) -> Result<Vec<u8>, tcp::Error> {
    let deadline = now_ms() + timeout_ms;
    loop {
        let r = with_stack(|s| {
            let r = s.tcp.receive(now_ms(), id, max);
            s.flush_tcp();
            r
        })
        .unwrap_or(Err(tcp::Error::NoSocket));
        match r {
            Ok(tcp::Received::Data(d)) => return Ok(d),
            Ok(tcp::Received::End) => return Ok(Vec::new()),
            Err(tcp::Error::WouldBlock) => {
                let now = now_ms();
                if now >= deadline {
                    return Err(tcp::Error::WouldBlock);
                }
                wait_for(|s| s.tcp.readable(id), (deadline - now).min(1000));
            }
            Err(e) => return Err(e),
        }
    }
}

pub fn tcp_close(id: tcp::SocketId) {
    with_stack(|s| {
        s.tcp.close(now_ms(), id);
        s.flush_tcp();
    });
}

pub fn tcp_abort(id: tcp::SocketId) {
    with_stack(|s| {
        s.tcp.abort(id);
        s.flush_tcp();
    });
}

/// Kullanıcı işlemlerine verilen soket tanıtıcısı; son referans düşünce kapanır.
pub struct TcpHandle(pub tcp::SocketId);

impl Drop for TcpHandle {
    fn drop(&mut self) {
        tcp_close(self.0);
    }
}

pub fn tcp_sockets() -> Vec<tcp::SocketInfo> {
    with_stack(|s| s.tcp.sockets()).unwrap_or_default()
}

fn self_test() -> bool {
    let header = wire::ipv4(
        Ipv4([10, 0, 0, 1]),
        Ipv4([10, 0, 0, 2]),
        wire::PROTO_UDP,
        1,
        &[1, 2, 3, 4],
    );
    let parsed = wire::parse_ipv4(&header);
    let checksum_ok = wire::checksum(&[&[
        0x45, 0x00, 0x00, 0x73, 0x00, 0x00, 0x40, 0x00, 0x40, 0x11, 0xb8, 0x61, 0xc0, 0xa8, 0x00,
        0x01, 0xc0, 0xa8, 0x00, 0xc7,
    ]]) == 0;
    parsed.is_some_and(|p| p.payload == [1, 2, 3, 4] && p.destination == Ipv4([10, 0, 0, 2]))
        && checksum_ok
        && Ipv4::parse("192.168.1.20") == Some(Ipv4([192, 168, 1, 20]))
        && wire::checksum(&[&[0x12, 0x34, 0x56], &[], &[0x78, 0x9a], &[0xbc]])
            == wire::checksum(&[&[0x12, 0x34, 0x56, 0x78, 0x9a, 0xbc]])
        && wire::checksum(&[&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]]) == !0x241e
        && dhcp::self_test()
        && dns::self_test()
        && tcp::self_test()
}

/// Ağ kartını yığına bağlar (açılışta PCI kartı, sonradan USB Ethernet). Zaten bir kart
/// varsa ikincisi kullanılmaz.
pub fn attach(nic: Box<dyn Nic>) -> bool {
    let mac = nic.mac();
    let model = nic.model();
    let irq = nic.irq();
    {
        let mut guard = STACK.lock();
        if let Some(stack) = guard.as_mut() {
            // Çıkarılmış bir USB kartın yerine yenisi takıldı: kartı değiştir, adresi yeniden al.
            let (up, _, _) = stack.nic.link();
            if !up && stack.nic.model().starts_with("USB") {
                stack.nic = nic;
                stack.link_up = false;
                stack.config = None;
                stack.dhcp = dhcp::Client::new(&mac);
                stack.arp.clear();
                crate::log!(
                    "net: {} MAC {} replaces the unplugged card",
                    model,
                    Mac(&mac)
                );
                drop(guard);
                wake();
                return true;
            }
            crate::log!("net: {} ignored, a network card is already in use", model);
            return false;
        }
        *guard = Some(Stack {
            nic,
            link_up: false,
            speed: 0,
            full_duplex: false,
            config: None,
            dhcp: dhcp::Client::new(&mac),
            arp: BTreeMap::new(),
            pending: Vec::new(),
            ip_id: random_u32() as u16,
            udp: BTreeMap::new(),
            echo_replies: BTreeMap::new(),
            next_port: 49152 + (random_u32() % 8000) as u16,
            tcp: tcp::Tcp::new(random_u32()),
            stats: Stats::default(),
        });
    }
    PRESENT.store(true, Ordering::Relaxed);
    if sched::spawn_detached("netd", netd).is_none() {
        return false;
    }
    ntp::start();
    crate::log!("net: {} MAC {} irq {}", model, Mac(&mac), irq);
    true
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    check("net", self_test())?;
    log_wireless();
    let nic = nic::probe();
    STARTED.store(true, Ordering::Release);
    let Some(nic) = nic else {
        let _ = write!(
            detail,
            "no supported Ethernet controller (USB Ethernet is attached when plugged in)"
        );
        return Ok(());
    };
    let (model, mac) = (nic.model(), nic.mac());
    if !attach(nic) {
        return Err("could not start netd");
    }
    let _ = write!(detail, "{} {}", model, Mac(&mac));
    Ok(())
}

/// Kablosuz kartı tanır ve günlüğe yazar (sürücüsü henüz yok).
fn log_wireless() {
    for d in crate::drivers::pci::devices()
        .into_iter()
        .filter(|d| d.class == 0x02 && d.subclass == 0x80)
    {
        let name = match (d.vendor_id, d.device_id) {
            (0x8086, 0x51f0 | 0x51f1 | 0x7a70 | 0x7af0 | 0x54f0) => "Intel Wi-Fi 6E AX211 (CNVi)",
            (0x8086, 0x2725) => "Intel Wi-Fi 6E AX210",
            (0x8086, 0x272b) => "Intel Wi-Fi 7 BE200",
            (0x14c3, 0x7961) => "MediaTek MT7921 (Wi-Fi 6)",
            (0x14c3, 0x0616 | 0x7922) => "MediaTek MT7922 (Wi-Fi 6E)",
            (0x10ec, 0xb852) => "Realtek RTL8852BE (Wi-Fi 6)",
            (0x10ec, 0xc852) => "Realtek RTL8852CE (Wi-Fi 6E)",
            (0x10ec, 0xc822) => "Realtek RTL8822CE",
            _ => "unknown",
        };
        crate::log!(
            "net: wireless {:04x}:{:04x} {} at {:02x}:{:02x}.{} (no driver yet)",
            d.vendor_id,
            d.device_id,
            name,
            d.bus,
            d.slot,
            d.function
        );
    }
}

crate::initcall!(driver, 70, "Network (Ethernet + IPv4)", init);
