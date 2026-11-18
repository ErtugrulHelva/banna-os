//! Blok aygıtları: sürücü kaydı, MBR/GPT bölüm tablosu ve bölüm görünümleri.

pub mod ahci;
pub mod ata;
pub mod nvme;

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::kernel::initcall::Detail;
use crate::sync::SpinLock;

pub const SECTOR: usize = 512;

pub type Result<T> = core::result::Result<T, &'static str>;

pub trait BlockDevice: Send + Sync {
    fn name(&self) -> String;
    fn sectors(&self) -> u64;
    /// `buf.len()` 512'nin katı olmalıdır.
    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()>;
    fn write(&self, lba: u64, buf: &[u8]) -> Result<()>;
    fn model(&self) -> String {
        String::new()
    }
    /// Çıkarılabilir ortam (USB bellek): kök olarak bağlanmaz.
    fn removable(&self) -> bool {
        false
    }
    /// Aygıtın gerçek mantıksal blok boyutu (4Kn disklerde 4096); bölüm tablosundaki
    /// LBA'lar bu birimdedir. Arayüz her zaman 512 baytlık sektörlerle çalışır.
    fn block_size(&self) -> usize {
        SECTOR
    }
}

/// Büyük mantıksal bloklu (ör. 4Kn) aygıtı 512 baytlık sektör arayüzüyle sunar. `inner`
/// kendi blok birimiyle okunur/yazılır; hizasız yazmalar oku-değiştir-yaz yapılır.
pub struct Emulated512 {
    inner: Arc<dyn BlockDevice>,
    ratio: u64,
    lock: crate::sched::Mutex<()>,
}

impl Emulated512 {
    /// 512 baytlık aygıtları olduğu gibi döndürür.
    pub fn wrap(inner: Arc<dyn BlockDevice>) -> Arc<dyn BlockDevice> {
        let bs = inner.block_size();
        if bs == SECTOR {
            return inner;
        }
        crate::log!(
            "storage: {} uses {}-byte blocks, emulating 512-byte sectors",
            inner.name(),
            bs
        );
        Arc::new(Self {
            ratio: (bs / SECTOR) as u64,
            inner,
            lock: crate::sched::Mutex::new(()),
        })
    }

    fn span(&self, lba: u64, len: usize) -> (u64, u64) {
        let first = lba / self.ratio;
        let end = (lba + (len / SECTOR) as u64).div_ceil(self.ratio);
        (first, end)
    }
}

impl BlockDevice for Emulated512 {
    fn name(&self) -> String {
        self.inner.name()
    }

    fn sectors(&self) -> u64 {
        self.inner.sectors().saturating_mul(self.ratio)
    }

    fn model(&self) -> String {
        self.inner.model()
    }

    fn removable(&self) -> bool {
        self.inner.removable()
    }

    fn block_size(&self) -> usize {
        self.inner.block_size()
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let n = (buf.len() / SECTOR) as u64;
        if lba.is_multiple_of(self.ratio) && n.is_multiple_of(self.ratio) {
            return self.inner.read(lba / self.ratio, buf);
        }
        let (first, end) = self.span(lba, buf.len());
        let mut tmp = vec![0u8; ((end - first) * self.ratio) as usize * SECTOR];
        self.inner.read(first, &mut tmp)?;
        let at = (lba % self.ratio) as usize * SECTOR;
        buf.copy_from_slice(&tmp[at..at + buf.len()]);
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let n = (buf.len() / SECTOR) as u64;
        let _guard = self.lock.lock();
        if lba.is_multiple_of(self.ratio) && n.is_multiple_of(self.ratio) {
            return self.inner.write(lba / self.ratio, buf);
        }
        let (first, end) = self.span(lba, buf.len());
        let bs = self.ratio as usize * SECTOR;
        let mut tmp = vec![0u8; (end - first) as usize * bs];
        // Yalnızca kısmen yazılan uç bloklar okunur.
        self.inner.read(first, &mut tmp[..bs])?;
        if end - first > 1 {
            let last = tmp.len() - bs;
            self.inner.read(end - 1, &mut tmp[last..])?;
        }
        let at = (lba % self.ratio) as usize * SECTOR;
        tmp[at..at + buf.len()].copy_from_slice(buf);
        self.inner.write(first, &tmp)
    }
}

pub struct Partition {
    pub parent: Arc<dyn BlockDevice>,
    pub index: usize,
    pub start: u64,
    pub count: u64,
    pub kind: String,
}

/// GPT tipleri (diskteki bayt sırasıyla).
pub const ESP_TYPE: [u8; 16] = [
    0x28, 0x73, 0x2a, 0xc1, 0x1f, 0xf8, 0xd2, 0x11, 0xba, 0x4b, 0x00, 0xa0, 0xc9, 0x3e, 0xc9, 0x3b,
];
pub const LINUX_ROOT_TYPE: [u8; 16] = [
    0xe3, 0xbc, 0x68, 0x4f, 0xcd, 0xe8, 0xb1, 0x4d, 0x96, 0xe7, 0xfb, 0xca, 0xf9, 0x84, 0xb7, 0x09,
];

pub struct PartitionInfo {
    pub disk: String,
    pub type_guid: [u8; 16],
    pub unique: [u8; 16],
}

/// Birim bir GPT bölümüyse kimlik bilgileri.
pub fn partition_info(volume: &Arc<dyn BlockDevice>) -> Option<PartitionInfo> {
    PARTITIONS
        .lock()
        .iter()
        .find(|p| p.0 == volume.name())
        .map(|p| PartitionInfo {
            disk: p.1.clone(),
            type_guid: p.3,
            unique: p.4,
        })
}

/// GPT bölüm kimliklerinin metin biçimi (büyük harf, tireli).
pub fn guid_text(b: &[u8; 16]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3],
        b[2],
        b[1],
        b[0],
        b[5],
        b[4],
        b[7],
        b[6],
        b[8],
        b[9],
        b[10],
        b[11],
        b[12],
        b[13],
        b[14],
        b[15]
    )
}

/// (birim adı, disk adı, sıra, tip, kimlik)
type PartitionRecord = (String, String, usize, [u8; 16], [u8; 16]);
static PARTITIONS: SpinLock<Vec<PartitionRecord>> = SpinLock::new(Vec::new());

impl BlockDevice for Partition {
    fn name(&self) -> String {
        format!("{}p{}", self.parent.name(), self.index)
    }

    fn sectors(&self) -> u64 {
        self.count
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        if lba + (buf.len() / SECTOR) as u64 > self.count {
            return Err("read beyond partition");
        }
        self.parent.read(self.start + lba, buf)
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        if lba + (buf.len() / SECTOR) as u64 > self.count {
            return Err("write beyond partition");
        }
        self.parent.write(self.start + lba, buf)
    }

    fn model(&self) -> String {
        self.kind.clone()
    }

    fn removable(&self) -> bool {
        self.parent.removable()
    }

    fn block_size(&self) -> usize {
        self.parent.block_size()
    }
}

static DISKS: SpinLock<Vec<Arc<dyn BlockDevice>>> = SpinLock::new(Vec::new());
static VOLUMES: SpinLock<Vec<Arc<dyn BlockDevice>>> = SpinLock::new(Vec::new());

pub fn register_disk(disk: Arc<dyn BlockDevice>) -> Arc<dyn BlockDevice> {
    let disk = Emulated512::wrap(disk);
    crate::log!(
        "storage: {} '{}' {} MiB",
        disk.name(),
        disk.model(),
        disk.sectors() * SECTOR as u64 / (1024 * 1024)
    );
    DISKS.lock().push(disk.clone());
    disk
}

static READY: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Sürücülerin (ör. USB) eklediği disk: açılışta kaydedilir; açılıştan sonra takıldıysa
/// bölümleri taranıp `/usbN` gibi bir yola bağlanır.
pub fn add_disk(disk: Arc<dyn BlockDevice>) {
    let disk = register_disk(disk);
    if !READY.load(core::sync::atomic::Ordering::Acquire) {
        return;
    }
    let volumes = scan_partitions(&disk);
    for volume in &volumes {
        VOLUMES.lock().push(volume.clone());
    }
    for volume in volumes {
        crate::fs::volume_added(volume);
    }
}

/// Bölüm tablosunu yeniden okur (bölümleme aracından sonra). Diskin birimlerinden
/// biri bağlıysa reddedilir.
pub fn rescan(disk_name: &str) -> core::result::Result<usize, &'static str> {
    let disk = disks()
        .into_iter()
        .find(|d| d.name() == disk_name)
        .ok_or("no such disk")?;
    let prefix = format!("{}p", disk_name);
    let ours = |name: &str| name == disk_name || name.starts_with(prefix.as_str());
    if crate::fs::mounts().iter().any(|m| ours(&m.device)) {
        return Err("a volume of this disk is mounted");
    }
    VOLUMES.lock().retain(|v| !ours(&v.name()));
    PARTITIONS.lock().retain(|p| p.1 != disk_name);
    let volumes = scan_partitions(&disk);
    let count = volumes.len();
    VOLUMES.lock().extend(volumes.iter().cloned());
    crate::log!("storage: {} rescanned, {} volume(s)", disk_name, count);
    for volume in volumes {
        crate::fs::volume_added(volume);
    }
    Ok(count)
}

/// Çıkarılan diski ve bölümlerini kaldırır, bağlı dosya sistemlerini ayırır.
pub fn remove_disk(name: &str) {
    DISKS.lock().retain(|d| d.name() != name);
    let gone: Vec<String> = VOLUMES
        .lock()
        .iter()
        .map(|v| v.name())
        .filter(|v| v == name || v.starts_with(&format!("{}p", name)))
        .collect();
    VOLUMES.lock().retain(|v| !gone.contains(&v.name()));
    PARTITIONS.lock().retain(|p| p.1 != name);
    let mut mounts = crate::fs::mounts();
    mounts.sort_by_key(|m| core::cmp::Reverse(m.point.len()));
    for m in mounts {
        if gone.contains(&m.device) {
            let _ = crate::fs::force_unmount(&m.point);
            crate::log!("storage: {} removed, {} unmounted", m.device, m.point);
        }
    }
}

pub fn disks() -> Vec<Arc<dyn BlockDevice>> {
    DISKS.lock().clone()
}

/// Dosya sistemi barındırabilecek birimler (bölümler ya da bölümsüz diskler).
pub fn volumes() -> Vec<Arc<dyn BlockDevice>> {
    VOLUMES.lock().clone()
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

fn mbr_kind(id: u8) -> &'static str {
    match id {
        0x0b | 0x0c => "FAT32",
        0x06 | 0x0e => "FAT16",
        0x07 => "NTFS/exFAT",
        0x83 => "Linux",
        0xee => "GPT protective",
        _ => "unknown",
    }
}

/// MBR ya da GPT bölümlerini bulur; tablo yoksa diskin tamamı tek birimdir.
fn scan_partitions(disk: &Arc<dyn BlockDevice>) -> Vec<Arc<dyn BlockDevice>> {
    let mut sector = vec![0u8; SECTOR];
    if disk.read(0, &mut sector).is_err() {
        return Vec::new();
    }
    let has_signature = sector[510] == 0x55 && sector[511] == 0xaa;
    // Bölümsüz FAT birimi: önyükleme sektöründe BPB ve "FAT" imzası (MBR önyükleme
    // kodu da 0xEB ile başlayabildiğinden yalnızca ilk bayta bakılmaz).
    let looks_like_bpb = &sector[82..85] == b"FAT" || &sector[54..57] == b"FAT";
    let mut out: Vec<Arc<dyn BlockDevice>> = Vec::new();

    if has_signature && !looks_like_bpb {
        let gpt = (0..4).any(|i| sector[446 + i * 16 + 4] == 0xee);
        // Bölüm tablosundaki LBA'lar aygıtın kendi blok birimindedir.
        let f = (disk.block_size() / SECTOR) as u64;
        let blocks = disk.sectors() / f;
        if gpt {
            let mut header = vec![0u8; SECTOR];
            if disk.read(f, &mut header).is_ok() && &header[0..8] == b"EFI PART" {
                let entries_lba = le64(&header, 72);
                let count = le32(&header, 80).min(128) as usize;
                let size = le32(&header, 84) as usize;
                let readable = (128..=4096).contains(&size) && entries_lba < blocks;
                let mut table = vec![0u8; (count * size.min(4096)).div_ceil(SECTOR) * SECTOR];
                if readable && disk.read(entries_lba * f, &mut table).is_ok() {
                    for i in 0..count {
                        let e = &table[i * size..(i + 1) * size];
                        if e[0..16].iter().all(|&b| b == 0) {
                            continue;
                        }
                        let first = le64(e, 32);
                        let last = le64(e, 40);
                        if last < first || last >= blocks {
                            continue;
                        }
                        let type_guid: [u8; 16] = e[0..16].try_into().unwrap();
                        let unique: [u8; 16] = e[16..32].try_into().unwrap();
                        let kind = if type_guid == ESP_TYPE {
                            "EFI system"
                        } else if type_guid == LINUX_ROOT_TYPE {
                            "Linux root"
                        } else {
                            "GPT"
                        };
                        let partition = Partition {
                            parent: disk.clone(),
                            index: i + 1,
                            start: first * f,
                            count: (last - first + 1) * f,
                            kind: String::from(kind),
                        };
                        PARTITIONS.lock().push((
                            partition.name(),
                            disk.name(),
                            i + 1,
                            type_guid,
                            unique,
                        ));
                        out.push(Arc::new(partition));
                    }
                }
            }
        } else {
            for i in 0..4 {
                let e = &sector[446 + i * 16..446 + (i + 1) * 16];
                let id = e[4];
                let start = le32(e, 8) as u64;
                let count = le32(e, 12) as u64;
                if id == 0 || count == 0 || start + count > blocks {
                    continue;
                }
                out.push(Arc::new(Partition {
                    parent: disk.clone(),
                    index: i + 1,
                    start: start * f,
                    count: count * f,
                    kind: String::from(mbr_kind(id)),
                }));
            }
        }
    }
    if out.is_empty() {
        out.push(disk.clone());
    }
    out
}

fn self_test() -> bool {
    let disks = disks();
    disks.iter().all(|disk| {
        let mut a = vec![0u8; SECTOR * 2];
        let mut b = vec![0u8; SECTOR * 2];
        disk.read(0, &mut a).is_ok() && disk.read(0, &mut b).is_ok() && a == b
    })
}

fn init(detail: &mut Detail) -> Result<()> {
    ata::probe();
    ahci::probe();
    nvme::probe();
    let disks = disks();
    let mut volumes = Vec::new();
    for disk in &disks {
        for volume in scan_partitions(disk) {
            crate::log!(
                "storage: volume {} ({}) {} sectors",
                volume.name(),
                volume.model(),
                volume.sectors()
            );
            volumes.push(volume);
        }
    }
    crate::kernel::initcall::self_test("storage", self_test())?;
    READY.store(true, core::sync::atomic::Ordering::Release);
    let _ = write!(
        detail,
        "{} disk(s), {} volume(s)",
        disks.len(),
        volumes.len()
    );
    *VOLUMES.lock() = volumes;
    Ok(())
}

crate::initcall!(driver, 30, "Storage (ATA/AHCI/NVMe)", init);
