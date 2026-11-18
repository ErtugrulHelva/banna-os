//! Sanal dosya sistemi: ext2 (Unix izinleri) ve FAT32 birimleri, `/dev` aygıtları.
//!
//! Kök (`/`): çekirdek komut satırındaki `root=PARTUUID=<guid>`, yoksa GPT'de Linux kök
//! tipi bölüm, o da yoksa ilk sabit disk birimi. Kökle aynı diskteki EFI bölümü `/boot`'a,
//! diğer birimler `/media/<aygıt>` altına bağlanır.

pub mod exfat;
// Ana makine aracı ve kullanıcı alanı da derler; her parçayı çekirdek kullanmaz.
#[allow(dead_code)]
pub mod ext2;
pub mod fat32;
pub mod vfs;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::kernel::initcall::Detail;
use crate::sched::Mutex;
use crate::storage::{self, BlockDevice};
use crate::sync::SpinLock;
use exfat::Exfat;
use fat32::Fat32;
pub use vfs::{File, OpenFlags};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FsError {
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    NoSpace,
    InvalidPath,
    PermissionDenied,
    NotSupported,
    NotMounted,
    ReadOnly,
    Busy,
    Corrupt,
    Io,
}

impl FsError {
    /// Syscall dönüşleri için POSIX benzeri hata kodu.
    pub fn errno(self) -> i64 {
        -(match self {
            FsError::NotFound | FsError::NotMounted => 2,
            FsError::Io | FsError::Corrupt => 5,
            FsError::PermissionDenied => 13,
            FsError::Busy => 16,
            FsError::Exists => 17,
            FsError::NotDirectory => 20,
            FsError::IsDirectory => 21,
            FsError::InvalidPath => 22,
            FsError::NoSpace => 28,
            FsError::ReadOnly => 30,
            FsError::NotEmpty => 39,
            FsError::NotSupported => 95,
        })
    }

    pub fn message(self) -> &'static str {
        match self {
            FsError::NotFound => "no such file or directory",
            FsError::Exists => "already exists",
            FsError::NotDirectory => "not a directory",
            FsError::IsDirectory => "is a directory",
            FsError::NotEmpty => "directory not empty",
            FsError::NoSpace => "no space left on device",
            FsError::InvalidPath => "invalid path",
            FsError::PermissionDenied => "permission denied",
            FsError::NotSupported => "operation not supported",
            FsError::NotMounted => "no file system mounted",
            FsError::ReadOnly => "read-only file system",
            FsError::Busy => "device or resource busy",
            FsError::Corrupt => "file system is corrupt",
            FsError::Io => "I/O error",
        }
    }
}

impl From<ext2::Error> for FsError {
    fn from(e: ext2::Error) -> Self {
        match e {
            ext2::Error::NotFound => FsError::NotFound,
            ext2::Error::Exists => FsError::Exists,
            ext2::Error::NotDirectory => FsError::NotDirectory,
            ext2::Error::IsDirectory => FsError::IsDirectory,
            ext2::Error::NotEmpty => FsError::NotEmpty,
            ext2::Error::NoSpace => FsError::NoSpace,
            ext2::Error::InvalidPath | ext2::Error::NameTooLong => FsError::InvalidPath,
            ext2::Error::Unsupported => FsError::NotSupported,
            ext2::Error::ReadOnly => FsError::ReadOnly,
            ext2::Error::Corrupt => FsError::Corrupt,
            ext2::Error::Io => FsError::Io,
        }
    }
}

pub type Result<T> = core::result::Result<T, FsError>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Directory,
    Device,
}

#[derive(Clone, Debug)]
pub struct Metadata {
    pub name: String,
    pub kind: Kind,
    pub size: u64,
    pub read_only: bool,
    /// Unix izin bitleri (0o7777).
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    /// Son değişiklik (Unix zamanı; FAT32'de 0).
    pub mtime: u32,
}

/// ext2 sürücüsünün gördüğü sektör aygıtı.
pub struct Disk(pub Arc<dyn BlockDevice>);

impl ext2::Device for Disk {
    fn sectors(&self) -> u64 {
        self.0.sectors()
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> core::result::Result<(), ()> {
        self.0.read(lba, buf).map_err(|_| ())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> core::result::Result<(), ()> {
        self.0.write(lba, buf).map_err(|_| ())
    }
}

pub fn unix_now() -> u32 {
    crate::drivers::rtc::now()
        .map(|t| t.unix_timestamp() as u32)
        .unwrap_or(0)
}

pub struct Ext2Volume {
    pub fs: Mutex<ext2::Ext2<Disk>>,
    pub label: String,
}

#[derive(Clone)]
pub enum Volume {
    Fat(Arc<Fat32>),
    Exfat(Arc<Exfat>),
    Ext2(Arc<Ext2Volume>),
}

impl Volume {
    pub fn kind(&self) -> &'static str {
        match self {
            Volume::Fat(_) => "fat32",
            Volume::Exfat(_) => "exfat",
            Volume::Ext2(_) => "ext2",
        }
    }

    pub fn label(&self) -> String {
        match self {
            Volume::Fat(f) => f.label.clone(),
            Volume::Exfat(f) => f.label.clone(),
            Volume::Ext2(e) => e.label.clone(),
        }
    }

    pub fn capacity(&self) -> (u64, u64) {
        match self {
            Volume::Fat(f) => f.capacity(),
            Volume::Exfat(f) => f.capacity(),
            Volume::Ext2(e) => e.fs.lock().capacity(),
        }
    }

    pub fn is_ext2(&self) -> bool {
        matches!(self, Volume::Ext2(_))
    }
}

fn ext2_meta(name: &str, inode: &ext2::Inode) -> Metadata {
    Metadata {
        name: String::from(name),
        kind: if inode.is_dir() {
            Kind::Directory
        } else {
            Kind::File
        },
        size: inode.size,
        read_only: false,
        mode: inode.permissions(),
        uid: inode.uid,
        gid: inode.gid,
        mtime: inode.mtime,
    }
}

struct Mount {
    point: String,
    device: String,
    volume: Volume,
}

pub struct MountInfo {
    pub point: String,
    pub device: String,
    pub kind: &'static str,
    pub label: String,
}

static MOUNTS: SpinLock<Vec<Mount>> = SpinLock::new(Vec::new());
/// Kök henüz bağlanmadıysa beklenen bölüm (`root=PARTUUID=`).
static ROOT_SPEC: SpinLock<Option<[u8; 16]>> = SpinLock::new(None);

fn last_name(path: &str) -> String {
    String::from(
        path.rsplit('/')
            .next()
            .filter(|n| !n.is_empty())
            .unwrap_or("/"),
    )
}

/// Mutlak yolu en uzun eşleşen bağlama noktasına ve o birim içindeki yola çevirir.
pub fn resolve(path: &str) -> Result<(Volume, String)> {
    let mounts = MOUNTS.lock();
    let best = mounts
        .iter()
        .filter(|m| {
            m.point == "/"
                || path == m.point
                || path
                    .strip_prefix(m.point.as_str())
                    .is_some_and(|r| r.starts_with('/'))
        })
        .max_by_key(|m| m.point.len())
        .ok_or(FsError::NotMounted)?;
    let inner = if best.point == "/" {
        String::from(path)
    } else {
        let rest = &path[best.point.len()..];
        if rest.is_empty() {
            String::from("/")
        } else {
            String::from(rest)
        }
    };
    Ok((best.volume.clone(), inner))
}

/// Birimi açar: ext2, exFAT ve FAT32 sırayla denenir.
pub fn open_volume(device: Arc<dyn BlockDevice>) -> Result<Volume> {
    match ext2::Ext2::mount(Disk(device.clone()), unix_now) {
        Ok(fs) => {
            let label = fs.label.clone();
            return Ok(Volume::Ext2(Arc::new(Ext2Volume {
                fs: Mutex::new(fs),
                label,
            })));
        }
        Err(ext2::Error::Unsupported) => {}
        Err(e) => return Err(e.into()),
    }
    match Exfat::mount(device.clone()) {
        Ok(fs) => return Ok(Volume::Exfat(Arc::new(fs))),
        Err(FsError::NotSupported) => {}
        Err(e) => return Err(e),
    }
    Ok(Volume::Fat(Arc::new(Fat32::mount(device)?)))
}

pub fn mount(point: &str, device: &str, volume: Volume) -> Result<()> {
    let mut mounts = MOUNTS.lock();
    if mounts.iter().any(|m| m.point == point) {
        return Err(FsError::Busy);
    }
    if mounts.iter().any(|m| m.device == device) {
        return Err(FsError::Busy);
    }
    mounts.push(Mount {
        point: String::from(point),
        device: String::from(device),
        volume,
    });
    Ok(())
}

pub fn unmount(point: &str) -> Result<()> {
    let mut mounts = MOUNTS.lock();
    if point == "/" {
        return Err(FsError::Busy);
    }
    let inner = alloc::format!("{}/", point);
    if mounts.iter().any(|m| m.point.starts_with(inner.as_str())) {
        return Err(FsError::Busy);
    }
    let before = mounts.len();
    mounts.retain(|m| m.point != point);
    if mounts.len() == before {
        return Err(FsError::NotMounted);
    }
    Ok(())
}

/// Aygıt çıkarıldığında: bağlama noktası koşulsuz kaldırılır.
pub fn force_unmount(point: &str) -> Result<()> {
    let mut mounts = MOUNTS.lock();
    let before = mounts.len();
    mounts.retain(|m| m.point != point);
    if mounts.len() == before {
        return Err(FsError::NotMounted);
    }
    Ok(())
}

pub fn mounts() -> Vec<MountInfo> {
    MOUNTS
        .lock()
        .iter()
        .map(|m| MountInfo {
            point: m.point.clone(),
            device: m.device.clone(),
            kind: m.volume.kind(),
            label: m.volume.label(),
        })
        .collect()
}

pub fn mount_of(device: &str) -> Option<String> {
    MOUNTS
        .lock()
        .iter()
        .find(|m| m.device == device)
        .map(|m| m.point.clone())
}

pub fn is_mount_parent(path: &str) -> bool {
    !child_mounts(path).is_empty()
}

/// `path` dizininin doğrudan altındaki bağlama noktalarının adları.
fn child_mounts(path: &str) -> Vec<String> {
    let prefix = if path == "/" {
        String::from("/")
    } else {
        alloc::format!("{}/", path)
    };
    MOUNTS
        .lock()
        .iter()
        .filter_map(|m| {
            let rest = m.point.strip_prefix(prefix.as_str())?;
            let first = rest.split('/').next()?;
            (!first.is_empty()).then(|| String::from(first))
        })
        .collect()
}

/// Birimi açıp `point` noktasına bağlar; ara dizinler yoksa (bağlama noktası olarak) görünür.
pub fn mount_volume(volume: Arc<dyn BlockDevice>, point: &str) -> Result<String> {
    let opened = open_volume(volume.clone())?;
    let label = opened.label();
    let kind = opened.kind();
    mount(point, &volume.name(), opened)?;
    crate::log!(
        "fs: mounted {} ({} '{}') at {}",
        volume.name(),
        kind,
        label,
        point
    );
    Ok(label)
}

/// `.` ve `..` bileşenlerini çözümleyip mutlak yol üretir.
pub fn normalize(cwd: &str, path: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    let joined = if path.starts_with('/') {
        String::from(path)
    } else {
        alloc::format!("{}/{}", cwd, path)
    };
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

// ---- /dev ----

/// `/dev/<ad>` → aygıt; `null` ve `zero` özel dosyalardır.
pub enum DevNode {
    Null,
    Zero,
    /// Ses çıkışı: 48 kHz, 16 bit, stereo s16le.
    Dsp,
    Block(Arc<dyn BlockDevice>),
}

pub fn device_node(path: &str) -> Option<DevNode> {
    let name = path.strip_prefix("/dev/")?;
    match name {
        "null" => Some(DevNode::Null),
        "zero" => Some(DevNode::Zero),
        "dsp" if crate::drivers::audio::present() => Some(DevNode::Dsp),
        _ => block_device(name).map(DevNode::Block),
    }
}

pub fn block_device(name: &str) -> Option<Arc<dyn BlockDevice>> {
    storage::disks()
        .into_iter()
        .chain(storage::volumes())
        .find(|d| d.name() == name)
}

fn device_meta(name: &str, node: &DevNode) -> Metadata {
    let (size, mode) = match node {
        DevNode::Block(d) => (d.sectors() * storage::SECTOR as u64, 0o600),
        _ => (0, 0o666),
    };
    Metadata {
        name: String::from(name),
        kind: Kind::Device,
        size,
        read_only: false,
        mode,
        uid: 0,
        gid: 0,
        mtime: 0,
    }
}

fn dev_dir_meta() -> Metadata {
    Metadata {
        name: String::from("dev"),
        kind: Kind::Directory,
        size: 0,
        read_only: true,
        mode: 0o755,
        uid: 0,
        gid: 0,
        mtime: 0,
    }
}

fn dev_listing() -> Vec<Metadata> {
    let mut list = alloc::vec![
        device_meta("null", &DevNode::Null),
        device_meta("zero", &DevNode::Zero),
    ];
    if crate::drivers::audio::present() {
        list.push(device_meta("dsp", &DevNode::Dsp));
    }
    let mut names: Vec<Arc<dyn BlockDevice>> = storage::disks();
    for v in storage::volumes() {
        if !names.iter().any(|d| d.name() == v.name()) {
            names.push(v);
        }
    }
    for d in names {
        list.push(device_meta(&d.name(), &DevNode::Block(d.clone())));
    }
    list
}

// ---- meta veri ----

/// `/dev/<disk>` için aygıtın mantıksal blok boyutu; diğer yollar için 512.
pub fn device_block_size(path: &str) -> usize {
    match device_node(path) {
        Some(DevNode::Block(d)) => d.block_size(),
        _ => storage::SECTOR,
    }
}

pub fn stat(path: &str) -> Result<Metadata> {
    if path == "/dev" {
        return Ok(dev_dir_meta());
    }
    if path.starts_with("/dev/") {
        let node = device_node(path).ok_or(FsError::NotFound)?;
        return Ok(device_meta(&last_name(path), &node));
    }
    let (volume, inner) = resolve(path)?;
    let found = match &volume {
        Volume::Fat(fs) => fs.lookup(&inner).map(|e| e.metadata()),
        Volume::Exfat(fs) => fs.lookup(&inner).map(|e| e.metadata()),
        Volume::Ext2(e) => {
            let mut fs = e.fs.lock();
            fs.lookup(&inner)
                .and_then(|ino| fs.inode(ino))
                .map(|inode| ext2_meta(&last_name(&inner), &inode))
                .map_err(Into::into)
        }
    };
    let mut meta = match found {
        Ok(meta) => meta,
        // Diskte olmayan ara dizin, altında bağlama noktası varsa sanal olarak görünür.
        Err(FsError::NotFound) if is_mount_parent(path) => {
            let mut meta = dev_dir_meta();
            meta.name = last_name(path);
            return Ok(meta);
        }
        Err(e) => return Err(e),
    };
    if inner == "/" {
        meta.name = last_name(path);
    }
    Ok(meta)
}

pub fn read_dir(path: &str) -> Result<Vec<Metadata>> {
    if path == "/dev" {
        return Ok(dev_listing());
    }
    let (volume, inner) = resolve(path)?;
    let listed: Result<Vec<Metadata>> = match &volume {
        Volume::Fat(fs) => fs.lookup(&inner).and_then(|dir| {
            Ok(fs
                .read_dir(&dir)?
                .into_iter()
                .filter(|e| e.name != "." && e.name != "..")
                .map(|e| e.metadata())
                .collect())
        }),
        Volume::Exfat(fs) => fs.lookup(&inner).and_then(|dir| {
            Ok(fs
                .read_dir(&dir)?
                .into_iter()
                .map(|e| e.metadata())
                .collect())
        }),
        Volume::Ext2(e) => (|| {
            let mut fs = e.fs.lock();
            let dir = fs.lookup(&inner)?;
            let entries = fs.read_dir(dir)?;
            let mut out = Vec::new();
            for entry in entries {
                if entry.name == "." || entry.name == ".." {
                    continue;
                }
                let inode = fs.inode(entry.inode)?;
                out.push(ext2_meta(&entry.name, &inode));
            }
            Ok(out)
        })(),
    };
    let mut list = match listed {
        Ok(list) => list,
        Err(FsError::NotFound) if is_mount_parent(path) => Vec::new(),
        Err(e) => return Err(e),
    };
    let mut extra = child_mounts(path);
    if path == "/" {
        extra.push(String::from("dev"));
    }
    for name in extra {
        if !list.iter().any(|m| m.name.eq_ignore_ascii_case(&name)) {
            let mut meta = dev_dir_meta();
            meta.name = name;
            list.push(meta);
        }
    }
    Ok(list)
}

pub fn read_all(path: &str) -> Result<Vec<u8>> {
    let mut file = File::open(path, OpenFlags::READ, ROOT_OWNER)?;
    let mut data = alloc::vec![0u8; file.size() as usize];
    let n = file.read(&mut data)?;
    data.truncate(n);
    Ok(data)
}

pub fn write_all(path: &str, data: &[u8]) -> Result<()> {
    let mut file = File::open(
        path,
        OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::TRUNCATE,
        ROOT_OWNER,
    )?;
    file.write(data)?;
    Ok(())
}

/// Oluşturulan dosya ve dizinlerin sahibi (uid, gid).
#[derive(Clone, Copy)]
pub struct Owner {
    pub uid: u32,
    pub gid: u32,
}

pub const ROOT_OWNER: Owner = Owner { uid: 0, gid: 0 };

pub fn mkdir_as(path: &str, owner: Owner, mode: u16) -> Result<()> {
    if path.starts_with("/dev") {
        return Err(FsError::PermissionDenied);
    }
    let (volume, inner) = resolve(path)?;
    match volume {
        Volume::Fat(fs) => fs.create(&inner, true).map(|_| ()),
        Volume::Exfat(fs) => fs.create(&inner, true).map(|_| ()),
        Volume::Ext2(e) => {
            e.fs.lock()
                .create(
                    &inner,
                    ext2::S_IFDIR | (mode & 0o7777),
                    owner.uid,
                    owner.gid,
                )
                .map(|_| ())
                .map_err(Into::into)
        }
    }
}

pub fn mkdir(path: &str) -> Result<()> {
    mkdir_as(path, ROOT_OWNER, 0o755)
}

pub fn remove(path: &str) -> Result<()> {
    if path.starts_with("/dev") {
        return Err(FsError::PermissionDenied);
    }
    if MOUNTS.lock().iter().any(|m| m.point == path) {
        return Err(FsError::Busy);
    }
    let (volume, inner) = resolve(path)?;
    if inner == "/" {
        return Err(FsError::Busy);
    }
    match volume {
        Volume::Fat(fs) => fs.remove(&inner),
        Volume::Exfat(fs) => fs.remove(&inner),
        Volume::Ext2(e) => e.fs.lock().remove(&inner).map_err(Into::into),
    }
}

/// Taşır / yeniden adlandırır (aynı birim içinde). FAT32'de yalnızca dosyalar (kopyalayarak).
pub fn rename(from: &str, to: &str) -> Result<()> {
    if from.starts_with("/dev") || to.starts_with("/dev") {
        return Err(FsError::PermissionDenied);
    }
    let (a, inner_from) = resolve(from)?;
    let (b, inner_to) = resolve(to)?;
    let same = match (&a, &b) {
        (Volume::Fat(x), Volume::Fat(y)) => Arc::ptr_eq(x, y),
        (Volume::Exfat(x), Volume::Exfat(y)) => Arc::ptr_eq(x, y),
        (Volume::Ext2(x), Volume::Ext2(y)) => Arc::ptr_eq(x, y),
        _ => false,
    };
    if !same {
        return Err(FsError::NotSupported);
    }
    if inner_from == "/" || inner_to == "/" {
        return Err(FsError::Busy);
    }
    match a {
        Volume::Ext2(e) => {
            e.fs.lock()
                .rename(&inner_from, &inner_to)
                .map_err(Into::into)
        }
        Volume::Exfat(fs) => {
            let entry = fs.lookup(&inner_from)?;
            if entry.is_dir() {
                return Err(FsError::NotSupported);
            }
            if inner_from.eq_ignore_ascii_case(&inner_to) {
                return Ok(());
            }
            let mut data = alloc::vec![0u8; entry.size as usize];
            let n = fs.read(&entry, 0, &mut data)?;
            data.truncate(n);
            if let Ok(existing) = fs.lookup(&inner_to) {
                if existing.is_dir() {
                    return Err(FsError::IsDirectory);
                }
                fs.remove(&inner_to)?;
            }
            let mut target = fs.create(&inner_to, false)?;
            fs.write(&mut target, 0, &data)?;
            fs.remove(&inner_from)
        }
        Volume::Fat(fs) => {
            let entry = fs.lookup(&inner_from)?;
            if entry.is_dir() {
                return Err(FsError::NotSupported);
            }
            if inner_from.eq_ignore_ascii_case(&inner_to) {
                return Ok(());
            }
            let mut data = alloc::vec![0u8; entry.size as usize];
            let n = fs.read(&entry, 0, &mut data)?;
            data.truncate(n);
            if let Ok(existing) = fs.lookup(&inner_to) {
                if existing.is_dir() {
                    return Err(FsError::IsDirectory);
                }
                fs.remove(&inner_to)?;
            }
            let mut target = fs.create(&inner_to, false)?;
            fs.write(&mut target, 0, &data)?;
            fs.remove(&inner_from)
        }
    }
}

fn with_ext2_inode(
    path: &str,
    f: impl FnOnce(&mut ext2::Ext2<Disk>, u32) -> ext2::Result<()>,
) -> Result<()> {
    let (volume, inner) = resolve(path)?;
    match volume {
        Volume::Fat(_) | Volume::Exfat(_) => Err(FsError::NotSupported),
        Volume::Ext2(e) => {
            let mut fs = e.fs.lock();
            let ino = fs.lookup(&inner)?;
            f(&mut fs, ino).map_err(Into::into)
        }
    }
}

pub fn chmod(path: &str, mode: u16) -> Result<()> {
    with_ext2_inode(path, |fs, ino| fs.set_mode(ino, mode))
}

pub fn chown(path: &str, uid: u32, gid: u32) -> Result<()> {
    with_ext2_inode(path, |fs, ino| fs.set_owner(ino, uid, gid))
}

/// Yolu içeren birimin (toplam, boş) baytları.
pub fn capacity_of(path: &str) -> Result<(u64, u64)> {
    Ok(resolve(path)?.0.capacity())
}

// ---- erişim denetimi ----

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    Write,
    Execute,
}

fn unix_allows(meta: &Metadata, uid: u32, want: Access) -> bool {
    if uid == crate::kernel::users::ROOT_UID {
        return want != Access::Execute || meta.kind == Kind::Directory || meta.mode & 0o111 != 0;
    }
    let bits = if meta.uid == uid {
        meta.mode >> 6
    } else if meta.gid == uid {
        meta.mode >> 3
    } else {
        meta.mode
    } & 7;
    let need = match want {
        Access::Read => 4,
        Access::Write => 2,
        Access::Execute => 1,
    };
    bits & need != 0
}

fn parent_of(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

/// ext2 içinde yolun üst dizinlerinin hepsinde arama (x) izni var mı.
fn can_traverse(uid: u32, path: &str) -> bool {
    let mut current = String::new();
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for part in parts.iter().take(parts.len().saturating_sub(1)) {
        current.push('/');
        current.push_str(part);
        if let Ok(meta) = stat(&current)
            && meta.kind == Kind::Directory
            && !unix_allows(&meta, uid, Access::Execute)
        {
            return false;
        }
    }
    stat("/").map_or(true, |m| unix_allows(&m, uid, Access::Execute))
}

/// `stat` için: yolun dizinlerinde arama izni (FAT32'de okuma kuralı).
pub fn may_lookup(uid: u32, path: &str) -> bool {
    if uid == crate::kernel::users::ROOT_UID || path.starts_with("/dev") {
        return true;
    }
    if is_ext2(path) {
        can_traverse(uid, path)
    } else {
        crate::kernel::users::can_read(uid, path)
    }
}

fn is_ext2(path: &str) -> bool {
    !path.starts_with("/dev") && resolve(path).is_ok_and(|(v, _)| v.is_ext2())
}

/// Var olan bir yola erişim izni.
pub fn may_access(uid: u32, path: &str, want: Access) -> bool {
    if uid == crate::kernel::users::ROOT_UID && want != Access::Execute {
        return true;
    }
    if path == "/dev" || path.starts_with("/dev/") {
        return stat(path).is_ok_and(|m| unix_allows(&m, uid, want));
    }
    if !is_ext2(path) {
        return match want {
            Access::Write => crate::kernel::users::can_write(uid, path),
            Access::Read | Access::Execute => crate::kernel::users::can_read(uid, path),
        };
    }
    if !can_traverse(uid, path) {
        return false;
    }
    match stat(path) {
        Ok(meta) => unix_allows(&meta, uid, want),
        Err(_) => true,
    }
}

/// `path` oluşturulabilir ya da silinebilir mi (üst dizinde yazma; /tmp gibi
/// yapışkan dizinlerde yalnızca sahibi siler).
pub fn may_modify_entry(uid: u32, path: &str, deleting: bool) -> bool {
    if uid == crate::kernel::users::ROOT_UID {
        return true;
    }
    if path.starts_with("/dev") {
        return false;
    }
    if !is_ext2(path) {
        return crate::kernel::users::can_write(uid, path);
    }
    let parent = parent_of(path);
    if !may_access(uid, &parent, Access::Write) || !may_access(uid, &parent, Access::Execute) {
        return false;
    }
    if deleting
        && let Ok(dir) = stat(&parent)
        && dir.mode & 0o1000 != 0
    {
        return stat(path).is_ok_and(|m| m.uid == uid) || dir.uid == uid;
    }
    true
}

// ---- açılışta bağlama ----

fn parse_guid(text: &str) -> Option<[u8; 16]> {
    let hex: Vec<u8> = text
        .bytes()
        .filter(|b| *b != b'-')
        .map(|b| (b as char).to_digit(16).map(|d| d as u8))
        .collect::<Option<Vec<u8>>>()?;
    if hex.len() != 32 {
        return None;
    }
    let mut b = [0u8; 16];
    for i in 0..16 {
        b[i] = hex[i * 2] << 4 | hex[i * 2 + 1];
    }
    Some([
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15],
    ])
}

fn root_spec() -> Option<[u8; 16]> {
    crate::boot::cmdline()
        .split_whitespace()
        .find_map(|w| {
            w.strip_prefix("root=PARTUUID=")
                .or(w.strip_prefix("root=partuuid="))
        })
        .and_then(parse_guid)
}

fn pick_root(volumes: &[Arc<dyn BlockDevice>], spec: Option<[u8; 16]>) -> Option<usize> {
    if let Some(spec) = spec {
        return volumes
            .iter()
            .position(|v| storage::partition_info(v).is_some_and(|p| p.unique == spec));
    }
    let root_type = |v: &Arc<dyn BlockDevice>| {
        storage::partition_info(v).is_some_and(|p| p.type_guid == storage::LINUX_ROOT_TYPE)
    };
    volumes
        .iter()
        .position(|v| root_type(v) && !v.removable())
        .or_else(|| volumes.iter().position(root_type))
}

fn mount_root(volume: &Arc<dyn BlockDevice>) -> Result<String> {
    let label = mount_volume(volume.clone(), "/")?;
    // Aynı diskteki EFI sistem bölümü /boot olur.
    if let Some(info) = storage::partition_info(volume) {
        for other in storage::volumes() {
            if let Some(p) = storage::partition_info(&other)
                && p.disk == info.disk
                && p.type_guid == storage::ESP_TYPE
            {
                let _ = mount_volume(other.clone(), "/boot");
            }
        }
    }
    *ROOT_SPEC.lock() = None;
    Ok(label)
}

/// Açılıştan sonra takılan birim (USB): beklenen kökse `/`, değilse `/media/<ad>`.
pub fn volume_added(volume: Arc<dyn BlockDevice>) {
    let pending = *ROOT_SPEC.lock();
    if let Some(spec) = pending
        && storage::partition_info(&volume).is_some_and(|p| p.unique == spec)
    {
        if let Err(e) = mount_root(&volume) {
            crate::log!("fs: root {} not mountable: {}", volume.name(), e.message());
        }
        return;
    }
    if storage::partition_info(&volume).is_some_and(|p| p.type_guid == storage::ESP_TYPE)
        && MOUNTS.lock().iter().all(|m| m.point != "/boot")
    {
        return;
    }
    let point = alloc::format!("/media/{}", volume.name());
    if let Err(e) = mount_volume(volume.clone(), &point) {
        crate::log!("fs: {} not mountable: {}", volume.name(), e.message());
    }
}

fn self_test() -> bool {
    let path = "/tmp/.banna-selftest";
    let _ = mkdir("/tmp");
    let payload: Vec<u8> = (0..3000u32).map(|i| (i * 7 + 3) as u8).collect();
    let _ = remove(path);
    let ok = write_all(path, &payload).is_ok()
        && read_all(path).is_ok_and(|data| data == payload)
        && stat(path).is_ok_and(|m| m.size == payload.len() as u64);
    let renamed = alloc::format!("{}2", path);
    let _ = remove(&renamed);
    let moved = rename(path, &renamed).is_ok() && stat(&renamed).is_ok();
    let removed = remove(&renamed).is_ok() && matches!(stat(&renamed), Err(FsError::NotFound));
    ok && moved && removed
}

fn init(detail: &mut Detail) -> core::result::Result<(), &'static str> {
    let spec = root_spec();
    let mut volumes = storage::volumes();
    let mut root = pick_root(&volumes, spec);
    if spec.is_some() && root.is_none() {
        // USB kökü geç gelebilir: en fazla 10 sn bekle.
        crate::log!("fs: waiting for the root partition");
        *ROOT_SPEC.lock() = spec;
        let deadline = crate::arch::pit::uptime_ms() + 10_000;
        while crate::arch::pit::uptime_ms() < deadline {
            crate::sched::sleep_ms(200);
            volumes = storage::volumes();
            root = pick_root(&volumes, spec);
            if root.is_some() || resolve("/").is_ok() {
                break;
            }
        }
    }
    if root.is_none() && resolve("/").is_err() {
        root = volumes.iter().position(|v| {
            !v.removable()
                && storage::partition_info(v).is_none_or(|p| p.type_guid != storage::ESP_TYPE)
                && open_volume(v.clone()).is_ok()
        });
    }
    let mut root_text = String::from("no root file system");
    if let Some(i) = root
        && resolve("/").is_err()
    {
        match mount_root(&volumes[i]) {
            Ok(label) => {
                let kind = resolve("/").map_or("?", |(v, _)| v.kind());
                root_text = alloc::format!("{} '{}' on {} at /", kind, label, volumes[i].name());
            }
            Err(e) => crate::log!("fs: {} not mountable: {}", volumes[i].name(), e.message()),
        }
    }
    *ROOT_SPEC.lock() = None;
    let mut extra = 0;
    for volume in &volumes {
        if mount_of(&volume.name()).is_some() {
            continue;
        }
        if storage::partition_info(volume).is_some_and(|p| p.type_guid == storage::ESP_TYPE) {
            continue;
        }
        if let Err(e) = open_volume(volume.clone()) {
            crate::log!("fs: {} not mountable: {}", volume.name(), e.message());
            continue;
        }
        let point = alloc::format!("/media/{}", volume.name());
        if mount_volume(volume.clone(), &point).is_ok() {
            extra += 1;
        }
    }
    if resolve("/").is_ok() {
        crate::kernel::initcall::self_test("fs", self_test())?;
    }
    let _ = write!(detail, "{}", root_text);
    if extra > 0 {
        let _ = write!(detail, " + {} more", extra);
    }
    Ok(())
}

crate::initcall!(driver, 32, "File system (VFS: ext2 + FAT32)", init);
