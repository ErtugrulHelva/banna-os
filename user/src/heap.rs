//! `mmap` ile büyüyen ayırıcı. Küçük nesneler (16-4096 bayt) boyut sınıflarının serbest
//! listelerinden sabit sürede verilir; büyükler adrese göre sıralı, birleştirmeli first-fit
//! listeden. (Eskiden her şey tek listedeydi: binlerce küçük nesne tutan programlarda —
//! web motoru — her ayırma ve bırakma listeyi baştan tarıyordu.)

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::ptr::null_mut;

use crate::sys;

const CHUNK: usize = 256 * 1024;

struct Block {
    size: usize,
    next: *mut Block,
}

const MIN: usize = core::mem::size_of::<Block>();

/// Boyut sınıfları: 16, 32, ..., 4096 bayt (her nesne kendi boyutuna hizalı).
const CLASSES: usize = 9;
const MIN_CLASS: usize = 16;
const MAX_CLASS: usize = MIN_CLASS << (CLASSES - 1);
/// Bir sınıf boşalınca büyük listeden alınan dilim.
const SLAB: usize = 32 * 1024;

struct Node {
    next: *mut Node,
}

struct Heap {
    head: UnsafeCell<*mut Block>,
    classes: UnsafeCell<[*mut Node; CLASSES]>,
}

/// Küçük ayırmanın sınıfı (boyut ve hizanın en büyüğüne göre ikinin kuvveti).
fn class_of(l: Layout) -> Option<usize> {
    let need = l.size().max(l.align()).max(MIN_CLASS);
    (need <= MAX_CLASS).then(|| need.next_power_of_two().trailing_zeros() as usize - 4)
}

unsafe impl Sync for Heap {}

fn align_up(v: usize, a: usize) -> usize {
    (v + a - 1) & !(a - 1)
}

impl Heap {
    unsafe fn insert(&self, addr: usize, size: usize) {
        let head = self.head.get();
        let mut prev: *mut Block = null_mut();
        let mut cur = unsafe { *head };
        while !cur.is_null() && (cur as usize) < addr {
            prev = cur;
            cur = unsafe { (*cur).next };
        }
        let block = addr as *mut Block;
        unsafe {
            block.write(Block { size, next: cur });
            if !cur.is_null() && addr + size == cur as usize {
                (*block).size += (*cur).size;
                (*block).next = (*cur).next;
            }
            if prev.is_null() {
                *head = block;
            } else if prev as usize + (*prev).size == addr {
                (*prev).size += (*block).size;
                (*prev).next = (*block).next;
            } else {
                (*prev).next = block;
            }
        }
    }

    unsafe fn take(&self, size: usize, align: usize) -> *mut u8 {
        let head = self.head.get();
        let mut prev: *mut Block = null_mut();
        let mut cur = unsafe { *head };
        while !cur.is_null() {
            let start = cur as usize;
            let (bsize, next) = unsafe { ((*cur).size, (*cur).next) };
            let end = start + bsize;
            let mut alloc = align_up(start, align);
            if alloc != start && alloc - start < MIN {
                alloc = align_up(start + MIN, align);
            }
            let alloc_end = alloc + size;
            if alloc_end <= end {
                unsafe {
                    if prev.is_null() {
                        *head = next;
                    } else {
                        (*prev).next = next;
                    }
                    if alloc != start {
                        self.insert(start, alloc - start);
                    }
                    if end - alloc_end >= MIN {
                        self.insert(alloc_end, end - alloc_end);
                    }
                }
                return alloc as *mut u8;
            }
            prev = cur;
            cur = next;
        }
        null_mut()
    }
}

fn layout(l: Layout) -> (usize, usize) {
    let align = l.align().max(core::mem::align_of::<Block>());
    (
        align_up(l.size().max(MIN), core::mem::align_of::<Block>()),
        align,
    )
}

impl Heap {
    unsafe fn alloc_large(&self, size: usize, align: usize) -> *mut u8 {
        let ptr = unsafe { self.take(size, align) };
        if !ptr.is_null() {
            return ptr;
        }
        let chunk = align_up((size + align).max(CHUNK), 4096);
        let addr = sys::call(sys::MMAP, &[chunk as u64]);
        if addr <= 0 {
            return null_mut();
        }
        unsafe {
            self.insert(addr as usize, chunk);
            self.take(size, align)
        }
    }

    unsafe fn alloc_small(&self, class: usize) -> *mut u8 {
        let classes = unsafe { &mut *self.classes.get() };
        if classes[class].is_null() {
            let object = MIN_CLASS << class;
            let slab = unsafe { self.alloc_large(SLAB, object.max(core::mem::align_of::<Block>())) };
            if slab.is_null() {
                return null_mut();
            }
            for i in (0..SLAB / object).rev() {
                let node = (slab as usize + i * object) as *mut Node;
                unsafe { node.write(Node { next: classes[class] }) };
                classes[class] = node;
            }
        }
        let node = classes[class];
        classes[class] = unsafe { (*node).next };
        node as *mut u8
    }
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        if let Some(class) = class_of(l) {
            return unsafe { self.alloc_small(class) };
        }
        let (size, align) = layout(l);
        unsafe { self.alloc_large(size, align) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, l: Layout) {
        if let Some(class) = class_of(l) {
            let classes = unsafe { &mut *self.classes.get() };
            let node = ptr as *mut Node;
            unsafe { node.write(Node { next: classes[class] }) };
            classes[class] = node;
            return;
        }
        let (size, _) = layout(l);
        unsafe { self.insert(ptr as usize, size) };
    }

    /// Aynı sınıfta kalan büyüme/küçülme yerinde olur (dizeye karakter eklemek gibi).
    unsafe fn realloc(&self, ptr: *mut u8, l: Layout, new_size: usize) -> *mut u8 {
        let new = unsafe { Layout::from_size_align_unchecked(new_size, l.align()) };
        if let Some(class) = class_of(l)
            && class_of(new) == Some(class)
        {
            return ptr;
        }
        let out = unsafe { self.alloc(new) };
        if !out.is_null() {
            unsafe {
                core::ptr::copy_nonoverlapping(ptr, out, l.size().min(new_size));
                self.dealloc(ptr, l);
            }
        }
        out
    }
}

#[global_allocator]
static HEAP: Heap = Heap {
    head: UnsafeCell::new(null_mut()),
    classes: UnsafeCell::new([null_mut(); CLASSES]),
};
