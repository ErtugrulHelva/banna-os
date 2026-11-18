//! `syscall`/`sysret` girişi ve ring 3'e geçiş/dönüş.
//!
//! Çekirdekte GS tabanı per-CPU alanını gösterir; kullanıcı kipindeyken
//! `swapgs` ile KERNEL_GS_BASE'e saklanır.

use core::arch::{asm, global_asm};
use core::ptr::addr_of_mut;

use super::cpu;
use super::gdt::{self, CpuTables};
use crate::kernel::initcall::Detail;

pub const MAX_CPUS: usize = 256;

/// Çekirdek başına alan; çekirdek kipindeyken GS tabanı bunu gösterir.
/// Konumlar `syscall_entry` ve `cpu_index` tarafından sabit kullanılır.
#[repr(C)]
struct CpuLocal {
    kernel_rsp: u64,
    user_rsp_scratch: u64,
    index: u64,
    tables: *mut CpuTables,
}

static mut CPU_LOCALS: [CpuLocal; MAX_CPUS] = [const {
    CpuLocal {
        kernel_rsp: 0,
        user_rsp_scratch: 0,
        index: 0,
        tables: core::ptr::null_mut(),
    }
}; MAX_CPUS];

/// Geçerli çekirdeğin sıra numarası (önyükleme çekirdeği 0).
pub fn cpu_index() -> usize {
    let index: u64;
    unsafe { asm!("mov {}, gs:[16]", out(reg) index, options(nostack, readonly, preserves_flags)) };
    index as usize
}

/// Syscall girişinde kaydedilen tüm kullanıcı yazmaçları (`fork` çocuğu bunlarla döner).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SyscallFrame {
    pub number: u64,
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub rbp: u64,
    pub rbx: u64,
    pub arg0: u64,
    pub arg1: u64,
    pub arg2: u64,
    pub arg3: u64,
    pub arg4: u64,
    pub arg5: u64,
    pub user_rip: u64,
    pub user_rflags: u64,
    pub user_rsp: u64,
}

const RFLAGS_TF: u64 = 1 << 8;
const RFLAGS_IF: u64 = 1 << 9;
const RFLAGS_DF: u64 = 1 << 10;
const RFLAGS_AC: u64 = 1 << 18;
const EFER_SCE: u64 = 1 << 0;

global_asm!(
    ".global syscall_entry",
    "syscall_entry:",
    "swapgs",
    "mov gs:[8], rsp",
    "mov rsp, gs:[0]",
    "push qword ptr gs:[8]",
    "push r11",
    "push rcx",
    "push r9",
    "push r8",
    "push r10",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbx",
    "push rbp",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "push rax",
    "mov rdi, rsp",
    "call {dispatch}",
    "cli",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "pop rcx",
    "pop r11",
    "pop rsp",
    "swapgs",
    "sysretq",
    dispatch = sym crate::kernel::syscall::dispatch,
);

// `fork` çocuğu: kopyalanan çerçevedeki yazmaçlarla, rax = 0 olarak ring 3'e döner.
global_asm!(
    ".global fork_return",
    "fork_return:",
    "cli",
    "mov rsp, rdi",
    "xor eax, eax",
    "add rsp, 8",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop rbp",
    "pop rbx",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop r10",
    "pop r8",
    "pop r9",
    "pop rcx",
    "pop r11",
    "pop rsp",
    "swapgs",
    "sysretq",
);

global_asm!(
    ".global enter_user_mode",
    "enter_user_mode:",
    "cli",
    "push {user_ss}",
    "push rsi",
    "push 0x202",
    "push {user_cs}",
    "push rdi",
    "mov rdi, rdx",
    "mov rsi, rcx",
    "xor eax, eax",
    "xor ebx, ebx",
    "xor ecx, ecx",
    "xor edx, edx",
    "xor ebp, ebp",
    "xor r8d, r8d",
    "xor r9d, r9d",
    "xor r10d, r10d",
    "xor r11d, r11d",
    "xor r12d, r12d",
    "xor r13d, r13d",
    "xor r14d, r14d",
    "xor r15d, r15d",
    "swapgs",
    "iretq",
    user_ss = const gdt::USER_DATA as u64,
    user_cs = const gdt::USER_CODE as u64,
);

unsafe extern "C" {
    fn syscall_entry();
    /// Geçerli thread'in çekirdek yığını ayarlıyken `entry` adresinden ring 3'e geçer.
    /// `arg0`/`arg1` kullanıcı kodunda `rdi`/`rsi` olarak görünür.
    pub fn enter_user_mode(entry: u64, user_stack: u64, arg0: u64, arg1: u64) -> !;
    /// `frame` çekirdek yığınında olmalıdır; geri dönmez.
    pub fn fork_return(frame: *const SyscallFrame) -> !;
}

/// Ring 3'ten gelen syscall ve kesmelerin kullanacağı çekirdek yığını.
pub fn set_kernel_stack(top: u64) {
    unsafe {
        let local = &mut (*addr_of_mut!(CPU_LOCALS))[cpu_index()];
        local.kernel_rsp = top;
        if let Some(tables) = local.tables.as_mut() {
            tables.set_kernel_stack(top);
        }
    }
}

/// Çekirdeğin per-CPU alanını ve syscall MSR'lerini kurar (her çekirdekte bir kez).
pub fn init_cpu(index: usize, tables: *mut CpuTables) {
    let local = unsafe {
        let local = &mut (*addr_of_mut!(CPU_LOCALS))[index];
        local.index = index as u64;
        local.tables = tables;
        local as *mut CpuLocal as u64
    };
    cpu::wrmsr(cpu::MSR_GS_BASE, local);
    cpu::wrmsr(cpu::MSR_KERNEL_GS_BASE, 0);

    let star = ((gdt::KERNEL_DATA as u64) << 48) | ((gdt::KERNEL_CODE as u64) << 32);
    cpu::wrmsr(cpu::MSR_STAR, star);
    cpu::wrmsr(cpu::MSR_LSTAR, syscall_entry as *const () as u64);
    cpu::wrmsr(
        cpu::MSR_FMASK,
        RFLAGS_TF | RFLAGS_IF | RFLAGS_DF | RFLAGS_AC,
    );
    let efer = cpu::rdmsr(cpu::MSR_EFER);
    cpu::wrmsr(cpu::MSR_EFER, efer | EFER_SCE);
}

fn init(_: &mut Detail) -> Result<(), &'static str> {
    init_cpu(0, gdt::boot_tables());
    Ok(())
}

crate::initcall!(arch, 30, "syscall / sysret", init);
