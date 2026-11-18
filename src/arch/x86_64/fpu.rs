//! x87/SSE durumu: etkinleştirme ve thread başına FXSAVE alanı.
//!
//! Çekirdek soft-float derlenir ve XMM yazmaçlarına dokunmaz; bu yüzden durum
//! yalnızca bağlam değişiminde kaydedilip geri yüklenir.

use core::arch::asm;
use core::sync::atomic::{AtomicBool, Ordering};

use super::cpu::{self, CpuidRegister};
use crate::kernel::initcall::Detail;
use crate::sync::SpinLock;

const CR0_MP: u64 = 1 << 1;
const CR0_EM: u64 = 1 << 2;
const CR0_TS: u64 = 1 << 3;
const CR0_NE: u64 = 1 << 5;
const CR4_OSFXSR: u64 = 1 << 9;
const CR4_OSXMMEXCPT: u64 = 1 << 10;

static ENABLED: AtomicBool = AtomicBool::new(false);

#[repr(C, align(16))]
#[derive(Clone)]
pub struct FpuState([u8; 512]);

static TEMPLATE: SpinLock<FpuState> = SpinLock::new(FpuState([0; 512]));

impl FpuState {
    /// Temiz başlangıç durumu (FNINIT + MXCSR 0x1F80).
    pub fn initial() -> Self {
        TEMPLATE.lock().clone()
    }

    pub fn save(&mut self) {
        if ENABLED.load(Ordering::Relaxed) {
            unsafe { asm!("fxsave64 [{}]", in(reg) self.0.as_mut_ptr(), options(nostack)) };
        }
    }

    pub fn restore(&self) {
        if ENABLED.load(Ordering::Relaxed) {
            unsafe { asm!("fxrstor64 [{}]", in(reg) self.0.as_ptr(), options(nostack)) };
        }
    }
}

fn read_cr0() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr0", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

/// Geçerli CPU'da x87/SSE'yi açar (her çekirdekte çağrılır).
pub fn enable_local() {
    unsafe {
        let cr0 = (read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        cpu::write_cr4(cpu::read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT);
        let mxcsr: u32 = 0x1f80;
        asm!("fninit", "ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let fxsr = cpu::has_feature(1, CpuidRegister::Edx, 24);
    let sse2 = cpu::has_feature(1, CpuidRegister::Edx, 26);
    if !fxsr || !sse2 {
        return Err("CPU lacks FXSAVE/SSE2");
    }
    unsafe {
        let cr0 = (read_cr0() & !(CR0_EM | CR0_TS)) | CR0_MP | CR0_NE;
        asm!("mov cr0, {}", in(reg) cr0, options(nostack));
        cpu::write_cr4(cpu::read_cr4() | CR4_OSFXSR | CR4_OSXMMEXCPT);
        let mxcsr: u32 = 0x1f80;
        asm!("fninit", "ldmxcsr [{}]", in(reg) &mxcsr, options(nostack));
    }
    ENABLED.store(true, Ordering::Relaxed);
    TEMPLATE.lock().save();

    let avx = cpu::has_feature(1, CpuidRegister::Ecx, 28);
    let sse42 = cpu::has_feature(1, CpuidRegister::Ecx, 20);
    use core::fmt::Write;
    let _ = write!(
        detail,
        "x87 + SSE2{}{}, per-thread FXSAVE",
        if sse42 { " + SSE4.2" } else { "" },
        if avx {
            " (AVX present, not enabled)"
        } else {
            ""
        }
    );
    Ok(())
}

crate::initcall!(arch, 25, "FPU / SSE", init);
