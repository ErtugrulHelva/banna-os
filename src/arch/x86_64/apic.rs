//! Local APIC ve I/O APIC: 8259 PIC'in yerini alan kesme denetleyicisi.
//!
//! ISA IRQ'ları (0-15) MADT yönlendirmelerine uyularak I/O APIC üzerinden
//! 32+IRQ vektörlerine gönderilir; böylece `register_irq` API'si değişmez.

use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, Ordering};

use super::cpu::{self, outb};
use super::idt::{IRQ_BASE, LAPIC_TIMER_VECTOR, SPURIOUS_VECTOR};
use super::{pic, smp, tsc};
use crate::drivers::acpi;
use crate::kernel::initcall::{Detail, self_test as check};
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::sync::SpinLock;

const MSR_APIC_BASE: u32 = 0x1b;
const REG_ID: usize = 0x20;
const REG_VERSION: usize = 0x30;
const REG_TPR: usize = 0x80;
const REG_EOI: usize = 0xb0;
const REG_SVR: usize = 0xf0;
const REG_ICR_LOW: usize = 0x300;
const REG_ICR_HIGH: usize = 0x310;
const REG_LVT_TIMER: usize = 0x320;
const REG_LVT_LINT0: usize = 0x350;
const REG_LVT_LINT1: usize = 0x360;
const REG_TIMER_INITIAL: usize = 0x380;
const REG_TIMER_CURRENT: usize = 0x390;
const REG_TIMER_DIVIDE: usize = 0x3e0;
const LVT_MASKED: u32 = 1 << 16;
const TIMER_PERIODIC: u32 = 1 << 17;

static LAPIC: AtomicU64 = AtomicU64::new(0);
static X2APIC: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static ENABLED_IRQS: AtomicU16 = AtomicU16::new(0);
static TICKS_PER_MS: AtomicU64 = AtomicU64::new(0);
static BSP_TIMER_FIRED: AtomicU64 = AtomicU64::new(0);
static BSP_LAPIC_TICK: AtomicBool = AtomicBool::new(false);
static IPIS: AtomicU64 = AtomicU64::new(0);

struct IoApic {
    base: *mut u8,
    gsi_base: u32,
    entries: u32,
}

unsafe impl Send for IoApic {}

static IOAPICS: SpinLock<Vec<IoApic>> = SpinLock::new(Vec::new());
/// ISA IRQ → (GSI, düzey tetiklemeli mi, aktif düşük mü).
static ROUTES: SpinLock<[(u32, bool, bool); 16]> = SpinLock::new([(0, false, false); 16]);

pub fn active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// UEFI bazı makinelerde LAPIC'i x2APIC kipinde bırakır; o zaman MMIO yok sayılır
/// ve yazmaçlara 0x800 + reg/16 MSR'leriyle erişilir.
fn x2apic() -> bool {
    X2APIC.load(Ordering::Relaxed)
}

fn lapic_read(reg: usize) -> u32 {
    if x2apic() {
        return cpu::rdmsr(0x800 + (reg >> 4) as u32) as u32;
    }
    unsafe { read_volatile((LAPIC.load(Ordering::Relaxed) as usize + reg) as *const u32) }
}

fn lapic_write(reg: usize, value: u32) {
    if x2apic() {
        cpu::wrmsr(0x800 + (reg >> 4) as u32, value as u64);
        return;
    }
    unsafe {
        write_volatile(
            (LAPIC.load(Ordering::Relaxed) as usize + reg) as *mut u32,
            value,
        )
    }
}

pub fn lapic_id() -> u32 {
    if LAPIC.load(Ordering::Relaxed) == 0 {
        return 0;
    }
    if x2apic() {
        return lapic_read(REG_ID);
    }
    lapic_read(REG_ID) >> 24
}

fn write_icr(lapic_id: u32, low: u32) {
    if x2apic() {
        cpu::wrmsr(0x830, (lapic_id as u64) << 32 | low as u64);
        return;
    }
    lapic_write(REG_ICR_HIGH, lapic_id << 24);
    lapic_write(REG_ICR_LOW, low);
    while lapic_read(REG_ICR_LOW) & (1 << 12) != 0 {
        core::hint::spin_loop();
    }
}

pub fn eoi() {
    lapic_write(REG_EOI, 0);
}

impl IoApic {
    fn read(&self, reg: u32) -> u32 {
        unsafe {
            write_volatile(self.base as *mut u32, reg);
            read_volatile(self.base.add(0x10) as *const u32)
        }
    }

    fn write(&self, reg: u32, value: u32) {
        unsafe {
            write_volatile(self.base as *mut u32, reg);
            write_volatile(self.base.add(0x10) as *mut u32, value);
        }
    }

    fn set_entry(
        &self,
        pin: u32,
        vector: u8,
        level: bool,
        active_low: bool,
        masked: bool,
        dest: u32,
    ) {
        let mut low = vector as u32;
        if active_low {
            low |= 1 << 13;
        }
        if level {
            low |= 1 << 15;
        }
        if masked {
            low |= 1 << 16;
        }
        self.write(0x10 + pin * 2 + 1, dest << 24);
        self.write(0x10 + pin * 2, low);
    }
}

fn route(irq: u8, masked: bool) {
    let (gsi, level, active_low) = ROUTES.lock()[irq as usize];
    let dest = smp::boot_lapic_id();
    for ioapic in IOAPICS.lock().iter() {
        if gsi >= ioapic.gsi_base && gsi < ioapic.gsi_base + ioapic.entries {
            ioapic.set_entry(
                gsi - ioapic.gsi_base,
                IRQ_BASE + irq,
                level,
                active_low,
                masked,
                dest,
            );
        }
    }
}

/// ISA dışı bir GSI'yi `vector`'e yönlendirir; GSI hiçbir I/O APIC'te yoksa `false`.
pub fn route_gsi(gsi: u32, vector: u8, level: bool, active_low: bool) -> bool {
    let dest = smp::boot_lapic_id();
    for ioapic in IOAPICS.lock().iter() {
        if gsi >= ioapic.gsi_base && gsi < ioapic.gsi_base + ioapic.entries {
            ioapic.set_entry(
                gsi - ioapic.gsi_base,
                vector,
                level,
                active_low,
                false,
                dest,
            );
            return true;
        }
    }
    false
}

pub fn unmask_irq(irq: u8) {
    if irq >= 16 {
        return;
    }
    ENABLED_IRQS.fetch_or(1 << irq, Ordering::Relaxed);
    if active() {
        route(irq, false);
    } else {
        pic::unmask(irq);
    }
}

/// Geçerli CPU'nun Local APIC'ini açar (her çekirdekte çağrılır).
pub fn init_local() {
    if x2apic() {
        let base = cpu::rdmsr(MSR_APIC_BASE);
        if base & (1 << 10) == 0 {
            cpu::wrmsr(MSR_APIC_BASE, base | (1 << 11) | (1 << 10));
        }
    }
    lapic_write(REG_TPR, 0);
    lapic_write(REG_SVR, 0x100 | SPURIOUS_VECTOR as u32);
    lapic_write(REG_LVT_TIMER, LVT_MASKED);
    lapic_write(REG_LVT_LINT0, LVT_MASKED);
    if smp::is_boot_cpu() {
        lapic_write(REG_LVT_LINT1, 0x400);
    } else {
        lapic_write(REG_LVT_LINT1, LVT_MASKED);
    }
}

/// LAPIC zamanlayıcısını periyodik kipte başlatır.
pub fn start_timer(hz: u64) {
    let per_ms = TICKS_PER_MS.load(Ordering::Relaxed);
    if per_ms == 0 {
        return;
    }
    lapic_write(REG_TIMER_DIVIDE, 0x3);
    lapic_write(REG_LVT_TIMER, LAPIC_TIMER_VECTOR as u32 | TIMER_PERIODIC);
    lapic_write(REG_TIMER_INITIAL, (per_ms * 1000 / hz).max(1) as u32);
}

/// LAPIC zamanlayıcısını durdurur (boşta bekleyen ikincil çekirdek).
pub fn stop_timer() {
    lapic_write(REG_TIMER_INITIAL, 0);
}

fn one_shot(ms: u64) {
    lapic_write(REG_TIMER_DIVIDE, 0x3);
    lapic_write(REG_LVT_TIMER, LAPIC_TIMER_VECTOR as u32);
    lapic_write(
        REG_TIMER_INITIAL,
        (TICKS_PER_MS.load(Ordering::Relaxed) * ms).max(1) as u32,
    );
}

fn calibrate() -> u64 {
    lapic_write(REG_TIMER_DIVIDE, 0x3);
    lapic_write(REG_LVT_TIMER, LVT_MASKED);
    lapic_write(REG_TIMER_INITIAL, u32::MAX);
    let start = tsc::now_ns();
    while tsc::now_ns() < start + 10_000_000 {
        core::hint::spin_loop();
    }
    let elapsed = u32::MAX - lapic_read(REG_TIMER_CURRENT);
    lapic_write(REG_TIMER_INITIAL, 0);
    elapsed as u64 / 10
}

pub fn timer_interrupt() {
    if smp::is_boot_cpu() {
        BSP_TIMER_FIRED.fetch_add(1, Ordering::Relaxed);
    }
}

pub fn ipi_received() {
    IPIS.fetch_add(1, Ordering::Relaxed);
}

/// `lapic_id` çekirdeğine sabit vektörlü IPI gönderir.
pub fn send_ipi(lapic_id: u32, vector: u8) {
    write_icr(lapic_id, vector as u32);
}

/// INIT ya da STARTUP gibi özel teslim kipli IPI (ICR alt sözcüğü doğrudan verilir).
pub fn send_raw(lapic_id: u32, low: u32) {
    write_icr(lapic_id, low);
}

/// PIT kesmesi gelmediği için önyükleme çekirdeği tikini LAPIC'ten alıyor mu?
pub fn bsp_lapic_tick() -> bool {
    BSP_LAPIC_TICK.load(Ordering::Relaxed)
}

pub fn ipi_count() -> u64 {
    IPIS.load(Ordering::Relaxed)
}

pub fn ticks_per_ms() -> u64 {
    TICKS_PER_MS.load(Ordering::Relaxed)
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let Some(madt) = acpi::with(|a| a.madt).flatten() else {
        let _ = write!(detail, "no MADT, staying on the 8259 PIC");
        return Ok(());
    };
    if madt.io_apics == 0 || !cpu::has_feature(1, cpu::CpuidRegister::Edx, 9) {
        let _ = write!(detail, "no I/O APIC, staying on the 8259 PIC");
        return Ok(());
    }

    let base = cpu::rdmsr(MSR_APIC_BASE) & 0xffff_f000;
    let base = if madt.local_apic_address != 0 {
        madt.local_apic_address
    } else {
        base
    };
    let lapic = map_physical(base, 0x1000, NO_CACHE).map_err(|_| "cannot map the local APIC")?;
    LAPIC.store(lapic as u64, Ordering::Relaxed);
    let apic_base = cpu::rdmsr(MSR_APIC_BASE);
    X2APIC.store(apic_base & (1 << 10) != 0, Ordering::Relaxed);
    cpu::wrmsr(MSR_APIC_BASE, apic_base | (1 << 11));

    {
        let mut ioapics = IOAPICS.lock();
        for entry in madt.io_apic_list.iter().take(madt.io_apics as usize) {
            let ptr = map_physical(entry.address as u64, 0x20, NO_CACHE)
                .map_err(|_| "cannot map an I/O APIC")?;
            crate::log!(
                "apic: I/O APIC id {} at {:#x}, GSI base {}",
                entry.id,
                entry.address,
                entry.gsi_base
            );
            let mut ioapic = IoApic {
                base: ptr,
                gsi_base: entry.gsi_base,
                entries: 0,
            };
            ioapic.entries = ((ioapic.read(1) >> 16) & 0xff) + 1;
            for pin in 0..ioapic.entries {
                ioapic.set_entry(pin, 0, false, false, true, 0);
            }
            ioapics.push(ioapic);
        }
    }
    {
        let mut routes = ROUTES.lock();
        for (irq, route) in routes.iter_mut().enumerate() {
            *route = (irq as u32, false, false);
        }
        for o in madt.override_list.iter().take(madt.overrides as usize) {
            if (o.source as usize) < 16 {
                let active_low = o.flags & 0x3 == 0x3;
                let level = (o.flags >> 2) & 0x3 == 0x3;
                routes[o.source as usize] = (o.gsi, level, active_low);
            }
        }
    }

    cpu::without_interrupts(|| {
        unsafe {
            outb(0xa1, 0xff);
            outb(0x21, 0xff);
            if madt.legacy_pics {
                outb(0x22, 0x70);
                outb(0x23, 0x01);
            }
        }
        init_local();
        ACTIVE.store(true, Ordering::Relaxed);
        let enabled = ENABLED_IRQS.load(Ordering::Relaxed);
        for irq in 0..16u8 {
            if enabled & (1 << irq) != 0 {
                route(irq, false);
            }
        }
    });

    TICKS_PER_MS.store(calibrate(), Ordering::Relaxed);
    let before = BSP_TIMER_FIRED.load(Ordering::Relaxed);
    one_shot(2);
    let start = tsc::now_ns();
    while BSP_TIMER_FIRED.load(Ordering::Relaxed) == before && tsc::now_ns() < start + 100_000_000 {
        core::hint::spin_loop();
    }
    let timer_ok = BSP_TIMER_FIRED.load(Ordering::Relaxed) > before;

    let pit_start = super::pit::ticks();
    let start = tsc::now_ns();
    while super::pit::ticks() < pit_start + 5 && tsc::now_ns() < start + 50_000_000 {
        core::hint::spin_loop();
    }
    if super::pit::ticks() < pit_start + 5 {
        crate::log!("apic: PIT IRQ0 is silent, BSP ticks from the LAPIC timer");
        BSP_LAPIC_TICK.store(true, Ordering::Relaxed);
        start_timer(1000);
    }
    let version = lapic_read(REG_VERSION);

    crate::log!(
        "apic: LAPIC {:#x}{} id {} version {:#x}, {} I/O APIC(s), timer {} ticks/ms",
        base,
        if x2apic() { " (x2APIC)" } else { "" },
        lapic_id(),
        version & 0xff,
        madt.io_apics,
        ticks_per_ms()
    );
    check("apic", timer_ok && ticks_per_ms() > 0)?;
    let _ = write!(
        detail,
        "LAPIC + {} I/O APIC, timer {} kHz{}",
        madt.io_apics,
        ticks_per_ms(),
        if bsp_lapic_tick() {
            ", tick: LAPIC"
        } else {
            ""
        }
    );
    Ok(())
}

crate::initcall!(core, 55, "APIC (LAPIC + I/O APIC)", init);
