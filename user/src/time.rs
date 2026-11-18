//! Zaman.

use crate::sys;

pub fn uptime_ms() -> u64 {
    sys::call(sys::UPTIME_MS, &[]) as u64
}

/// Açılıştan beri nanosaniye (TSC).
pub fn now_ns() -> u64 {
    sys::call(sys::TIME_NS, &[]) as u64
}

pub fn sleep_ms(ms: u64) {
    sys::call(sys::SLEEP_MS, &[ms]);
}

pub fn unix_time() -> i64 {
    sys::call(sys::TIME, &[])
}
