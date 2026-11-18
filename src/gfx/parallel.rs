//! Çekirdekler arası paralel iş: `for_each(n, f)` f(0..n)'i çağıranla birlikte tüm
//! çevrimiçi çekirdeklerdeki işçi thread'lerinde çalıştırır (compositor şeritleri).

use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use crate::sched::{self, Mutex, WaitQueue};
use crate::sync::SpinLock;

const MAX_WORKERS: usize = 63;

#[derive(Clone, Copy)]
struct Job {
    f: *const (dyn Fn(usize) + Sync),
    count: usize,
}

unsafe impl Send for Job {}

static JOB: SpinLock<Option<Job>> = SpinLock::new(None);
static NEXT: AtomicUsize = AtomicUsize::new(0);
static DONE: AtomicUsize = AtomicUsize::new(0);
/// İş tanıtıcısını almış ve henüz bırakmamış işçi sayısı.
static ACTIVE: AtomicUsize = AtomicUsize::new(0);
static GENERATION: AtomicU64 = AtomicU64::new(0);
static WAKE: WaitQueue = WaitQueue::new();
static WORKERS: AtomicUsize = AtomicUsize::new(0);
static SERIAL: Mutex<()> = Mutex::new(());

fn claim(job: &Job) {
    loop {
        let i = NEXT.fetch_add(1, Ordering::AcqRel);
        if i >= job.count {
            return;
        }
        unsafe { (*job.f)(i) };
        DONE.fetch_add(1, Ordering::AcqRel);
    }
}

fn worker() -> i64 {
    let mut seen = 0;
    loop {
        WAKE.wait_until(|| GENERATION.load(Ordering::Acquire) != seen);
        seen = GENERATION.load(Ordering::Acquire);
        let job = {
            let guard = JOB.lock();
            let job = *guard;
            if job.is_some() {
                ACTIVE.fetch_add(1, Ordering::AcqRel);
            }
            job
        };
        if let Some(job) = job {
            claim(&job);
            ACTIVE.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// İşçileri başlatır (çevrimiçi çekirdek sayısı - 1); bir kez çağrılır.
pub fn start() -> usize {
    let wanted = crate::sched::online_cpus()
        .saturating_sub(1)
        .min(MAX_WORKERS);
    for i in 0..wanted {
        if sched::spawn_detached(&alloc::format!("render/{}", i + 1), worker).is_some() {
            WORKERS.fetch_add(1, Ordering::Relaxed);
        }
    }
    WORKERS.load(Ordering::Relaxed)
}

pub fn workers() -> usize {
    WORKERS.load(Ordering::Relaxed)
}

/// `f(0)..f(count-1)`'i paralel çalıştırır ve hepsi bitene kadar bekler.
pub fn for_each(count: usize, f: &(dyn Fn(usize) + Sync)) {
    if count <= 1 || workers() == 0 {
        (0..count).for_each(f);
        return;
    }
    let _serial = SERIAL.lock();
    // İşçiler `f`'yi yalnızca bu fonksiyon dönmeden önce kullanır (aşağıdaki bekleme).
    let job = Job {
        f: unsafe {
            core::mem::transmute::<&(dyn Fn(usize) + Sync), &'static (dyn Fn(usize) + Sync)>(f)
        },
        count,
    };
    NEXT.store(0, Ordering::Release);
    DONE.store(0, Ordering::Release);
    *JOB.lock() = Some(job);
    GENERATION.fetch_add(1, Ordering::AcqRel);
    WAKE.wake_all();
    claim(&job);
    while DONE.load(Ordering::Acquire) < count {
        core::hint::spin_loop();
    }
    *JOB.lock() = None;
    while ACTIVE.load(Ordering::Acquire) != 0 {
        core::hint::spin_loop();
    }
}
