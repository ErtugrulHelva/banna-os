//! Yüklenebilir çekirdek modülleri (`/lib/modules/*.bko`, SDK: `modules/sdk`).
//!
//! Modül konumdan bağımsız bir ELF'tir (ET_DYN, yalnızca R_X86_64_RELATIVE). Ayrılmış
//! sanal bölgeye yüklenir, `module_entry(&KernelApi)` çağrılır. Çekirdekle yalnızca
//! `KernelApi` tablosu üzerinden konuştuğu için sürücüler çekirdek değişmeden
//! güncellenebilir. `KernelApi`/`NicOps` düzeni `modules/sdk/src/lib.rs` ile aynıdır.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::alloc::Layout;
use core::ffi::c_void;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use crate::drivers::nic::Nic;
use crate::drivers::pci;
use crate::kernel::initcall::Detail;
use crate::memory::paging::{self, NO_CACHE, NO_EXECUTE, WRITABLE};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sync::SpinLock;

pub const DIR: &str = "/lib/modules";
const API_VERSION: u32 = 1;
/// Modüllerin yüklendiği sanal bölge (çekirdek yarısında, kendi PML4 girdisi).
const REGION: u64 = 0xffff_e000_0000_0000;
const REGION_SIZE: u64 = 1 << 30;
const PT_LOAD: u32 = 1;
const PT_DYNAMIC: u32 = 2;
const PF_X: u32 = 1;
const PF_W: u32 = 2;
const DT_RELA: i64 = 7;
const DT_RELASZ: i64 = 8;
const R_X86_64_RELATIVE: u32 = 8;

static NEXT: AtomicU64 = AtomicU64::new(REGION);
/// Yüklenmekte olan modülün adı ve modüllerin sürdüğü PCI aygıtları.
static CURRENT: SpinLock<Option<&'static str>> = SpinLock::new(None);
static CLAIMS: SpinLock<Vec<(u32, &'static str)>> = SpinLock::new(Vec::new());

/// Aygıt bir modül tarafından sürülüyorsa modülün adı.
pub fn claimed(d: &pci::Device) -> Option<&'static str> {
    let b = bdf(d);
    CLAIMS.lock().iter().find(|(x, _)| *x == b).map(|(_, n)| *n)
}
static READY: AtomicBool = AtomicBool::new(false);

pub struct Loaded {
    pub name: String,
    pub base: u64,
    pub size: u64,
}

static LOADED: SpinLock<Vec<Loaded>> = SpinLock::new(Vec::new());

/// (ad, yükleme adresi, boyut)
pub fn loaded() -> Vec<(String, u64, u64)> {
    LOADED
        .lock()
        .iter()
        .map(|m| (m.name.clone(), m.base, m.size))
        .collect()
}

#[repr(C)]
pub struct KernelApi {
    version: u32,
    log: extern "C" fn(*const u8, usize),
    alloc: extern "C" fn(usize, usize) -> *mut u8,
    free: extern "C" fn(*mut u8, usize, usize),
    map_mmio: extern "C" fn(u64, u64) -> *mut u8,
    dma_alloc: extern "C" fn(u64, *mut u64) -> *mut u8,
    pci_find: extern "C" fn(u16, u16, u32) -> i64,
    pci_read: extern "C" fn(u32, u8) -> u32,
    pci_write: extern "C" fn(u32, u8, u32),
    pci_enable: extern "C" fn(u32),
    pci_bar: extern "C" fn(u32, u8) -> u64,
    pci_enable_interrupt: extern "C" fn(u32, extern "C" fn()) -> bool,
    now_ns: extern "C" fn() -> u64,
    sleep_ms: extern "C" fn(u64),
    net_wake: extern "C" fn(),
    register_nic: extern "C" fn(*const NicOps, *mut c_void) -> bool,
}

#[repr(C)]
pub struct NicOps {
    model: *const u8,
    model_len: usize,
    mac: extern "C" fn(*mut c_void, *mut [u8; 6]),
    send: extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    receive: extern "C" fn(*mut c_void, *mut u8, usize) -> isize,
    link: extern "C" fn(*mut c_void) -> u32,
    take_cause: extern "C" fn(*mut c_void) -> u32,
}

static API: KernelApi = KernelApi {
    version: API_VERSION,
    log: api_log,
    alloc: api_alloc,
    free: api_free,
    map_mmio: api_map_mmio,
    dma_alloc: api_dma_alloc,
    pci_find: api_pci_find,
    pci_read: api_pci_read,
    pci_write: api_pci_write,
    pci_enable: api_pci_enable,
    pci_bar: api_pci_bar,
    pci_enable_interrupt: api_pci_enable_interrupt,
    now_ns: api_now_ns,
    sleep_ms: api_sleep_ms,
    net_wake: api_net_wake,
    register_nic: api_register_nic,
};

extern "C" fn api_log(text: *const u8, len: usize) {
    let bytes = unsafe { core::slice::from_raw_parts(text, len) };
    crate::log!("{}", String::from_utf8_lossy(bytes));
}

extern "C" fn api_alloc(size: usize, align: usize) -> *mut u8 {
    match Layout::from_size_align(size.max(1), align) {
        Ok(layout) => unsafe { alloc::alloc::alloc(layout) },
        Err(_) => core::ptr::null_mut(),
    }
}

extern "C" fn api_free(ptr: *mut u8, size: usize, align: usize) {
    if let Ok(layout) = Layout::from_size_align(size.max(1), align) {
        unsafe { alloc::alloc::dealloc(ptr, layout) }
    }
}

extern "C" fn api_map_mmio(phys: u64, len: u64) -> *mut u8 {
    paging::map_physical(phys, len, NO_CACHE).unwrap_or(core::ptr::null_mut())
}

extern "C" fn api_dma_alloc(pages: u64, phys: *mut u64) -> *mut u8 {
    let Some(frame) = pmm::alloc_contiguous(pages.max(1)) else {
        return core::ptr::null_mut();
    };
    let virt = phys_to_virt(frame);
    unsafe {
        core::ptr::write_bytes(virt, 0, (pages.max(1) * PAGE_SIZE) as usize);
        *phys = frame;
    }
    virt
}

fn bdf(d: &pci::Device) -> u32 {
    (d.bus as u32) << 8 | (d.slot as u32) << 3 | d.function as u32
}

fn device(bdf_value: u32) -> Option<pci::Device> {
    pci::devices().into_iter().find(|d| bdf(d) == bdf_value)
}

extern "C" fn api_pci_find(vendor: u16, id: u16, index: u32) -> i64 {
    pci::devices()
        .iter()
        .filter(|d| d.vendor_id == vendor && d.device_id == id)
        .nth(index as usize)
        .map_or(-1, |d| bdf(d) as i64)
}

extern "C" fn api_pci_read(b: u32, offset: u8) -> u32 {
    device(b).map_or(!0, |d| d.read(offset))
}

extern "C" fn api_pci_write(b: u32, offset: u8, value: u32) {
    if let Some(d) = device(b) {
        d.write(offset, value);
    }
}

extern "C" fn api_pci_enable(b: u32) {
    if let Some(name) = *CURRENT.lock() {
        let mut claims = CLAIMS.lock();
        if !claims.iter().any(|(x, _)| *x == b) {
            claims.push((b, name));
        }
    }
    if let Some(d) = device(b) {
        d.enable();
    }
}

extern "C" fn api_pci_bar(b: u32, index: u8) -> u64 {
    device(b).and_then(|d| d.bar(index)).unwrap_or(0)
}

/// Modül kesme işleyicileri (`extern "C"`) için çekirdek `fn()` köprüleri.
const IRQ_SLOTS: usize = 8;
static IRQ_TARGETS: [AtomicUsize; IRQ_SLOTS] = [const { AtomicUsize::new(0) }; IRQ_SLOTS];

macro_rules! trampolines {
    ($($name:ident = $i:literal),*) => {
        $(fn $name() {
            let target = IRQ_TARGETS[$i].load(Ordering::Acquire);
            if target != 0 {
                let f: extern "C" fn() = unsafe { core::mem::transmute(target) };
                f();
            }
        })*
        const TRAMPOLINES: [fn(); IRQ_SLOTS] = [$($name),*];
    };
}

trampolines!(
    irq0 = 0,
    irq1 = 1,
    irq2 = 2,
    irq3 = 3,
    irq4 = 4,
    irq5 = 5,
    irq6 = 6,
    irq7 = 7
);

extern "C" fn api_pci_enable_interrupt(b: u32, handler: extern "C" fn()) -> bool {
    let Some(d) = device(b) else { return false };
    let Some(slot) = IRQ_TARGETS.iter().position(|t| {
        t.compare_exchange(0, handler as usize, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }) else {
        return false;
    };
    d.enable_interrupt(TRAMPOLINES[slot])
}

extern "C" fn api_now_ns() -> u64 {
    crate::arch::tsc::now_ns()
}

extern "C" fn api_sleep_ms(ms: u64) {
    crate::sched::sleep_ms(ms);
}

extern "C" fn api_net_wake() {
    crate::net::wake();
}

struct ModuleNic {
    ops: &'static NicOps,
    ctx: *mut c_void,
    model: &'static str,
}

unsafe impl Send for ModuleNic {}

impl Nic for ModuleNic {
    fn mac(&self) -> [u8; 6] {
        let mut mac = [0; 6];
        (self.ops.mac)(self.ctx, &mut mac);
        mac
    }
    fn model(&self) -> &'static str {
        self.model
    }
    fn send(&mut self, frame: &[u8]) -> bool {
        (self.ops.send)(self.ctx, frame.as_ptr(), frame.len())
    }
    fn receive(&mut self) -> Option<Vec<u8>> {
        let mut buf = alloc::vec![0u8; 2048];
        let n = (self.ops.receive)(self.ctx, buf.as_mut_ptr(), buf.len());
        (n >= 0).then(|| {
            buf.truncate(n as usize);
            buf
        })
    }
    fn link(&self) -> (bool, u32, bool) {
        let s = (self.ops.link)(self.ctx);
        (s & 1 != 0, s >> 16, s & 2 != 0)
    }
    fn take_cause(&mut self) -> (bool, bool) {
        let c = (self.ops.take_cause)(self.ctx);
        (c & 1 != 0, c & 2 != 0)
    }
}

extern "C" fn api_register_nic(ops: *const NicOps, ctx: *mut c_void) -> bool {
    if ops.is_null() {
        return false;
    }
    let ops: &'static NicOps = unsafe { &*ops };
    let model = unsafe { core::slice::from_raw_parts(ops.model, ops.model_len) };
    let model = core::str::from_utf8(model).unwrap_or("module NIC");
    crate::drivers::nic::register(Box::new(ModuleNic { ops, ctx, model }))
}

/// Modülü yükler ve başlatır.
pub fn load(path: &str) -> Result<(), String> {
    if !READY.load(Ordering::Acquire) {
        return Err(String::from("module loader not ready"));
    }
    let name = path
        .rsplit('/')
        .next()
        .unwrap_or(path)
        .trim_end_matches(".bko");
    if LOADED.lock().iter().any(|m| m.name == name) {
        return Err(alloc::format!("{} is already loaded", name));
    }
    let data = crate::fs::read_all(path).map_err(|e| alloc::format!("{}: {:?}", path, e))?;
    let image = Image::parse(&data).ok_or_else(|| String::from("not a Banna kernel module"))?;
    let size = image.span.next_multiple_of(PAGE_SIZE);
    let base = NEXT.fetch_add(size + PAGE_SIZE, Ordering::AcqRel);
    if base + size > REGION + REGION_SIZE {
        return Err(String::from("module region full"));
    }
    let pages = size / PAGE_SIZE;
    let mut frames = Vec::new();
    for i in 0..pages {
        let frame = pmm::alloc_frame().ok_or("out of memory")?;
        unsafe { core::ptr::write_bytes(phys_to_virt(frame), 0, PAGE_SIZE as usize) };
        paging::map_page(base + i * PAGE_SIZE, frame, WRITABLE)
            .map_err(|_| String::from("cannot map module"))?;
        frames.push(frame);
    }
    for seg in &image.segments {
        let file = data
            .get(seg.offset as usize..(seg.offset + seg.filesz) as usize)
            .ok_or("truncated segment")?;
        unsafe {
            core::ptr::copy_nonoverlapping(file.as_ptr(), (base + seg.vaddr) as *mut u8, file.len())
        };
    }
    for &(offset, addend) in &image.relocations {
        if offset + 8 > image.span {
            return Err(String::from("relocation out of range"));
        }
        unsafe { ((base + offset) as *mut u64).write_unaligned(base.wrapping_add(addend as u64)) };
    }
    // Son izinler: yürütülebilir olmayan sayfalar NX, yazılamayanlar salt okunur.
    for (i, &frame) in frames.iter().enumerate() {
        let start = i as u64 * PAGE_SIZE;
        let (mut write, mut exec) = (false, false);
        for seg in &image.segments {
            if seg.vaddr < start + PAGE_SIZE && seg.vaddr + seg.memsz > start {
                write |= seg.flags & PF_W != 0;
                exec |= seg.flags & PF_X != 0;
            }
        }
        let flags = if write { WRITABLE } else { 0 } | if exec { 0 } else { NO_EXECUTE };
        let virt = base + start;
        paging::unmap_page(virt);
        let _ = paging::map_page(virt, frame, flags);
    }
    let entry: extern "C" fn(*const KernelApi) -> i32 =
        unsafe { core::mem::transmute(base + image.entry) };
    let leaked: &'static str = alloc::boxed::Box::leak(String::from(name).into_boxed_str());
    *CURRENT.lock() = Some(leaked);
    let code = entry(&API);
    *CURRENT.lock() = None;
    if code != 0 {
        CLAIMS.lock().retain(|(_, n)| *n != leaked);
    }
    if code == 1 {
        return Err(alloc::format!("{}: no matching hardware", name));
    }
    if code != 0 {
        return Err(alloc::format!("{}: init returned {}", name, code));
    }
    crate::log!(
        "module: {} loaded at {:#x} ({} KiB)",
        name,
        base,
        size / 1024
    );
    LOADED.lock().push(Loaded {
        name: String::from(name),
        base,
        size,
    });
    Ok(())
}

struct Segment {
    offset: u64,
    vaddr: u64,
    filesz: u64,
    memsz: u64,
    flags: u32,
}

struct Image {
    entry: u64,
    span: u64,
    segments: Vec<Segment>,
    relocations: Vec<(u64, i64)>,
}

impl Image {
    fn parse(d: &[u8]) -> Option<Self> {
        let u16_at = |o: usize| Some(u16::from_le_bytes(d.get(o..o + 2)?.try_into().ok()?));
        let u32_at = |o: usize| Some(u32::from_le_bytes(d.get(o..o + 4)?.try_into().ok()?));
        let u64_at = |o: usize| Some(u64::from_le_bytes(d.get(o..o + 8)?.try_into().ok()?));
        if d.get(..4)? != b"\x7fELF" || d[4] != 2 || u16_at(16)? != 3 || u16_at(18)? != 62 {
            return None;
        }
        let entry = u64_at(24)?;
        let phoff = u64_at(32)? as usize;
        let phentsize = u16_at(54)? as usize;
        let phnum = u16_at(56)? as usize;
        let mut segments = Vec::new();
        let mut dynamic = None;
        for i in 0..phnum {
            let p = phoff + i * phentsize;
            let kind = u32_at(p)?;
            let seg = Segment {
                flags: u32_at(p + 4)?,
                offset: u64_at(p + 8)?,
                vaddr: u64_at(p + 16)?,
                filesz: u64_at(p + 32)?,
                memsz: u64_at(p + 40)?,
            };
            match kind {
                PT_LOAD => segments.push(seg),
                PT_DYNAMIC => dynamic = Some((seg.offset, seg.filesz)),
                _ => {}
            }
        }
        let span = segments.iter().map(|s| s.vaddr + s.memsz).max()?;
        if span > 64 << 20 {
            return None;
        }
        // Dosya ofseti ↔ sanal adres (DT_RELA sanal adres verir).
        let to_file = |vaddr: u64| {
            segments
                .iter()
                .find(|s| vaddr >= s.vaddr && vaddr < s.vaddr + s.filesz)
                .map(|s| (vaddr - s.vaddr + s.offset) as usize)
        };
        let mut relocations = Vec::new();
        if let Some((offset, size)) = dynamic {
            let (mut rela, mut relasz) = (0, 0);
            for j in (offset..offset + size).step_by(16) {
                let tag = u64_at(j as usize)? as i64;
                let value = u64_at(j as usize + 8)?;
                match tag {
                    0 => break,
                    DT_RELA => rela = value,
                    DT_RELASZ => relasz = value,
                    _ => {}
                }
            }
            if relasz > 0 {
                let start = to_file(rela)?;
                for r in (start..start + relasz as usize).step_by(24) {
                    let kind = u64_at(r + 8)? as u32;
                    if kind != R_X86_64_RELATIVE {
                        return None;
                    }
                    relocations.push((u64_at(r)?, u64_at(r + 16)? as i64));
                }
            }
        }
        Some(Self {
            entry,
            span,
            segments,
            relocations,
        })
    }
}

/// `/lib/modules` içindeki modülleri sırayla yükler (`nomodules` ile kapatılır).
fn init(detail: &mut Detail) -> Result<(), &'static str> {
    // Bölgenin PML4 girdisi açılışta oluşur; sonradan açılan süreçler de paylaşır.
    let frame = pmm::alloc_frame().ok_or("out of memory")?;
    paging::map_page(REGION + REGION_SIZE, frame, WRITABLE | NO_EXECUTE)
        .map_err(|_| "cannot reserve the module region")?;
    READY.store(true, Ordering::Release);
    if crate::boot::has_flag("nomodules") {
        let _ = write!(detail, "disabled (nomodules)");
        return Ok(());
    }
    let mut names: Vec<String> = crate::fs::read_dir(DIR)
        .map(|list| {
            list.into_iter()
                .map(|m| m.name)
                .filter(|n| n.ends_with(".bko"))
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    let mut ok = 0;
    for name in &names {
        match load(&alloc::format!("{}/{}", DIR, name)) {
            Ok(()) => ok += 1,
            Err(e) => crate::log!("module: {}", e),
        }
    }
    let _ = write!(detail, "{} of {} module(s) loaded", ok, names.len());
    Ok(())
}

crate::initcall!(driver, 34, "Kernel modules", init);
