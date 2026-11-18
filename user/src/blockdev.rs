//! `/dev/<aygıt>` üzerinden sektör erişimi (yalnızca root) ve ext2 biçimlendirici bağlantısı.

use crate::fs::{self, File};
use core::cell::RefCell;
use core::sync::atomic::{AtomicU64, Ordering};

pub const SECTOR: u64 = 512;
const CHUNK: usize = 64 * 1024;

pub struct BlockFile {
    file: RefCell<File>,
    sectors: u64,
    block: u64,
}

impl BlockFile {
    /// `device`: `usb1`, `usb1p2` ya da `/dev/usb1p2`.
    pub fn open(device: &str) -> Result<Self, i64> {
        let path = if device.starts_with("/dev/") {
            crate::String::from(device)
        } else {
            crate::format!("/dev/{}", device)
        };
        let stat = fs::stat(&path)?;
        if stat.kind != fs::KIND_DEVICE {
            return Err(-19);
        }
        let file = File::open(&path, fs::READ | fs::WRITE)?;
        Ok(Self {
            file: RefCell::new(file),
            sectors: stat.size / SECTOR,
            block: (stat.block as u64).clamp(SECTOR, 4096),
        })
    }

    pub fn sectors(&self) -> u64 {
        self.sectors
    }

    /// Aygıtın mantıksal blok boyutu; GPT LBA'ları ve FAT sektörleri bu birimdedir.
    pub fn block_size(&self) -> u64 {
        self.block
    }

    pub fn read_at(&self, lba: u64, buf: &mut [u8]) -> Result<(), i64> {
        let mut file = self.file.borrow_mut();
        let mut done = 0;
        while done < buf.len() {
            file.seek(lba * SECTOR + done as u64);
            let end = (done + CHUNK).min(buf.len());
            let n = file.read(&mut buf[done..end])?;
            if n == 0 {
                return Err(-5);
            }
            done += n;
        }
        Ok(())
    }

    pub fn write_at(&self, lba: u64, data: &[u8]) -> Result<(), i64> {
        let mut file = self.file.borrow_mut();
        let mut done = 0;
        while done < data.len() {
            file.seek(lba * SECTOR + done as u64);
            let end = (done + CHUNK).min(data.len());
            let n = file.write(&data[done..end])?;
            if n == 0 {
                return Err(-5);
            }
            done += n;
        }
        Ok(())
    }
}

impl crate::ext2::Device for BlockFile {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        self.read_at(lba, buf).map_err(|_| ())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        self.write_at(lba, buf).map_err(|_| ())
    }
}

/// Rastgele 16 bayt (zaman, pid ve sayaçtan SHA-256).
pub fn random16() -> [u8; 16] {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut seed = [0u8; 24];
    seed[..8].copy_from_slice(&crate::time::now_ns().to_le_bytes());
    seed[8..16].copy_from_slice(&crate::process::pid().to_le_bytes());
    seed[16..].copy_from_slice(&counter.to_le_bytes());
    let digest = crate::crypto::sha256(&seed);
    let mut out = [0u8; 16];
    out.copy_from_slice(&digest[..16]);
    out
}

/// Aygıtı ext2 ile biçimlendirir.
pub fn format_ext2(device: &BlockFile, label: &str) -> Result<(), crate::ext2::Error> {
    crate::ext2::format(
        device,
        &crate::ext2::FormatOptions {
            label,
            uuid: random16(),
            now: crate::time::unix_time() as u32,
        },
    )
}
