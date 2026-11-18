//! Kullanıcı işlemleri: adres alanı, dosya tanıtıcıları, çalışma dizini, süreç grubu,
//! sinyaller, mmap bölgesi ve çıkış durumu.

use alloc::string::String;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::sync::atomic::{AtomicI64, AtomicU32, AtomicU64, Ordering};

use super::{Completion, Mutex};
use crate::fs::File;
use crate::kernel::{pipe, pty};
use crate::memory::paging;
use crate::sync::SpinLock;

static NEXT_PID: AtomicU64 = AtomicU64::new(1);

pub const MMAP_BASE: u64 = 0x0000_1000_0000_0000;
pub const MMAP_LIMIT: u64 = 0x0000_2000_0000_0000;
const MAX_FDS: usize = 64;
const TABLE_LIMIT: usize = 128;

pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGPIPE: u32 = 13;
pub const MAX_SIGNAL: u32 = 31;

#[derive(Clone)]
pub enum Descriptor {
    Console,
    File(Arc<Mutex<File>>),
    Socket(Arc<crate::net::TcpHandle>),
    PipeRead(Arc<pipe::ReadEnd>),
    PipeWrite(Arc<pipe::WriteEnd>),
    PtyMaster(Arc<pty::Master>),
    PtySlave(Arc<pty::Slave>),
}

struct Record {
    pid: u64,
    name: String,
    process: Weak<Process>,
    completion: Option<Arc<Completion>>,
}

static TABLE: SpinLock<Vec<Record>> = SpinLock::new(Vec::new());

pub struct Process {
    pub pid: u64,
    pub ppid: u64,
    /// Kimlik: başlatan işlemden, yoksa kabuk oturumundan devralınır.
    uid: AtomicU32,
    name: SpinLock<String>,
    root: AtomicU64,
    pgid: AtomicU64,
    /// Bekleyen sonlandırıcı sinyal (0 = yok).
    signal: AtomicU32,
    /// Yok sayılan sinyallerin bit maskesi (SIGKILL yok sayılamaz).
    ignored: AtomicU32,
    exit_code: AtomicI64,
    fds: SpinLock<Vec<Option<Descriptor>>>,
    cwd: SpinLock<String>,
    mmap_next: AtomicU64,
}

impl Process {
    /// Yeni adres alanlı işlem. Kimlik, dizin, grup ve 0-2 tanıtıcıları çağıran
    /// işlemden devralınır; çekirdekten başlatılınca konsola bağlanır.
    pub fn new(name: &str) -> Option<Arc<Self>> {
        let root = paging::create_address_space()?;
        Some(Self::with_root(name, root))
    }

    pub fn with_root(name: &str, root: u64) -> Arc<Self> {
        let parent = super::current_process();
        let pid = NEXT_PID.fetch_add(1, Ordering::Relaxed);
        let fds = match &parent {
            Some(p) => (0..3).map(|fd| p.fd(fd)).collect(),
            None => alloc::vec![
                Some(Descriptor::Console),
                Some(Descriptor::Console),
                Some(Descriptor::Console),
            ],
        };
        let process = Arc::new(Self {
            pid,
            ppid: parent.as_ref().map_or(0, |p| p.pid),
            uid: AtomicU32::new(
                parent
                    .as_ref()
                    .map(|p| p.uid())
                    .unwrap_or_else(crate::kernel::users::effective_uid),
            ),
            name: SpinLock::new(String::from(name)),
            root: AtomicU64::new(root),
            pgid: AtomicU64::new(parent.as_ref().map_or(pid, |p| p.pgid())),
            signal: AtomicU32::new(0),
            ignored: AtomicU32::new(parent.as_ref().map_or(0, |p| p.ignored())),
            exit_code: AtomicI64::new(0),
            fds: SpinLock::new(fds),
            cwd: SpinLock::new(
                parent
                    .as_ref()
                    .map_or_else(|| String::from("/"), |p| p.cwd()),
            ),
            mmap_next: AtomicU64::new(MMAP_BASE),
        });
        register(&process);
        process
    }

    /// `fork`: adres alanı dışında her şey (tanıtıcılar, dizin, grup) kopyalanır.
    pub fn fork(&self, root: u64) -> Arc<Self> {
        let process = Arc::new(Self {
            pid: NEXT_PID.fetch_add(1, Ordering::Relaxed),
            ppid: self.pid,
            uid: AtomicU32::new(self.uid()),
            name: SpinLock::new(self.name()),
            root: AtomicU64::new(root),
            pgid: AtomicU64::new(self.pgid()),
            signal: AtomicU32::new(0),
            ignored: AtomicU32::new(self.ignored()),
            exit_code: AtomicI64::new(0),
            fds: SpinLock::new(self.fds.lock().clone()),
            cwd: SpinLock::new(self.cwd()),
            mmap_next: AtomicU64::new(self.mmap_next.load(Ordering::Relaxed)),
        });
        register(&process);
        process
    }

    pub fn uid(&self) -> u32 {
        self.uid.load(Ordering::Acquire)
    }

    /// Yalnızca `elevate` syscall'ı (parola doğrulandıktan sonra) kullanır.
    pub fn set_uid(&self, uid: u32) {
        self.uid.store(uid, Ordering::Release);
    }

    pub fn root(&self) -> u64 {
        self.root.load(Ordering::Acquire)
    }

    pub fn name(&self) -> String {
        self.name.lock().clone()
    }

    /// `exec`: yeni adres alanına geçer, eskisini döndürür (çağıran serbest bırakır).
    pub fn replace_image(&self, root: u64, name: &str) -> u64 {
        *self.name.lock() = String::from(name);
        if let Some(record) = TABLE.lock().iter_mut().find(|r| r.pid == self.pid) {
            record.name = String::from(name);
        }
        self.mmap_next.store(MMAP_BASE, Ordering::Relaxed);
        self.root.swap(root, Ordering::AcqRel)
    }

    pub fn pgid(&self) -> u64 {
        self.pgid.load(Ordering::Relaxed)
    }

    pub fn set_pgid(&self, pgid: u64) {
        self.pgid.store(pgid, Ordering::Relaxed);
    }

    pub fn cwd(&self) -> String {
        self.cwd.lock().clone()
    }

    pub fn set_cwd(&self, path: String) {
        *self.cwd.lock() = path;
    }

    pub fn ignored(&self) -> u32 {
        self.ignored.load(Ordering::Relaxed)
    }

    pub fn set_ignored(&self, mask: u32) -> u32 {
        self.ignored.swap(mask & !(1 << SIGKILL), Ordering::Relaxed)
    }

    /// Sinyali kaydeder; yok sayılıyorsa `false`.
    pub fn signal(&self, signal: u32) -> bool {
        if signal == 0 || signal > MAX_SIGNAL || self.ignored() & (1 << signal) != 0 {
            return false;
        }
        let _ = self
            .signal
            .compare_exchange(0, signal, Ordering::AcqRel, Ordering::Acquire);
        true
    }

    pub fn is_killed(&self) -> bool {
        self.pending_signal() != 0
    }

    pub fn pending_signal(&self) -> u32 {
        self.signal.load(Ordering::Acquire)
    }

    pub fn set_exit_code(&self, code: i64) {
        self.exit_code.store(code, Ordering::Release);
    }

    pub fn add_fd(&self, descriptor: Descriptor) -> Option<usize> {
        let mut fds = self.fds.lock();
        if let Some(slot) = fds.iter().position(|d| d.is_none()) {
            fds[slot] = Some(descriptor);
            return Some(slot);
        }
        if fds.len() >= MAX_FDS {
            return None;
        }
        fds.push(Some(descriptor));
        Some(fds.len() - 1)
    }

    /// `fd` yuvasına yerleştirir; yuvadaki eski tanıtıcı kapanır.
    pub fn set_fd(&self, fd: u64, descriptor: Descriptor) -> bool {
        let fd = fd as usize;
        if fd >= MAX_FDS {
            return false;
        }
        let old = {
            let mut fds = self.fds.lock();
            if fds.len() <= fd {
                fds.resize(fd + 1, None);
            }
            fds[fd].replace(descriptor)
        };
        drop(old);
        true
    }

    pub fn fd(&self, fd: u64) -> Option<Descriptor> {
        self.fds.lock().get(fd as usize).cloned().flatten()
    }

    pub fn close_fd(&self, fd: u64) -> bool {
        let old = self
            .fds
            .lock()
            .get_mut(fd as usize)
            .and_then(|slot| slot.take());
        old.is_some()
    }

    /// Çıkışta tüm tanıtıcıları kapatır (pipe okuyucuları hemen EOF görür).
    pub fn close_all(&self) {
        let fds = core::mem::take(&mut *self.fds.lock());
        drop(fds);
    }

    /// `len` baytlık anonim bölge için sanal adres ayırır.
    pub fn reserve_mmap(&self, len: u64) -> Option<u64> {
        let base = self.mmap_next.fetch_add(len, Ordering::Relaxed);
        (base + len <= MMAP_LIMIT).then_some(base)
    }
}

impl Drop for Process {
    fn drop(&mut self) {
        crate::log!(
            "process: pid {} '{}' released",
            self.pid,
            self.name.lock().as_str()
        );
        paging::destroy_address_space(self.root());
    }
}

fn register(process: &Arc<Process>) {
    let mut table = TABLE.lock();
    if table.len() >= TABLE_LIMIT {
        table.retain(|r| {
            r.process.strong_count() > 0 || !r.completion.as_ref().is_some_and(|c| c.is_done())
        });
    }
    table.push(Record {
        pid: process.pid,
        name: process.name(),
        process: Arc::downgrade(process),
        completion: None,
    });
}

pub fn set_completion(pid: u64, completion: Arc<Completion>) {
    if let Some(record) = TABLE.lock().iter_mut().find(|r| r.pid == pid) {
        record.completion = Some(completion);
    }
}

pub fn completion_of(pid: u64) -> Option<Arc<Completion>> {
    TABLE
        .lock()
        .iter()
        .find(|r| r.pid == pid)?
        .completion
        .clone()
}

pub fn find(pid: u64) -> Option<Arc<Process>> {
    TABLE
        .lock()
        .iter()
        .find(|r| r.pid == pid)?
        .process
        .upgrade()
}

/// Gruptaki canlı işlemler.
pub fn group(pgid: u64) -> Vec<Arc<Process>> {
    TABLE
        .lock()
        .iter()
        .filter_map(|r| r.process.upgrade())
        .filter(|p| p.pgid() == pgid)
        .collect()
}

pub struct Info {
    pub pid: u64,
    pub name: String,
    pub running: bool,
}

pub fn list() -> Vec<Info> {
    TABLE
        .lock()
        .iter()
        .map(|r| Info {
            pid: r.pid,
            name: r.name.clone(),
            running: r.process.strong_count() > 0
                && !r.completion.as_ref().is_some_and(|c| c.is_done()),
        })
        .collect()
}

pub fn is_running(pid: u64) -> bool {
    TABLE.lock().iter().any(|r| {
        r.pid == pid
            && r.process.strong_count() > 0
            && !r.completion.as_ref().is_some_and(|c| c.is_done())
    })
}

pub fn live_count() -> usize {
    TABLE
        .lock()
        .iter()
        .filter(|r| r.process.strong_count() > 0)
        .count()
}
