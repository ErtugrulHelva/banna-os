//! UHCI (USB 1.1, Intel/VIA) ana denetleyicisi: düşük ve tam hızlı aygıtlar. Eski
//! bilgisayarlarda klavye/fare çoğunlukla EHCI'nin eşlikçisi olan bu denetleyiciye düşer.
//!
//! Çerçeve listesinin her girdisi kesme QH zincirine, o da kontrol/toplu QH zincirine
//! bağlanır. Her uç noktanın kendi QH'si vardır; aktarımlar TD zinciri olarak kurulup
//! yoklamayla beklenir. Veri geçişi (toggle) yazılımda tutulur. Yapılar 4 GiB altındadır.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use super::host::{CC_STALL, EndpointConfig, Handler, Route, Setup};
use super::{Dma, Speed};
use crate::arch::cpu::{inw, outl, outw};
use crate::arch::{pit, tsc};
use crate::drivers::pci::{self, Device as PciDevice};
use crate::sync::SpinLock;

const USBCMD: u16 = 0x00;
const USBSTS: u16 = 0x02;
const USBINTR: u16 = 0x04;
const FRNUM: u16 = 0x06;
const FRBASEADD: u16 = 0x08;
const PORTSC: u16 = 0x10;

const CMD_RUN: u16 = 1 << 0;
const CMD_HCRESET: u16 = 1 << 1;
const CMD_GRESET: u16 = 1 << 2;
const CMD_CF: u16 = 1 << 6;
const CMD_MAXP: u16 = 1 << 7;

const PORT_CONNECT: u16 = 1 << 0;
const PORT_CONNECT_CHANGE: u16 = 1 << 1;
const PORT_ENABLE: u16 = 1 << 2;
const PORT_ENABLE_CHANGE: u16 = 1 << 3;
const PORT_LOW_SPEED: u16 = 1 << 8;
const PORT_RESET: u16 = 1 << 9;
const PORT_WC: u16 = PORT_CONNECT_CHANGE | PORT_ENABLE_CHANGE;

const TERMINATE: u32 = 1;
const LINK_QH: u32 = 1 << 1;
const LINK_DEPTH: u32 = 1 << 2;

const TD_ACTIVE: u32 = 1 << 23;
const TD_STALLED: u32 = 1 << 22;
const TD_ERRORS: u32 = 0x7e << 16;
const TD_LOW_SPEED: u32 = 1 << 26;
const TD_SPD: u32 = 1 << 29;

const PID_SETUP: u32 = 0x2d;
const PID_IN: u32 = 0x69;
const PID_OUT: u32 = 0xe1;

const TD_BYTES: usize = 32;
const TDS_PER_PAGE: usize = 4096 / TD_BYTES;

/// QH: 0 yatay bağ, 1 öğe (TD) bağı.
struct Qh {
    dma: Dma,
}

impl Qh {
    fn new() -> Option<Self> {
        let dma = Dma::alloc(1)?;
        let qh = (dma.phys < 1 << 32).then_some(Self { dma })?;
        qh.set(0, TERMINATE);
        qh.set(1, TERMINATE);
        Some(qh)
    }

    fn get(&self, i: usize) -> u32 {
        unsafe { read_volatile((self.dma.virt as *const u32).add(i)) }
    }

    fn set(&self, i: usize, v: u32) {
        unsafe { write_volatile((self.dma.virt as *mut u32).add(i), v) }
    }

    fn phys(&self) -> u32 {
        self.dma.phys as u32
    }
}

/// Bir aktarımın TD'leri: (pid, tampon, uzunluk) paketleri, ardışık sayfalarda.
struct Chain {
    pages: Dma,
    count: usize,
    /// Her TD'nin istenen uzunluğu ve IN olup olmadığı.
    packets: Vec<(usize, bool)>,
}

impl Chain {
    fn td(&self, i: usize) -> *mut u32 {
        unsafe { self.pages.virt.add(i * TD_BYTES) as *mut u32 }
    }

    fn td_phys(&self, i: usize) -> u32 {
        (self.pages.phys + (i * TD_BYTES) as u64) as u32
    }

    fn status(&self, i: usize) -> u32 {
        unsafe { read_volatile(self.td(i).add(1)) }
    }

    /// TD'nin aktardığı bayt (ActLen n-1 kodlu).
    fn actual(&self, i: usize) -> usize {
        ((self.status(i) + 1) & 0x7ff) as usize
    }
}

struct Endpoint {
    qh: Qh,
    max_packet: u16,
    toggle: u32,
    interrupt: bool,
}

struct Pipe {
    chain: Chain,
    /// İşlenecek sıradaki TD.
    next: usize,
    buffer: Dma,
    size: usize,
    handler: Handler,
}

struct Dev {
    port: u8,
    route: u32,
    low_speed: bool,
    ep0: Qh,
    ep0_max: u16,
    control: Dma,
    endpoints: BTreeMap<u8, Endpoint>,
    pipes: BTreeMap<u8, Pipe>,
}

struct Controller {
    io: u16,
    ports: u8,
    interrupt_head: Qh,
    async_head: Qh,
    _frames: Dma,
    devices: BTreeMap<u8, Dev>,
    port_changes: Vec<u8>,
}

unsafe impl Send for Controller {}

static CONTROLLERS: SpinLock<Vec<Controller>> = SpinLock::new(Vec::new());

impl Controller {
    fn r16(&self, reg: u16) -> u16 {
        unsafe { inw(self.io + reg) }
    }

    fn w16(&self, reg: u16, v: u16) {
        unsafe { outw(self.io + reg, v) }
    }

    fn portsc(&self, port: u8) -> u16 {
        self.r16(PORTSC + 2 * (port as u16 - 1))
    }

    fn write_port(&self, port: u8, v: u16) {
        self.w16(PORTSC + 2 * (port as u16 - 1), v & !PORT_WC);
    }

    /// QH'yi `head` zincirinin başına ekler.
    fn link(head: &Qh, qh: &Qh) {
        qh.set(0, head.get(0));
        fence(Ordering::SeqCst);
        head.set(0, qh.phys() | LINK_QH);
    }

    /// QH'yi zincirden çıkarır; denetleyicinin bırakması için bir çerçeve bekler.
    fn unlink(head: &Qh, qh: &Qh) {
        let mut current = head.phys();
        for _ in 0..512 {
            let virt = crate::memory::phys_to_virt(current as u64) as *mut u32;
            let next = unsafe { read_volatile(virt) };
            if next & TERMINATE != 0 {
                return;
            }
            if next & !0xf == qh.phys() {
                unsafe { write_volatile(virt, qh.get(0)) };
                break;
            }
            current = next & !0xf;
        }
        crate::sched::sleep_ms(2);
    }
}

fn with<R>(i: usize, f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
    CONTROLLERS.lock().get_mut(i).map(f)
}

/// Paketleri (pid, tampon, uzunluk, toggle) TD zincirine çevirir.
fn build_chain(
    address: u8,
    endpoint: u8,
    low_speed: bool,
    spd: bool,
    packets: &[(u32, u64, usize, u32)],
) -> Option<Chain> {
    let pages = Dma::alloc(packets.len().div_ceil(TDS_PER_PAGE).max(1) as u64)?;
    if pages.phys >= 1 << 32 {
        return None;
    }
    let chain = Chain {
        pages,
        count: packets.len(),
        packets: packets.iter().map(|p| (p.2, p.0 == PID_IN)).collect(),
    };
    for (i, &(pid, buffer, len, toggle)) in packets.iter().enumerate() {
        let next = if i + 1 == packets.len() {
            TERMINATE
        } else {
            chain.td_phys(i + 1) | LINK_DEPTH
        };
        let mut status = TD_ACTIVE | (3 << 27);
        if low_speed {
            status |= TD_LOW_SPEED;
        }
        // Kısa paketle kuyruğu durdur (kesme kuyruklarında her paket ayrı rapordur).
        if pid == PID_IN && spd {
            status |= TD_SPD;
        }
        let maxlen = if len == 0 { 0x7ff } else { len as u32 - 1 };
        let token = pid
            | (address as u32) << 8
            | (endpoint as u32 & 0xf) << 15
            | toggle << 19
            | maxlen << 21;
        let td = chain.td(i);
        unsafe {
            write_volatile(td, next);
            write_volatile(td.add(1), status);
            write_volatile(td.add(2), token);
            write_volatile(td.add(3), buffer as u32);
        }
    }
    fence(Ordering::SeqCst);
    Some(chain)
}

/// Veriyi `max_packet`lik paketlere böler; her paket toggle'ı çevirir.
fn split(
    pid: u32,
    buffer: u64,
    len: usize,
    max_packet: u16,
    toggle: &mut u32,
) -> Vec<(u32, u64, usize, u32)> {
    let mp = max_packet.max(8) as usize;
    let mut out = Vec::new();
    let mut done = 0;
    loop {
        let take = (len - done).min(mp);
        out.push((pid, buffer + done as u64, take, *toggle));
        *toggle ^= 1;
        done += take;
        if done >= len {
            break;
        }
    }
    out
}

enum Wait {
    /// Tüm TD'ler bitti ya da kısa paketle bitti: (bitmiş TD sayısı, aktarılan bayt).
    Done(usize, usize),
    Error(u8),
}

/// Zinciri bekler; `status_td` kısa pakette atlanacak TD (kontrol durum aşaması).
fn wait_chain(
    qh: u32,
    chain: &Chain,
    data: core::ops::Range<usize>,
    status_td: Option<usize>,
    timeout_ms: u64,
) -> Wait {
    let start = tsc::now_ns();
    let deadline = pit::uptime_ms() + timeout_ms;
    let mut i = 0;
    let mut transferred = 0;
    while i < chain.count {
        let s = chain.status(i);
        if s & TD_ACTIVE != 0 {
            if pit::uptime_ms() > deadline {
                set_element(qh, TERMINATE);
                // Denetleyici TD'yi bıraksın (zincir belleği sonra serbest kalır).
                crate::sched::sleep_ms(2);
                return Wait::Error(0);
            }
            if tsc::now_ns() - start > 200_000 {
                crate::sched::sleep_ms(1);
            } else {
                core::hint::spin_loop();
            }
            continue;
        }
        if s & (TD_STALLED | TD_ERRORS) != 0 {
            set_element(qh, TERMINATE);
            return Wait::Error(
                if s & TD_STALLED != 0 && s & (TD_ERRORS & !TD_STALLED) == 0 {
                    CC_STALL
                } else {
                    4
                },
            );
        }
        let (wanted, input) = chain.packets[i];
        let got = chain.actual(i).min(wanted);
        if data.contains(&i) {
            transferred += got;
        }
        if input && got < wanted {
            // Kısa paket: kuyruk durur; kontrol aktarımında durum aşamasına geç.
            match status_td {
                Some(st) if i < st => {
                    set_element(qh, chain.td_phys(st));
                    i = st;
                    continue;
                }
                _ => {
                    set_element(qh, TERMINATE);
                    return Wait::Done(i + 1, transferred);
                }
            }
        }
        i += 1;
    }
    Wait::Done(chain.count, transferred)
}

// ---- denetleyici ----

fn init_controller(dev: &PciDevice) -> Result<Controller, &'static str> {
    dev.enable();
    let bar = dev.read(0x20);
    if bar & 1 == 0 {
        return Err("no I/O BAR");
    }
    let io = (bar & 0xffe0) as u16;
    // BIOS'un eski klavye öykünmesini (SMI) kapat.
    let legacy = dev.read(0xc0);
    dev.write(0xc0, (legacy & 0xffff_0000) | 0x8f00);

    let w = |reg: u16, v: u16| unsafe { outw(io + reg, v) };
    let r = |reg: u16| unsafe { inw(io + reg) };
    w(USBCMD, 0);
    w(USBCMD, CMD_GRESET);
    crate::sched::sleep_ms(20);
    w(USBCMD, 0);
    crate::sched::sleep_ms(5);
    w(USBCMD, CMD_HCRESET);
    let deadline = pit::uptime_ms() + 100;
    while r(USBCMD) & CMD_HCRESET != 0 {
        if pit::uptime_ms() > deadline {
            return Err("controller reset timed out");
        }
        crate::sched::sleep_ms(1);
    }

    let frames = Dma::alloc(1).ok_or("out of memory")?;
    let interrupt_head = Qh::new().ok_or("out of memory")?;
    let async_head = Qh::new().ok_or("out of memory")?;
    if frames.phys >= 1 << 32 {
        return Err("DMA memory above 4 GiB");
    }
    interrupt_head.set(0, async_head.phys() | LINK_QH);
    let list = frames.virt as *mut u32;
    for f in 0..1024 {
        unsafe { write_volatile(list.add(f), interrupt_head.phys() | LINK_QH) };
    }
    w(USBINTR, 0);
    w(FRNUM, 0);
    unsafe { outl(io + FRBASEADD, frames.phys as u32) };
    w(USBSTS, 0xffff);
    w(USBCMD, CMD_RUN | CMD_CF | CMD_MAXP);

    // Port sayısı: geçerli PORTSC'de bit 7 hep 1'dir.
    let mut ports = 0u8;
    while ports < 8 {
        let v = r(PORTSC + 2 * ports as u16);
        if v == 0xffff || v & 0x80 == 0 {
            break;
        }
        ports += 1;
    }
    crate::log!(
        "uhci: {:02x}:{:02x}.{} {:04x}:{:04x} io {:#x}, {} ports",
        dev.bus,
        dev.slot,
        dev.function,
        dev.vendor_id,
        dev.device_id,
        io,
        ports
    );
    Ok(Controller {
        io,
        ports,
        interrupt_head,
        async_head,
        _frames: frames,
        devices: BTreeMap::new(),
        port_changes: Vec::new(),
    })
}

pub fn probe() -> usize {
    let mut found = 0;
    for dev in pci::devices() {
        if dev.class != 0x0c || dev.subclass != 0x03 || dev.prog_if != 0x00 {
            continue;
        }
        match init_controller(&dev) {
            Ok(c) => {
                CONTROLLERS.lock().push(c);
                found += 1;
            }
            Err(e) => crate::log!(
                "uhci: {:02x}:{:02x}.{}: {}",
                dev.bus,
                dev.slot,
                dev.function,
                e
            ),
        }
    }
    found
}

pub fn max_ports(i: usize) -> u8 {
    with(i, |c| c.ports).unwrap_or(0)
}

pub fn port_connected(i: usize, port: u8) -> bool {
    with(i, |c| c.portsc(port) & PORT_CONNECT != 0).unwrap_or(false)
}

pub fn clear_port_changes(i: usize, port: u8) {
    with(i, |c| {
        let sc = c.portsc(port);
        c.w16(PORTSC + 2 * (port as u16 - 1), sc);
    });
}

pub fn take_port_changes() -> Vec<(usize, u8)> {
    let mut list = CONTROLLERS.lock();
    let mut out = Vec::new();
    for (i, c) in list.iter_mut().enumerate() {
        for port in 1..=c.ports {
            if c.portsc(port) & PORT_CONNECT_CHANGE != 0 && !c.port_changes.contains(&port) {
                c.port_changes.push(port);
            }
        }
        out.extend(c.port_changes.drain(..).map(|p| (i, p)));
    }
    out
}

pub fn reset_port(i: usize, port: u8) -> Option<Speed> {
    let sc = with(i, |c| c.portsc(port))?;
    if sc & PORT_CONNECT == 0 {
        return None;
    }
    with(i, |c| c.write_port(port, c.portsc(port) | PORT_RESET));
    crate::sched::sleep_ms(50);
    with(i, |c| c.write_port(port, c.portsc(port) & !PORT_RESET));
    crate::sched::sleep_ms(1);
    // Etkinleştirme birkaç denemede tutar.
    for _ in 0..10 {
        let sc = with(i, |c| c.portsc(port))?;
        if sc & PORT_CONNECT == 0 {
            return None;
        }
        if sc & PORT_ENABLE != 0 {
            break;
        }
        with(i, |c| c.write_port(port, sc | PORT_ENABLE));
        crate::sched::sleep_ms(10);
    }
    let sc = with(i, |c| c.portsc(port))?;
    if sc & PORT_ENABLE == 0 {
        crate::log!("uhci: port {} could not be enabled", port);
        return None;
    }
    clear_port_changes(i, port);
    crate::sched::sleep_ms(10);
    Some(if sc & PORT_LOW_SPEED != 0 {
        Speed::Low
    } else {
        Speed::Full
    })
}

fn free_address(c: &Controller) -> Option<u8> {
    (1..=127u8).find(|a| !c.devices.contains_key(a))
}

pub fn address_device(i: usize, at: Route, speed: Speed) -> Result<u8, &'static str> {
    let address = with(i, |c| -> Result<u8, &'static str> {
        let address = free_address(c).ok_or("no free USB address")?;
        let ep0 = Qh::new().ok_or("out of memory")?;
        let control = Dma::alloc(2).ok_or("out of memory")?;
        if control.phys >= 1 << 32 {
            return Err("DMA memory above 4 GiB");
        }
        Controller::link(&c.async_head, &ep0);
        c.devices.insert(
            address,
            Dev {
                port: at.root_port,
                route: at.route,
                low_speed: matches!(speed, Speed::Low),
                ep0,
                ep0_max: 8,
                control,
                endpoints: BTreeMap::new(),
                pipes: BTreeMap::new(),
            },
        );
        Ok(address)
    })
    .ok_or("no controller")??;
    // SET_ADDRESS adres 0'a gider; sonra aygıt yeni adresiyle konuşulur.
    let set_address = Setup {
        request_type: 0,
        request: 5,
        value: address as u16,
        index: 0,
        length: 0,
    };
    if control_at(i, address, 0, set_address, &mut []).is_err() {
        disable_slot(i, address);
        return Err("set address failed");
    }
    crate::sched::sleep_ms(2);
    Ok(address)
}

pub fn set_ep0_max_packet(i: usize, slot: u8, mps: u16) -> bool {
    with(i, |c| {
        c.devices.get_mut(&slot).map(|d| d.ep0_max = mps.max(8))
    })
    .flatten()
    .is_some()
}

pub fn disable_slot(i: usize, slot: u8) {
    with(i, |c| {
        if let Some(d) = c.devices.remove(&slot) {
            Controller::unlink(&c.async_head, &d.ep0);
            for ep in d.endpoints.values() {
                let head = if ep.interrupt {
                    &c.interrupt_head
                } else {
                    &c.async_head
                };
                Controller::unlink(head, &ep.qh);
            }
        }
    });
}

pub fn slot_at(i: usize, port: u8, route: u32) -> Option<u8> {
    with(i, |c| {
        c.devices
            .iter()
            .find(|(_, d)| d.port == port && d.route == route)
            .map(|(&a, _)| a)
    })
    .flatten()
}

pub fn control(i: usize, slot: u8, setup: Setup, data: &mut [u8]) -> Result<usize, u8> {
    control_at(i, slot, slot, setup, data)
}

/// Kontrol aktarımı; `wire` aygıtın şu anki adresi (SET_ADDRESS öncesi 0).
fn control_at(i: usize, slot: u8, wire: u8, setup: Setup, data: &mut [u8]) -> Result<usize, u8> {
    let len = (setup.length as usize).min(data.len()).min(4096);
    let input = setup.request_type & 0x80 != 0;
    let (chain, data_tds, status_td, qh_phys) = with(i, |c| {
        let d = c.devices.get(&slot)?;
        let raw = [
            setup.request_type,
            setup.request,
            setup.value as u8,
            (setup.value >> 8) as u8,
            setup.index as u8,
            (setup.index >> 8) as u8,
            len as u8,
            (len >> 8) as u8,
        ];
        unsafe {
            core::ptr::copy_nonoverlapping(raw.as_ptr(), d.control.virt, 8);
            if !input && len > 0 {
                core::ptr::copy_nonoverlapping(data.as_ptr(), d.control.virt.add(4096), len);
            }
        }
        let mut packets = alloc::vec![(PID_SETUP, d.control.phys, 8, 0)];
        let mut toggle = 1;
        if len > 0 {
            let pid = if input { PID_IN } else { PID_OUT };
            packets.extend(split(
                pid,
                d.control.phys + 4096,
                len,
                d.ep0_max,
                &mut toggle,
            ));
        }
        let data_tds = 1..packets.len();
        let status_td = packets.len();
        let status_pid = if input && len > 0 { PID_OUT } else { PID_IN };
        packets.push((status_pid, 0, 0, 1));
        let chain = build_chain(wire, 0, d.low_speed, true, &packets)?;
        d.ep0.set(1, chain.td_phys(0));
        Some((chain, data_tds, status_td, d.ep0.phys()))
    })
    .flatten()
    .ok_or(0u8)?;
    let result = wait_chain(qh_phys, &chain, data_tds, Some(status_td), 2000);
    match result {
        Wait::Error(e) => Err(e),
        Wait::Done(_, n) => {
            if input && n > 0 {
                with(i, |c| {
                    if let Some(d) = c.devices.get(&slot) {
                        unsafe {
                            core::ptr::copy_nonoverlapping(
                                d.control.virt.add(4096),
                                data.as_mut_ptr(),
                                n,
                            )
                        };
                    }
                });
            }
            Ok(n)
        }
    }
}

/// QH'nin öğe bağını yazar (QH, aygıt silinene kadar yaşar; kilit gerekmez).
fn set_element(qh_phys: u32, v: u32) {
    let p = crate::memory::phys_to_virt(qh_phys as u64) as *mut u32;
    unsafe { write_volatile(p.add(1), v) }
}

pub fn configure_endpoints(
    i: usize,
    slot: u8,
    _speed: Speed,
    endpoints: &[EndpointConfig],
) -> bool {
    with(i, |c| {
        let mut created = Vec::new();
        for ep in endpoints {
            created.push((
                ep.address,
                Endpoint {
                    qh: Qh::new()?,
                    max_packet: ep.max_packet & 0x7ff,
                    toggle: 0,
                    interrupt: ep.transfer == 3,
                },
            ));
        }
        for (_, ep) in &created {
            let head = if ep.interrupt {
                &c.interrupt_head
            } else {
                &c.async_head
            };
            Controller::link(head, &ep.qh);
        }
        c.devices.get_mut(&slot)?.endpoints.extend(created);
        Some(())
    })
    .flatten()
    .is_some()
}

/// Kesme hattında bekleyen TD sayısı: yoklamalar arasında bu kadar rapor birikebilir.
const PIPE_DEPTH: usize = 16;
/// Her TD'nin tampondaki payı.
const PIPE_SLOT: usize = 64;

fn arm_pipe(
    d: &Dev,
    slot: u8,
    address: u8,
    ep: &Endpoint,
    buffer: u64,
    size: usize,
) -> Option<Chain> {
    let len = size.min(ep.max_packet.max(1) as usize).min(PIPE_SLOT);
    let packets: Vec<(u32, u64, usize, u32)> = (0..PIPE_DEPTH)
        .map(|k| {
            (
                PID_IN,
                buffer + (k * PIPE_SLOT) as u64,
                len,
                ep.toggle ^ (k as u32 & 1),
            )
        })
        .collect();
    let chain = build_chain(slot, address & 0xf, d.low_speed, false, &packets)?;
    ep.qh.set(1, chain.td_phys(0));
    Some(chain)
}

pub fn start_interrupt_pipe(
    i: usize,
    slot: u8,
    address: u8,
    size: usize,
    handler: Handler,
) -> bool {
    with(i, |c| {
        let d = c.devices.get(&slot)?;
        let buffer = Dma::alloc(1)?;
        let size = size.clamp(1, PIPE_SLOT);
        let chain = arm_pipe(
            d,
            slot,
            address,
            d.endpoints.get(&address)?,
            buffer.phys,
            size,
        )?;
        c.devices.get_mut(&slot)?.pipes.insert(
            address,
            Pipe {
                chain,
                next: 0,
                buffer,
                size,
                handler,
            },
        );
        Some(())
    })
    .flatten()
    .is_some()
}

/// Biten kesme aktarımlarını sırayla işleyicilere verir; kuyruk bitince yeniden kurar.
pub fn poll_all() {
    let mut callbacks: Vec<(Handler, Vec<u8>)> = Vec::new();
    {
        let mut list = CONTROLLERS.lock();
        for c in list.iter_mut() {
            for (&slot, d) in c.devices.iter_mut() {
                let addresses: Vec<u8> = d.pipes.keys().copied().collect();
                for address in addresses {
                    let mut failed = false;
                    let mut completed = 0u32;
                    let pipe = d.pipes.get_mut(&address).unwrap();
                    while pipe.next < pipe.chain.count {
                        let status = pipe.chain.status(pipe.next);
                        if status & TD_ACTIVE != 0 {
                            break;
                        }
                        if status & (TD_STALLED | TD_ERRORS) != 0 {
                            failed = true;
                            break;
                        }
                        let n = pipe.chain.actual(pipe.next).min(pipe.size);
                        if n > 0 {
                            let at = unsafe { pipe.buffer.virt.add(pipe.next * PIPE_SLOT) };
                            let data = unsafe { core::slice::from_raw_parts(at, n) }.to_vec();
                            callbacks.push((pipe.handler.clone(), data));
                        }
                        completed += 1;
                        pipe.next += 1;
                    }
                    let exhausted = pipe.next >= pipe.chain.count;
                    let (buffer, size) = (pipe.buffer.phys, pipe.size);
                    let Some(ep) = d.endpoints.get_mut(&address) else {
                        continue;
                    };
                    ep.toggle ^= completed & 1;
                    if failed {
                        ep.toggle = 0;
                    }
                    if !(failed || exhausted) {
                        continue;
                    }
                    let ep = &d.endpoints[&address];
                    if let Some(chain) = arm_pipe(d, slot, address, ep, buffer, size) {
                        let pipe = d.pipes.get_mut(&address).unwrap();
                        pipe.chain = chain;
                        pipe.next = 0;
                    }
                }
            }
        }
    }
    for (handler, data) in callbacks {
        handler(&data);
    }
}

pub fn bulk(
    i: usize,
    slot: u8,
    address: u8,
    buffer: u64,
    len: usize,
    timeout_ms: u64,
) -> Result<usize, u8> {
    if buffer + len as u64 > 1 << 32 {
        return Err(0);
    }
    let input = address & 0x80 != 0;
    let (chain, qh_phys, start_toggle) = with(i, |c| {
        let d = c.devices.get_mut(&slot)?;
        let low = d.low_speed;
        let ep = d.endpoints.get_mut(&address)?;
        if ep.qh.get(1) & TERMINATE == 0 {
            return None;
        }
        let start = ep.toggle;
        let mut toggle = ep.toggle;
        let pid = if input { PID_IN } else { PID_OUT };
        let packets = split(pid, buffer, len, ep.max_packet, &mut toggle);
        let chain = build_chain(slot, address & 0xf, low, true, &packets)?;
        ep.qh.set(1, chain.td_phys(0));
        Some((chain, ep.qh.phys(), start))
    })
    .flatten()
    .ok_or(0u8)?;
    let result = wait_chain(qh_phys, &chain, 0..chain.count, None, timeout_ms);
    // Toggle: başarılı her paket bir kez çevirir.
    let completed = match result {
        Wait::Done(n, _) => n,
        Wait::Error(_) => (0..chain.count)
            .take_while(|&k| chain.status(k) & (TD_ACTIVE | TD_STALLED | TD_ERRORS) == 0)
            .count(),
    };
    with(i, |c| {
        if let Some(ep) = c
            .devices
            .get_mut(&slot)
            .and_then(|d| d.endpoints.get_mut(&address))
        {
            ep.toggle = start_toggle ^ (completed as u32 & 1);
            ep.qh.set(1, TERMINATE);
        }
    });
    match result {
        Wait::Done(_, n) => Ok(n),
        Wait::Error(e) => Err(e),
    }
}

/// Takılan uç noktayı temizler (toggle 0); aygıtta CLEAR_FEATURE ayrıca gönderilir.
pub fn reset_endpoint(i: usize, slot: u8, address: u8) -> bool {
    with(i, |c| {
        let ep = c.devices.get_mut(&slot)?.endpoints.get_mut(&address)?;
        ep.qh.set(1, TERMINATE);
        ep.toggle = 0;
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn take_event_pending() -> bool {
    false
}
