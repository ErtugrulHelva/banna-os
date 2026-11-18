//! Limine önyükleme protokolü (base revision 3) için en küçük bağlayıcılar.

use core::cell::UnsafeCell;
use core::ptr::read_volatile;

const COMMON_MAGIC: [u64; 2] = [0xc7b1dd30df4c8b88, 0x0a82e883a194f07b];

const fn id(a: u64, b: u64) -> [u64; 4] {
    [COMMON_MAGIC[0], COMMON_MAGIC[1], a, b]
}

#[repr(C)]
pub struct BaseRevision(UnsafeCell<[u64; 3]>);

unsafe impl Sync for BaseRevision {}

impl BaseRevision {
    pub const fn new(revision: u64) -> Self {
        Self(UnsafeCell::new([
            0xf9562b2d5c95a6c8,
            0x6a7b384944536bdc,
            revision,
        ]))
    }

    pub fn is_supported(&self) -> bool {
        unsafe { read_volatile(self.0.get())[2] == 0 }
    }
}

#[repr(C)]
pub struct Request<R> {
    id: [u64; 4],
    revision: u64,
    response: UnsafeCell<*const R>,
}

unsafe impl<R> Sync for Request<R> {}

impl<R> Request<R> {
    const fn new(id: [u64; 4]) -> Self {
        Self {
            id,
            revision: 0,
            response: UnsafeCell::new(core::ptr::null()),
        }
    }

    pub fn response(&self) -> Option<&'static R> {
        unsafe { read_volatile(self.response.get()).as_ref() }
    }
}

#[repr(C)]
pub struct StackSizeRequest {
    request: Request<StackSizeResponse>,
    stack_size: u64,
}

unsafe impl Sync for StackSizeRequest {}

impl StackSizeRequest {
    pub const fn new(stack_size: u64) -> Self {
        Self {
            request: Request::new(id(0x224ef0460a8e8926, 0xe1cb0fc25f46ea3d)),
            stack_size,
        }
    }

    pub fn response(&self) -> Option<&'static StackSizeResponse> {
        self.request.response()
    }
}

#[repr(C)]
pub struct StackSizeResponse {
    pub revision: u64,
}

#[repr(C)]
pub struct HhdmResponse {
    pub revision: u64,
    pub offset: u64,
}

#[repr(C)]
pub struct Framebuffer {
    pub address: *mut u8,
    pub width: u64,
    pub height: u64,
    pub pitch: u64,
    pub bpp: u16,
    pub memory_model: u8,
    pub red_mask_size: u8,
    pub red_mask_shift: u8,
    pub green_mask_size: u8,
    pub green_mask_shift: u8,
    pub blue_mask_size: u8,
    pub blue_mask_shift: u8,
}

#[repr(C)]
pub struct FramebufferResponse {
    pub revision: u64,
    framebuffer_count: u64,
    framebuffers: *const *const Framebuffer,
}

impl FramebufferResponse {
    pub fn framebuffers(&self) -> impl Iterator<Item = &'static Framebuffer> {
        let base = self.framebuffers;
        (0..self.framebuffer_count as usize).map(move |i| unsafe { &**base.add(i) })
    }
}

#[repr(u64)]
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum MemoryKind {
    Usable = 0,
    Reserved = 1,
    AcpiReclaimable = 2,
    AcpiNvs = 3,
    BadMemory = 4,
    BootloaderReclaimable = 5,
    ExecutableAndModules = 6,
    Framebuffer = 7,
}

#[repr(C)]
pub struct MemmapEntry {
    pub base: u64,
    pub length: u64,
    pub kind: MemoryKind,
}

#[repr(C)]
pub struct MemmapResponse {
    pub revision: u64,
    entry_count: u64,
    entries: *const *const MemmapEntry,
}

impl MemmapResponse {
    pub fn entries(&self) -> impl Iterator<Item = &'static MemmapEntry> + Clone {
        let base = self.entries;
        (0..self.entry_count as usize).map(move |i| unsafe { &**base.add(i) })
    }
}

#[repr(C)]
pub struct ExecutableAddressResponse {
    pub revision: u64,
    pub physical_base: u64,
    pub virtual_base: u64,
}

#[repr(C)]
pub struct RsdpResponse {
    pub revision: u64,
    pub address: u64,
}

#[repr(C)]
pub struct File {
    pub revision: u64,
    pub address: *const u8,
    pub size: u64,
    pub path: *const u8,
    pub cmdline: *const u8,
}

#[repr(C)]
pub struct ExecutableFileResponse {
    pub revision: u64,
    pub file: *const File,
}

pub type ExecutableFileRequest = Request<ExecutableFileResponse>;

#[repr(C)]
pub struct EfiSystemTableResponse {
    pub revision: u64,
    pub address: u64,
}

#[repr(C)]
pub struct EfiMemmapResponse {
    pub revision: u64,
    pub memmap: *const u8,
    pub memmap_size: u64,
    pub desc_size: u64,
    pub desc_version: u64,
}

pub type EfiSystemTableRequest = Request<EfiSystemTableResponse>;
pub type EfiMemmapRequest = Request<EfiMemmapResponse>;

impl EfiSystemTableRequest {
    pub const fn efi_system_table() -> Self {
        Self::new(id(0x5ceba5163eaaf6d6, 0x0a6981610cf65fcc))
    }
}

impl EfiMemmapRequest {
    pub const fn efi_memmap() -> Self {
        Self::new(id(0x7df62a431d6872d5, 0xa4fcdfb3e57306c8))
    }
}

impl ExecutableFileRequest {
    pub const fn executable_file() -> Self {
        Self::new(id(0xad97e90e83f1ed67, 0x31eb5d1c5ff23b69))
    }
}

pub type HhdmRequest = Request<HhdmResponse>;
pub type RsdpRequest = Request<RsdpResponse>;
pub type FramebufferRequest = Request<FramebufferResponse>;
pub type MemmapRequest = Request<MemmapResponse>;
pub type ExecutableAddressRequest = Request<ExecutableAddressResponse>;

impl HhdmRequest {
    pub const fn hhdm() -> Self {
        Self::new(id(0x48dcf1cb8ad2b852, 0x63984e959a98244b))
    }
}

impl RsdpRequest {
    pub const fn rsdp() -> Self {
        Self::new(id(0xc5e77b6b397e7b43, 0x27637845accdcf3c))
    }
}

impl FramebufferRequest {
    pub const fn framebuffer() -> Self {
        Self::new(id(0x9d5827dcd881dd75, 0xa3148604f6fab11b))
    }
}

impl MemmapRequest {
    pub const fn memmap() -> Self {
        Self::new(id(0x67cf3d9d378a806f, 0xe304acdfc50c3c62))
    }
}

impl ExecutableAddressRequest {
    pub const fn executable_address() -> Self {
        Self::new(id(0x71ba76863cc55f63, 0xb2644a48c516a487))
    }
}

#[repr(C)]
pub struct MpRequest {
    request: Request<MpResponse>,
    flags: u64,
}

unsafe impl Sync for MpRequest {}

impl MpRequest {
    pub const fn new() -> Self {
        Self {
            request: Request::new(id(0x95a67b819a1b857e, 0xa0b61b723b6a73e0)),
            flags: 0,
        }
    }

    pub fn response(&self) -> Option<&'static MpResponse> {
        self.request.response()
    }
}

#[repr(C)]
pub struct MpResponse {
    pub revision: u64,
    pub flags: u32,
    pub bsp_lapic_id: u32,
    cpu_count: u64,
    cpus: *const *mut MpInfo,
}

#[repr(C)]
pub struct MpInfo {
    pub processor_id: u32,
    pub lapic_id: u32,
    reserved: u64,
    pub goto_address: core::sync::atomic::AtomicU64,
    pub extra_argument: u64,
}

impl MpResponse {
    pub fn cpus(&self) -> impl Iterator<Item = &'static mut MpInfo> {
        let base = self.cpus;
        (0..self.cpu_count as usize).map(move |i| unsafe { &mut **base.add(i) })
    }
}

#[repr(C)]
pub struct StartMarker([u64; 4]);

impl StartMarker {
    pub const fn new() -> Self {
        Self([
            0xf6b8f4b39de7d1ae,
            0xfab91a6940fcb9cf,
            0x785c6ed015d3e316,
            0x181e920a7852b9d9,
        ])
    }
}

#[repr(C)]
pub struct EndMarker([u64; 2]);

impl EndMarker {
    pub const fn new() -> Self {
        Self([0xadc0e0531bb10d03, 0x9572709f31764c62])
    }
}
