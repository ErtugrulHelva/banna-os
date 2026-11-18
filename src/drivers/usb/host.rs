//! USB ana denetleyici katmanı: çekirdek ve sınıf sürücüleri denetleyiciden bağımsız
//! çağrılar yapar; numara 0..7 xHCI, 8..15 EHCI, 16..23 UHCI, 24 ve üstü OHCI'ye gider.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::{Speed, ehci, ohci, uhci, xhci};
use crate::sched::WaitQueue;

/// EHCI denetleyicilerinin numaralandığı taban.
pub const EHCI_BASE: usize = 8;
/// UHCI denetleyicilerinin numaralandığı taban.
pub const UHCI_BASE: usize = 16;
/// OHCI denetleyicilerinin numaralandığı taban.
pub const OHCI_BASE: usize = 24;

/// Port durumu değişince ya da aktarım bitince uyandırılan `usbd` kuyruğu.
pub static EVENTS: WaitQueue = WaitQueue::new();

pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT_PACKET: u8 = 13;
pub type Handler = Arc<dyn Fn(&[u8]) + Send + Sync>;
/// Isochronous OUT tamponunu dolduran geri çağırma (kesme bağlamında, kısa olmalı).
pub type IsoFill = Arc<dyn Fn(&mut [u8]) + Send + Sync>;
/// Aygıtın USB ağacındaki yeri: kök port, hub katmanları (her biri 4 bit) ve düşük/tam
/// hızlı aygıtlar için yüksek hızlı hub'daki geçiş çevirmeni (TT: hub yuvası, port).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Route {
    pub root_port: u8,
    pub route: u32,
    pub tt_slot: u8,
    pub tt_port: u8,
}

/// Hub yuvası için Configure Endpoint'te verilecek bilgiler.
#[derive(Clone, Copy, Debug)]
pub struct HubInfo {
    pub ports: u8,
    /// TT düşünme süresi (0..3).
    pub think_time: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Setup {
    pub request_type: u8,
    pub request: u8,
    pub value: u16,
    pub index: u16,
    pub length: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct EndpointConfig {
    pub address: u8,
    /// 2 = bulk, 3 = interrupt
    pub transfer: u8,
    pub max_packet: u16,
    pub interval: u8,
}

enum Hc {
    X(usize),
    E(usize),
    U(usize),
    O(usize),
}

fn hc(c: usize) -> Hc {
    if c >= OHCI_BASE {
        Hc::O(c - OHCI_BASE)
    } else if c >= UHCI_BASE {
        Hc::U(c - UHCI_BASE)
    } else if c >= EHCI_BASE {
        Hc::E(c - EHCI_BASE)
    } else {
        Hc::X(c)
    }
}

/// Tüm denetleyicileri bulur; numaraları döndürür.
pub fn probe() -> Vec<usize> {
    let mut out: Vec<usize> = (0..xhci::probe()).collect();
    out.extend((0..ehci::probe()).map(|i| EHCI_BASE + i));
    // Eşlikçiler EHCI'den sonra: EHCI portları önce sahiplenip yavaş aygıtları bırakır.
    out.extend((0..uhci::probe()).map(|i| UHCI_BASE + i));
    out.extend((0..ohci::probe()).map(|i| OHCI_BASE + i));
    out
}

pub fn poll_all() {
    xhci::poll_all();
    ehci::poll_all();
    uhci::poll_all();
    ohci::poll_all();
}

pub fn take_event_pending() -> bool {
    xhci::take_event_pending()
        | ehci::take_event_pending()
        | uhci::take_event_pending()
        | ohci::take_event_pending()
}

pub fn take_port_changes() -> Vec<(usize, u8)> {
    let mut out = xhci::take_port_changes();
    out.extend(
        ehci::take_port_changes()
            .into_iter()
            .map(|(c, p)| (EHCI_BASE + c, p)),
    );
    out.extend(
        uhci::take_port_changes()
            .into_iter()
            .map(|(c, p)| (UHCI_BASE + c, p)),
    );
    out.extend(
        ohci::take_port_changes()
            .into_iter()
            .map(|(c, p)| (OHCI_BASE + c, p)),
    );
    out
}

pub fn max_ports(c: usize) -> u8 {
    match hc(c) {
        Hc::X(i) => xhci::max_ports(i),
        Hc::E(i) => ehci::max_ports(i),
        Hc::U(i) => uhci::max_ports(i),
        Hc::O(i) => ohci::max_ports(i),
    }
}

pub fn port_connected(c: usize, port: u8) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::port_status(i, port) & 1 != 0,
        Hc::E(i) => ehci::port_connected(i, port),
        Hc::U(i) => uhci::port_connected(i, port),
        Hc::O(i) => ohci::port_connected(i, port),
    }
}

pub fn clear_port_changes(c: usize, port: u8) {
    match hc(c) {
        Hc::X(i) => xhci::clear_port_changes(i, port),
        Hc::E(i) => ehci::clear_port_changes(i, port),
        Hc::U(i) => uhci::clear_port_changes(i, port),
        Hc::O(i) => ohci::clear_port_changes(i, port),
    }
}

pub fn reset_port(c: usize, port: u8) -> Option<Speed> {
    match hc(c) {
        Hc::X(i) => xhci::reset_port(i, port),
        Hc::E(i) => ehci::reset_port(i, port),
        Hc::U(i) => uhci::reset_port(i, port),
        Hc::O(i) => ohci::reset_port(i, port),
    }
}

pub fn address_device(c: usize, at: Route, speed: Speed) -> Result<u8, &'static str> {
    match hc(c) {
        Hc::X(i) => xhci::address_device(i, at, speed),
        Hc::E(i) => ehci::address_device(i, at, speed),
        Hc::U(i) => uhci::address_device(i, at, speed),
        Hc::O(i) => ohci::address_device(i, at, speed),
    }
}

pub fn set_ep0_max_packet(c: usize, slot: u8, mps: u16) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::set_ep0_max_packet(i, slot, mps),
        Hc::E(i) => ehci::set_ep0_max_packet(i, slot, mps),
        Hc::U(i) => uhci::set_ep0_max_packet(i, slot, mps),
        Hc::O(i) => ohci::set_ep0_max_packet(i, slot, mps),
    }
}

pub fn disable_slot(c: usize, slot: u8) {
    match hc(c) {
        Hc::X(i) => xhci::disable_slot(i, slot),
        Hc::E(i) => ehci::disable_slot(i, slot),
        Hc::U(i) => uhci::disable_slot(i, slot),
        Hc::O(i) => ohci::disable_slot(i, slot),
    }
}

pub fn slot_of_port(c: usize, port: u8) -> Option<u8> {
    match hc(c) {
        Hc::X(i) => xhci::slot_of_port(i, port),
        Hc::E(i) => ehci::slot_at(i, port, 0),
        Hc::U(i) => uhci::slot_at(i, port, 0),
        Hc::O(i) => ohci::slot_at(i, port, 0),
    }
}

pub fn mark_hub(c: usize, slot: u8, info: HubInfo) {
    if let Hc::X(i) = hc(c) {
        xhci::mark_hub(i, slot, info);
    }
}

pub fn control(c: usize, slot: u8, setup: Setup, data: &mut [u8]) -> Result<usize, u8> {
    match hc(c) {
        Hc::X(i) => xhci::control(i, slot, setup, data),
        Hc::E(i) => ehci::control(i, slot, setup, data),
        Hc::U(i) => uhci::control(i, slot, setup, data),
        Hc::O(i) => ohci::control(i, slot, setup, data),
    }
}

pub fn configure_endpoints(c: usize, slot: u8, speed: Speed, endpoints: &[EndpointConfig]) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::configure_endpoints(i, slot, speed, endpoints),
        Hc::E(i) => ehci::configure_endpoints(i, slot, speed, endpoints),
        Hc::U(i) => uhci::configure_endpoints(i, slot, speed, endpoints),
        Hc::O(i) => ohci::configure_endpoints(i, slot, speed, endpoints),
    }
}

/// Isochronous OUT akışı (yalnızca xHCI; EHCI/UHCI/OHCI'de `false`).
pub fn start_iso_out(
    c: usize,
    slot: u8,
    address: u8,
    bytes: usize,
    count: usize,
    fill: IsoFill,
) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::start_iso_out(i, slot, address, bytes, count, fill),
        _ => false,
    }
}

pub fn start_interrupt_pipe(
    c: usize,
    slot: u8,
    address: u8,
    size: usize,
    handler: Handler,
) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::start_interrupt_pipe(i, slot, address, size, handler),
        Hc::E(i) => ehci::start_interrupt_pipe(i, slot, address, size, handler),
        Hc::U(i) => uhci::start_interrupt_pipe(i, slot, address, size, handler),
        Hc::O(i) => ohci::start_interrupt_pipe(i, slot, address, size, handler),
    }
}

/// `buffer` fiziksel adrestir (EHCI için 4 GiB altında olmalı).
pub fn bulk(
    c: usize,
    slot: u8,
    address: u8,
    buffer: u64,
    len: usize,
    timeout_ms: u64,
) -> Result<usize, u8> {
    match hc(c) {
        Hc::X(i) => xhci::bulk(i, slot, address, buffer, len, timeout_ms),
        Hc::E(i) => ehci::bulk(i, slot, address, buffer, len, timeout_ms),
        Hc::U(i) => uhci::bulk(i, slot, address, buffer, len, timeout_ms),
        Hc::O(i) => ohci::bulk(i, slot, address, buffer, len, timeout_ms),
    }
}

pub fn reset_endpoint(c: usize, slot: u8, address: u8) -> bool {
    match hc(c) {
        Hc::X(i) => xhci::reset_endpoint(i, slot, address),
        Hc::E(i) => ehci::reset_endpoint(i, slot, address),
        Hc::U(i) => uhci::reset_endpoint(i, slot, address),
        Hc::O(i) => ohci::reset_endpoint(i, slot, address),
    }
}

pub fn controller_name(c: usize) -> &'static str {
    match hc(c) {
        Hc::X(_) => "xHCI",
        Hc::E(_) => "EHCI",
        Hc::U(_) => "UHCI",
        Hc::O(_) => "OHCI",
    }
}
