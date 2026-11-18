//! Banna OS çekirdek modülü SDK'sı (`.bko`).
//!
//! Modül konumdan bağımsız bir ELF'tir; çekirdekle yalnızca `KernelApi` fonksiyon tablosu
//! üzerinden konuşur, bu yüzden çekirdekle aynı derlemeden olması gerekmez (yalnızca
//! `API_VERSION` uyuşmalı). Modül `banna_kmod::module!(ad, init)` ile tanımlanır.
//! Bu dosyadaki `KernelApi`/`NicOps` düzeni `src/kernel/module.rs` ile aynı olmalıdır.

#![no_std]

extern crate alloc;

use core::alloc::{GlobalAlloc, Layout};
use core::ffi::c_void;
use core::sync::atomic::{AtomicPtr, Ordering};

pub const API_VERSION: u32 = 1;

#[repr(C)]
pub struct KernelApi {
    pub version: u32,
    pub log: extern "C" fn(*const u8, usize),
    pub alloc: extern "C" fn(usize, usize) -> *mut u8,
    pub free: extern "C" fn(*mut u8, usize, usize),
    pub map_mmio: extern "C" fn(u64, u64) -> *mut u8,
    /// 4 GiB altında bitişik, sıfırlanmış sayfalar; fiziksel adres `phys`'e yazılır.
    pub dma_alloc: extern "C" fn(u64, *mut u64) -> *mut u8,
    /// `index`. eşleşen PCI aygıtı: bus << 8 | slot << 3 | function, yoksa -1.
    pub pci_find: extern "C" fn(u16, u16, u32) -> i64,
    pub pci_read: extern "C" fn(u32, u8) -> u32,
    pub pci_write: extern "C" fn(u32, u8, u32),
    pub pci_enable: extern "C" fn(u32),
    pub pci_bar: extern "C" fn(u32, u8) -> u64,
    /// MSI → ACPI _PRT → ISA hattı; işleyici kesme bağlamında çalışır.
    pub pci_enable_interrupt: extern "C" fn(u32, extern "C" fn()) -> bool,
    pub now_ns: extern "C" fn() -> u64,
    pub sleep_ms: extern "C" fn(u64),
    pub net_wake: extern "C" fn(),
    pub register_nic: extern "C" fn(*const NicOps, *mut c_void) -> bool,
}

/// Ağ kartı işlemleri (çekirdeğin `Nic` arayüzünün C karşılığı).
#[repr(C)]
pub struct NicOps {
    pub model: *const u8,
    pub model_len: usize,
    pub mac: extern "C" fn(*mut c_void, *mut [u8; 6]),
    pub send: extern "C" fn(*mut c_void, *const u8, usize) -> bool,
    /// Bir çerçeveyi `buf`'a kopyalar; uzunluk ya da yoksa -1.
    pub receive: extern "C" fn(*mut c_void, *mut u8, usize) -> isize,
    /// bit 0: bağlı, bit 1: tam çift yönlü, bit 16..: hız (Mbit/s).
    pub link: extern "C" fn(*mut c_void) -> u32,
    /// bit 0: bağlantı değişti, bit 1: alım oldu.
    pub take_cause: extern "C" fn(*mut c_void) -> u32,
}

unsafe impl Sync for NicOps {}

static API: AtomicPtr<KernelApi> = AtomicPtr::new(core::ptr::null_mut());

pub fn api() -> &'static KernelApi {
    unsafe { &*API.load(Ordering::Acquire) }
}

#[doc(hidden)]
pub fn set_api(api: *const KernelApi) -> bool {
    if api.is_null() || unsafe { (*api).version } != API_VERSION {
        return false;
    }
    API.store(api as *mut KernelApi, Ordering::Release);
    true
}

pub fn log(text: &str) {
    (api().log)(text.as_ptr(), text.len());
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => {
        $crate::log(&alloc::format!($($arg)*))
    };
}

pub struct Dma {
    pub virt: *mut u8,
    pub phys: u64,
}

pub fn dma_alloc(pages: u64) -> Option<Dma> {
    let mut phys = 0;
    let virt = (api().dma_alloc)(pages, &mut phys);
    (!virt.is_null()).then_some(Dma { virt, phys })
}

pub struct Pci(pub u32);

impl Pci {
    pub fn find(vendor: u16, device: u16) -> Option<Self> {
        let bdf = (api().pci_find)(vendor, device, 0);
        (bdf >= 0).then_some(Self(bdf as u32))
    }
    pub fn read(&self, offset: u8) -> u32 {
        (api().pci_read)(self.0, offset)
    }
    pub fn write(&self, offset: u8, value: u32) {
        (api().pci_write)(self.0, offset, value)
    }
    pub fn enable(&self) {
        (api().pci_enable)(self.0)
    }
    pub fn bar(&self, index: u8) -> u64 {
        (api().pci_bar)(self.0, index)
    }
    pub fn enable_interrupt(&self, handler: extern "C" fn()) -> bool {
        (api().pci_enable_interrupt)(self.0, handler)
    }
}

pub fn now_ns() -> u64 {
    (api().now_ns)()
}

pub fn sleep_ms(ms: u64) {
    (api().sleep_ms)(ms)
}

pub struct KernelAllocator;

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        (api().alloc)(layout.size(), layout.align())
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        (api().free)(ptr, layout.size(), layout.align())
    }
}

/// Modülün giriş noktası, ayırıcısı ve panik işleyicisi.
/// `init` başarıda 0, hatada başka bir değer döndürür (modül bellekte kalmaz).
#[macro_export]
macro_rules! module {
    ($name:literal, $init:path) => {
        #[global_allocator]
        static ALLOCATOR: $crate::KernelAllocator = $crate::KernelAllocator;

        #[unsafe(no_mangle)]
        pub extern "C" fn module_entry(api: *const $crate::KernelApi) -> i32 {
            if !$crate::set_api(api) {
                return -1;
            }
            $init()
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            $crate::log(&alloc::format!("module {} panicked: {}", $name, info));
            loop {
                core::hint::spin_loop();
            }
        }
    };
}
