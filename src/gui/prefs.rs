//! Masaüstü tercihlerinin saklanması: `/etc/banna/desktop.conf`.
//!
//! Dosyada bulunmayan sistem ayarlarına (ses, klavye, parlaklık...) açılışta dokunulmaz.

use alloc::string::String;
use core::sync::atomic::{AtomicBool, Ordering};

use crate::sync::SpinLock;

pub use super::theme::*;

pub const PATH: &str = "/etc/banna/desktop.conf";

pub fn load() -> Prefs {
    let text = crate::fs::read_all(PATH)
        .map(|data| String::from(String::from_utf8_lossy(&data)))
        .unwrap_or_default();
    let mut prefs = Prefs::parse(&text);
    // Öykünmede animasyon kareleri ağır çekim gibi görünür ve her biri tüm alanı yeniden
    // çizer: kullanıcı Ayarlar'dan açmadıysa kapalı başlar.
    if crate::arch::cpuinfo::emulated() && !text.lines().any(|l| l.starts_with("animations=")) {
        prefs.animations = false;
    }
    prefs
}

static PENDING: SpinLock<Option<String>> = SpinLock::new(None);
static SAVER: AtomicBool = AtomicBool::new(false);

/// Ayarları arka planda kaydeder; art arda gelen değişiklikler tek yazmada birleşir.
pub fn save(prefs: &Prefs) {
    *PENDING.lock() = Some(prefs.serialize());
    if SAVER.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::sched::spawn_detached("prefs-save", || {
        crate::sched::sleep_ms(400);
        SAVER.store(false, Ordering::Release);
        let Some(text) = PENDING.lock().take() else {
            return 0;
        };
        if crate::fs::stat("/etc/banna").is_err() {
            let _ = crate::fs::mkdir("/etc/banna");
        }
        match crate::fs::write_all(PATH, text.as_bytes()) {
            Ok(()) => {
                crate::log!("prefs: saved");
                0
            }
            Err(e) => {
                crate::log!("prefs: could not save ({})", e.message());
                -1
            }
        }
    });
}
