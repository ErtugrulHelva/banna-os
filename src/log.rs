//! Çıktı makroları: `print!`/`println!` ekran + seri, `log!` yalnızca seri.

use core::fmt::{self, Write};

use crate::drivers::console::{self, CONSOLE};
use crate::drivers::serial::COM1_PORT;

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::log::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::log::print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log::serial(format_args!("{}\n", format_args!($($arg)*))) };
}

/// Açılış günlüğü: seri çıktının ilk `BOOT_LOG_SIZE` baytı (COM1 kilidi altında yazılır).
/// Seri portu olmayan gerçek donanımda `save_boot_log` ile ESP'ye kaydedilir.
const BOOT_LOG_SIZE: usize = 128 * 1024;
static mut BOOT_LOG: [u8; BOOT_LOG_SIZE] = [0; BOOT_LOG_SIZE];
static mut BOOT_LOG_LEN: usize = 0;

struct BootLog;

impl Write for BootLog {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        unsafe {
            let len = BOOT_LOG_LEN;
            let n = s.len().min(BOOT_LOG_SIZE - len);
            let log = &mut *core::ptr::addr_of_mut!(BOOT_LOG);
            log[len..len + n].copy_from_slice(&s.as_bytes()[..n]);
            BOOT_LOG_LEN = len + n;
        }
        Ok(())
    }
}

pub fn serial(args: fmt::Arguments) {
    let mut port = COM1_PORT.lock();
    let _ = port.write_fmt(args);
    let _ = BootLog.write_fmt(args);
}

/// Açılıştan beri tutulan günlüğün kopyası (`dmesg`).
pub fn boot_log() -> alloc::vec::Vec<u8> {
    let _port = COM1_PORT.lock();
    unsafe {
        let log = &*core::ptr::addr_of!(BOOT_LOG);
        alloc::vec::Vec::from(&log[..BOOT_LOG_LEN])
    }
}

/// Açılış günlüğünü `path`'e yazar (ör. ESP'deki `/boot/boot/kernel.log`).
pub fn save_boot_log(path: &str) -> bool {
    let copy = {
        let _port = COM1_PORT.lock();
        unsafe {
            let log = &*core::ptr::addr_of!(BOOT_LOG);
            alloc::vec::Vec::from(&log[..BOOT_LOG_LEN])
        }
    };
    crate::fs::write_all(path, &copy).is_ok()
}

pub fn print(args: fmt::Arguments) {
    if crate::kernel::shell::redirected(args) {
        return;
    }
    serial(args);
    let _ = CONSOLE.lock().write_fmt(args);
    console::flush();
}

/// Kilitleri zorla açarak yazar; yalnızca panik/exception yolunda, geri dönülmeyecekse kullanılır.
pub fn emergency(args: fmt::Arguments) {
    unsafe { COM1_PORT.force_unlock() };
    console::emergency_takeover();
    serial(args);
    let mut console = CONSOLE.lock();
    console.fg = console::RED;
    let _ = console.write_fmt(args);
}

fn status(tag: &str, color: console::Color, name: fmt::Arguments) {
    serial(format_args!("[{}] {}\n", tag, name));
    let mut console = CONSOLE.lock();
    let previous = console.fg;
    let _ = console.write_str("[");
    console.fg = color;
    let _ = console.write_str(tag);
    console.fg = previous;
    let _ = console.write_fmt(format_args!("] {}\n", name));
    drop(console);
    console::flush();
}

pub fn ok(name: fmt::Arguments) {
    status(" OK ", console::GREEN, name);
}

pub fn fail(name: fmt::Arguments) {
    status("FAIL", console::RED, name);
}
