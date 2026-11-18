//! libbanna: Banna OS kullanıcı programları için çalışma zamanı.
//!
//! Giriş noktası, syscall sarmalayıcıları, `print!`, heap ayırıcı, dosya,
//! işlem ve pencere API'leri.

#![no_std]

extern crate alloc;

pub mod blockdev;
pub mod brs;
/// SHA-256, HMAC, PBKDF2 ve onaltılık yardımcılar (çekirdekle aynı kod).
#[path = "../../src/crypto.rs"]
pub mod crypto;
pub mod desktop;
pub mod ecosystem;
/// ext2 biçimlendirme ve erişim (çekirdekle aynı kod).
#[path = "../../src/fs/ext2.rs"]
#[allow(dead_code)]
pub mod ext2;
pub mod fat;
pub mod fs;
pub mod gfx;
pub mod gpt;
pub mod gui;
mod heap;
/// Uygulama ve ayar simgeleri (masaüstüyle aynı kod).
#[path = "../../src/gui/icons.rs"]
#[allow(dead_code)]
pub mod icons;
pub mod io;
pub mod json;
/// MP3 (MPEG Layer III) çözücü ve ID3 etiketleri.
pub mod mp3;
pub mod net;
pub mod process;
pub mod sys;
pub mod system;
pub mod sysupdate;
pub mod task;
/// Masaüstü tercihleri ve tema renkleri (masaüstüyle aynı kod).
#[path = "../../src/gui/theme.rs"]
pub mod theme;
pub mod time;
/// TLS 1.2 istemcisi (https).
pub mod tls;
pub mod tty;
pub mod ui;
pub mod web;
/// Duvar kağıdı desenleri ve BMP/PPM çözücüleri (masaüstüyle aynı kod).
#[path = "../../src/gui/wallpaper_art.rs"]
pub mod wallpaper_art;

pub use alloc::{boxed::Box, format, string::String, vec, vec::Vec};

use core::panic::PanicInfo;

/// Programın giriş noktasını tanımlar: `banna::entry!(main);` ve `fn main(args: &[&str]) -> i32`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[unsafe(no_mangle)]
        /// # Safety
        /// Yalnızca çekirdek, geçerli bir argüman dizisiyle çağırır.
        pub unsafe extern "C" fn _start(argc: usize, argv: *const [u64; 2]) -> ! {
            let args = unsafe { $crate::process::collect_args(argc, argv) };
            let refs: $crate::Vec<&str> = args.iter().map(|s| s.as_str()).collect();
            let code: i32 = $main(&refs);
            $crate::process::exit(code)
        }
    };
}

/// Banna OS programının giriş noktası (brct ile derlenen uygulamalar bunu kullanır):
///
/// ```ignore
/// #![no_std]
/// #![no_main]
/// use banna::prelude::*;
///
/// banna::main!(main);
///
/// fn main(args: &[&str]) -> i32 {
///     println!("Merhaba Banna!");
///     0
/// }
/// ```
#[macro_export]
macro_rules! main {
    ($main:path) => {
        $crate::entry!($main);
    };
}

/// Uygulamaların sık kullandıkları: `use banna::prelude::*;`
pub mod prelude {
    pub use crate::gfx::{Canvas, Rect, rgb};
    pub use crate::gui::{
        EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
        EVENT_WHEEL, Window,
    };
    pub use crate::theme::{Prefs, Theme};
    pub use crate::{Box, String, Vec, desktop, format, fs, net, process, time, ui, vec};
    pub use crate::{eprintln, print, println};
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    io::write_err(&format!("panic: {}\n", info));
    process::exit(101)
}
