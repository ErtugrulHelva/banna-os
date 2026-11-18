//! Sözde terminal (pty) ve satır disiplini denetimi.

use crate::sys;

pub const ECHO: u32 = 1 << 0;
pub const ICANON: u32 = 1 << 1;
pub const ISIG: u32 = 1 << 2;
pub const DEFAULT_MODE: u32 = ECHO | ICANON | ISIG;

/// `(ana uç, yardımcı uç)`.
pub fn openpty() -> Result<(u64, u64), i64> {
    let mut fds = [0u32; 2];
    let r = sys::call(sys::OPENPTY, &[fds.as_mut_ptr() as u64]);
    if r < 0 {
        Err(r)
    } else {
        Ok((fds[0] as u64, fds[1] as u64))
    }
}

/// Ctrl+C'nin gideceği ön plan süreç grubunu ayarlar.
pub fn set_foreground(fd: u64, pgid: u64) -> i64 {
    sys::call(sys::TCSETPGRP, &[fd, pgid])
}

pub fn foreground(fd: u64) -> i64 {
    sys::call(sys::TCGETPGRP, &[fd])
}

/// Kipi değiştirir, eskisini döndürür; tanıtıcı terminal değilse `Err`.
pub fn set_mode(fd: u64, mode: u32) -> Result<u32, i64> {
    let r = sys::call(sys::TTYMODE, &[fd, mode as u64]);
    if r < 0 { Err(r) } else { Ok(r as u32) }
}

pub fn mode(fd: u64) -> Result<u32, i64> {
    set_mode(fd, u32::MAX)
}

pub fn is_tty(fd: u64) -> bool {
    mode(fd).is_ok()
}

/// `(sütun, satır)`; terminal değilse 80x24.
pub fn window_size(fd: u64) -> (usize, usize) {
    let r = sys::call(sys::TTYSIZE, &[fd, u32::MAX as u64]);
    if r <= 0 {
        return (80, 24);
    }
    ((r & 0xffff) as usize, ((r >> 16) & 0xffff) as usize)
}

pub fn set_window_size(fd: u64, columns: usize, rows: usize) {
    sys::call(
        sys::TTYSIZE,
        &[
            fd,
            (columns as u64 & 0xffff) | ((rows as u64 & 0xffff) << 16),
        ],
    );
}
