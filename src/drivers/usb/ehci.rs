//! EHCI (USB 2.0) ana denetleyicisi: asenkron (kontrol, toplu) ve periyodik (kesme)
//! zamanlama, port sıfırlama ve hub arkasındaki düşük/tam hızlı aygıtlar için bölünmüş
//! işlemler (TT). Kök porttaki düşük/tam hızlı aygıt eşlikçi denetleyiciye (UHCI, OHCI)
//! bırakılır. Tüm yapılar 4 GiB altında tutulur.
//!
//! Aktarımlar kesme yerine yoklamayla beklenir; kesme uç noktaları `poll_all` ile
//! (usbd döngüsünde) okunup yeniden kurulur.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{Ordering, fence};

use super::host::{CC_STALL, EndpointConfig, Handler, Route, Setup};
use super::{Dma, Speed};
use crate::arch::{pit, tsc};
use crate::drivers::pci::{self, Device as PciDevice};
use crate::memory::PAGE_SIZE;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::sync::SpinLock;

const CMD_RUN: u32 = 1 << 0;
const CMD_RESET: u32 = 1 << 1;
const CMD_PERIODIC: u32 = 1 << 4;
const CMD_ASYNC: u32 = 1 << 5;
const CMD_DOORBELL: u32 = 1 << 6;
const STS_ASYNC_ADVANCE: u32 = 1 << 5;
const STS_HALTED: u32 = 1 << 12;

const PORT_CONNECT: u32 = 1 << 0;
const PORT_CONNECT_CHANGE: u32 = 1 << 1;
const PORT_ENABLE: u32 = 1 << 2;
const PORT_ENABLE_CHANGE: u32 = 1 << 3;
const PORT_OVERCURRENT_CHANGE: u32 = 1 << 5;
const PORT_RESET: u32 = 1 << 8;
const PORT_POWER: u32 = 1 << 12;
const PORT_OWNER: u32 = 1 << 13;
const PORT_WC: u32 = PORT_CONNECT_CHANGE | PORT_ENABLE_CHANGE | PORT_OVERCURRENT_CHANGE;

const TERMINATE: u32 = 1;
const TYPE_QH: u32 = 1 << 1;
const TOKEN_ACTIVE: u32 = 1 << 7;
const TOKEN_HALTED: u32 = 1 << 6;
const TOKEN_ERRORS: u32 = 0x3c;
const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;
const QTD_BYTES: usize = 64;
/// Bir qTD'nin taşıdığı en fazla veri (beş sayfa işaretçisi; ilk sayfa kaymalı olabilir).
const QTD_MAX: usize = 16 * 1024;

/// QH: 0 yatay bağ, 1 özellikler, 2 yetenekler, 3 geçerli qTD, 4.. qTD kaplaması.
struct Qh {
    dma: Dma,
}

impl Qh {
    fn new() -> Option<Self> {
        let dma = Dma::alloc(1)?;
        (dma.phys < 1 << 32).then_some(Self { dma })
    }

    fn word(&self, i: usize) -> *mut u32 {
        unsafe { (self.dma.virt as *mut u32).add(i) }
    }

    fn get(&self, i: usize) -> u32 {
        unsafe { read_volatile(self.word(i)) }
    }

    fn set(&self, i: usize, value: u32) {
        unsafe { write_volatile(self.word(i), value) }
    }

    fn phys(&self) -> u32 {
        self.dma.phys as u32
    }

    fn overlay_active(&self) -> bool {
        self.get(6) & TOKEN_ACTIVE != 0
    }
}

struct Endpoint {
    qh: Qh,
    interrupt: bool,
}

struct Pipe {
    qtd: Dma,
    buffer: Dma,
    size: usize,
    handler: Handler,
}

struct Dev {
    port: u8,
    route: u32,
    tt: (u8, u8),
    ep0: Qh,
    control: Dma,
    endpoints: BTreeMap<u8, Endpoint>,
    pipes: BTreeMap<u8, Pipe>,
}

struct Controller {
    op: u64,
    ports: u8,
    async_head: Qh,
    periodic_head: Qh,
    _frame_list: Dma,
    devices: BTreeMap<u8, Dev>,
    port_changes: Vec<u8>,
}

unsafe impl Send for Controller {}

static CONTROLLERS: SpinLock<Vec<Controller>> = SpinLock::new(Vec::new());

impl Controller {
    fn r32(&self, off: u64) -> u32 {
        unsafe { read_volatile((self.op + off) as *const u32) }
    }

    fn w32(&self, off: u64, value: u32) {
        unsafe { write_volatile((self.op + off) as *mut u32, value) }
    }

    fn portsc(&self, port: u8) -> u32 {
        self.r32(0x44 + 4 * (port as u64 - 1))
    }

    /// RW1C değişiklik bitlerini koruyarak (yanlışlıkla temizlemeden) yazar.
    fn write_port(&self, port: u8, value: u32) {
        self.w32(0x44 + 4 * (port as u64 - 1), value & !PORT_WC);
    }

    /// QH'yi asenkron halkaya ekler.
    fn link_async(&self, qh: &Qh) {
        qh.set(0, self.async_head.get(0));
        fence(Ordering::SeqCst);
        self.async_head.set(0, qh.phys() | TYPE_QH);
    }

    /// QH'yi asenkron halkadan çıkarır ve denetleyicinin bıraktığından emin olur.
    fn unlink_async(&self, qh: &Qh) {
        let mut current = self.async_head.phys();
        for _ in 0..256 {
            let virt = crate::memory::phys_to_virt(current as u64) as *mut u32;
            let next = unsafe { read_volatile(virt) };
            if next & !0x1f == qh.phys() {
                unsafe { write_volatile(virt, qh.get(0)) };
                break;
            }
            if next & !0x1f == self.async_head.phys() || next & TERMINATE != 0 {
                return;
            }
            current = next & !0x1f;
        }
        self.w32(0, self.r32(0) | CMD_DOORBELL);
        let deadline = pit::uptime_ms() + 50;
        while self.r32(4) & STS_ASYNC_ADVANCE == 0 && pit::uptime_ms() < deadline {
            core::hint::spin_loop();
        }
        self.w32(4, STS_ASYNC_ADVANCE);
    }

    fn link_periodic(&self, qh: &Qh) {
        qh.set(0, self.periodic_head.get(0));
        fence(Ordering::SeqCst);
        self.periodic_head.set(0, qh.phys() | TYPE_QH);
    }

    fn unlink_periodic(&self, qh: &Qh) {
        let mut current = self.periodic_head.phys();
        for _ in 0..256 {
            let virt = crate::memory::phys_to_virt(current as u64) as *mut u32;
            let next = unsafe { read_volatile(virt) };
            if next & TERMINATE != 0 {
                return;
            }
            if next & !0x1f == qh.phys() {
                unsafe { write_volatile(virt, qh.get(0)) };
                break;
            }
            current = next & !0x1f;
        }
        // Denetleyicinin en az bir çerçeve boyunca eski bağı görmediğinden emin ol.
        crate::sched::sleep_ms(2);
    }
}

fn with<R>(i: usize, f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
    CONTROLLERS.lock().get_mut(i).map(f)
}

fn eps_of(speed: Speed) -> u32 {
    match speed {
        Speed::Full => 0,
        Speed::Low => 1,
        _ => 2,
    }
}

/// QH özellik ve yetenek sözcükleri.
fn qh_words(
    address: u8,
    endpoint: u8,
    speed: Speed,
    max_packet: u16,
    control: bool,
    tt: (u8, u8),
    interrupt: bool,
) -> (u32, u32) {
    let high = matches!(speed, Speed::High);
    let mut chars = address as u32
        | ((endpoint as u32 & 0xf) << 8)
        | (eps_of(speed) << 12)
        | ((max_packet as u32 & 0x7ff) << 16);
    if control {
        chars |= 1 << 14;
        if !high {
            chars |= 1 << 27;
        }
    }
    if !interrupt {
        chars |= 4 << 28;
    }
    let mut caps = 1 << 30;
    if !high {
        caps |= (tt.0 as u32 & 0x7f) << 16 | (tt.1 as u32 & 0x7f) << 23;
    }
    if interrupt {
        caps |= 0x01;
        if !high {
            caps |= 0x1c << 8;
        }
    }
    (chars, caps)
}

fn init_qh(qh: &Qh, chars: u32, caps: u32) {
    qh.set(1, chars);
    qh.set(2, caps);
    qh.set(3, 0);
    qh.set(4, TERMINATE);
    qh.set(5, TERMINATE);
    qh.set(6, 0);
}

/// `len` baytın qTD'lere bölünmüş uzunlukları (boş aktarım tek, sıfır uzunluklu qTD).
fn chunks(len: usize) -> Vec<usize> {
    if len == 0 {
        return alloc::vec![0];
    }
    (0..len.div_ceil(QTD_MAX))
        .map(|i| (len - i * QTD_MAX).min(QTD_MAX))
        .collect()
}

/// qTD'leri bir sayfada kurar: (pid, tampon fiziksel adresi, uzunluk, toggle).
fn build_qtds(page: &Dma, stages: &[(u32, u64, usize, Option<u32>)]) -> Vec<u32> {
    let mut list: Vec<(u32, u64, usize, Option<u32>)> = Vec::new();
    for &(pid, buffer, len, toggle) in stages {
        if len == 0 {
            list.push((pid, buffer, 0, toggle));
            continue;
        }
        let mut done = 0;
        while done < len {
            let take = (len - done).min(QTD_MAX);
            list.push((
                pid,
                buffer + done as u64,
                take,
                if done == 0 { toggle } else { None },
            ));
            done += take;
        }
    }
    let mut phys = Vec::new();
    for (i, &(pid, buffer, len, toggle)) in list.iter().enumerate() {
        let base = page.phys + (i * QTD_BYTES) as u64;
        let words = unsafe { page.virt.add(i * QTD_BYTES) as *mut u32 };
        let last = i + 1 == list.len();
        let next = if last {
            TERMINATE
        } else {
            (base + QTD_BYTES as u64) as u32
        };
        let mut token = TOKEN_ACTIVE | (pid << 8) | (3 << 10) | ((len as u32) << 16);
        if let Some(t) = toggle {
            token |= t << 31;
        }
        if last {
            token |= 1 << 15;
        }
        unsafe {
            write_volatile(words, next);
            write_volatile(words.add(1), TERMINATE);
            write_volatile(words.add(2), token);
            let first = buffer as u32;
            write_volatile(words.add(3), first);
            for p in 1..5 {
                let page_addr = ((buffer & !0xfff) + p as u64 * PAGE_SIZE) as u32;
                write_volatile(words.add(3 + p), page_addr);
            }
            for p in 8..13 {
                write_volatile(words.add(p), 0);
            }
        }
        phys.push(base as u32);
    }
    phys
}

/// QH'ye qTD zincirini bağlar; toggle'ı QH tutuyorsa korunur.
fn arm(qh: &Qh, first: u32) {
    let toggle = qh.get(6) & (1 << 31);
    qh.set(5, TERMINATE);
    qh.set(6, toggle);
    fence(Ordering::SeqCst);
    qh.set(4, first);
}

/// Zincir bitene ya da hata verene kadar bekler; aktarılan bayt sayısını döndürür.
fn wait_chain(page: &Dma, count: usize, requested: &[usize], timeout_ms: u64) -> Result<usize, u8> {
    let start_ns = tsc::now_ns();
    let deadline = pit::uptime_ms() + timeout_ms;
    loop {
        let mut active = false;
        for i in 0..count {
            let token =
                unsafe { read_volatile((page.virt.add(i * QTD_BYTES) as *const u32).add(2)) };
            if token & TOKEN_HALTED != 0 {
                return Err(if token & TOKEN_ERRORS == 0 {
                    CC_STALL
                } else {
                    4
                });
            }
            if token & TOKEN_ACTIVE != 0 {
                active = true;
                break;
            }
        }
        if !active {
            break;
        }
        if pit::uptime_ms() > deadline {
            return Err(0);
        }
        if tsc::now_ns() - start_ns > 200_000 {
            crate::sched::sleep_ms(1);
        } else {
            core::hint::spin_loop();
        }
    }
    let mut total = 0;
    for (i, &req) in requested.iter().enumerate().take(count) {
        let token = unsafe { read_volatile((page.virt.add(i * QTD_BYTES) as *const u32).add(2)) };
        total += req - ((token >> 16) & 0x7fff) as usize;
    }
    Ok(total)
}

// ---- denetleyici ----

fn init_controller(dev: &PciDevice) -> Result<Controller, &'static str> {
    dev.enable();
    let bar = dev.bar(0).ok_or("no MMIO BAR")?;
    let base = map_physical(bar, 0x1000, NO_CACHE).map_err(|_| "BAR mapping failed")? as u64;
    let cap_length = unsafe { read_volatile(base as *const u8) } as u64;
    let hcs = unsafe { read_volatile((base + 4) as *const u32) };
    let hcc = unsafe { read_volatile((base + 8) as *const u32) };
    let op = base + cap_length;

    // BIOS'tan devralma (USBLEGSUP).
    let eecp = ((hcc >> 8) & 0xff) as u8;
    if eecp >= 0x40 {
        let legsup = dev.read(eecp);
        if legsup & 0xff == 1 {
            dev.write(eecp, legsup | (1 << 24));
            let deadline = pit::uptime_ms() + 1000;
            while dev.read(eecp) & (1 << 16) != 0 && pit::uptime_ms() < deadline {
                crate::sched::sleep_ms(5);
            }
            dev.write(eecp + 4, 0);
        }
    }

    let c = |off: u64| unsafe { read_volatile((op + off) as *const u32) };
    let w = |off: u64, v: u32| unsafe { write_volatile((op + off) as *mut u32, v) };
    w(0, c(0) & !CMD_RUN);
    let deadline = pit::uptime_ms() + 50;
    while c(4) & STS_HALTED == 0 && pit::uptime_ms() < deadline {
        core::hint::spin_loop();
    }
    w(0, CMD_RESET);
    let deadline = pit::uptime_ms() + 250;
    while c(0) & CMD_RESET != 0 {
        if pit::uptime_ms() > deadline {
            return Err("controller reset timed out");
        }
        crate::sched::sleep_ms(1);
    }

    let frame_list = Dma::alloc(1).ok_or("out of memory")?;
    let async_head = Qh::new().ok_or("out of memory")?;
    let periodic_head = Qh::new().ok_or("out of memory")?;
    if frame_list.phys >= 1 << 32 {
        return Err("DMA memory above 4 GiB");
    }
    // Asenkron baş: kendine bağlı, H biti açık.
    init_qh(&async_head, 1 << 15, 1 << 30);
    async_head.set(0, async_head.phys() | TYPE_QH);
    // Periyodik baş: tüm çerçeveler buna gelir; arkasına kesme QH'leri eklenir.
    init_qh(&periodic_head, 0, 1 << 30);
    periodic_head.set(0, TERMINATE);
    let frames = frame_list.virt as *mut u32;
    for i in 0..1024 {
        unsafe { write_volatile(frames.add(i), periodic_head.phys() | TYPE_QH) };
    }

    if hcc & 1 != 0 {
        w(0x10, 0);
    }
    w(8, 0);
    w(0x14, frame_list.phys as u32);
    w(0x18, async_head.phys());
    w(0, (8 << 16) | CMD_ASYNC | CMD_PERIODIC | CMD_RUN);
    w(0x40, 1);
    crate::sched::sleep_ms(5);

    let ports = (hcs & 0xf) as u8;
    let controller = Controller {
        op,
        ports,
        async_head,
        periodic_head,
        _frame_list: frame_list,
        devices: BTreeMap::new(),
        port_changes: Vec::new(),
    };
    if hcs & (1 << 4) != 0 {
        for port in 1..=ports {
            let sc = controller.portsc(port);
            controller.write_port(port, sc | PORT_POWER);
        }
        crate::sched::sleep_ms(20);
    }
    crate::log!(
        "ehci: {:02x}:{:02x}.{} {:04x}:{:04x} {} ports{}",
        dev.bus,
        dev.slot,
        dev.function,
        dev.vendor_id,
        dev.device_id,
        ports,
        if hcc & 1 != 0 { ", 64-bit" } else { "" }
    );
    Ok(controller)
}

pub fn probe() -> usize {
    let mut found = 0;
    for dev in pci::devices() {
        if dev.class != 0x0c || dev.subclass != 0x03 || dev.prog_if != 0x20 {
            continue;
        }
        match init_controller(&dev) {
            Ok(c) => {
                CONTROLLERS.lock().push(c);
                found += 1;
            }
            Err(e) => crate::log!(
                "ehci: {:02x}:{:02x}.{}: {}",
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
    with(i, |c| {
        c.portsc(port) & PORT_CONNECT != 0 && c.portsc(port) & PORT_OWNER == 0
    })
    .unwrap_or(false)
}

pub fn clear_port_changes(i: usize, port: u8) {
    with(i, |c| {
        // PE'ye 0 yazmak portu kapatır: okunan değer aynen geri yazılır, yalnızca
        // 1 olan değişiklik bitleri temizlenir.
        let sc = c.portsc(port);
        c.w32(0x44 + 4 * (port as u64 - 1), sc);
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

pub fn take_event_pending() -> bool {
    false
}

/// Portu sıfırlar. Yüksek hızlı olmayan aygıt eşlikçi denetleyiciye bırakılır (`None`).
pub fn reset_port(i: usize, port: u8) -> Option<Speed> {
    let sc = with(i, |c| c.portsc(port))?;
    if sc & PORT_CONNECT == 0 {
        return None;
    }
    let release = |why: &str| {
        crate::log!(
            "ehci: port {} {}: handed to companion controller",
            port,
            why
        );
        with(i, |c| c.write_port(port, c.portsc(port) | PORT_OWNER));
        None
    };
    if (sc >> 10) & 3 == 1 {
        return release("low-speed device");
    }
    with(i, |c| {
        c.write_port(port, (c.portsc(port) & !PORT_ENABLE) | PORT_RESET)
    });
    crate::sched::sleep_ms(50);
    with(i, |c| c.write_port(port, c.portsc(port) & !PORT_RESET));
    let deadline = pit::uptime_ms() + 100;
    while with(i, |c| c.portsc(port) & PORT_RESET != 0)? {
        if pit::uptime_ms() > deadline {
            return None;
        }
        crate::sched::sleep_ms(1);
    }
    crate::sched::sleep_ms(10);
    if with(i, |c| c.portsc(port) & PORT_ENABLE)? == 0 {
        return release("full-speed device");
    }
    clear_port_changes(i, port);
    Some(Speed::High)
}

fn free_address(c: &Controller) -> Option<u8> {
    (1..=127u8).find(|a| !c.devices.contains_key(a))
}

pub fn address_device(i: usize, at: Route, speed: Speed) -> Result<u8, &'static str> {
    let address = with(i, |c| -> Result<u8, &'static str> {
        let address = free_address(c).ok_or("no free USB address")?;
        let ep0 = Qh::new().ok_or("out of memory")?;
        let control = Dma::alloc(3).ok_or("out of memory")?;
        if control.phys >= 1 << 32 {
            return Err("DMA memory above 4 GiB");
        }
        let tt = (at.tt_slot, at.tt_port);
        let mps = match speed {
            Speed::High => 64,
            _ => 8,
        };
        let (chars, caps) = qh_words(0, 0, speed, mps, true, tt, false);
        init_qh(&ep0, chars, caps);
        c.link_async(&ep0);
        c.devices.insert(
            address,
            Dev {
                port: at.root_port,
                route: at.route,
                tt,
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
            let chars = d.ep0.get(1) & !0x7f;
            d.ep0.set(1, chars | address as u32);
        }
    });
    Ok(address)
}

pub fn set_ep0_max_packet(i: usize, slot: u8, mps: u16) -> bool {
    with(i, |c| {
        let d = c.devices.get(&slot)?;
        let chars = d.ep0.get(1) & !(0x7ff << 16);
        d.ep0.set(1, chars | ((mps as u32 & 0x7ff) << 16));
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn disable_slot(i: usize, slot: u8) {
    with(i, |c| {
        if let Some(d) = c.devices.remove(&slot) {
            c.unlink_async(&d.ep0);
            for ep in d.endpoints.values() {
                if ep.interrupt {
                    c.unlink_periodic(&ep.qh);
                } else {
                    c.unlink_async(&ep.qh);
                }
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

/// Kontrol aktarımı: SETUP, isteğe bağlı veri, durum aşamaları (veri en fazla 4 KiB).
pub fn control(i: usize, slot: u8, setup: Setup, data: &mut [u8]) -> Result<usize, u8> {
    let len = (setup.length as usize).min(data.len()).min(4096);
    let input = setup.request_type & 0x80 != 0;
    let (page, count, requested) = with(i, |c| {
        let d = c.devices.get(&slot)?;
        let control = &d.control;
        unsafe {
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
            core::ptr::copy_nonoverlapping(raw.as_ptr(), control.virt, 8);
            if !input && len > 0 {
                core::ptr::copy_nonoverlapping(data.as_ptr(), control.virt.add(4096), len);
            }
        }
        let data_phys = control.phys + 4096;
        let status_pid = if input && len > 0 { PID_OUT } else { PID_IN };
        let mut stages = alloc::vec![(PID_SETUP, control.phys, 8, Some(0))];
        if len > 0 {
            stages.push((
                if input { PID_IN } else { PID_OUT },
                data_phys,
                len,
                Some(1),
            ));
        }
        stages.push((status_pid, control.phys + 8192, 0, Some(1)));
        let page = Dma::alloc(1)?;
        let qtds = build_qtds(&page, &stages);
        arm(&d.ep0, qtds[0]);
        let mut requested = alloc::vec![8usize];
        if len > 0 {
            requested.extend(chunks(len));
        }
        requested.push(0);
        Some((page, qtds.len(), requested))
    })
    .flatten()
    .ok_or(0u8)?;
    let result = wait_chain(&page, count, &requested, 2000);
    if result.is_err() {
        with(i, |c| {
            if let Some(d) = c.devices.get(&slot) {
                arm(&d.ep0, TERMINATE);
            }
        });
    }
    result?;
    let transferred = if len > 0 {
        let data_qtds = requested.len() - 2;
        let mut n = 0;
        for k in 0..data_qtds {
            let token =
                unsafe { read_volatile((page.virt.add((k + 1) * QTD_BYTES) as *const u32).add(2)) };
            n += requested[k + 1] - ((token >> 16) & 0x7fff) as usize;
        }
        n
    } else {
        0
    };
    if input && transferred > 0 {
        with(i, |c| {
            if let Some(d) = c.devices.get(&slot) {
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        d.control.virt.add(4096),
                        data.as_mut_ptr(),
                        transferred,
                    )
                };
            }
        });
    }
    Ok(transferred)
}

pub fn configure_endpoints(i: usize, slot: u8, speed: Speed, endpoints: &[EndpointConfig]) -> bool {
    with(i, |c| {
        let address = slot;
        let tt = c.devices.get(&slot)?.tt;
        let mut created = Vec::new();
        for ep in endpoints {
            let qh = Qh::new()?;
            let interrupt = ep.transfer == 3;
            let (chars, caps) = qh_words(
                address,
                ep.address & 0xf,
                speed,
                ep.max_packet & 0x7ff,
                false,
                tt,
                interrupt,
            );
            init_qh(&qh, chars, caps);
            created.push((ep.address, Endpoint { qh, interrupt }));
        }
        for (_, ep) in &created {
            if ep.interrupt {
                c.link_periodic(&ep.qh);
            } else {
                c.link_async(&ep.qh);
            }
        }
        let d = c.devices.get_mut(&slot)?;
        d.endpoints.extend(created);
        Some(())
    })
    .flatten()
    .is_some()
}

pub fn start_interrupt_pipe(
    i: usize,
    slot: u8,
    address: u8,
    size: usize,
    handler: Handler,
) -> bool {
    with(i, |c| {
        let d = c.devices.get_mut(&slot)?;
        let ep = d.endpoints.get(&address)?;
        let qtd = Dma::alloc(1)?;
        let buffer = Dma::alloc(1)?;
        let size = size.clamp(1, 4096);
        let first = build_qtds(&qtd, &[(PID_IN, buffer.phys, size, None)]);
        arm(&ep.qh, first[0]);
        d.pipes.insert(
            address,
            Pipe {
                qtd,
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

/// Biten kesme aktarımlarını işleyicilerine verir ve yeniden kurar.
pub fn poll_all() {
    let mut callbacks: Vec<(Handler, Vec<u8>)> = Vec::new();
    {
        let list = CONTROLLERS.lock();
        for c in list.iter() {
            for d in c.devices.values() {
                for (address, pipe) in &d.pipes {
                    let token = unsafe { read_volatile((pipe.qtd.virt as *const u32).add(2)) };
                    if token & TOKEN_ACTIVE != 0 {
                        continue;
                    }
                    let Some(ep) = d.endpoints.get(address) else {
                        continue;
                    };
                    if token & TOKEN_HALTED == 0 {
                        let n = pipe.size - ((token >> 16) & 0x7fff) as usize;
                        let data =
                            unsafe { core::slice::from_raw_parts(pipe.buffer.virt, n) }.to_vec();
                        if n > 0 {
                            callbacks.push((pipe.handler.clone(), data));
                        }
                    } else {
                        ep.qh.set(6, 0);
                    }
                    let first =
                        build_qtds(&pipe.qtd, &[(PID_IN, pipe.buffer.phys, pipe.size, None)]);
                    arm(&ep.qh, first[0]);
                }
            }
        }
    }
    for (handler, data) in callbacks {
        handler(&data);
    }
}

/// Toplu aktarım; `buffer` 4 GiB altında fiziksel adres olmalı.
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
    let (page, count, requested) = with(i, |c| {
        let d = c.devices.get(&slot)?;
        let ep = d.endpoints.get(&address)?;
        if ep.qh.overlay_active() {
            return None;
        }
        let page = Dma::alloc(1)?;
        let qtds = build_qtds(
            &page,
            &[(if input { PID_IN } else { PID_OUT }, buffer, len, None)],
        );
        let requested = chunks(len);
        arm(&ep.qh, qtds[0]);
        Some((page, qtds.len(), requested))
    })
    .flatten()
    .ok_or(0u8)?;
    let result = wait_chain(&page, count, &requested, timeout_ms);
    if result.is_err() {
        with(i, |c| {
            if let Some(ep) = c.devices.get(&slot).and_then(|d| d.endpoints.get(&address)) {
                arm(&ep.qh, TERMINATE);
            }
        });
    }
    result
}

/// Takılan uç noktayı temizler (toggle 0); aygıtta CLEAR_FEATURE ayrıca gönderilir.
pub fn reset_endpoint(i: usize, slot: u8, address: u8) -> bool {
    with(i, |c| {
        let ep = c.devices.get(&slot)?.endpoints.get(&address)?;
        ep.qh.set(4, TERMINATE);
        ep.qh.set(6, 0);
        Some(())
    })
    .flatten()
    .is_some()
}
