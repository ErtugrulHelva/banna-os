//! GUID bölüm tablosu (GPT) yazma ve okuma.

use crate::blockdev::{BlockFile, SECTOR, random16};
use crate::{String, Vec, vec};

pub const ESP: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
pub const LINUX_ROOT: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";
pub const LINUX_DATA: &str = "0FC63DAF-8483-4772-8E79-3D69D8477DE4";
/// FAT/NTFS veri bölümü (Windows ve macOS da tanır).
pub const BASIC_DATA: &str = "EBD0A0A2-B9E5-4433-87C0-68B6B72699C7";

const ENTRIES: u64 = 128;
const ENTRY_SIZE: u64 = 128;
const TABLE_BYTES: u64 = ENTRIES * ENTRY_SIZE;

/// "AABBCCDD-..." metnini diskteki karışık sıralı baytlara çevirir.
pub fn guid_bytes(text: &str) -> [u8; 16] {
    let hex: Vec<u8> = text
        .bytes()
        .filter(|b| *b != b'-')
        .map(|b| (b as char).to_digit(16).unwrap_or(0) as u8)
        .collect();
    let mut b = [0u8; 16];
    for i in 0..16 {
        b[i] = hex[i * 2] << 4 | hex[i * 2 + 1];
    }
    [
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15],
    ]
}

pub fn guid_text(b: &[u8]) -> String {
    crate::format!(
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

fn random_guid() -> [u8; 16] {
    let mut g = random16();
    g[7] = (g[7] & 0x0f) | 0x40;
    g[8] = (g[8] & 0x3f) | 0x80;
    g
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xedb8_8320
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

pub struct NewPartition<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    /// Sektör sayısı; `None` = kalan alanın tamamı.
    pub sectors: Option<u64>,
}

/// İlk kullanılabilir 512 baytlık sektör (1 MiB hizalı bölümler için).
pub const FIRST_LBA: u64 = 2048;

/// Diske yeni bir GPT yazar (eski tablo ve bölümler kaybolur); 512 baytlık sektör
/// cinsinden (ilk, son) listesi döner. 4Kn disklerde tablo 4096 baytlık LBA'larla yazılır.
pub fn write(disk: &BlockFile, parts: &[NewPartition]) -> Result<Vec<(u64, u64)>, i64> {
    let bs = disk.block_size();
    let f = bs / SECTOR;
    let total = disk.sectors() / f;
    let table_blocks = TABLE_BYTES.div_ceil(bs);
    if total < 2 * table_blocks + 3 + FIRST_LBA / f {
        return Err(-28);
    }
    let last_usable = total - 2 - table_blocks;
    let align = FIRST_LBA / f;
    let mut table = vec![0u8; (table_blocks * bs) as usize];
    let mut ranges = Vec::new();
    let mut next = align;
    for (i, p) in parts.iter().enumerate() {
        let first = next.next_multiple_of(align);
        let last = match p.sectors {
            Some(n) => first + n.div_ceil(f) - 1,
            None => last_usable,
        };
        if last > last_usable || last < first {
            return Err(-28);
        }
        let e = &mut table[i * 128..(i + 1) * 128];
        e[0..16].copy_from_slice(&guid_bytes(p.kind));
        e[16..32].copy_from_slice(&random_guid());
        e[32..40].copy_from_slice(&first.to_le_bytes());
        e[40..48].copy_from_slice(&last.to_le_bytes());
        for (j, c) in p.name.encode_utf16().take(36).enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&c.to_le_bytes());
        }
        ranges.push((first * f, last * f + f - 1));
        next = last + 1;
    }
    let table_crc = crc32(&table[..TABLE_BYTES as usize]);
    let disk_guid = random_guid();
    let header = |current: u64, backup: u64, table_lba: u64| -> Vec<u8> {
        let mut h = vec![0u8; bs as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&backup.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + table_blocks).to_le_bytes());
        h[48..56].copy_from_slice(&last_usable.to_le_bytes());
        h[56..72].copy_from_slice(&disk_guid);
        h[72..80].copy_from_slice(&table_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(ENTRIES as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
        h[88..92].copy_from_slice(&table_crc.to_le_bytes());
        let crc = crc32(&h[0..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    let mut mbr = vec![0u8; bs as usize];
    mbr[446 + 2] = 0x02;
    mbr[446 + 4] = 0xee;
    mbr[446 + 5] = 0xff;
    mbr[446 + 6] = 0xff;
    mbr[446 + 7] = 0xff;
    mbr[446 + 8..446 + 12].copy_from_slice(&1u32.to_le_bytes());
    let size = (total - 1).min(u32::MAX as u64) as u32;
    mbr[446 + 12..446 + 16].copy_from_slice(&size.to_le_bytes());
    mbr[510] = 0x55;
    mbr[511] = 0xaa;
    let at = |lba: u64| lba * f;
    disk.write_at(0, &mbr)?;
    disk.write_at(at(1), &header(1, total - 1, 2))?;
    disk.write_at(at(2), &table)?;
    disk.write_at(at(total - 1 - table_blocks), &table)?;
    disk.write_at(
        at(total - 1),
        &header(total - 1, 1, total - 1 - table_blocks),
    )?;
    // Bölüm başlarındaki eski dosya sistemi imzaları silinir.
    let zeros = vec![0u8; 64 * 1024];
    for &(first, _) in &ranges {
        disk.write_at(first, &zeros)?;
    }
    Ok(ranges)
}

pub struct Entry {
    pub index: usize,
    pub kind: String,
    pub unique: String,
    pub first: u64,
    pub last: u64,
    pub name: String,
}

/// GPT girdileri; GPT yoksa `None`.
pub fn read(disk: &BlockFile) -> Option<Vec<Entry>> {
    let f = disk.block_size() / SECTOR;
    let mut header = vec![0u8; SECTOR as usize];
    disk.read_at(f, &mut header).ok()?;
    if &header[0..8] != b"EFI PART" {
        return None;
    }
    let lba = u64::from_le_bytes(header[72..80].try_into().ok()?);
    let count = u32::from_le_bytes(header[80..84].try_into().ok()?).min(256) as usize;
    let size = u32::from_le_bytes(header[84..88].try_into().ok()?) as usize;
    if !(128..=4096).contains(&size) {
        return None;
    }
    let mut table = vec![0u8; (count * size).next_multiple_of(SECTOR as usize)];
    disk.read_at(lba.checked_mul(f)?, &mut table).ok()?;
    let mut out = Vec::new();
    for (i, e) in table.chunks(size).take(count).enumerate() {
        if e[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let name: Vec<u16> = e[56..128]
            .chunks(2)
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .take_while(|&c| c != 0)
            .collect();
        out.push(Entry {
            index: i + 1,
            kind: guid_text(&e[0..16]),
            unique: guid_text(&e[16..32]),
            first: u64::from_le_bytes(e[32..40].try_into().ok()?).saturating_mul(f),
            last: u64::from_le_bytes(e[40..48].try_into().ok()?)
                .saturating_mul(f)
                .saturating_add(f - 1),
            name: String::from_utf16_lossy(&name),
        });
    }
    Some(out)
}
