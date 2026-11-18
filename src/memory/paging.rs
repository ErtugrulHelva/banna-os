//! 4 seviyeli sayfa tabloları, adres alanları ve bellek tipleri (PAT).
//!
//! Çekirdek yarısının (PML4 256..511) tüm girdileri açılışta önceden ayrılır;
//! böylece her adres alanı aynı çekirdek tablolarını paylaşır ve sonradan
//! eklenen çekirdek eşlemeleri her yerde görünür.

use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use super::{PAGE_SIZE, align_down, align_up, hhdm_offset, phys_to_virt, pmm};
use crate::arch::cpu::{self, CpuidRegister};
use crate::kernel::initcall::{Detail, self_test as check};

pub const PRESENT: u64 = 1 << 0;
pub const WRITABLE: u64 = 1 << 1;
pub const USER: u64 = 1 << 2;
pub const WRITE_THROUGH: u64 = 1 << 3;
pub const NO_CACHE: u64 = 1 << 4;
pub const HUGE: u64 = 1 << 7;
/// Yazılımın kullandığı bit: sayfa paylaşılıyor, ilk yazmada kopyalanacak.
pub const COW: u64 = 1 << 9;
pub const PAT_4K: u64 = 1 << 7;
pub const NO_EXECUTE: u64 = 1 << 63;

/// PAT girdisi 5 = write-combining (framebuffer için).
pub const WRITE_COMBINING: u64 = PAT_4K | WRITE_THROUGH;

const ADDRESS_MASK: u64 = 0x000f_ffff_ffff_f000;
const EFER_NXE: u64 = 1 << 11;
const CR4_SMEP: u64 = 1 << 20;
const CR4_SMAP: u64 = 1 << 21;
const CR4_PGE: u64 = 1 << 7;
const MSR_PAT: u32 = 0x277;
const PAT_LAYOUT: u64 = 0x0007_0105_0007_0406;
pub const USER_SPACE_END: u64 = 0x0000_8000_0000_0000;

static NX_ENABLED: AtomicBool = AtomicBool::new(false);
static SMAP_ENABLED: AtomicBool = AtomicBool::new(false);
static PAT_ENABLED: AtomicBool = AtomicBool::new(false);
static KERNEL_ROOT: AtomicU64 = AtomicU64::new(0);
/// Sayfa tablosu değişikliklerini çekirdekler arasında sıralar.
static TABLES: crate::sync::SpinLock<()> = crate::sync::SpinLock::new(());

#[derive(Debug)]
pub enum MapError {
    OutOfMemory,
    AlreadyMapped,
    HugePageInTheWay,
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let nx = cpu::has_feature(0x8000_0001, CpuidRegister::Edx, 20);
    if nx {
        let efer = cpu::rdmsr(cpu::MSR_EFER);
        cpu::wrmsr(cpu::MSR_EFER, efer | EFER_NXE);
    }
    NX_ENABLED.store(nx, Ordering::Relaxed);

    let smep = cpu::has_feature(7, CpuidRegister::Ebx, 7);
    let smap = cpu::has_feature(7, CpuidRegister::Ebx, 20);
    let mut cr4 = cpu::read_cr4();
    if smep {
        cr4 |= CR4_SMEP;
    }
    if smap {
        cr4 |= CR4_SMAP;
    }
    unsafe { cpu::write_cr4(cr4) };
    SMAP_ENABLED.store(smap, Ordering::Relaxed);
    SMEP_ENABLED.store(smep, Ordering::Relaxed);

    let pat = cpu::has_feature(1, CpuidRegister::Edx, 16);
    if pat {
        cpu::wrmsr(MSR_PAT, PAT_LAYOUT);
        flush_tlb_all();
    }
    PAT_ENABLED.store(pat, Ordering::Relaxed);

    let root = cpu::read_cr3() & ADDRESS_MASK;
    KERNEL_ROOT.store(root, Ordering::Relaxed);
    let pml4 = table(root);
    // Önyükleyicinin geçiş için bıraktığı kimlik eşlemeleri (GDT/IDT artık çekirdeğin).
    pml4[..256].fill(0);
    flush_tlb_all();
    let mut reserved = 0;
    for entry in pml4[256..].iter_mut() {
        if *entry & PRESENT == 0 {
            let frame = pmm::alloc_zeroed_frame().ok_or("out of memory for kernel tables")?;
            *entry = frame | PRESENT | WRITABLE;
            reserved += 1;
        }
    }

    let on = |flag: bool| if flag { "on" } else { "off" };
    crate::log!(
        "paging: NX {}, SMEP {}, SMAP {}, PAT/WC {}, {} kernel PML4 slots pre-allocated",
        on(nx),
        on(smep),
        on(smap),
        on(pat),
        reserved
    );
    let _ = write!(
        detail,
        "NX {}, SMEP {}, SMAP {}, WC {}",
        on(nx),
        on(smep),
        on(smap),
        on(pat)
    );
    check("paging", self_test())
}

crate::initcall!(core, 20, "Paging", init);

static SMEP_ENABLED: AtomicBool = AtomicBool::new(false);

/// Önyükleme CPU'sunda seçilen NX/SMEP/SMAP/PAT ayarlarını geçerli CPU'ya uygular.
pub fn enable_cpu_features() {
    if NX_ENABLED.load(Ordering::Relaxed) {
        let efer = cpu::rdmsr(cpu::MSR_EFER);
        cpu::wrmsr(cpu::MSR_EFER, efer | EFER_NXE);
    }
    let mut cr4 = cpu::read_cr4();
    if SMEP_ENABLED.load(Ordering::Relaxed) {
        cr4 |= CR4_SMEP;
    }
    if SMAP_ENABLED.load(Ordering::Relaxed) {
        cr4 |= CR4_SMAP;
    }
    unsafe { cpu::write_cr4(cr4) };
    if PAT_ENABLED.load(Ordering::Relaxed) {
        cpu::wrmsr(MSR_PAT, PAT_LAYOUT);
    }
}

pub fn smap_enabled() -> bool {
    SMAP_ENABLED.load(Ordering::Relaxed)
}

pub fn write_combining_available() -> bool {
    PAT_ENABLED.load(Ordering::Relaxed)
}

pub fn kernel_root() -> u64 {
    KERNEL_ROOT.load(Ordering::Relaxed)
}

pub fn current_root() -> u64 {
    cpu::read_cr3() & ADDRESS_MASK
}

fn flush_tlb_all() {
    let cr4 = cpu::read_cr4();
    unsafe {
        cpu::write_cr4(cr4 & !CR4_PGE);
        cpu::write_cr4(cr4);
        core::arch::asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _, options(nostack));
    }
}

fn table(phys: u64) -> &'static mut [u64; 512] {
    unsafe { &mut *(phys_to_virt(phys) as *mut [u64; 512]) }
}

fn indices(virt: u64) -> [usize; 4] {
    [
        ((virt >> 39) & 0x1ff) as usize,
        ((virt >> 30) & 0x1ff) as usize,
        ((virt >> 21) & 0x1ff) as usize,
        ((virt >> 12) & 0x1ff) as usize,
    ]
}

fn sanitize(flags: u64) -> u64 {
    let mut flags = flags;
    if !NX_ENABLED.load(Ordering::Relaxed) {
        flags &= !NO_EXECUTE;
    }
    if !PAT_ENABLED.load(Ordering::Relaxed) && flags & PAT_4K != 0 {
        flags &= !(PAT_4K | WRITE_THROUGH);
        flags |= NO_CACHE;
    }
    flags
}

fn invalidate(root: u64, virt: u64) {
    if root == current_root() || virt >= USER_SPACE_END {
        cpu::invlpg(virt);
    }
}

pub fn map_page_in(root: u64, virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
    let _tables = TABLES.lock();
    let idx = indices(virt);
    let parent_flags = PRESENT | WRITABLE | (flags & USER);
    let mut table_phys = root;

    for &index in &idx[..3] {
        let entries = table(table_phys);
        let entry = entries[index];
        if entry & PRESENT == 0 {
            let new_table = pmm::alloc_zeroed_frame().ok_or(MapError::OutOfMemory)?;
            entries[index] = new_table | parent_flags;
            table_phys = new_table;
        } else if entry & HUGE != 0 {
            return Err(MapError::HugePageInTheWay);
        } else {
            entries[index] = entry | parent_flags;
            table_phys = entry & ADDRESS_MASK;
        }
    }

    let entries = table(table_phys);
    if entries[idx[3]] & PRESENT != 0 {
        return Err(MapError::AlreadyMapped);
    }
    entries[idx[3]] = (phys & ADDRESS_MASK) | sanitize(flags) | PRESENT;
    invalidate(root, virt);
    Ok(())
}

pub fn unmap_page_in(root: u64, virt: u64) -> Option<u64> {
    let _tables = TABLES.lock();
    let idx = indices(virt);
    let mut table_phys = root;
    for &index in &idx[..3] {
        let entry = table(table_phys)[index];
        if entry & PRESENT == 0 || entry & HUGE != 0 {
            return None;
        }
        table_phys = entry & ADDRESS_MASK;
    }
    let entries = table(table_phys);
    let entry = entries[idx[3]];
    if entry & PRESENT == 0 {
        return None;
    }
    entries[idx[3]] = 0;
    invalidate(root, virt);
    Some(entry & ADDRESS_MASK)
}

pub fn translate_in(root: u64, virt: u64) -> Option<u64> {
    let idx = indices(virt);
    let mut table_phys = root;
    for (level, &index) in idx.iter().enumerate() {
        let entry = table(table_phys)[index];
        if entry & PRESENT == 0 {
            return None;
        }
        if level == 3 || (level > 0 && entry & HUGE != 0) {
            let page_size = 1u64 << (12 + 9 * (3 - level));
            let base = entry & ADDRESS_MASK & !(page_size - 1);
            return Some(base + (virt & (page_size - 1)));
        }
        table_phys = entry & ADDRESS_MASK;
    }
    None
}

pub fn map_page(virt: u64, phys: u64, flags: u64) -> Result<(), MapError> {
    map_page_in(current_root(), virt, phys, flags)
}

pub fn unmap_page(virt: u64) -> Option<u64> {
    unmap_page_in(current_root(), virt)
}

pub fn translate(virt: u64) -> Option<u64> {
    translate_in(current_root(), virt)
}

pub fn user_accessible(virt: u64, write: bool) -> bool {
    if virt >= USER_SPACE_END {
        return false;
    }
    let idx = indices(virt);
    let mut table_phys = current_root();
    for (level, &index) in idx.iter().enumerate() {
        let entry = table(table_phys)[index];
        if entry & PRESENT == 0 || entry & USER == 0 || (write && entry & WRITABLE == 0) {
            return false;
        }
        if level == 3 || (level > 0 && entry & HUGE != 0) {
            return true;
        }
        table_phys = entry & ADDRESS_MASK;
    }
    false
}

/// Fiziksel aralığın HHDM penceresinde eşli olmasını sağlar (ACPI tabloları, MMIO).
pub fn map_physical(phys: u64, len: u64, flags: u64) -> Result<*mut u8, MapError> {
    let start = align_down(phys, PAGE_SIZE);
    let end = align_up(phys + len.max(1), PAGE_SIZE);
    let mut page = start;
    while page < end {
        let virt = page + hhdm_offset();
        if translate(virt).is_none() {
            map_page(virt, page, WRITABLE | NO_EXECUTE | flags)?;
        }
        page += PAGE_SIZE;
    }
    Ok(phys_to_virt(phys))
}

/// Kullanıcı yarısı boş, çekirdek yarısı paylaşılan yeni bir PML4.
pub fn create_address_space() -> Option<u64> {
    let root = pmm::alloc_zeroed_frame()?;
    let kernel = table(kernel_root());
    table(root)[256..].copy_from_slice(&kernel[256..]);
    Some(root)
}

/// Kullanıcı yarısındaki tüm sayfaları ve tabloları serbest bırakır, sonra PML4'ü.
pub fn destroy_address_space(root: u64) {
    let _tables = TABLES.lock();
    if root == kernel_root() {
        return;
    }
    fn free_level(table_phys: u64, level: usize) {
        for &entry in table(table_phys).iter() {
            if entry & PRESENT == 0 {
                continue;
            }
            let phys = entry & ADDRESS_MASK;
            if level < 3 && entry & HUGE == 0 {
                free_level(phys, level + 1);
            }
            if level == 3 || entry & HUGE == 0 {
                pmm::free_frame(phys);
            }
        }
    }
    for &entry in table(root)[..256].iter() {
        if entry & PRESENT != 0 {
            free_level(entry & ADDRESS_MASK, 1);
            pmm::free_frame(entry & ADDRESS_MASK);
        }
    }
    pmm::free_frame(root);
}

/// `fork` için kullanıcı yarısını copy-on-write olarak paylaşan yeni bir adres alanı.
pub fn fork_address_space(parent: u64) -> Option<u64> {
    let child = create_address_space()?;
    let copied = {
        let _tables = TABLES.lock();
        fn copy_level(from: u64, to: u64, level: usize, limit: usize) -> bool {
            let source = table(from);
            let target = table(to);
            for i in 0..limit {
                let entry = source[i];
                if entry & PRESENT == 0 {
                    continue;
                }
                let phys = entry & ADDRESS_MASK;
                if level == 3 {
                    let shared = if entry & (WRITABLE | COW) != 0 {
                        (entry & !WRITABLE) | COW
                    } else {
                        entry
                    };
                    source[i] = shared;
                    target[i] = shared;
                    pmm::share_frame(phys);
                    continue;
                }
                let Some(new_table) = pmm::alloc_zeroed_frame() else {
                    return false;
                };
                target[i] = new_table | (entry & !ADDRESS_MASK);
                if !copy_level(phys, new_table, level + 1, 512) {
                    return false;
                }
            }
            true
        }
        copy_level(parent, child, 0, 256)
    };
    if parent == current_root() {
        unsafe { core::arch::asm!("mov {0}, cr3", "mov cr3, {0}", out(reg) _, options(nostack)) };
    }
    if copied {
        Some(child)
    } else {
        destroy_address_space(child);
        None
    }
}

/// Copy-on-write sayfaya yazma: çerçeve hâlâ paylaşılıyorsa kopyalanır, değilse
/// yeniden yazılabilir yapılır. Sayfa CoW değilse `false`.
pub fn resolve_cow(root: u64, virt: u64) -> bool {
    if virt >= USER_SPACE_END {
        return false;
    }
    let _tables = TABLES.lock();
    let idx = indices(virt);
    let mut table_phys = root;
    for &index in &idx[..3] {
        let entry = table(table_phys)[index];
        if entry & PRESENT == 0 || entry & HUGE != 0 {
            return false;
        }
        table_phys = entry & ADDRESS_MASK;
    }
    let entries = table(table_phys);
    let entry = entries[idx[3]];
    if entry & PRESENT == 0 || entry & COW == 0 {
        return false;
    }
    let old = entry & ADDRESS_MASK;
    let flags = (entry & !ADDRESS_MASK & !COW) | WRITABLE;
    if pmm::is_shared(old) {
        let Some(copy) = pmm::alloc_frame() else {
            return false;
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                phys_to_virt(old),
                phys_to_virt(copy),
                PAGE_SIZE as usize,
            )
        };
        entries[idx[3]] = copy | flags;
        pmm::free_frame(old);
    } else {
        entries[idx[3]] = old | flags;
    }
    invalidate(root, virt);
    true
}

pub fn switch_address_space(root: u64) {
    if current_root() != root {
        unsafe { core::arch::asm!("mov cr3, {}", in(reg) root, options(nostack)) };
    }
}

fn self_test() -> bool {
    const TEST_VIRT: u64 = 0xffff_d000_0000_0000;
    let Some(frame) = pmm::alloc_zeroed_frame() else {
        return false;
    };
    if map_page(TEST_VIRT, frame, WRITABLE | NO_EXECUTE).is_err() {
        pmm::free_frame(frame);
        return false;
    }
    let mapped = translate(TEST_VIRT + 0x123) == Some(frame + 0x123);
    let through_mapping = TEST_VIRT as *mut u64;
    let through_hhdm = phys_to_virt(frame) as *const u64;
    let roundtrip = unsafe {
        through_mapping.write_volatile(0x0ba7_7a05);
        through_hhdm.read_volatile() == 0x0ba7_7a05
    };
    let double_map_rejected = matches!(
        map_page(TEST_VIRT, frame, WRITABLE),
        Err(MapError::AlreadyMapped)
    );
    let unmapped = unmap_page(TEST_VIRT) == Some(frame) && translate(TEST_VIRT).is_none();
    pmm::free_frame(frame);

    let spaces_ok = (|| {
        let a = create_address_space()?;
        let b = create_address_space()?;
        let page = pmm::alloc_zeroed_frame()?;
        map_page_in(a, 0x1000_0000, page, USER | WRITABLE).ok()?;
        let isolated = translate_in(a, 0x1000_0000) == Some(page)
            && translate_in(b, 0x1000_0000).is_none()
            && translate_in(b, TEST_VIRT).is_none()
            && translate_in(a, kernel_root() + hhdm_offset()).is_some();
        let before = pmm::stats().free_bytes;
        destroy_address_space(a);
        destroy_address_space(b);
        let reclaimed = pmm::stats().free_bytes > before;
        Some(isolated && reclaimed)
    })()
    .unwrap_or(false);

    mapped && roundtrip && double_map_rejected && unmapped && spaces_ok
}
