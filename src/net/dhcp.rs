//! DHCP istemcisi (RFC 2131) ve adres çakışması denetimi (RFC 5227).
//!
//! INIT → SELECTING → REQUESTING → CHECKING (ARP yoklaması) → BOUND
//! → RENEWING (T1) → REBINDING (T2) → süre biterse adres bırakılır.
//! Bağlantı koparsa beklenir; geri gelince eski adres INIT-REBOOT ile istenir.

use alloc::string::String;
use alloc::vec::Vec;

use super::wire::{self, Ipv4};
use super::{Config, random_u32};

pub const CLIENT_PORT: u16 = 68;
pub const SERVER_PORT: u16 = 67;

const DISCOVER: u8 = 1;
const OFFER: u8 = 2;
const REQUEST: u8 = 3;
const DECLINE: u8 = 4;
const ACK: u8 = 5;
const NAK: u8 = 6;
const RELEASE: u8 = 7;
const MAGIC: [u8; 4] = [99, 130, 83, 99];
const PROBE_MS: u64 = 1000;
const DECLINE_WAIT_MS: u64 = 10_000;
const MIN_RENEW_RETRY_MS: u64 = 60_000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum State {
    Init,
    Selecting,
    Requesting,
    Checking,
    Bound,
    Renewing,
    Rebinding,
    InitReboot,
    Rebooting,
    LinkDown,
}

pub enum Change {
    None,
    Send(Vec<u8>),
    SendUnicast(Ipv4, Vec<u8>),
    Probe(Ipv4),
    Bound(Config),
    Lost(&'static str),
}

#[derive(Clone, Debug, Default)]
struct Lease {
    address: Ipv4,
    netmask: Ipv4,
    gateway: Ipv4,
    dns: Vec<Ipv4>,
    domain: String,
    server: Ipv4,
    lease_s: u32,
    t1_s: u32,
    t2_s: u32,
}

struct Message {
    kind: u8,
    xid: u32,
    chaddr: [u8; 6],
    lease: Lease,
}

pub struct Client {
    mac: [u8; 6],
    state: State,
    xid: u32,
    attempts: u32,
    next_ms: u64,
    started_ms: u64,
    offer: Option<Lease>,
    lease: Option<Lease>,
    bound_ms: u64,
    check_until: u64,
    conflict: bool,
}

fn backoff(attempt: u32) -> u64 {
    let base = 1000u64 << attempt.min(5).saturating_sub(1);
    base.min(32_000) + (random_u32() % 500) as u64
}

fn parse(data: &[u8]) -> Option<Message> {
    if data.len() < 240 || data[0] != 2 || data[1] != 1 || data[2] != 6 || data[236..240] != MAGIC {
        return None;
    }
    let mut lease = Lease {
        address: Ipv4(data[16..20].try_into().ok()?),
        server: Ipv4(data[20..24].try_into().ok()?),
        ..Lease::default()
    };
    let mut kind = 0;
    let mut i = 240;
    while i < data.len() {
        let code = data[i];
        if code == 0 {
            i += 1;
            continue;
        }
        if code == 255 || i + 1 >= data.len() {
            break;
        }
        let len = data[i + 1] as usize;
        let value = data.get(i + 2..i + 2 + len)?;
        let ip = |v: &[u8]| v.get(0..4).and_then(|b| b.try_into().ok()).map(Ipv4);
        match code {
            1 => lease.netmask = ip(value)?,
            3 => lease.gateway = ip(value).unwrap_or_default(),
            6 => lease.dns = value.as_chunks::<4>().0.iter().map(|c| Ipv4(*c)).collect(),
            15 => lease.domain = String::from_utf8_lossy(value).trim_end_matches('\0').into(),
            51 => lease.lease_s = wire::be32(value, 0)?,
            53 => kind = *value.first()?,
            54 => lease.server = ip(value)?,
            58 => lease.t1_s = wire::be32(value, 0)?,
            59 => lease.t2_s = wire::be32(value, 0)?,
            _ => {}
        }
        i += 2 + len;
    }
    if lease.lease_s == 0 {
        lease.lease_s = 3600;
    }
    if lease.t1_s == 0 || lease.t1_s >= lease.lease_s {
        lease.t1_s = lease.lease_s / 2;
    }
    if lease.t2_s == 0 || lease.t2_s >= lease.lease_s || lease.t2_s <= lease.t1_s {
        lease.t2_s = lease.lease_s / 8 * 7;
    }
    if lease.netmask.is_unspecified() {
        lease.netmask = Ipv4([255, 255, 255, 0]);
    }
    Some(Message {
        kind,
        xid: wire::be32(data, 4)?,
        chaddr: data[28..34].try_into().ok()?,
        lease,
    })
}

impl Client {
    pub fn new(mac: &[u8; 6]) -> Self {
        Self {
            mac: *mac,
            state: State::LinkDown,
            xid: random_u32(),
            attempts: 0,
            next_ms: 0,
            started_ms: 0,
            offer: None,
            lease: None,
            bound_ms: 0,
            check_until: 0,
            conflict: false,
        }
    }

    pub fn state_name(&self) -> &'static str {
        match self.state {
            State::Init | State::Selecting => "dhcp discover",
            State::Requesting => "dhcp request",
            State::Checking => "dhcp probe",
            State::Bound => "bound",
            State::Renewing => "renewing",
            State::Rebinding => "rebinding",
            State::InitReboot | State::Rebooting => "dhcp reboot",
            State::LinkDown => "link down",
        }
    }

    fn message(
        &self,
        kind: u8,
        ciaddr: Ipv4,
        requested: Option<Ipv4>,
        server: Option<Ipv4>,
        now: u64,
    ) -> Vec<u8> {
        let mut m = alloc::vec![0u8; 240];
        m[0] = 1;
        m[1] = 1;
        m[2] = 6;
        m[4..8].copy_from_slice(&self.xid.to_be_bytes());
        let secs = ((now.saturating_sub(self.started_ms)) / 1000).min(0xffff) as u16;
        m[8..10].copy_from_slice(&secs.to_be_bytes());
        if ciaddr.is_unspecified() {
            m[10] = 0x80;
        }
        m[12..16].copy_from_slice(&ciaddr.0);
        m[28..34].copy_from_slice(&self.mac);
        m[236..240].copy_from_slice(&MAGIC);
        m.extend_from_slice(&[53, 1, kind]);
        m.extend_from_slice(&[61, 7, 1]);
        m.extend_from_slice(&self.mac);
        if let Some(ip) = requested {
            m.extend_from_slice(&[50, 4]);
            m.extend_from_slice(&ip.0);
        }
        if let Some(ip) = server {
            m.extend_from_slice(&[54, 4]);
            m.extend_from_slice(&ip.0);
        }
        if kind == DISCOVER || kind == REQUEST {
            m.extend_from_slice(&[12, 8]);
            m.extend_from_slice(b"banna-os");
            m.extend_from_slice(&[57, 2, 0x05, 0xdc]);
            m.extend_from_slice(&[55, 7, 1, 3, 6, 15, 51, 58, 59]);
        }
        m.push(255);
        while m.len() < 300 {
            m.push(0);
        }
        m
    }

    fn broadcast(&self, source: Ipv4, payload: &[u8]) -> Vec<u8> {
        let udp = wire::udp(source, Ipv4::BROADCAST, CLIENT_PORT, SERVER_PORT, payload);
        let ip = wire::ipv4(
            source,
            Ipv4::BROADCAST,
            wire::PROTO_UDP,
            random_u32() as u16,
            &udp,
        );
        wire::ethernet(wire::BROADCAST_MAC, self.mac, wire::ETHERTYPE_IPV4, &ip)
    }

    fn config(lease: &Lease, now: u64) -> Config {
        Config {
            address: lease.address,
            netmask: lease.netmask,
            gateway: lease.gateway,
            dns: lease.dns.clone(),
            domain: lease.domain.clone(),
            server: lease.server,
            lease_seconds: lease.lease_s,
            acquired_ms: now,
        }
    }

    fn restart(&mut self, now: u64, delay: u64) {
        self.state = State::Init;
        self.offer = None;
        self.attempts = 0;
        self.next_ms = now + delay;
    }

    pub fn link_up(&mut self, now: u64) -> Change {
        self.xid = random_u32();
        self.started_ms = now;
        self.attempts = 0;
        self.next_ms = now;
        self.state = if self.lease.is_some() {
            State::InitReboot
        } else {
            State::Init
        };
        Change::None
    }

    pub fn link_down(&mut self) -> Change {
        self.state = State::LinkDown;
        Change::Lost("link down")
    }

    pub fn force_renew(&mut self, now: u64) -> Change {
        if self.lease.is_some()
            && matches!(
                self.state,
                State::Bound | State::Renewing | State::Rebinding
            )
        {
            self.state = State::Renewing;
            self.attempts = 0;
            self.next_ms = now;
        } else if self.state != State::LinkDown {
            self.restart(now, 0);
        }
        Change::None
    }

    pub fn release(&mut self, _mac: [u8; 6]) -> Option<Vec<u8>> {
        let lease = self.lease.take()?;
        let message = self.message(RELEASE, lease.address, None, Some(lease.server), 0);
        self.state = State::Init;
        self.next_ms = u64::MAX;
        Some(message)
    }

    pub fn observe_arp(&mut self, sender_ip: Ipv4, sender_mac: [u8; 6], our_mac: [u8; 6]) {
        if self.state == State::Checking
            && sender_mac != our_mac
            && self.offer.as_ref().is_some_and(|l| l.address == sender_ip)
        {
            self.conflict = true;
        }
    }

    pub fn poll(&mut self, now: u64, _mac: [u8; 6]) -> Change {
        match self.state {
            State::LinkDown => Change::None,
            State::Init if now >= self.next_ms => {
                self.xid = random_u32();
                self.started_ms = now;
                self.state = State::Selecting;
                self.attempts = 0;
                self.next_ms = now;
                Change::None
            }
            State::Selecting if now >= self.next_ms => {
                self.attempts += 1;
                self.next_ms = now + backoff(self.attempts);
                let message = self.message(DISCOVER, Ipv4::UNSPECIFIED, None, None, now);
                Change::Send(self.broadcast(Ipv4::UNSPECIFIED, &message))
            }
            State::Requesting if now >= self.next_ms => {
                if self.attempts >= 4 {
                    self.restart(now, 0);
                    return Change::None;
                }
                self.attempts += 1;
                self.next_ms = now + backoff(self.attempts);
                let offer = self.offer.clone().unwrap_or_default();
                let message = self.message(
                    REQUEST,
                    Ipv4::UNSPECIFIED,
                    Some(offer.address),
                    Some(offer.server),
                    now,
                );
                Change::Send(self.broadcast(Ipv4::UNSPECIFIED, &message))
            }
            State::Checking if self.conflict => {
                let offer = self.offer.clone().unwrap_or_default();
                crate::log!("dhcp: {} is already in use, declining", offer.address);
                let message = self.message(
                    DECLINE,
                    Ipv4::UNSPECIFIED,
                    Some(offer.address),
                    Some(offer.server),
                    now,
                );
                self.restart(now, DECLINE_WAIT_MS);
                Change::Send(self.broadcast(Ipv4::UNSPECIFIED, &message))
            }
            State::Checking if now >= self.check_until => {
                let lease = self.offer.take().unwrap_or_default();
                self.state = State::Bound;
                self.bound_ms = now;
                let config = Self::config(&lease, now);
                self.lease = Some(lease);
                Change::Bound(config)
            }
            State::Bound => {
                let lease = self.lease.as_ref().unwrap();
                if now >= self.bound_ms + lease.t1_s as u64 * 1000 {
                    self.state = State::Renewing;
                    self.attempts = 0;
                    self.next_ms = now;
                }
                Change::None
            }
            State::Renewing | State::Rebinding => {
                let lease = self.lease.clone().unwrap();
                let t2 = self.bound_ms + lease.t2_s as u64 * 1000;
                let expiry = self.bound_ms + lease.lease_s as u64 * 1000;
                if now >= expiry {
                    self.lease = None;
                    self.restart(now, 0);
                    return Change::Lost("lease expired");
                }
                if self.state == State::Renewing && now >= t2 {
                    self.state = State::Rebinding;
                    self.next_ms = now;
                }
                if now < self.next_ms {
                    return Change::None;
                }
                self.xid = random_u32();
                let deadline = if self.state == State::Renewing {
                    t2
                } else {
                    expiry
                };
                self.next_ms =
                    now + ((deadline - now) / 2).max(MIN_RENEW_RETRY_MS.min(deadline - now));
                let message = self.message(REQUEST, lease.address, None, None, now);
                if self.state == State::Renewing && !self.force_broadcast() {
                    Change::SendUnicast(lease.server, message)
                } else {
                    Change::Send(self.broadcast(lease.address, &message))
                }
            }
            State::InitReboot => {
                let requested = self.lease.as_ref().map(|l| l.address);
                self.state = State::Rebooting;
                self.attempts = 1;
                self.next_ms = now + backoff(1);
                let message = self.message(REQUEST, Ipv4::UNSPECIFIED, requested, None, now);
                Change::Send(self.broadcast(Ipv4::UNSPECIFIED, &message))
            }
            State::Rebooting if now >= self.next_ms => {
                if self.attempts >= 2 {
                    self.lease = None;
                    self.restart(now, 0);
                    return Change::None;
                }
                self.attempts += 1;
                self.next_ms = now + backoff(self.attempts);
                let requested = self.lease.as_ref().map(|l| l.address);
                let message = self.message(REQUEST, Ipv4::UNSPECIFIED, requested, None, now);
                Change::Send(self.broadcast(Ipv4::UNSPECIFIED, &message))
            }
            _ => Change::None,
        }
    }

    fn force_broadcast(&self) -> bool {
        self.lease
            .as_ref()
            .is_none_or(|l| l.server.is_unspecified())
    }

    pub fn receive(&mut self, data: &[u8], now: u64, mac: [u8; 6]) -> Change {
        let Some(message) = parse(data) else {
            return Change::None;
        };
        if message.xid != self.xid || message.chaddr != mac {
            return Change::None;
        }
        match (self.state, message.kind) {
            (State::Selecting, OFFER) => {
                crate::log!(
                    "dhcp: offer {} from {}",
                    message.lease.address,
                    message.lease.server
                );
                self.offer = Some(message.lease);
                self.state = State::Requesting;
                self.attempts = 0;
                self.next_ms = now;
                Change::None
            }
            (State::Requesting | State::Rebooting, ACK) => {
                let address = message.lease.address;
                self.offer = Some(message.lease);
                self.state = State::Checking;
                self.conflict = false;
                self.check_until = now + PROBE_MS;
                Change::Probe(address)
            }
            (State::Renewing | State::Rebinding, ACK) => {
                self.state = State::Bound;
                self.bound_ms = now;
                let config = Self::config(&message.lease, now);
                self.lease = Some(message.lease);
                Change::Bound(config)
            }
            (State::Requesting | State::Rebooting | State::Renewing | State::Rebinding, NAK) => {
                crate::log!("dhcp: server refused the address (NAK)");
                let had = self.lease.take().is_some();
                self.restart(now, 0);
                if had {
                    Change::Lost("NAK")
                } else {
                    Change::None
                }
            }
            _ => Change::None,
        }
    }
}

pub fn self_test() -> bool {
    let mac = [0x52, 0x54, 0, 0x12, 0x34, 0x56];
    let client = Client::new(&mac);
    let discover = client.message(DISCOVER, Ipv4::UNSPECIFIED, None, None, 0);
    let mut offer = discover.clone();
    offer[0] = 2;
    offer[16..20].copy_from_slice(&[10, 0, 2, 15]);
    offer.truncate(240);
    offer.extend_from_slice(&[
        53, 1, OFFER, 1, 4, 255, 255, 255, 0, 3, 4, 10, 0, 2, 2, 6, 8, 10, 0, 2, 3, 1, 1, 1, 1,
    ]);
    offer.extend_from_slice(&[51, 4, 0, 0, 0x0e, 0x10, 54, 4, 10, 0, 2, 2, 255]);
    match parse(&offer) {
        Some(m) => {
            m.kind == OFFER
                && m.lease.address == Ipv4([10, 0, 2, 15])
                && m.lease.netmask == Ipv4([255, 255, 255, 0])
                && m.lease.gateway == Ipv4([10, 0, 2, 2])
                && m.lease.dns == [Ipv4([10, 0, 2, 3]), Ipv4([1, 1, 1, 1])]
                && m.lease.lease_s == 3600
                && m.lease.t1_s == 1800
                && m.lease.t2_s == 3150
                && discover.len() >= 300
        }
        None => false,
    }
}
