//! ext2 (revizyon 1): biçimlendirme, okuma ve yazma.
//!
//! Yalnızca `core` ve `alloc` kullanır; çekirdek, ana makinedeki imaj aracı
//! (`tools/mkimage`) ve kullanıcı alanındaki `mkfs.ext2` aynı dosyayı derler.
//!
//! Desteklenen özellikler: `filetype` (incompat), `sparse_super` ve `large_file`
//! (ro_compat); doğrudan + tek/çift/üç katlı dolaylı bloklar. Günlük (ext3),
//! extent (ext4) ve htree dizinler desteklenmez; bilinmeyen ro_compat özellikleri
//! salt okunur bağlanır. Meta veri her işlemden sonra diske yazılır.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

pub const SECTOR: usize = 512;
pub const ROOT_INODE: u32 = 2;
const MAGIC: u16 = 0xef53;
const FIRST_INODE: u32 = 11;
const INODE_SIZE: usize = 128;
const DIRECT: usize = 12;

const INCOMPAT_FILETYPE: u32 = 0x2;
const RO_COMPAT_SPARSE_SUPER: u32 = 0x1;
const RO_COMPAT_LARGE_FILE: u32 = 0x2;
const COMPAT_DIR_INDEX: u32 = 0x20;
const COMPAT_RESIZE_INODE: u32 = 0x10;
const INDEX_FLAG: u32 = 0x1000;

pub const S_IFMT: u16 = 0xf000;
pub const S_IFREG: u16 = 0x8000;
pub const S_IFDIR: u16 = 0x4000;
pub const S_IFLNK: u16 = 0xa000;

const FT_REG: u8 = 1;
const FT_DIR: u8 = 2;
const FT_SYMLINK: u8 = 7;

const CACHE_LIMIT: usize = 256;
/// Tek aygıt işleminde yazılan en fazla ardışık veri bloğu.
const RUN_BLOCKS: usize = 64;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    NotFound,
    Exists,
    NotDirectory,
    IsDirectory,
    NotEmpty,
    NoSpace,
    InvalidPath,
    NameTooLong,
    Unsupported,
    ReadOnly,
    Corrupt,
    Io,
}

pub type Result<T> = core::result::Result<T, Error>;

/// 512 baytlık sektörlerle adreslenen aygıt.
#[allow(clippy::result_unit_err)]
pub trait Device {
    fn sectors(&self) -> u64;
    fn read(&self, lba: u64, buf: &mut [u8]) -> core::result::Result<(), ()>;
    fn write(&self, lba: u64, buf: &[u8]) -> core::result::Result<(), ()>;
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

fn put16(b: &mut [u8], at: usize, v: u16) {
    b[at..at + 2].copy_from_slice(&v.to_le_bytes());
}

fn put32(b: &mut [u8], at: usize, v: u32) {
    b[at..at + 4].copy_from_slice(&v.to_le_bytes());
}

#[derive(Clone, Debug)]
pub struct Inode {
    raw: Vec<u8>,
    pub mode: u16,
    pub uid: u32,
    pub gid: u32,
    pub size: u64,
    pub atime: u32,
    pub ctime: u32,
    pub mtime: u32,
    pub dtime: u32,
    pub links: u16,
    /// 512 baytlık birimlerle ayrılmış alan (dolaylı bloklar dahil).
    pub sectors: u32,
    pub flags: u32,
    pub block: [u32; 15],
}

impl Inode {
    fn parse(raw: &[u8]) -> Self {
        let mut block = [0u32; 15];
        for (i, b) in block.iter_mut().enumerate() {
            *b = le32(raw, 40 + i * 4);
        }
        let mode = le16(raw, 0);
        let size_high = if mode & S_IFMT == S_IFREG {
            le32(raw, 108) as u64
        } else {
            0
        };
        Self {
            raw: raw.to_vec(),
            mode,
            uid: le16(raw, 2) as u32 | (le16(raw, 120) as u32) << 16,
            gid: le16(raw, 24) as u32 | (le16(raw, 122) as u32) << 16,
            size: le32(raw, 4) as u64 | size_high << 32,
            atime: le32(raw, 8),
            ctime: le32(raw, 12),
            mtime: le32(raw, 16),
            dtime: le32(raw, 20),
            links: le16(raw, 26),
            sectors: le32(raw, 28),
            flags: le32(raw, 32),
            block,
        }
    }

    fn serialize(&self) -> Vec<u8> {
        let mut raw = self.raw.clone();
        put16(&mut raw, 0, self.mode);
        put16(&mut raw, 2, self.uid as u16);
        put32(&mut raw, 4, self.size as u32);
        put32(&mut raw, 8, self.atime);
        put32(&mut raw, 12, self.ctime);
        put32(&mut raw, 16, self.mtime);
        put32(&mut raw, 20, self.dtime);
        put16(&mut raw, 24, self.gid as u16);
        put16(&mut raw, 26, self.links);
        put32(&mut raw, 28, self.sectors);
        put32(&mut raw, 32, self.flags);
        for (i, b) in self.block.iter().enumerate() {
            put32(&mut raw, 40 + i * 4, *b);
        }
        if self.mode & S_IFMT == S_IFREG {
            put32(&mut raw, 108, (self.size >> 32) as u32);
        }
        put16(&mut raw, 120, (self.uid >> 16) as u16);
        put16(&mut raw, 122, (self.gid >> 16) as u16);
        raw
    }

    fn empty(size: usize) -> Self {
        Self::parse(&vec![0u8; size])
    }

    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub fn is_symlink(&self) -> bool {
        self.mode & S_IFMT == S_IFLNK
    }

    pub fn permissions(&self) -> u16 {
        self.mode & 0o7777
    }
}

#[derive(Clone, Debug)]
pub struct DirEntry {
    pub name: String,
    pub inode: u32,
    pub kind: u8,
}

impl DirEntry {
    pub fn is_dir(&self) -> bool {
        self.kind == FT_DIR
    }
}

#[derive(Clone, Copy)]
struct Group {
    block_bitmap: u32,
    inode_bitmap: u32,
    inode_table: u32,
    free_blocks: u16,
    free_inodes: u16,
    used_dirs: u16,
}

pub struct Ext2<D: Device> {
    dev: D,
    pub block_size: usize,
    blocks_count: u32,
    first_data_block: u32,
    blocks_per_group: u32,
    inodes_per_group: u32,
    inodes_count: u32,
    inode_size: usize,
    free_blocks: u32,
    free_inodes: u32,
    groups: Vec<Group>,
    dirty_groups: bool,
    superblock: Vec<u8>,
    filetype: bool,
    pub read_only: bool,
    pub label: String,
    cache: BTreeMap<u32, Vec<u8>>,
    clock: fn() -> u32,
    /// Son `map_block` çağrısı yeni (içeriği tanımsız) bir veri bloğu ayırdı.
    fresh: bool,
    /// Önbellekte değişmiş, işlem sonunda (`flush`) yazılacak bit eşlemi blokları.
    dirty: BTreeSet<u32>,
}

/// Bölüm boyutuna göre blok boyutu: 512 MiB altında 1 KiB, üstünde 4 KiB.
fn pick_block_size(bytes: u64) -> usize {
    if bytes < 512 * 1024 * 1024 {
        1024
    } else {
        4096
    }
}

fn has_super(group: u32) -> bool {
    if group <= 1 {
        return true;
    }
    for base in [3u32, 5, 7] {
        let mut n = base;
        while n < group {
            n *= base;
        }
        if n == group {
            return true;
        }
    }
    false
}

pub struct FormatOptions<'a> {
    pub label: &'a str,
    pub uuid: [u8; 16],
    pub now: u32,
}

/// Aygıtı boş bir ext2 ile biçimlendirir (kök dizin + `lost+found`).
pub fn format<D: Device>(dev: &D, options: &FormatOptions) -> Result<()> {
    let bytes = dev.sectors() * SECTOR as u64;
    let bs = pick_block_size(bytes);
    let spb = (bs / SECTOR) as u64;
    let mut blocks = (bytes / bs as u64).min(u32::MAX as u64 - 1) as u32;
    let first_data = if bs == 1024 { 1 } else { 0 };
    let bpg = (bs * 8) as u32;
    if blocks < 64 {
        return Err(Error::NoSpace);
    }
    let bytes_per_inode: u64 = if bytes < 1024 * 1024 * 1024 {
        8192
    } else {
        65536
    };
    let inodes_per_block = (bs / INODE_SIZE) as u32;

    // Son grup kendi meta verisini taşıyamayacak kadar küçükse atılır.
    let (groups, ipg, itb, gdt_blocks) = loop {
        let groups = (blocks - first_data).div_ceil(bpg);
        let total_inodes = ((blocks as u64 * bs as u64) / bytes_per_inode).max(16) as u32;
        let ipg = total_inodes
            .div_ceil(groups)
            .div_ceil(inodes_per_block)
            .max(1)
            * inodes_per_block;
        let ipg = ipg.min(bpg).max(16);
        let itb = ipg / inodes_per_block;
        let gdt_blocks = ((groups as usize * 32).div_ceil(bs)) as u32;
        let last = blocks - first_data - (groups - 1) * bpg;
        let overhead = 2
            + itb
            + if has_super(groups - 1) {
                1 + gdt_blocks
            } else {
                0
            };
        if groups > 1 && last < overhead + 16 {
            blocks = first_data + (groups - 1) * bpg;
            continue;
        }
        break (groups, ipg, itb, gdt_blocks);
    };

    let write_block = |block: u32, data: &[u8]| -> Result<()> {
        dev.write(block as u64 * spb, data).map_err(|_| Error::Io)
    };
    let zero_blocks = |start: u32, count: u32| -> Result<()> {
        let chunk = 64usize;
        let zeros = vec![0u8; bs * chunk];
        let mut done = 0;
        while done < count {
            let n = ((count - done) as usize).min(chunk);
            dev.write((start + done) as u64 * spb, &zeros[..n * bs])
                .map_err(|_| Error::Io)?;
            done += n as u32;
        }
        Ok(())
    };

    let mut descs: Vec<Group> = Vec::new();
    let mut total_free_blocks = 0u32;
    let mut root_block = 0;
    let mut lost_block = 0;
    for g in 0..groups {
        let start = first_data + g * bpg;
        let count = (blocks - start).min(bpg);
        let mut next = start;
        if has_super(g) {
            next += 1 + gdt_blocks;
        }
        let block_bitmap = next;
        let inode_bitmap = next + 1;
        let inode_table = next + 2;
        next += 2 + itb;
        let mut used = next - start;
        if g == 0 {
            root_block = next;
            lost_block = next + 1;
            used += 2;
        }
        let mut bitmap = vec![0u8; bs];
        for i in 0..used {
            bitmap[(i / 8) as usize] |= 1 << (i % 8);
        }
        for i in count..bpg {
            bitmap[(i / 8) as usize] |= 1 << (i % 8);
        }
        write_block(block_bitmap, &bitmap)?;
        let mut ibitmap = vec![0u8; bs];
        if g == 0 {
            for i in 0..FIRST_INODE {
                ibitmap[(i / 8) as usize] |= 1 << (i % 8);
            }
        }
        for i in ipg..(bs as u32 * 8) {
            ibitmap[(i / 8) as usize] |= 1 << (i % 8);
        }
        write_block(inode_bitmap, &ibitmap)?;
        zero_blocks(inode_table, itb)?;
        let free = count - used;
        total_free_blocks += free;
        descs.push(Group {
            block_bitmap,
            inode_bitmap,
            inode_table,
            free_blocks: free as u16,
            free_inodes: (if g == 0 { ipg - FIRST_INODE } else { ipg }) as u16,
            used_dirs: if g == 0 { 2 } else { 0 },
        });
    }

    // Kök dizin (2) ve lost+found (11).
    let now = options.now;
    let dir_block = |own: u32, parent: u32, extra: Option<(u32, &str)>| -> Vec<u8> {
        let mut b = vec![0u8; bs];
        put32(&mut b, 0, own);
        put16(&mut b, 4, 12);
        b[6] = 1;
        b[7] = FT_DIR;
        b[8] = b'.';
        put32(&mut b, 12, parent);
        b[18] = 2;
        b[19] = FT_DIR;
        b[20] = b'.';
        b[21] = b'.';
        match extra {
            None => put16(&mut b, 16, (bs - 12) as u16),
            Some((ino, name)) => {
                put16(&mut b, 16, 12);
                put32(&mut b, 24, ino);
                put16(&mut b, 28, (bs - 24) as u16);
                b[30] = name.len() as u8;
                b[31] = FT_DIR;
                b[32..32 + name.len()].copy_from_slice(name.as_bytes());
            }
        }
        b
    };
    write_block(
        root_block,
        &dir_block(ROOT_INODE, ROOT_INODE, Some((FIRST_INODE, "lost+found"))),
    )?;
    write_block(lost_block, &dir_block(FIRST_INODE, ROOT_INODE, None))?;
    let make_dir_inode = |mode: u16, links: u16, block: u32| -> Vec<u8> {
        let mut inode = Inode::empty(INODE_SIZE);
        inode.mode = S_IFDIR | mode;
        inode.links = links;
        inode.size = bs as u64;
        inode.sectors = (bs / SECTOR) as u32;
        inode.atime = now;
        inode.ctime = now;
        inode.mtime = now;
        inode.block[0] = block;
        inode.serialize()
    };
    let table = descs[0].inode_table;
    let mut first_table_block = vec![0u8; bs];
    let root_raw = make_dir_inode(0o755, 3, root_block);
    let off = (ROOT_INODE as usize - 1) * INODE_SIZE;
    first_table_block[off..off + INODE_SIZE].copy_from_slice(&root_raw);
    let lost_raw = make_dir_inode(0o700, 2, lost_block);
    let lost_off = (FIRST_INODE as usize - 1) * INODE_SIZE;
    let lost_table_block = lost_off / bs;
    if lost_table_block == 0 {
        first_table_block[lost_off..lost_off + INODE_SIZE].copy_from_slice(&lost_raw);
        write_block(table, &first_table_block)?;
    } else {
        write_block(table, &first_table_block)?;
        let mut b = vec![0u8; bs];
        let o = lost_off % bs;
        b[o..o + INODE_SIZE].copy_from_slice(&lost_raw);
        write_block(table + lost_table_block as u32, &b)?;
    }

    // Süper blok ve grup tanımlayıcıları (seyrek yedeklerle).
    let mut sb = vec![0u8; 1024];
    put32(&mut sb, 0, ipg * groups);
    put32(&mut sb, 4, blocks);
    put32(&mut sb, 8, blocks / 20);
    put32(&mut sb, 12, total_free_blocks);
    put32(&mut sb, 16, ipg * groups - FIRST_INODE);
    put32(&mut sb, 20, first_data);
    put32(&mut sb, 24, (bs / 1024).trailing_zeros());
    put32(&mut sb, 28, (bs / 1024).trailing_zeros());
    put32(&mut sb, 32, bpg);
    put32(&mut sb, 36, bpg);
    put32(&mut sb, 40, ipg);
    put32(&mut sb, 48, now);
    put16(&mut sb, 54, 0xffff);
    put16(&mut sb, 56, MAGIC);
    put16(&mut sb, 58, 1);
    put16(&mut sb, 60, 1);
    put32(&mut sb, 64, now);
    put32(&mut sb, 76, 1);
    put32(&mut sb, 84, FIRST_INODE);
    put16(&mut sb, 88, INODE_SIZE as u16);
    put32(&mut sb, 96, INCOMPAT_FILETYPE);
    put32(&mut sb, 100, RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE);
    sb[104..120].copy_from_slice(&options.uuid);
    let label = options.label.as_bytes();
    let n = label.len().min(16);
    sb[120..120 + n].copy_from_slice(&label[..n]);
    let mut gdt = vec![0u8; gdt_blocks as usize * bs];
    for (i, d) in descs.iter().enumerate() {
        let o = i * 32;
        put32(&mut gdt, o, d.block_bitmap);
        put32(&mut gdt, o + 4, d.inode_bitmap);
        put32(&mut gdt, o + 8, d.inode_table);
        put16(&mut gdt, o + 12, d.free_blocks);
        put16(&mut gdt, o + 14, d.free_inodes);
        put16(&mut gdt, o + 16, d.used_dirs);
    }
    for g in (0..groups).filter(|&g| has_super(g)) {
        let start = first_data + g * bpg;
        put16(&mut sb, 90, g as u16);
        if g == 0 {
            let mut boot = vec![0u8; bs.max(2048)];
            if bs == 1024 {
                dev.read(0, &mut boot[..1024]).map_err(|_| Error::Io)?;
                boot[1024..2048].copy_from_slice(&sb);
                dev.write(0, &boot[..2048]).map_err(|_| Error::Io)?;
            } else {
                dev.read(0, &mut boot[..1024]).map_err(|_| Error::Io)?;
                boot[1024..2048].copy_from_slice(&sb);
                write_block(0, &boot[..bs])?;
            }
        } else {
            let mut b = vec![0u8; bs];
            b[..1024].copy_from_slice(&sb);
            write_block(start, &b)?;
        }
        write_block(start + 1, &gdt)?;
    }
    Ok(())
}

impl<D: Device> Ext2<D> {
    pub fn mount(dev: D, clock: fn() -> u32) -> Result<Self> {
        let mut sb = vec![0u8; 1024];
        dev.read(2, &mut sb).map_err(|_| Error::Io)?;
        if le16(&sb, 56) != MAGIC {
            return Err(Error::Unsupported);
        }
        let rev = le32(&sb, 76);
        let incompat = if rev >= 1 { le32(&sb, 96) } else { 0 };
        let ro_compat = if rev >= 1 { le32(&sb, 100) } else { 0 };
        if incompat & !INCOMPAT_FILETYPE != 0 {
            return Err(Error::Unsupported);
        }
        let read_only = ro_compat & !(RO_COMPAT_SPARSE_SUPER | RO_COMPAT_LARGE_FILE) != 0;
        let block_size = 1024usize << le32(&sb, 24);
        if !(1024..=65536).contains(&block_size) {
            return Err(Error::Corrupt);
        }
        let blocks_count = le32(&sb, 4);
        let first_data_block = le32(&sb, 20);
        let blocks_per_group = le32(&sb, 32);
        let inodes_per_group = le32(&sb, 40);
        if blocks_per_group == 0 || inodes_per_group == 0 {
            return Err(Error::Corrupt);
        }
        let inode_size = if rev >= 1 {
            le16(&sb, 88) as usize
        } else {
            128
        };
        let groups = (blocks_count - first_data_block).div_ceil(blocks_per_group) as usize;
        let label_end = sb[120..136].iter().position(|&b| b == 0).unwrap_or(16);
        let mut fs = Self {
            dev,
            block_size,
            blocks_count,
            first_data_block,
            blocks_per_group,
            inodes_per_group,
            inodes_count: le32(&sb, 0),
            inode_size,
            free_blocks: le32(&sb, 12),
            free_inodes: le32(&sb, 16),
            groups: Vec::with_capacity(groups),
            dirty_groups: false,
            filetype: incompat & INCOMPAT_FILETYPE != 0,
            read_only,
            label: String::from_utf8_lossy(&sb[120..120 + label_end]).into_owned(),
            superblock: sb,
            cache: BTreeMap::new(),
            clock,
            fresh: false,
            dirty: BTreeSet::new(),
        };
        let gdt_blocks = (groups * 32).div_ceil(block_size);
        let mut gdt = vec![0u8; gdt_blocks * block_size];
        for i in 0..gdt_blocks {
            let block = fs.read_block(first_data_block + 1 + i as u32)?;
            gdt[i * block_size..(i + 1) * block_size].copy_from_slice(&block);
        }
        for g in 0..groups {
            let o = g * 32;
            fs.groups.push(Group {
                block_bitmap: le32(&gdt, o),
                inode_bitmap: le32(&gdt, o + 4),
                inode_table: le32(&gdt, o + 8),
                free_blocks: le16(&gdt, o + 12),
                free_inodes: le16(&gdt, o + 14),
                used_dirs: le16(&gdt, o + 16),
            });
        }
        Ok(fs)
    }

    pub fn device(&self) -> &D {
        &self.dev
    }

    /// (toplam bayt, boş bayt)
    pub fn capacity(&self) -> (u64, u64) {
        let bs = self.block_size as u64;
        (self.blocks_count as u64 * bs, self.free_blocks as u64 * bs)
    }

    fn now(&self) -> u32 {
        (self.clock)()
    }

    fn sectors_per_block(&self) -> u64 {
        (self.block_size / SECTOR) as u64
    }

    fn read_block(&mut self, block: u32) -> Result<Vec<u8>> {
        if let Some(data) = self.cache.get(&block) {
            return Ok(data.clone());
        }
        if block >= self.blocks_count && self.blocks_count != 0 {
            return Err(Error::Corrupt);
        }
        let mut data = vec![0u8; self.block_size];
        self.dev
            .read(block as u64 * self.sectors_per_block(), &mut data)
            .map_err(|_| Error::Io)?;
        self.remember(block, data.clone());
        Ok(data)
    }

    fn write_block(&mut self, block: u32, data: &[u8]) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        if block == 0 && self.first_data_block == 0 && self.block_size > 1024 {
            // Blok 0 süper bloğu da içerir; yalnızca uygun bayt aralığı yazılır.
            return Err(Error::Corrupt);
        }
        self.dev
            .write(block as u64 * self.sectors_per_block(), data)
            .map_err(|_| Error::Io)?;
        self.remember(block, data.to_vec());
        Ok(())
    }

    fn remember(&mut self, block: u32, data: Vec<u8>) {
        if self.cache.len() >= CACHE_LIMIT && !self.cache.contains_key(&block) {
            let victim = self
                .cache
                .keys()
                .copied()
                .find(|b| !self.dirty.contains(b))
                .unwrap_or_else(|| *self.cache.keys().next().unwrap());
            if self.dirty.remove(&victim) {
                let data = self.cache[&victim].clone();
                let _ = self
                    .dev
                    .write(victim as u64 * self.sectors_per_block(), &data);
            }
            self.cache.remove(&victim);
        }
        self.cache.insert(block, data);
    }

    /// Bloğu önbellekte değiştirir; diske `flush` sırasında yazılır.
    fn write_block_later(&mut self, block: u32, data: Vec<u8>) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        self.remember(block, data);
        self.dirty.insert(block);
        Ok(())
    }

    /// Süper bloğu ve grup tanımlayıcılarını (birincil kopya) yazar.
    fn flush(&mut self) -> Result<()> {
        if self.read_only {
            return Ok(());
        }
        for block in core::mem::take(&mut self.dirty) {
            if let Some(data) = self.cache.get(&block) {
                self.dev
                    .write(block as u64 * self.sectors_per_block(), data)
                    .map_err(|_| Error::Io)?;
            }
        }
        let now = self.now();
        let mut sb = self.superblock.clone();
        put32(&mut sb, 12, self.free_blocks);
        put32(&mut sb, 16, self.free_inodes);
        put32(&mut sb, 48, now);
        put16(&mut sb, 90, 0);
        self.superblock = sb.clone();
        self.dev.write(2, &sb).map_err(|_| Error::Io)?;
        if self.block_size == 1024 {
            self.cache.remove(&1);
        } else {
            self.cache.remove(&0);
        }
        if self.dirty_groups {
            let bs = self.block_size;
            let mut gdt = vec![0u8; (self.groups.len() * 32).div_ceil(bs) * bs];
            for (i, d) in self.groups.iter().enumerate() {
                let o = i * 32;
                put32(&mut gdt, o, d.block_bitmap);
                put32(&mut gdt, o + 4, d.inode_bitmap);
                put32(&mut gdt, o + 8, d.inode_table);
                put16(&mut gdt, o + 12, d.free_blocks);
                put16(&mut gdt, o + 14, d.free_inodes);
                put16(&mut gdt, o + 16, d.used_dirs);
            }
            for (i, chunk) in gdt.chunks(bs).enumerate() {
                let block = self.first_data_block + 1 + i as u32;
                let mut existing = self.read_block(block)?;
                let n = chunk.len();
                existing[..n].copy_from_slice(chunk);
                self.write_block(block, &existing)?;
            }
            self.dirty_groups = false;
        }
        Ok(())
    }

    // ---- inode'lar ----

    fn inode_location(&self, ino: u32) -> Result<(u32, usize)> {
        if ino == 0 || ino > self.inodes_count {
            return Err(Error::Corrupt);
        }
        let index = ino - 1;
        let group = (index / self.inodes_per_group) as usize;
        let slot = (index % self.inodes_per_group) as usize;
        let byte = slot * self.inode_size;
        let table = self.groups.get(group).ok_or(Error::Corrupt)?.inode_table;
        Ok((
            table + (byte / self.block_size) as u32,
            byte % self.block_size,
        ))
    }

    pub fn inode(&mut self, ino: u32) -> Result<Inode> {
        let (block, offset) = self.inode_location(ino)?;
        let data = self.read_block(block)?;
        Ok(Inode::parse(&data[offset..offset + self.inode_size]))
    }

    fn write_inode(&mut self, ino: u32, inode: &Inode) -> Result<()> {
        let (block, offset) = self.inode_location(ino)?;
        let mut data = self.read_block(block)?;
        let raw = inode.serialize();
        data[offset..offset + self.inode_size].copy_from_slice(&raw[..self.inode_size]);
        self.write_block(block, &data)
    }

    // ---- bit eşlemleri ----

    fn alloc_bit(&mut self, inode: bool, goal_group: usize) -> Result<u32> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let groups = self.groups.len();
        for step in 0..groups {
            let g = (goal_group + step) % groups;
            let free = if inode {
                self.groups[g].free_inodes
            } else {
                self.groups[g].free_blocks
            };
            if free == 0 {
                continue;
            }
            let bitmap_block = if inode {
                self.groups[g].inode_bitmap
            } else {
                self.groups[g].block_bitmap
            };
            let mut bitmap = self.read_block(bitmap_block)?;
            let limit = if inode {
                self.inodes_per_group
            } else {
                let start = self.first_data_block + g as u32 * self.blocks_per_group;
                (self.blocks_count - start).min(self.blocks_per_group)
            };
            let Some(bit) = (0..limit).find(|&i| bitmap[(i / 8) as usize] & (1 << (i % 8)) == 0)
            else {
                continue;
            };
            bitmap[(bit / 8) as usize] |= 1 << (bit % 8);
            self.write_block_later(bitmap_block, bitmap)?;
            self.dirty_groups = true;
            if inode {
                self.groups[g].free_inodes -= 1;
                self.free_inodes = self.free_inodes.saturating_sub(1);
                return Ok(g as u32 * self.inodes_per_group + bit + 1);
            }
            self.groups[g].free_blocks -= 1;
            self.free_blocks = self.free_blocks.saturating_sub(1);
            return Ok(self.first_data_block + g as u32 * self.blocks_per_group + bit);
        }
        Err(Error::NoSpace)
    }

    fn free_bit(&mut self, inode: bool, number: u32) -> Result<()> {
        let (g, bit) = if inode {
            let i = number - 1;
            (
                (i / self.inodes_per_group) as usize,
                i % self.inodes_per_group,
            )
        } else {
            let b = number - self.first_data_block;
            (
                (b / self.blocks_per_group) as usize,
                b % self.blocks_per_group,
            )
        };
        let group = *self.groups.get(g).ok_or(Error::Corrupt)?;
        let bitmap_block = if inode {
            group.inode_bitmap
        } else {
            group.block_bitmap
        };
        let mut bitmap = self.read_block(bitmap_block)?;
        let mask = 1u8 << (bit % 8);
        if bitmap[(bit / 8) as usize] & mask == 0 {
            return Ok(());
        }
        bitmap[(bit / 8) as usize] &= !mask;
        self.write_block_later(bitmap_block, bitmap)?;
        self.dirty_groups = true;
        if inode {
            self.groups[g].free_inodes += 1;
            self.free_inodes += 1;
        } else {
            self.groups[g].free_blocks += 1;
            self.free_blocks += 1;
        }
        Ok(())
    }

    fn group_of_inode(&self, ino: u32) -> usize {
        ((ino.max(1) - 1) / self.inodes_per_group) as usize
    }

    fn alloc_zeroed_block(&mut self, goal_group: usize, inode: &mut Inode) -> Result<u32> {
        let block = self.alloc_bit(false, goal_group)?;
        let zeros = vec![0u8; self.block_size];
        self.write_block(block, &zeros)?;
        inode.sectors += self.sectors_per_block() as u32;
        Ok(block)
    }

    // ---- blok eşleme ----

    fn pointers(&self) -> u64 {
        (self.block_size / 4) as u64
    }

    /// Dosyanın `index`'inci bloğunun disk adresi; `allocate` ise eksikler ayrılır.
    fn map_block(
        &mut self,
        ino: u32,
        inode: &mut Inode,
        index: u64,
        allocate: bool,
    ) -> Result<Option<u32>> {
        self.fresh = false;
        let p = self.pointers();
        let goal = self.group_of_inode(ino);
        let (slot, path): (usize, Vec<u64>) = if index < DIRECT as u64 {
            (index as usize, Vec::new())
        } else if index < DIRECT as u64 + p {
            (12, vec![index - DIRECT as u64])
        } else if index < DIRECT as u64 + p + p * p {
            let i = index - DIRECT as u64 - p;
            (13, vec![i / p, i % p])
        } else {
            let i = index - DIRECT as u64 - p - p * p;
            if i >= p * p * p {
                return Err(Error::NoSpace);
            }
            (14, vec![i / (p * p), (i / p) % p, i % p])
        };
        let mut current = inode.block[slot];
        if current == 0 {
            if !allocate {
                return Ok(None);
            }
            current = if path.is_empty() {
                let b = self.alloc_bit(false, goal)?;
                inode.sectors += self.sectors_per_block() as u32;
                self.fresh = true;
                b
            } else {
                self.alloc_zeroed_block(goal, inode)?
            };
            inode.block[slot] = current;
        }
        for (depth, &entry) in path.iter().enumerate() {
            let mut table = self.read_block(current)?;
            let mut next = le32(&table, entry as usize * 4);
            if next == 0 {
                if !allocate {
                    return Ok(None);
                }
                let leaf = depth + 1 == path.len();
                next = if leaf {
                    let b = self.alloc_bit(false, goal)?;
                    inode.sectors += self.sectors_per_block() as u32;
                    self.fresh = true;
                    b
                } else {
                    self.alloc_zeroed_block(goal, inode)?
                };
                put32(&mut table, entry as usize * 4, next);
                self.write_block(current, &table)?;
            }
            current = next;
        }
        Ok(Some(current))
    }

    /// `keep` bloktan sonrasını serbest bırakır; boşalan dolaylı bloklar da gider.
    fn free_blocks_from(&mut self, inode: &mut Inode, keep: u64) -> Result<()> {
        let p = self.pointers();
        for i in (keep as usize).min(DIRECT)..DIRECT {
            if inode.block[i] != 0 {
                self.free_bit(false, inode.block[i])?;
                inode.block[i] = 0;
                inode.sectors = inode
                    .sectors
                    .saturating_sub(self.sectors_per_block() as u32);
            }
        }
        let mut base = DIRECT as u64;
        for (slot, depth) in [(12usize, 1u32), (13, 2), (14, 3)] {
            let span = p.pow(depth);
            if inode.block[slot] != 0 {
                let first = keep.saturating_sub(base);
                if self.free_tree(inode, inode.block[slot], depth, first)? {
                    inode.block[slot] = 0;
                }
            }
            base += span;
        }
        Ok(())
    }

    /// Dolaylı ağaçta `first` yaprağından sonrasını boşaltır; ağaç tamamen boşaldıysa `true`.
    fn free_tree(&mut self, inode: &mut Inode, block: u32, depth: u32, first: u64) -> Result<bool> {
        let p = self.pointers();
        let span = p.pow(depth - 1);
        let mut table = self.read_block(block)?;
        let mut changed = false;
        for i in 0..p {
            let child = le32(&table, i as usize * 4);
            if child == 0 {
                continue;
            }
            let child_first = (i + 1) * span;
            if child_first <= first {
                continue;
            }
            let start_in_child = first.saturating_sub(i * span);
            let emptied = if depth == 1 {
                self.free_bit(false, child)?;
                inode.sectors = inode
                    .sectors
                    .saturating_sub(self.sectors_per_block() as u32);
                true
            } else {
                self.free_tree(inode, child, depth - 1, start_in_child)?
            };
            if emptied {
                put32(&mut table, i as usize * 4, 0);
                changed = true;
            }
        }
        let empty = (0..p).all(|i| le32(&table, i as usize * 4) == 0);
        if empty {
            self.free_bit(false, block)?;
            inode.sectors = inode
                .sectors
                .saturating_sub(self.sectors_per_block() as u32);
            return Ok(true);
        }
        if changed {
            self.write_block(block, &table)?;
        }
        Ok(false)
    }

    // ---- dosya içeriği ----

    pub fn read(&mut self, ino: u32, offset: u64, buf: &mut [u8]) -> Result<usize> {
        let mut inode = self.inode(ino)?;
        if offset >= inode.size {
            return Ok(0);
        }
        let len = buf.len().min((inode.size - offset) as usize);
        let bs = self.block_size as u64;
        let mut done = 0;
        while done < len {
            let pos = offset + done as u64;
            let within = (pos % bs) as usize;
            let take = (self.block_size - within).min(len - done);
            match self.map_block(ino, &mut inode, pos / bs, false)? {
                Some(block)
                    if take == self.block_size
                        && block < self.blocks_count
                        && !self.cache.contains_key(&block) =>
                {
                    // Ardışık tam bloklar tek aygıt işlemiyle okunur (USB'de her komut pahalı).
                    let mut run = 1;
                    let full = (len - done) / self.block_size;
                    while run < full.min(RUN_BLOCKS) {
                        let next = block + run as u32;
                        if self.map_block(ino, &mut inode, pos / bs + run as u64, false)?
                            != Some(next)
                            || self.cache.contains_key(&next)
                            || next >= self.blocks_count
                        {
                            break;
                        }
                        run += 1;
                    }
                    let bytes = run * self.block_size;
                    self.dev
                        .read(
                            block as u64 * self.sectors_per_block(),
                            &mut buf[done..done + bytes],
                        )
                        .map_err(|_| Error::Io)?;
                    done += bytes;
                    continue;
                }
                Some(block) => {
                    let data = self.read_block(block)?;
                    buf[done..done + take].copy_from_slice(&data[within..within + take]);
                }
                None => buf[done..done + take].fill(0),
            }
            done += take;
        }
        Ok(len)
    }

    pub fn write(&mut self, ino: u32, offset: u64, data: &[u8]) -> Result<usize> {
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(Error::IsDirectory);
        }
        let result = self.write_into(ino, &mut inode, offset, data);
        let written = match result {
            Ok(n) => n,
            Err(Error::NoSpace) => 0,
            Err(e) => return Err(e),
        };
        if offset + written as u64 > inode.size {
            inode.size = offset + written as u64;
        }
        let now = self.now();
        inode.mtime = now;
        inode.ctime = now;
        self.write_inode(ino, &inode)?;
        self.flush()?;
        if written == 0 && !data.is_empty() {
            return Err(Error::NoSpace);
        }
        Ok(written)
    }

    fn write_into(
        &mut self,
        ino: u32,
        inode: &mut Inode,
        offset: u64,
        data: &[u8],
    ) -> Result<usize> {
        let bs = self.block_size as u64;
        let mut done = 0;
        while done < data.len() {
            let pos = offset + done as u64;
            let within = (pos % bs) as usize;
            let take = (self.block_size - within).min(data.len() - done);
            let block = match self.map_block(ino, inode, pos / bs, true) {
                Ok(Some(b)) => b,
                Ok(None) => return Err(Error::Corrupt),
                Err(Error::NoSpace) if done > 0 => return Ok(done),
                Err(e) => return Err(e),
            };
            if take == self.block_size {
                // Ardışık tam bloklar tek aygıt işlemiyle yazılır.
                let mut run = 1;
                let full = (data.len() - done) / self.block_size;
                while run < full.min(RUN_BLOCKS) {
                    let next = match self.map_block(ino, inode, pos / bs + run as u64, true) {
                        Ok(Some(b)) => b,
                        Err(Error::NoSpace) => break,
                        Ok(None) => return Err(Error::Corrupt),
                        Err(e) => return Err(e),
                    };
                    if next != block + run as u32 {
                        // Bitişik değil: bu blok bir sonraki turda yazılır (zaten eşli).
                        break;
                    }
                    run += 1;
                }
                let bytes = run * self.block_size;
                if self.read_only {
                    return Err(Error::ReadOnly);
                }
                self.dev
                    .write(
                        block as u64 * self.sectors_per_block(),
                        &data[done..done + bytes],
                    )
                    .map_err(|_| Error::Io)?;
                for i in 0..run as u32 {
                    self.cache.remove(&(block + i));
                }
                done += bytes;
                if inode.size < offset + done as u64 {
                    inode.size = offset + done as u64;
                }
                continue;
            }
            let mut buffer = if self.fresh {
                vec![0u8; self.block_size]
            } else {
                self.read_block(block)?
            };
            buffer[within..within + take].copy_from_slice(&data[done..done + take]);
            self.write_block(block, &buffer)?;
            done += take;
            if inode.size < offset + done as u64 {
                inode.size = offset + done as u64;
            }
        }
        Ok(done)
    }

    pub fn truncate(&mut self, ino: u32, size: u64) -> Result<()> {
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            return Err(Error::IsDirectory);
        }
        if size < inode.size {
            let bs = self.block_size as u64;
            self.free_blocks_from(&mut inode, size.div_ceil(bs))?;
            if !size.is_multiple_of(bs)
                && let Some(block) = self.map_block(ino, &mut inode, size / bs, false)?
            {
                let mut data = self.read_block(block)?;
                data[(size % bs) as usize..].fill(0);
                self.write_block(block, &data)?;
            }
        }
        inode.size = size;
        let now = self.now();
        inode.mtime = now;
        inode.ctime = now;
        self.write_inode(ino, &inode)?;
        self.flush()
    }

    // ---- dizinler ----

    fn dir_blocks(&mut self, ino: u32, inode: &mut Inode) -> Result<Vec<(u64, u32)>> {
        let count = inode.size.div_ceil(self.block_size as u64);
        let mut out = Vec::new();
        for i in 0..count {
            if let Some(b) = self.map_block(ino, inode, i, false)? {
                out.push((i, b));
            }
        }
        Ok(out)
    }

    pub fn read_dir(&mut self, ino: u32) -> Result<Vec<DirEntry>> {
        let mut inode = self.inode(ino)?;
        if !inode.is_dir() {
            return Err(Error::NotDirectory);
        }
        let mut out = Vec::new();
        for (_, block) in self.dir_blocks(ino, &mut inode)? {
            let data = self.read_block(block)?;
            let mut off = 0;
            while off + 8 <= data.len() {
                let entry_ino = le32(&data, off);
                let rec_len = le16(&data, off + 4) as usize;
                if rec_len < 8 || off + rec_len > data.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = data[off + 6] as usize;
                if entry_ino != 0 && name_len > 0 && off + 8 + name_len <= data.len() {
                    let name =
                        String::from_utf8_lossy(&data[off + 8..off + 8 + name_len]).into_owned();
                    let kind = if self.filetype {
                        data[off + 7]
                    } else {
                        let child = self.inode(entry_ino)?;
                        match child.mode & S_IFMT {
                            S_IFDIR => FT_DIR,
                            S_IFLNK => FT_SYMLINK,
                            _ => FT_REG,
                        }
                    };
                    out.push(DirEntry {
                        name,
                        inode: entry_ino,
                        kind,
                    });
                }
                off += rec_len;
            }
        }
        Ok(out)
    }

    pub fn find(&mut self, dir: u32, name: &str) -> Result<u32> {
        self.read_dir(dir)?
            .into_iter()
            .find(|e| e.name == name)
            .map(|e| e.inode)
            .ok_or(Error::NotFound)
    }

    /// Mutlak yolu (`/a/b`) inode numarasına çevirir.
    pub fn lookup(&mut self, path: &str) -> Result<u32> {
        let mut ino = ROOT_INODE;
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let inode = self.inode(ino)?;
            if !inode.is_dir() {
                return Err(Error::NotDirectory);
            }
            ino = self.find(ino, part)?;
        }
        Ok(ino)
    }

    fn entry_size(name_len: usize) -> usize {
        (8 + name_len).div_ceil(4) * 4
    }

    fn file_type(mode: u16) -> u8 {
        match mode & S_IFMT {
            S_IFDIR => FT_DIR,
            S_IFLNK => FT_SYMLINK,
            _ => FT_REG,
        }
    }

    fn add_entry(&mut self, dir: u32, name: &str, ino: u32, mode: u16) -> Result<()> {
        if name.is_empty() || name.len() > 255 || name.contains('/') {
            return Err(if name.len() > 255 {
                Error::NameTooLong
            } else {
                Error::InvalidPath
            });
        }
        let need = Self::entry_size(name.len());
        let mut inode = self.inode(dir)?;
        let kind = if self.filetype {
            Self::file_type(mode)
        } else {
            0
        };
        let write_entry = |data: &mut [u8], off: usize, rec_len: usize| {
            put32(data, off, ino);
            put16(data, off + 4, rec_len as u16);
            data[off + 6] = name.len() as u8;
            data[off + 7] = kind;
            data[off + 8..off + 8 + name.len()].copy_from_slice(name.as_bytes());
        };
        for (_, block) in self.dir_blocks(dir, &mut inode)? {
            let mut data = self.read_block(block)?;
            let mut off = 0;
            while off + 8 <= data.len() {
                let entry_ino = le32(&data, off);
                let rec_len = le16(&data, off + 4) as usize;
                if rec_len < 8 || off + rec_len > data.len() {
                    return Err(Error::Corrupt);
                }
                if entry_ino == 0 && rec_len >= need {
                    write_entry(&mut data, off, rec_len);
                    self.write_block(block, &data)?;
                    return self.touch_dir(dir, &mut inode);
                }
                let used = Self::entry_size(data[off + 6] as usize);
                if entry_ino != 0 && rec_len >= used + need {
                    put16(&mut data, off + 4, used as u16);
                    write_entry(&mut data, off + used, rec_len - used);
                    self.write_block(block, &data)?;
                    return self.touch_dir(dir, &mut inode);
                }
                off += rec_len;
            }
        }
        let index = inode.size.div_ceil(self.block_size as u64);
        let block = self
            .map_block(dir, &mut inode, index, true)?
            .ok_or(Error::Corrupt)?;
        let mut data = vec![0u8; self.block_size];
        write_entry(&mut data, 0, self.block_size);
        self.write_block(block, &data)?;
        inode.size += self.block_size as u64;
        self.touch_dir(dir, &mut inode)
    }

    fn touch_dir(&mut self, dir: u32, inode: &mut Inode) -> Result<()> {
        let now = self.now();
        inode.mtime = now;
        inode.ctime = now;
        inode.flags &= !INDEX_FLAG;
        self.write_inode(dir, inode)
    }

    fn remove_entry(&mut self, dir: u32, name: &str) -> Result<u32> {
        let mut inode = self.inode(dir)?;
        for (_, block) in self.dir_blocks(dir, &mut inode)? {
            let mut data = self.read_block(block)?;
            let mut off = 0;
            let mut previous: Option<usize> = None;
            while off + 8 <= data.len() {
                let entry_ino = le32(&data, off);
                let rec_len = le16(&data, off + 4) as usize;
                if rec_len < 8 || off + rec_len > data.len() {
                    return Err(Error::Corrupt);
                }
                let name_len = data[off + 6] as usize;
                if entry_ino != 0 && &data[off + 8..off + 8 + name_len] == name.as_bytes() {
                    match previous {
                        Some(prev) => {
                            let prev_len = le16(&data, prev + 4) as usize;
                            put16(&mut data, prev + 4, (prev_len + rec_len) as u16);
                        }
                        None => put32(&mut data, off, 0),
                    }
                    self.write_block(block, &data)?;
                    self.touch_dir(dir, &mut inode)?;
                    return Ok(entry_ino);
                }
                previous = Some(off);
                off += rec_len;
            }
        }
        Err(Error::NotFound)
    }

    fn split(path: &str) -> Result<(&str, &str)> {
        let path = path.trim_end_matches('/');
        let i = path.rfind('/').ok_or(Error::InvalidPath)?;
        let name = &path[i + 1..];
        if name.is_empty() || name == "." || name == ".." {
            return Err(Error::InvalidPath);
        }
        Ok((if i == 0 { "/" } else { &path[..i] }, name))
    }

    /// Dosya ya da dizin oluşturur; yeni inode numarasını döndürür.
    pub fn create(&mut self, path: &str, mode: u16, uid: u32, gid: u32) -> Result<u32> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let (parent_path, name) = Self::split(path)?;
        let parent = self.lookup(parent_path)?;
        if !self.inode(parent)?.is_dir() {
            return Err(Error::NotDirectory);
        }
        if self.find(parent, name).is_ok() {
            return Err(Error::Exists);
        }
        let dir = mode & S_IFMT == S_IFDIR;
        let goal = if dir {
            self.groups
                .iter()
                .enumerate()
                .max_by_key(|(_, g)| g.free_inodes)
                .map_or(0, |(i, _)| i)
        } else {
            self.group_of_inode(parent)
        };
        let ino = self.alloc_bit(true, goal)?;
        let now = self.now();
        let mut inode = Inode::empty(self.inode_size);
        if self.inode_size > INODE_SIZE {
            // Büyük inode'larda ek alan boyutu (i_extra_isize).
            put16(&mut inode.raw, 128, 32);
        }
        inode.mode = mode;
        inode.uid = uid;
        inode.gid = gid;
        inode.atime = now;
        inode.ctime = now;
        inode.mtime = now;
        inode.links = if dir { 2 } else { 1 };
        if dir {
            let block = self.alloc_bit(false, self.group_of_inode(ino))?;
            let bs = self.block_size;
            let mut data = vec![0u8; bs];
            put32(&mut data, 0, ino);
            put16(&mut data, 4, 12);
            data[6] = 1;
            data[7] = if self.filetype { FT_DIR } else { 0 };
            data[8] = b'.';
            put32(&mut data, 12, parent);
            put16(&mut data, 16, (bs - 12) as u16);
            data[18] = 2;
            data[19] = if self.filetype { FT_DIR } else { 0 };
            data[20] = b'.';
            data[21] = b'.';
            self.write_block(block, &data)?;
            inode.block[0] = block;
            inode.size = bs as u64;
            inode.sectors = self.sectors_per_block() as u32;
        }
        self.write_inode(ino, &inode)?;
        self.add_entry(parent, name, ino, mode)?;
        if dir {
            let mut p = self.inode(parent)?;
            p.links += 1;
            self.write_inode(parent, &p)?;
            let g = self.group_of_inode(ino);
            self.groups[g].used_dirs += 1;
            self.dirty_groups = true;
        }
        self.flush()?;
        Ok(ino)
    }

    fn release_inode(&mut self, ino: u32, inode: &mut Inode) -> Result<()> {
        self.free_blocks_from(inode, 0)?;
        inode.size = 0;
        inode.links = 0;
        inode.dtime = self.now();
        self.write_inode(ino, inode)?;
        self.free_bit(true, ino)
    }

    /// Dosyayı ya da boş dizini siler.
    pub fn remove(&mut self, path: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let (parent_path, name) = Self::split(path)?;
        let parent = self.lookup(parent_path)?;
        let ino = self.find(parent, name)?;
        let mut inode = self.inode(ino)?;
        if inode.is_dir() {
            let others = self
                .read_dir(ino)?
                .into_iter()
                .any(|e| e.name != "." && e.name != "..");
            if others {
                return Err(Error::NotEmpty);
            }
        }
        self.remove_entry(parent, name)?;
        if inode.is_dir() {
            let mut p = self.inode(parent)?;
            p.links = p.links.saturating_sub(1);
            self.write_inode(parent, &p)?;
            let g = self.group_of_inode(ino);
            self.groups[g].used_dirs = self.groups[g].used_dirs.saturating_sub(1);
            self.dirty_groups = true;
            self.release_inode(ino, &mut inode)?;
        } else {
            inode.links = inode.links.saturating_sub(1);
            if inode.links == 0 {
                self.release_inode(ino, &mut inode)?;
            } else {
                inode.ctime = self.now();
                self.write_inode(ino, &inode)?;
            }
        }
        self.flush()
    }

    /// Taşır / yeniden adlandırır; hedef dosya varsa üzerine yazılır.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let (from_parent_path, from_name) = Self::split(from)?;
        let (to_parent_path, to_name) = Self::split(to)?;
        let from_parent = self.lookup(from_parent_path)?;
        let to_parent = self.lookup(to_parent_path)?;
        let ino = self.find(from_parent, from_name)?;
        let inode = self.inode(ino)?;
        if inode.is_dir() {
            // Dizin kendi altına taşınamaz.
            let mut cursor = to_parent;
            while cursor != ROOT_INODE {
                if cursor == ino {
                    return Err(Error::InvalidPath);
                }
                cursor = self.find(cursor, "..")?;
            }
        }
        if let Ok(existing) = self.find(to_parent, to_name) {
            if existing == ino {
                return Ok(());
            }
            let target = self.inode(existing)?;
            if target.is_dir() != inode.is_dir() {
                return Err(if target.is_dir() {
                    Error::IsDirectory
                } else {
                    Error::NotDirectory
                });
            }
            self.remove(to)?;
        }
        self.add_entry(to_parent, to_name, ino, inode.mode)?;
        self.remove_entry(from_parent, from_name)?;
        if inode.is_dir() && from_parent != to_parent {
            let mut data = self.read_block(inode.block[0])?;
            put32(&mut data, 12, to_parent);
            self.write_block(inode.block[0], &data)?;
            let mut old = self.inode(from_parent)?;
            old.links = old.links.saturating_sub(1);
            self.write_inode(from_parent, &old)?;
            let mut new = self.inode(to_parent)?;
            new.links += 1;
            self.write_inode(to_parent, &new)?;
        }
        let mut moved = self.inode(ino)?;
        moved.ctime = self.now();
        self.write_inode(ino, &moved)?;
        self.flush()
    }

    pub fn set_mode(&mut self, ino: u32, permissions: u16) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let mut inode = self.inode(ino)?;
        inode.mode = (inode.mode & S_IFMT) | (permissions & 0o7777);
        inode.ctime = self.now();
        self.write_inode(ino, &inode)
    }

    pub fn set_owner(&mut self, ino: u32, uid: u32, gid: u32) -> Result<()> {
        if self.read_only {
            return Err(Error::ReadOnly);
        }
        let mut inode = self.inode(ino)?;
        inode.uid = uid;
        inode.gid = gid;
        inode.ctime = self.now();
        self.write_inode(ino, &inode)
    }

    /// Kısa sembolik bağlantının hedefi (inode içinde saklananlar).
    pub fn read_link(&mut self, ino: u32) -> Result<String> {
        let mut inode = self.inode(ino)?;
        if !inode.is_symlink() {
            return Err(Error::InvalidPath);
        }
        let size = inode.size as usize;
        if size < 60 && inode.sectors == 0 {
            let mut bytes = Vec::with_capacity(60);
            for b in inode.block {
                bytes.extend_from_slice(&b.to_le_bytes());
            }
            return Ok(String::from_utf8_lossy(&bytes[..size]).into_owned());
        }
        let mut buf = vec![0u8; size.min(4096)];
        let block = self
            .map_block(ino, &mut inode, 0, false)?
            .ok_or(Error::Corrupt)?;
        let data = self.read_block(block)?;
        let n = buf.len().min(data.len());
        buf[..n].copy_from_slice(&data[..n]);
        Ok(String::from_utf8_lossy(&buf[..n]).into_owned())
    }

    /// Dizin dizini (htree) özelliği yazmaya uygun değilse bilgi verir.
    pub fn has_dir_index(&self) -> bool {
        le32(&self.superblock, 92) & COMPAT_DIR_INDEX != 0
    }

    // ---- denetim (fsck) ----

    /// Blok bit eşlemi dışında kalan ve gruplara ait meta veri blokları.
    fn metadata_blocks(&self) -> Vec<u32> {
        let sparse = le32(&self.superblock, 100) & RO_COMPAT_SPARSE_SUPER != 0;
        let resize = le32(&self.superblock, 92) & COMPAT_RESIZE_INODE != 0;
        let reserved = if resize {
            le16(&self.superblock, 206) as u32
        } else {
            0
        };
        let gdt_blocks = (self.groups.len() * 32).div_ceil(self.block_size) as u32;
        let table_blocks =
            (self.inodes_per_group as usize * self.inode_size).div_ceil(self.block_size) as u32;
        let mut out = Vec::new();
        for (g, group) in self.groups.iter().enumerate() {
            let start = self.first_data_block + g as u32 * self.blocks_per_group;
            if !sparse || has_super(g as u32) {
                out.extend(start..start + 1 + gdt_blocks + reserved);
            }
            out.push(group.block_bitmap);
            out.push(group.inode_bitmap);
            out.extend(group.inode_table..group.inode_table + table_blocks);
        }
        out
    }

    /// Dosyanın bloklarını (dolaylı bloklar dahil) toplar; aralık dışındaki işaretçiler
    /// `bad`a eklenir ve `repair` ise sıfırlanır.
    fn check_tree(
        &mut self,
        block: u32,
        depth: u32,
        repair: bool,
        out: &mut Vec<u32>,
        bad: &mut u32,
    ) -> Result<()> {
        out.push(block);
        if depth == 0 {
            return Ok(());
        }
        let mut table = self.read_block(block)?;
        let mut changed = false;
        for i in 0..self.block_size / 4 {
            let child = le32(&table, i * 4);
            if child == 0 {
                continue;
            }
            if child < self.first_data_block || child >= self.blocks_count {
                *bad += 1;
                if repair {
                    put32(&mut table, i * 4, 0);
                    changed = true;
                }
                continue;
            }
            self.check_tree(child, depth - 1, repair, out, bad)?;
        }
        if changed {
            self.write_block(block, &table)?;
        }
        Ok(())
    }

    fn inode_blocks(&mut self, ino: u32, repair: bool, bad: &mut u32) -> Result<Vec<u32>> {
        let mut inode = self.inode(ino)?;
        let mut out = Vec::new();
        // Kısa sembolik bağlar hedefi blok dizisinde tutar.
        if inode.is_symlink() && inode.sectors == 0 {
            return Ok(out);
        }
        let mut changed = false;
        #[allow(clippy::needless_range_loop)]
        for slot in 0..15 {
            let b = inode.block[slot];
            if b == 0 {
                continue;
            }
            if b < self.first_data_block || b >= self.blocks_count {
                *bad += 1;
                if repair {
                    inode.block[slot] = 0;
                    changed = true;
                }
                continue;
            }
            let depth = slot.saturating_sub(DIRECT - 1) as u32;
            self.check_tree(b, depth, repair, &mut out, bad)?;
        }
        if changed {
            self.write_inode(ino, &inode)?;
        }
        Ok(out)
    }

    fn bit(bitmap: &[u8], i: u32) -> bool {
        bitmap[(i / 8) as usize] & (1 << (i % 8)) != 0
    }

    /// `dir` altındaki ağacı gezer; girdileri sayar, bozuk girdileri (`repair` ise) siler.
    fn check_walk(
        &mut self,
        dir: u32,
        repair: bool,
        walk: &mut Walk,
        report: &mut CheckReport,
        log: &mut impl FnMut(&str),
    ) -> Result<()> {
        let n_inodes = walk.refs.len();
        let mut stack = vec![dir];
        while let Some(dir) = stack.pop() {
            report.directories += 1;
            let Ok(entries) = self.read_dir(dir) else {
                report.problems += 1;
                log(&alloc::format!("inode {}: dizin okunamadı (bozuk)", dir));
                continue;
            };
            for e in entries {
                let ino = e.inode as usize;
                if ino >= n_inodes {
                    report.problems += 1;
                    log(&alloc::format!(
                        "inode {}: '{}' geçersiz inode {} gösteriyor",
                        dir,
                        e.name,
                        e.inode
                    ));
                    if repair && self.remove_entry(dir, &e.name).is_ok() {
                        report.fixed += 1;
                    }
                    continue;
                }
                if e.name == "." || e.name == ".." {
                    walk.refs[ino] = walk.refs[ino].saturating_add(1);
                    continue;
                }
                let child = self.inode(e.inode)?;
                if child.mode == 0 || child.dtime != 0 {
                    report.problems += 1;
                    log(&alloc::format!(
                        "'{}' silinmiş inode {} gösteriyor",
                        e.name,
                        e.inode
                    ));
                    if repair && self.remove_entry(dir, &e.name).is_ok() {
                        report.fixed += 1;
                    }
                    continue;
                }
                walk.refs[ino] = walk.refs[ino].saturating_add(1);
                if walk.reachable[ino] {
                    if child.is_dir() {
                        report.problems += 1;
                        log(&alloc::format!(
                            "dizin inode {} birden fazla yerde bağlı",
                            e.inode
                        ));
                    }
                    continue;
                }
                walk.reachable[ino] = true;
                if child.is_dir() {
                    stack.push(e.inode);
                } else {
                    report.files += 1;
                }
            }
        }
        Ok(())
    }

    /// Dosya sistemini denetler; `repair` ise bulunan sorunları düzeltir.
    /// Aygıt bu sırada başka biri tarafından yazılmamalıdır.
    pub fn check(&mut self, repair: bool, mut log: impl FnMut(&str)) -> Result<CheckReport> {
        if repair && self.read_only {
            return Err(Error::ReadOnly);
        }
        let mut report = CheckReport::default();
        let n_inodes = self.inodes_count as usize + 1;
        let n_blocks = self.blocks_count as usize;
        let mut used_blocks = vec![false; n_blocks];
        for b in self.metadata_blocks() {
            if (b as usize) < n_blocks {
                used_blocks[b as usize] = true;
            }
        }

        // 1. Dizin ağacı: erişilebilir inode'lar ve bağlantı sayıları.
        log("1/4 dizin ağacı taranıyor");
        let mut walk = Walk {
            refs: vec![0u16; n_inodes],
            reachable: vec![false; n_inodes],
        };
        walk.reachable[ROOT_INODE as usize] = true;
        self.check_walk(ROOT_INODE, repair, &mut walk, &mut report, &mut log)?;

        // 2. Ayrılmış ama bağlantısız inode'lar: lost+found'a bağlanır (önce dizinler).
        log("2/4 bağlantısız inode'lar aranıyor");
        let first = if le32(&self.superblock, 76) >= 1 {
            le32(&self.superblock, 84)
        } else {
            FIRST_INODE
        };
        let mut orphans = Vec::new();
        for g in 0..self.groups.len() {
            let bitmap = self.read_block(self.groups[g].inode_bitmap)?;
            for i in 0..self.inodes_per_group {
                let ino = g as u32 * self.inodes_per_group + i + 1;
                if ino > self.inodes_count
                    || ino < first
                    || walk.reachable[ino as usize]
                    || !Self::bit(&bitmap, i)
                {
                    continue;
                }
                let inode = self.inode(ino)?;
                if inode.mode != 0 && inode.links > 0 && inode.dtime == 0 {
                    orphans.push((!inode.is_dir(), ino));
                }
            }
        }
        orphans.sort();
        if !orphans.is_empty() {
            report.problems += orphans.len() as u32;
            log(&alloc::format!(
                "{} bağlantısız inode bulundu",
                orphans.len()
            ));
        }
        if repair && !orphans.is_empty() {
            let lost = match self.find(ROOT_INODE, "lost+found") {
                Ok(i) => i,
                Err(_) => {
                    let i = self.create("/lost+found", S_IFDIR | 0o700, 0, 0)?;
                    walk.reachable[i as usize] = true;
                    self.check_walk(i, false, &mut walk, &mut report, &mut log)?;
                    walk.refs[i as usize] += 1;
                    i
                }
            };
            for (_, ino) in orphans {
                if walk.reachable[ino as usize] {
                    continue;
                }
                let inode = self.inode(ino)?;
                if self
                    .add_entry(lost, &alloc::format!("#{}", ino), ino, inode.mode)
                    .is_err()
                {
                    continue;
                }
                walk.reachable[ino as usize] = true;
                walk.refs[ino as usize] += 1;
                if inode.is_dir() {
                    // ".." eski üst dizini gösterir; yeni üst dizin lost+found.
                    let _ = self.remove_entry(ino, "..");
                    let _ = self.add_entry(ino, "..", lost, S_IFDIR);
                    self.check_walk(ino, repair, &mut walk, &mut report, &mut log)?;
                }
                report.fixed += 1;
            }
            log("bağlantısız dosyalar /lost+found altına taşındı");
        }
        let reachable = walk.reachable;
        let refs = walk.refs;

        // 3. Bağlantı sayıları ve blok kullanımı.
        log("3/4 bağlantı sayıları ve bloklar denetleniyor");
        let mut dirs_per_group = vec![0u16; self.groups.len()];
        for ino in 1..n_inodes as u32 {
            let reserved = ino < first && ino != ROOT_INODE;
            if !reachable[ino as usize] && !reserved {
                continue;
            }
            let mut inode = self.inode(ino)?;
            if reserved && inode.mode == 0 && inode.block.iter().all(|b| *b == 0) {
                continue;
            }
            if inode.is_dir() && !reserved {
                dirs_per_group[self.group_of_inode(ino)] += 1;
            }
            if !reserved && inode.links != refs[ino as usize] {
                report.problems += 1;
                log(&alloc::format!(
                    "inode {}: bağlantı sayısı {} (olması gereken {})",
                    ino,
                    inode.links,
                    refs[ino as usize]
                ));
                if repair {
                    inode.links = refs[ino as usize];
                    self.write_inode(ino, &inode)?;
                    report.fixed += 1;
                }
            }
            let mut bad = 0;
            let blocks = self.inode_blocks(ino, repair, &mut bad)?;
            if bad > 0 {
                report.problems += bad;
                log(&alloc::format!(
                    "inode {}: {} geçersiz blok işaretçisi",
                    ino,
                    bad
                ));
                if repair {
                    report.fixed += bad;
                }
            }
            let sectors = blocks.len() as u32 * self.sectors_per_block() as u32;
            let fast_link = inode.is_symlink() && inode.sectors == 0;
            if !fast_link && !reserved && inode.sectors != sectors {
                report.problems += 1;
                log(&alloc::format!(
                    "inode {}: ayrılmış alan {} sektör (olması gereken {})",
                    ino,
                    inode.sectors,
                    sectors
                ));
                if repair {
                    let mut fresh = self.inode(ino)?;
                    fresh.sectors = sectors;
                    self.write_inode(ino, &fresh)?;
                    report.fixed += 1;
                }
            }
            for b in blocks {
                let slot = &mut used_blocks[b as usize];
                // Yeniden boyutlandırma inode'u ayrılmış GDT bloklarını zaten sahiplenir.
                if *slot && ino != 7 {
                    report.problems += 1;
                    log(&alloc::format!(
                        "blok {} birden fazla yerde kullanılıyor (inode {})",
                        b,
                        ino
                    ));
                }
                *slot = true;
            }
            report.inodes += 1;
        }

        // 4. Bit eşlemleri ve sayaçlar.
        log("4/4 bit eşlemleri ve boş alan sayaçları denetleniyor");
        let mut total_free_blocks = 0u32;
        let mut total_free_inodes = 0u32;
        for g in 0..self.groups.len() {
            let start = self.first_data_block + g as u32 * self.blocks_per_group;
            let count = (self.blocks_count - start).min(self.blocks_per_group);
            let mut bitmap = self.read_block(self.groups[g].block_bitmap)?;
            let mut changed = false;
            let mut free = 0u32;
            for i in 0..count {
                let used = used_blocks[(start + i) as usize];
                if used != Self::bit(&bitmap, i) {
                    if used {
                        report.problems += 1;
                        log(&alloc::format!(
                            "blok {} kullanımda ama boş işaretli",
                            start + i
                        ));
                    } else {
                        report.leaked_blocks += 1;
                    }
                    if repair {
                        bitmap[(i / 8) as usize] ^= 1 << (i % 8);
                        changed = true;
                    }
                }
                if !used {
                    free += 1;
                }
            }
            if changed {
                self.write_block(self.groups[g].block_bitmap, &bitmap)?;
            }
            let mut ibitmap = self.read_block(self.groups[g].inode_bitmap)?;
            let mut ichanged = false;
            let mut ifree = 0u32;
            for i in 0..self.inodes_per_group {
                let ino = g as u32 * self.inodes_per_group + i + 1;
                if ino > self.inodes_count {
                    break;
                }
                let used = ino < first || reachable[ino as usize];
                if used != Self::bit(&ibitmap, i) {
                    if used {
                        report.problems += 1;
                        log(&alloc::format!("inode {} kullanımda ama boş işaretli", ino));
                    } else {
                        report.leaked_inodes += 1;
                    }
                    if repair {
                        ibitmap[(i / 8) as usize] ^= 1 << (i % 8);
                        ichanged = true;
                    }
                }
                if !used {
                    ifree += 1;
                }
            }
            if ichanged {
                self.write_block(self.groups[g].inode_bitmap, &ibitmap)?;
            }
            let group = self.groups[g];
            if group.free_blocks as u32 != free
                || group.free_inodes as u32 != ifree
                || group.used_dirs != dirs_per_group[g]
            {
                report.counters += 1;
                if repair {
                    self.groups[g].free_blocks = free as u16;
                    self.groups[g].free_inodes = ifree as u16;
                    self.groups[g].used_dirs = dirs_per_group[g];
                    self.dirty_groups = true;
                }
            }
            total_free_blocks += free;
            total_free_inodes += ifree;
        }
        if self.free_blocks != total_free_blocks || self.free_inodes != total_free_inodes {
            report.counters += 1;
        }
        if report.leaked_blocks > 0 {
            log(&alloc::format!(
                "{} blok boşta ama dolu işaretli",
                report.leaked_blocks
            ));
        }
        if report.leaked_inodes > 0 {
            log(&alloc::format!(
                "{} inode boşta ama dolu işaretli",
                report.leaked_inodes
            ));
        }
        if report.counters > 0 {
            log(&alloc::format!(
                "{} boş alan sayacı hatalı",
                report.counters
            ));
        }
        report.free_blocks = total_free_blocks;
        report.free_inodes = total_free_inodes;
        if repair {
            if report.leaked_blocks + report.leaked_inodes + report.counters > 0 {
                report.fixed += report.leaked_blocks + report.leaked_inodes + report.counters;
            }
            self.free_blocks = total_free_blocks;
            self.free_inodes = total_free_inodes;
            // Temiz kapatılmış durum ve son denetim zamanı.
            let now = self.now();
            put16(&mut self.superblock, 58, 1);
            put32(&mut self.superblock, 64, now);
            put16(&mut self.superblock, 52, 0);
            self.dirty_groups = true;
            self.flush()?;
        }
        Ok(report)
    }
}

struct Walk {
    refs: Vec<u16>,
    reachable: Vec<bool>,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct CheckReport {
    pub directories: u32,
    pub files: u32,
    /// Denetlenen (kullanımdaki) inode sayısı.
    pub inodes: u32,
    pub problems: u32,
    pub leaked_blocks: u32,
    pub leaked_inodes: u32,
    pub counters: u32,
    pub fixed: u32,
    pub free_blocks: u32,
    pub free_inodes: u32,
}

impl CheckReport {
    /// Düzeltilmesi gereken bir şey var mı?
    pub fn clean(&self) -> bool {
        self.problems == 0
            && self.leaked_blocks == 0
            && self.leaked_inodes == 0
            && self.counters == 0
    }
}
