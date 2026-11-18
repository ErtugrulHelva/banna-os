//! Banna OS syscall ABI'si.
//!
//! `rax` = numara, argümanlar `rdi, rsi, rdx, r10, r8, r9`; dönüş `rax`.
//! Negatif dönüşler hata kodudur (POSIX errno'larının eksi değeri).

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::arch::asm;

use crate::arch::fpu::FpuState;
use crate::arch::syscall::SyscallFrame;
use crate::arch::{cpu, pit, tsc};
use crate::drivers::{keyboard, rtc};
use crate::fs::{self, File, OpenFlags};
use crate::kernel::{pipe, pty};
use crate::memory::paging::{self, NO_EXECUTE, USER, USER_SPACE_END, WRITABLE};
use crate::memory::{PAGE_SIZE, align_up, pmm};
use crate::net;
use crate::sched::process::Descriptor;
use crate::sched::{self, Mutex};

pub const SYS_EXIT: u64 = 0;
pub const SYS_WRITE: u64 = 1;
pub const SYS_READ: u64 = 2;
pub const SYS_UPTIME_MS: u64 = 3;
pub const SYS_SLEEP_MS: u64 = 4;
pub const SYS_TIME: u64 = 5;
pub const SYS_GETPID: u64 = 6;
pub const SYS_YIELD: u64 = 7;
pub const SYS_OPEN: u64 = 8;
pub const SYS_CLOSE: u64 = 9;
pub const SYS_SEEK: u64 = 10;
pub const SYS_STAT: u64 = 11;
pub const SYS_READDIR: u64 = 12;
pub const SYS_MKDIR: u64 = 13;
pub const SYS_UNLINK: u64 = 14;
pub const SYS_SPAWN: u64 = 15;
pub const SYS_WAIT: u64 = 16;
pub const SYS_MMAP: u64 = 17;
pub const SYS_MUNMAP: u64 = 18;
pub const SYS_SYSINFO: u64 = 19;
pub const SYS_WIN_CREATE: u64 = 20;
pub const SYS_WIN_PRESENT: u64 = 21;
pub const SYS_WIN_EVENT: u64 = 22;
pub const SYS_WIN_CLOSE: u64 = 23;
pub const SYS_KILL: u64 = 24;
pub const SYS_TIME_NS: u64 = 25;
pub const SYS_CONNECT: u64 = 26;
pub const SYS_LISTEN: u64 = 27;
pub const SYS_ACCEPT: u64 = 28;
pub const SYS_RESOLVE: u64 = 29;
pub const SYS_RECV: u64 = 30;
pub const SYS_GETUID: u64 = 31;
pub const SYS_USERNAME: u64 = 32;
pub const SYS_FORK: u64 = 33;
pub const SYS_EXEC: u64 = 34;
pub const SYS_PIPE: u64 = 35;
pub const SYS_DUP2: u64 = 36;
pub const SYS_CHDIR: u64 = 37;
pub const SYS_GETCWD: u64 = 38;
pub const SYS_SETPGID: u64 = 39;
pub const SYS_GETPGID: u64 = 40;
pub const SYS_SIGIGNORE: u64 = 41;
pub const SYS_OPENPTY: u64 = 42;
pub const SYS_TCSETPGRP: u64 = 43;
pub const SYS_TTYMODE: u64 = 44;
pub const SYS_POLL: u64 = 45;
pub const SYS_LOG: u64 = 46;
pub const SYS_KCMD: u64 = 47;
pub const SYS_GETPPID: u64 = 48;
pub const SYS_TCGETPGRP: u64 = 49;
pub const SYS_ELEVATE: u64 = 50;
pub const SYS_NETINFO: u64 = 51;
pub const SYS_TTYSIZE: u64 = 52;
pub const SYS_RENAME: u64 = 53;
pub const SYS_CHMOD: u64 = 54;
pub const SYS_CHOWN: u64 = 55;
pub const SYS_MOUNT: u64 = 56;
pub const SYS_UMOUNT: u64 = 57;
pub const SYS_RESCAN: u64 = 58;
pub const SYS_STATFS: u64 = 59;
pub const SYS_INFO: u64 = 60;
pub const SYS_DESKTOP_SET: u64 = 61;
pub const SYS_RANDOM: u64 = 62;
pub const SYS_CLIPBOARD: u64 = 63;
pub const SYS_ACCOUNT: u64 = 64;
pub const SYS_WIN_PRESENT_RECT: u64 = 65;

pub const ENOENT: i64 = -2;
pub const EBADF: i64 = -9;
pub const ENOMEM: i64 = -12;
pub const EFAULT: i64 = -14;
pub const EINVAL: i64 = -22;
pub const ENOSYS: i64 = -38;
pub const ESRCH: i64 = -3;
pub const EINTR: i64 = -4;
pub const EACCES: i64 = -13;
pub const ENODEV: i64 = -19;
pub const EAGAIN: i64 = -11;
pub const ENOTDIR: i64 = -20;
pub const EMFILE: i64 = -24;
pub const ENOTTY: i64 = -25;
pub const EPERM: i64 = -1;
pub const ECHILD: i64 = -10;
pub const EADDRINUSE: i64 = -98;
pub const ENETUNREACH: i64 = -101;
pub const ECONNRESET: i64 = -104;
pub const ENOTCONN: i64 = -107;
pub const ETIMEDOUT: i64 = -110;
pub const ECONNREFUSED: i64 = -111;

const MAX_IO: usize = 64 * 1024;
const MAX_PATH: usize = 1024;
const MAX_ARGS: usize = 64;
const MAX_MMAP: u64 = 256 * 1024 * 1024;

pub extern "C" fn dispatch(frame: &mut SyscallFrame) -> i64 {
    if frame.user_rip >= USER_SPACE_END {
        sched::exit_current(-11);
    }
    cpu::enable_interrupts();
    let (a0, a1, a2, a3) = (frame.arg0, frame.arg1, frame.arg2, frame.arg3);
    let result = match frame.number {
        SYS_EXIT => sched::exit_current(a0 as i64),
        SYS_WRITE => sys_write(a0, a1, a2 as usize),
        SYS_READ => sys_read(a0, a1, a2 as usize),
        SYS_UPTIME_MS => pit::uptime_ms() as i64,
        SYS_SLEEP_MS => {
            sched::sleep_ms(a0.min(60_000));
            0
        }
        SYS_TIME => rtc::now().map(|t| t.unix_timestamp()).unwrap_or(EINVAL),
        SYS_TIME_NS => tsc::now_ns() as i64,
        SYS_GETPID => sched::current_process().map(|p| p.pid as i64).unwrap_or(0),
        SYS_YIELD => {
            sched::yield_now();
            0
        }
        SYS_OPEN => sys_open(a0, a1, a2),
        SYS_CLOSE => current(|p| if p.close_fd(a0) { 0 } else { EBADF }),
        SYS_SEEK => with_file(a0, |f| f.seek(a1) as i64),
        SYS_STAT => sys_stat(a0, a1, a2),
        SYS_READDIR => sys_readdir(a0, a1, a2),
        SYS_MKDIR => path_arg(a0, a1).map_or(EFAULT, |p| {
            if !fs::may_modify_entry(caller_uid(), &p, false) {
                return EACCES;
            }
            result(fs::mkdir_as(&p, caller_owner(), 0o755))
        }),
        SYS_UNLINK => path_arg(a0, a1).map_or(EFAULT, |p| {
            if !fs::may_modify_entry(caller_uid(), &p, true) {
                return EACCES;
            }
            result(fs::remove(&p))
        }),
        SYS_RENAME => sys_rename(a0, a1, a2, a3),
        SYS_CHMOD => sys_chmod(a0, a1, a2),
        SYS_CHOWN => sys_chown(a0, a1, a2, a3),
        SYS_MOUNT => sys_mount(a0, a1, a2, a3),
        SYS_UMOUNT => sys_umount(a0, a1),
        SYS_RESCAN => sys_rescan(a0, a1),
        SYS_STATFS => sys_statfs(a0, a1, a2),
        SYS_INFO => sys_info(a0, a1, a2),
        SYS_DESKTOP_SET => sys_desktop_set(a0, a1, a2, a3),
        SYS_RANDOM => sys_random(a0, a1),
        SYS_CLIPBOARD => sys_clipboard(a0, a1, a2),
        SYS_ACCOUNT => sys_account(a0, a1, a2, a3),
        SYS_GETUID => caller_uid() as i64,
        SYS_USERNAME => sys_username(a0, a1),
        SYS_SPAWN => sys_spawn(a0, a1, a2, a3),
        SYS_WAIT => sys_wait(a0, a1),
        SYS_KILL => sys_kill(a0 as i64, a1),
        SYS_FORK => sys_fork(frame),
        SYS_EXEC => match sys_exec(a0, a1, a2, a3) {
            Ok(start) => unsafe {
                crate::arch::syscall::enter_user_mode(start.0, start.1, start.2, start.3)
            },
            Err(e) => e,
        },
        SYS_PIPE => sys_pipe(a0),
        SYS_DUP2 => sys_dup2(a0, a1),
        SYS_CHDIR => sys_chdir(a0, a1),
        SYS_GETCWD => current(|p| {
            let cwd = p.cwd();
            let n = cwd.len();
            if n > a1 as usize {
                return EINVAL;
            }
            copy_to_user(a0, cwd.as_bytes()).map_or(EFAULT, |_| n as i64)
        }),
        SYS_SETPGID => sys_setpgid(a0, a1),
        SYS_GETPGID => sys_getpgid(a0),
        SYS_GETPPID => current(|p| p.ppid as i64),
        SYS_SIGIGNORE => current(|p| p.set_ignored(a0 as u32) as i64),
        SYS_OPENPTY => sys_openpty(a0),
        SYS_TCSETPGRP => sys_tty(a0, |t| {
            t.set_foreground(a1);
            0
        }),
        SYS_TCGETPGRP => sys_tty(a0, |t| t.foreground() as i64),
        SYS_TTYMODE => sys_tty(a0, |t| t.set_mode(a1 as u32) as i64),
        SYS_TTYSIZE => sys_tty(a0, |t| t.window_size(a1 as u32) as i64),
        SYS_ELEVATE => sys_elevate(a0, a1),
        SYS_NETINFO => sys_netinfo(a0),
        SYS_POLL => sys_poll(a0, a1, a2),
        SYS_LOG => sys_log(a0, a1),
        SYS_KCMD => sys_kcmd(a0, a1),
        SYS_MMAP => sys_mmap(a0),
        SYS_MUNMAP => sys_munmap(a0, a1),
        SYS_SYSINFO => sys_sysinfo(a0),
        SYS_WIN_CREATE => sys_win_create(a0, a1, a2, a3),
        SYS_WIN_PRESENT => sys_win_present(a0, a1, a2, a3),
        SYS_WIN_PRESENT_RECT => sys_win_present_rect(a0, a1, a2, a3),
        SYS_WIN_EVENT => sys_win_event(a0, a1, a2),
        SYS_WIN_CLOSE => current(|p| crate::gui::app::close(p.pid, a0)),
        SYS_CONNECT => sys_connect(a0, a1, a2),
        SYS_LISTEN => sys_listen(a0),
        SYS_ACCEPT => sys_accept(a0, a1),
        SYS_RESOLVE => sys_resolve(a0, a1),
        SYS_RECV => sys_recv(a0, a1, a2 as usize, a3),
        _ => ENOSYS,
    };
    if let Some(signal) = sched::pending_signal() {
        sched::exit_current(-(signal as i64));
    }
    cpu::disable_interrupts();
    result
}

fn result(r: fs::Result<()>) -> i64 {
    match r {
        Ok(()) => 0,
        Err(e) => e.errno(),
    }
}

fn current(f: impl FnOnce(&Arc<sched::Process>) -> i64) -> i64 {
    match sched::current_process() {
        Some(p) => f(&p),
        None => EBADF,
    }
}

fn with_file(fd: u64, f: impl FnOnce(&mut File) -> i64) -> i64 {
    current(|p| match p.fd(fd) {
        Some(Descriptor::File(file)) => f(&mut file.lock()),
        Some(_) => EINVAL,
        None => EBADF,
    })
}

fn path_arg(ptr: u64, len: u64) -> Option<String> {
    let len = len as usize;
    if len == 0 || len > MAX_PATH {
        return None;
    }
    let mut bytes = vec![0u8; len];
    copy_from_user(&mut bytes, ptr).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    let cwd = sched::current_process().map_or_else(|| String::from("/"), |p| p.cwd());
    Some(fs::normalize(&cwd, &text))
}

fn sys_write(fd: u64, buf: u64, len: usize) -> i64 {
    let len = len.min(MAX_IO);
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, buf).is_err() {
        return EFAULT;
    }
    match sched::current_process().and_then(|p| p.fd(fd)) {
        Some(Descriptor::Console) | None if fd == 1 || fd == 2 => {
            crate::print!("{}", String::from_utf8_lossy(&bytes));
            len as i64
        }
        Some(Descriptor::File(file)) => match file.lock().write(&bytes) {
            Ok(n) => n as i64,
            Err(e) => e.errno(),
        },
        Some(Descriptor::Socket(socket)) => match net::tcp_send(socket.0, &bytes) {
            Ok(n) => n as i64,
            Err(e) => tcp_errno(e),
        },
        Some(Descriptor::PipeWrite(pipe)) => match pipe.write(&bytes) {
            Ok(n) => n as i64,
            Err(e) => {
                if e == pipe::EPIPE
                    && let Some(p) = sched::current_process()
                {
                    p.signal(sched::process::SIGPIPE);
                }
                e
            }
        },
        Some(Descriptor::PtySlave(tty)) => tty.write(&bytes).map_or_else(|e| e, |n| n as i64),
        Some(Descriptor::PtyMaster(tty)) => tty.write(&bytes) as i64,
        _ => EBADF,
    }
}

fn tcp_errno(e: net::tcp::Error) -> i64 {
    use net::tcp::Error;
    match e {
        Error::Refused => ECONNREFUSED,
        Error::Reset => ECONNRESET,
        Error::TimedOut => ETIMEDOUT,
        Error::Closed => ENOTCONN,
        Error::NoSocket => EBADF,
        Error::AddressInUse => EADDRINUSE,
        Error::WouldBlock => EAGAIN,
    }
}

fn socket_of(fd: u64) -> Option<Arc<net::TcpHandle>> {
    match sched::current_process()?.fd(fd)? {
        Descriptor::Socket(socket) => Some(socket),
        _ => None,
    }
}

fn install_socket(id: net::tcp::SocketId) -> i64 {
    let handle = Arc::new(net::TcpHandle(id));
    current(|p| match p.add_fd(Descriptor::Socket(handle.clone())) {
        Some(fd) => fd as i64,
        None => -24,
    })
}

/// `ip` ağ bayt sırasıyla paketlenmiş IPv4 adresidir (a.b.c.d → a<<24|...).
fn sys_connect(ip: u64, port: u64, timeout_ms: u64) -> i64 {
    if !net::present() {
        return ENETUNREACH;
    }
    let address = net::wire::Ipv4((ip as u32).to_be_bytes());
    let timeout = if timeout_ms == 0 {
        10_000
    } else {
        timeout_ms.min(120_000)
    };
    match net::tcp_connect(address, port as u16, timeout) {
        Ok(id) => install_socket(id),
        Err(e) => tcp_errno(e),
    }
}

fn sys_listen(port: u64) -> i64 {
    if !net::present() {
        return ENETUNREACH;
    }
    match net::tcp_listen(port as u16) {
        Ok(id) => install_socket(id),
        Err(e) => tcp_errno(e),
    }
}

/// `timeout_ms` 0 ise bağlantı gelene kadar bekler.
fn sys_accept(fd: u64, timeout_ms: u64) -> i64 {
    let Some(listener) = socket_of(fd) else {
        return EBADF;
    };
    let deadline = pit::uptime_ms().saturating_add(if timeout_ms == 0 {
        u64::MAX / 2
    } else {
        timeout_ms
    });
    loop {
        match net::tcp_accept(listener.0, 500) {
            Ok(id) => return install_socket(id),
            Err(net::tcp::Error::WouldBlock) => {
                if killed() {
                    return EINTR;
                }
                if pit::uptime_ms() >= deadline {
                    return EAGAIN;
                }
            }
            Err(e) => return tcp_errno(e),
        }
    }
}

fn sys_resolve(name: u64, len: u64) -> i64 {
    let len = len as usize;
    if len == 0 || len > 255 {
        return EINVAL;
    }
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, name).is_err() {
        return EFAULT;
    }
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return EINVAL;
    };
    if let Some(ip) = net::wire::Ipv4::parse(text) {
        return u32::from_be_bytes(ip.0) as i64;
    }
    match net::dns::resolve(text) {
        Ok(list) => list
            .first()
            .map_or(ENOENT, |ip| u32::from_be_bytes(ip.0) as i64),
        Err(_) => ENOENT,
    }
}

fn killed() -> bool {
    sched::current_process().is_some_and(|p| p.is_killed())
}

/// Soketten okur; `timeout_ms` 0 ise veri gelene kadar bekler. 0 = bağlantı sonu.
fn sys_recv(fd: u64, buf: u64, len: usize, timeout_ms: u64) -> i64 {
    let Some(socket) = socket_of(fd) else {
        return EBADF;
    };
    socket_read(&socket, buf, len, timeout_ms)
}

fn socket_read(socket: &net::TcpHandle, buf: u64, len: usize, timeout_ms: u64) -> i64 {
    let len = len.clamp(1, MAX_IO);
    let deadline = pit::uptime_ms().saturating_add(if timeout_ms == 0 {
        u64::MAX / 2
    } else {
        timeout_ms
    });
    loop {
        let wait = deadline.saturating_sub(pit::uptime_ms()).min(500);
        match net::tcp_receive(socket.0, len, wait) {
            Ok(data) => {
                if copy_to_user(buf, &data).is_err() {
                    return EFAULT;
                }
                return data.len() as i64;
            }
            Err(net::tcp::Error::WouldBlock) => {
                if killed() {
                    return EINTR;
                }
                if pit::uptime_ms() >= deadline {
                    return EAGAIN;
                }
            }
            Err(e) => return tcp_errno(e),
        }
    }
}

fn sys_read(fd: u64, buf: u64, len: usize) -> i64 {
    let len = len.min(MAX_IO);
    let descriptor = sched::current_process().and_then(|p| p.fd(fd));
    let bytes = match descriptor {
        Some(Descriptor::Console) | None if fd == 0 => {
            let mut out = Vec::new();
            while out.len() < len {
                match keyboard::read_char() {
                    Some(ch) => out.extend_from_slice(ch.encode_utf8(&mut [0; 4]).as_bytes()),
                    None => break,
                }
            }
            out
        }
        Some(Descriptor::Socket(socket)) => return socket_read(&socket, buf, len, 0),
        Some(Descriptor::PipeRead(pipe)) => match pipe.read(len) {
            Ok(data) => data,
            Err(e) => return e,
        },
        Some(Descriptor::PtySlave(tty)) => match tty.read(len) {
            Ok(data) => data,
            Err(e) => return e,
        },
        Some(Descriptor::PtyMaster(tty)) => match tty.read(len) {
            Ok(data) => data,
            Err(e) => return e,
        },
        Some(Descriptor::File(file)) => {
            let mut out = vec![0u8; len];
            match file.lock().read(&mut out) {
                Ok(n) => out.truncate(n),
                Err(e) => return e.errno(),
            }
            out
        }
        _ => return EBADF,
    };
    if copy_to_user(buf, &bytes).is_err() {
        return EFAULT;
    }
    bytes.len() as i64
}

fn sys_open(path: u64, len: u64, flags: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    let open_flags = OpenFlags(flags as u32);
    let writes = [
        OpenFlags::WRITE,
        OpenFlags::CREATE,
        OpenFlags::TRUNCATE,
        OpenFlags::APPEND,
    ]
    .iter()
    .any(|&f| open_flags.contains(f));
    let uid = caller_uid();
    let allowed = if fs::stat(&path).is_ok() {
        (!open_flags.contains(OpenFlags::READ) || fs::may_access(uid, &path, fs::Access::Read))
            && (!writes || fs::may_access(uid, &path, fs::Access::Write))
    } else if open_flags.contains(OpenFlags::CREATE) {
        fs::may_modify_entry(uid, &path, false)
    } else {
        true
    };
    if !allowed {
        return EACCES;
    }
    let file = match File::open(&path, open_flags, caller_owner()) {
        Ok(file) => file,
        Err(e) => return e.errno(),
    };
    current(
        |p| match p.add_fd(Descriptor::File(Arc::new(Mutex::new(file)))) {
            Some(fd) => fd as i64,
            None => EMFILE,
        },
    )
}

#[repr(C)]
struct UserStat {
    kind: u32,
    read_only: u32,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: u32,
    /// Blok aygıtlarında gerçek mantıksal blok boyutu, diğerlerinde 512.
    block: u32,
    _pad: u32,
}

fn kind_code(kind: fs::Kind) -> u32 {
    match kind {
        fs::Kind::File => 0,
        fs::Kind::Directory => 1,
        fs::Kind::Device => 2,
    }
}

fn as_bytes<T>(value: &T) -> &[u8] {
    unsafe { core::slice::from_raw_parts(value as *const T as *const u8, size_of::<T>()) }
}

fn sys_stat(path: u64, len: u64, out: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    if !fs::may_lookup(caller_uid(), &path) {
        return EACCES;
    }
    match fs::stat(&path) {
        Ok(meta) => {
            let stat = UserStat {
                kind: kind_code(meta.kind),
                read_only: meta.read_only as u32,
                size: meta.size,
                mode: meta.mode as u32,
                uid: meta.uid,
                gid: meta.gid,
                mtime: meta.mtime,
                block: fs::device_block_size(&path) as u32,
                _pad: 0,
            };
            if copy_to_user(out, as_bytes(&stat)).is_err() {
                EFAULT
            } else {
                0
            }
        }
        Err(e) => e.errno(),
    }
}

#[repr(C)]
struct UserDirent {
    kind: u32,
    name_len: u32,
    size: u64,
    mode: u32,
    uid: u32,
    gid: u32,
    mtime: u32,
    name: [u8; 256],
}

fn sys_readdir(fd: u64, index: u64, out: u64) -> i64 {
    with_file(fd, |file| match file.dir_entry(index as usize) {
        Ok(Some(meta)) => {
            let mut dirent = UserDirent {
                kind: kind_code(meta.kind),
                name_len: meta.name.len().min(256) as u32,
                size: meta.size,
                mode: meta.mode as u32,
                uid: meta.uid,
                gid: meta.gid,
                mtime: meta.mtime,
                name: [0; 256],
            };
            dirent.name[..dirent.name_len as usize]
                .copy_from_slice(&meta.name.as_bytes()[..dirent.name_len as usize]);
            if copy_to_user(out, as_bytes(&dirent)).is_err() {
                EFAULT
            } else {
                1
            }
        }
        Ok(None) => 0,
        Err(e) => e.errno(),
    })
}

fn caller_uid() -> u32 {
    sched::current_process().map_or(crate::kernel::users::effective_uid(), |p| p.uid())
}

fn caller_owner() -> fs::Owner {
    let uid = caller_uid();
    fs::Owner { uid, gid: uid }
}

fn may_execute(path: &str) -> bool {
    fs::may_access(caller_uid(), path, fs::Access::Execute)
}

fn is_root() -> bool {
    caller_uid() == crate::kernel::users::ROOT_UID
}

fn sys_rename(from: u64, from_len: u64, to: u64, to_len: u64) -> i64 {
    let (Some(from), Some(to)) = (path_arg(from, from_len), path_arg(to, to_len)) else {
        return EFAULT;
    };
    let uid = caller_uid();
    if !fs::may_modify_entry(uid, &from, true) || !fs::may_modify_entry(uid, &to, true) {
        return EACCES;
    }
    result(fs::rename(&from, &to))
}

fn sys_chmod(path: u64, len: u64, mode: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    match fs::stat(&path) {
        Ok(meta) if is_root() || meta.uid == caller_uid() => result(fs::chmod(&path, mode as u16)),
        Ok(_) => EPERM,
        Err(e) => e.errno(),
    }
}

fn sys_chown(path: u64, len: u64, uid: u64, gid: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    if !is_root() {
        return EPERM;
    }
    result(fs::chown(&path, uid as u32, gid as u32))
}

/// Aygıt adı: `usb1p2` ya da `/dev/usb1p2` (çalışma dizinine göre çözümlenmez).
fn device_arg(ptr: u64, len: u64) -> Option<String> {
    let len = len as usize;
    if len == 0 || len > 64 {
        return None;
    }
    let mut bytes = vec![0u8; len];
    copy_from_user(&mut bytes, ptr).ok()?;
    let text = String::from_utf8(bytes).ok()?;
    Some(String::from(text.strip_prefix("/dev/").unwrap_or(&text)))
}

/// Yalnızca root: `/dev/<birim>` aygıtını var olan bir dizine bağlar (ext2 ya da FAT32).
fn sys_mount(device: u64, device_len: u64, point: u64, point_len: u64) -> i64 {
    let (Some(device), Some(point)) = (device_arg(device, device_len), path_arg(point, point_len))
    else {
        return EFAULT;
    };
    if !is_root() {
        return EPERM;
    }
    let Some(volume) = fs::block_device(&device) else {
        return ENOENT;
    };
    match fs::stat(&point) {
        Ok(meta) if meta.kind == fs::Kind::Directory => {}
        Ok(_) => return ENOTDIR,
        Err(e) => return e.errno(),
    }
    match fs::mount_volume(volume, &point) {
        Ok(_) => 0,
        Err(e) => e.errno(),
    }
}

fn sys_umount(point: u64, len: u64) -> i64 {
    let Some(point) = path_arg(point, len) else {
        return EFAULT;
    };
    if !is_root() {
        return EPERM;
    }
    result(fs::unmount(&point))
}

/// Yalnızca root: diskin bölüm tablosunu yeniden okur; birim sayısını döndürür.
fn sys_rescan(disk: u64, len: u64) -> i64 {
    let Some(disk) = device_arg(disk, len) else {
        return EFAULT;
    };
    if !is_root() {
        return EPERM;
    }
    match crate::storage::rescan(&disk) {
        Ok(n) => n as i64,
        Err("no such disk") => ENOENT,
        Err(_) => -16,
    }
}

/// Diskler, bölümler ve bağlama noktaları; satır başına bir kayıt:
/// `disk <ad> <sektör> <çıkarılabilir> <model>`,
/// `part <ad> <disk> <sektör> <tür> <PARTUUID|->`, `mount <nokta> <aygıt> <tür> <etiket>`.
fn storage_report() -> String {
    use core::fmt::Write;
    let mut out = String::new();
    let disks = crate::storage::disks();
    for d in &disks {
        let _ = writeln!(
            out,
            "disk {} {} {} {}",
            d.name(),
            d.sectors(),
            d.removable() as u8,
            d.model().replace(' ', "_")
        );
    }
    for v in crate::storage::volumes() {
        let name = v.name();
        if disks.iter().any(|d| d.name() == name) {
            continue;
        }
        let (disk, kind, id) = match crate::storage::partition_info(&v) {
            Some(p) => {
                let kind = if p.type_guid == crate::storage::ESP_TYPE {
                    "efi"
                } else if p.type_guid == crate::storage::LINUX_ROOT_TYPE {
                    "linux-root"
                } else {
                    "gpt"
                };
                (
                    p.disk,
                    String::from(kind),
                    crate::storage::guid_text(&p.unique),
                )
            }
            None => (
                String::from(name.rsplit_once('p').map_or(name.as_str(), |(d, _)| d)),
                v.model().replace(' ', "_").to_ascii_lowercase(),
                String::from("-"),
            ),
        };
        let _ = writeln!(
            out,
            "part {} {} {} {} {}",
            name,
            disk,
            v.sectors(),
            kind,
            id
        );
    }
    for m in fs::mounts() {
        let label = if m.label.is_empty() {
            String::from("-")
        } else {
            m.label.replace(' ', "_")
        };
        let _ = writeln!(out, "mount {} {} {} {}", m.point, m.device, m.kind, label);
    }
    out
}

/// `topic` 0: çekirdek komut satırı, 1: depolama raporu. Tam uzunluğu döndürür
/// (tampon küçükse kırpılır; çağıran daha büyüğüyle yeniden sorabilir).
fn sys_info(topic: u64, buf: u64, len: u64) -> i64 {
    let text = match topic {
        0 => String::from(crate::boot::cmdline()),
        1 => storage_report(),
        2 => match crate::gui::query() {
            Some(text) => text,
            None => return ENODEV,
        },
        3 => crate::drivers::report::text(),
        _ => return EINVAL,
    };
    let n = text.len().min(len as usize);
    if copy_to_user(buf, &text.as_bytes()[..n]).is_err() {
        return EFAULT;
    }
    text.len() as i64
}

/// Pano: `op` 0 = oku (`buf`, `len`; gereken uzunluğu döndürür), 1 = yaz.
/// Hesap yönetimi. `buf`: satırlarla ayrılmış alanlar; hata iletisi `out`a (256 bayt) yazılır.
/// 0 oluştur: ad, parola, yönetici(0/1), yönetici parolası
/// 1 parola: hedef, yeni parola, doğrulama (kendi için mevcut parola, başkası için yönetici parolası)
/// 2 sil: ad, yönetici parolası
fn sys_account(op: u64, buf: u64, len: u64, out: u64) -> i64 {
    use crate::kernel::users::{self, AdminCheck};
    let len = (len as usize).min(1024);
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, buf).is_err() {
        return EFAULT;
    }
    let text = String::from_utf8_lossy(&bytes).into_owned();
    let fields: Vec<&str> = text.split('\n').collect();
    let field = |i: usize| fields.get(i).copied().unwrap_or("");
    let uid = caller_uid();
    let fail = |code: i64, message: &str| {
        let mut m = Vec::from(&message.as_bytes()[..message.len().min(255)]);
        m.push(0);
        let _ = copy_to_user(out, &m);
        code
    };
    let admin_ok = |password: &str| -> Result<(), i64> {
        if uid == users::ROOT_UID {
            return Ok(());
        }
        match users::verify_admin(uid, password) {
            Ok(_) => Ok(()),
            Err(AdminCheck::NotAdmin) => Err(fail(EPERM, "bu işlem için yönetici hesabı gerekir")),
            Err(AdminCheck::WrongPassword) => {
                sched::sleep_ms(600);
                Err(fail(EACCES, "parola hatalı"))
            }
        }
    };
    let result = match op {
        0 => {
            if let Err(e) = admin_ok(field(3)) {
                return e;
            }
            users::create(field(0), field(1), field(2) == "1").map(|u| {
                crate::log!("users: {} created by uid {}", u.name, uid);
            })
        }
        1 => {
            let me = users::name_of(uid);
            if field(0) == me && uid != users::ROOT_UID {
                if users::authenticate(&me, field(2)).is_none() {
                    sched::sleep_ms(600);
                    return fail(EACCES, "mevcut parola hatalı");
                }
            } else if let Err(e) = admin_ok(field(2)) {
                return e;
            }
            users::set_password(field(0), field(1))
        }
        2 => {
            if let Err(e) = admin_ok(field(1)) {
                return e;
            }
            if users::session().is_some_and(|s| s.name == field(0)) {
                return fail(EPERM, "oturumu açık kullanıcı silinemez");
            }
            users::remove(field(0))
        }
        _ => return EINVAL,
    };
    match result {
        Ok(()) => 0,
        Err(message) => fail(EINVAL, message),
    }
}

fn sys_clipboard(op: u64, buf: u64, len: u64) -> i64 {
    match op {
        0 => {
            let text = crate::gui::app::clipboard_get();
            let n = text.len().min(len as usize);
            if copy_to_user(buf, &text.as_bytes()[..n]).is_err() {
                return EFAULT;
            }
            text.len() as i64
        }
        1 => {
            if len > 65536 {
                return EINVAL;
            }
            let mut bytes = vec![0u8; len as usize];
            if copy_from_user(&mut bytes, buf).is_err() {
                return EFAULT;
            }
            crate::gui::app::clipboard_set(&String::from_utf8_lossy(&bytes));
            0
        }
        _ => EINVAL,
    }
}

/// Rastgele baytlar (TLS anahtarları): RDRAND ve TSC karışımı SHA-256 ile beyazlatılır.
fn sys_random(buf: u64, len: u64) -> i64 {
    if len > 4096 {
        return EINVAL;
    }
    static COUNTER: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
    let mut out = vec![0u8; len as usize];
    for chunk in out.chunks_mut(32) {
        let mut seed = [0u8; 48];
        for i in 0..4 {
            seed[i * 4..i * 4 + 4].copy_from_slice(&crate::net::random_u32().to_le_bytes());
        }
        seed[16..24].copy_from_slice(&crate::arch::tsc::read().to_le_bytes());
        let n = COUNTER.fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        seed[24..32].copy_from_slice(&n.to_le_bytes());
        seed[32..40].copy_from_slice(&crate::arch::pit::uptime_ms().to_le_bytes());
        let digest = crate::crypto::sha256(&seed);
        chunk.copy_from_slice(&digest[..chunk.len()]);
    }
    if copy_to_user(buf, &out).is_err() {
        return EFAULT;
    }
    0
}

/// Masaüstü ayarı ya da eylemi; yalnızca oturum sahibi ve root.
fn sys_desktop_set(key: u64, key_len: u64, value: u64, value_len: u64) -> i64 {
    let uid = caller_uid();
    if uid != 0 && crate::kernel::users::session().is_none_or(|s| s.uid != uid) {
        return EACCES;
    }
    let text = |ptr: u64, len: u64| -> Option<String> {
        if len > 512 {
            return None;
        }
        let mut bytes = vec![0u8; len as usize];
        copy_from_user(&mut bytes, ptr).ok()?;
        String::from_utf8(bytes).ok()
    };
    let (Some(key), Some(value)) = (text(key, key_len), text(value, value_len)) else {
        return EFAULT;
    };
    match crate::gui::set(&key, &value) {
        Ok(()) => 0,
        Err(_) => EINVAL,
    }
}

fn sys_statfs(path: u64, len: u64, out: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    match fs::capacity_of(&path) {
        Ok((total, free)) => {
            let data = [total, free];
            let bytes: [u8; 16] = unsafe { core::mem::transmute(data) };
            copy_to_user(out, &bytes).map_or(EFAULT, |_| 0)
        }
        Err(e) => e.errno(),
    }
}

fn sys_username(buf: u64, len: u64) -> i64 {
    let name = crate::kernel::users::name_of(caller_uid());
    let n = name.len().min(len as usize);
    if copy_to_user(buf, &name.as_bytes()[..n]).is_err() {
        return EFAULT;
    }
    n as i64
}

fn user_args(argv: u64, argc: u64) -> Result<Vec<String>, i64> {
    if argc as usize > MAX_ARGS {
        return Err(EINVAL);
    }
    let mut pairs = vec![0u8; argc as usize * 16];
    if copy_from_user(&mut pairs, argv).is_err() {
        return Err(EFAULT);
    }
    let mut args = Vec::with_capacity(argc as usize);
    for pair in pairs.chunks(16) {
        let ptr = u64::from_le_bytes(pair[0..8].try_into().unwrap());
        let len = u64::from_le_bytes(pair[8..16].try_into().unwrap()) as usize;
        if len > MAX_PATH {
            return Err(EINVAL);
        }
        let mut bytes = vec![0u8; len];
        if copy_from_user(&mut bytes, ptr).is_err() {
            return Err(EFAULT);
        }
        args.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    Ok(args)
}

fn sys_spawn(path: u64, len: u64, argv: u64, argc: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    if !may_execute(&path) {
        return EACCES;
    }
    let args = match user_args(argv, argc) {
        Ok(args) => args,
        Err(e) => return e,
    };
    match crate::kernel::elf::spawn(&path, &args) {
        Ok((process, _)) => process.pid as i64,
        Err(e) => e.errno(),
    }
}

fn sys_mmap(len: u64) -> i64 {
    let len = align_up(len.max(1), PAGE_SIZE);
    if len > MAX_MMAP {
        return ENOMEM;
    }
    current(|p| {
        let Some(base) = p.reserve_mmap(len) else {
            return ENOMEM;
        };
        let mut offset = 0;
        while offset < len {
            let Some(frame) = pmm::alloc_zeroed_frame() else {
                return ENOMEM;
            };
            if paging::map_page_in(p.root(), base + offset, frame, USER | WRITABLE | NO_EXECUTE)
                .is_err()
            {
                pmm::free_frame(frame);
                return ENOMEM;
            }
            offset += PAGE_SIZE;
        }
        base as i64
    })
}

fn sys_munmap(addr: u64, len: u64) -> i64 {
    if !addr.is_multiple_of(PAGE_SIZE)
        || addr < sched::process::MMAP_BASE
        || addr + len > sched::process::MMAP_LIMIT
    {
        return EINVAL;
    }
    current(|p| {
        let mut offset = 0;
        while offset < align_up(len, PAGE_SIZE) {
            if let Some(frame) = paging::unmap_page_in(p.root(), addr + offset) {
                pmm::free_frame(frame);
            }
            offset += PAGE_SIZE;
        }
        0
    })
}

#[repr(C)]
struct SysInfo {
    uptime_ms: u64,
    total_memory: u64,
    free_memory: u64,
    threads: u64,
    processes: u64,
    cpus: u64,
    tsc_mhz: u64,
    fps: u64,
    vendor: [u8; 16],
    brand: [u8; 48],
    screen_width: u32,
    screen_height: u32,
}

fn sys_sysinfo(out: u64) -> i64 {
    let mem = pmm::stats();
    let mut info = SysInfo {
        uptime_ms: pit::uptime_ms(),
        total_memory: mem.total_bytes,
        free_memory: mem.free_bytes,
        threads: sched::threads().len() as u64,
        processes: sched::process::live_count() as u64,
        cpus: crate::arch::smp::online_count() as u64,
        tsc_mhz: tsc::mhz(),
        fps: crate::gui::stats().fps,
        vendor: [0; 16],
        brand: [0; 48],
        screen_width: 0,
        screen_height: 0,
    };
    if let Some((w, h)) = crate::gfx::display::with(|d| d.size()) {
        info.screen_width = w as u32;
        info.screen_height = h as u32;
    }
    info.vendor[..12].copy_from_slice(&cpu::vendor());
    if let Some(brand) = cpu::brand() {
        info.brand.copy_from_slice(&brand);
    }
    if copy_to_user(out, as_bytes(&info)).is_err() {
        EFAULT
    } else {
        0
    }
}

fn sys_win_create(width: u64, height: u64, title: u64, title_len: u64) -> i64 {
    let mut bytes = vec![0u8; title_len.min(64) as usize];
    if copy_from_user(&mut bytes, title).is_err() {
        return EFAULT;
    }
    let title = String::from_utf8_lossy(&bytes).into_owned();
    current(|p| crate::gui::app::create(p.pid, width as i32, height as i32, &title))
}

fn sys_win_present(id: u64, pixels: u64, width: u64, height: u64) -> i64 {
    let count = (width * height) as usize;
    if width == 0 || height == 0 || count > 4096 * 4096 {
        return EINVAL;
    }
    let mut data = vec![0u32; count];
    let bytes = unsafe { core::slice::from_raw_parts_mut(data.as_mut_ptr() as *mut u8, count * 4) };
    if copy_from_user(bytes, pixels).is_err() {
        return EFAULT;
    }
    current(|p| crate::gui::app::present(p.pid, id, width as i32, height as i32, data))
}

/// Yalnızca değişen dikdörtgeni gönderir: `size` = genişlik << 32 | yükseklik,
/// `rect` = x << 48 | y << 32 | w << 16 | h (tampondaki konum).
fn sys_win_present_rect(id: u64, pixels: u64, size: u64, rect: u64) -> i64 {
    let (width, height) = ((size >> 32) as usize, (size & 0xffff_ffff) as usize);
    let (x, y, w, h) = (
        (rect >> 48) as usize,
        (rect >> 32 & 0xffff) as usize,
        (rect >> 16 & 0xffff) as usize,
        (rect & 0xffff) as usize,
    );
    if width == 0 || height == 0 || width * height > 4096 * 4096 {
        return EINVAL;
    }
    if w == 0 || h == 0 || x + w > width || y + h > height {
        return EINVAL;
    }
    let mut data = alloc::vec::Vec::with_capacity(w * h);
    for row in y..y + h {
        let at = data.len();
        data.resize(at + w, 0u32);
        let bytes =
            unsafe { core::slice::from_raw_parts_mut(data[at..].as_mut_ptr() as *mut u8, w * 4) };
        if copy_from_user(bytes, pixels + ((row * width + x) * 4) as u64).is_err() {
            return EFAULT;
        }
    }
    let region = crate::gfx::Rect::new(x as i32, y as i32, w as i32, h as i32);
    current(|p| {
        crate::gui::app::present_rect(p.pid, id, width as i32, height as i32, region, &data)
    })
}

fn sys_win_event(id: u64, out: u64, timeout_ms: u64) -> i64 {
    current(
        |p| match crate::gui::app::next_event(p.pid, id, timeout_ms.min(60_000)) {
            Ok(Some(event)) => {
                if copy_to_user(out, as_bytes(&event)).is_err() {
                    EFAULT
                } else {
                    1
                }
            }
            Ok(None) => 0,
            Err(code) => code,
        },
    )
}

fn sys_wait(pid: u64, flags: u64) -> i64 {
    const NOHANG: u64 = 1;
    let Some(done) = sched::process::completion_of(pid) else {
        return ECHILD;
    };
    if flags & NOHANG != 0 && !done.is_done() {
        return EAGAIN;
    }
    done.wait()
}

/// `pid` > 0 tek işlem, < 0 süreç grubu (-pgid). `signal` 0 ise SIGKILL.
fn sys_kill(pid: i64, signal: u64) -> i64 {
    let signal = if signal == 0 {
        sched::process::SIGKILL
    } else {
        signal as u32
    };
    if signal > sched::process::MAX_SIGNAL {
        return EINVAL;
    }
    let uid = caller_uid();
    let allowed = |p: &sched::Process| uid == crate::kernel::users::ROOT_UID || p.uid() == uid;
    if pid < 0 {
        let members = sched::process::group(pid.unsigned_abs());
        if members.is_empty() {
            return ESRCH;
        }
        if !members.iter().all(|p| allowed(p)) {
            return EPERM;
        }
        sched::signal_group(pid.unsigned_abs(), signal);
        return 0;
    }
    match sched::process::find(pid as u64) {
        Some(p) if !allowed(&p) => EPERM,
        Some(p) => {
            drop(p);
            sched::signal_process(pid as u64, signal);
            0
        }
        None => ESRCH,
    }
}

fn sys_fork(frame: &SyscallFrame) -> i64 {
    let Some(parent) = sched::current_process() else {
        return EINVAL;
    };
    let Some(root) = paging::fork_address_space(parent.root()) else {
        return ENOMEM;
    };
    let child = parent.fork(root);
    let mut fpu = FpuState::initial();
    fpu.save();
    let pid = child.pid;
    match sched::spawn_user(child, sched::UserStart::Fork(*frame), Some(fpu)) {
        Some((_, done)) => {
            sched::process::set_completion(pid, done);
            crate::log!("fork: pid {} -> {}", parent.pid, pid);
            pid as i64
        }
        None => ENOMEM,
    }
}

/// Geçerli işlemin görüntüsünü değiştirir; başarıda ring 3 giriş bilgisi döner.
fn sys_exec(path: u64, len: u64, argv: u64, argc: u64) -> Result<(u64, u64, u64, u64), i64> {
    let path = path_arg(path, len).ok_or(EFAULT)?;
    if !may_execute(&path) {
        return Err(EACCES);
    }
    let args = user_args(argv, argc)?;
    let image = crate::kernel::elf::load(&path, &args).map_err(|e| e.errno())?;
    let process = sched::current_process().ok_or(EINVAL)?;
    let name = path.rsplit('/').next().unwrap_or(&path);
    let old = process.replace_image(image.root, name);
    sched::set_current_image(image.root, name);
    paging::destroy_address_space(old);
    FpuState::initial().restore();
    crate::log!(
        "exec: {} as pid {} (entry {:#x})",
        path,
        process.pid,
        image.entry
    );
    Ok((image.entry, image.stack, image.argc, image.argv))
}

fn install_pair(pair: [Descriptor; 2], out: u64) -> i64 {
    current(|p| {
        let [a, b] = pair;
        let Some(first) = p.add_fd(a) else {
            return EMFILE;
        };
        let Some(second) = p.add_fd(b) else {
            p.close_fd(first as u64);
            return EMFILE;
        };
        let fds = [first as u32, second as u32];
        let bytes: [u8; 8] = unsafe { core::mem::transmute(fds) };
        if copy_to_user(out, &bytes).is_err() {
            p.close_fd(first as u64);
            p.close_fd(second as u64);
            return EFAULT;
        }
        0
    })
}

/// `out` = [okuma ucu, yazma ucu].
fn sys_pipe(out: u64) -> i64 {
    let (reader, writer) = pipe::new();
    install_pair(
        [Descriptor::PipeRead(reader), Descriptor::PipeWrite(writer)],
        out,
    )
}

/// `out` = [ana uç, yardımcı uç].
fn sys_openpty(out: u64) -> i64 {
    let (master, slave) = pty::open();
    install_pair(
        [Descriptor::PtyMaster(master), Descriptor::PtySlave(slave)],
        out,
    )
}

fn sys_dup2(old: u64, new: u64) -> i64 {
    current(|p| {
        let Some(descriptor) = p.fd(old) else {
            return EBADF;
        };
        if old == new {
            return new as i64;
        }
        if p.set_fd(new, descriptor) {
            new as i64
        } else {
            EBADF
        }
    })
}

fn sys_chdir(path: u64, len: u64) -> i64 {
    let Some(path) = path_arg(path, len) else {
        return EFAULT;
    };
    if !fs::may_access(caller_uid(), &path, fs::Access::Execute) {
        return EACCES;
    }
    match fs::stat(&path) {
        Ok(meta) if meta.kind == fs::Kind::Directory => current(|p| {
            p.set_cwd(path);
            0
        }),
        Ok(_) => ENOTDIR,
        Err(e) => e.errno(),
    }
}

/// `pid` 0 = kendisi, `pgid` 0 = `pid`. Yalnızca kendisi ya da çocukları için.
fn sys_setpgid(pid: u64, pgid: u64) -> i64 {
    current(|me| {
        let target = if pid == 0 || pid == me.pid {
            me.clone()
        } else {
            match sched::process::find(pid) {
                Some(p) if p.ppid == me.pid => p,
                Some(_) => return EPERM,
                None => return ESRCH,
            }
        };
        target.set_pgid(if pgid == 0 { target.pid } else { pgid });
        0
    })
}

fn sys_getpgid(pid: u64) -> i64 {
    if pid == 0 {
        return current(|p| p.pgid() as i64);
    }
    sched::process::find(pid).map_or(ESRCH, |p| p.pgid() as i64)
}

enum Tty {
    Master(Arc<pty::Master>),
    Slave(Arc<pty::Slave>),
}

impl Tty {
    fn set_foreground(&self, pgid: u64) {
        match self {
            Tty::Master(t) => t.set_foreground(pgid),
            Tty::Slave(t) => t.set_foreground(pgid),
        }
    }

    fn foreground(&self) -> u64 {
        match self {
            Tty::Master(t) => t.foreground(),
            Tty::Slave(t) => t.foreground(),
        }
    }

    fn set_mode(&self, mode: u32) -> u32 {
        match self {
            Tty::Master(t) => t.set_mode(mode),
            Tty::Slave(t) => t.set_mode(mode),
        }
    }

    fn window_size(&self, size: u32) -> u32 {
        match self {
            Tty::Master(t) => t.window_size(size),
            Tty::Slave(t) => t.window_size(size),
        }
    }
}

fn sys_tty(fd: u64, f: impl FnOnce(&Tty) -> i64) -> i64 {
    match sched::current_process().and_then(|p| p.fd(fd)) {
        Some(Descriptor::PtyMaster(t)) => f(&Tty::Master(t)),
        Some(Descriptor::PtySlave(t)) => f(&Tty::Slave(t)),
        Some(_) => ENOTTY,
        None => EBADF,
    }
}

pub const POLL_WINDOW: u16 = 1;

/// `poll` girdisi: `kind` = tanıtıcı ya da pencere; `ready` çekirdekçe doldurulur.
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct PollEntry {
    id: u32,
    kind: u16,
    ready: u16,
}

fn poll_ready(process: &sched::Process, entry: &PollEntry) -> bool {
    if entry.kind == POLL_WINDOW {
        return crate::gui::app::has_event(process.pid, entry.id as u64);
    }
    match process.fd(entry.id as u64) {
        Some(Descriptor::PipeRead(p)) => p.readable(),
        Some(Descriptor::PtyMaster(t)) => t.readable(),
        Some(Descriptor::PtySlave(t)) => t.readable(),
        Some(Descriptor::Socket(_) | Descriptor::File(_)) | None => true,
        Some(Descriptor::Console) => keyboard::has_event(),
        Some(Descriptor::PipeWrite(_)) => true,
    }
}

/// Girdilerden biri hazır olana ya da süre dolana kadar bekler; hazır sayısını döndürür.
/// `timeout_ms` = u64::MAX sonsuz bekler.
fn sys_poll(entries: u64, count: u64, timeout_ms: u64) -> i64 {
    if count == 0 || count > 64 {
        return EINVAL;
    }
    let size = size_of::<PollEntry>();
    let mut raw = vec![0u8; count as usize * size];
    if copy_from_user(&mut raw, entries).is_err() {
        return EFAULT;
    }
    let mut list: Vec<PollEntry> = raw
        .chunks(size)
        .map(|c| unsafe { core::ptr::read_unaligned(c.as_ptr() as *const PollEntry) })
        .collect();
    let Some(process) = sched::current_process() else {
        return EINVAL;
    };
    let deadline = pit::uptime_ms().saturating_add(timeout_ms.min(u64::MAX / 2));
    let scan = |list: &mut Vec<PollEntry>| {
        let mut n = 0;
        for entry in list.iter_mut() {
            entry.ready = poll_ready(&process, entry) as u16;
            n += entry.ready as i64;
        }
        n
    };
    let mut ready = scan(&mut list);
    while ready == 0 && pit::uptime_ms() < deadline && sched::pending_signal().is_none() {
        let remaining = deadline - pit::uptime_ms();
        pipe::POLL.wait_timeout(
            || list.iter().any(|e| poll_ready(&process, e)) || sched::pending_signal().is_some(),
            remaining.min(1000),
        );
        ready = scan(&mut list);
    }
    drop(process);
    let bytes = unsafe { core::slice::from_raw_parts(list.as_ptr() as *const u8, raw.len()) };
    if copy_to_user(entries, bytes).is_err() {
        return EFAULT;
    }
    ready
}

/// `sudo`: çağıran yönetici ve parola doğruysa işlem (ve sonradan başlattıkları) root olur.
fn sys_elevate(password: u64, len: u64) -> i64 {
    let len = (len as usize).min(256);
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, password).is_err() {
        return EFAULT;
    }
    let Some(process) = sched::current_process() else {
        return EINVAL;
    };
    let uid = process.uid();
    if uid == crate::kernel::users::ROOT_UID {
        return 0;
    }
    let password = String::from_utf8_lossy(&bytes);
    match crate::kernel::users::verify_admin(uid, &password) {
        Ok(name) => {
            process.set_uid(crate::kernel::users::ROOT_UID);
            crate::log!("sudo: {} elevated pid {}", name, process.pid);
            0
        }
        Err(crate::kernel::users::AdminCheck::NotAdmin) => {
            crate::log!("sudo: uid {} is not an administrator", uid);
            EPERM
        }
        Err(crate::kernel::users::AdminCheck::WrongPassword) => {
            crate::log!("sudo: wrong password for uid {}", uid);
            sched::sleep_ms(600);
            EACCES
        }
    }
}

#[repr(C)]
struct NetInfo {
    address: u32,
    netmask: u32,
    gateway: u32,
    dns: u32,
}

/// Adresler `resolve` gibi a.b.c.d → a<<24|... biçimindedir; ağ yoksa ENETUNREACH.
fn sys_netinfo(out: u64) -> i64 {
    let Some(config) = net::config() else {
        return ENETUNREACH;
    };
    let pack = |ip: net::wire::Ipv4| u32::from_be_bytes(ip.0);
    let info = NetInfo {
        address: pack(config.address),
        netmask: pack(config.netmask),
        gateway: pack(config.gateway),
        dns: config.dns.first().copied().map_or(0, pack),
    };
    if copy_to_user(out, as_bytes(&info)).is_err() {
        EFAULT
    } else {
        0
    }
}

fn sys_log(buf: u64, len: u64) -> i64 {
    let len = (len as usize).min(1024);
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, buf).is_err() {
        return EFAULT;
    }
    crate::log!("{}", String::from_utf8_lossy(&bytes));
    0
}

/// Çekirdek tanı komutunu çalıştırır; çıktısı işlemin stdout'una gider.
fn sys_kcmd(line: u64, len: u64) -> i64 {
    let len = (len as usize).min(MAX_PATH);
    let mut bytes = vec![0u8; len];
    if copy_from_user(&mut bytes, line).is_err() {
        return EFAULT;
    }
    let Ok(text) = core::str::from_utf8(&bytes) else {
        return EINVAL;
    };
    let Some(stdout) = sched::current_process().and_then(|p| p.fd(1)) else {
        return EBADF;
    };
    if crate::kernel::shell::run_for_process(text.trim(), stdout) {
        0
    } else {
        ENOENT
    }
}

/// `print!` yönlendirmesi için: metni tanıtıcıya yazar (konsol ise çekirdek konsoluna).
pub fn write_descriptor(descriptor: &Descriptor, bytes: &[u8]) {
    match descriptor {
        Descriptor::PtySlave(t) => {
            let _ = t.write(bytes);
        }
        Descriptor::PipeWrite(p) => {
            let _ = p.write(bytes);
        }
        Descriptor::File(f) => {
            let _ = f.lock().write(bytes);
        }
        Descriptor::PtyMaster(t) => {
            t.write(bytes);
        }
        Descriptor::Socket(s) => {
            let _ = net::tcp_send(s.0, bytes);
        }
        Descriptor::Console | Descriptor::PipeRead(_) => {}
    }
}

fn user_range_ok(addr: u64, len: usize, write: bool) -> bool {
    if len == 0 {
        return true;
    }
    let Some(end) = addr.checked_add(len as u64) else {
        return false;
    };
    if end > USER_SPACE_END {
        return false;
    }
    let mut page = addr & !0xfff;
    while page < end {
        if write {
            paging::resolve_cow(paging::current_root(), page);
        }
        if !paging::user_accessible(page, write) {
            return false;
        }
        page += 0x1000;
    }
    true
}

fn with_user_access<R>(f: impl FnOnce() -> R) -> R {
    let smap = paging::smap_enabled();
    if smap {
        unsafe { asm!("stac", options(nomem, nostack)) };
    }
    let result = f();
    if smap {
        unsafe { asm!("clac", options(nomem, nostack)) };
    }
    result
}

pub fn copy_from_user(dst: &mut [u8], src: u64) -> Result<(), ()> {
    if !user_range_ok(src, dst.len(), false) {
        return Err(());
    }
    with_user_access(|| unsafe {
        core::ptr::copy_nonoverlapping(src as *const u8, dst.as_mut_ptr(), dst.len())
    });
    Ok(())
}

pub fn copy_to_user(dst: u64, src: &[u8]) -> Result<(), ()> {
    if !user_range_ok(dst, src.len(), true) {
        return Err(());
    }
    with_user_access(|| unsafe {
        core::ptr::copy_nonoverlapping(src.as_ptr(), dst as *mut u8, src.len())
    });
    Ok(())
}

#[allow(dead_code)]
pub const ERRNO_NOT_FOUND: i64 = ENOENT;
