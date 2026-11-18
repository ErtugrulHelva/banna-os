//! USB hub sınıf sürücüsü (USB 2 ve USB 3 hub'ları, iç içe hub'lar).
//!
//! Hub yuvası xHCI'ye hub olarak tanıtılır, portlarına güç verilir; port durumları
//! `usbd` thread'inden periyodik olarak sorgulanır. Takılan aygıt sıfırlanıp hızı
//! okunur ve USB çekirdeğinin numaralandırma yoluna (`attach_device`) verilir.

use alloc::vec::Vec;

use super::{InterfaceInfo, Speed, UsbDevice, host};
use crate::arch::pit;
use crate::sync::SpinLock;

const HUB_DESCRIPTOR: u16 = 0x29;
const SUPERSPEED_HUB_DESCRIPTOR: u16 = 0x2a;
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_ENABLE: u16 = 17;
const C_PORT_RESET: u16 = 20;
const C_PORT_LINK_STATE: u16 = 25;
const C_BH_PORT_RESET: u16 = 29;
const POLL_MS: u64 = 250;
const MAX_DEPTH: u8 = 5;

#[derive(Clone)]
struct Hub {
    device: UsbDevice,
    ports: u8,
    superspeed: bool,
    /// Portta bağlı (numaralandırılmış) aygıt var mı.
    attached: [bool; 16],
}

static HUBS: SpinLock<Vec<Hub>> = SpinLock::new(Vec::new());
static LAST_POLL: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

fn class_request(
    device: &UsbDevice,
    request_type: u8,
    request: u8,
    value: u16,
    index: u16,
) -> bool {
    device.control_out(request_type, request, value, index)
}

fn set_port_feature(device: &UsbDevice, port: u8, feature: u16) -> bool {
    class_request(device, 0x23, 3, feature, port as u16)
}

fn clear_port_feature(device: &UsbDevice, port: u8, feature: u16) -> bool {
    class_request(device, 0x23, 1, feature, port as u16)
}

/// (wPortStatus, wPortChange)
fn port_status(device: &UsbDevice, port: u8) -> Option<(u16, u16)> {
    let mut buf = [0u8; 4];
    let n = device
        .control(
            host::Setup {
                request_type: 0xa3,
                request: 0,
                value: 0,
                index: port as u16,
                length: 4,
            },
            &mut buf,
        )
        .ok()?;
    (n == 4).then(|| {
        (
            u16::from_le_bytes([buf[0], buf[1]]),
            u16::from_le_bytes([buf[2], buf[3]]),
        )
    })
}

pub fn attach(device: &UsbDevice, iface: &InterfaceInfo) -> Option<&'static str> {
    if device.depth >= MAX_DEPTH {
        crate::log!("usb: hub '{}' too deep, ignored", device.name);
        return None;
    }
    let superspeed = matches!(device.speed, Speed::Super | Speed::SuperPlus);
    let kind = if superspeed {
        SUPERSPEED_HUB_DESCRIPTOR
    } else {
        HUB_DESCRIPTOR
    };
    let mut desc = [0u8; 16];
    let n = device
        .control(
            host::Setup {
                request_type: 0xa0,
                request: 6,
                value: kind << 8,
                index: 0,
                length: desc.len() as u16,
            },
            &mut desc,
        )
        .ok()?;
    if n < 7 {
        return None;
    }
    let ports = desc[2].min(15);
    let characteristics = u16::from_le_bytes([desc[3], desc[4]]);
    let power_good_ms = desc[5] as u64 * 2;

    host::mark_hub(
        device.controller,
        device.slot,
        host::HubInfo {
            ports,
            think_time: ((characteristics >> 5) & 3) as u8,
        },
    );
    let status = iface
        .endpoints
        .iter()
        .find(|e| e.is_in() && e.transfer_type() == 3)?;
    let config = host::EndpointConfig {
        address: status.address,
        transfer: 3,
        max_packet: status.max_packet,
        interval: status.interval,
    };
    if !host::configure_endpoints(device.controller, device.slot, device.speed, &[config]) {
        crate::log!("usb: hub '{}' could not be configured", device.name);
        return None;
    }
    if superspeed {
        // SET_HUB_DEPTH: kök porta bağlı hub için 0.
        class_request(device, 0x20, 12, device.depth as u16, 0);
    }
    for port in 1..=ports {
        set_port_feature(device, port, PORT_POWER);
    }
    crate::sched::sleep_ms(power_good_ms.max(20) + 20);
    let hub = Hub {
        device: device.clone(),
        ports,
        superspeed,
        attached: [false; 16],
    };
    HUBS.lock().push(hub.clone());
    crate::log!(
        "usb: hub '{}' with {} port(s){}",
        device.name,
        ports,
        if superspeed { " (USB 3)" } else { "" }
    );
    scan(&hub);
    Some("hub")
}

pub fn detach(device: &UsbDevice) {
    HUBS.lock()
        .retain(|h| !(h.device.controller == device.controller && h.device.slot == device.slot));
}

/// Portu sıfırlar; etkinleşen aygıtın hızını döndürür.
fn reset_port(hub: &Hub, port: u8) -> Option<Speed> {
    let device = &hub.device;
    set_port_feature(device, port, PORT_RESET);
    let deadline = pit::uptime_ms() + 500;
    let status = loop {
        crate::sched::sleep_ms(10);
        let (status, change) = port_status(device, port)?;
        if change & (1 << 4) != 0 || (status & (1 << 4) == 0 && status & 2 != 0) {
            break status;
        }
        if pit::uptime_ms() > deadline {
            return None;
        }
    };
    clear_port_feature(device, port, C_PORT_RESET);
    if hub.superspeed {
        clear_port_feature(device, port, C_BH_PORT_RESET);
        clear_port_feature(device, port, C_PORT_LINK_STATE);
    }
    crate::sched::sleep_ms(10);
    if status & 2 == 0 {
        return None;
    }
    Some(if hub.superspeed {
        Speed::Super
    } else if status & (1 << 9) != 0 {
        Speed::Low
    } else if status & (1 << 10) != 0 {
        Speed::High
    } else {
        Speed::Full
    })
}

fn child_route(hub: &UsbDevice, port: u8, speed: Speed) -> host::Route {
    let mut route = host::Route {
        root_port: hub.route.root_port,
        route: hub.route.route | ((port as u32 & 0xf) << (4 * hub.depth as u32)),
        tt_slot: 0,
        tt_port: 0,
    };
    if matches!(speed, Speed::Low | Speed::Full) {
        if hub.speed == Speed::High {
            route.tt_slot = hub.slot;
            route.tt_port = port;
        } else {
            route.tt_slot = hub.route.tt_slot;
            route.tt_port = hub.route.tt_port;
        }
    }
    route
}

/// Hub portlarındaki bağlanma/kopma değişikliklerini işler.
fn scan(hub: &Hub) {
    let device = &hub.device;
    for port in 1..=hub.ports {
        let Some((status, change)) = port_status(device, port) else {
            continue;
        };
        let connected = status & 1 != 0;
        if change & 1 != 0 {
            clear_port_feature(device, port, C_PORT_CONNECTION);
        }
        if change & 2 != 0 {
            clear_port_feature(device, port, C_PORT_ENABLE);
        }
        let attached = hub.attached[port as usize];
        let route_bits = (port as u32 & 0xf) << (4 * device.depth as u32);
        let child = device.route.route | route_bits;
        if attached && (!connected || change & 1 != 0) {
            super::detach_route(
                device.controller,
                device.route.root_port,
                child,
                device.depth + 1,
            );
            set_attached(device, port, false);
        }
        if connected && (!attached || change & 1 != 0) {
            crate::sched::sleep_ms(100);
            let Some(speed) = reset_port(hub, port) else {
                crate::log!("usb: hub '{}' port {}: reset failed", device.name, port);
                continue;
            };
            let route = child_route(device, port, speed);
            match super::attach_device(device.controller, route, device.depth + 1, speed) {
                Ok(()) => set_attached(device, port, true),
                Err(e) => crate::log!("usb: hub '{}' port {}: {}", device.name, port, e),
            }
        }
    }
}

fn set_attached(device: &UsbDevice, port: u8, value: bool) {
    if let Some(h) = HUBS
        .lock()
        .iter_mut()
        .find(|h| h.device.controller == device.controller && h.device.slot == device.slot)
    {
        h.attached[port as usize] = value;
    }
}

/// `usbd` thread'inden çağrılır; hub portlarını en fazla 250 ms'de bir yoklar.
pub fn poll() {
    let now = pit::uptime_ms();
    let last = LAST_POLL.load(core::sync::atomic::Ordering::Relaxed);
    if now < last + POLL_MS {
        return;
    }
    LAST_POLL.store(now, core::sync::atomic::Ordering::Relaxed);
    let hubs: Vec<Hub> = HUBS.lock().clone();
    for hub in hubs {
        // Tarama sırasında güncellenen `attached` bilgisi için en son kopya.
        let current = HUBS
            .lock()
            .iter()
            .find(|h| {
                h.device.controller == hub.device.controller && h.device.slot == hub.device.slot
            })
            .cloned();
        if let Some(current) = current {
            scan(&current);
        }
    }
}
