//! Sözde terminal (pty): terminal uygulaması ana uçtan klavye girdisi yazar ve
//! ekran çıktısını okur; kabuk ve programlar yardımcı ucu stdin/stdout olarak kullanır.
//!
//! Satır disiplini (ICANON) satırı düzenler ve Enter'da teslim eder, ECHO girdiyi
//! ekrana yansıtır, ISIG Ctrl+C / Ctrl+\ ile ön plandaki süreç grubuna sinyal yollar.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};

use super::pipe::{EINTR, interrupted, notify};
use crate::sched::process::{SIGINT, SIGQUIT};
use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;

pub const ECHO: u32 = 1 << 0;
pub const ICANON: u32 = 1 << 1;
pub const ISIG: u32 = 1 << 2;
pub const DEFAULT_MODE: u32 = ECHO | ICANON | ISIG;

const OUTPUT_LIMIT: usize = 64 * 1024;
const LINE_LIMIT: usize = 4096;
pub const EIO: i64 = -5;

#[derive(Default)]
struct Input {
    ready: VecDeque<u8>,
    line: Vec<u8>,
    /// Boş satırda Ctrl+D: bir sonraki okuma 0 döner.
    eof: bool,
}

struct Pty {
    input: SpinLock<Input>,
    output: SpinLock<VecDeque<u8>>,
    masters: AtomicUsize,
    slaves: AtomicUsize,
    foreground: AtomicU64,
    mode: AtomicU32,
    /// Sütun | satır << 16 (terminal penceresi ayarlar).
    size: AtomicU32,
    input_wait: WaitQueue,
    output_wait: WaitQueue,
}

pub struct Master(Arc<Pty>);
pub struct Slave(Arc<Pty>);

pub fn open() -> (Arc<Master>, Arc<Slave>) {
    let pty = Arc::new(Pty {
        input: SpinLock::new(Input::default()),
        output: SpinLock::new(VecDeque::new()),
        masters: AtomicUsize::new(1),
        slaves: AtomicUsize::new(1),
        foreground: AtomicU64::new(0),
        mode: AtomicU32::new(DEFAULT_MODE),
        size: AtomicU32::new(80 | (24 << 16)),
        input_wait: WaitQueue::new(),
        output_wait: WaitQueue::new(),
    });
    (Arc::new(Master(pty.clone())), Arc::new(Slave(pty)))
}

impl Pty {
    fn emit(&self, bytes: &[u8]) {
        self.output.lock().extend(bytes);
        self.output_wait.wake_all();
        notify();
    }

    fn deliver_input(&self) {
        self.input_wait.wake_all();
        notify();
    }

    fn mode(&self) -> u32 {
        self.mode.load(Ordering::Relaxed)
    }

    fn signal_foreground(&self, signal: u32) {
        let pgid = self.foreground.load(Ordering::Relaxed);
        if pgid != 0 {
            sched::signal_group(pgid, signal);
        }
    }

    /// Klavyeden gelen baytları satır disiplininden geçirir.
    fn receive(&self, bytes: &[u8]) {
        let mut echo: Vec<u8> = Vec::new();
        let mut delivered = false;
        let mut signals = Vec::new();
        {
            // Kip girdi kilidi altında okunur: `set_mode` ile yarışan bayt satırda kalmaz.
            let mut input = self.input.lock();
            let mode = self.mode();
            for &byte in bytes {
                if mode & ISIG != 0 && (byte == 0x03 || byte == 0x1c) {
                    signals.push(if byte == 0x03 { SIGINT } else { SIGQUIT });
                    input.line.clear();
                    if mode & ECHO != 0 {
                        echo.extend_from_slice(if byte == 0x03 { b"^C\n" } else { b"^\\\n" });
                    }
                    continue;
                }
                if mode & ICANON == 0 {
                    input.ready.push_back(byte);
                    delivered = true;
                    if mode & ECHO != 0 {
                        echo.push(byte);
                    }
                    continue;
                }
                match byte {
                    b'\r' | b'\n' => {
                        input.line.push(b'\n');
                        let line = core::mem::take(&mut input.line);
                        input.ready.extend(line);
                        delivered = true;
                        if mode & ECHO != 0 {
                            echo.push(b'\n');
                        }
                    }
                    0x7f | 0x08 => {
                        // UTF-8 karakterin devam baytlarıyla birlikte silinir.
                        while let Some(last) = input.line.pop() {
                            if last & 0xc0 != 0x80 {
                                break;
                            }
                        }
                        if mode & ECHO != 0 {
                            echo.extend_from_slice(b"\x08 \x08");
                        }
                    }
                    0x15 => {
                        let chars = core::str::from_utf8(&input.line)
                            .map(|s| s.chars().count())
                            .unwrap_or(input.line.len());
                        input.line.clear();
                        if mode & ECHO != 0 {
                            for _ in 0..chars {
                                echo.extend_from_slice(b"\x08 \x08");
                            }
                        }
                    }
                    0x04 => {
                        if input.line.is_empty() {
                            input.eof = true;
                        } else {
                            let line = core::mem::take(&mut input.line);
                            input.ready.extend(line);
                        }
                        delivered = true;
                    }
                    byte if input.line.len() < LINE_LIMIT => {
                        input.line.push(byte);
                        if mode & ECHO != 0 {
                            echo.push(byte);
                        }
                    }
                    _ => {}
                }
            }
        }
        if !echo.is_empty() {
            self.emit(&echo);
        }
        for signal in signals {
            self.signal_foreground(signal);
        }
        if delivered {
            self.deliver_input();
        }
    }
}

impl Master {
    /// Programların çıktısını okur; yardımcı uç kalmadıysa boş döner.
    pub fn read(&self, max: usize) -> Result<Vec<u8>, i64> {
        let pty = &self.0;
        pty.output_wait.wait_until(|| {
            !pty.output.lock().is_empty()
                || pty.slaves.load(Ordering::Acquire) == 0
                || interrupted()
        });
        let out: Vec<u8> = {
            let mut output = pty.output.lock();
            let n = output.len().min(max);
            output.drain(..n).collect()
        };
        if out.is_empty() && interrupted() && pty.slaves.load(Ordering::Acquire) > 0 {
            return Err(EINTR);
        }
        pty.output_wait.wake_all();
        Ok(out)
    }

    pub fn write(&self, bytes: &[u8]) -> usize {
        self.0.receive(bytes);
        bytes.len()
    }

    pub fn readable(&self) -> bool {
        !self.0.output.lock().is_empty() || self.0.slaves.load(Ordering::Acquire) == 0
    }

    pub fn set_foreground(&self, pgid: u64) {
        self.0.foreground.store(pgid, Ordering::Relaxed);
    }

    pub fn foreground(&self) -> u64 {
        self.0.foreground.load(Ordering::Relaxed)
    }

    /// `mode` `u32::MAX` ise yalnızca okur; eski kipi döndürür.
    pub fn set_mode(&self, mode: u32) -> u32 {
        set_mode(&self.0, mode)
    }

    pub fn window_size(&self, size: u32) -> u32 {
        window_size(&self.0, size)
    }
}

impl Drop for Master {
    fn drop(&mut self) {
        self.0.masters.fetch_sub(1, Ordering::AcqRel);
        self.0.input_wait.wake_all();
        self.0.output_wait.wake_all();
        notify();
    }
}

/// `size` `u32::MAX` ise yalnızca okur; geçerli boyutu döndürür.
fn window_size(pty: &Pty, size: u32) -> u32 {
    if size != u32::MAX {
        pty.size.store(size, Ordering::Relaxed);
    }
    pty.size.load(Ordering::Relaxed)
}

fn set_mode(pty: &Pty, mode: u32) -> u32 {
    if mode == u32::MAX {
        return pty.mode();
    }
    let old = {
        let mut input = pty.input.lock();
        let old = pty.mode.swap(mode & DEFAULT_MODE, Ordering::Relaxed);
        if mode & ICANON == 0 {
            let line = core::mem::take(&mut input.line);
            input.ready.extend(line);
        }
        old
    };
    pty.deliver_input();
    old
}

impl Slave {
    /// Kanonik kipte en fazla bir satır, ham kipte eldeki baytlar; ana uç kapandıysa 0.
    pub fn read(&self, max: usize) -> Result<Vec<u8>, i64> {
        let pty = &self.0;
        pty.input_wait.wait_until(|| {
            let input = pty.input.lock();
            !input.ready.is_empty()
                || input.eof
                || pty.masters.load(Ordering::Acquire) == 0
                || interrupted()
        });
        let mut input = pty.input.lock();
        if input.ready.is_empty() {
            if input.eof {
                input.eof = false;
                return Ok(Vec::new());
            }
            if pty.masters.load(Ordering::Acquire) == 0 {
                return Ok(Vec::new());
            }
            return Err(EINTR);
        }
        let mut n = input.ready.len().min(max);
        if pty.mode() & ICANON != 0
            && let Some(newline) = input.ready.iter().position(|&b| b == b'\n')
        {
            n = n.min(newline + 1);
        }
        Ok(input.ready.drain(..n).collect())
    }

    /// Çıktıyı ana uca iletir (ve seri loga yansıtır); terminal okuyana kadar bekleyebilir.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, i64> {
        let pty = &self.0;
        pty.output_wait.wait_until(|| {
            pty.output.lock().len() < OUTPUT_LIMIT
                || pty.masters.load(Ordering::Acquire) == 0
                || interrupted()
        });
        if pty.masters.load(Ordering::Acquire) == 0 {
            return Err(EIO);
        }
        if pty.output.lock().len() >= OUTPUT_LIMIT {
            return Err(EINTR);
        }
        crate::log::serial(format_args!(
            "{}",
            alloc::string::String::from_utf8_lossy(bytes)
        ));
        pty.emit(bytes);
        Ok(bytes.len())
    }

    pub fn readable(&self) -> bool {
        let input = self.0.input.lock();
        !input.ready.is_empty() || input.eof || self.0.masters.load(Ordering::Acquire) == 0
    }

    pub fn set_foreground(&self, pgid: u64) {
        self.0.foreground.store(pgid, Ordering::Relaxed);
    }

    pub fn foreground(&self) -> u64 {
        self.0.foreground.load(Ordering::Relaxed)
    }

    pub fn set_mode(&self, mode: u32) -> u32 {
        set_mode(&self.0, mode)
    }

    pub fn window_size(&self, size: u32) -> u32 {
        window_size(&self.0, size)
    }
}

impl Drop for Slave {
    fn drop(&mut self) {
        self.0.slaves.fetch_sub(1, Ordering::AcqRel);
        self.0.output_wait.wake_all();
        notify();
    }
}

pub fn self_test() -> bool {
    let (master, slave) = open();
    master.write(b"ab\x7fc\n");
    let line = slave.read(64).ok();
    let echoed = master.read(64).ok();
    slave.set_mode(0);
    master.write(b"xy");
    let raw = slave.read(64).ok();
    let quiet = !master.readable();
    drop(master);
    let hangup = slave.read(64).ok();
    line.as_deref() == Some(b"ac\n".as_slice())
        && echoed.as_deref() == Some(b"ab\x08 \x08c\n".as_slice())
        && raw.as_deref() == Some(b"xy".as_slice())
        && quiet
        && hangup.is_some_and(|h| h.is_empty())
}
