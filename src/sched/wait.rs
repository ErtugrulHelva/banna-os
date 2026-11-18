//! Bekleme kuyrukları ve tamamlanma nesneleri.

use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use super::{Tid, block_current, block_current_until, current_tid, is_running, wake};
use crate::arch::cpu;
use crate::arch::pit;
use crate::sync::SpinLock;

pub struct WaitQueue {
    waiters: SpinLock<Vec<Tid>>,
}

impl WaitQueue {
    pub const fn new() -> Self {
        Self {
            waiters: SpinLock::new(Vec::new()),
        }
    }

    /// `ready` doğru olana kadar geçerli thread'i uyutur.
    ///
    /// Thread koşula bakmadan önce kuyruğa yazılır: arada gelen uyandırma
    /// `pending_wake` olarak kalır ve bloklanma hemen biter.
    pub fn wait_until(&self, ready: impl Fn() -> bool) {
        loop {
            let done = cpu::without_interrupts(|| {
                if !is_running() {
                    return ready();
                }
                let tid = current_tid();
                self.waiters.lock().push(tid);
                if ready() {
                    self.forget(tid);
                    return true;
                }
                block_current();
                self.forget(tid);
                false
            });
            if done {
                return;
            }
            if !is_running() {
                cpu::enable_interrupts();
                cpu::halt();
            }
        }
    }

    /// `ready` doğru olana ya da süre dolana kadar bekler; koşul sağlandıysa `true`.
    pub fn wait_timeout(&self, ready: impl Fn() -> bool, timeout_ms: u64) -> bool {
        let deadline = pit::uptime_ms() + timeout_ms;
        loop {
            if ready() {
                return true;
            }
            if pit::uptime_ms() >= deadline || !is_running() {
                return ready();
            }
            cpu::without_interrupts(|| {
                let tid = current_tid();
                self.waiters.lock().push(tid);
                if ready() {
                    self.forget(tid);
                    return;
                }
                block_current_until(deadline);
                self.forget(tid);
            });
        }
    }

    fn forget(&self, tid: Tid) {
        self.waiters.lock().retain(|&t| t != tid);
    }

    pub fn wake_all(&self) {
        let tids = core::mem::take(&mut *self.waiters.lock());
        if !tids.is_empty() {
            wake(&tids);
        }
    }
}

pub struct Completion {
    done: AtomicBool,
    code: AtomicI64,
    queue: WaitQueue,
}

impl Completion {
    pub const fn new() -> Self {
        Self {
            done: AtomicBool::new(false),
            code: AtomicI64::new(0),
            queue: WaitQueue::new(),
        }
    }

    pub fn complete(&self, code: i64) {
        self.code.store(code, Ordering::Release);
        self.done.store(true, Ordering::Release);
        self.queue.wake_all();
    }

    pub fn is_done(&self) -> bool {
        self.done.load(Ordering::Acquire)
    }

    pub fn wait(&self) -> i64 {
        self.queue.wait_until(|| self.is_done());
        self.code.load(Ordering::Acquire)
    }
}
