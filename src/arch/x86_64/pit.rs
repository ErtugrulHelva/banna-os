//! 8254 PIT kanal 0: sabit frekanslı sistem tik'i.

use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu::{self, outb};
use super::{idt, pic, tsc};
use crate::kernel::initcall::{Detail, self_test as check};
use core::fmt::Write;

const BASE_FREQUENCY: u32 = 1_193_182;
const CHANNEL0: u16 = 0x40;
const COMMAND: u16 = 0x43;

static TICKS: AtomicU64 = AtomicU64::new(0);
static FREQUENCY: AtomicU64 = AtomicU64::new(0);

fn init_call(detail: &mut Detail) -> Result<(), &'static str> {
    pic::init();
    init(1000);
    let source = calibrate_tsc();
    crate::log!("tsc: {} MHz ({})", tsc::mhz(), source);

    // Gerçek donanımda IRQ0 hiç gelmeyebilir (8254 saat kapılaması, UEFI'nin
    // maskelediği LINT0); bu durumda APIC kurulumu LAPIC zamanlayıcısına geçer.
    cpu::enable_interrupts();
    let start = ticks();
    let deadline = tsc::now_ns() + 50_000_000;
    while ticks() < start + 5 && tsc::now_ns() < deadline {
        core::hint::spin_loop();
    }
    let irq_ok = ticks() >= start + 5;
    if !irq_ok {
        crate::log!("pit: IRQ0 not arriving through the 8259");
    }
    let _ = write!(
        detail,
        "TSC {} MHz via {}{}",
        tsc::mhz(),
        source,
        if irq_ok { "" } else { ", IRQ0 missing" }
    );
    check("pit", tsc::mhz() > 0)
}

crate::initcall!(core, 50, "PIC + PIT timer (1 kHz)", init_call);

fn init(hz: u32) {
    let divisor = (BASE_FREQUENCY / hz).clamp(1, 0xffff) as u16;
    FREQUENCY.store((BASE_FREQUENCY / divisor as u32) as u64, Ordering::Relaxed);
    unsafe {
        outb(COMMAND, 0x34);
        outb(CHANNEL0, divisor as u8);
        outb(CHANNEL0, (divisor >> 8) as u8);
    }
    idt::register_irq(0, on_tick);
}

/// Kesme kullanmadan TSC frekansını ölçer; ilk çalışan kaynağın adını döndürür.
fn calibrate_tsc() -> &'static str {
    if calibrate_with_pm_timer() {
        return "ACPI PM timer";
    }
    if calibrate_with_pit_channel2() {
        return "PIT channel 2";
    }
    if let Some(mhz) = cpuid_tsc_mhz() {
        tsc::set_mhz(mhz);
        return "CPUID";
    }
    tsc::set_mhz(2000);
    "guess"
}

const PM_TIMER_HZ: u64 = 3_579_545;

fn calibrate_with_pm_timer() -> bool {
    let Some(fadt) = crate::drivers::acpi::fadt() else {
        return false;
    };
    if fadt.pm_timer == 0 {
        return false;
    }
    let port = fadt.pm_timer;
    let mask: u32 = if fadt.flags & crate::drivers::acpi::Fadt::FLAG_TMR_VAL_EXT != 0 {
        u32::MAX
    } else {
        0x00ff_ffff
    };
    let read = || unsafe { cpu::inl(port) } & mask;
    let target = (PM_TIMER_HZ / 20) as u32;
    let first = read();
    let mut spins = 0u32;
    let begin = loop {
        let now = read();
        if now != first {
            break now;
        }
        spins += 1;
        if spins > 1_000_000 {
            return false;
        }
    };
    let tsc_start = tsc::read();
    let mut elapsed = 0u32;
    let mut last = begin;
    let mut stalls = 0u32;
    while elapsed < target {
        let now = read();
        if now == last {
            stalls += 1;
            if stalls > 10_000_000 {
                return false;
            }
            continue;
        }
        stalls = 0;
        elapsed = elapsed.wrapping_add(now.wrapping_sub(last) & mask);
        last = now;
    }
    let tsc_end = tsc::read();
    let elapsed_us = elapsed as u64 * 1_000_000 / PM_TIMER_HZ;
    tsc::calibrate(tsc_start, tsc_end, elapsed_us);
    plausible()
}

fn calibrate_with_pit_channel2() -> bool {
    const PORT_B: u16 = 0x61;
    const COUNT: u16 = 59_659; // ~50 ms
    unsafe {
        let b = cpu::inb(PORT_B);
        outb(PORT_B, (b & !0x02) | 0x01);
        outb(COMMAND, 0xb0);
        outb(0x42, COUNT as u8);
        outb(0x42, (COUNT >> 8) as u8);
    }
    let tsc_start = tsc::read();
    let mut spins = 0u64;
    while unsafe { cpu::inb(PORT_B) } & 0x20 == 0 {
        spins += 1;
        if spins > 50_000_000 {
            return false;
        }
    }
    let tsc_end = tsc::read();
    tsc::calibrate(
        tsc_start,
        tsc_end,
        COUNT as u64 * 1_000_000 / BASE_FREQUENCY as u64,
    );
    plausible()
}

fn plausible() -> bool {
    (100..=10_000).contains(&tsc::mhz())
}

fn cpuid_tsc_mhz() -> Option<u64> {
    use core::arch::x86_64::__cpuid;
    let max = __cpuid(0).eax;
    if max >= 0x15 {
        let r = __cpuid(0x15);
        if r.eax != 0 && r.ebx != 0 && r.ecx != 0 {
            return Some(r.ecx as u64 * r.ebx as u64 / r.eax as u64 / 1_000_000);
        }
    }
    if max >= 0x16 {
        let base = __cpuid(0x16).eax & 0xffff;
        if base != 0 {
            return Some(base as u64);
        }
    }
    None
}

fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    crate::sched::tick();
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Açılıştan beri geçen süre (ms). TSC kalibre edildiyse ondan okunur: kesme sayımı,
/// sanal makinelerde işlemci meşgulken tik kaçırıp geride kalabilir (animasyonlar yavaşlar).
pub fn uptime_ms() -> u64 {
    let ms = super::tsc::uptime_ms().unwrap_or_else(tick_ms);
    // Çekirdekler arası küçük TSC farklarında bile geri gitmez.
    ms.max(LAST_MS.fetch_max(ms, Ordering::Relaxed))
}

static LAST_MS: AtomicU64 = AtomicU64::new(0);

/// Kesme sayımından geçen süre (ms).
pub fn tick_ms() -> u64 {
    match FREQUENCY.load(Ordering::Relaxed) {
        0 => 0,
        hz => ticks() * 1000 / hz,
    }
}
