//! Açık dosya tanıtıcıları: FAT32 ve ext2 dosyaları, dizinler ve `/dev` aygıtları.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::exfat::{self, Exfat};
use super::fat32::{Entry, Fat32};
use super::{DevNode, Ext2Volume, FsError, Metadata, Owner, Result, Volume, ext2, ext2_meta};
use crate::storage::{BlockDevice, SECTOR};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct OpenFlags(pub u32);

impl OpenFlags {
    pub const READ: Self = Self(1);
    pub const WRITE: Self = Self(2);
    pub const CREATE: Self = Self(4);
    pub const TRUNCATE: Self = Self(8);
    pub const APPEND: Self = Self(16);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl core::ops::BitOr for OpenFlags {
    type Output = Self;

    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

enum Node {
    Fat {
        fs: Arc<Fat32>,
        entry: Entry,
    },
    Exfat {
        fs: Arc<Exfat>,
        entry: exfat::Entry,
    },
    Ext2 {
        volume: Arc<Ext2Volume>,
        ino: u32,
        dir: bool,
    },
    Block(Arc<dyn BlockDevice>),
    Null,
    Zero,
    Dsp,
    /// `/dev` ve bağlama noktası olarak var olan sanal dizinler.
    Virtual(alloc::string::String),
}

pub struct File {
    node: Node,
    flags: OpenFlags,
    position: u64,
    listing: Option<Vec<Metadata>>,
}

impl File {
    /// `owner`: dosya oluşturulursa sahibi (kip 0644).
    pub fn open(path: &str, flags: OpenFlags, owner: Owner) -> Result<Self> {
        let node = if path == "/dev" {
            Node::Virtual(alloc::string::String::from(path))
        } else if path.starts_with("/dev/") {
            match super::device_node(path).ok_or(FsError::NotFound)? {
                DevNode::Null => Node::Null,
                DevNode::Zero => Node::Zero,
                DevNode::Dsp => Node::Dsp,
                DevNode::Block(d) => Node::Block(d),
            }
        } else {
            let opened = match super::resolve(path) {
                Ok((Volume::Fat(fs), inner)) => Self::open_fat(fs, &inner, flags),
                Ok((Volume::Exfat(fs), inner)) => Self::open_exfat(fs, &inner, flags),
                Ok((Volume::Ext2(volume), inner)) => Self::open_ext2(volume, &inner, flags, owner),
                Err(e) => Err(e),
            };
            match opened {
                Err(FsError::NotFound) if super::is_mount_parent(path) => {
                    Node::Virtual(alloc::string::String::from(path))
                }
                other => other?,
            }
        };
        let mut file = Self {
            node,
            flags,
            position: 0,
            listing: None,
        };
        if flags.contains(OpenFlags::APPEND) {
            file.position = file.size();
        }
        Ok(file)
    }

    fn open_fat(fs: Arc<Fat32>, path: &str, flags: OpenFlags) -> Result<Node> {
        let mut entry = match fs.lookup(path) {
            Ok(entry) => entry,
            Err(FsError::NotFound) if flags.contains(OpenFlags::CREATE) => {
                fs.create(path, false)?
            }
            Err(error) => return Err(error),
        };
        if entry.is_dir() && flags.contains(OpenFlags::WRITE) {
            return Err(FsError::IsDirectory);
        }
        if flags.contains(OpenFlags::TRUNCATE) && flags.contains(OpenFlags::WRITE) {
            fs.truncate(&mut entry)?;
        }
        Ok(Node::Fat { fs, entry })
    }

    fn open_exfat(fs: Arc<Exfat>, path: &str, flags: OpenFlags) -> Result<Node> {
        let mut entry = match fs.lookup(path) {
            Ok(entry) => entry,
            Err(FsError::NotFound) if flags.contains(OpenFlags::CREATE) => {
                fs.create(path, false)?
            }
            Err(error) => return Err(error),
        };
        if entry.is_dir() && flags.contains(OpenFlags::WRITE) {
            return Err(FsError::IsDirectory);
        }
        if flags.contains(OpenFlags::TRUNCATE) && flags.contains(OpenFlags::WRITE) {
            fs.truncate(&mut entry)?;
        }
        Ok(Node::Exfat { fs, entry })
    }

    fn open_ext2(
        volume: Arc<Ext2Volume>,
        path: &str,
        flags: OpenFlags,
        owner: Owner,
    ) -> Result<Node> {
        let mut fs = volume.fs.lock();
        let ino = match fs.lookup(path) {
            Ok(ino) => ino,
            Err(ext2::Error::NotFound) if flags.contains(OpenFlags::CREATE) => {
                fs.create(path, ext2::S_IFREG | 0o644, owner.uid, owner.gid)?
            }
            Err(e) => return Err(e.into()),
        };
        let inode = fs.inode(ino)?;
        let dir = inode.is_dir();
        if dir && flags.contains(OpenFlags::WRITE) {
            return Err(FsError::IsDirectory);
        }
        if flags.contains(OpenFlags::TRUNCATE) && flags.contains(OpenFlags::WRITE) {
            fs.truncate(ino, 0)?;
        }
        drop(fs);
        Ok(Node::Ext2 { volume, ino, dir })
    }

    pub fn size(&self) -> u64 {
        match &self.node {
            Node::Fat { entry, .. } => entry.size as u64,
            Node::Exfat { entry, .. } => entry.size,
            Node::Ext2 { volume, ino, .. } => volume.fs.lock().inode(*ino).map_or(0, |i| i.size),
            Node::Block(d) => d.sectors() * SECTOR as u64,
            _ => 0,
        }
    }

    pub fn is_dir(&self) -> bool {
        match &self.node {
            Node::Fat { entry, .. } => entry.is_dir(),
            Node::Exfat { entry, .. } => entry.is_dir(),
            Node::Ext2 { dir, .. } => *dir,
            Node::Virtual(_) => true,
            _ => false,
        }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        if !self.flags.contains(OpenFlags::READ) {
            return Err(FsError::PermissionDenied);
        }
        let n = match &self.node {
            Node::Fat { fs, entry } => fs.read(entry, self.position, buf)?,
            Node::Exfat { fs, entry } => fs.read(entry, self.position, buf)?,
            Node::Ext2 { dir: true, .. } | Node::Virtual(_) => return Err(FsError::IsDirectory),
            Node::Ext2 { volume, ino, .. } => volume.fs.lock().read(*ino, self.position, buf)?,
            Node::Block(d) => block_read(d, self.position, buf)?,
            Node::Null | Node::Dsp => 0,
            Node::Zero => {
                buf.fill(0);
                buf.len()
            }
        };
        self.position += n as u64;
        Ok(n)
    }

    pub fn write(&mut self, data: &[u8]) -> Result<usize> {
        if !self.flags.contains(OpenFlags::WRITE) {
            return Err(FsError::PermissionDenied);
        }
        if self.flags.contains(OpenFlags::APPEND) {
            self.position = self.size();
        }
        let n = match &mut self.node {
            Node::Fat { fs, entry } => fs.write(entry, self.position, data)?,
            Node::Exfat { fs, entry } => fs.write(entry, self.position, data)?,
            Node::Ext2 { volume, ino, .. } => volume.fs.lock().write(*ino, self.position, data)?,
            Node::Block(d) => block_write(d, self.position, data)?,
            Node::Null | Node::Zero => data.len(),
            Node::Dsp => crate::drivers::audio::write(data),
            Node::Virtual(_) => return Err(FsError::IsDirectory),
        };
        self.position += n as u64;
        Ok(n)
    }

    pub fn seek(&mut self, position: u64) -> u64 {
        self.position = position;
        position
    }

    /// Dizin tanıtıcısı için `index`'inci girdi (liste ilk çağrıda alınır).
    pub fn dir_entry(&mut self, index: usize) -> Result<Option<Metadata>> {
        if !self.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if self.listing.is_none() {
            let list = match &self.node {
                Node::Fat { fs, entry } => fs
                    .read_dir(entry)?
                    .into_iter()
                    .filter(|e| e.name != "." && e.name != "..")
                    .map(|e| e.metadata())
                    .collect(),
                Node::Exfat { fs, entry } => fs
                    .read_dir(entry)?
                    .into_iter()
                    .map(|e| e.metadata())
                    .collect(),
                Node::Ext2 { volume, ino, .. } => {
                    let mut fs = volume.fs.lock();
                    let mut out = Vec::new();
                    for entry in fs.read_dir(*ino)? {
                        if entry.name != "." && entry.name != ".." {
                            let inode = fs.inode(entry.inode)?;
                            out.push(ext2_meta(&entry.name, &inode));
                        }
                    }
                    out
                }
                Node::Virtual(path) => super::read_dir(path)?,
                _ => Vec::new(),
            };
            self.listing = Some(list);
        }
        Ok(self.listing.as_ref().unwrap().get(index).cloned())
    }
}

/// Bayt düzeyinde blok aygıtı okuma (kısmi sektörler tamponla).
fn block_read(device: &Arc<dyn BlockDevice>, offset: u64, buf: &mut [u8]) -> Result<usize> {
    let size = device.sectors() * SECTOR as u64;
    if offset >= size {
        return Ok(0);
    }
    let len = buf.len().min((size - offset) as usize);
    let first = offset / SECTOR as u64;
    let last = (offset + len as u64).div_ceil(SECTOR as u64);
    let mut tmp = vec![0u8; ((last - first) as usize) * SECTOR];
    device.read(first, &mut tmp).map_err(|_| FsError::Io)?;
    let skip = (offset % SECTOR as u64) as usize;
    buf[..len].copy_from_slice(&tmp[skip..skip + len]);
    Ok(len)
}

fn block_write(device: &Arc<dyn BlockDevice>, offset: u64, data: &[u8]) -> Result<usize> {
    let size = device.sectors() * SECTOR as u64;
    if offset >= size {
        return Err(FsError::NoSpace);
    }
    let len = data.len().min((size - offset) as usize);
    let first = offset / SECTOR as u64;
    let last = (offset + len as u64).div_ceil(SECTOR as u64);
    let skip = (offset % SECTOR as u64) as usize;
    let mut tmp = vec![0u8; ((last - first) as usize) * SECTOR];
    let partial = skip != 0 || !len.is_multiple_of(SECTOR);
    if partial {
        device.read(first, &mut tmp).map_err(|_| FsError::Io)?;
    }
    tmp[skip..skip + len].copy_from_slice(&data[..len]);
    device.write(first, &tmp).map_err(|_| FsError::Io)?;
    Ok(len)
}
