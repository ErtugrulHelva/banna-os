//! IDT, kesme giriş saplamaları ve exception/IRQ dağıtımı.

use core::arch::{asm, global_asm, naked_asm};
use core::mem::size_of;
use core::ptr::addr_of;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::{cpu, gdt, pic};

pub const IRQ_BASE: u8 = 32;
const VECTOR_COUNT: usize = 48;

#[repr(C)]
#[derive(Debug)]
pub struct InterruptFrame {
    pub r15: u64,
    pub r14: u64,
    pub r13: u64,
    pub r12: u64,
    pub r11: u64,
    pub r10: u64,
    pub r9: u64,
    pub r8: u64,
    pub rbp: u64,
    pub rdi: u64,
    pub rsi: u64,
    pub rdx: u64,
    pub rcx: u64,
    pub rbx: u64,
    pub rax: u64,
    pub vector: u64,
    pub error_code: u64,
    pub rip: u64,
    pub cs: u64,
    pub rflags: u64,
    pub rsp: u64,
    pub ss: u64,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const EMPTY: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn new(handler: u64, ist: u8) -> Self {
        Self {
            offset_low: handler as u16,
            selector: gdt::KERNEL_CODE,
            ist,
            attributes: 0x8e,
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::EMPTY; 256];

/// IRQ başına en fazla 4 işleyici: PCI INTx hatları cihazlar arasında paylaşılabilir.
const HANDLERS_PER_IRQ: usize = 4;
static IRQ_HANDLERS: [[AtomicUsize; HANDLERS_PER_IRQ]; 16] =
    [const { [const { AtomicUsize::new(0) }; HANDLERS_PER_IRQ] }; 16];

global_asm!(
    ".global interrupt_common",
    "interrupt_common:",
    "test qword ptr [rsp + 24], 3",
    "jz 1f",
    "swapgs",
    "1:",
    "push rax",
    "push rbx",
    "push rcx",
    "push rdx",
    "push rsi",
    "push rdi",
    "push rbp",
    "push r8",
    "push r9",
    "push r10",
    "push r11",
    "push r12",
    "push r13",
    "push r14",
    "push r15",
    "cld",
    "mov rdi, rsp",
    "call {dispatch}",
    "pop r15",
    "pop r14",
    "pop r13",
    "pop r12",
    "pop r11",
    "pop r10",
    "pop r9",
    "pop r8",
    "pop rbp",
    "pop rdi",
    "pop rsi",
    "pop rdx",
    "pop rcx",
    "pop rbx",
    "pop rax",
    "add rsp, 16",
    "test qword ptr [rsp + 8], 3",
    "jz 2f",
    "swapgs",
    "2:",
    "iretq",
    dispatch = sym dispatch,
);

macro_rules! stub {
    ($name:ident, $vector:literal) => {
        #[unsafe(naked)]
        extern "C" fn $name() {
            naked_asm!("push 0", concat!("push ", $vector), "jmp interrupt_common");
        }
    };
    ($name:ident, $vector:literal, error) => {
        #[unsafe(naked)]
        extern "C" fn $name() {
            naked_asm!(concat!("push ", $vector), "jmp interrupt_common");
        }
    };
}

stub!(isr0, 0);
stub!(isr1, 1);
stub!(isr2, 2);
stub!(isr3, 3);
stub!(isr4, 4);
stub!(isr5, 5);
stub!(isr6, 6);
stub!(isr7, 7);
stub!(isr8, 8, error);
stub!(isr9, 9);
stub!(isr10, 10, error);
stub!(isr11, 11, error);
stub!(isr12, 12, error);
stub!(isr13, 13, error);
stub!(isr14, 14, error);
stub!(isr15, 15);
stub!(isr16, 16);
stub!(isr17, 17, error);
stub!(isr18, 18);
stub!(isr19, 19);
stub!(isr20, 20);
stub!(isr21, 21, error);
stub!(isr22, 22);
stub!(isr23, 23);
stub!(isr24, 24);
stub!(isr25, 25);
stub!(isr26, 26);
stub!(isr27, 27);
stub!(isr28, 28);
stub!(isr29, 29, error);
stub!(isr30, 30, error);
stub!(isr31, 31);
stub!(irq0, 32);
stub!(irq1, 33);
stub!(irq2, 34);
stub!(irq3, 35);
stub!(irq4, 36);
stub!(irq5, 37);
stub!(irq6, 38);
stub!(irq7, 39);
stub!(irq8, 40);
stub!(irq9, 41);
stub!(irq10, 42);
stub!(irq11, 43);
stub!(irq12, 44);
stub!(irq13, 45);
stub!(irq14, 46);
stub!(irq15, 47);
stub!(lapic_timer_stub, 48);
stub!(msi0, 64);
stub!(msi1, 65);
stub!(msi2, 66);
stub!(msi3, 67);
stub!(msi4, 68);
stub!(msi5, 69);
stub!(msi6, 70);
stub!(msi7, 71);
stub!(msi8, 72);
stub!(msi9, 73);
stub!(msi10, 74);
stub!(msi11, 75);
stub!(msi12, 76);
stub!(msi13, 77);
stub!(msi14, 78);
stub!(msi15, 79);
stub!(gsi0, 80);
stub!(gsi1, 81);
stub!(gsi2, 82);
stub!(gsi3, 83);
stub!(gsi4, 84);
stub!(gsi5, 85);
stub!(gsi6, 86);
stub!(gsi7, 87);
stub!(gsi8, 88);
stub!(gsi9, 89);
stub!(gsi10, 90);
stub!(gsi11, 91);
stub!(gsi12, 92);
stub!(gsi13, 93);
stub!(gsi14, 94);
stub!(gsi15, 95);
stub!(ipi_stub, 240);
stub!(tlb_stub, 241);
stub!(spurious_stub, 255);

pub const LAPIC_TIMER_VECTOR: u8 = 48;
pub const IPI_VECTOR: u8 = 240;
pub const TLB_VECTOR: u8 = 241;
pub const SPURIOUS_VECTOR: u8 = 255;
/// PCI MSI/MSI-X kesmeleri için ayrılan vektörler.
pub const MSI_BASE: u8 = 64;
const MSI_COUNT: usize = 16;

const MSI_STUBS: [extern "C" fn(); MSI_COUNT] = [
    msi0, msi1, msi2, msi3, msi4, msi5, msi6, msi7, msi8, msi9, msi10, msi11, msi12, msi13, msi14,
    msi15,
];

static MSI_HANDLERS: [AtomicUsize; MSI_COUNT] = [const { AtomicUsize::new(0) }; MSI_COUNT];

/// I/O APIC'in ISA dışı girişleri (GSI ≥ 16, PCI INTx) için vektörler.
pub const GSI_BASE: u8 = 80;
const GSI_COUNT: usize = 16;

const GSI_STUBS: [extern "C" fn(); GSI_COUNT] = [
    gsi0, gsi1, gsi2, gsi3, gsi4, gsi5, gsi6, gsi7, gsi8, gsi9, gsi10, gsi11, gsi12, gsi13, gsi14,
    gsi15,
];

/// Yuva başına GSI+1 (0 = boş) ve paylaşan en fazla 4 işleyici.
static GSI_LINES: [AtomicUsize; GSI_COUNT] = [const { AtomicUsize::new(0) }; GSI_COUNT];
static GSI_HANDLERS: [[AtomicUsize; HANDLERS_PER_IRQ]; GSI_COUNT] =
    [const { [const { AtomicUsize::new(0) }; HANDLERS_PER_IRQ] }; GSI_COUNT];

/// Bir GSI'yi (ör. ACPI _PRT'den PCI INTx) işleyiciye bağlar ve I/O APIC'te açar.
pub fn register_gsi(gsi: u32, level: bool, active_low: bool, handler: fn()) -> bool {
    if !super::apic::active() {
        return false;
    }
    let key = gsi as usize + 1;
    let slot = GSI_LINES
        .iter()
        .position(|l| l.load(Ordering::Acquire) == key)
        .or_else(|| {
            GSI_LINES.iter().position(|l| {
                l.compare_exchange(0, key, Ordering::AcqRel, Ordering::Acquire)
                    .is_ok()
            })
        });
    let Some(slot) = slot else {
        crate::log!("idt: no free vector for GSI {}", gsi);
        return false;
    };
    let value = handler as usize;
    let slots = &GSI_HANDLERS[slot];
    if !slots.iter().any(|s| s.load(Ordering::Acquire) == value)
        && !slots.iter().any(|s| {
            s.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        })
    {
        crate::log!("idt: GSI {} has no free handler slot", gsi);
        return false;
    }
    super::apic::route_gsi(gsi, GSI_BASE + slot as u8, level, active_low)
}

/// MSI için boş bir vektör ayırır ve işleyiciyi bağlar (LAPIC yoksa `None`).
pub fn alloc_msi_vector(handler: fn()) -> Option<u8> {
    if !super::apic::active() {
        return None;
    }
    let value = handler as usize;
    let slot = MSI_HANDLERS.iter().position(|s| {
        s.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    })?;
    Some(MSI_BASE + slot as u8)
}

const STUBS: [extern "C" fn(); VECTOR_COUNT] = [
    isr0, isr1, isr2, isr3, isr4, isr5, isr6, isr7, isr8, isr9, isr10, isr11, isr12, isr13, isr14,
    isr15, isr16, isr17, isr18, isr19, isr20, isr21, isr22, isr23, isr24, isr25, isr26, isr27,
    isr28, isr29, isr30, isr31, irq0, irq1, irq2, irq3, irq4, irq5, irq6, irq7, irq8, irq9, irq10,
    irq11, irq12, irq13, irq14, irq15,
];

const EXCEPTION_NAMES: [&str; 32] = [
    "Divide error",
    "Debug",
    "Non-maskable interrupt",
    "Breakpoint",
    "Overflow",
    "Bound range exceeded",
    "Invalid opcode",
    "Device not available",
    "Double fault",
    "Coprocessor segment overrun",
    "Invalid TSS",
    "Segment not present",
    "Stack-segment fault",
    "General protection fault",
    "Page fault",
    "Reserved",
    "x87 floating-point exception",
    "Alignment check",
    "Machine check",
    "SIMD floating-point exception",
    "Virtualization exception",
    "Control protection exception",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Reserved",
    "Hypervisor injection exception",
    "VMM communication exception",
    "Security exception",
    "Reserved",
];

fn init_call(_: &mut crate::kernel::initcall::Detail) -> Result<(), &'static str> {
    init();
    unsafe { asm!("int3") };
    Ok(())
}

crate::initcall!(arch, 20, "IDT / CPU exceptions", init_call);

fn init() {
    unsafe {
        for (vector, stub) in STUBS.iter().enumerate() {
            let ist = if vector == 8 {
                gdt::DOUBLE_FAULT_IST
            } else {
                0
            };
            IDT[vector] = IdtEntry::new(*stub as *const () as u64, ist);
        }
        IDT[LAPIC_TIMER_VECTOR as usize] = IdtEntry::new(lapic_timer_stub as *const () as u64, 0);
        for (i, stub) in MSI_STUBS.iter().enumerate() {
            IDT[MSI_BASE as usize + i] = IdtEntry::new(*stub as *const () as u64, 0);
        }
        for (i, stub) in GSI_STUBS.iter().enumerate() {
            IDT[GSI_BASE as usize + i] = IdtEntry::new(*stub as *const () as u64, 0);
        }
        IDT[IPI_VECTOR as usize] = IdtEntry::new(ipi_stub as *const () as u64, 0);
        IDT[TLB_VECTOR as usize] = IdtEntry::new(tlb_stub as *const () as u64, 0);
        IDT[SPURIOUS_VECTOR as usize] = IdtEntry::new(spurious_stub as *const () as u64, 0);
    }
    load();
}

/// IDT'yi geçerli CPU'ya yükler (ikincil çekirdekler de aynı tabloyu kullanır).
pub fn load() {
    unsafe {
        let pointer = DescriptorPointer {
            limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
            base: addr_of!(IDT) as u64,
        };
        asm!("lidt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
    }
}

pub fn register_irq(irq: u8, handler: fn()) {
    // Yalnızca ISA hatları (0-15) yönlendirilir; üstündekiler sürücü yoklamasıyla çalışır.
    let Some(slots) = IRQ_HANDLERS.get(irq as usize) else {
        crate::log!("idt: irq {} not routable, driver will poll", irq);
        return;
    };
    let value = handler as usize;
    if !slots.iter().any(|s| s.load(Ordering::Acquire) == value) {
        let free = slots.iter().find(|s| {
            s.compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
                .is_ok()
        });
        if free.is_none() {
            crate::log!("idt: irq {} has no free handler slot", irq);
            return;
        }
    }
    super::apic::unmask_irq(irq);
}

extern "C" fn dispatch(frame: &mut InterruptFrame) {
    let vector = frame.vector as u8;
    match vector {
        2 if crate::kernel::watchdog::nmi(frame) => {}
        0..IRQ_BASE => handle_exception(frame),
        LAPIC_TIMER_VECTOR => {
            super::apic::timer_interrupt();
            super::apic::eoi();
            if !super::smp::is_boot_cpu() {
                crate::sched::tick_secondary();
            } else if super::apic::bsp_lapic_tick() {
                crate::sched::tick();
            }
            crate::sched::preempt_point(frame.cs & 3 == 3);
        }
        IPI_VECTOR => {
            super::apic::ipi_received();
            super::smp::run_pending_call();
            super::apic::eoi();
            crate::sched::request_resched();
            crate::sched::preempt_point(frame.cs & 3 == 3);
        }
        TLB_VECTOR => {
            super::smp::tlb_flush_received();
            super::apic::eoi();
        }
        SPURIOUS_VECTOR => {}
        v if (GSI_BASE..GSI_BASE + GSI_COUNT as u8).contains(&v) => {
            for slot in &GSI_HANDLERS[(v - GSI_BASE) as usize] {
                let handler = slot.load(Ordering::Acquire);
                if handler != 0 {
                    let handler: fn() = unsafe { core::mem::transmute(handler) };
                    handler();
                }
            }
            super::apic::eoi();
            crate::sched::preempt_point(frame.cs & 3 == 3);
        }
        v if (MSI_BASE..MSI_BASE + MSI_COUNT as u8).contains(&v) => {
            let handler = MSI_HANDLERS[(v - MSI_BASE) as usize].load(Ordering::Acquire);
            if handler != 0 {
                let handler: fn() = unsafe { core::mem::transmute(handler) };
                handler();
            }
            super::apic::eoi();
            crate::sched::preempt_point(frame.cs & 3 == 3);
        }
        _ => {
            handle_irq(vector - IRQ_BASE);
            crate::sched::preempt_point(frame.cs & 3 == 3);
        }
    }
}

fn handle_irq(irq: u8) {
    if !super::apic::active() && pic::is_spurious(irq) {
        return;
    }
    let Some(slots) = IRQ_HANDLERS.get(irq as usize) else {
        return;
    };
    for slot in slots {
        let handler = slot.load(Ordering::Acquire);
        if handler != 0 {
            let handler: fn() = unsafe { core::mem::transmute(handler) };
            handler();
        }
    }
    if super::apic::active() {
        super::apic::eoi();
    } else {
        pic::end_of_interrupt(irq);
    }
}

fn handle_exception(frame: &mut InterruptFrame) {
    if frame.vector == 3 {
        crate::log!("idt: breakpoint at {:#018x}", frame.rip);
        return;
    }

    let name = EXCEPTION_NAMES[frame.vector as usize];
    const PF_PRESENT_WRITE: u64 = 0b11;
    if frame.vector == 14
        && frame.cs & 3 == 3
        && frame.error_code & PF_PRESENT_WRITE == PF_PRESENT_WRITE
        && crate::memory::paging::resolve_cow(
            crate::memory::paging::current_root(),
            cpu::read_cr2(),
        )
    {
        return;
    }
    if frame.cs & 3 == 3 {
        crate::log!(
            "user: killed by exception {} ({}) at rip={:#x} error={:#x} cr2={:#x}",
            frame.vector,
            name,
            frame.rip,
            frame.error_code,
            cpu::read_cr2()
        );
        crate::sched::exit_current(-(frame.vector as i64) - 128);
    }
    crate::log::emergency(format_args!(
        "\nEXCEPTION {} ({}) error={:#x}\n\
         rip={:#018x} cs={:#06x} rflags={:#010x} rsp={:#018x} ss={:#06x}\n\
         rax={:#018x} rbx={:#018x} rcx={:#018x} rdx={:#018x}\n\
         rsi={:#018x} rdi={:#018x} rbp={:#018x} r8 ={:#018x}\n\
         r9 ={:#018x} r10={:#018x} r11={:#018x} r12={:#018x}\n\
         r13={:#018x} r14={:#018x} r15={:#018x} cr2={:#018x} cr3={:#018x}\n",
        frame.vector,
        name,
        frame.error_code,
        frame.rip,
        frame.cs,
        frame.rflags,
        frame.rsp,
        frame.ss,
        frame.rax,
        frame.rbx,
        frame.rcx,
        frame.rdx,
        frame.rsi,
        frame.rdi,
        frame.rbp,
        frame.r8,
        frame.r9,
        frame.r10,
        frame.r11,
        frame.r12,
        frame.r13,
        frame.r14,
        frame.r15,
        cpu::read_cr2(),
        cpu::read_cr3(),
    ));
    crate::kernel::crash::raise(
        crate::kernel::crash::Report {
            kind: "exception",
            title: alloc::format!("Çekirdek istisnası: {} ({})", frame.vector, name),
            details: alloc::vec![
                alloc::format!(
                    "rip={:#018x} rsp={:#018x} rflags={:#x} error={:#x} cr2={:#x}",
                    frame.rip,
                    frame.rsp,
                    frame.rflags,
                    frame.error_code,
                    cpu::read_cr2()
                ),
                alloc::format!(
                    "rax={:#x} rbx={:#x} rcx={:#x} rdx={:#x} rsi={:#x} rdi={:#x} rbp={:#x}",
                    frame.rax,
                    frame.rbx,
                    frame.rcx,
                    frame.rdx,
                    frame.rsi,
                    frame.rdi,
                    frame.rbp
                ),
            ],
        },
        true,
    );
    cpu::halt_forever();
}
