//! Port G/Ç, kontrol yazmaçları ve kesme bayrağı yardımcıları.

use core::arch::asm;

pub unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags))
    };
}

pub unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", out("al") value, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    value
}

pub unsafe fn outw(port: u16, value: u16) {
    unsafe {
        asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags))
    };
}

pub unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe {
        asm!("in ax, dx", out("ax") value, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    value
}

pub unsafe fn outl(port: u16, value: u32) {
    unsafe {
        asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags))
    };
}

pub unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe {
        asm!("in eax, dx", out("eax") value, in("dx") port, options(nomem, nostack, preserves_flags))
    };
    value
}

pub fn io_wait() {
    unsafe { outb(0x80, 0) };
}

pub fn interrupts_enabled() -> bool {
    let rflags: u64;
    unsafe { asm!("pushfq", "pop {}", out(reg) rflags, options(nomem, preserves_flags)) };
    rflags & (1 << 9) != 0
}

pub fn disable_interrupts() {
    unsafe { asm!("cli", options(nomem, nostack)) };
}

pub fn enable_interrupts() {
    unsafe { asm!("sti", options(nomem, nostack)) };
}

pub fn without_interrupts<R>(f: impl FnOnce() -> R) -> R {
    let enabled = interrupts_enabled();
    disable_interrupts();
    let result = f();
    if enabled {
        enable_interrupts();
    }
    result
}

pub fn halt() {
    unsafe { asm!("hlt", options(nomem, nostack, preserves_flags)) };
}

pub fn halt_loop() -> ! {
    loop {
        halt();
    }
}

pub fn halt_forever() -> ! {
    disable_interrupts();
    halt_loop()
}

pub fn read_cr2() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

pub fn read_cr3() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

pub fn invlpg(address: u64) {
    unsafe { asm!("invlpg [{}]", in(reg) address, options(nostack, preserves_flags)) };
}

pub fn rdmsr(msr: u32) -> u64 {
    let (low, high): (u32, u32);
    unsafe {
        asm!("rdmsr", in("ecx") msr, out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags))
    };
    ((high as u64) << 32) | low as u64
}

pub fn wrmsr(msr: u32, value: u64) {
    unsafe {
        asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32, options(nostack, preserves_flags))
    };
}

pub fn read_cr4() -> u64 {
    let value: u64;
    unsafe { asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)) };
    value
}

pub unsafe fn write_cr4(value: u64) {
    unsafe { asm!("mov cr4, {}", in(reg) value, options(nostack, preserves_flags)) };
}

pub fn has_feature(leaf: u32, register: CpuidRegister, bit: u32) -> bool {
    let max = core::arch::x86_64::__cpuid(leaf & 0x8000_0000).eax;
    if leaf > max {
        return false;
    }
    let result = core::arch::x86_64::__cpuid_count(leaf, 0);
    let value = match register {
        CpuidRegister::Eax => result.eax,
        CpuidRegister::Ebx => result.ebx,
        CpuidRegister::Ecx => result.ecx,
        CpuidRegister::Edx => result.edx,
    };
    value & (1 << bit) != 0
}

#[allow(dead_code)]
#[derive(Clone, Copy)]
pub enum CpuidRegister {
    Eax,
    Ebx,
    Ecx,
    Edx,
}

pub fn vendor() -> [u8; 12] {
    let r = core::arch::x86_64::__cpuid(0);
    let mut out = [0u8; 12];
    out[0..4].copy_from_slice(&r.ebx.to_le_bytes());
    out[4..8].copy_from_slice(&r.edx.to_le_bytes());
    out[8..12].copy_from_slice(&r.ecx.to_le_bytes());
    out
}

pub fn brand() -> Option<[u8; 48]> {
    if core::arch::x86_64::__cpuid(0x8000_0000).eax < 0x8000_0004 {
        return None;
    }
    let mut out = [0u8; 48];
    for (i, leaf) in (0x8000_0002u32..=0x8000_0004).enumerate() {
        let r = core::arch::x86_64::__cpuid(leaf);
        for (j, reg) in [r.eax, r.ebx, r.ecx, r.edx].iter().enumerate() {
            let at = i * 16 + j * 4;
            out[at..at + 4].copy_from_slice(&reg.to_le_bytes());
        }
    }
    Some(out)
}

pub const MSR_EFER: u32 = 0xc000_0080;
pub const MSR_STAR: u32 = 0xc000_0081;
pub const MSR_LSTAR: u32 = 0xc000_0082;
pub const MSR_FMASK: u32 = 0xc000_0084;
pub const MSR_GS_BASE: u32 = 0xc000_0101;
pub const MSR_KERNEL_GS_BASE: u32 = 0xc000_0102;
