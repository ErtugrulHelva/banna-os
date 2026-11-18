//! Kesme-güvenli spinlock.

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::ops::{Deref, DerefMut};
use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::cpu;

pub struct SpinLock<T> {
    locked: AtomicBool,
    data: UnsafeCell<T>,
}

unsafe impl<T: Send> Sync for SpinLock<T> {}
unsafe impl<T: Send> Send for SpinLock<T> {}

pub struct SpinLockGuard<'a, T> {
    lock: &'a SpinLock<T>,
    restore_interrupts: bool,
}

impl<T> SpinLock<T> {
    pub const fn new(value: T) -> Self {
        Self {
            locked: AtomicBool::new(false),
            data: UnsafeCell::new(value),
        }
    }

    pub fn lock(&self) -> SpinLockGuard<'_, T> {
        let restore_interrupts = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            while self.locked.load(Ordering::Relaxed) {
                spin_loop();
            }
        }
        SpinLockGuard {
            lock: self,
            restore_interrupts,
        }
    }

    /// `timeout_ns` içinde alınamazsa `None` (tanı kodu takılmasın diye).
    pub fn try_lock_for(&self, timeout_ns: u64) -> Option<SpinLockGuard<'_, T>> {
        let restore_interrupts = cpu::interrupts_enabled();
        cpu::disable_interrupts();
        let deadline = crate::arch::tsc::now_ns() + timeout_ns;
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            if crate::arch::tsc::now_ns() > deadline {
                if restore_interrupts {
                    cpu::enable_interrupts();
                }
                return None;
            }
            spin_loop();
        }
        Some(SpinLockGuard {
            lock: self,
            restore_interrupts,
        })
    }

    /// Yalnızca panik yolunda, kilidi tutan kod bir daha çalışmayacaksa kullanılır.
    pub unsafe fn force_unlock(&self) {
        self.locked.store(false, Ordering::Release);
    }
}

impl<T> Deref for SpinLockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinLockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinLockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.locked.store(false, Ordering::Release);
        if self.restore_interrupts {
            cpu::enable_interrupts();
        }
    }
}
