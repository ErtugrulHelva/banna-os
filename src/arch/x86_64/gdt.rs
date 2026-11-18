//! GDT ve TSS: çekirdek/kullanıcı segmentleri, double fault için ayrı yığın.
//! Her CPU'nun kendi GDT/TSS kopyası vardır.

use alloc::boxed::Box;
use core::arch::asm;
use core::mem::size_of;
use core::ptr::addr_of_mut;

pub const KERNEL_CODE: u16 = 0x08;
pub const KERNEL_DATA: u16 = 0x10;
pub const USER_DATA: u16 = 0x18 | 3;
pub const USER_CODE: u16 = 0x20 | 3;
pub const TSS_SELECTOR: u16 = 0x28;

pub const DOUBLE_FAULT_IST: u8 = 1;

const IST_STACK_SIZE: usize = 16 * 1024;

#[repr(C, align(16))]
struct Stack([u8; IST_STACK_SIZE]);

#[repr(C, packed)]
struct TaskStateSegment {
    reserved0: u32,
    rsp: [u64; 3],
    reserved1: u64,
    ist: [u64; 7],
    reserved2: u64,
    reserved3: u16,
    iomap_base: u16,
}

#[repr(C, align(16))]
pub struct CpuTables {
    gdt: [u64; 7],
    tss: TaskStateSegment,
    double_fault_stack: Stack,
}

const TEMPLATE_GDT: [u64; 7] = [
    0,
    0x00af_9a00_0000_ffff,
    0x00cf_9200_0000_ffff,
    0x00cf_f200_0000_ffff,
    0x00af_fa00_0000_ffff,
    0,
    0,
];

impl CpuTables {
    const fn new() -> Self {
        Self {
            gdt: TEMPLATE_GDT,
            tss: TaskStateSegment {
                reserved0: 0,
                rsp: [0; 3],
                reserved1: 0,
                ist: [0; 7],
                reserved2: 0,
                reserved3: 0,
                iomap_base: size_of::<TaskStateSegment>() as u16,
            },
            double_fault_stack: Stack([0; IST_STACK_SIZE]),
        }
    }

    pub fn boxed() -> &'static mut Self {
        let mut tables = Box::<Self>::new_uninit();
        unsafe {
            tables.as_mut_ptr().write(Self::new());
            Box::leak(tables.assume_init())
        }
    }

    /// Bu tabloları geçerli CPU'ya yükler (lgdt, segmentler, ltr).
    pub fn load(&'static mut self) {
        let stack_top = self.double_fault_stack.0.as_ptr() as u64 + IST_STACK_SIZE as u64;
        self.tss.ist[(DOUBLE_FAULT_IST - 1) as usize] = stack_top;
        let base = &self.tss as *const _ as u64;
        let limit = (size_of::<TaskStateSegment>() - 1) as u64;
        self.gdt[5] = (limit & 0xffff)
            | ((base & 0xff_ffff) << 16)
            | (0x89 << 40)
            | (((limit >> 16) & 0xf) << 48)
            | (((base >> 24) & 0xff) << 56);
        self.gdt[6] = base >> 32;

        let pointer = DescriptorPointer {
            limit: (size_of::<[u64; 7]>() - 1) as u16,
            base: self.gdt.as_ptr() as u64,
        };
        unsafe {
            asm!("lgdt [{}]", in(reg) &pointer, options(readonly, nostack, preserves_flags));
            asm!(
                "push {code}",
                "lea {tmp}, [rip + 2f]",
                "push {tmp}",
                "retfq",
                "2:",
                "mov ds, {data:x}",
                "mov es, {data:x}",
                "mov ss, {data:x}",
                "xor {tmp:e}, {tmp:e}",
                "mov fs, {tmp:x}",
                "mov gs, {tmp:x}",
                code = in(reg) KERNEL_CODE as u64,
                data = in(reg) KERNEL_DATA as u64,
                tmp = out(reg) _,
                options(preserves_flags),
            );
            asm!("ltr {:x}", in(reg) TSS_SELECTOR, options(nostack, preserves_flags));
        }
    }

    pub fn set_kernel_stack(&mut self, top: u64) {
        self.tss.rsp[0] = top;
    }
}

#[repr(C, packed)]
struct DescriptorPointer {
    limit: u16,
    base: u64,
}

static mut BOOT_CPU: CpuTables = CpuTables::new();

fn init_call(_: &mut crate::kernel::initcall::Detail) -> Result<(), &'static str> {
    unsafe { (*addr_of_mut!(BOOT_CPU)).load() };
    Ok(())
}

crate::initcall!(arch, 10, "GDT / TSS", init_call);

pub fn boot_tables() -> *mut CpuTables {
    addr_of_mut!(BOOT_CPU)
}
