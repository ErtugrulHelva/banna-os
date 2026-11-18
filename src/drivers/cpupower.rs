//! İşlemci gücü: Intel HWP (Speed Shift) profilleri ve sıcaklık okuma.
//!
//! Desteklenmeyen MSR'a erişim #GP ile çekirdeği durdurur; bu yüzden her MSR yalnızca
//! CPUID desteği bildirdiğinde ve gerçek donanımda (hipervizör biti kapalıyken) kullanılır.
//! AMD'de frekans firmware'e bırakılır; sıcaklık SMN/northbridge kaydından okunur.

use core::fmt::Write;
use core::sync::atomic::{AtomicU8, Ordering};

use crate::arch::cpu::{self, CpuidRegister};
use crate::kernel::initcall::Detail;

const IA32_PM_ENABLE: u32 = 0x770;
const IA32_HWP_CAPABILITIES: u32 = 0x771;
const IA32_HWP_REQUEST: u32 = 0x774;
const IA32_THERM_STATUS: u32 = 0x19c;
const IA32_PACKAGE_THERM_STATUS: u32 = 0x1b1;
const MSR_TEMPERATURE_TARGET: u32 = 0x1a2;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Profile {
    Performance,
    Balanced,
    PowerSave,
}

impl Profile {
    pub fn name(self) -> &'static str {
        match self {
            Profile::Performance => "performance",
            Profile::Balanced => "balanced",
            Profile::PowerSave => "powersave",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "performance" | "performans" => Some(Profile::Performance),
            "balanced" | "dengeli" => Some(Profile::Balanced),
            "powersave" | "tasarruf" => Some(Profile::PowerSave),
            _ => None,
        }
    }

    /// Enerji/performans tercihi (EPP): 0 = en hızlı, 255 = en tasarruflu.
    fn epp(self) -> u64 {
        match self {
            Profile::Performance => 0x00,
            Profile::Balanced => 0x80,
            Profile::PowerSave => 0xff,
        }
    }
}

static PROFILE: AtomicU8 = AtomicU8::new(1);

fn vendor_is(name: &[u8; 12]) -> bool {
    &cpu::vendor() == name
}

fn virtualized() -> bool {
    cpu::has_feature(1, CpuidRegister::Ecx, 31)
}

/// Intel HWP kullanılabilir mi (EPP dahil).
pub fn hwp_available() -> bool {
    vendor_is(b"GenuineIntel")
        && !virtualized()
        && cpu::has_feature(6, CpuidRegister::Eax, 7)
        && cpu::has_feature(6, CpuidRegister::Eax, 10)
}

pub fn profile() -> Profile {
    match PROFILE.load(Ordering::Relaxed) {
        0 => Profile::Performance,
        2 => Profile::PowerSave,
        _ => Profile::Balanced,
    }
}

/// Geçerli çekirdekte HWP'yi açar ve profili uygular (`smp::run_on_all` ile her çekirdekte).
fn apply_local() {
    cpu::wrmsr(IA32_PM_ENABLE, 1);
    let caps = cpu::rdmsr(IA32_HWP_CAPABILITIES);
    let highest = caps & 0xff;
    let lowest = (caps >> 24) & 0xff;
    let request = lowest | highest << 8 | profile().epp() << 24;
    cpu::wrmsr(IA32_HWP_REQUEST, request);
}

/// Profili ayarlar; HWP yoksa `false` (frekansı firmware yönetir).
pub fn set_profile(p: Profile) -> bool {
    PROFILE.store(
        match p {
            Profile::Performance => 0,
            Profile::Balanced => 1,
            Profile::PowerSave => 2,
        },
        Ordering::Relaxed,
    );
    if !hwp_available() {
        return false;
    }
    crate::arch::smp::run_on_all(apply_local);
    crate::log!("cpupower: profile {}", p.name());
    true
}

/// İşlemci (paket ya da çekirdek) sıcaklığı, °C.
pub fn temperature() -> Option<i32> {
    if virtualized() {
        return None;
    }
    if vendor_is(b"GenuineIntel") {
        if !cpu::has_feature(6, CpuidRegister::Eax, 0) {
            return None;
        }
        // TEMPERATURE_TARGET Nehalem (aile 6, model 0x1a) ve sonrasında vardır.
        let info = crate::arch::cpuinfo::read();
        let (family, model) = (info.family, info.model);
        let tjmax = if family == 6 && model >= 0x1a {
            ((cpu::rdmsr(MSR_TEMPERATURE_TARGET) >> 16) & 0xff) as i32
        } else {
            100
        };
        let tjmax = if (60..=130).contains(&tjmax) {
            tjmax
        } else {
            100
        };
        let status = if cpu::has_feature(6, CpuidRegister::Eax, 6) {
            cpu::rdmsr(IA32_PACKAGE_THERM_STATUS)
        } else {
            cpu::rdmsr(IA32_THERM_STATUS)
        };
        if status & (1 << 31) == 0 {
            return None;
        }
        return Some(tjmax - ((status >> 16) & 0x7f) as i32);
    }
    if vendor_is(b"AuthenticAMD") {
        let family = crate::arch::cpuinfo::read().family;
        if family >= 0x17 {
            // SMN 0x59800 (Tctl): kök kompleks 00:00.0 üzerinden dolaylı okuma.
            crate::drivers::pci::write_config(0, 0, 0, 0x60, 0x0005_9800);
            let raw = crate::drivers::pci::read_config(0, 0, 0, 0x64);
            let mut temp = ((raw >> 21) as i32) / 8;
            if raw & (1 << 19) != 0 {
                temp -= 49;
            }
            return Some(temp);
        }
        if (0x10..=0x16).contains(&family) {
            let raw = crate::drivers::pci::read_config(0, 0x18, 3, 0xa4);
            return Some(((raw >> 21) as i32) / 8);
        }
    }
    None
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    if hwp_available() {
        set_profile(Profile::Balanced);
        let _ = write!(detail, "Intel HWP, profile balanced");
    } else {
        let _ = write!(detail, "frequency managed by firmware");
    }
    if let Some(t) = temperature() {
        let _ = write!(detail, ", {} °C", t);
    }
    Ok(())
}

crate::initcall!(driver, 45, "CPU power (HWP, thermal)", init);
