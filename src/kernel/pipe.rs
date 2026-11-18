//! Tek yönlü bayt kanalları (pipe) ve `poll` için ortak uyandırma kuyruğu.

use alloc::collections::VecDeque;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::sched::{self, WaitQueue};
use crate::sync::SpinLock;

pub const CAPACITY: usize = 64 * 1024;
pub const EINTR: i64 = -4;
pub const EPIPE: i64 = -32;

/// Okunabilir/yazılabilir durumu değişen her kanal bunu uyandırır; `poll` burada bekler.
pub static POLL: WaitQueue = WaitQueue::new();

pub fn notify() {
    POLL.wake_all();
}

/// Bloklu bekleme sinyalle kesilmeli mi?
pub fn interrupted() -> bool {
    sched::pending_signal().is_some()
}

struct Pipe {
    data: SpinLock<VecDeque<u8>>,
    readers: AtomicUsize,
    writers: AtomicUsize,
    wait: WaitQueue,
}

impl Pipe {
    fn changed(&self) {
        self.wait.wake_all();
        notify();
    }
}

pub struct ReadEnd(Arc<Pipe>);
pub struct WriteEnd(Arc<Pipe>);

pub fn new() -> (Arc<ReadEnd>, Arc<WriteEnd>) {
    let pipe = Arc::new(Pipe {
        data: SpinLock::new(VecDeque::new()),
        readers: AtomicUsize::new(1),
        writers: AtomicUsize::new(1),
        wait: WaitQueue::new(),
    });
    (Arc::new(ReadEnd(pipe.clone())), Arc::new(WriteEnd(pipe)))
}

impl ReadEnd {
    /// Veri gelene ya da tüm yazıcılar kapanana (boş = EOF) kadar bekler.
    pub fn read(&self, max: usize) -> Result<Vec<u8>, i64> {
        let pipe = &self.0;
        pipe.wait.wait_until(|| {
            !pipe.data.lock().is_empty()
                || pipe.writers.load(Ordering::Acquire) == 0
                || interrupted()
        });
        let out: Vec<u8> = {
            let mut data = pipe.data.lock();
            let n = data.len().min(max);
            data.drain(..n).collect()
        };
        if out.is_empty() && interrupted() && pipe.writers.load(Ordering::Acquire) > 0 {
            return Err(EINTR);
        }
        pipe.changed();
        Ok(out)
    }

    pub fn readable(&self) -> bool {
        !self.0.data.lock().is_empty() || self.0.writers.load(Ordering::Acquire) == 0
    }
}

impl Drop for ReadEnd {
    fn drop(&mut self) {
        self.0.readers.fetch_sub(1, Ordering::AcqRel);
        self.0.changed();
    }
}

impl WriteEnd {
    /// Tüm baytlar yazılana kadar bekler; okuyucu kalmadıysa EPIPE.
    pub fn write(&self, bytes: &[u8]) -> Result<usize, i64> {
        let pipe = &self.0;
        let mut written = 0;
        while written < bytes.len() {
            pipe.wait.wait_until(|| {
                pipe.data.lock().len() < CAPACITY
                    || pipe.readers.load(Ordering::Acquire) == 0
                    || interrupted()
            });
            if pipe.readers.load(Ordering::Acquire) == 0 {
                return if written > 0 { Ok(written) } else { Err(EPIPE) };
            }
            if interrupted() {
                return if written > 0 { Ok(written) } else { Err(EINTR) };
            }
            {
                let mut data = pipe.data.lock();
                let room = CAPACITY.saturating_sub(data.len());
                let take = room.min(bytes.len() - written);
                data.extend(&bytes[written..written + take]);
                written += take;
            }
            pipe.changed();
        }
        Ok(written)
    }
}

impl Drop for WriteEnd {
    fn drop(&mut self) {
        self.0.writers.fetch_sub(1, Ordering::AcqRel);
        self.0.changed();
    }
}

fn init(detail: &mut crate::kernel::initcall::Detail) -> Result<(), &'static str> {
    use core::fmt::Write;
    let _ = write!(detail, "pipes, pseudo terminals");
    crate::kernel::initcall::self_test("pipe", self_test() && super::pty::self_test())
}

crate::initcall!(late, 5, "Pipes + pty", init);

fn self_test() -> bool {
    let (reader, writer) = new();
    let wrote = writer.write(b"banna") == Ok(5);
    let first = reader.read(3).ok();
    let second = reader.read(16).ok();
    drop(writer);
    let eof = reader.read(16).ok();
    let (reader2, writer2) = new();
    drop(reader2);
    let broken = writer2.write(b"x") == Err(EPIPE);
    wrote
        && first.as_deref() == Some(b"ban".as_slice())
        && second.as_deref() == Some(b"na".as_slice())
        && eof.is_some_and(|e| e.is_empty())
        && broken
}
