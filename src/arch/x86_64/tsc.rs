//! TSC tabanlı yüksek çözünürlüklü saat (PIT ile kalibre edilir).

use core::arch::x86_64::_rdtsc;
use core::sync::atomic::{AtomicU64, Ordering};

static TSC_PER_US: AtomicU64 = AtomicU64::new(0);
static TSC_BASE: AtomicU64 = AtomicU64::new(0);
/// Kalibrasyon başladığında PIT'e göre geçmiş süre (ms).
static BASE_MS: AtomicU64 = AtomicU64::new(0);

pub fn read() -> u64 {
    unsafe { _rdtsc() }
}

pub fn calibrate(tsc_start: u64, tsc_end: u64, elapsed_us: u64) {
    if elapsed_us == 0 || tsc_end <= tsc_start {
        return;
    }
    // Taban değerler önce yazılır: TSC_PER_US sıfırdan çıkınca `uptime_ms` TSC'ye geçer.
    let pit_ms = super::pit::tick_ms();
    BASE_MS.store(pit_ms.saturating_sub(elapsed_us / 1000), Ordering::Relaxed);
    TSC_BASE.store(tsc_start, Ordering::Relaxed);
    TSC_PER_US.store(
        ((tsc_end - tsc_start) / elapsed_us).max(1),
        Ordering::Release,
    );
}

/// Frekansı ölçülmeden bilinen TSC'yi (CPUID) şu andan itibaren saat yapar.
pub fn set_mhz(per_us: u64) {
    BASE_MS.store(super::pit::tick_ms(), Ordering::Relaxed);
    TSC_BASE.store(read(), Ordering::Relaxed);
    TSC_PER_US.store(per_us.max(1), Ordering::Release);
}

/// Açılıştan beri geçen süre (ms), TSC'den; kalibre edilmediyse `None`.
pub fn uptime_ms() -> Option<u64> {
    (TSC_PER_US.load(Ordering::Relaxed) != 0)
        .then(|| BASE_MS.load(Ordering::Relaxed) + now_ns() / 1_000_000)
}

pub fn mhz() -> u64 {
    TSC_PER_US.load(Ordering::Relaxed)
}

/// Kalibrasyondan beri geçen nanosaniye; kalibre edilmediyse PIT'e düşer.
pub fn now_ns() -> u64 {
    let per_us = TSC_PER_US.load(Ordering::Relaxed);
    if per_us == 0 {
        return super::pit::uptime_ms() * 1_000_000;
    }
    let delta = read().saturating_sub(TSC_BASE.load(Ordering::Relaxed));
    (delta as u128 * 1000 / per_us as u128) as u64
}

pub fn busy_wait_us(us: u64) {
    let target = now_ns() + us * 1000;
    while now_ns() < target {
        core::hint::spin_loop();
    }
}
