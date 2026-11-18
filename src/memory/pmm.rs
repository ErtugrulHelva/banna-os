//! Bitmap tabanlı fiziksel sayfa çerçevesi ayırıcı.

use alloc::collections::BTreeMap;
use core::fmt::Write;
use core::ptr::write_bytes;
use core::slice;

use super::{PAGE_SIZE, align_down, align_up, phys_to_virt};
use crate::boot::{self, limine::MemmapResponse, limine::MemoryKind};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::sync::SpinLock;

const LOW_MEMORY_END: u64 = 0x10_0000;
/// 32 bit DMA yapabilen aygıtların (UHCI/OHCI, S64A'sız AHCI...) erişebildiği sınır.
/// Genel ayırmalar önce bunun üstünü kullanır; altı DMA için saklanır.
const DMA_LIMIT: u64 = 0x1_0000_0000;

struct FrameAllocator {
    bitmap: &'static mut [u64],
    frame_count: u64,
    free_frames: u64,
    usable_frames: u64,
    next_hint: u64,
    /// 4 GiB üstünde boş çerçeve kalmadı (her ayırmada boşuna taranmasın).
    high_full: bool,
    /// Birden çok sahibi olan çerçeveler (copy-on-write): çerçeve → ek sahip sayısı.
    shared: BTreeMap<u64, u32>,
}

impl FrameAllocator {
    fn is_used(&self, frame: u64) -> bool {
        self.bitmap[(frame / 64) as usize] & (1 << (frame % 64)) != 0
    }

    fn set_used(&mut self, frame: u64) {
        if !self.is_used(frame) {
            self.bitmap[(frame / 64) as usize] |= 1 << (frame % 64);
            self.free_frames -= 1;
        }
    }

    fn set_free(&mut self, frame: u64) {
        if self.is_used(frame) {
            self.bitmap[(frame / 64) as usize] &= !(1 << (frame % 64));
            self.free_frames += 1;
            if frame >= DMA_LIMIT / PAGE_SIZE {
                self.high_full = false;
            }
        }
    }

    fn alloc(&mut self) -> Option<u64> {
        let words = self.bitmap.len() as u64;
        let dma_words = (DMA_LIMIT / PAGE_SIZE / 64).min(words);
        if words > dma_words && !self.high_full {
            if let Some(frame) = self.alloc_in(dma_words, words) {
                return Some(frame);
            }
            self.high_full = true;
        }
        self.alloc_in(0, words)
    }

    /// `[first, last)` sözcük aralığında, ipucundan başlayarak dönerek arar.
    fn alloc_in(&mut self, first: u64, last: u64) -> Option<u64> {
        let words = last - first;
        let hint = (self.next_hint / 64).clamp(first, last - 1) - first;
        for i in 0..words {
            let word_index = (first + (hint + i) % words) as usize;
            let word = self.bitmap[word_index];
            if word == u64::MAX {
                continue;
            }
            let frame = word_index as u64 * 64 + (!word).trailing_zeros() as u64;
            if frame >= self.frame_count {
                continue;
            }
            self.set_used(frame);
            self.next_hint = frame + 1;
            return Some(frame * PAGE_SIZE);
        }
        None
    }

    fn alloc_contiguous(&mut self, count: u64) -> Option<u64> {
        let mut run_start = 0;
        let mut run_length = 0;
        let end = self.frame_count.min(DMA_LIMIT / PAGE_SIZE);
        for frame in LOW_MEMORY_END / PAGE_SIZE..end {
            if self.is_used(frame) {
                run_length = 0;
                continue;
            }
            if run_length == 0 {
                run_start = frame;
            }
            run_length += 1;
            if run_length == count {
                for f in run_start..run_start + count {
                    self.set_used(f);
                }
                return Some(run_start * PAGE_SIZE);
            }
        }
        None
    }
}

static ALLOCATOR: SpinLock<Option<FrameAllocator>> = SpinLock::new(None);

pub struct Stats {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

fn init_call(detail: &mut Detail) -> Result<(), &'static str> {
    let memmap = boot::MEMMAP
        .response()
        .ok_or("bootloader gave no memory map")?;
    for entry in memmap.entries() {
        crate::log!(
            "memmap: {:#014x}-{:#014x} {:?}",
            entry.base,
            entry.base + entry.length,
            entry.kind
        );
    }
    let stats = init(memmap)?;
    let _ = write!(detail, "{} MiB usable", stats.total_bytes / (1024 * 1024));
    check("pmm", self_test())
}

crate::initcall!(core, 10, "Physical memory", init_call);

fn init(memmap: &MemmapResponse) -> Result<Stats, &'static str> {
    let usable = memmap
        .entries()
        .filter(|e| e.kind == MemoryKind::Usable)
        .map(|e| {
            let start = align_up(e.base.max(LOW_MEMORY_END), PAGE_SIZE);
            let end = align_down(e.base + e.length, PAGE_SIZE);
            (start, end.max(start))
        })
        .filter(|(start, end)| end > start);

    let highest = usable
        .clone()
        .map(|(_, end)| end)
        .max()
        .ok_or("no usable memory")?;
    let frame_count = align_up(highest, PAGE_SIZE) / PAGE_SIZE;
    let bitmap_bytes = align_up(frame_count.div_ceil(64) * 8, PAGE_SIZE);

    let (bitmap_phys, _) = usable
        .clone()
        .find(|(start, end)| end - start >= bitmap_bytes)
        .ok_or("no region large enough for the frame bitmap")?;

    let bitmap = unsafe {
        let ptr = phys_to_virt(bitmap_phys);
        write_bytes(ptr, 0xff, bitmap_bytes as usize);
        slice::from_raw_parts_mut(ptr as *mut u64, (bitmap_bytes / 8) as usize)
    };

    let mut allocator = FrameAllocator {
        bitmap,
        frame_count,
        free_frames: 0,
        usable_frames: 0,
        next_hint: 0,
        high_full: false,
        shared: BTreeMap::new(),
    };

    for (start, end) in usable {
        for frame in start / PAGE_SIZE..end / PAGE_SIZE {
            allocator.set_free(frame);
        }
    }

    for frame in bitmap_phys / PAGE_SIZE..(bitmap_phys + bitmap_bytes) / PAGE_SIZE {
        allocator.set_used(frame);
    }
    allocator.usable_frames = allocator.free_frames;

    let stats = Stats {
        total_bytes: allocator.usable_frames * PAGE_SIZE,
        free_bytes: allocator.free_frames * PAGE_SIZE,
    };
    crate::log!(
        "pmm: {} frames tracked, bitmap {} KiB at {:#x}",
        frame_count,
        bitmap_bytes / 1024,
        bitmap_phys
    );
    *ALLOCATOR.lock() = Some(allocator);
    Ok(stats)
}

pub fn alloc_frame() -> Option<u64> {
    ALLOCATOR.lock().as_mut()?.alloc()
}

pub fn alloc_zeroed_frame() -> Option<u64> {
    let frame = alloc_frame()?;
    unsafe { write_bytes(phys_to_virt(frame), 0, PAGE_SIZE as usize) };
    Some(frame)
}

/// Sıfırlanmış, 4 GiB altında bir çerçeve (aygıtın DMA ile eriştiği yapılar için).
pub fn alloc_dma_frame() -> Option<u64> {
    let frame = alloc_contiguous(1)?;
    unsafe { write_bytes(phys_to_virt(frame), 0, PAGE_SIZE as usize) };
    Some(frame)
}

/// Ardışık çerçeveler, 4 GiB altından (DMA tamponları).
pub fn alloc_contiguous(count: u64) -> Option<u64> {
    ALLOCATOR.lock().as_mut()?.alloc_contiguous(count)
}

/// Çerçevenin bir sahibini bırakır; son sahipse çerçeve serbest kalır.
pub fn free_frame(phys: u64) {
    if let Some(allocator) = ALLOCATOR.lock().as_mut() {
        let frame = phys / PAGE_SIZE;
        match allocator.shared.get_mut(&frame) {
            Some(extra) if *extra > 1 => *extra -= 1,
            Some(_) => {
                allocator.shared.remove(&frame);
            }
            None => allocator.set_free(frame),
        }
    }
}

/// Çerçeveye bir sahip daha ekler (her sahip sonunda `free_frame` çağırır).
pub fn share_frame(phys: u64) {
    if let Some(allocator) = ALLOCATOR.lock().as_mut() {
        *allocator.shared.entry(phys / PAGE_SIZE).or_insert(0) += 1;
    }
}

pub fn is_shared(phys: u64) -> bool {
    ALLOCATOR
        .lock()
        .as_ref()
        .is_some_and(|a| a.shared.contains_key(&(phys / PAGE_SIZE)))
}

pub fn stats() -> Stats {
    let guard = ALLOCATOR.lock();
    match guard.as_ref() {
        Some(a) => Stats {
            total_bytes: a.usable_frames * PAGE_SIZE,
            free_bytes: a.free_frames * PAGE_SIZE,
        },
        None => Stats {
            total_bytes: 0,
            free_bytes: 0,
        },
    }
}

fn self_test() -> bool {
    let before = stats().free_bytes;
    let (Some(a), Some(b)) = (alloc_zeroed_frame(), alloc_frame()) else {
        return false;
    };
    let distinct = a != b && a % PAGE_SIZE == 0 && b % PAGE_SIZE == 0;
    let zeroed = unsafe { slice::from_raw_parts(phys_to_virt(a), PAGE_SIZE as usize) }
        .iter()
        .all(|&byte| byte == 0);
    let run = alloc_contiguous(4);
    free_frame(a);
    free_frame(b);
    if let Some(run) = run {
        for i in 0..4 {
            free_frame(run + i * PAGE_SIZE);
        }
    }
    distinct && zeroed && run.is_some() && stats().free_bytes == before
}
