//! exFAT: okuma/yazma (USB bellekler ve SD kartlar çoğunlukla exFAT gelir).
//!
//! Ayırma bit eşlemi bellekte tutulur ve her işlemden sonra diske yazılır; dosyalar ya
//! FAT zinciriyle ya da bitişik (NoFatChain) saklanır. Girdi kümeleri (dosya + akış + ad)
//! sağlama toplamı ve büyük harf tablosuyla ad özeti hesaplanarak yazılır.

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::{FsError, Kind, Metadata, Result};
use crate::drivers::rtc;
use crate::sched::Mutex;
use crate::storage::{BlockDevice, SECTOR};

const ATTR_READ_ONLY: u16 = 0x01;
const ATTR_DIRECTORY: u16 = 0x10;
const ATTR_ARCHIVE: u16 = 0x20;
const TYPE_BITMAP: u8 = 0x81;
const TYPE_UPCASE: u8 = 0x82;
const TYPE_LABEL: u8 = 0x83;
const TYPE_FILE: u8 = 0x85;
const TYPE_STREAM: u8 = 0xc0;
const TYPE_NAME: u8 = 0xc1;
const FLAG_ALLOCATION_POSSIBLE: u8 = 1;
const FLAG_NO_FAT_CHAIN: u8 = 2;
const END_OF_CHAIN: u32 = 0xffff_ffff;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub attr: u16,
    pub cluster: u32,
    pub size: u64,
    valid: u64,
    contiguous: bool,
    /// Girdi kümesinin diskteki yerleri (sektör, sektör içi konum); kök için boş.
    slots: Vec<(u64, usize)>,
}

impl Entry {
    pub fn is_dir(&self) -> bool {
        self.attr & ATTR_DIRECTORY != 0
    }

    pub fn metadata(&self) -> Metadata {
        Metadata {
            name: self.name.clone(),
            kind: if self.is_dir() {
                Kind::Directory
            } else {
                Kind::File
            },
            size: if self.is_dir() { 0 } else { self.size },
            read_only: self.attr & ATTR_READ_ONLY != 0,
            mode: if self.attr & ATTR_READ_ONLY != 0 {
                0o555
            } else {
                0o755
            },
            uid: 0,
            gid: 0,
            mtime: 0,
        }
    }
}

struct State {
    bitmap: Vec<u8>,
    bitmap_dirty: bool,
    free_hint: u32,
}

pub struct Exfat {
    dev: Arc<dyn BlockDevice>,
    /// Aygıt sektörü (512 bayt) cinsinden.
    fat_start: u64,
    heap_start: u64,
    sectors_per_cluster: u64,
    cluster_count: u32,
    root_cluster: u32,
    bitmap_cluster: u32,
    bitmap_length: u64,
    upcase: Vec<u16>,
    pub label: String,
    state: Mutex<State>,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}
fn le64(b: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(b[at..at + 8].try_into().unwrap())
}

/// exFAT zaman damgası (DOS biçimi, yerel saat).
fn timestamp() -> u32 {
    match rtc::now().map(|t| t.to_local()) {
        Some(t) => {
            let date =
                ((t.year.saturating_sub(1980)) << 9) as u32 | (t.month as u32) << 5 | t.day as u32;
            let time = (t.hour as u32) << 11 | (t.minute as u32) << 5 | (t.second as u32 / 2);
            date << 16 | time
        }
        None => 0x0021_0000,
    }
}

/// Girdi kümesi sağlama toplamı (ilk girdinin 2. ve 3. baytları hariç).
fn set_checksum(entries: &[[u8; 32]]) -> u16 {
    let mut sum: u16 = 0;
    for (i, entry) in entries.iter().enumerate() {
        for (j, &b) in entry.iter().enumerate() {
            if i == 0 && (j == 2 || j == 3) {
                continue;
            }
            sum = sum.rotate_right(1).wrapping_add(b as u16);
        }
    }
    sum
}

impl Exfat {
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        let mut boot = vec![0u8; SECTOR];
        dev.read(0, &mut boot).map_err(|_| FsError::Io)?;
        if &boot[3..11] != b"EXFAT   " || boot[510] != 0x55 || boot[511] != 0xaa {
            return Err(FsError::NotSupported);
        }
        let sector_shift = boot[108] as u32;
        let cluster_shift = boot[109] as u32;
        if !(9..=12).contains(&sector_shift) || cluster_shift > 25 - sector_shift {
            return Err(FsError::Corrupt);
        }
        let ratio = 1u64 << (sector_shift - 9);
        let mut fs = Self {
            dev,
            fat_start: le32(&boot, 80) as u64 * ratio,
            heap_start: le32(&boot, 88) as u64 * ratio,
            sectors_per_cluster: (1u64 << cluster_shift) * ratio,
            cluster_count: le32(&boot, 92),
            root_cluster: le32(&boot, 96),
            bitmap_cluster: 0,
            bitmap_length: 0,
            upcase: Vec::new(),
            label: String::new(),
            state: Mutex::new(State {
                bitmap: Vec::new(),
                bitmap_dirty: false,
                free_hint: 2,
            }),
        };
        // Kök dizindeki özel girdiler: bit eşlemi, büyük harf tablosu, etiket.
        let root = fs.root();
        let raw = fs.read_dir_raw(&root)?;
        let mut upcase = None;
        for e in &raw {
            match e[0] {
                TYPE_BITMAP if e[1] & 1 == 0 => {
                    fs.bitmap_cluster = le32(e, 20);
                    fs.bitmap_length = le64(e, 24);
                }
                TYPE_UPCASE => upcase = Some((le32(e, 20), le64(e, 24))),
                TYPE_LABEL => {
                    let n = (e[1] as usize).min(11);
                    let units: Vec<u16> = (0..n).map(|i| le16(e, 2 + i * 2)).collect();
                    fs.label = String::from_utf16_lossy(&units);
                }
                _ => {}
            }
        }
        if fs.bitmap_cluster < 2 {
            return Err(FsError::Corrupt);
        }
        let bitmap = fs.read_chain_bytes(fs.bitmap_cluster, fs.bitmap_length, false)?;
        fs.state.lock().bitmap = bitmap;
        fs.upcase = match upcase {
            Some((cluster, len)) => {
                let raw = fs.read_chain_bytes(cluster, len, false)?;
                decompress_upcase(&raw)
            }
            None => (0..=0xffffu32).map(|c| c as u16).collect(),
        };
        Ok(fs)
    }

    fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    fn cluster_lba(&self, cluster: u32) -> u64 {
        self.heap_start + (cluster as u64 - 2) * self.sectors_per_cluster
    }

    pub fn capacity(&self) -> (u64, u64) {
        let state = self.state.lock();
        let used: u64 = (0..self.cluster_count as usize)
            .filter(|&i| {
                state
                    .bitmap
                    .get(i / 8)
                    .is_some_and(|b| b >> (i % 8) & 1 != 0)
            })
            .count() as u64;
        let bytes = self.cluster_bytes() as u64;
        (
            self.cluster_count as u64 * bytes,
            (self.cluster_count as u64 - used) * bytes,
        )
    }

    pub fn root(&self) -> Entry {
        Entry {
            name: String::from("/"),
            attr: ATTR_DIRECTORY,
            cluster: self.root_cluster,
            size: 0,
            valid: 0,
            contiguous: false,
            slots: Vec::new(),
        }
    }

    // ---- FAT ve kümeler ----

    fn fat_get(&self, cluster: u32) -> Result<u32> {
        let offset = cluster as u64 * 4;
        let mut buf = vec![0u8; SECTOR];
        self.dev
            .read(self.fat_start + offset / SECTOR as u64, &mut buf)
            .map_err(|_| FsError::Io)?;
        Ok(le32(&buf, (offset % SECTOR as u64) as usize))
    }

    fn fat_set(&self, cluster: u32, value: u32) -> Result<()> {
        let offset = cluster as u64 * 4;
        let sector = self.fat_start + offset / SECTOR as u64;
        let mut buf = vec![0u8; SECTOR];
        self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
        let at = (offset % SECTOR as u64) as usize;
        buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
        self.dev.write(sector, &buf).map_err(|_| FsError::Io)
    }

    /// Küme listesi: bitişikse ardışık, değilse FAT zinciri.
    fn chain(&self, first: u32, length: u64, contiguous: bool) -> Result<Vec<u32>> {
        if first < 2 {
            return Ok(Vec::new());
        }
        let csize = self.cluster_bytes() as u64;
        if contiguous {
            let count = length.div_ceil(csize).max(1) as u32;
            return Ok((first..first + count).collect());
        }
        let mut out = Vec::new();
        let mut c = first;
        while (2..self.cluster_count + 2).contains(&c) {
            out.push(c);
            if out.len() > self.cluster_count as usize {
                return Err(FsError::Corrupt);
            }
            c = self.fat_get(c)?;
        }
        Ok(out)
    }

    fn read_chain_bytes(&self, first: u32, length: u64, contiguous: bool) -> Result<Vec<u8>> {
        let chain = self.chain(first, length, contiguous)?;
        let csize = self.cluster_bytes();
        let mut out = vec![0u8; chain.len() * csize];
        for (i, c) in chain.iter().enumerate() {
            self.dev
                .read(self.cluster_lba(*c), &mut out[i * csize..(i + 1) * csize])
                .map_err(|_| FsError::Io)?;
        }
        out.truncate(length as usize);
        Ok(out)
    }

    fn is_free(state: &State, cluster: u32) -> bool {
        let i = (cluster - 2) as usize;
        state
            .bitmap
            .get(i / 8)
            .is_some_and(|b| b >> (i % 8) & 1 == 0)
    }

    fn mark(state: &mut State, cluster: u32, used: bool) {
        let i = (cluster - 2) as usize;
        if let Some(b) = state.bitmap.get_mut(i / 8) {
            if used {
                *b |= 1 << (i % 8);
            } else {
                *b &= !(1 << (i % 8));
            }
            state.bitmap_dirty = true;
        }
    }

    fn find_free(&self, state: &mut State, prefer: Option<u32>) -> Result<u32> {
        if let Some(p) = prefer
            && p < self.cluster_count + 2
            && Self::is_free(state, p)
        {
            return Ok(p);
        }
        let start = state.free_hint.max(2);
        for k in 0..self.cluster_count {
            let c = 2 + (start - 2 + k) % self.cluster_count;
            if Self::is_free(state, c) {
                state.free_hint = c + 1;
                return Ok(c);
            }
        }
        Err(FsError::NoSpace)
    }

    fn flush_bitmap(&self, state: &mut State) -> Result<()> {
        if !state.bitmap_dirty {
            return Ok(());
        }
        let chain = self.chain(self.bitmap_cluster, self.bitmap_length, false)?;
        let csize = self.cluster_bytes();
        for (i, c) in chain.iter().enumerate() {
            let mut buf = vec![0u8; csize];
            let start = i * csize;
            let end = (start + csize).min(state.bitmap.len());
            if start < end {
                buf[..end - start].copy_from_slice(&state.bitmap[start..end]);
            }
            self.dev
                .write(self.cluster_lba(*c), &buf)
                .map_err(|_| FsError::Io)?;
        }
        state.bitmap_dirty = false;
        Ok(())
    }

    /// Dosyayı `clusters` kümeye büyütür; gerekirse bitişik düzenden FAT zincirine geçer.
    fn grow(&self, state: &mut State, entry: &mut Entry, clusters: usize) -> Result<Vec<u32>> {
        let mut chain = self.chain(entry.cluster, entry.size.max(entry.valid), entry.contiguous)?;
        if entry.cluster < 2 {
            chain.clear();
        }
        while chain.len() < clusters {
            let prefer = chain.last().map(|c| c + 1);
            let new = self.find_free(state, prefer)?;
            Self::mark(state, new, true);
            match chain.last().copied() {
                None => {
                    entry.cluster = new;
                    entry.contiguous = true;
                }
                Some(last) => {
                    if entry.contiguous && new != last + 1 {
                        // Bitişiklik bozuldu: mevcut kümeler için FAT zinciri yaz.
                        for w in chain.windows(2) {
                            self.fat_set(w[0], w[1])?;
                        }
                        entry.contiguous = false;
                    }
                    if !entry.contiguous {
                        self.fat_set(last, new)?;
                    }
                }
            }
            if !entry.contiguous {
                self.fat_set(new, END_OF_CHAIN)?;
            }
            chain.push(new);
        }
        Ok(chain)
    }

    fn free_all(&self, state: &mut State, entry: &Entry) -> Result<()> {
        for c in self.chain(entry.cluster, entry.size.max(entry.valid), entry.contiguous)? {
            Self::mark(state, c, false);
            if !entry.contiguous {
                self.fat_set(c, 0)?;
            }
        }
        Ok(())
    }

    // ---- dizinler ----

    fn dir_length(&self, dir: &Entry) -> u64 {
        if dir.slots.is_empty() { 0 } else { dir.size }
    }

    /// Dizin kümelerindeki tüm 32 baytlık girdiler.
    fn read_dir_raw(&self, dir: &Entry) -> Result<Vec<[u8; 32]>> {
        let data = if dir.slots.is_empty() {
            let chain = self.chain(dir.cluster, 0, false)?;
            let len = chain.len() as u64 * self.cluster_bytes() as u64;
            self.read_chain_bytes(dir.cluster, len, false)?
        } else {
            self.read_chain_bytes(dir.cluster, self.dir_length(dir), dir.contiguous)?
        };
        Ok(data.as_chunks::<32>().0.to_vec())
    }

    /// Dizindeki i'inci girdinin diskteki yeri.
    fn slot_location(&self, chain: &[u32], index: usize) -> Option<(u64, usize)> {
        let per_cluster = self.cluster_bytes() / 32;
        let cluster = *chain.get(index / per_cluster)?;
        let byte = (index % per_cluster) * 32;
        Some((
            self.cluster_lba(cluster) + (byte / SECTOR) as u64,
            byte % SECTOR,
        ))
    }

    fn dir_chain(&self, dir: &Entry) -> Result<Vec<u32>> {
        if dir.slots.is_empty() {
            self.chain(dir.cluster, 0, false)
        } else {
            self.chain(dir.cluster, self.dir_length(dir), dir.contiguous)
        }
    }

    pub fn read_dir(&self, dir: &Entry) -> Result<Vec<Entry>> {
        let raw = self.read_dir_raw(dir)?;
        let chain = self.dir_chain(dir)?;
        let mut out = Vec::new();
        let mut i = 0;
        while i < raw.len() {
            let e = &raw[i];
            if e[0] == 0 {
                break;
            }
            if e[0] != TYPE_FILE {
                i += 1;
                continue;
            }
            let secondary = e[1] as usize;
            if secondary < 2
                || i + secondary > raw.len()
                || raw.get(i + 1).map(|s| s[0]) != Some(TYPE_STREAM)
            {
                i += 1;
                continue;
            }
            let stream = &raw[i + 1];
            let name_len = stream[3] as usize;
            let mut units = Vec::with_capacity(name_len);
            for k in 0..secondary - 1 {
                let Some(n) = raw.get(i + 2 + k) else { break };
                if n[0] != TYPE_NAME {
                    break;
                }
                for u in 0..15 {
                    units.push(le16(n, 2 + u * 2));
                }
            }
            units.truncate(name_len);
            let slots = (0..=secondary)
                .filter_map(|k| self.slot_location(&chain, i + k))
                .collect();
            out.push(Entry {
                name: String::from_utf16_lossy(&units),
                attr: le16(e, 4),
                cluster: le32(stream, 20),
                size: le64(stream, 24),
                valid: le64(stream, 8),
                contiguous: stream[1] & FLAG_NO_FAT_CHAIN != 0,
                slots,
            });
            i += secondary + 1;
        }
        Ok(out)
    }

    fn upper(&self, unit: u16) -> u16 {
        self.upcase.get(unit as usize).copied().unwrap_or(unit)
    }

    fn same_name(&self, a: &str, b: &str) -> bool {
        let ua: Vec<u16> = a.encode_utf16().map(|u| self.upper(u)).collect();
        let ub: Vec<u16> = b.encode_utf16().map(|u| self.upper(u)).collect();
        ua == ub
    }

    fn name_hash(&self, name: &[u16]) -> u16 {
        let mut hash: u16 = 0;
        for &u in name {
            for b in self.upper(u).to_le_bytes() {
                hash = hash.rotate_right(1).wrapping_add(b as u16);
            }
        }
        hash
    }

    pub fn lookup(&self, path: &str) -> Result<Entry> {
        let mut current = self.root();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            if !current.is_dir() {
                return Err(FsError::NotDirectory);
            }
            current = self
                .read_dir(&current)?
                .into_iter()
                .find(|e| self.same_name(&e.name, part))
                .ok_or(FsError::NotFound)?;
        }
        Ok(current)
    }

    // ---- dosya içeriği ----

    pub fn read(&self, entry: &Entry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if entry.is_dir() {
            return Err(FsError::IsDirectory);
        }
        if offset >= entry.size {
            return Ok(0);
        }
        let len = buf.len().min((entry.size - offset) as usize);
        let chain = self.chain(entry.cluster, entry.size, entry.contiguous)?;
        let csize = self.cluster_bytes();
        let mut cluster_buf = vec![0u8; csize];
        let mut done = 0;
        while done < len {
            let pos = offset as usize + done;
            let take = (csize - pos % csize).min(len - done);
            if pos as u64 >= entry.valid {
                // Geçerli veri sınırının ötesi sıfır okunur.
                buf[done..done + take].fill(0);
            } else {
                let cluster = *chain.get(pos / csize).ok_or(FsError::Corrupt)?;
                self.dev
                    .read(self.cluster_lba(cluster), &mut cluster_buf)
                    .map_err(|_| FsError::Io)?;
                let within = pos % csize;
                buf[done..done + take].copy_from_slice(&cluster_buf[within..within + take]);
            }
            done += take;
        }
        Ok(len)
    }

    /// Girdi kümesini (dosya + akış + adlar) yeniden yazar.
    fn write_set(&self, entry: &Entry) -> Result<()> {
        if entry.slots.is_empty() {
            return Ok(());
        }
        let mut set: Vec<[u8; 32]> = Vec::new();
        for &(sector, offset) in &entry.slots {
            let mut buf = vec![0u8; SECTOR];
            self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
            set.push(buf[offset..offset + 32].try_into().unwrap());
        }
        let now = timestamp();
        set[0][4..6].copy_from_slice(&entry.attr.to_le_bytes());
        set[0][12..16].copy_from_slice(&now.to_le_bytes());
        set[0][16..20].copy_from_slice(&now.to_le_bytes());
        let stream = &mut set[1];
        stream[1] = FLAG_ALLOCATION_POSSIBLE
            | if entry.contiguous {
                FLAG_NO_FAT_CHAIN
            } else {
                0
            };
        stream[8..16].copy_from_slice(&entry.valid.to_le_bytes());
        stream[20..24]
            .copy_from_slice(&(if entry.cluster >= 2 { entry.cluster } else { 0 }).to_le_bytes());
        stream[24..32].copy_from_slice(&entry.size.to_le_bytes());
        let checksum = set_checksum(&set);
        set[0][2..4].copy_from_slice(&checksum.to_le_bytes());
        for (raw, &(sector, offset)) in set.iter().zip(&entry.slots) {
            let mut buf = vec![0u8; SECTOR];
            self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
            buf[offset..offset + 32].copy_from_slice(raw);
            self.dev.write(sector, &buf).map_err(|_| FsError::Io)?;
        }
        Ok(())
    }

    pub fn write(&self, entry: &mut Entry, offset: u64, data: &[u8]) -> Result<usize> {
        if entry.is_dir() {
            return Err(FsError::IsDirectory);
        }
        let end = offset + data.len() as u64;
        let csize = self.cluster_bytes();
        let chain = {
            let mut state = self.state.lock();
            let chain = self.grow(&mut state, entry, (end as usize).div_ceil(csize).max(1))?;
            self.flush_bitmap(&mut state)?;
            chain
        };
        // Geçerli veri sınırıyla yazma başlangıcı arası sıfırlanır.
        let mut cluster_buf = vec![0u8; csize];
        let start = entry.valid.min(offset);
        let mut pos = start as usize;
        let end = end as usize;
        while pos < end {
            let cluster = chain[pos / csize];
            let within = pos % csize;
            let take = (csize - within).min(end - pos);
            if within != 0 || take != csize {
                self.dev
                    .read(self.cluster_lba(cluster), &mut cluster_buf)
                    .map_err(|_| FsError::Io)?;
            }
            for k in 0..take {
                let p = pos + k;
                cluster_buf[within + k] = if p as u64 >= offset {
                    data[p - offset as usize]
                } else {
                    0
                };
            }
            self.dev
                .write(self.cluster_lba(cluster), &cluster_buf)
                .map_err(|_| FsError::Io)?;
            pos += take;
        }
        entry.size = entry.size.max(end as u64);
        entry.valid = entry.valid.max(end as u64);
        self.write_set(entry)?;
        Ok(data.len())
    }

    pub fn truncate(&self, entry: &mut Entry) -> Result<()> {
        {
            let mut state = self.state.lock();
            if entry.cluster >= 2 {
                self.free_all(&mut state, entry)?;
            }
            self.flush_bitmap(&mut state)?;
        }
        entry.cluster = 0;
        entry.size = 0;
        entry.valid = 0;
        entry.contiguous = false;
        self.write_set(entry)
    }

    fn split(path: &str) -> Result<(&str, &str)> {
        let trimmed = path.trim_end_matches('/');
        let (parent, name) = trimmed.rsplit_once('/').ok_or(FsError::InvalidPath)?;
        if name.is_empty()
            || name.encode_utf16().count() > 255
            || name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
        {
            return Err(FsError::InvalidPath);
        }
        Ok((if parent.is_empty() { "/" } else { parent }, name))
    }

    /// Dizinde `count` ardışık boş girdi; yoksa dizini bir küme büyütür.
    fn free_slots(
        &self,
        state: &mut State,
        dir: &mut Entry,
        count: usize,
    ) -> Result<Vec<(u64, usize)>> {
        loop {
            let raw = self.read_dir_raw(dir)?;
            let chain = self.dir_chain(dir)?;
            let mut run = Vec::new();
            for (i, e) in raw.iter().enumerate() {
                if e[0] & 0x80 == 0 {
                    run.push(i);
                    if run.len() == count {
                        return Ok(run
                            .iter()
                            .filter_map(|&k| self.slot_location(&chain, k))
                            .collect());
                    }
                } else {
                    run.clear();
                }
            }
            // Büyüt: yeni küme sıfırlanır; kök FAT zinciriyle, alt dizin girdisiyle büyür.
            let csize = self.cluster_bytes();
            let new = if dir.slots.is_empty() {
                let new = self.find_free(state, chain.last().map(|c| c + 1))?;
                Self::mark(state, new, true);
                self.fat_set(*chain.last().ok_or(FsError::Corrupt)?, new)?;
                self.fat_set(new, END_OF_CHAIN)?;
                new
            } else {
                let clusters = chain.len() + 1;
                let grown = self.grow(state, dir, clusters)?;
                dir.size = (clusters * csize) as u64;
                dir.valid = dir.size;
                self.write_set(dir)?;
                *grown.last().unwrap()
            };
            self.flush_bitmap(state)?;
            self.dev
                .write(self.cluster_lba(new), &vec![0u8; csize])
                .map_err(|_| FsError::Io)?;
        }
    }

    pub fn create(&self, path: &str, directory: bool) -> Result<Entry> {
        let (parent_path, name) = Self::split(path)?;
        let mut parent = self.lookup(parent_path)?;
        if !parent.is_dir() {
            return Err(FsError::NotDirectory);
        }
        if self
            .read_dir(&parent)?
            .iter()
            .any(|e| self.same_name(&e.name, name))
        {
            return Err(FsError::Exists);
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_entries = units.len().div_ceil(15);
        let mut state = self.state.lock();
        let slots = self.free_slots(&mut state, &mut parent, 2 + name_entries)?;
        let csize = self.cluster_bytes();
        let cluster = if directory {
            let c = self.find_free(&mut state, None)?;
            Self::mark(&mut state, c, true);
            self.dev
                .write(self.cluster_lba(c), &vec![0u8; csize])
                .map_err(|_| FsError::Io)?;
            c
        } else {
            0
        };
        self.flush_bitmap(&mut state)?;
        drop(state);

        let attr = if directory {
            ATTR_DIRECTORY
        } else {
            ATTR_ARCHIVE
        };
        let size = if directory { csize as u64 } else { 0 };
        let mut set = vec![[0u8; 32]; 2 + name_entries];
        set[0][0] = TYPE_FILE;
        set[0][1] = (1 + name_entries) as u8;
        set[0][4..6].copy_from_slice(&attr.to_le_bytes());
        let now = timestamp();
        for at in [8, 12, 16] {
            set[0][at..at + 4].copy_from_slice(&now.to_le_bytes());
        }
        set[1][0] = TYPE_STREAM;
        set[1][1] = FLAG_ALLOCATION_POSSIBLE | if directory { FLAG_NO_FAT_CHAIN } else { 0 };
        set[1][3] = units.len() as u8;
        set[1][4..6].copy_from_slice(&self.name_hash(&units).to_le_bytes());
        set[1][8..16].copy_from_slice(&size.to_le_bytes());
        set[1][20..24].copy_from_slice(&cluster.to_le_bytes());
        set[1][24..32].copy_from_slice(&size.to_le_bytes());
        for (k, chunk) in units.chunks(15).enumerate() {
            let e = &mut set[2 + k];
            e[0] = TYPE_NAME;
            for (u, unit) in chunk.iter().enumerate() {
                e[2 + u * 2..4 + u * 2].copy_from_slice(&unit.to_le_bytes());
            }
        }
        let checksum = set_checksum(&set);
        set[0][2..4].copy_from_slice(&checksum.to_le_bytes());
        for (raw, &(sector, offset)) in set.iter().zip(&slots) {
            let mut buf = vec![0u8; SECTOR];
            self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
            buf[offset..offset + 32].copy_from_slice(raw);
            self.dev.write(sector, &buf).map_err(|_| FsError::Io)?;
        }
        Ok(Entry {
            name: String::from(name),
            attr,
            cluster,
            size,
            valid: size,
            contiguous: directory,
            slots,
        })
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        let entry = self.lookup(path)?;
        if entry.slots.is_empty() {
            return Err(FsError::InvalidPath);
        }
        if entry.is_dir() && !self.read_dir(&entry)?.is_empty() {
            return Err(FsError::NotEmpty);
        }
        {
            let mut state = self.state.lock();
            if entry.cluster >= 2 {
                self.free_all(&mut state, &entry)?;
            }
            self.flush_bitmap(&mut state)?;
        }
        for &(sector, offset) in &entry.slots {
            let mut buf = vec![0u8; SECTOR];
            self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
            buf[offset] &= 0x7f;
            self.dev.write(sector, &buf).map_err(|_| FsError::Io)?;
        }
        Ok(())
    }
}

/// Sıkıştırılmış büyük harf tablosu: 0xFFFF + n, n karakterlik "kendisi" aralığıdır.
fn decompress_upcase(raw: &[u8]) -> Vec<u16> {
    let mut table: Vec<u16> = (0..=0xffffu32).map(|c| c as u16).collect();
    let units: Vec<u16> = raw
        .as_chunks::<2>()
        .0
        .iter()
        .map(|c| u16::from_le_bytes(*c))
        .collect();
    let mut index = 0usize;
    let mut i = 0;
    while i < units.len() && index < table.len() {
        if units[i] == 0xffff && i + 1 < units.len() {
            index += units[i + 1] as usize;
            i += 2;
        } else {
            table[index] = units[i];
            index += 1;
            i += 1;
        }
    }
    table
}
