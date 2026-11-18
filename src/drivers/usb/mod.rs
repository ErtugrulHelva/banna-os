//! USB çekirdeği: aygıt sayımı (enumeration), tanımlayıcı ayrıştırma, sınıf
//! sürücülerine dağıtım (HID, yığın bellek) ve sıcak takma için `usbd` thread'i.

pub mod ecm;
pub mod ehci;
pub mod hid;
pub mod hid_report;
pub mod host;
pub mod hub;
pub mod msc;
pub mod ohci;
pub mod uac;
pub mod uhci;
pub mod xhci;

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::kernel::initcall::Detail;
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sync::SpinLock;

/// Fiziksel olarak bitişik, sıfırlanmış DMA belleği.
pub struct Dma {
    pub phys: u64,
    pub virt: *mut u8,
    pages: u64,
}

unsafe impl Send for Dma {}
unsafe impl Sync for Dma {}

impl Dma {
    pub fn alloc(pages: u64) -> Option<Self> {
        let phys = pmm::alloc_contiguous(pages)?;
        let virt = phys_to_virt(phys);
        unsafe { core::ptr::write_bytes(virt, 0, (pages * PAGE_SIZE) as usize) };
        Some(Self { phys, virt, pages })
    }

    /// `bytes` boyutunda, `align` sınırına hizalı bölge (ör. 64 KiB sınırını geçmeyen tampon).
    pub fn alloc_aligned(bytes: u64, align: u64) -> Option<(Self, u64)> {
        let pages = (bytes + align).div_ceil(PAGE_SIZE);
        let dma = Self::alloc(pages)?;
        let aligned = dma.phys.next_multiple_of(align);
        Some((dma, aligned))
    }
}

impl Drop for Dma {
    fn drop(&mut self) {
        for i in 0..self.pages {
            pmm::free_frame(self.phys + i * PAGE_SIZE);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Low,
    Full,
    High,
    Super,
    SuperPlus,
}

impl Speed {
    fn from_xhci(id: u32) -> Option<Self> {
        Some(match id {
            1 => Speed::Full,
            2 => Speed::Low,
            3 => Speed::High,
            4 => Speed::Super,
            5 => Speed::SuperPlus,
            _ => return None,
        })
    }

    fn xhci_id(self) -> u32 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::Super => 4,
            Speed::SuperPlus => 5,
        }
    }

    fn default_max_packet(self) -> u16 {
        match self {
            Speed::Low | Speed::Full => 8,
            Speed::High => 64,
            Speed::Super | Speed::SuperPlus => 512,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Speed::Low => "1.5 Mbit/s",
            Speed::Full => "12 Mbit/s",
            Speed::High => "480 Mbit/s",
            Speed::Super => "5 Gbit/s",
            Speed::SuperPlus => "10 Gbit/s",
        }
    }
}

#[derive(Clone, Debug)]
pub struct EndpointInfo {
    pub address: u8,
    pub attributes: u8,
    pub max_packet: u16,
    pub interval: u8,
}

impl EndpointInfo {
    pub fn transfer_type(&self) -> u8 {
        self.attributes & 3
    }

    pub fn is_in(&self) -> bool {
        self.address & 0x80 != 0
    }
}

#[derive(Clone, Debug)]
pub struct InterfaceInfo {
    pub number: u8,
    pub class: u8,
    pub subclass: u8,
    pub protocol: u8,
    /// Alternatif ayar numarası (sınıf sürücüleri çoğunlukla 0'ı kullanır).
    pub alternate: u8,
    pub hid_report_length: u16,
    /// CDC Ethernet işlevsel tanımlayıcısındaki MAC adresi dizgesinin numarası.
    pub mac_string: u8,
    pub endpoints: Vec<EndpointInfo>,
    /// Arayüze ait sınıfa özgü tanımlayıcılar (CS_INTERFACE, ör. ses biçimi).
    pub extra: Vec<u8>,
}

/// Sınıf sürücülerinin kullandığı aygıt tanıtıcısı.
#[derive(Clone, Debug)]
pub struct UsbDevice {
    pub controller: usize,
    pub slot: u8,
    /// Kök port (hub arkasındaysa da yolun başladığı port).
    pub port: u8,
    /// Hub ağacındaki yer; kök porttaki aygıtta `route.route` = 0.
    pub route: host::Route,
    /// Üstündeki hub sayısı.
    pub depth: u8,
    pub speed: Speed,
    pub vendor: u16,
    pub product: u16,
    pub class: u8,
    pub name: String,
}

impl UsbDevice {
    pub fn control(&self, setup: host::Setup, data: &mut [u8]) -> Result<usize, u8> {
        host::control(self.controller, self.slot, setup, data)
    }

    pub fn control_out(&self, request_type: u8, request: u8, value: u16, index: u16) -> bool {
        self.control(
            host::Setup {
                request_type,
                request,
                value,
                index,
                length: 0,
            },
            &mut [],
        )
        .is_ok()
    }

    /// Dizge tanımlayıcısı (UTF-16 → String).
    pub fn string_at(&self, index: u8) -> Option<String> {
        self.string(index)
    }

    pub fn get_descriptor(
        &self,
        kind: u8,
        index: u8,
        language: u16,
        len: usize,
    ) -> Option<Vec<u8>> {
        let mut buf = alloc::vec![0u8; len];
        let n = self
            .control(
                host::Setup {
                    request_type: 0x80,
                    request: 6,
                    value: (kind as u16) << 8 | index as u16,
                    index: language,
                    length: len as u16,
                },
                &mut buf,
            )
            .ok()?;
        buf.truncate(n);
        Some(buf)
    }

    fn string(&self, index: u8) -> Option<String> {
        if index == 0 {
            return None;
        }
        let raw = self.get_descriptor(3, index, 0x0409, 255)?;
        let len = (*raw.first()? as usize).min(raw.len());
        let units: Vec<u16> = raw[2..len]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|c| u16::from_le_bytes(*c))
            .collect();
        let text: String = char::decode_utf16(units)
            .map(|c| c.unwrap_or('?'))
            .collect();
        Some(String::from(text.trim()))
    }
}

/// Yapılandırma tanımlayıcısından arayüzleri ve uç noktaları çıkarır.
pub fn parse_configuration(data: &[u8]) -> (u8, Vec<InterfaceInfo>) {
    let value = data.get(5).copied().unwrap_or(1);
    let mut interfaces: Vec<InterfaceInfo> = Vec::new();
    let mut i = 0;
    while i + 2 <= data.len() {
        let len = data[i] as usize;
        if len < 2 || i + len > data.len() {
            break;
        }
        let d = &data[i..i + len];
        match d[1] {
            4 if len >= 9 => {
                interfaces.push(InterfaceInfo {
                    number: d[2],
                    class: d[5],
                    subclass: d[6],
                    protocol: d[7],
                    alternate: d[3],
                    hid_report_length: 0,
                    mac_string: 0,
                    endpoints: Vec::new(),
                    extra: Vec::new(),
                });
            }
            0x24 => {
                if let Some(iface) = interfaces.last_mut() {
                    // CDC Ethernet Networking işlevsel tanımlayıcısı: iMACAddress.
                    if len >= 4 && d[2] == 0x0f {
                        iface.mac_string = d[3];
                    }
                    iface.extra.extend_from_slice(d);
                }
            }
            0x21 if len >= 9 => {
                if let Some(iface) = interfaces.last_mut() {
                    iface.hid_report_length = u16::from_le_bytes([d[7], d[8]]);
                }
            }
            5 if len >= 7 => {
                if let Some(iface) = interfaces.last_mut() {
                    iface.endpoints.push(EndpointInfo {
                        address: d[2],
                        attributes: d[3],
                        max_packet: u16::from_le_bytes([d[4], d[5]]),
                        interval: d[6],
                    });
                }
            }
            _ => {}
        }
        i += len;
    }
    (value, interfaces)
}

#[derive(Clone, Debug)]
pub struct Attached {
    pub device: UsbDevice,
    pub drivers: Vec<&'static str>,
}

static DEVICES: SpinLock<Vec<Attached>> = SpinLock::new(Vec::new());

pub fn devices() -> Vec<Attached> {
    DEVICES.lock().clone()
}

fn class_name(class: u8) -> &'static str {
    match class {
        0 => "per-interface",
        3 => "HID",
        8 => "mass storage",
        9 => "hub",
        0xe0 => "wireless",
        0xef => "misc",
        0xff => "vendor",
        _ => "other",
    }
}

/// Kök porttaki yeni aygıtı sıfırlar ve bağlar.
fn enumerate(c: usize, port: u8) -> Result<(), &'static str> {
    let speed = host::reset_port(c, port).ok_or("port reset failed")?;
    let route = host::Route {
        root_port: port,
        ..Default::default()
    };
    attach_device(c, route, 0, speed)
}

/// Sıfırlanmış (kök ya da hub portundaki) aygıtı adresler, yapılandırır ve sınıf
/// sürücüsüne bağlar.
pub fn attach_device(
    c: usize,
    route: host::Route,
    depth: u8,
    speed: Speed,
) -> Result<(), &'static str> {
    let port = route.root_port;
    let slot = host::address_device(c, route, speed)?;
    let mut device = UsbDevice {
        controller: c,
        slot,
        port,
        route,
        depth,
        speed,
        vendor: 0,
        product: 0,
        class: 0,
        name: String::new(),
    };
    let first = device
        .get_descriptor(1, 0, 0, 8)
        .filter(|d| d.len() >= 8)
        .ok_or("no device descriptor")?;
    let mps = match speed {
        Speed::Super | Speed::SuperPlus => 1u16 << first[7].min(10),
        _ => first[7] as u16,
    };
    if mps != speed.default_max_packet() && mps >= 8 {
        host::set_ep0_max_packet(c, slot, mps);
    }
    let desc = device
        .get_descriptor(1, 0, 0, 18)
        .filter(|d| d.len() >= 18)
        .ok_or("short device descriptor")?;
    device.class = desc[4];
    device.vendor = u16::from_le_bytes([desc[8], desc[9]]);
    device.product = u16::from_le_bytes([desc[10], desc[11]]);
    // Birden çok yapılandırma varsa sürücüsü olan ilkini seç (ör. USB Ethernet
    // adaptörlerinde CDC-ECM çoğunlukla 2. yapılandırmadadır).
    let read_config = |index: u8| -> Option<Vec<u8>> {
        let header = device
            .get_descriptor(2, index, 0, 9)
            .filter(|d| d.len() >= 4)?;
        let total = u16::from_le_bytes([header[2], header[3]]) as usize;
        device.get_descriptor(2, index, 0, total.min(4096))
    };
    let supported = |interfaces: &[InterfaceInfo]| {
        interfaces
            .iter()
            .any(|i| matches!(i.class, 1 | 3 | 8 | 9) || ecm::is_ecm(i))
    };
    let configurations = desc[17].clamp(1, 8);
    let mut config = read_config(0).ok_or("no configuration")?;
    if !supported(&parse_configuration(&config).1) {
        for index in 1..configurations {
            if let Some(other) = read_config(index)
                && supported(&parse_configuration(&other).1)
            {
                config = other;
                break;
            }
        }
    }
    device.name = device
        .string(desc[15])
        .or_else(|| device.string(desc[14]))
        .unwrap_or_else(|| alloc::format!("{:04x}:{:04x}", device.vendor, device.product));
    let (value, interfaces) = parse_configuration(&config);
    if !device.control_out(0x00, 9, value as u16, 0) {
        host::disable_slot(c, slot);
        return Err("set configuration failed");
    }
    let mut drivers = Vec::new();
    // Hub, alt aygıtlarından önce listeye girer (kopuşta ağaç sırası korunur).
    DEVICES.lock().push(Attached {
        device: device.clone(),
        drivers: Vec::new(),
    });
    for iface in interfaces.iter().filter(|i| i.alternate == 0) {
        let attached = match iface.class {
            3 => hid::attach(&device, iface),
            8 => msc::attach(&device, iface),
            9 => hub::attach(&device, iface),
            1 if iface.subclass == 1 => uac::attach(&device, iface, &interfaces),
            2 if ecm::is_ecm(iface) => ecm::attach(&device, iface, &interfaces),
            _ => None,
        };
        if let Some(name) = attached {
            drivers.push(name);
        }
    }
    crate::log!(
        "usb: port {}{} {} {:04x}:{:04x} '{}' class {} -> {}",
        port,
        route_suffix(route.route, depth),
        speed.name(),
        device.vendor,
        device.product,
        device.name,
        class_name(if device.class == 0 {
            interfaces.first().map_or(0, |i| i.class)
        } else {
            device.class
        }),
        if drivers.is_empty() {
            String::from("no driver")
        } else {
            drivers.join(", ")
        }
    );
    if let Some(entry) = DEVICES
        .lock()
        .iter_mut()
        .find(|a| a.device.controller == c && a.device.slot == slot)
    {
        entry.device = device;
        entry.drivers = drivers;
    }
    Ok(())
}

/// Hub yolu metni: kök port 3 → hub portu 2 → ".2".
fn route_suffix(route: u32, depth: u8) -> String {
    (0..depth)
        .map(|i| alloc::format!(".{}", (route >> (4 * i)) & 0xf))
        .collect()
}

/// `route`'un (derinlik `depth`) altında kalan aygıt mı (kendisi dahil).
fn is_under(device: &UsbDevice, root_port: u8, route: u32, depth: u8) -> bool {
    let mask = if depth == 0 {
        0
    } else {
        (1u32 << (4 * depth as u32)) - 1
    };
    device.port == root_port && device.depth >= depth && device.route.route & mask == route
}

/// Bir portun (kök ya da hub) arkasındaki tüm aygıtları ayırır.
pub fn detach_route(c: usize, root_port: u8, route: u32, depth: u8) {
    let removed: Vec<Attached> = {
        let mut list = DEVICES.lock();
        let (gone, keep): (Vec<_>, Vec<_>) = list.drain(..).partition(|a| {
            a.device.controller == c && is_under(&a.device, root_port, route, depth)
        });
        *list = keep;
        gone
    };
    // En derindekiler önce: alt aygıtlar hub'larından önce kapanır.
    for a in removed.iter().rev() {
        crate::log!(
            "usb: port {}{} '{}' disconnected",
            a.device.port,
            route_suffix(a.device.route.route, a.device.depth),
            a.device.name
        );
        hid::detach(&a.device);
        msc::detach(&a.device);
        hub::detach(&a.device);
        ecm::detach(&a.device);
        uac::detach(&a.device);
        host::disable_slot(c, a.device.slot);
    }
}

fn detach(c: usize, port: u8) {
    detach_route(c, port, 0, 0);
    if let Some(slot) = host::slot_of_port(c, port) {
        host::disable_slot(c, slot);
    }
}

fn scan_ports(c: usize) {
    for port in 1..=host::max_ports(c) {
        if host::port_connected(c, port) && host::slot_of_port(c, port).is_none() {
            if let Err(e) = enumerate(c, port)
                && host::port_connected(c, port)
            {
                crate::log!("usb: port {}: {}", port, e);
            }
        } else {
            host::clear_port_changes(c, port);
        }
    }
}

fn usbd() -> i64 {
    loop {
        host::EVENTS.wait_timeout(host::take_event_pending, 8);
        host::poll_all();
        for (c, port) in host::take_port_changes() {
            let connected = host::port_connected(c, port);
            let attached = host::slot_of_port(c, port).is_some();
            if connected && !attached {
                crate::sched::sleep_ms(50);
                // Eşlikçiye devredilen port artık bağlı görünmez: hata sayılmaz.
                if let Err(e) = enumerate(c, port)
                    && host::port_connected(c, port)
                {
                    crate::log!("usb: port {}: {}", port, e);
                }
            } else if !connected && attached {
                detach(c, port);
            }
            host::clear_port_changes(c, port);
        }
        hid::tick();
        hub::poll();
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    crate::kernel::initcall::self_test("usb", self_test())?;
    let controllers = host::probe();
    if controllers.is_empty() {
        let _ = write!(detail, "no USB controller");
        return Ok(());
    }
    crate::sched::sleep_ms(50);
    for &c in &controllers {
        scan_ports(c);
    }
    host::take_port_changes();
    crate::sched::spawn_detached("usbd", usbd).ok_or("could not start usbd")?;
    let list = devices();
    let names: Vec<&str> = controllers
        .iter()
        .map(|&c| host::controller_name(c))
        .collect();
    let _ = write!(detail, "{}, {} device(s)", names.join("+"), list.len());
    for a in &list {
        let _ = write!(detail, ", {}", a.drivers.join("+"));
    }
    Ok(())
}

fn self_test() -> bool {
    // Klavye + tablet içeren bileşik yapılandırma tanımlayıcısı.
    let config = [
        9, 2, 59, 0, 2, 1, 0, 0xa0, 50, // config
        9, 4, 0, 0, 1, 3, 1, 1, 0, // interface 0: boot keyboard
        9, 0x21, 0x11, 1, 0, 1, 0x22, 63, 0, // HID
        7, 5, 0x81, 3, 8, 0, 10, // EP1 IN interrupt
        9, 4, 1, 0, 1, 3, 0, 0, 0, // interface 1: tablet
        9, 0x21, 0x11, 1, 0, 1, 0x22, 74, 0, // HID
        7, 5, 0x82, 3, 8, 0, 4, // EP2 IN
    ];
    let (value, ifaces) = parse_configuration(&config);
    value == 1
        && ifaces.len() == 2
        && ifaces[0].protocol == 1
        && ifaces[0].hid_report_length == 63
        && ifaces[1].endpoints[0].address == 0x82
        && ifaces[1].endpoints[0].interval == 4
        && hid_report::self_test()
        && hid::self_test()
}

crate::initcall!(driver, 27, "USB (xHCI/EHCI)", init);
