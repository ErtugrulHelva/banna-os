//! Dosya sistemi erişimi.

use crate::sys::{self, Dirent, Stat};
use crate::{String, Vec};

pub const READ: u64 = 1;
pub const WRITE: u64 = 2;
pub const CREATE: u64 = 4;
pub const TRUNCATE: u64 = 8;
pub const APPEND: u64 = 16;
pub const DIRECTORY: u64 = 32;

pub const KIND_FILE: u32 = 0;
pub const KIND_DIRECTORY: u32 = 1;
pub const KIND_DEVICE: u32 = 2;

pub const ENOENT: i64 = -2;

pub fn error_text(code: i64) -> &'static str {
    match -code {
        1 => "operation not permitted",
        2 => "no such file or directory",
        3 => "no such process",
        7 => "argument list too long",
        8 => "not an executable",
        12 => "out of memory",
        24 => "too many open files",
        32 => "broken pipe",
        5 => "I/O error",
        9 => "bad file descriptor",
        13 => "permission denied",
        14 => "bad address",
        17 => "already exists",
        20 => "not a directory",
        21 => "is a directory",
        22 => "invalid argument",
        16 => "device or resource busy",
        28 => "no space left on device",
        30 => "read-only file system",
        95 => "operation not supported",
        39 => "directory not empty",
        _ => "error",
    }
}

pub struct File {
    fd: u64,
}

impl File {
    pub fn open(path: &str, flags: u64) -> Result<Self, i64> {
        let fd = sys::call(sys::OPEN, &[path.as_ptr() as u64, path.len() as u64, flags]);
        if fd < 0 {
            Err(fd)
        } else {
            Ok(Self { fd: fd as u64 })
        }
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, i64> {
        let n = sys::call(
            sys::READ,
            &[self.fd, buf.as_mut_ptr() as u64, buf.len() as u64],
        );
        if n < 0 { Err(n) } else { Ok(n as usize) }
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize, i64> {
        let n = sys::call(
            sys::WRITE,
            &[self.fd, data.as_ptr() as u64, data.len() as u64],
        );
        if n < 0 { Err(n) } else { Ok(n as usize) }
    }

    /// Kısmi yazmaları (çekirdek tek çağrıda en fazla 64 KiB yazar) tamamlar.
    pub fn write_all(&mut self, mut data: &[u8]) -> Result<(), i64> {
        while !data.is_empty() {
            match self.write(data)? {
                0 => return Err(-28),
                n => data = &data[n..],
            }
        }
        Ok(())
    }

    pub fn seek(&mut self, position: u64) -> i64 {
        sys::call(sys::SEEK, &[self.fd, position])
    }

    pub fn read_to_end(&mut self) -> Result<Vec<u8>, i64> {
        let mut out = Vec::new();
        let mut buf = crate::vec![0u8; 64 * 1024];
        loop {
            let n = self.read(&mut buf)?;
            if n == 0 {
                return Ok(out);
            }
            out.extend_from_slice(&buf[..n]);
        }
    }

    pub fn entry(&self, index: usize) -> Option<(String, Stat)> {
        let mut dirent = Dirent {
            kind: 0,
            name_len: 0,
            size: 0,
            mode: 0,
            uid: 0,
            gid: 0,
            mtime: 0,
            name: [0; 256],
        };
        let r = sys::call(
            sys::READDIR,
            &[self.fd, index as u64, &mut dirent as *mut _ as u64],
        );
        if r <= 0 {
            return None;
        }
        let name = String::from_utf8_lossy(&dirent.name[..dirent.name_len as usize]).into_owned();
        Some((
            name,
            Stat {
                kind: dirent.kind,
                read_only: 0,
                size: dirent.size,
                mode: dirent.mode,
                uid: dirent.uid,
                gid: dirent.gid,
                mtime: dirent.mtime,
                ..Stat::default()
            },
        ))
    }
}

impl Drop for File {
    fn drop(&mut self) {
        sys::call(sys::CLOSE, &[self.fd]);
    }
}

pub fn read(path: &str) -> Result<Vec<u8>, i64> {
    File::open(path, READ)?.read_to_end()
}

pub fn write(path: &str, data: &[u8]) -> Result<(), i64> {
    File::open(path, WRITE | CREATE | TRUNCATE)?.write_all(data)
}

pub fn stat(path: &str) -> Result<Stat, i64> {
    let mut stat = Stat::default();
    let r = sys::call(
        sys::STAT,
        &[
            path.as_ptr() as u64,
            path.len() as u64,
            &mut stat as *mut _ as u64,
        ],
    );
    if r < 0 { Err(r) } else { Ok(stat) }
}

pub fn read_dir(path: &str) -> Result<Vec<(String, Stat)>, i64> {
    let dir = File::open(path, READ | DIRECTORY)?;
    let mut out = Vec::new();
    while let Some(entry) = dir.entry(out.len()) {
        out.push(entry);
    }
    Ok(out)
}

pub fn mkdir(path: &str) -> Result<(), i64> {
    let r = sys::call(sys::MKDIR, &[path.as_ptr() as u64, path.len() as u64]);
    if r < 0 { Err(r) } else { Ok(()) }
}

pub fn remove(path: &str) -> Result<(), i64> {
    let r = sys::call(sys::UNLINK, &[path.as_ptr() as u64, path.len() as u64]);
    if r < 0 { Err(r) } else { Ok(()) }
}

/// `.` ve `..` bileşenlerini çözümleyip `cwd`'ye göre mutlak yol üretir.
pub fn normalize(cwd: &str, path: &str) -> String {
    let joined = if path.starts_with('/') {
        String::from(path)
    } else {
        crate::format!("{}/{}", cwd, path)
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            p => parts.push(p),
        }
    }
    let mut out = String::from("/");
    out.push_str(&parts.join("/"));
    out
}

fn path_call(n: u64, path: &str, extra: &[u64]) -> Result<i64, i64> {
    let mut args = alloc::vec![path.as_ptr() as u64, path.len() as u64];
    args.extend_from_slice(extra);
    let r = sys::call(n, &args);
    if r < 0 { Err(r) } else { Ok(r) }
}

/// Taşır / yeniden adlandırır (aynı birim içinde).
pub fn rename(from: &str, to: &str) -> Result<(), i64> {
    path_call(sys::RENAME, from, &[to.as_ptr() as u64, to.len() as u64]).map(|_| ())
}

pub fn chmod(path: &str, mode: u32) -> Result<(), i64> {
    path_call(sys::CHMOD, path, &[mode as u64]).map(|_| ())
}

pub fn chown(path: &str, uid: u32, gid: u32) -> Result<(), i64> {
    path_call(sys::CHOWN, path, &[uid as u64, gid as u64]).map(|_| ())
}

/// `device`: `/dev/usb0p1` ya da `usb0p1`.
pub fn mount(device: &str, point: &str) -> Result<(), i64> {
    path_call(
        sys::MOUNT,
        device,
        &[point.as_ptr() as u64, point.len() as u64],
    )
    .map(|_| ())
}

pub fn umount(point: &str) -> Result<(), i64> {
    path_call(sys::UMOUNT, point, &[]).map(|_| ())
}

/// Diskin bölüm tablosunu yeniden okutur; birim sayısını döndürür.
pub fn rescan(disk: &str) -> Result<usize, i64> {
    path_call(sys::RESCAN, disk, &[]).map(|n| n as usize)
}

/// Yolu içeren birimin (toplam, boş) baytları.
pub fn statfs(path: &str) -> Result<(u64, u64), i64> {
    let mut out = [0u64; 2];
    path_call(sys::STATFS, path, &[out.as_mut_ptr() as u64])?;
    Ok((out[0], out[1]))
}
