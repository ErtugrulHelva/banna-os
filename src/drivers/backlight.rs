//! Dizüstü panel arka ışığı, Intel tümleşik grafiğin PCH PWM yazmaçlarıyla (ACPI _BCM
//! olmayan ya da Windows 8+ firmware'inde işlevsiz olan makineler için).
//!
//! - Cannon Point ve sonrası (Coffee Lake-H, Ice/Tiger/Alder/Raptor/Meteor Lake):
//!   BXT düzeni, ayrı frekans (0xC8254) ve doluluk (0xC8258) yazmaçları.
//! - Lynx/Sunrise Point (Haswell - Kaby Lake): 0xC8254 = frekans << 16 | doluluk.
//!
//! Firmware'in kurduğu PWM etkin değilse (bit 31) dokunulmaz.

use core::sync::atomic::{AtomicU8, AtomicU64, Ordering};

use crate::drivers::pci;
use crate::memory::paging::{NO_CACHE, map_physical};

const PWM_CTL: usize = 0xc8250;
const PWM_FREQ: usize = 0xc8254;
const PWM_DUTY: usize = 0xc8258;
const PWM_ENABLE: u32 = 1 << 31;

const NONE: u8 = 0;
const SPLIT: u8 = 1;
const PACKED: u8 = 2;

static KIND: AtomicU8 = AtomicU8::new(NONE);
static REGS: AtomicU64 = AtomicU64::new(0);

fn r(offset: usize) -> u32 {
    let base = REGS.load(Ordering::Relaxed) as usize;
    unsafe { core::ptr::read_volatile((base + offset) as *const u32) }
}

fn w(offset: usize, value: u32) {
    let base = REGS.load(Ordering::Relaxed) as usize;
    unsafe { core::ptr::write_volatile((base + offset) as *mut u32, value) }
}

/// Intel iGPU'nun etkin bir PWM arka ışığı varsa onu seçer.
pub fn probe() -> Option<&'static str> {
    if crate::arch::cpuinfo::emulated() {
        return None;
    }
    let dev = pci::devices()
        .into_iter()
        .find(|d| d.vendor_id == 0x8086 && d.class == 0x03 && d.bus == 0)?;
    let base = dev.bar(0).filter(|&b| b != 0)?;
    let regs = map_physical(base, 0x100000, NO_CACHE).ok()?;
    REGS.store(regs as u64, Ordering::Relaxed);
    if r(PWM_CTL) & PWM_ENABLE == 0 {
        return None;
    }
    let (freq, duty) = (r(PWM_FREQ), r(PWM_DUTY));
    let packed = r(PWM_FREQ);
    let kind = if freq != 0 && freq != u32::MAX && duty != 0 && duty <= freq {
        SPLIT
    } else if packed >> 16 != 0 && packed & 0xffff <= packed >> 16 {
        PACKED
    } else {
        return None;
    };
    KIND.store(kind, Ordering::Relaxed);
    crate::log!(
        "backlight: Intel {:04x} PCH PWM ({}), {}%",
        dev.device_id,
        if kind == SPLIT { "CNP+" } else { "LPT/SPT" },
        get().unwrap_or(0)
    );
    Some("Intel PWM")
}

fn period_and_duty() -> Option<(u32, u32)> {
    match KIND.load(Ordering::Relaxed) {
        SPLIT => Some((r(PWM_FREQ), r(PWM_DUTY))),
        PACKED => {
            let v = r(PWM_FREQ);
            Some((v >> 16, v & 0xffff))
        }
        _ => None,
    }
}

pub fn available() -> bool {
    KIND.load(Ordering::Relaxed) != NONE
}

/// Geçerli parlaklık (%).
pub fn get() -> Option<u32> {
    let (period, duty) = period_and_duty()?;
    (period != 0).then(|| (duty as u64 * 100 / period as u64).min(100) as u32)
}

/// Parlaklığı yüzde olarak ayarlar (0 = arka ışık kapalı).
pub fn set(percent: u32) -> bool {
    let Some((period, _)) = period_and_duty() else {
        return false;
    };
    let duty = (period as u64 * percent.min(100) as u64 / 100) as u32;
    match KIND.load(Ordering::Relaxed) {
        SPLIT => w(PWM_DUTY, duty),
        _ => w(PWM_FREQ, period << 16 | duty),
    }
    true
}
