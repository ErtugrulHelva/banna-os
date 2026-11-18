//! Çok çekirdek: ikincil çekirdeklerin (AP) başlatılması. Limine MP yanıtı varsa onunla,
//! yoksa (Banna UEFI önyükleyicisi) MADT'deki LAPIC'lere INIT-SIPI-SIPI gönderilerek: AP,
//! 1 MiB altına kopyalanan başlatma kodunda gerçek kipten doğrudan uzun kipe geçer.
//!
//! Her AP kendi GDT/TSS'ini, IDT'yi, CPU özelliklerini ve LAPIC zamanlayıcısını
//! kurar, sonra kesme bekleyerek boşta durur. Zamanlayıcı (scheduler) şimdilik
//! yalnızca önyükleme çekirdeğinde iş çalıştırır.

use core::arch::global_asm;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};

use super::gdt::CpuTables;
use super::{apic, cpu, fpu, idt, tsc};
use crate::boot::{self, limine::MpInfo};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::memory::paging;

const MAX_CPUS: usize = 256;
const AP_TIMER_HZ: u64 = 1000;

struct ApStart {
    tables: *mut CpuTables,
    index: usize,
}

static TLB_ACKS: AtomicU64 = AtomicU64::new(0);

static BSP_LAPIC: AtomicU32 = AtomicU32::new(u32::MAX);
static ONLINE: [AtomicBool; MAX_CPUS] = [const { AtomicBool::new(false) }; MAX_CPUS];
static STARTED: AtomicU32 = AtomicU32::new(0);

pub fn boot_lapic_id() -> u32 {
    match BSP_LAPIC.load(Ordering::Relaxed) {
        u32::MAX => apic::lapic_id(),
        id => id,
    }
}

pub fn is_boot_cpu() -> bool {
    // BSP_LAPIC ayarlıysa her çekirdeğin GS per-CPU alanı kuruludur; LAPIC MMIO okunmaz.
    BSP_LAPIC.load(Ordering::Relaxed) == u32::MAX
        || !apic::active()
        || super::syscall::cpu_index() == 0
}

pub struct CpuStatus {
    pub lapic_id: u32,
    pub boot: bool,
}

pub fn cpus() -> alloc::vec::Vec<CpuStatus> {
    let bsp = boot_lapic_id();
    (0..MAX_CPUS)
        .filter(|&id| ONLINE[id].load(Ordering::Relaxed) || id as u32 == bsp)
        .map(|id| CpuStatus {
            lapic_id: id as u32,
            boot: id as u32 == bsp,
        })
        .collect()
}

pub fn online_count() -> usize {
    1 + (0..MAX_CPUS)
        .filter(|&i| ONLINE[i].load(Ordering::Relaxed))
        .count()
}

extern "C" fn ap_entry(info: *const MpInfo) -> ! {
    let info = unsafe { &*info };
    ap_main(unsafe { &*(info.extra_argument as *const ApStart) })
}

/// Kendi başlatma kodumuzdan gelen AP: önce asıl çekirdek adres alanına geçer.
extern "C" fn ap_native_entry(start: *const ApStart) -> ! {
    paging::switch_address_space(paging::kernel_root());
    ap_main(unsafe { &*start })
}

fn ap_main(start: &ApStart) -> ! {
    let tables = unsafe { &mut *start.tables };
    tables.load();
    idt::load();
    super::syscall::init_cpu(start.index, start.tables);
    paging::enable_cpu_features();
    fpu::enable_local();
    apic::init_local();
    apic::start_timer(AP_TIMER_HZ);
    let id = apic::lapic_id() as usize;
    if id < MAX_CPUS {
        ONLINE[id].store(true, Ordering::Release);
    }
    STARTED.fetch_add(1, Ordering::Release);
    cpu::enable_interrupts();
    while !crate::sched::is_running() {
        cpu::halt();
    }
    crate::sched::register_cpu(apic::lapic_id());
    loop {
        cpu::enable_interrupts();
        cpu::halt();
    }
}

// AP başlatma kodu: 16 bit gerçek kip → (PAE, LME, PG) → 64 bit. Sayfa başından konumdan
// bağımsızdır; sondaki alanlar kopyalandıktan sonra doldurulur.
global_asm!(
    ".pushsection .rodata.ap_trampoline, \"a\"",
    ".global ap_trampoline_start",
    ".global ap_trampoline_end",
    ".global ap_gdtr",
    ".global ap_cr3",
    ".global ap_efer",
    ".global ap_ljmp",
    ".global ap_stack",
    ".global ap_entry_ptr",
    ".global ap_arg",
    ".global ap_long",
    ".code16",
    "ap_trampoline_start:",
    "cli",
    "cld",
    "mov ax, cs",
    "mov ds, ax",
    // lgdt [ap_gdtr]
    ".byte 0x0f, 0x01, 0x16",
    ".word ap_gdtr - ap_trampoline_start",
    "mov eax, cr4",
    "or eax, 0x20",
    "mov cr4, eax",
    // mov eax, [ap_cr3]
    ".byte 0x66, 0xa1",
    ".word ap_cr3 - ap_trampoline_start",
    "mov cr3, eax",
    "mov ecx, 0xc0000080",
    "rdmsr",
    // or eax, [ap_efer]
    ".byte 0x66, 0x0b, 0x06",
    ".word ap_efer - ap_trampoline_start",
    "wrmsr",
    "mov eax, cr0",
    "or eax, 0x80000001",
    "mov cr0, eax",
    // jmp far dword [ap_ljmp]: 64 bit kod seçicisine geçiş
    ".byte 0x66, 0xff, 0x2e",
    ".word ap_ljmp - ap_trampoline_start",
    ".code64",
    "ap_long:",
    "mov ax, 0x10",
    "mov ds, ax",
    "mov es, ax",
    "mov ss, ax",
    "xor eax, eax",
    "mov fs, ax",
    "mov gs, ax",
    "mov rsp, qword ptr [rip + ap_stack]",
    "mov rdi, qword ptr [rip + ap_arg]",
    "mov rax, qword ptr [rip + ap_entry_ptr]",
    "jmp rax",
    ".balign 8",
    "ap_gdt:",
    ".quad 0",
    ".quad 0x00af9a000000ffff",
    ".quad 0x00cf92000000ffff",
    "ap_gdtr:",
    ".word 23",
    ".long 0",
    ".balign 4",
    "ap_cr3: .long 0",
    "ap_efer: .long 0",
    "ap_ljmp: .long 0",
    ".word 0x08",
    ".balign 8",
    "ap_stack: .quad 0",
    "ap_entry_ptr: .quad 0",
    "ap_arg: .quad 0",
    "ap_trampoline_end:",
    ".popsection",
);

unsafe extern "C" {
    static ap_trampoline_start: u8;
    static ap_trampoline_end: u8;
    static ap_gdtr: u8;
    static ap_cr3: u8;
    static ap_efer: u8;
    static ap_ljmp: u8;
    static ap_stack: u8;
    static ap_entry_ptr: u8;
    static ap_arg: u8;
    static ap_long: u8;
}

/// Başlatma kodu ve onun sayfa tabloları için 1 MiB altındaki dört ardışık boş sayfa.
fn low_pages() -> Option<u64> {
    let memmap = boot::MEMMAP.response()?;
    memmap
        .entries()
        .filter(|e| e.kind == boot::limine::MemoryKind::Usable)
        .filter_map(|e| {
            let start = e.base.max(0x1000).next_multiple_of(0x1000);
            let end = (e.base + e.length).min(0x9f000) & !0xfff;
            (end >= start + 4 * 0x1000).then_some(end - 4 * 0x1000)
        })
        .max()
}

fn offset_of(field: *const u8) -> usize {
    field as usize - (&raw const ap_trampoline_start) as usize
}

fn delay_us(us: u64) {
    let end = tsc::now_ns() + us * 1000;
    while tsc::now_ns() < end {
        core::hint::spin_loop();
    }
}

/// MADT'deki AP'leri kendi başlatma kodumuzla uyandırır; başlayan sayısını döndürür.
fn start_native(detail: &mut Detail) -> Result<(), &'static str> {
    let bsp = apic::lapic_id();
    BSP_LAPIC.store(bsp, Ordering::Relaxed);
    let Some(madt) = crate::drivers::acpi::with(|a| a.madt).flatten() else {
        let _ = write!(detail, "single CPU (no MADT)");
        return Ok(());
    };
    let targets: alloc::vec::Vec<u32> = madt.lapic_ids[..madt.lapic_count]
        .iter()
        .copied()
        .filter(|&id| id != bsp)
        .take(super::syscall::MAX_CPUS - 1)
        .collect();
    if targets.is_empty() || !apic::active() {
        let _ = write!(detail, "single CPU");
        return Ok(());
    }
    let Some(base) = low_pages() else {
        let _ = write!(detail, "single CPU (no low memory for AP start-up)");
        return Ok(());
    };
    let (code, pml4, pdpt, pd) = (base, base + 0x1000, base + 0x2000, base + 0x3000);
    let virt = |phys: u64| crate::memory::phys_to_virt(phys);
    unsafe {
        let start = &raw const ap_trampoline_start;
        let len = (&raw const ap_trampoline_end) as usize - start as usize;
        core::ptr::write_bytes(virt(code), 0, 4 * 0x1000);
        core::ptr::copy_nonoverlapping(start, virt(code), len);
        // Alt 2 MiB kimlik eşlemesi + çekirdek yarısı (asıl tablolarla paylaşılır).
        let kernel = &*(virt(paging::kernel_root()) as *const [u64; 512]);
        let tables = &mut *(virt(pml4) as *mut [u64; 512]);
        tables[256..].copy_from_slice(&kernel[256..]);
        tables[0] = pdpt | 3;
        (*(virt(pdpt) as *mut [u64; 512]))[0] = pd | 3;
        (*(virt(pd) as *mut [u64; 512]))[0] = 0x83;
        let field = |sym: *const u8| virt(code).add(offset_of(sym));
        (field(&raw const ap_gdtr).add(2) as *mut u32)
            .write_unaligned(code as u32 + offset_of(&raw const ap_gdtr) as u32 - 24);
        (field(&raw const ap_cr3) as *mut u32).write_unaligned(pml4 as u32);
        let nx = cpu::rdmsr(cpu::MSR_EFER) & (1 << 11);
        (field(&raw const ap_efer) as *mut u32).write_unaligned((1 << 8) | nx as u32);
        (field(&raw const ap_ljmp) as *mut u32)
            .write_unaligned(code as u32 + offset_of(&raw const ap_long) as u32);
        (field(&raw const ap_entry_ptr) as *mut u64)
            .write_unaligned(ap_native_entry as *const () as u64);
    }

    let mut started = 0u32;
    for (i, &lapic) in targets.iter().enumerate() {
        let stack = alloc::vec![0u8; 64 * 1024].leak();
        let top = (stack.as_ptr() as u64 + stack.len() as u64) & !0xf;
        let start = alloc::boxed::Box::leak(alloc::boxed::Box::new(ApStart {
            tables: CpuTables::boxed(),
            index: i + 1,
        }));
        unsafe {
            let field = |sym: *const u8| virt(code).add(offset_of(sym));
            (field(&raw const ap_stack) as *mut u64).write_unaligned(top);
            (field(&raw const ap_arg) as *mut u64).write_unaligned(start as *mut ApStart as u64);
        }
        let before = STARTED.load(Ordering::Acquire);
        apic::send_raw(lapic, 0x4500);
        delay_us(10_000);
        let vector = (code >> 12) as u32;
        for _ in 0..2 {
            if STARTED.load(Ordering::Acquire) > before {
                break;
            }
            apic::send_raw(lapic, 0x4600 | vector);
            delay_us(200);
        }
        let deadline = tsc::now_ns() + 1_000_000_000;
        while STARTED.load(Ordering::Acquire) == before && tsc::now_ns() < deadline {
            core::hint::spin_loop();
        }
        if STARTED.load(Ordering::Acquire) > before {
            started += 1;
        } else {
            // Geç uyanıp sonraki çekirdeğin yığınını kullanmasın diye yeniden INIT'te tutulur.
            apic::send_raw(lapic, 0x4500);
            crate::log!("smp: CPU with LAPIC {} did not start", lapic);
        }
    }
    finish(detail, started, targets.len() as u32)
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let Some(mp) = boot::MP.response() else {
        return start_native(detail);
    };
    BSP_LAPIC.store(mp.bsp_lapic_id, Ordering::Relaxed);
    if !apic::active() {
        let _ = write!(detail, "APIC inactive, secondary CPUs left parked");
        return Ok(());
    }

    let mut expected: u32 = 0;
    for info in mp.cpus() {
        if info.lapic_id == mp.bsp_lapic_id {
            continue;
        }
        if expected as usize + 1 >= super::syscall::MAX_CPUS {
            break;
        }
        let start = alloc::boxed::Box::leak(alloc::boxed::Box::new(ApStart {
            tables: CpuTables::boxed(),
            index: expected as usize + 1,
        }));
        info.extra_argument = start as *mut ApStart as u64;
        info.goto_address
            .store(ap_entry as *const () as u64, Ordering::SeqCst);
        expected += 1;
    }

    let start = tsc::now_ns();
    while STARTED.load(Ordering::Acquire) < expected && tsc::now_ns() < start + 1_000_000_000 {
        core::hint::spin_loop();
    }
    finish(detail, STARTED.load(Ordering::Acquire), expected)
}

/// IPI'ları sınar, sonucu raporlar.
fn finish(detail: &mut Detail, started: u32, expected: u32) -> Result<(), &'static str> {
    let mut ipi_ok = true;
    if started > 0 {
        let before = apic::ipi_count();
        for status in cpus().iter().filter(|c| !c.boot) {
            apic::send_ipi(status.lapic_id, idt::IPI_VECTOR);
        }
        let start = tsc::now_ns();
        while apic::ipi_count() < before + started as u64 && tsc::now_ns() < start + 100_000_000 {
            core::hint::spin_loop();
        }
        ipi_ok = apic::ipi_count() >= before + started as u64;
    }

    crate::log!(
        "smp: {} of {} secondary CPU(s) online, IPI {}",
        started,
        expected,
        if ipi_ok { "ok" } else { "lost" }
    );
    // Açılmayan bir çekirdek sistemi durdurmaz; açılanlarla devam edilir.
    check("smp", ipi_ok)?;
    let _ = write!(detail, "{} CPU(s) online", started + 1);
    let efficient = crate::sched::efficiency_cpus() as u32;
    if efficient > 0 {
        let _ = write!(
            detail,
            " ({} P + {} E, hybrid)",
            started + 1 - efficient,
            efficient
        );
    }
    if started < expected {
        let _ = write!(detail, ", {} failed to start", expected - started);
    }
    Ok(())
}

crate::initcall!(core, 57, "SMP (secondary CPUs)", init);

static CALL: AtomicU64 = AtomicU64::new(0);
static CALL_GENERATION: AtomicU64 = AtomicU64::new(0);
static CALL_ACKS: AtomicU64 = AtomicU64::new(0);
static CALL_SEEN: [AtomicU64; MAX_CPUS] = [const { AtomicU64::new(0) }; MAX_CPUS];
static CALL_SERIAL: crate::sync::SpinLock<()> = crate::sync::SpinLock::new(());

/// IPI geldiğinde bekleyen "tüm çekirdeklerde çalıştır" çağrısını yürütür.
pub fn run_pending_call() {
    let id = apic::lapic_id() as usize;
    if id >= MAX_CPUS {
        return;
    }
    let generation = CALL_GENERATION.load(Ordering::Acquire);
    if CALL_SEEN[id].swap(generation, Ordering::AcqRel) == generation {
        return;
    }
    let f = CALL.load(Ordering::Acquire);
    if f != 0 {
        let f: fn() = unsafe { core::mem::transmute(f as usize) };
        f();
    }
    CALL_ACKS.fetch_add(1, Ordering::AcqRel);
}

/// `f`'yi önce bu çekirdekte, sonra IPI ile diğer tüm çevrimiçi çekirdeklerde çalıştırır
/// (MSR gibi çekirdeğe özel ayarlar için) ve hepsini bekler.
pub fn run_on_all(f: fn()) {
    let _serial = CALL_SERIAL.lock();
    let me = apic::lapic_id();
    let others: alloc::vec::Vec<u32> = cpus()
        .iter()
        .map(|c| c.lapic_id)
        .filter(|&id| id != me)
        .collect();
    CALL.store(f as usize as u64, Ordering::Release);
    let generation = CALL_GENERATION.fetch_add(1, Ordering::AcqRel) + 1;
    if (me as usize) < MAX_CPUS {
        CALL_SEEN[me as usize].store(generation, Ordering::Release);
    }
    CALL_ACKS.store(0, Ordering::Release);
    f();
    if !apic::active() || others.is_empty() {
        return;
    }
    for &lapic in &others {
        apic::send_ipi(lapic, idt::IPI_VECTOR);
    }
    let deadline = tsc::now_ns() + 100_000_000;
    while CALL_ACKS.load(Ordering::Acquire) < others.len() as u64 && tsc::now_ns() < deadline {
        core::hint::spin_loop();
    }
}

pub fn tlb_flush_received() {
    unsafe { core::arch::asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _, options(nostack)) };
    TLB_ACKS.fetch_add(1, Ordering::Release);
}

/// Diğer çekirdeklere TLB'lerini boşaltmalarını söyler (çekirdek eşlemesi kaldırıldıktan sonra).
pub fn flush_tlb_others() {
    if !apic::active() {
        return;
    }
    let me = apic::lapic_id();
    let targets: alloc::vec::Vec<u32> = cpus()
        .iter()
        .filter(|c| c.lapic_id != me)
        .map(|c| c.lapic_id)
        .collect();
    if targets.is_empty() {
        return;
    }
    let before = TLB_ACKS.load(Ordering::Acquire);
    for lapic in &targets {
        apic::send_ipi(*lapic, idt::TLB_VECTOR);
    }
    let deadline = tsc::now_ns() + 100_000_000;
    while TLB_ACKS.load(Ordering::Acquire) < before + targets.len() as u64
        && tsc::now_ns() < deadline
    {
        core::hint::spin_loop();
    }
}
