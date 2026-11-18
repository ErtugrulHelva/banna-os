//! Çekirdek heap'i: 4 KiB'a kadarki ayırmalar boyut sınıflı serbest listelerden (O(1)),
//! büyükleri talep üzerine büyüyen, birleştirmeli first-fit serbest listeden gelir.

use core::alloc::{GlobalAlloc, Layout};
use core::fmt::Write;
use core::mem::{align_of, size_of};
use core::ptr::null_mut;

use super::paging::{self, NO_EXECUTE, WRITABLE};
use super::{PAGE_SIZE, align_up, pmm};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::sync::SpinLock;

pub const HEAP_START: u64 = 0xffff_c000_0000_0000;
const HEAP_INITIAL: u64 = 1024 * 1024;
/// Yalnızca sanal alan tavanı; gerçek sınır fiziksel bellektir.
const HEAP_MAX: u64 = 1 << 40;
const MIN_GROWTH: u64 = 64 * 1024;
/// Boyut sınıfları: 16, 32, ..., 4096 bayt (her nesne kendi boyutuna hizalı).
const CLASSES: usize = 9;
const MIN_CLASS: usize = 16;
const MAX_CLASS: usize = MIN_CLASS << (CLASSES - 1);
/// Bir sınıf boşalınca büyük heap'ten alınan dilim.
const SLAB: usize = 32 * 1024;

struct ClassNode {
    next: *mut ClassNode,
}

struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

const BLOCK_ALIGN: usize = align_of::<FreeBlock>();
const MIN_BLOCK: usize = size_of::<FreeBlock>();

struct Heap {
    head: *mut FreeBlock,
    end: u64,
}

unsafe impl Send for Heap {}

impl Heap {
    const fn empty() -> Self {
        Self {
            head: null_mut(),
            end: HEAP_START,
        }
    }

    fn grow(&mut self, at_least: u64) -> bool {
        let bytes = align_up(at_least.max(MIN_GROWTH), PAGE_SIZE);
        if self.end + bytes > HEAP_START + HEAP_MAX {
            return false;
        }
        let start = self.end;
        let mut mapped = 0;
        while mapped < bytes {
            let Some(frame) = pmm::alloc_frame() else {
                break;
            };
            if paging::map_page(start + mapped, frame, WRITABLE | NO_EXECUTE).is_err() {
                pmm::free_frame(frame);
                break;
            }
            mapped += PAGE_SIZE;
        }
        if mapped == 0 {
            return false;
        }
        self.end += mapped;
        unsafe { self.insert(start as usize, mapped as usize) };
        mapped >= bytes
    }

    unsafe fn insert(&mut self, addr: usize, size: usize) {
        let mut prev: *mut FreeBlock = null_mut();
        let mut current = self.head;
        while !current.is_null() && (current as usize) < addr {
            prev = current;
            current = unsafe { (*current).next };
        }

        let block = addr as *mut FreeBlock;
        unsafe {
            block.write(FreeBlock {
                size,
                next: current,
            });
            if !current.is_null() && addr + size == current as usize {
                (*block).size += (*current).size;
                (*block).next = (*current).next;
            }
            if prev.is_null() {
                self.head = block;
            } else if prev as usize + (*prev).size == addr {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            } else {
                (*prev).next = block;
            }
        }
    }

    fn take(&mut self, size: usize, align: usize) -> *mut u8 {
        let mut prev: *mut FreeBlock = null_mut();
        let mut current = self.head;
        while !current.is_null() {
            let block_start = current as usize;
            let (block_size, next) = unsafe { ((*current).size, (*current).next) };
            let block_end = block_start + block_size;

            let mut alloc_start = align_up(block_start as u64, align as u64) as usize;
            let mut front_gap = alloc_start - block_start;
            if front_gap != 0 && front_gap < MIN_BLOCK {
                alloc_start = align_up((block_start + MIN_BLOCK) as u64, align as u64) as usize;
                front_gap = alloc_start - block_start;
            }
            let alloc_end = alloc_start.saturating_add(size);

            if alloc_end <= block_end {
                let mut back_gap = block_end - alloc_end;
                let mut used_end = alloc_end;
                if back_gap != 0 && back_gap < MIN_BLOCK {
                    used_end = block_end;
                    back_gap = 0;
                }

                unsafe {
                    if prev.is_null() {
                        self.head = next;
                    } else {
                        (*prev).next = next;
                    }
                    if front_gap != 0 {
                        self.insert(block_start, front_gap);
                    }
                    if back_gap != 0 {
                        self.insert(used_end, back_gap);
                    }
                }
                return alloc_start as *mut u8;
            }

            prev = current;
            current = next;
        }
        null_mut()
    }
}

fn block_layout(layout: Layout) -> (usize, usize) {
    let align = layout.align().max(BLOCK_ALIGN);
    let size = align_up(layout.size().max(MIN_BLOCK) as u64, BLOCK_ALIGN as u64) as usize;
    (size, align)
}

/// Küçük ayırmanın sınıfı (boyut ve hizanın en büyüğüne göre ikinin kuvveti).
fn class_of(layout: Layout) -> Option<usize> {
    let need = layout.size().max(layout.align()).max(MIN_CLASS);
    (need <= MAX_CLASS).then(|| need.next_power_of_two().trailing_zeros() as usize - 4)
}

struct State {
    heap: Heap,
    classes: [*mut ClassNode; CLASSES],
}

unsafe impl Send for State {}

impl State {
    fn take_large(&mut self, size: usize, align: usize) -> *mut u8 {
        let ptr = self.heap.take(size, align);
        if !ptr.is_null() {
            return ptr;
        }
        if self.heap.grow((size + align) as u64) {
            return self.heap.take(size, align);
        }
        null_mut()
    }

    fn take_small(&mut self, class: usize) -> *mut u8 {
        if self.classes[class].is_null() {
            let object = MIN_CLASS << class;
            let slab = self.take_large(SLAB, object.max(BLOCK_ALIGN));
            if slab.is_null() {
                return null_mut();
            }
            for i in (0..SLAB / object).rev() {
                let node = (slab as usize + i * object) as *mut ClassNode;
                unsafe {
                    node.write(ClassNode {
                        next: self.classes[class],
                    })
                };
                self.classes[class] = node;
            }
        }
        let node = self.classes[class];
        self.classes[class] = unsafe { (*node).next };
        node as *mut u8
    }
}

pub struct KernelAllocator {
    state: SpinLock<State>,
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut state = self.state.lock();
        if let Some(class) = class_of(layout) {
            return state.take_small(class);
        }
        let (size, align) = block_layout(layout);
        state.take_large(size, align)
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut state = self.state.lock();
        if let Some(class) = class_of(layout) {
            let node = ptr as *mut ClassNode;
            unsafe {
                node.write(ClassNode {
                    next: state.classes[class],
                })
            };
            state.classes[class] = node;
            return;
        }
        let (size, _) = block_layout(layout);
        unsafe { state.heap.insert(ptr as usize, size) };
    }
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator {
    state: SpinLock::new(State {
        heap: Heap::empty(),
        classes: [null_mut(); CLASSES],
    }),
};

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    if !ALLOCATOR.state.lock().heap.grow(HEAP_INITIAL) {
        return Err("could not map the initial heap");
    }
    check("heap", self_test())?;
    let _ = write!(detail, "{} KiB mapped", mapped_bytes() / 1024);
    Ok(())
}

crate::initcall!(core, 30, "Kernel heap", init);

pub fn mapped_bytes() -> u64 {
    ALLOCATOR.state.lock().heap.end - HEAP_START
}

fn self_test() -> bool {
    use alloc::boxed::Box;
    use alloc::string::String;
    use alloc::vec::Vec;

    let boxed = Box::new(0xdead_beef_u64);
    let mut numbers: Vec<u64> = Vec::new();
    for i in 0..200_000 {
        numbers.push(i);
    }
    let sum_ok = numbers.iter().sum::<u64>() == 199_999 * 200_000 / 2;

    let mut text = String::from("Banna");
    text.push_str(" OS");

    #[repr(align(4096))]
    struct PageAligned([u8; 64]);
    let aligned = Box::new(PageAligned([7; 64]));
    let aligned_ok = (&*aligned as *const PageAligned as usize) % 4096 == 0 && aligned.0[63] == 7;

    let ok = *boxed == 0xdead_beef && sum_ok && text == "Banna OS" && aligned_ok;
    drop(numbers);
    drop(aligned);

    let before = mapped_bytes();
    let mut churn: Vec<Box<[u8; 128]>> = Vec::new();
    for round in 0..4 {
        for _ in 0..512 {
            churn.push(Box::new([round as u8; 128]));
        }
        churn.clear();
    }
    let reuse_ok = mapped_bytes() == before;

    ok && reuse_ok
}
