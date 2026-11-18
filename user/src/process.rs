//! İşlem yönetimi.

use crate::sys::{self, SysInfo};
use crate::{String, Vec};

pub fn exit(code: i32) -> ! {
    sys::call(sys::EXIT, &[code as i64 as u64]);
    unreachable!()
}

pub fn pid() -> u64 {
    sys::call(sys::GETPID, &[]) as u64
}

pub fn yield_now() {
    sys::call(sys::YIELD, &[]);
}

/// `path` programını `args` ile başlatır; pid döndürür.
pub fn spawn(path: &str, args: &[&str]) -> Result<u64, i64> {
    let pairs: Vec<[u64; 2]> = args
        .iter()
        .map(|a| [a.as_ptr() as u64, a.len() as u64])
        .collect();
    let r = sys::call(
        sys::SPAWN,
        &[
            path.as_ptr() as u64,
            path.len() as u64,
            pairs.as_ptr() as u64,
            pairs.len() as u64,
        ],
    );
    if r < 0 { Err(r) } else { Ok(r as u64) }
}

pub fn wait(pid: u64) -> i64 {
    sys::call(sys::WAIT, &[pid])
}

/// Bitmişse çıkış kodunu, hâlâ çalışıyorsa `None` döndürür.
pub fn try_wait(pid: u64) -> Option<i64> {
    let r = sys::call(sys::WAIT, &[pid, 1]);
    (r != EAGAIN).then_some(r)
}

pub fn kill(pid: u64) -> bool {
    sys::call(sys::KILL, &[pid]) == 0
}

pub const SIGINT: u32 = 2;
pub const SIGQUIT: u32 = 3;
pub const SIGKILL: u32 = 9;
pub const SIGPIPE: u32 = 13;
pub const SIGTERM: u32 = 15;

pub const EPERM: i64 = -1;
pub const ESRCH: i64 = -3;
pub const EINTR: i64 = -4;
pub const EAGAIN: i64 = -11;

/// `target` > 0 işlem, < 0 süreç grubu (-pgid).
pub fn signal(target: i64, signal: u32) -> Result<(), i64> {
    let r = sys::call(sys::KILL, &[target as u64, signal as u64]);
    if r < 0 { Err(r) } else { Ok(()) }
}

/// Sinyal ile bitmiş işlemin çıkış kodu `-sinyal`; CPU istisnası `-128 - vektör`.
pub fn describe_exit(code: i64) -> Option<String> {
    match code {
        0 => None,
        -2 => Some(String::from("interrupted")),
        -3 => Some(String::from("quit")),
        -9 => Some(String::from("killed")),
        -13 => Some(String::from("broken pipe")),
        -15 => Some(String::from("terminated")),
        c if c <= -128 => Some(crate::format!("CPU exception {}", -c - 128)),
        c => Some(crate::format!("exit code {}", c)),
    }
}

/// Ebeveynde çocuğun pid'i, çocukta 0.
pub fn fork() -> Result<u64, i64> {
    let r = sys::call(sys::FORK, &[]);
    if r < 0 { Err(r) } else { Ok(r as u64) }
}

/// Geçerli işlemi `path` programıyla değiştirir; yalnızca hata olursa döner.
pub fn exec(path: &str, args: &[&str]) -> i64 {
    let pairs: Vec<[u64; 2]> = args
        .iter()
        .map(|a| [a.as_ptr() as u64, a.len() as u64])
        .collect();
    sys::call(
        sys::EXEC,
        &[
            path.as_ptr() as u64,
            path.len() as u64,
            pairs.as_ptr() as u64,
            pairs.len() as u64,
        ],
    )
}

/// `pid` 0 = kendisi; `pgid` 0 = `pid`.
pub fn setpgid(pid: u64, pgid: u64) -> i64 {
    sys::call(sys::SETPGID, &[pid, pgid])
}

pub fn pgid() -> u64 {
    sys::call(sys::GETPGID, &[0]) as u64
}

pub fn ppid() -> u64 {
    sys::call(sys::GETPPID, &[]) as u64
}

/// Yok sayılacak sinyallerin maskesini ayarlar (bit n = sinyal n); eskisini döndürür.
pub fn ignore_signals(mask: u32) -> u32 {
    sys::call(sys::SIGIGNORE, &[mask as u64]) as u32
}

pub fn chdir(path: &str) -> Result<(), i64> {
    let r = sys::call(sys::CHDIR, &[path.as_ptr() as u64, path.len() as u64]);
    if r < 0 { Err(r) } else { Ok(()) }
}

pub fn cwd() -> String {
    let mut buf = [0u8; 1024];
    let n = sys::call(sys::GETCWD, &[buf.as_mut_ptr() as u64, buf.len() as u64]);
    if n <= 0 {
        return String::from("/");
    }
    String::from_utf8_lossy(&buf[..n as usize]).into_owned()
}

pub fn uid() -> u32 {
    sys::call(sys::GETUID, &[]) as u32
}

pub fn username() -> String {
    let mut buf = [0u8; 64];
    let n = sys::call(sys::USERNAME, &[buf.as_mut_ptr() as u64, buf.len() as u64]);
    String::from_utf8_lossy(&buf[..n.max(0) as usize]).into_owned()
}

/// Çekirdeğin tanı komutlarından birini çalıştırır (çıktı stdout'a); bilinmiyorsa `Err`.
pub fn kcmd(line: &str) -> Result<(), i64> {
    let r = sys::call(sys::KCMD, &[line.as_ptr() as u64, line.len() as u64]);
    if r < 0 { Err(r) } else { Ok(()) }
}

pub fn sysinfo() -> SysInfo {
    let mut info = SysInfo::default();
    sys::call(sys::SYSINFO, &[&mut info as *mut _ as u64]);
    info
}

/// Çekirdeğin yığına koyduğu (işaretçi, uzunluk) çiftlerinden argümanları kopyalar.
///
/// # Safety
/// `argv`, `_start`'a çekirdekten gelen geçerli bir dizi olmalıdır.
pub unsafe fn collect_args(argc: usize, argv: *const [u64; 2]) -> Vec<String> {
    (0..argc)
        .map(|i| {
            let [ptr, len] = unsafe { *argv.add(i) };
            let bytes = unsafe { core::slice::from_raw_parts(ptr as *const u8, len as usize) };
            String::from_utf8_lossy(bytes).into_owned()
        })
        .collect()
}

/// `sudo`: yönetici parolasıyla geçerli işlemi root yapar (çocuklar devralır).
/// `EPERM` = yönetici değil, `EACCES` = parola yanlış.
pub fn elevate(password: &str) -> Result<(), i64> {
    let r = sys::call(
        sys::ELEVATE,
        &[password.as_ptr() as u64, password.len() as u64],
    );
    if r < 0 { Err(r) } else { Ok(()) }
}

/// Hesap işlemi (0 oluştur, 1 parola, 2 sil); hata durumunda çekirdeğin iletisi.
pub fn account(op: u64, fields: &[&str]) -> Result<(), String> {
    let request = fields.join("\n");
    let mut message = [0u8; 256];
    let r = sys::call(
        sys::ACCOUNT,
        &[
            op,
            request.as_ptr() as u64,
            request.len() as u64,
            message.as_mut_ptr() as u64,
        ],
    );
    if r >= 0 {
        return Ok(());
    }
    let end = message.iter().position(|b| *b == 0).unwrap_or(0);
    if end == 0 {
        return Err(String::from(crate::fs::error_text(r)));
    }
    Err(String::from_utf8_lossy(&message[..end]).into_owned())
}
