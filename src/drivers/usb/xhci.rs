//! xHCI (USB 3.x) sunucu denetleyicisi: BIOS devri, komut/olay halkaları,
//! port sıfırlama, aygıt adresleme, kontrol, kesme (interrupt) ve bulk aktarımları.
//!
//! Tüm denetleyici durumu `CONTROLLERS` kilidinin arkasındadır. Bloklayan
//! işlemler kilidi bırakıp olay halkasını kendileri de işleyerek bekler; böylece
//! kesme hattı çalışmasa bile (yoklama) ilerleme olur.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, Ordering, fence};

use super::host::{
    CC_SHORT_PACKET, CC_STALL, CC_SUCCESS, EndpointConfig, Handler, HubInfo, IsoFill, Route, Setup,
};
use super::{Dma, Speed};
use crate::arch::{pit, tsc};
use crate::drivers::pci::{self, Device as PciDevice};
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_ISOCH: u32 = 5;
const TRB_LINK: u32 = 6;
/// Isoch TRB: TD'yi ilk uygun çerçevede başlat (Start Isoch ASAP).
const TRB_SIA: u32 = 1 << 31;
const CC_RING_UNDERRUN: u8 = 14;
const CC_RING_OVERRUN: u8 = 15;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_ENDPOINT: u32 = 12;
const TRB_EVALUATE_CONTEXT: u32 = 13;
const TRB_RESET_ENDPOINT: u32 = 14;
const TRB_SET_DEQUEUE: u32 = 16;
const EVENT_TRANSFER: u32 = 32;
const EVENT_COMMAND: u32 = 33;
const EVENT_PORT: u32 = 34;

const TRB_CYCLE: u32 = 1 << 0;
const TRB_TOGGLE: u32 = 1 << 1;
const TRB_ISP: u32 = 1 << 2;
const TRB_CHAIN: u32 = 1 << 4;
const TRB_IOC: u32 = 1 << 5;
const TRB_IDT: u32 = 1 << 6;
const TRB_DIR_IN: u32 = 1 << 16;

const USBCMD_RUN: u32 = 1 << 0;
const USBCMD_RESET: u32 = 1 << 1;
const USBCMD_INTE: u32 = 1 << 2;
const USBSTS_HALTED: u32 = 1 << 0;
const USBSTS_EINT: u32 = 1 << 3;
const USBSTS_NOT_READY: u32 = 1 << 11;

const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
const PORT_LWS: u32 = 1 << 16;
const PORT_CHANGE_BITS: u32 = 0x7f << 17;
const PORT_PRC: u32 = 1 << 21;

const RING_TRBS: usize = 256;
const EVENT_TRBS: usize = 256;

type Trb = [u32; 4];

/// Üretici halkası (komut ya da transfer); sonunda kendine dönen bir Link TRB'si vardır.
struct Ring {
    dma: Dma,
    enqueue: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Option<Self> {
        let dma = Dma::alloc(1)?;
        let mut ring = Self {
            dma,
            enqueue: 0,
            cycle: 1,
        };
        let phys = ring.dma.phys;
        ring.write(
            RING_TRBS - 1,
            [
                phys as u32,
                (phys >> 32) as u32,
                0,
                TRB_LINK << 10 | TRB_TOGGLE,
            ],
        );
        Some(ring)
    }

    fn slot(&self, i: usize) -> *mut u32 {
        unsafe { (self.dma.virt as *mut u32).add(i * 4) }
    }

    fn write(&mut self, i: usize, trb: Trb) {
        let p = self.slot(i);
        unsafe {
            write_volatile(p, trb[0]);
            write_volatile(p.add(1), trb[1]);
            write_volatile(p.add(2), trb[2]);
            fence(Ordering::SeqCst);
            write_volatile(p.add(3), trb[3]);
        }
    }

    /// TRB'yi ekler (cycle biti eklenir); TRB'nin fiziksel adresini döndürür.
    fn push(&mut self, trb: Trb) -> u64 {
        let i = self.enqueue;
        let phys = self.dma.phys + (i * 16) as u64;
        self.write(
            i,
            [trb[0], trb[1], trb[2], (trb[3] & !TRB_CYCLE) | self.cycle],
        );
        self.enqueue += 1;
        if self.enqueue == RING_TRBS - 1 {
            let link_phys = self.dma.phys;
            let chain = trb[3] & TRB_CHAIN;
            self.write(
                RING_TRBS - 1,
                [
                    link_phys as u32,
                    (link_phys >> 32) as u32,
                    0,
                    TRB_LINK << 10 | TRB_TOGGLE | chain | self.cycle,
                ],
            );
            self.cycle ^= 1;
            self.enqueue = 0;
        }
        phys
    }

    /// Set TR Dequeue için: sıradaki boş TRB ve döngü biti.
    fn dequeue_pointer(&self) -> u64 {
        (self.dma.phys + (self.enqueue * 16) as u64) | self.cycle as u64
    }
}

struct EventRing {
    dma: Dma,
    erst: Dma,
    dequeue: usize,
    cycle: u32,
}

impl EventRing {
    fn new() -> Option<Self> {
        let dma = Dma::alloc(1)?;
        let erst = Dma::alloc(1)?;
        unsafe {
            let e = erst.virt as *mut u32;
            write_volatile(e, dma.phys as u32);
            write_volatile(e.add(1), (dma.phys >> 32) as u32);
            write_volatile(e.add(2), EVENT_TRBS as u32);
            write_volatile(e.add(3), 0);
        }
        Some(Self {
            dma,
            erst,
            dequeue: 0,
            cycle: 1,
        })
    }

    fn pop(&mut self) -> Option<Trb> {
        let p = unsafe { (self.dma.virt as *const u32).add(self.dequeue * 4) };
        let control = unsafe { read_volatile(p.add(3)) };
        if control & TRB_CYCLE != self.cycle {
            return None;
        }
        fence(Ordering::SeqCst);
        let trb = unsafe {
            [
                read_volatile(p),
                read_volatile(p.add(1)),
                read_volatile(p.add(2)),
                control,
            ]
        };
        self.dequeue += 1;
        if self.dequeue == EVENT_TRBS {
            self.dequeue = 0;
            self.cycle ^= 1;
        }
        Some(trb)
    }

    fn dequeue_phys(&self) -> u64 {
        self.dma.phys + (self.dequeue * 16) as u64
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Done {
    pub code: u8,
    pub residual: u32,
    pub slot: u8,
}

/// Sürekli yeniden kuyruğa alınan kesme (interrupt IN) aktarımı.
struct Pipe {
    buffer: Dma,
    size: usize,
    pending: u64,
    handler: Handler,
}

struct Slot {
    port: u8,
    route: u32,
    hub: Option<HubInfo>,
    out_ctx: Dma,
    in_ctx: Dma,
    control_buffer: Dma,
    rings: BTreeMap<u8, Ring>,
    pipes: BTreeMap<u8, Pipe>,
    iso: BTreeMap<u8, IsoStream>,
}

/// Sürekli isochronous akış: her TD tamamlanınca tamponu `fill` doldurur (OUT) ya da
/// gelen veriyi alır (IN) ve TD halkaya geri konur.
struct IsoStream {
    buffers: Vec<Dma>,
    bytes: usize,
    /// TRB fiziksel adresi → tampon numarası.
    pending: BTreeMap<u64, usize>,
    fill: IsoFill,
}

pub struct Controller {
    pub index: usize,
    op: u64,
    rt: u64,
    db: u64,
    pub max_slots: u8,
    pub max_ports: u8,
    ctx_size: usize,
    dcbaa: Dma,
    _scratchpad: Vec<Dma>,
    command: Ring,
    events: EventRing,
    slots: BTreeMap<u8, Slot>,
    done: BTreeMap<u64, Done>,
    port_changes: Vec<u8>,
    usb3_ports: Vec<(u8, u8)>,
    irq: u8,
}

unsafe impl Send for Controller {}

static CONTROLLERS: SpinLock<Vec<Controller>> = SpinLock::new(Vec::new());
static EVENT_PENDING: AtomicBool = AtomicBool::new(false);
static COMPLETIONS: WaitQueue = WaitQueue::new();
static COMPLETION_GEN: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

impl Controller {
    fn r32(&self, addr: u64) -> u32 {
        unsafe { read_volatile(addr as *const u32) }
    }

    fn w32(&self, addr: u64, value: u32) {
        unsafe { write_volatile(addr as *mut u32, value) }
    }

    fn w64(&self, addr: u64, value: u64) {
        self.w32(addr, value as u32);
        self.w32(addr + 4, (value >> 32) as u32);
    }

    fn portsc_addr(&self, port: u8) -> u64 {
        self.op + 0x400 + 0x10 * (port as u64 - 1)
    }

    pub fn portsc(&self, port: u8) -> u32 {
        self.r32(self.portsc_addr(port))
    }

    /// PORTSC'ye yazarken PED'i (1 yazmak portu kapatır) ve RW1C bitlerini korur.
    fn write_portsc(&self, port: u8, set: u32) {
        let neutral = self.portsc(port) & !(PORT_PED | PORT_LWS | PORT_CHANGE_BITS);
        self.w32(self.portsc_addr(port), neutral | set);
    }

    fn ring_doorbell(&self, slot: u8, target: u8) {
        fence(Ordering::SeqCst);
        self.w32(self.db + 4 * slot as u64, target as u32);
    }

    fn interrupter(&self) -> u64 {
        self.rt + 0x20
    }

    fn ctx(&self, dma: &Dma, index: usize) -> *mut u32 {
        unsafe { dma.virt.add(index * self.ctx_size) as *mut u32 }
    }

    /// Olay halkasını boşaltır; tamamlanan kesme aktarımlarının işleyicilerini döndürür.
    fn process_events(&mut self) -> Vec<(Handler, Vec<u8>)> {
        let mut callbacks = Vec::new();
        let mut any = false;
        while let Some(trb) = self.events.pop() {
            any = true;
            let kind = (trb[3] >> 10) & 0x3f;
            let phys = trb[0] as u64 | (trb[1] as u64) << 32;
            let code = (trb[2] >> 24) as u8;
            match kind {
                EVENT_TRANSFER => {
                    let slot_id = (trb[3] >> 24) as u8;
                    let dci = ((trb[3] >> 16) & 0x1f) as u8;
                    let residual = trb[2] & 0x00ff_ffff;
                    let mut requeue = None;
                    if let Some(slot) = self.slots.get_mut(&slot_id)
                        && let Some(stream) = slot.iso.get_mut(&dci)
                    {
                        let ring = slot.rings.get_mut(&dci);
                        if let (Some(index), Some(ring)) = (stream.pending.remove(&phys), ring) {
                            let buffer = &stream.buffers[index];
                            let data = unsafe {
                                core::slice::from_raw_parts_mut(buffer.virt, stream.bytes)
                            };
                            (stream.fill)(data);
                            let trb = ring.push(isoch_trb(buffer.phys, stream.bytes));
                            stream.pending.insert(trb, index);
                        }
                        if code == CC_RING_UNDERRUN || code == CC_RING_OVERRUN || phys != 0 {
                            self.ring_doorbell(slot_id, dci);
                        }
                        continue;
                    }
                    if let Some(slot) = self.slots.get_mut(&slot_id)
                        && let Some(pipe) = slot.pipes.get_mut(&dci)
                        && pipe.pending == phys
                    {
                        if code == CC_SUCCESS || code == CC_SHORT_PACKET {
                            let len = pipe.size.saturating_sub(residual as usize);
                            let data =
                                unsafe { core::slice::from_raw_parts(pipe.buffer.virt, len) }
                                    .to_vec();
                            callbacks.push((pipe.handler.clone(), data));
                        }
                        if code != CC_STALL {
                            requeue = Some((pipe.buffer.phys, pipe.size));
                        }
                    } else {
                        self.done.insert(
                            phys,
                            Done {
                                code,
                                residual,
                                slot: slot_id,
                            },
                        );
                    }
                    if let Some((buffer, size)) = requeue
                        && let Some(slot) = self.slots.get_mut(&slot_id)
                        && let Some(ring) = slot.rings.get_mut(&dci)
                    {
                        let trb_phys = ring.push(normal_trb(buffer, size, TRB_IOC | TRB_ISP));
                        if let Some(pipe) = slot.pipes.get_mut(&dci) {
                            pipe.pending = trb_phys;
                        }
                        self.ring_doorbell(slot_id, dci);
                    }
                }
                EVENT_COMMAND => {
                    self.done.insert(
                        phys,
                        Done {
                            code,
                            residual: 0,
                            slot: (trb[3] >> 24) as u8,
                        },
                    );
                }
                EVENT_PORT => {
                    let port = (trb[0] >> 24) as u8;
                    if !self.port_changes.contains(&port) {
                        self.port_changes.push(port);
                    }
                }
                _ => {}
            }
        }
        if any {
            let erdp = self.events.dequeue_phys() | (1 << 3);
            self.w64(self.interrupter() + 0x18, erdp);
            COMPLETION_GEN.fetch_add(1, Ordering::Release);
        }
        callbacks
    }
}

fn isoch_trb(buffer: u64, len: usize) -> Trb {
    [
        buffer as u32,
        (buffer >> 32) as u32,
        len as u32 & 0x1ffff,
        TRB_ISOCH << 10 | TRB_SIA | TRB_IOC | TRB_ISP,
    ]
}

fn normal_trb(buffer: u64, len: usize, flags: u32) -> Trb {
    [
        buffer as u32,
        (buffer >> 32) as u32,
        len as u32,
        TRB_NORMAL << 10 | flags,
    ]
}

fn with<R>(c: usize, f: impl FnOnce(&mut Controller) -> R) -> Option<R> {
    CONTROLLERS.lock().get_mut(c).map(f)
}

pub fn count() -> usize {
    CONTROLLERS.lock().len()
}

/// Tüm denetleyicilerin olaylarını işler, kesme aktarımı işleyicilerini (kilit dışında) çağırır.
pub fn poll_all() {
    let callbacks: Vec<(Handler, Vec<u8>)> = {
        let mut list = CONTROLLERS.lock();
        list.iter_mut().flat_map(|c| c.process_events()).collect()
    };
    if !callbacks.is_empty() {
        COMPLETIONS.wake_all();
    }
    for (handler, data) in callbacks {
        handler(&data);
    }
}

/// Bekleyen port değişikliklerini (bağlandı/çıkarıldı) alır.
pub fn take_port_changes() -> Vec<(usize, u8)> {
    let mut out = Vec::new();
    let mut list = CONTROLLERS.lock();
    for c in list.iter_mut() {
        for port in c.port_changes.drain(..) {
            out.push((c.index, port));
        }
    }
    out
}

pub fn port_status(c: usize, port: u8) -> u32 {
    with(c, |x| x.portsc(port)).unwrap_or(0)
}

pub fn clear_port_changes(c: usize, port: u8) {
    with(c, |x| {
        let changes = x.portsc(port) & PORT_CHANGE_BITS;
        x.write_portsc(port, changes);
    });
}

/// `phys` adresli TRB tamamlanana kadar (olayları kendisi de işleyerek) bekler.
fn wait_done(c: usize, phys: u64, timeout_ms: u64) -> Option<Done> {
    let deadline = pit::uptime_ms() + timeout_ms;
    // Kısa aktarımlar çoğu zaman 1 ms'den önce biter; kesme gelmeyen (IRQ ≥ 16) gerçek
    // makinelerde uyuyup bir sonraki tik'i beklemek her komutu milisaniyelerce uzatır.
    let spin_until = tsc::now_ns() + 300_000;
    loop {
        poll_all();
        if let Some(done) = with(c, |x| x.done.remove(&phys)).flatten() {
            return Some(done);
        }
        if tsc::now_ns() < spin_until {
            core::hint::spin_loop();
            continue;
        }
        let now = pit::uptime_ms();
        if now >= deadline {
            return None;
        }
        let generation = COMPLETION_GEN.load(Ordering::Acquire);
        COMPLETIONS.wait_timeout(
            || COMPLETION_GEN.load(Ordering::Acquire) != generation,
            (deadline - now).min(2),
        );
    }
}

fn command(c: usize, trb: Trb) -> Option<Done> {
    let phys = with(c, |x| {
        let phys = x.command.push(trb);
        x.ring_doorbell(0, 0);
        phys
    })?;
    wait_done(c, phys, 1000)
}

fn on_irq() {
    let mut list = CONTROLLERS.lock();
    for c in list.iter_mut() {
        let iman = c.r32(c.interrupter());
        if iman & 1 != 0 {
            c.w32(c.interrupter(), iman | 1);
        }
        let sts = c.r32(c.op + 4);
        if sts & USBSTS_EINT != 0 {
            c.w32(c.op + 4, USBSTS_EINT);
        }
    }
    drop(list);
    EVENT_PENDING.store(true, Ordering::Release);
    super::host::EVENTS.wake_all();
    COMPLETIONS.wake_all();
}

pub fn take_event_pending() -> bool {
    EVENT_PENDING.swap(false, Ordering::AcqRel)
}

fn wait_until(timeout_ms: u64, mut done: impl FnMut() -> bool) -> bool {
    let deadline = tsc::now_ns() + timeout_ms * 1_000_000;
    loop {
        if done() {
            return true;
        }
        if tsc::now_ns() > deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

fn init_controller(index: usize, dev: &PciDevice) -> Result<Controller, &'static str> {
    dev.enable();
    let base = dev.bar(0).ok_or("no BAR0")?;
    let cap = map_physical(base, 0x10000, NO_CACHE).map_err(|_| "cannot map registers")? as u64;
    let r32 = |a: u64| unsafe { read_volatile(a as *const u32) };
    let w32 = |a: u64, v: u32| unsafe { write_volatile(a as *mut u32, v) };
    let cap_length = r32(cap) & 0xff;
    let hcs1 = r32(cap + 4);
    let hcs2 = r32(cap + 8);
    let hcc1 = r32(cap + 0x10);
    let op = cap + cap_length as u64;
    let db = cap + (r32(cap + 0x14) & !0x3) as u64;
    let rt = cap + (r32(cap + 0x18) & !0x1f) as u64;
    let max_slots = (hcs1 & 0xff) as u8;
    let max_ports = (hcs1 >> 24) as u8;
    let ctx_size = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };

    // Genişletilmiş yetenekler: BIOS devri ve USB 3 port aralıkları.
    let mut usb3_ports = Vec::new();
    let mut ext = ((hcc1 >> 16) as u64) << 2;
    let mut guard = 0;
    while ext != 0 && guard < 64 {
        let addr = cap + ext;
        let header = r32(addr);
        match header & 0xff {
            1 => {
                w32(addr, header | (1 << 24));
                let ok = wait_until(1000, || r32(addr) & (1 << 16) == 0);
                if !ok {
                    crate::log!("xhci: BIOS did not release the controller, taking over");
                    w32(addr, (r32(addr) & !(1 << 16)) | (1 << 24));
                }
                w32(addr + 4, 0xe000_0000);
            }
            2 => {
                let major = header >> 24;
                let ports = r32(addr + 8);
                if major == 3 {
                    usb3_ports.push(((ports & 0xff) as u8, ((ports >> 8) & 0xff) as u8));
                }
            }
            _ => {}
        }
        let next = ((header >> 8) & 0xff) as u64;
        ext = if next == 0 { 0 } else { ext + (next << 2) };
        guard += 1;
    }

    w32(op, r32(op) & !USBCMD_RUN);
    if !wait_until(100, || r32(op + 4) & USBSTS_HALTED != 0) {
        return Err("controller did not halt");
    }
    w32(op, USBCMD_RESET);
    if !wait_until(1000, || {
        r32(op) & USBCMD_RESET == 0 && r32(op + 4) & USBSTS_NOT_READY == 0
    }) {
        return Err("controller reset timed out");
    }
    w32(op + 0x38, max_slots as u32);

    let dcbaa = Dma::alloc(1).ok_or("out of memory")?;
    let scratch_count = (((hcs2 >> 21) & 0x1f) << 5 | (hcs2 >> 27)) as usize;
    let mut scratchpad = Vec::new();
    if scratch_count > 0 {
        let array = Dma::alloc(1).ok_or("out of memory")?;
        for i in 0..scratch_count.min(512) {
            let page = Dma::alloc(1).ok_or("out of memory")?;
            unsafe { write_volatile((array.virt as *mut u64).add(i), page.phys) };
            scratchpad.push(page);
        }
        unsafe { write_volatile(dcbaa.virt as *mut u64, array.phys) };
        scratchpad.push(array);
    }

    let command = Ring::new().ok_or("out of memory")?;
    let events = EventRing::new().ok_or("out of memory")?;
    let c = Controller {
        index,
        op,
        rt,
        db,
        max_slots,
        max_ports,
        ctx_size,
        dcbaa,
        _scratchpad: scratchpad,
        command,
        events,
        slots: BTreeMap::new(),
        done: BTreeMap::new(),
        port_changes: Vec::new(),
        usb3_ports,
        irq: dev.interrupt_line,
    };
    c.w64(op + 0x30, c.dcbaa.phys);
    c.w64(op + 0x18, c.command.dma.phys | 1);
    let ir = c.interrupter();
    c.w32(ir + 0x08, 1);
    c.w64(ir + 0x18, c.events.dma.phys);
    c.w64(ir + 0x10, c.events.erst.phys);
    c.w32(ir + 0x04, 1000);
    c.w32(ir, (1 << 1) | 1);
    c.w32(op, USBCMD_RUN | USBCMD_INTE);
    if !wait_until(100, || r32(op + 4) & USBSTS_HALTED == 0) {
        return Err("controller did not start");
    }
    for port in 1..=max_ports {
        if c.portsc(port) & PORT_PP == 0 {
            c.write_portsc(port, PORT_PP);
        }
    }
    Ok(c)
}

/// PCI'deki xHCI denetleyicilerini başlatır; başlatılanların sayısını döndürür.
pub fn probe() -> usize {
    let devices = pci::find_all(0x0c, 0x03);
    for dev in devices.iter().filter(|d| d.prog_if == 0x30) {
        let index = CONTROLLERS.lock().len();
        match init_controller(index, dev) {
            Ok(c) => {
                crate::log!(
                    "xhci: {:02x}:{:02x}.{} {:04x}:{:04x} {} slots, {} ports (USB3 {:?}), irq {}, {}-byte contexts",
                    dev.bus,
                    dev.slot,
                    dev.function,
                    dev.vendor_id,
                    dev.device_id,
                    c.max_slots,
                    c.max_ports,
                    c.usb3_ports,
                    c.irq,
                    c.ctx_size
                );
                CONTROLLERS.lock().push(c);
                dev.enable_interrupt(on_irq);
            }
            Err(e) => crate::log!(
                "xhci: {:02x}:{:02x}.{}: {}",
                dev.bus,
                dev.slot,
                dev.function,
                e
            ),
        }
    }
    count()
}

pub fn max_ports(c: usize) -> u8 {
    with(c, |x| x.max_ports).unwrap_or(0)
}

/// Portu sıfırlar (USB 2) ya da USB 3 bağlantısının etkinleşmesini bekler.
pub fn reset_port(c: usize, port: u8) -> Option<Speed> {
    let sc = port_status(c, port);
    if sc & PORT_CCS == 0 {
        return None;
    }
    if sc & PORT_PED == 0 {
        with(c, |x| x.write_portsc(port, PORT_PR));
        let deadline = pit::uptime_ms() + 500;
        while port_status(c, port) & PORT_PRC == 0 {
            if pit::uptime_ms() > deadline {
                return None;
            }
            crate::sched::sleep_ms(1);
        }
        crate::sched::sleep_ms(10);
    }
    clear_port_changes(c, port);
    let sc = port_status(c, port);
    if sc & PORT_PED == 0 || sc & PORT_CCS == 0 {
        return None;
    }
    Speed::from_xhci((sc >> 10) & 0xf)
}

/// Yeni bir aygıt yuvası açar ve varsayılan kontrol uç noktasıyla adresler.
pub fn address_device(c: usize, at: Route, speed: Speed) -> Result<u8, &'static str> {
    let port = at.root_port;
    let done = command(c, [0, 0, 0, TRB_ENABLE_SLOT << 10]).ok_or("enable slot timed out")?;
    if done.code != CC_SUCCESS {
        return Err("no free device slot");
    }
    let slot_id = done.slot;
    let mps = speed.default_max_packet();
    let in_ctx_phys = with(c, |x| -> Result<u64, &'static str> {
        let out_ctx = Dma::alloc(1).ok_or("out of memory")?;
        let in_ctx = Dma::alloc(1).ok_or("out of memory")?;
        let control_buffer = Dma::alloc(1).ok_or("out of memory")?;
        let ring = Ring::new().ok_or("out of memory")?;
        unsafe {
            let control = x.ctx(&in_ctx, 0);
            write_volatile(control.add(1), 0b11);
            let slot = x.ctx(&in_ctx, 1);
            write_volatile(
                slot,
                (at.route & 0xf_ffff) | (speed.xhci_id() << 20) | (1 << 27),
            );
            write_volatile(slot.add(1), (port as u32) << 16);
            if at.tt_slot != 0 {
                write_volatile(slot.add(2), at.tt_slot as u32 | (at.tt_port as u32) << 8);
            }
            let ep0 = x.ctx(&in_ctx, 2);
            write_volatile(ep0.add(1), (3 << 1) | (4 << 3) | ((mps as u32) << 16));
            write_volatile(ep0.add(2), ring.dma.phys as u32 | 1);
            write_volatile(ep0.add(3), (ring.dma.phys >> 32) as u32);
            write_volatile(ep0.add(4), 8);
            write_volatile(
                (x.dcbaa.virt as *mut u64).add(slot_id as usize),
                out_ctx.phys,
            );
        }
        let phys = in_ctx.phys;
        let mut rings = BTreeMap::new();
        rings.insert(1, ring);
        x.slots.insert(
            slot_id,
            Slot {
                port,
                route: at.route,
                hub: None,
                out_ctx,
                in_ctx,
                control_buffer,
                rings,
                pipes: BTreeMap::new(),
                iso: BTreeMap::new(),
            },
        );
        Ok(phys)
    })
    .ok_or("no controller")??;
    let done = command(
        c,
        [
            in_ctx_phys as u32,
            (in_ctx_phys >> 32) as u32,
            0,
            TRB_ADDRESS_DEVICE << 10 | (slot_id as u32) << 24,
        ],
    )
    .ok_or("address device timed out")?;
    if done.code != CC_SUCCESS {
        disable_slot(c, slot_id);
        return Err("address device failed");
    }
    Ok(slot_id)
}

/// EP0 azami paket boyutunu günceller (tam hızlı aygıtların gerçek değeri 8..64).
pub fn set_ep0_max_packet(c: usize, slot: u8, mps: u16) -> bool {
    let phys = with(c, |x| {
        let ctx_size = x.ctx_size;
        let s = x.slots.get(&slot)?;
        unsafe {
            core::ptr::write_bytes(s.in_ctx.virt, 0, ctx_size * 3);
            let control = s.in_ctx.virt as *mut u32;
            write_volatile(control.add(1), 0b10);
            let ep0 = s.in_ctx.virt.add(2 * ctx_size) as *mut u32;
            let out_ep0 = s.out_ctx.virt.add(ctx_size) as *const u32;
            for i in 0..5 {
                write_volatile(ep0.add(i), read_volatile(out_ep0.add(i)));
            }
            let dw1 = read_volatile(ep0.add(1)) & 0xffff;
            write_volatile(ep0.add(1), dw1 | (mps as u32) << 16);
        }
        Some(s.in_ctx.phys)
    })
    .flatten();
    let Some(phys) = phys else { return false };
    command(
        c,
        [
            phys as u32,
            (phys >> 32) as u32,
            0,
            TRB_EVALUATE_CONTEXT << 10 | (slot as u32) << 24,
        ],
    )
    .is_some_and(|d| d.code == CC_SUCCESS)
}

pub fn disable_slot(c: usize, slot: u8) {
    let _ = command(c, [0, 0, 0, TRB_DISABLE_SLOT << 10 | (slot as u32) << 24]);
    with(c, |x| {
        x.slots.remove(&slot);
        unsafe { write_volatile((x.dcbaa.virt as *mut u64).add(slot as usize), 0) };
    });
}

/// Kök porta doğrudan bağlı aygıtın yuvası.
pub fn slot_of_port(c: usize, port: u8) -> Option<u8> {
    slot_at(c, port, 0)
}

pub fn slot_at(c: usize, port: u8, route: u32) -> Option<u8> {
    with(c, |x| {
        x.slots
            .iter()
            .find(|(_, s)| s.port == port && s.route == route)
            .map(|(&id, _)| id)
    })
    .flatten()
}

/// Yuvayı hub olarak işaretler; bir sonraki `configure_endpoints` xHCI'ye bildirir.
pub fn mark_hub(c: usize, slot: u8, info: HubInfo) {
    with(c, |x| {
        if let Some(s) = x.slots.get_mut(&slot) {
            s.hub = Some(info);
        }
    });
}

/// Kontrol aktarımı; IN için okunan bayt sayısını döndürür.
pub fn control(c: usize, slot: u8, setup: Setup, data: &mut [u8]) -> Result<usize, u8> {
    let len = (setup.length as usize).min(data.len()).min(4096);
    let input = setup.request_type & 0x80 != 0;
    let started = with(c, |x| {
        let s = x.slots.get_mut(&slot)?;
        let buffer = s.control_buffer.phys;
        if !input && len > 0 {
            unsafe { core::ptr::copy_nonoverlapping(data.as_ptr(), s.control_buffer.virt, len) };
        }
        let ring = s.rings.get_mut(&1)?;
        let transfer_type = match (len, input) {
            (0, _) => 0,
            (_, false) => 2,
            (_, true) => 3,
        };
        ring.push([
            setup.request_type as u32 | (setup.request as u32) << 8 | (setup.value as u32) << 16,
            setup.index as u32 | (len as u32) << 16,
            8,
            TRB_SETUP << 10 | TRB_IDT | transfer_type << 16,
        ]);
        let mut data_trb = None;
        if len > 0 {
            data_trb = Some(ring.push([
                buffer as u32,
                (buffer >> 32) as u32,
                len as u32,
                TRB_DATA << 10
                    | if input {
                        TRB_DIR_IN | TRB_ISP | TRB_IOC
                    } else {
                        0
                    },
            ]));
        }
        let status_dir = if len > 0 && input { 0 } else { TRB_DIR_IN };
        let status = ring.push([0, 0, 0, TRB_STATUS << 10 | TRB_IOC | status_dir]);
        x.ring_doorbell(slot, 1);
        Some((status, data_trb))
    })
    .flatten();
    let Some((status_trb, data_trb)) = started else {
        return Err(0);
    };
    let done = wait_done(c, status_trb, 1000);
    let data_done = data_trb.and_then(|phys| with(c, |x| x.done.remove(&phys)).flatten());
    let done = match (done, data_done) {
        (Some(d), _) => d,
        (None, Some(d)) if d.code != CC_SUCCESS && d.code != CC_SHORT_PACKET => d,
        _ => return Err(0),
    };
    if let Some(d) = data_done
        && d.code != CC_SUCCESS
        && d.code != CC_SHORT_PACKET
    {
        return Err(d.code);
    }
    if done.code != CC_SUCCESS && done.code != CC_SHORT_PACKET {
        return Err(done.code);
    }
    let actual = len - data_done.map_or(0, |d| d.residual as usize).min(len);
    if input {
        with(c, |x| {
            if let Some(s) = x.slots.get(&slot) {
                unsafe {
                    core::ptr::copy_nonoverlapping(s.control_buffer.virt, data.as_mut_ptr(), actual)
                };
            }
        });
    }
    Ok(actual)
}

fn dci_of(address: u8) -> u8 {
    (address & 0x0f) * 2 + (address >> 7)
}

/// Uç noktaları denetleyiciye tanıtır (Configure Endpoint).
pub fn configure_endpoints(c: usize, slot: u8, speed: Speed, endpoints: &[EndpointConfig]) -> bool {
    let phys = with(c, |x| {
        let ctx_size = x.ctx_size;
        let s = x.slots.get_mut(&slot)?;
        unsafe { core::ptr::write_bytes(s.in_ctx.virt, 0, 4096) };
        let mut add = 1u32;
        let mut max_dci = 1;
        for ep in endpoints {
            let dci = dci_of(ep.address);
            let ring = Ring::new()?;
            let input = ep.address & 0x80 != 0;
            let ep_type = match (ep.transfer, input) {
                (1, false) => 1,
                (1, true) => 5,
                (2, false) => 2,
                (2, true) => 6,
                (3, false) => 3,
                _ => 7,
            };
            let interval = if ep.transfer == 1 {
                // Isochronous: 2^(bInterval-1) çerçeve (FS) ya da mikroçerçeve (HS/SS).
                let exponent = (ep.interval.clamp(1, 16) - 1) as u32;
                match speed {
                    Speed::Low | Speed::Full => exponent + 3,
                    _ => exponent,
                }
            } else if ep.transfer == 3 {
                match speed {
                    Speed::Low | Speed::Full => {
                        let frames = (ep.interval.max(1) as u32) * 8;
                        (31 - frames.leading_zeros()).clamp(3, 10)
                    }
                    _ => (ep.interval.clamp(1, 16) - 1) as u32,
                }
            } else {
                0
            };
            let mps = (ep.max_packet & 0x7ff) as u32;
            let burst = ((ep.max_packet >> 11) & 0x3) as u32;
            unsafe {
                let ctx = s.in_ctx.virt.add((dci as usize + 1) * ctx_size) as *mut u32;
                write_volatile(ctx, interval << 16);
                // Isochronous uç noktalarda hata sayacı (CErr) 0 olmalı.
                let cerr = if ep.transfer == 1 { 0 } else { 3 };
                write_volatile(
                    ctx.add(1),
                    (cerr << 1) | (ep_type << 3) | (burst << 8) | (mps << 16),
                );
                write_volatile(ctx.add(2), ring.dma.phys as u32 | 1);
                write_volatile(ctx.add(3), (ring.dma.phys >> 32) as u32);
                let avg = if matches!(ep.transfer, 1 | 3) {
                    mps
                } else {
                    3072
                };
                write_volatile(ctx.add(4), avg | ((mps * (burst + 1)) << 16));
            }
            s.rings.insert(dci, ring);
            add |= 1 << dci;
            max_dci = max_dci.max(dci as u32);
        }
        unsafe {
            let control = s.in_ctx.virt as *mut u32;
            write_volatile(control.add(1), add);
            let slot_ctx = s.in_ctx.virt.add(ctx_size) as *mut u32;
            let out_slot = s.out_ctx.virt as *const u32;
            for i in 0..4 {
                write_volatile(slot_ctx.add(i), read_volatile(out_slot.add(i)));
            }
            // Önceden yapılandırılmış uç noktalar (aynı aygıtın başka arayüzü) kaybolmasın.
            max_dci = max_dci.max(read_volatile(out_slot) >> 27);
            let mut dw0 = read_volatile(slot_ctx) & !(0x1f << 27);
            if let Some(hub) = s.hub {
                dw0 |= 1 << 26;
                let dw1 = read_volatile(slot_ctx.add(1)) & 0x00ff_ffff;
                write_volatile(slot_ctx.add(1), dw1 | (hub.ports as u32) << 24);
                let dw2 = read_volatile(slot_ctx.add(2)) & !(3 << 16);
                write_volatile(slot_ctx.add(2), dw2 | ((hub.think_time as u32 & 3) << 16));
            }
            write_volatile(slot_ctx, dw0 | (max_dci << 27));
            write_volatile(slot_ctx.add(3), 0);
        }
        Some(s.in_ctx.phys)
    })
    .flatten();
    let Some(phys) = phys else { return false };
    command(
        c,
        [
            phys as u32,
            (phys >> 32) as u32,
            0,
            TRB_CONFIGURE_ENDPOINT << 10 | (slot as u32) << 24,
        ],
    )
    .is_some_and(|d| d.code == CC_SUCCESS)
}

/// Kesme IN uç noktasını sürekli dinler; her rapor `handler`'a gider.
pub fn start_interrupt_pipe(
    c: usize,
    slot: u8,
    address: u8,
    size: usize,
    handler: Handler,
) -> bool {
    let dci = dci_of(address);
    with(c, |x| {
        let buffer = Dma::alloc(1)?;
        let size = size.clamp(1, 4096);
        let s = x.slots.get_mut(&slot)?;
        let ring = s.rings.get_mut(&dci)?;
        let pending = ring.push(normal_trb(buffer.phys, size, TRB_IOC | TRB_ISP));
        s.pipes.insert(
            dci,
            Pipe {
                buffer,
                size,
                pending,
                handler,
            },
        );
        x.ring_doorbell(slot, dci);
        Some(())
    })
    .flatten()
    .is_some()
}

/// Isochronous OUT akışı başlatır: `count` TD, her biri `bytes` bayt; tamamlanan her
/// TD `fill` ile yeniden doldurulup halkaya konur.
pub fn start_iso_out(
    c: usize,
    slot: u8,
    address: u8,
    bytes: usize,
    count: usize,
    fill: IsoFill,
) -> bool {
    let dci = dci_of(address);
    with(c, |x| {
        let s = x.slots.get_mut(&slot)?;
        let ring = s.rings.get_mut(&dci)?;
        let count = count.clamp(2, RING_TRBS / 2);
        let mut stream = IsoStream {
            buffers: Vec::new(),
            bytes,
            pending: BTreeMap::new(),
            fill,
        };
        for index in 0..count {
            let buffer = Dma::alloc(1)?;
            let data = unsafe { core::slice::from_raw_parts_mut(buffer.virt, bytes) };
            (stream.fill)(data);
            let trb = ring.push(isoch_trb(buffer.phys, bytes));
            stream.pending.insert(trb, index);
            stream.buffers.push(buffer);
        }
        s.iso.insert(dci, stream);
        x.ring_doorbell(slot, dci);
        Some(())
    })
    .flatten()
    .is_some()
}

/// Bulk aktarım (tek TRB, en fazla 64 KiB, 64 KiB sınırını geçmemeli). Aktarılan bayt sayısı.
pub fn bulk(
    c: usize,
    slot: u8,
    address: u8,
    buffer: u64,
    len: usize,
    timeout_ms: u64,
) -> Result<usize, u8> {
    let dci = dci_of(address);
    let phys = with(c, |x| {
        let s = x.slots.get_mut(&slot)?;
        let ring = s.rings.get_mut(&dci)?;
        let phys = ring.push(normal_trb(buffer, len, TRB_IOC | TRB_ISP));
        x.ring_doorbell(slot, dci);
        Some(phys)
    })
    .flatten()
    .ok_or(0u8)?;
    let done = wait_done(c, phys, timeout_ms).ok_or(0u8)?;
    match done.code {
        CC_SUCCESS | CC_SHORT_PACKET => Ok(len - (done.residual as usize).min(len)),
        code => Err(code),
    }
}

/// Takılan (STALL) uç noktayı sıfırlar ve halkayı ilerletir; ardından aygıtta
/// CLEAR_FEATURE(ENDPOINT_HALT) gönderilmelidir.
pub fn reset_endpoint(c: usize, slot: u8, address: u8) -> bool {
    let dci = dci_of(address) as u32;
    let reset = command(
        c,
        [
            0,
            0,
            0,
            TRB_RESET_ENDPOINT << 10 | dci << 16 | (slot as u32) << 24,
        ],
    );
    if !reset.is_some_and(|d| d.code == CC_SUCCESS) {
        return false;
    }
    let Some(pointer) = with(c, |x| {
        x.slots
            .get(&slot)
            .and_then(|s| s.rings.get(&(dci as u8)))
            .map(|r| r.dequeue_pointer())
    })
    .flatten() else {
        return false;
    };
    command(
        c,
        [
            pointer as u32,
            (pointer >> 32) as u32,
            0,
            TRB_SET_DEQUEUE << 10 | dci << 16 | (slot as u32) << 24,
        ],
    )
    .is_some_and(|d| d.code == CC_SUCCESS)
}
