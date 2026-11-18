//! FAT32: okuma/yazma, uzun dosya adları (LFN), dizin oluşturma ve silme.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use super::{FsError, Kind, Metadata, Result};
use crate::drivers::rtc;
use crate::sched::Mutex;
use crate::storage::{BlockDevice, SECTOR};

const ATTR_READ_ONLY: u8 = 0x01;
const ATTR_HIDDEN: u8 = 0x02;
const ATTR_SYSTEM: u8 = 0x04;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const ATTR_ARCHIVE: u8 = 0x20;
const ATTR_LFN: u8 = 0x0f;
const END_OF_CHAIN: u32 = 0x0fff_fff8;
/// 4096 sektör = 2 MiB: 64 MiB'lık EFI bölümünün FAT'inin tamamı sığar (taramada önbellek bozulmaz).
const FAT_CACHE_LIMIT: usize = 4096;
/// Tek aygıt işleminde aktarılan en fazla bitişik küme.
const RUN_LIMIT: usize = 128;

#[derive(Clone, Debug)]
pub struct Entry {
    pub name: String,
    pub attr: u8,
    pub cluster: u32,
    pub size: u32,
    /// Kısa ad girdisinin diskteki yeri (sektör, sektör içi konum); kök için `None`.
    location: Option<(u64, usize)>,
    /// LFN girdileri dahil tüm girdilerin yerleri (silme için).
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
            size: self.size as u64,
            read_only: self.attr & ATTR_READ_ONLY != 0,
            mode: if self.is_dir() {
                0o755
            } else if self.attr & ATTR_READ_ONLY != 0 {
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
    /// İlk FAT kopyasının sektörleri; değişenler `dirty`'de bekler ve işlem sonunda
    /// tüm kopyalara birlikte yazılır.
    fat_cache: BTreeMap<u64, Vec<u8>>,
    dirty: alloc::collections::BTreeSet<u64>,
    /// Son yürünen küme zinciri (ilk küme, zincir); FAT değişince silinir.
    last_chain: Option<(u32, Vec<u32>)>,
    free_hint: u32,
}

pub struct Fat32 {
    dev: Arc<dyn BlockDevice>,
    sectors_per_cluster: u32,
    reserved: u32,
    fats: u32,
    fat_size: u32,
    root_cluster: u32,
    data_start: u64,
    cluster_count: u32,
    pub label: String,
    state: Mutex<State>,
}

fn le16(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

fn short_name_text(raw: &[u8]) -> String {
    let base = core::str::from_utf8(&raw[0..8]).unwrap_or("").trim_end();
    let ext = core::str::from_utf8(&raw[8..11]).unwrap_or("").trim_end();
    let mut name = String::from(base);
    if !ext.is_empty() {
        name.push('.');
        name.push_str(ext);
    }
    if raw[0] == 0x05 {
        name.replace_range(0..1, "\u{e5}");
    }
    name
}

fn lfn_checksum(short: &[u8; 11]) -> u8 {
    short
        .iter()
        .fold(0u8, |sum, &c| sum.rotate_right(1).wrapping_add(c))
}

fn valid_short_char(c: char) -> bool {
    c.is_ascii_uppercase() || c.is_ascii_digit() || "$%'-_@~`!(){}^#&".contains(c)
}

/// Ad 8.3 kurallarına (büyük harf) uyuyorsa kısa adını döndürür.
fn exact_short_name(name: &str) -> Option<[u8; 11]> {
    if name == "." || name == ".." {
        let mut out = [b' '; 11];
        out[..name.len()].copy_from_slice(name.as_bytes());
        return Some(out);
    }
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) => (b, e),
        None => (name, ""),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    if !base.chars().chain(ext.chars()).all(valid_short_char) {
        return None;
    }
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(base.as_bytes());
    out[8..8 + ext.len()].copy_from_slice(ext.as_bytes());
    Some(out)
}

fn short_name_candidate(name: &str, n: u32) -> [u8; 11] {
    let (base, ext) = match name.rsplit_once('.') {
        Some((b, e)) if !b.is_empty() => (b, e),
        _ => (name, ""),
    };
    let clean = |s: &str, max: usize| -> Vec<u8> {
        s.chars()
            .map(|c| c.to_ascii_uppercase())
            .filter(|&c| valid_short_char(c))
            .take(max)
            .map(|c| c as u8)
            .collect()
    };
    let suffix = alloc::format!("~{}", n);
    let mut base = clean(base, 8 - suffix.len());
    if base.is_empty() {
        base.push(b'F');
    }
    base.extend_from_slice(suffix.as_bytes());
    let ext = clean(ext, 3);
    let mut out = [b' '; 11];
    out[..base.len()].copy_from_slice(&base);
    out[8..8 + ext.len()].copy_from_slice(&ext);
    out
}

fn fat_time() -> (u16, u16) {
    match rtc::now().map(|t| t.to_local()) {
        Some(t) => (
            ((t.year.saturating_sub(1980)) << 9) | (t.month as u16) << 5 | t.day as u16,
            (t.hour as u16) << 11 | (t.minute as u16) << 5 | (t.second as u16 / 2),
        ),
        None => (0x21, 0),
    }
}

impl Fat32 {
    pub fn mount(dev: Arc<dyn BlockDevice>) -> Result<Self> {
        let mut bpb = vec![0u8; SECTOR];
        dev.read(0, &mut bpb).map_err(|_| FsError::Io)?;
        let bytes_per_sector = le16(&bpb, 11) as u32;
        if bpb[510] != 0x55
            || bpb[511] != 0xaa
            || !matches!(bytes_per_sector, 512 | 1024 | 2048 | 4096)
        {
            return Err(FsError::NotSupported);
        }
        // 4Kn disklerde BPB sektörü 4096 bayttır; her şey 512 baytlık birime çevrilir.
        let ratio = bytes_per_sector / SECTOR as u32;
        let sectors_per_cluster = bpb[13] as u32 * ratio;
        let reserved = le16(&bpb, 14) as u32 * ratio;
        let fats = bpb[16] as u32;
        let root_entries = le16(&bpb, 17);
        let fat_size = le32(&bpb, 36).saturating_mul(ratio);
        let total = match le16(&bpb, 19) {
            0 => le32(&bpb, 32),
            n => n as u32,
        } as u64
            * ratio as u64;
        if root_entries != 0
            || fat_size == 0
            || sectors_per_cluster == 0
            || &bpb[82..87] != b"FAT32"
        {
            return Err(FsError::NotSupported);
        }
        let data_start = reserved as u64 + fats as u64 * fat_size as u64;
        if data_start >= total {
            return Err(FsError::Corrupt);
        }
        let cluster_count =
            ((total - data_start) / sectors_per_cluster as u64).min(0x0fff_fff5) as u32;
        let label = String::from(core::str::from_utf8(&bpb[71..82]).unwrap_or("").trim_end());
        Ok(Self {
            dev,
            sectors_per_cluster,
            reserved,
            fats,
            fat_size,
            root_cluster: le32(&bpb, 44),
            data_start,
            cluster_count,
            label,
            state: Mutex::new(State {
                fat_cache: BTreeMap::new(),
                dirty: alloc::collections::BTreeSet::new(),
                last_chain: None,
                free_hint: 2,
            }),
        })
    }

    pub fn cluster_bytes(&self) -> usize {
        self.sectors_per_cluster as usize * SECTOR
    }

    pub fn capacity(&self) -> (u64, u64) {
        let mut free = 0u64;
        let mut state = self.state.lock();
        for c in 2..self.cluster_count + 2 {
            if self.fat_get(&mut state, c).unwrap_or(1) == 0 {
                free += 1;
            }
        }
        let bytes = self.cluster_bytes() as u64;
        (self.cluster_count as u64 * bytes, free * bytes)
    }

    fn cluster_lba(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }

    fn read_cluster(&self, cluster: u32, buf: &mut [u8]) -> Result<()> {
        self.dev
            .read(self.cluster_lba(cluster), buf)
            .map_err(|_| FsError::Io)
    }

    fn write_cluster(&self, cluster: u32, buf: &[u8]) -> Result<()> {
        self.dev
            .write(self.cluster_lba(cluster), buf)
            .map_err(|_| FsError::Io)
    }

    fn load_fat(&self, state: &mut State, sector: u64) -> Result<()> {
        if state.fat_cache.contains_key(&sector) {
            return Ok(());
        }
        if state.fat_cache.len() >= FAT_CACHE_LIMIT {
            self.flush_fat(state)?;
            let first = *state.fat_cache.keys().next().unwrap();
            state.fat_cache.remove(&first);
        }
        let mut buf = vec![0u8; SECTOR];
        self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
        state.fat_cache.insert(sector, buf);
        Ok(())
    }

    fn fat_location(&self, cluster: u32) -> (u64, usize) {
        let offset = cluster as u64 * 4;
        (
            self.reserved as u64 + offset / SECTOR as u64,
            (offset % SECTOR as u64) as usize,
        )
    }

    fn fat_get(&self, state: &mut State, cluster: u32) -> Result<u32> {
        let (sector, within) = self.fat_location(cluster);
        self.load_fat(state, sector)?;
        Ok(le32(&state.fat_cache[&sector], within) & 0x0fff_ffff)
    }

    fn fat_set(&self, state: &mut State, cluster: u32, value: u32) -> Result<()> {
        let (sector, within) = self.fat_location(cluster);
        self.load_fat(state, sector)?;
        let buf = state.fat_cache.get_mut(&sector).unwrap();
        let old = le32(buf, within) & 0xf000_0000;
        buf[within..within + 4].copy_from_slice(&(old | (value & 0x0fff_ffff)).to_le_bytes());
        state.dirty.insert(sector);
        state.last_chain = None;
        Ok(())
    }

    /// Değişen FAT sektörlerini (bitişik olanları tek seferde) her FAT kopyasına yazar.
    fn flush_fat(&self, state: &mut State) -> Result<()> {
        let dirty: Vec<u64> = core::mem::take(&mut state.dirty).into_iter().collect();
        let mut i = 0;
        while i < dirty.len() {
            let mut j = i + 1;
            while j < dirty.len() && dirty[j] == dirty[j - 1] + 1 && j - i < 64 {
                j += 1;
            }
            let mut run = Vec::with_capacity((j - i) * SECTOR);
            for sector in &dirty[i..j] {
                run.extend_from_slice(&state.fat_cache[sector]);
            }
            for copy in 0..self.fats as u64 {
                self.dev
                    .write(dirty[i] + copy * self.fat_size as u64, &run)
                    .map_err(|_| FsError::Io)?;
            }
            i = j;
        }
        Ok(())
    }

    fn chain(&self, state: &mut State, first: u32) -> Result<Vec<u32>> {
        if let Some((cached, chain)) = &state.last_chain
            && *cached == first
        {
            return Ok(chain.clone());
        }
        let chain = self.walk_chain(state, first)?;
        state.last_chain = Some((first, chain.clone()));
        Ok(chain)
    }

    fn walk_chain(&self, state: &mut State, first: u32) -> Result<Vec<u32>> {
        let mut out = Vec::new();
        let mut cluster = first;
        while (2..END_OF_CHAIN).contains(&cluster) {
            out.push(cluster);
            if out.len() > self.cluster_count as usize {
                return Err(FsError::Corrupt);
            }
            cluster = self.fat_get(state, cluster)?;
        }
        Ok(out)
    }

    fn allocate(&self, state: &mut State, previous: Option<u32>, zero: bool) -> Result<u32> {
        let total = self.cluster_count;
        let start = state.free_hint.clamp(2, total + 1);
        for i in 0..total {
            let cluster = 2 + (start - 2 + i) % total;
            if self.fat_get(state, cluster)? == 0 {
                self.fat_set(state, cluster, 0x0fff_ffff)?;
                if let Some(prev) = previous {
                    self.fat_set(state, prev, cluster)?;
                }
                state.free_hint = cluster + 1;
                if zero {
                    self.write_cluster(cluster, &vec![0u8; self.cluster_bytes()])?;
                }
                return Ok(cluster);
            }
        }
        Err(FsError::NoSpace)
    }

    fn free_chain(&self, state: &mut State, first: u32) -> Result<()> {
        for cluster in self.chain(state, first)? {
            self.fat_set(state, cluster, 0)?;
        }
        Ok(())
    }

    pub fn root(&self) -> Entry {
        Entry {
            name: String::from("/"),
            attr: ATTR_DIRECTORY,
            cluster: self.root_cluster,
            size: 0,
            location: None,
            slots: Vec::new(),
        }
    }

    fn dir_clusters(&self, state: &mut State, dir: &Entry) -> Result<Vec<u32>> {
        let first = if dir.cluster == 0 {
            self.root_cluster
        } else {
            dir.cluster
        };
        self.chain(state, first)
    }

    fn read_dir_locked(&self, state: &mut State, dir: &Entry) -> Result<Vec<Entry>> {
        if !dir.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let mut out = Vec::new();
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        let mut slots: Vec<(u64, usize)> = Vec::new();
        let mut buf = vec![0u8; self.cluster_bytes()];
        for cluster in self.dir_clusters(state, dir)? {
            self.read_cluster(cluster, &mut buf)?;
            for i in 0..buf.len() / 32 {
                let raw = &buf[i * 32..(i + 1) * 32];
                let sector = self.cluster_lba(cluster) + (i * 32 / SECTOR) as u64;
                let offset = (i * 32) % SECTOR;
                match raw[0] {
                    0x00 => return Ok(out),
                    0xe5 => {
                        lfn.clear();
                        slots.clear();
                        continue;
                    }
                    _ => {}
                }
                let attr = raw[11];
                if attr == ATTR_LFN {
                    let mut chars = [0u16; 13];
                    for (j, at) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                        .iter()
                        .enumerate()
                    {
                        chars[j] = le16(raw, *at);
                    }
                    lfn.push((raw[0] & 0x1f, chars));
                    slots.push((sector, offset));
                    continue;
                }
                if attr & ATTR_VOLUME != 0 {
                    lfn.clear();
                    slots.clear();
                    continue;
                }
                let name = if lfn.is_empty() {
                    let mut name = short_name_text(raw);
                    if raw[12] & 0x08 != 0 {
                        let (b, e) = name.split_once('.').unwrap_or((&name, ""));
                        name = if e.is_empty() {
                            b.to_ascii_lowercase()
                        } else {
                            alloc::format!("{}.{}", b, e)
                        };
                    }
                    if raw[12] & 0x10 != 0
                        && let Some((b, e)) = name.clone().split_once('.')
                    {
                        name = alloc::format!("{}.{}", b, e.to_ascii_lowercase());
                    }
                    name
                } else {
                    lfn.sort_by_key(|(order, _)| *order);
                    let units: Vec<u16> = lfn
                        .iter()
                        .flat_map(|(_, c)| c.iter().copied())
                        .take_while(|&u| u != 0 && u != 0xffff)
                        .collect();
                    String::from_utf16_lossy(&units)
                };
                slots.push((sector, offset));
                let cluster = (le16(raw, 20) as u32) << 16 | le16(raw, 26) as u32;
                out.push(Entry {
                    name,
                    attr,
                    cluster,
                    size: le32(raw, 28),
                    location: Some((sector, offset)),
                    slots: core::mem::take(&mut slots),
                });
                lfn.clear();
            }
        }
        Ok(out)
    }

    pub fn read_dir(&self, dir: &Entry) -> Result<Vec<Entry>> {
        let mut state = self.state.lock();
        self.read_dir_locked(&mut state, dir)
    }

    pub fn lookup(&self, path: &str) -> Result<Entry> {
        let mut state = self.state.lock();
        self.lookup_locked(&mut state, path)
    }

    fn lookup_locked(&self, state: &mut State, path: &str) -> Result<Entry> {
        let mut current = self.root();
        for part in path.split('/').filter(|p| !p.is_empty()) {
            let entries = self.read_dir_locked(state, &current)?;
            current = entries
                .into_iter()
                .find(|e| e.name.eq_ignore_ascii_case(part))
                .ok_or(FsError::NotFound)?;
            if current.is_dir() && current.cluster == 0 {
                current.cluster = self.root_cluster;
            }
        }
        Ok(current)
    }

    pub fn read(&self, entry: &Entry, offset: u64, buf: &mut [u8]) -> Result<usize> {
        if entry.is_dir() {
            return Err(FsError::IsDirectory);
        }
        if offset >= entry.size as u64 {
            return Ok(0);
        }
        let mut state = self.state.lock();
        let len = buf.len().min((entry.size as u64 - offset) as usize);
        let csize = self.cluster_bytes();
        let chain = self.chain(&mut state, entry.cluster)?;
        drop(state);
        let mut cluster_buf = vec![0u8; csize];
        let mut done = 0;
        while done < len {
            let pos = offset as usize + done;
            let index = pos / csize;
            let within = pos % csize;
            let cluster = *chain.get(index).ok_or(FsError::Corrupt)?;
            let take = (csize - within).min(len - done);
            if within == 0 && take == csize {
                let run = self.run_length(&chain, index, (len - done) / csize);
                let bytes = run * csize;
                self.dev
                    .read(self.cluster_lba(cluster), &mut buf[done..done + bytes])
                    .map_err(|_| FsError::Io)?;
                done += bytes;
                continue;
            }
            self.read_cluster(cluster, &mut cluster_buf)?;
            buf[done..done + take].copy_from_slice(&cluster_buf[within..within + take]);
            done += take;
        }
        Ok(len)
    }

    /// `chain[index]`'ten başlayan, en fazla `limit` kümelik bitişik dizinin uzunluğu.
    fn run_length(&self, chain: &[u32], index: usize, limit: usize) -> usize {
        let limit = limit.clamp(1, RUN_LIMIT);
        let mut run = 1;
        while run < limit
            && index + run < chain.len()
            && chain[index + run] == chain[index] + run as u32
        {
            run += 1;
        }
        run
    }

    fn update_entry(&self, entry: &Entry) -> Result<()> {
        let Some((sector, offset)) = entry.location else {
            return Ok(());
        };
        let mut buf = vec![0u8; SECTOR];
        self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
        let raw = &mut buf[offset..offset + 32];
        raw[20..22].copy_from_slice(&((entry.cluster >> 16) as u16).to_le_bytes());
        raw[26..28].copy_from_slice(&(entry.cluster as u16).to_le_bytes());
        raw[28..32].copy_from_slice(&entry.size.to_le_bytes());
        let (date, time) = fat_time();
        raw[22..24].copy_from_slice(&time.to_le_bytes());
        raw[24..26].copy_from_slice(&date.to_le_bytes());
        raw[18..20].copy_from_slice(&date.to_le_bytes());
        self.dev.write(sector, &buf).map_err(|_| FsError::Io)
    }

    pub fn write(&self, entry: &mut Entry, offset: u64, data: &[u8]) -> Result<usize> {
        if entry.is_dir() {
            return Err(FsError::IsDirectory);
        }
        let end = offset as usize + data.len();
        if end > u32::MAX as usize {
            return Err(FsError::NoSpace);
        }
        let mut state = self.state.lock();
        let csize = self.cluster_bytes();
        let mut chain = if entry.cluster == 0 {
            Vec::new()
        } else {
            self.chain(&mut state, entry.cluster)?
        };
        let needed = end.div_ceil(csize).max(1);
        while chain.len() < needed {
            let new = self.allocate(
                &mut state,
                chain.last().copied(),
                offset as usize > chain.len() * csize,
            )?;
            if chain.is_empty() {
                entry.cluster = new;
            }
            chain.push(new);
        }
        self.flush_fat(&mut state)?;
        drop(state);

        let mut cluster_buf = vec![0u8; csize];
        let mut done = 0;
        while done < data.len() {
            let pos = offset as usize + done;
            let index = pos / csize;
            let cluster = chain[index];
            let within = pos % csize;
            let take = (csize - within).min(data.len() - done);
            if within == 0 && take == csize {
                let run = self.run_length(&chain, index, (data.len() - done) / csize);
                let bytes = run * csize;
                self.dev
                    .write(self.cluster_lba(cluster), &data[done..done + bytes])
                    .map_err(|_| FsError::Io)?;
                done += bytes;
                continue;
            }
            self.read_cluster(cluster, &mut cluster_buf)?;
            cluster_buf[within..within + take].copy_from_slice(&data[done..done + take]);
            self.write_cluster(cluster, &cluster_buf)?;
            done += take;
        }
        entry.size = entry.size.max(end as u32);
        self.update_entry(entry)?;
        Ok(data.len())
    }

    pub fn truncate(&self, entry: &mut Entry) -> Result<()> {
        if entry.cluster != 0 {
            let mut state = self.state.lock();
            self.free_chain(&mut state, entry.cluster)?;
            self.flush_fat(&mut state)?;
        }
        entry.cluster = 0;
        entry.size = 0;
        self.update_entry(entry)
    }

    fn split(path: &str) -> Result<(&str, &str)> {
        let trimmed = path.trim_end_matches('/');
        let (parent, name) = trimmed.rsplit_once('/').ok_or(FsError::InvalidPath)?;
        if name.is_empty()
            || name.len() > 255
            || name.contains(['\\', ':', '*', '?', '"', '<', '>', '|'])
        {
            return Err(FsError::InvalidPath);
        }
        Ok((if parent.is_empty() { "/" } else { parent }, name))
    }

    /// Dizinde `count` ardışık boş girdi bulur; yoksa dizini bir küme büyütür.
    fn free_slots(
        &self,
        state: &mut State,
        dir: &Entry,
        count: usize,
    ) -> Result<Vec<(u64, usize)>> {
        let mut buf = vec![0u8; self.cluster_bytes()];
        let mut run: Vec<(u64, usize)> = Vec::new();
        let clusters = self.dir_clusters(state, dir)?;
        for &cluster in &clusters {
            self.read_cluster(cluster, &mut buf)?;
            for i in 0..buf.len() / 32 {
                let first = buf[i * 32];
                if first == 0x00 || first == 0xe5 {
                    run.push((
                        self.cluster_lba(cluster) + (i * 32 / SECTOR) as u64,
                        (i * 32) % SECTOR,
                    ));
                    if run.len() == count {
                        return Ok(run);
                    }
                } else {
                    run.clear();
                }
            }
        }
        let new = self.allocate(state, clusters.last().copied(), true)?;
        for i in 0..self.cluster_bytes() / 32 {
            run.push((
                self.cluster_lba(new) + (i * 32 / SECTOR) as u64,
                (i * 32) % SECTOR,
            ));
            if run.len() == count {
                return Ok(run);
            }
        }
        Err(FsError::NoSpace)
    }

    fn write_raw_entry(&self, sector: u64, offset: usize, raw: &[u8; 32]) -> Result<()> {
        let mut buf = vec![0u8; SECTOR];
        self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
        buf[offset..offset + 32].copy_from_slice(raw);
        self.dev.write(sector, &buf).map_err(|_| FsError::Io)
    }

    pub fn create(&self, path: &str, directory: bool) -> Result<Entry> {
        let (parent_path, name) = Self::split(path)?;
        let mut state = self.state.lock();
        let parent = self.lookup_locked(&mut state, parent_path)?;
        if !parent.is_dir() {
            return Err(FsError::NotDirectory);
        }
        let siblings = self.read_dir_locked(&mut state, &parent)?;
        if siblings.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
            return Err(FsError::Exists);
        }

        let (short, needs_lfn) = match exact_short_name(name) {
            Some(short) => (short, false),
            None => {
                let mut n = 1;
                loop {
                    let candidate = short_name_candidate(name, n);
                    let text = short_name_text(&candidate);
                    if !siblings.iter().any(|e| e.name.eq_ignore_ascii_case(&text)) {
                        break (candidate, true);
                    }
                    n += 1;
                    if n > 9999 {
                        return Err(FsError::Exists);
                    }
                }
            }
        };
        let units: Vec<u16> = name.encode_utf16().collect();
        let lfn_count = if needs_lfn {
            units.len().div_ceil(13)
        } else {
            0
        };
        let slots = self.free_slots(&mut state, &parent, lfn_count + 1)?;

        let cluster = if directory {
            self.allocate(&mut state, None, true)?
        } else {
            0
        };
        self.flush_fat(&mut state)?;
        drop(state);

        let checksum = lfn_checksum(&short);
        for (i, &(sector, offset)) in slots.iter().take(lfn_count).enumerate() {
            let order = lfn_count - i;
            let mut raw = [0u8; 32];
            raw[0] = order as u8 | if i == 0 { 0x40 } else { 0 };
            raw[11] = ATTR_LFN;
            raw[13] = checksum;
            for (j, at) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
                .iter()
                .enumerate()
            {
                let index = (order - 1) * 13 + j;
                let unit = match index.cmp(&units.len()) {
                    core::cmp::Ordering::Less => units[index],
                    core::cmp::Ordering::Equal => 0,
                    core::cmp::Ordering::Greater => 0xffff,
                };
                raw[*at..*at + 2].copy_from_slice(&unit.to_le_bytes());
            }
            self.write_raw_entry(sector, offset, &raw)?;
        }

        let (date, time) = fat_time();
        let mut raw = [0u8; 32];
        raw[..11].copy_from_slice(&short);
        raw[11] = if directory {
            ATTR_DIRECTORY
        } else {
            ATTR_ARCHIVE
        };
        raw[14..16].copy_from_slice(&time.to_le_bytes());
        raw[16..18].copy_from_slice(&date.to_le_bytes());
        raw[18..20].copy_from_slice(&date.to_le_bytes());
        raw[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
        raw[22..24].copy_from_slice(&time.to_le_bytes());
        raw[24..26].copy_from_slice(&date.to_le_bytes());
        raw[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
        let (sector, offset) = slots[lfn_count];
        self.write_raw_entry(sector, offset, &raw)?;

        if directory {
            let mut dir = vec![0u8; self.cluster_bytes()];
            let parent_cluster = if parent.cluster == self.root_cluster {
                0
            } else {
                parent.cluster
            };
            for (i, (name, target)) in [(".", cluster), ("..", parent_cluster)].iter().enumerate() {
                let entry = &mut dir[i * 32..(i + 1) * 32];
                entry[..11].copy_from_slice(&exact_short_name(name).unwrap());
                entry[11] = ATTR_DIRECTORY;
                entry[20..22].copy_from_slice(&((target >> 16) as u16).to_le_bytes());
                entry[26..28].copy_from_slice(&(*target as u16).to_le_bytes());
                entry[16..18].copy_from_slice(&date.to_le_bytes());
            }
            self.write_cluster(cluster, &dir)?;
        }

        Ok(Entry {
            name: String::from(name),
            attr: raw[11],
            cluster,
            size: 0,
            location: Some((sector, offset)),
            slots: slots.into_iter().take(lfn_count + 1).collect(),
        })
    }

    pub fn remove(&self, path: &str) -> Result<()> {
        let mut state = self.state.lock();
        let entry = self.lookup_locked(&mut state, path)?;
        if entry.location.is_none() {
            return Err(FsError::InvalidPath);
        }
        if entry.is_dir() {
            let children = self.read_dir_locked(&mut state, &entry)?;
            if children.iter().any(|c| c.name != "." && c.name != "..") {
                return Err(FsError::NotEmpty);
            }
        }
        if entry.attr & (ATTR_SYSTEM | ATTR_HIDDEN) == ATTR_SYSTEM {
            return Err(FsError::PermissionDenied);
        }
        if entry.cluster != 0 {
            self.free_chain(&mut state, entry.cluster)?;
        }
        self.flush_fat(&mut state)?;
        drop(state);
        for &(sector, offset) in &entry.slots {
            let mut buf = vec![0u8; SECTOR];
            self.dev.read(sector, &mut buf).map_err(|_| FsError::Io)?;
            buf[offset] = 0xe5;
            self.dev.write(sector, &buf).map_err(|_| FsError::Io)?;
        }
        Ok(())
    }
}
