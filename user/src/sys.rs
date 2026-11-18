//! Ham syscall arayüzü ve çekirdekle paylaşılan veri yapıları.

use core::arch::asm;

pub const EXIT: u64 = 0;
pub const WRITE: u64 = 1;
pub const READ: u64 = 2;
pub const UPTIME_MS: u64 = 3;
pub const SLEEP_MS: u64 = 4;
pub const TIME: u64 = 5;
pub const GETPID: u64 = 6;
pub const YIELD: u64 = 7;
pub const OPEN: u64 = 8;
pub const CLOSE: u64 = 9;
pub const SEEK: u64 = 10;
pub const STAT: u64 = 11;
pub const READDIR: u64 = 12;
pub const MKDIR: u64 = 13;
pub const UNLINK: u64 = 14;
pub const SPAWN: u64 = 15;
pub const WAIT: u64 = 16;
pub const MMAP: u64 = 17;
pub const MUNMAP: u64 = 18;
pub const SYSINFO: u64 = 19;
pub const WIN_CREATE: u64 = 20;
pub const WIN_PRESENT: u64 = 21;
pub const WIN_EVENT: u64 = 22;
pub const WIN_CLOSE: u64 = 23;
pub const WIN_PRESENT_RECT: u64 = 65;
pub const KILL: u64 = 24;
pub const TIME_NS: u64 = 25;
pub const CONNECT: u64 = 26;
pub const LISTEN: u64 = 27;
pub const ACCEPT: u64 = 28;
pub const RESOLVE: u64 = 29;
pub const RECV: u64 = 30;
pub const GETUID: u64 = 31;
pub const USERNAME: u64 = 32;
pub const FORK: u64 = 33;
pub const EXEC: u64 = 34;
pub const PIPE: u64 = 35;
pub const DUP2: u64 = 36;
pub const CHDIR: u64 = 37;
pub const GETCWD: u64 = 38;
pub const SETPGID: u64 = 39;
pub const GETPGID: u64 = 40;
pub const SIGIGNORE: u64 = 41;
pub const OPENPTY: u64 = 42;
pub const TCSETPGRP: u64 = 43;
pub const TTYMODE: u64 = 44;
pub const POLL: u64 = 45;
pub const LOG: u64 = 46;
pub const KCMD: u64 = 47;
pub const GETPPID: u64 = 48;
pub const TCGETPGRP: u64 = 49;
pub const ELEVATE: u64 = 50;
pub const NETINFO: u64 = 51;
pub const TTYSIZE: u64 = 52;
pub const RENAME: u64 = 53;
pub const CHMOD: u64 = 54;
pub const CHOWN: u64 = 55;
pub const MOUNT: u64 = 56;
pub const UMOUNT: u64 = 57;
pub const RESCAN: u64 = 58;
pub const STATFS: u64 = 59;
pub const INFO: u64 = 60;
pub const DESKTOP_SET: u64 = 61;
pub const RANDOM: u64 = 62;
pub const CLIPBOARD: u64 = 63;
pub const ACCOUNT: u64 = 64;

/// # Safety
/// Argümanlar çekirdeğin `n` numaralı syscall için beklediği anlama sahip olmalıdır;
/// işaretçi argümanları geçerli kullanıcı belleğini göstermelidir.
#[inline(always)]
pub unsafe fn syscall6(n: u64, a0: u64, a1: u64, a2: u64, a3: u64, a4: u64, a5: u64) -> i64 {
    let ret: i64;
    unsafe {
        asm!(
            "syscall",
            inlateout("rax") n as i64 => ret,
            in("rdi") a0,
            in("rsi") a1,
            in("rdx") a2,
            in("r10") a3,
            in("r8") a4,
            in("r9") a5,
            lateout("rcx") _,
            lateout("r11") _,
            options(nostack),
        );
    }
    ret
}

#[inline(always)]
pub fn call(n: u64, args: &[u64]) -> i64 {
    let a = |i: usize| args.get(i).copied().unwrap_or(0);
    unsafe { syscall6(n, a(0), a(1), a(2), a(3), a(4), a(5)) }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct Stat {
    pub kind: u32,
    pub read_only: u32,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
    /// Blok aygıtının mantıksal blok boyutu (4Kn disklerde 4096).
    pub block: u32,
    pub _pad: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct Dirent {
    pub kind: u32,
    pub name_len: u32,
    pub size: u64,
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub mtime: u32,
    pub name: [u8; 256],
}

#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct SysInfo {
    pub uptime_ms: u64,
    pub total_memory: u64,
    pub free_memory: u64,
    pub threads: u64,
    pub processes: u64,
    pub cpus: u64,
    pub tsc_mhz: u64,
    pub fps: u64,
    pub vendor: [u8; 16],
    pub brand: [u8; 48],
    pub screen_width: u32,
    pub screen_height: u32,
}

impl Default for SysInfo {
    fn default() -> Self {
        Self {
            uptime_ms: 0,
            total_memory: 0,
            free_memory: 0,
            threads: 0,
            processes: 0,
            cpus: 0,
            tsc_mhz: 0,
            fps: 0,
            vendor: [0; 16],
            brand: [0; 48],
            screen_width: 0,
            screen_height: 0,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u32,
    pub x: i32,
    pub y: i32,
    pub button: u32,
    pub key: u32,
}

pub const EVENT_NONE: u32 = 0;
pub const EVENT_MOUSE_MOVE: u32 = 1;
pub const EVENT_MOUSE_DOWN: u32 = 2;
pub const EVENT_MOUSE_UP: u32 = 3;
pub const EVENT_KEY: u32 = 4;
pub const EVENT_CLOSE: u32 = 5;
/// Fare tekerleği: `key` adım sayısı (i32, aşağı pozitif).
pub const EVENT_WHEEL: u32 = 6;
/// Pencere yeniden boyutlandı: `x` × `y` yeni istemci alanı.
pub const EVENT_RESIZE: u32 = 7;
/// Sağ tık (bağlam menüsü): `x`/`y` istemci alanında.
pub const EVENT_CONTEXT: u32 = 8;
/// Ctrl+Shift+V (yapıştır) tuş kodu.
pub const KEY_PASTE: u32 = 0xf710;
pub const KEY_UP: u32 = 0xf700;
pub const KEY_DOWN: u32 = 0xf701;
pub const KEY_LEFT: u32 = 0xf702;
pub const KEY_RIGHT: u32 = 0xf703;
pub const KEY_HOME: u32 = 0xf704;
pub const KEY_END: u32 = 0xf705;
pub const KEY_DELETE: u32 = 0xf706;
pub const KEY_PAGE_UP: u32 = 0xf707;
pub const KEY_PAGE_DOWN: u32 = 0xf708;
pub const KEY_INSERT: u32 = 0xf709;
/// F1..F12 = KEY_F1..KEY_F1 + 11.
pub const KEY_F1: u32 = 0xf721;
/// Shift basılıyken gezinme tuşlarına eklenen bit.
pub const KEY_SHIFT: u32 = 0x1_0000;
/// Ctrl basılıyken gezinme tuşlarına eklenen bit (ör. Ctrl+← kelime başı).
pub const KEY_CTRL: u32 = 0x2_0000;
