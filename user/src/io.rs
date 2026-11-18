//! Konsol çıktısı: `print!`, `println!`, `eprintln!`.

use core::fmt::{self, Write};

use crate::sys;

pub fn write_fd(fd: u64, bytes: &[u8]) -> i64 {
    sys::call(sys::WRITE, &[fd, bytes.as_ptr() as u64, bytes.len() as u64])
}

pub fn write_out(text: &str) {
    write_fd(1, text.as_bytes());
}

pub fn write_err(text: &str) {
    write_fd(2, text.as_bytes());
}

pub fn print(args: fmt::Arguments) {
    let mut line = crate::String::new();
    let _ = line.write_fmt(args);
    write_out(&line);
}

/// Klavyeden bloklamadan en fazla `buf.len()` karakter okur.
pub fn read_stdin(buf: &mut [u8]) -> usize {
    let n = sys::call(sys::READ, &[0, buf.as_mut_ptr() as u64, buf.len() as u64]);
    n.max(0) as usize
}

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => { $crate::io::print(format_args!($($arg)*)) };
}

#[macro_export]
macro_rules! println {
    () => { $crate::print!("\n") };
    ($($arg:tt)*) => { $crate::io::print(format_args!("{}\n", format_args!($($arg)*))) };
}

#[macro_export]
macro_rules! eprintln {
    ($($arg:tt)*) => { $crate::io::write_err(&$crate::format!("{}\n", format_args!($($arg)*))) };
}

pub fn read_fd(fd: u64, buf: &mut [u8]) -> i64 {
    sys::call(sys::READ, &[fd, buf.as_mut_ptr() as u64, buf.len() as u64])
}

/// Tümünü yazar; hata olursa o ana kadar yazılanla birlikte döner.
pub fn write_all(fd: u64, mut bytes: &[u8]) -> Result<(), i64> {
    while !bytes.is_empty() {
        let n = write_fd(fd, bytes);
        if n <= 0 {
            return Err(n);
        }
        bytes = &bytes[n as usize..];
    }
    Ok(())
}

pub fn close(fd: u64) -> i64 {
    sys::call(sys::CLOSE, &[fd])
}

/// `(okuma ucu, yazma ucu)`.
pub fn pipe() -> Result<(u64, u64), i64> {
    let mut fds = [0u32; 2];
    let r = sys::call(sys::PIPE, &[fds.as_mut_ptr() as u64]);
    if r < 0 {
        Err(r)
    } else {
        Ok((fds[0] as u64, fds[1] as u64))
    }
}

/// `old` tanıtıcısını `new` yuvasına kopyalar (eskisi kapanır).
pub fn dup2(old: u64, new: u64) -> Result<u64, i64> {
    let r = sys::call(sys::DUP2, &[old, new]);
    if r < 0 { Err(r) } else { Ok(r as u64) }
}

pub const POLL_FD: u16 = 0;
pub const POLL_WINDOW: u16 = 1;

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct PollEntry {
    pub id: u32,
    pub kind: u16,
    pub ready: u16,
}

impl PollEntry {
    pub fn fd(fd: u64) -> Self {
        Self {
            id: fd as u32,
            kind: POLL_FD,
            ready: 0,
        }
    }

    pub fn window(id: u64) -> Self {
        Self {
            id: id as u32,
            kind: POLL_WINDOW,
            ready: 0,
        }
    }
}

/// Girdilerden biri hazır olana kadar bekler; `None` = sonsuz. Hazır sayısını döndürür.
pub fn poll(entries: &mut [PollEntry], timeout_ms: Option<u64>) -> i64 {
    sys::call(
        sys::POLL,
        &[
            entries.as_mut_ptr() as u64,
            entries.len() as u64,
            timeout_ms.unwrap_or(u64::MAX),
        ],
    )
}

/// Seri porta (çekirdek günlüğüne) bir satır yazar.
pub fn log(text: &str) {
    sys::call(sys::LOG, &[text.as_ptr() as u64, text.len() as u64]);
}

/// Soruyu yazar, bir satır okur; yanıt "evet" ise `true`.
pub fn confirm(question: &str) -> bool {
    print(format_args!("{} [evet/hayır]: ", question));
    let mut buf = [0u8; 64];
    let n = read_fd(0, &mut buf);
    n > 0
        && crate::String::from_utf8_lossy(&buf[..n as usize])
            .trim()
            .eq_ignore_ascii_case("evet")
}

/// İstemi yazar ve bir satır okur (sondaki satır sonu atılır).
pub fn prompt(question: &str) -> crate::String {
    print(format_args!("{}", question));
    let mut buf = [0u8; 256];
    let n = read_fd(0, &mut buf).max(0) as usize;
    crate::String::from(crate::String::from_utf8_lossy(&buf[..n]).trim())
}
