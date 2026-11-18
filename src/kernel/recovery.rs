//! Kurtarma kipi: açılışta B tuşu (ya da `recovery` komut satırı bayrağı) ile girilir.
//! Masaüstü açılmaz; yönetici girişinden sonra çekirdek kabuğu root yetkisiyle,
//! sistemi değiştiren tüm komutlarla çalışır.

use core::sync::atomic::{AtomicBool, Ordering};

use crate::arch::pit;
use crate::boot;
use crate::drivers::keyboard::{self, Key};
use crate::sched;

/// Açılış bittikten sonra B tuşunun bekleneceği süre (geç tanınan USB klavyeler için).
const WINDOW_MS: u64 = 900;

static ACTIVE: AtomicBool = AtomicBool::new(false);

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

/// Açılış sırasında basılan tuşları tarar; B varsa kurtarma kipini açar.
pub fn detect() -> bool {
    // Logo boyunca basılan B, klavye sürücüsünde işaretlenir (kuyruk taşsa bile).
    let mut requested = boot::has_flag("recovery") || keyboard::b_pressed();
    if !requested {
        crate::gfx::splash::set_step("Kurtarma kipi için B tuşuna basın");
    }
    let deadline = pit::uptime_ms() + WINDOW_MS;
    while !requested && pit::uptime_ms() < deadline {
        while let Some(event) = keyboard::read_event() {
            if matches!(event.key, Key::Char('b' | 'B')) {
                requested = true;
            }
        }
        requested |= keyboard::b_pressed();
        sched::sleep_ms(10);
    }
    if requested {
        crate::gfx::splash::set_step("Kurtarma kipi açılıyor");
    }
    while keyboard::read_event().is_some() {}
    if requested {
        crate::log!(
            "recovery: entering recovery mode ({})",
            if boot::has_flag("recovery") {
                "cmdline"
            } else {
                "B key"
            }
        );
    }
    ACTIVE.store(requested, Ordering::Relaxed);
    if requested {
        // Kurtarma kipine girildiğini duyurur (HD Audio, yoksa PC hoparlörü).
        crate::drivers::audio::beep(880, 120);
        crate::sched::sleep_ms(60);
        crate::drivers::audio::beep(660, 180);
    }
    requested
}
