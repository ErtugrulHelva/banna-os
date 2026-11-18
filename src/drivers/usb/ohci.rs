//! OHCI (USB 1.1; AMD, NVIDIA, VIA, SiS, Apple) ana denetleyicisi: düşük ve tam hızlı
//! aygıtlar. UHCI gibi eski bilgisayarlarda EHCI'nin eşlikçisidir.
//!
//! Her uç noktanın bir ED'si vardır; ED'ler kontrol, toplu ya da kesme listesine bağlanır
//! (kesme listesi HCCA tablosunun 32 girdisinden de erişilir). Aktarımlar TD zinciri
//! olarak kuyruğa eklenip yoklamayla beklenir; veri geçişini ED (toggle carry) tutar.

use alloc::collections::{BTreeMap, VecDeque};
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use super::host::{CC_STALL, EndpointConfig, Handler, Route, Setup};
use super::{Dma, Speed};
use crate::arch::{pit, tsc};
use crate::drivers::pci::{self, Device as PciDevice};
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::sync::SpinLock;

const HC_CONTROL: u64 = 0x04;
const HC_COMMAND: u64 = 0x08;
const HC_INT_STATUS: u64 = 0x0c;
const HC_INT_DISABLE: u64 = 0x14;
const HC_HCCA: u64 = 0x18;
const HC_CONTROL_HEAD: u64 = 0x20;
const HC_CONTROL_CURRENT: u64 = 0x24;
const HC_BULK_HEAD: u64 = 0x28;
const HC_BULK_CURRENT: u64 = 0x2c;
const HC_FM_INTERVAL: u64 = 0x34;
const HC_FM_NUMBER: u64 = 0x3c;
const HC_PERIODIC_START: u64 = 0x40;
const HC_RH_DESC_A: u64 = 0x48;
const HC_RH_STATUS: u64 = 0x50;
const HC_RH_PORT: u64 = 0x54;

const CTRL_PLE: u32 = 1 << 2;
const CTRL_CLE: u32 = 1 << 4;
const CTRL_BLE: u32 = 1 << 5;
const CTRL_OPERATIONAL: u32 = 2 << 6;
const CTRL_IR: u32 = 1 << 8;
const CMD_RESET: u32 = 1 << 0;
const CMD_CLF: u32 = 1 << 1;
const CMD_BLF: u32 = 1 << 2;
const CMD_OCR: u32 = 1 << 3;

const PORT_CCS: u32 = 1 << 0;
const PORT_PES: u32 = 1 << 1;
const PORT_PRS: u32 = 1 << 4;
const PORT_PPS: u32 = 1 << 8;
const PORT_LSDA: u32 = 1 << 9;
const PORT_CSC: u32 = 1 << 16;
const PORT_PRSC: u32 = 1 << 20;
const PORT_CHANGES: u32 = 0x1f << 16;

const ED_SKIP: u32 = 1 << 14;
const ED_LOW_SPEED: u32 = 1 << 13;
const ED_HALTED: u32 = 1;
const ED_CARRY: u32 = 2;

const TD_ROUNDING: u32 = 1 << 18;
const TD_SETUP: u32 = 0;
const TD_OUT: u32 = 1 << 19;
const TD_IN: u32 = 2 << 19;
const TD_NO_INTERRUPT: u32 = 7 << 21;
const TD_DATA0: u32 = 2 << 24;
const TD_DATA1: u32 = 3 << 24;
const CC_NOT_ACCESSED: u32 = 0xf;
const CC_STALLED: u32 = 4;
const CC_DATA_UNDERRUN: u32 = 9;

const TD_BYTES: usize = 32;
/// Bir TD en fazla bir sayfa sınırı geçebilir: 4 KiB parçalar her zaman uyar.
const TD_MAX: usize = 4096;

/// ED: 0 denetim, 1 kuyruk sonu (TailP), 2 kuyruk başı (HeadP | C | H), 3 sonraki ED.
struct Ed {
    dma: Dma,
}

impl Ed {
    fn new(control: u32) -> Option<Self> {
        let dma = Dma::alloc(1)?;
        let ed = (dma.phys < 1 << 32).then_some(Self { dma })?;
        ed.set(0, control);
        Some(ed)
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

struct Chain {
    pages: Dma,
    /// Gerçek TD sayısı (sondaki boş TD hariç).
    count: usize,
    /// Her TD: (istenen uzunluk, tampon başlangıcı).
    packets: Vec<(usize, u32)>,
}

impl Chain {
    fn td(&self, i: usize) -> *mut u32 {
        unsafe { self.pages.virt.add(i * TD_BYTES) as *mut u32 }
    }

    fn td_phys(&self, i: usize) -> u32 {
        (self.pages.phys + (i * TD_BYTES) as u64) as u32
    }

    fn cc(&self, i: usize) -> u32 {
        unsafe { read_volatile(self.td(i)) >> 28 }
    }

    /// TD'nin aktardığı bayt: CBP 0 ise hepsi, değilse BE - CBP + 1 kalan.
    fn actual(&self, i: usize) -> usize {
        let (len, _) = self.packets[i];
        let cbp = unsafe { read_volatile(self.td(i).add(1)) };
        let be = unsafe { read_volatile(self.td(i).add(3)) };
        if cbp == 0 || len == 0 {
            len
        } else {
            len - ((be - cbp + 1) as usize).min(len)
        }
    }

    fn dummy(&self) -> u32 {
        self.td_phys(self.count)
    }
}

/// TD zinciri kurar: (yön, tampon, uzunluk, toggle: None = ED'den, Some(0/1)).
/// `rounding`: kısa paket hata sayılmaz (yalnızca zincirin son TD'sinde verilir).
fn build_chain(packets: &[(u32, u64, usize, Option<u32>, bool)]) -> Option<Chain> {
    let pages = Dma::alloc(((packets.len() + 1) * TD_BYTES).div_ceil(4096) as u64)?;
    if pages.phys >= 1 << 32 {
        return None;
    }
    let chain = Chain {
        pages,
        count: packets.len(),
        packets: packets.iter().map(|p| (p.2, p.1 as u32)).collect(),
    };
    for (i, &(dir, buffer, len, toggle, rounding)) in packets.iter().enumerate() {
        let mut control = dir | TD_NO_INTERRUPT | CC_NOT_ACCESSED << 28;
        if rounding {
            control |= TD_ROUNDING;
        }
        match toggle {
            Some(0) => control |= TD_DATA0,
            Some(_) => control |= TD_DATA1,
            None => {}
        }
        let td = chain.td(i);
        unsafe {
            write_volatile(td, control);
            write_volatile(td.add(1), if len == 0 { 0 } else { buffer as u32 });
            write_volatile(td.add(2), chain.td_phys(i + 1));
            write_volatile(
                td.add(3),
                if len == 0 {
                    0
                } else {
                    (buffer + len as u64 - 1) as u32
                },
            );
        }
    }
    fence(Ordering::SeqCst);
    Some(chain)
}

/// Zinciri ED'nin kuyruğuna koyar (toggle carry korunur). Yeni zincir eski zincirin
/// serbest kalan adresine düşebileceğinden ED, yazma sırasında atlatılır ve denetleyicinin
/// onu bir çerçeve boyunca okumadığından emin olunur.
fn queue(regs: u64, ed: &Ed, chain: &Chain) {
    let control = ed.get(0);
    ed.set(0, control | ED_SKIP);
    wait_frame(regs);
    let carry = ed.get(2) & ED_CARRY;
    ed.set(1, chain.dummy());
    ed.set(2, chain.td_phys(0) | carry);
    fence(Ordering::SeqCst);
    ed.set(0, control & !ED_SKIP);
}

/// Çerçeve numarası değişene kadar (en fazla ~2 ms) bekler.
fn wait_frame(regs: u64) {
    let frame = || unsafe { read_volatile((regs + HC_FM_NUMBER) as *const u32) } & 0xffff;
    let start = frame();
    let deadline = tsc::now_ns() + 2_000_000;
    while frame() == start && tsc::now_ns() < deadline {
        core::hint::spin_loop();
    }
}

/// ED kuyruğunu boşaltır (takılmışsa çözer).
fn clear_queue(ed: &Ed, carry: Option<bool>) {
    let c = match carry {
        Some(true) => ED_CARRY,
        Some(false) => 0,
        None => ed.get(2) & ED_CARRY,
    };
    let tail = ed.get(1);
    ed.set(2, tail | c);
}

struct Endpoint {
    ed: Ed,
    max_packet: u16,
    interrupt: bool,
}

struct Pipe {
    chain: Chain,
    next: usize,
    buffer: Dma,
    size: usize,
    handler: Handler,
}

struct Dev {
    port: u8,
    route: u32,
    low_speed: bool,
    ep0: Ed,
    control: Dma,
    endpoints: BTreeMap<u8, Endpoint>,
    pipes: BTreeMap<u8, Pipe>,
}

struct Controller {
    regs: u64,
    ports: u8,
    control_head: Ed,
    bulk_head: Ed,
    interrupt_head: Ed,
    _hcca: Dma,
    devices: BTreeMap<u8, Dev>,
    port_changes: Vec<u8>,
    /// Biten zincirler bir süre tutulur: TD adresleri hemen yeniden kullanılmaz (bazı
    /// denetleyiciler, ör. QEMU, eşzamansız paketi TD adresiyle izler).
    retired: VecDeque<Chain>,
}

unsafe impl Send for Controller {}

const RETIRED_CHAINS: usize = 16;

fn retire(i: usize, chain: Chain) {
    with(i, |c| {
        c.retired.push_back(chain);
        if c.retired.len() > RETIRED_CHAINS {
            c.retired.pop_front();
        }
    });
}

static CONTROLLERS: SpinLock<Vec<Controller>> = SpinLock::new(Vec::new());

#[derive(Clone, Copy, PartialEq)]
enum List {
    Control,
    Bulk,
    Interrupt,
}

impl Controller {
    fn r32(&self, off: u64) -> u32 {
        unsafe { read_volatile((self.regs + off) as *const u32) }
    }

    fn w32(&self, off: u64, v: u32) {
        unsafe { write_volatile((self.regs + off) as *mut u32, v) }
    }

    fn port(&self, port: u8) -> u32 {
        self.r32(HC_RH_PORT + 4 * (port as u64 - 1))
    }

    fn set_port(&self, port: u8, v: u32) {
        self.w32(HC_RH_PORT + 4 * (port as u64 - 1), v);
    }

    fn head(&self, list: List) -> &Ed {
        match list {
            List::Control => &self.control_head,
            List::Bulk => &self.bulk_head,
            List::Interrupt => &self.interrupt_head,
        }
    }

    fn link(&self, list: List, ed: &Ed) {
        let head = self.head(list);
        ed.set(3, head.get(3));
        fence(Ordering::SeqCst);
        head.set(3, ed.phys());
    }

    /// ED'yi atlatır, listeden çıkarır ve denetleyicinin bırakması için bekler.
    fn unlink(&self, list: List, ed: &Ed) {
        ed.set(0, ed.get(0) | ED_SKIP);
        crate::sched::sleep_ms(2);
        let mut current = self.head(list).phys();
        for _ in 0..512 {
            let virt = crate::memory::phys_to_virt(current as u64) as *mut u32;
            let next = unsafe { read_volatile(virt.add(3)) };
            if next == 0 {
                break;
            }
            if next == ed.phys() {
                unsafe { write_volatile(virt.add(3), ed.get(3)) };
                break;
            }
            current = next;
        }
        // Denetleyici bu ED'de duruyorsa listeyi baştan aldır.
        match list {
            List::Control if self.r32(HC_CONTROL_CURRENT) == ed.phys() => {
                self.w32(HC_CONTROL_CURRENT, 0)
            }
            List::Bulk if self.r32(HC_BULK_CURRENT) == ed.phys() => self.w32(HC_BULK_CURRENT, 0),
            _ => {}
        }
        crate::sched::sleep_ms(2);
    }

    fn kick(&self, list: List) {
        match list {
            List::Control => self.w32(HC_COMMAND, CMD_CLF),
            List::Bulk => self.w32(HC_COMMAND, CMD_BLF),
            List::Interrupt => {}
        }
    }
}

fn with<R>(i: usize, f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
    CONTROLLERS.lock().get_mut(i).map(f)
}

fn ed_control(address: u8, endpoint: u8, low_speed: bool, max_packet: u16) -> u32 {
    let mut v = address as u32 | (endpoint as u32 & 0xf) << 7 | ((max_packet as u32) & 0x7ff) << 16;
    if low_speed {
        v |= ED_LOW_SPEED;
    }
    v
}

enum Wait {
    Done(usize),
    Error(u8),
}

/// Denetleyicinin bitirdiği (kuyruktan çıkardığı) TD sayısı: ED'nin kuyruk başı nerede?
/// CC alanı her işlemden sonra yazıldığından tek başına bitişi göstermez.
fn retired(ed_head: u32, chain: &Chain) -> usize {
    let head = ed_head & !0xf;
    let first = chain.td_phys(0);
    if head < first || head > chain.dummy() {
        return chain.count;
    }
    ((head - first) as usize / TD_BYTES).min(chain.count)
}

/// Zinciri bekler; `data` veri TD'leri. Kısa paket (DataUnderrun) ED'yi durdurur:
/// toplu aktarım orada biter, kontrol aktarımında `status_td` yine de çalıştırılır.
fn wait_chain(
    ed_phys: u32,
    chain: &Chain,
    data: core::ops::Range<usize>,
    status_td: Option<usize>,
    timeout_ms: u64,
) -> Wait {
    let ed = crate::memory::phys_to_virt(ed_phys as u64) as *mut u32;
    let ed_get = |i: usize| unsafe { read_volatile(ed.add(i)) };
    let ed_set = |i: usize, v: u32| unsafe { write_volatile(ed.add(i), v) };
    let carry = || ed_get(2) & ED_CARRY;
    let start = tsc::now_ns();
    let deadline = pit::uptime_ms() + timeout_ms;
    let mut skipped = false;
    loop {
        let head = ed_get(2);
        let done = retired(head, chain);
        if done >= chain.count {
            break;
        }
        if head & ED_HALTED != 0 {
            // Durduran TD, kuyruk başından hemen önceki (bitmiş) TD'dir.
            let failed = done.checked_sub(1).map(|k| chain.cc(k)).unwrap_or(0);
            if failed == CC_DATA_UNDERRUN {
                match status_td {
                    Some(st) if done <= st && !skipped => {
                        ed_set(2, chain.td_phys(st) | carry());
                        skipped = true;
                        continue;
                    }
                    _ => {
                        ed_set(2, ed_get(1) | carry());
                        break;
                    }
                }
            }
            ed_set(2, ed_get(1) | carry());
            return Wait::Error(if failed == CC_STALLED { CC_STALL } else { 4 });
        }
        if pit::uptime_ms() > deadline {
            ed_set(0, ed_get(0) | ED_SKIP);
            crate::sched::sleep_ms(2);
            ed_set(2, ed_get(1) | carry());
            ed_set(0, ed_get(0) & !ED_SKIP);
            return Wait::Error(0);
        }
        if tsc::now_ns() - start > 200_000 {
            crate::sched::sleep_ms(1);
        } else {
            core::hint::spin_loop();
        }
    }
    let transferred = data
        .filter(|&i| chain.cc(i) != CC_NOT_ACCESSED)
        .map(|i| chain.actual(i))
        .sum();
    Wait::Done(transferred)
}

// ---- denetleyici ----

fn init_controller(dev: &PciDevice) -> Result<Controller, &'static str> {
    dev.enable();
    let bar = dev.bar(0).ok_or("no MMIO BAR")?;
    let regs = map_physical(bar, 0x1000, NO_CACHE).map_err(|_| "BAR mapping failed")? as u64;
    let r = |off: u64| unsafe { read_volatile((regs + off) as *const u32) };
    let w = |off: u64, v: u32| unsafe { write_volatile((regs + off) as *mut u32, v) };

    // BIOS/SMM'den devralma.
    if r(HC_CONTROL) & CTRL_IR != 0 {
        w(HC_COMMAND, CMD_OCR);
        let deadline = pit::uptime_ms() + 500;
        while r(HC_CONTROL) & CTRL_IR != 0 && pit::uptime_ms() < deadline {
            crate::sched::sleep_ms(1);
        }
    }
    w(HC_INT_DISABLE, 0x8000_007f);
    let interval = r(HC_FM_INTERVAL) & 0x3fff;
    let interval = if interval == 0 { 0x2edf } else { interval };
    w(HC_CONTROL, 0);
    crate::sched::sleep_ms(50);
    w(HC_COMMAND, CMD_RESET);
    let deadline = pit::uptime_ms() + 30;
    while r(HC_COMMAND) & CMD_RESET != 0 {
        if pit::uptime_ms() > deadline {
            return Err("controller reset timed out");
        }
        core::hint::spin_loop();
    }

    let hcca = Dma::alloc(1).ok_or("out of memory")?;
    let control_head = Ed::new(ED_SKIP).ok_or("out of memory")?;
    let bulk_head = Ed::new(ED_SKIP).ok_or("out of memory")?;
    let interrupt_head = Ed::new(ED_SKIP).ok_or("out of memory")?;
    if hcca.phys >= 1 << 32 {
        return Err("DMA memory above 4 GiB");
    }
    let table = hcca.virt as *mut u32;
    for i in 0..32 {
        unsafe { write_volatile(table.add(i), interrupt_head.phys()) };
    }
    w(HC_HCCA, hcca.phys as u32);
    w(HC_CONTROL_HEAD, control_head.phys());
    w(HC_BULK_HEAD, bulk_head.phys());
    let fsmps = (6 * (interval - 210)) / 7;
    // FIT biti her yazımda çevrilir.
    w(
        HC_FM_INTERVAL,
        ((r(HC_FM_INTERVAL) & 0x8000_0000) ^ 0x8000_0000) | fsmps << 16 | interval,
    );
    w(HC_PERIODIC_START, interval * 9 / 10);
    w(HC_INT_STATUS, 0xffff_ffff);
    w(
        HC_CONTROL,
        CTRL_OPERATIONAL | CTRL_PLE | CTRL_CLE | CTRL_BLE | 3,
    );

    let desc_a = r(HC_RH_DESC_A);
    let ports = (desc_a & 0xff).min(15) as u8;
    // Port gücü: genel güç (LPSC) ve port başına güç.
    w(HC_RH_STATUS, 1 << 16);
    for p in 0..ports as u64 {
        w(HC_RH_PORT + 4 * p, PORT_PPS);
    }
    crate::sched::sleep_ms(((desc_a >> 24) * 2).max(20) as u64);
    crate::log!(
        "ohci: {:02x}:{:02x}.{} {:04x}:{:04x} {} ports",
        dev.bus,
        dev.slot,
        dev.function,
        dev.vendor_id,
        dev.device_id,
        ports
    );
    Ok(Controller {
        regs,
        ports,
        control_head,
        bulk_head,
        interrupt_head,
        _hcca: hcca,
        devices: BTreeMap::new(),
        port_changes: Vec::new(),
        retired: VecDeque::new(),
    })
}

pub fn probe() -> usize {
    let mut found = 0;
    for dev in pci::devices() {
        if dev.class != 0x0c || dev.subclass != 0x03 || dev.prog_if != 0x10 {
            continue;
        }
        match init_controller(&dev) {
            Ok(c) => {
                CONTROLLERS.lock().push(c);
                found += 1;
            }
            Err(e) => crate::log!(
                "ohci: {:02x}:{:02x}.{}: {}",
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
    with(i, |c| c.port(port) & PORT_CCS != 0).unwrap_or(false)
}

pub fn clear_port_changes(i: usize, port: u8) {
    with(i, |c| c.set_port(port, PORT_CHANGES));
}

pub fn take_port_changes() -> Vec<(usize, u8)> {
    let mut list = CONTROLLERS.lock();
    let mut out = Vec::new();
    for (i, c) in list.iter_mut().enumerate() {
        for port in 1..=c.ports {
            if c.port(port) & PORT_CSC != 0 && !c.port_changes.contains(&port) {
                c.port_changes.push(port);
            }
        }
        out.extend(c.port_changes.drain(..).map(|p| (i, p)));
    }
    out
}

pub fn reset_port(i: usize, port: u8) -> Option<Speed> {
    if with(i, |c| c.port(port))? & PORT_CCS == 0 {
        return None;
    }
    with(i, |c| c.set_port(port, PORT_PRS));
    let deadline = pit::uptime_ms() + 100;
    while with(i, |c| c.port(port) & PORT_PRSC == 0)? {
        if pit::uptime_ms() > deadline {
            return None;
        }
        crate::sched::sleep_ms(1);
    }
    with(i, |c| c.set_port(port, PORT_PRSC));
    crate::sched::sleep_ms(10);
    let status = with(i, |c| c.port(port))?;
    if status & PORT_PES == 0 {
        crate::log!("ohci: port {} could not be enabled", port);
        return None;
    }
    clear_port_changes(i, port);
    Some(if status & PORT_LSDA != 0 {
        Speed::Low
    } else {
        Speed::Full
    })
}

fn free_address(c: &Controller) -> Option<u8> {
    (1..=127u8).find(|a| !c.devices.contains_key(a))
}

pub fn address_device(i: usize, at: Route, speed: Speed) -> Result<u8, &'static str> {
    let low = matches!(speed, Speed::Low);
    let address = with(i, |c| -> Result<u8, &'static str> {
        let address = free_address(c).ok_or("no free USB address")?;
        let ep0 = Ed::new(ed_control(0, 0, low, 8)).ok_or("out of memory")?;
        let control = Dma::alloc(2).ok_or("out of memory")?;
        if control.phys >= 1 << 32 {
            return Err("DMA memory above 4 GiB");
        }
        c.link(List::Control, &ep0);
        c.devices.insert(
            address,
            Dev {
                port: at.root_port,
                route: at.route,
                low_speed: low,
                ep0,
                control,
                endpoints: BTreeMap::new(),
                pipes: BTreeMap::new(),
            },
        );
        Ok(address)
    })
    .ok_or("no controller")??;
    let set_address = Setup {
        request_type: 0,
        request: 5,
        value: address as u16,
        index: 0,
        length: 0,
    };
    if control(i, address, set_address, &mut []).is_err() {
        disable_slot(i, address);
        return Err("set address failed");
    }
    crate::sched::sleep_ms(2);
    with(i, |c| {
        if let Some(d) = c.devices.get(&address) {
            d.ep0.set(0, (d.ep0.get(0) & !0x7f) | address as u32);
        }
    });
    Ok(address)
}

pub fn set_ep0_max_packet(i: usize, slot: u8, mps: u16) -> bool {
    with(i, |c| {
        let d = c.devices.get(&slot)?;
        d.ep0.set(
            0,
            (d.ep0.get(0) & !(0x7ff << 16)) | ((mps as u32 & 0x7ff) << 16),
        );
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn disable_slot(i: usize, slot: u8) {
    with(i, |c| {
        if let Some(d) = c.devices.remove(&slot) {
            c.unlink(List::Control, &d.ep0);
            for ep in d.endpoints.values() {
                c.unlink(
                    if ep.interrupt {
                        List::Interrupt
                    } else {
                        List::Bulk
                    },
                    &ep.ed,
                );
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
    let len = (setup.length as usize).min(data.len()).min(4096);
    let input = setup.request_type & 0x80 != 0;
    let (chain, ed_phys) = with(i, |c| {
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
        let mut packets = alloc::vec![(TD_SETUP, d.control.phys, 8, Some(0), false)];
        if len > 0 {
            let dir = if input { TD_IN } else { TD_OUT };
            packets.push((dir, d.control.phys + 4096, len, Some(1), true));
        }
        let status_dir = if input && len > 0 { TD_OUT } else { TD_IN };
        packets.push((status_dir, 0, 0, Some(1), false));
        let chain = build_chain(&packets)?;
        queue(c.regs, &d.ep0, &chain);
        c.kick(List::Control);
        Some((chain, d.ep0.phys()))
    })
    .flatten()
    .ok_or(0u8)?;
    let data_tds = if len > 0 { 1..2 } else { 1..1 };
    let status_td = chain.count - 1;
    let result = wait_chain(ed_phys, &chain, data_tds, Some(status_td), 2000);
    retire(i, chain);
    match result {
        Wait::Error(e) => Err(e),
        Wait::Done(n) => {
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

pub fn configure_endpoints(
    i: usize,
    slot: u8,
    _speed: Speed,
    endpoints: &[EndpointConfig],
) -> bool {
    with(i, |c| {
        let low = c.devices.get(&slot)?.low_speed;
        let mut created = Vec::new();
        for ep in endpoints {
            let mut control = ed_control(slot, ep.address & 0xf, low, ep.max_packet & 0x7ff);
            control |= if ep.address & 0x80 != 0 {
                2 << 11
            } else {
                1 << 11
            };
            created.push((
                ep.address,
                Endpoint {
                    ed: Ed::new(control)?,
                    max_packet: ep.max_packet & 0x7ff,
                    interrupt: ep.transfer == 3,
                },
            ));
        }
        for (_, ep) in &created {
            c.link(
                if ep.interrupt {
                    List::Interrupt
                } else {
                    List::Bulk
                },
                &ep.ed,
            );
        }
        c.devices.get_mut(&slot)?.endpoints.extend(created);
        Some(())
    })
    .flatten()
    .is_some()
}

const PIPE_DEPTH: usize = 16;
const PIPE_SLOT: usize = 64;

fn arm_pipe(regs: u64, ep: &Endpoint, buffer: u64, size: usize) -> Option<Chain> {
    let len = size.min(ep.max_packet.max(1) as usize).min(PIPE_SLOT);
    let packets: Vec<(u32, u64, usize, Option<u32>, bool)> = (0..PIPE_DEPTH)
        .map(|k| (TD_IN, buffer + (k * PIPE_SLOT) as u64, len, None, true))
        .collect();
    let chain = build_chain(&packets)?;
    queue(regs, &ep.ed, &chain);
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
        let regs = c.regs;
        let d = c.devices.get_mut(&slot)?;
        let buffer = Dma::alloc(1)?;
        let size = size.clamp(1, PIPE_SLOT);
        let chain = arm_pipe(regs, d.endpoints.get(&address)?, buffer.phys, size)?;
        d.pipes.insert(
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
            let regs = c.regs;
            let mut old = Vec::new();
            for d in c.devices.values_mut() {
                for (address, pipe) in d.pipes.iter_mut() {
                    let mut failed = false;
                    let Some(ep) = d.endpoints.get(address) else {
                        continue;
                    };
                    let done = retired(ep.ed.get(2), &pipe.chain);
                    while pipe.next < done {
                        let cc = pipe.chain.cc(pipe.next);
                        if cc != 0 && cc != CC_DATA_UNDERRUN {
                            failed = true;
                            break;
                        }
                        let n = pipe.chain.actual(pipe.next).min(pipe.size);
                        if n > 0 {
                            let at = unsafe { pipe.buffer.virt.add(pipe.next * PIPE_SLOT) };
                            let data = unsafe { core::slice::from_raw_parts(at, n) }.to_vec();
                            callbacks.push((pipe.handler.clone(), data));
                        }
                        pipe.next += 1;
                    }
                    failed |= ep.ed.get(2) & ED_HALTED != 0;
                    if !(failed || pipe.next >= pipe.chain.count) {
                        continue;
                    }
                    if failed {
                        clear_queue(&ep.ed, Some(false));
                    }
                    if let Some(chain) = arm_pipe(regs, ep, pipe.buffer.phys, pipe.size) {
                        old.push(core::mem::replace(&mut pipe.chain, chain));
                        pipe.next = 0;
                    }
                }
            }
            for chain in old {
                c.retired.push_back(chain);
                if c.retired.len() > RETIRED_CHAINS {
                    c.retired.pop_front();
                }
            }
        }
    }
    for (handler, data) in callbacks {
        handler(&data);
    }
}

/// Toplu aktarım. Veri en fazla `SEGMENT_PACKETS` paketlik parçalarla kuyruğa konur:
/// QEMU'nun OHCI'si bir çerçevede tek ED'de 32'den fazla paket işlerse denetleyiciyi
/// durdurur. Kısa IN paketi aktarımı bitirir.
pub fn bulk(
    i: usize,
    slot: u8,
    address: u8,
    buffer: u64,
    len: usize,
    timeout_ms: u64,
) -> Result<usize, u8> {
    const SEGMENT_PACKETS: usize = 32;
    if buffer + len as u64 > 1 << 32 {
        return Err(0);
    }
    let input = address & 0x80 != 0;
    let mps = with(i, |c| {
        c.devices
            .get(&slot)
            .and_then(|d| d.endpoints.get(&address))
            .map(|e| e.max_packet.max(8) as usize)
    })
    .flatten()
    .ok_or(0u8)?;
    let segment = (SEGMENT_PACKETS * mps).min(TD_MAX);
    let mut done = 0;
    loop {
        let take = (len - done).min(segment);
        let (chain, ed_phys) = with(i, |c| {
            let ep = c.devices.get(&slot)?.endpoints.get(&address)?;
            if ep.ed.get(2) & !0xf != ep.ed.get(1) & !0xf {
                return None;
            }
            let dir = if input { TD_IN } else { TD_OUT };
            let chain = build_chain(&[(dir, buffer + done as u64, take, None, true)])?;
            queue(c.regs, &ep.ed, &chain);
            c.kick(List::Bulk);
            Some((chain, ep.ed.phys()))
        })
        .flatten()
        .ok_or(0u8)?;
        let r = wait_chain(ed_phys, &chain, 0..1, None, timeout_ms);
        retire(i, chain);
        let n = match r {
            Wait::Done(n) => n,
            Wait::Error(e) => return Err(e),
        };
        done += n;
        if done >= len || n < take {
            return Ok(done);
        }
    }
}

/// Takılan uç noktayı temizler (toggle 0); aygıtta CLEAR_FEATURE ayrıca gönderilir.
pub fn reset_endpoint(i: usize, slot: u8, address: u8) -> bool {
    with(i, |c| {
        let ep = c.devices.get(&slot)?.endpoints.get(&address)?;
        clear_queue(&ep.ed, Some(false));
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn take_event_pending() -> bool {
    false
}
