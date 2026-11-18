//! Koruma sayfalı çekirdek yığınları.
//!
//! Serbest bırakılan yuvalar eşlemeleriyle birlikte yeniden kullanılır; çekirdek
//! eşlemesi hiç kaldırılmadığı için diğer çekirdeklerde bayat TLB kaydı oluşmaz.

use alloc::vec::Vec;

use super::paging::{self, NO_EXECUTE, WRITABLE};
use super::{PAGE_SIZE, pmm};
use crate::sync::SpinLock;

const REGION_START: u64 = 0xffff_d800_0000_0000;
const SLOT_SIZE: u64 = 1024 * 1024;
const MAX_SLOTS: u64 = 4096;

pub struct KernelStack {
    slot: u64,
    pages: u64,
}

impl KernelStack {
    pub fn top(&self) -> u64 {
        top_of(self.slot)
    }
}

struct Slots {
    next: u64,
    /// (yuva, eşli sayfa sayısı)
    free: Vec<(u64, u64)>,
}

static SLOTS: SpinLock<Slots> = SpinLock::new(Slots {
    next: 0,
    free: Vec::new(),
});

fn top_of(slot: u64) -> u64 {
    REGION_START + (slot + 1) * SLOT_SIZE
}

pub fn alloc(size: u64) -> Option<KernelStack> {
    let pages = size.div_ceil(PAGE_SIZE).clamp(1, SLOT_SIZE / PAGE_SIZE - 1);
    let (slot, mapped) = {
        let mut slots = SLOTS.lock();
        let reuse = slots
            .free
            .iter()
            .position(|&(_, m)| m >= pages)
            .or(slots.free.len().checked_sub(1));
        match reuse {
            Some(i) => slots.free.swap_remove(i),
            None if slots.next < MAX_SLOTS => {
                slots.next += 1;
                (slots.next - 1, 0)
            }
            None => return None,
        }
    };
    let top = top_of(slot);
    for i in mapped..pages {
        let virt = top - (i + 1) * PAGE_SIZE;
        let Some(frame) = pmm::alloc_frame() else {
            SLOTS.lock().free.push((slot, i));
            return None;
        };
        if paging::map_page_in(paging::kernel_root(), virt, frame, WRITABLE | NO_EXECUTE).is_err() {
            pmm::free_frame(frame);
            SLOTS.lock().free.push((slot, i));
            return None;
        }
    }
    Some(KernelStack {
        slot,
        pages: pages.max(mapped),
    })
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        SLOTS.lock().free.push((self.slot, self.pages));
    }
}
