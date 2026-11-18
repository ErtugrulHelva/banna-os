//! USB Ethernet (CDC-ECM): iletişim arayüzü (sınıf 2, alt sınıf 6) + veri arayüzü
//! (sınıf 0x0a, alternatif ayar 1'de bulk IN/OUT). Çerçeveler başlıksız taşınır.
//!
//! Alım, xHCI'nin kalıcı aktarım hattıyla (bulk IN) yapılır; gönderim eşzamanlı bulk OUT.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use super::{Dma, InterfaceInfo, UsbDevice, host};
use crate::drivers::nic::Nic;
use crate::sync::SpinLock;

const MAX_FRAME: usize = 1514;
const QUEUE_LIMIT: usize = 256;

static RX: SpinLock<VecDeque<Vec<u8>>> = SpinLock::new(VecDeque::new());
static CONNECTED: AtomicBool = AtomicBool::new(false);
static ACTIVE: SpinLock<Option<(usize, u8)>> = SpinLock::new(None);

pub fn is_ecm(iface: &InterfaceInfo) -> bool {
    iface.class == 2 && iface.subclass == 6
}

struct UsbEthernet {
    controller: usize,
    slot: u8,
    out_ep: u8,
    out_max_packet: usize,
    tx: Dma,
    mac: [u8; 6],
}

unsafe impl Send for UsbEthernet {}

impl Nic for UsbEthernet {
    fn mac(&self) -> [u8; 6] {
        self.mac
    }
    fn model(&self) -> &'static str {
        "USB Ethernet (CDC-ECM)"
    }

    fn send(&mut self, frame: &[u8]) -> bool {
        if !CONNECTED.load(Ordering::Relaxed) || frame.len() > MAX_FRAME {
            return false;
        }
        unsafe { core::ptr::copy_nonoverlapping(frame.as_ptr(), self.tx.virt, frame.len()) };
        let ok = host::bulk(
            self.controller,
            self.slot,
            self.out_ep,
            self.tx.phys,
            frame.len(),
            100,
        )
        .is_ok();
        // Uç nokta paket boyutunun katıysa çerçeve sonu sıfır uzunluklu paketle bildirilir.
        if ok && frame.len().is_multiple_of(self.out_max_packet) {
            let _ = host::bulk(
                self.controller,
                self.slot,
                self.out_ep,
                self.tx.phys,
                0,
                100,
            );
        }
        ok
    }

    fn receive(&mut self) -> Option<Vec<u8>> {
        RX.lock().pop_front()
    }

    fn link(&self) -> (bool, u32, bool) {
        (CONNECTED.load(Ordering::Relaxed), 100, true)
    }

    fn take_cause(&mut self) -> (bool, bool) {
        (false, !RX.lock().is_empty())
    }
}

fn parse_mac(text: &str) -> Option<[u8; 6]> {
    let hex: Vec<u8> = text.bytes().filter(|b| b.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, m) in mac.iter_mut().enumerate() {
        let s = core::str::from_utf8(&hex[i * 2..i * 2 + 2]).ok()?;
        *m = u8::from_str_radix(s, 16).ok()?;
    }
    Some(mac)
}

pub fn attach(
    device: &UsbDevice,
    comm: &InterfaceInfo,
    all: &[InterfaceInfo],
) -> Option<&'static str> {
    if host::controller_name(device.controller) != "xHCI" {
        crate::log!("usb-ecm: '{}' needs an xHCI controller", device.name);
        return None;
    }
    // Veri arayüzü: bulk IN ve OUT uç noktası olan alternatif ayar.
    let data = all.iter().find(|i| {
        i.class == 0x0a
            && i.endpoints
                .iter()
                .any(|e| e.transfer_type() == 2 && e.is_in())
            && i.endpoints
                .iter()
                .any(|e| e.transfer_type() == 2 && !e.is_in())
    })?;
    let bulk_in = data
        .endpoints
        .iter()
        .find(|e| e.transfer_type() == 2 && e.is_in())?;
    let bulk_out = data
        .endpoints
        .iter()
        .find(|e| e.transfer_type() == 2 && !e.is_in())?;
    let mac = device
        .string_at(comm.mac_string)
        .and_then(|s| parse_mac(&s))
        .unwrap_or([0x02, 0xba, 0x00, 0x00, 0x00, device.slot]);

    let configs: Vec<host::EndpointConfig> = [bulk_in, bulk_out]
        .iter()
        .map(|e| host::EndpointConfig {
            address: e.address,
            transfer: 2,
            max_packet: e.max_packet,
            interval: 0,
        })
        .collect();
    if !host::configure_endpoints(device.controller, device.slot, device.speed, &configs) {
        crate::log!("usb-ecm: endpoints could not be configured");
        return None;
    }
    // SET_INTERFACE(veri, alternatif) ve paket süzgeci: yönlendirilmiş + yayın + çoklu yayın.
    device.control_out(0x01, 11, data.alternate as u16, data.number as u16);
    device.control_out(0x21, 0x43, 0x000e, comm.number as u16);

    let handler: host::Handler = alloc::sync::Arc::new(|data: &[u8]| {
        if !data.is_empty() && data.len() <= 2048 {
            let mut rx = RX.lock();
            if rx.len() < QUEUE_LIMIT {
                rx.push_back(data.to_vec());
            }
            drop(rx);
            crate::net::wake();
        }
    });
    if !host::start_interrupt_pipe(
        device.controller,
        device.slot,
        bulk_in.address,
        2048,
        handler,
    ) {
        crate::log!("usb-ecm: receive pipe could not be started");
        return None;
    }
    let tx = Dma::alloc(1)?;
    CONNECTED.store(true, Ordering::Relaxed);
    *ACTIVE.lock() = Some((device.controller, device.slot));
    let nic = UsbEthernet {
        controller: device.controller,
        slot: device.slot,
        out_ep: bulk_out.address,
        out_max_packet: bulk_out.max_packet.max(8) as usize,
        tx,
        mac,
    };
    // Ağ yığını zaten bir kartla çalışıyorsa bu kart kullanılmaz (log'a yazılır).
    crate::net::attach(Box::new(nic));
    Some("ethernet")
}

pub fn detach(device: &UsbDevice) {
    let mut active = ACTIVE.lock();
    if *active == Some((device.controller, device.slot)) {
        *active = None;
        CONNECTED.store(false, Ordering::Relaxed);
        RX.lock().clear();
        crate::log!("usb-ecm: '{}' disconnected", device.name);
        crate::net::wake();
    }
}
