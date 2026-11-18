//! FAT32 biçimlendirici (EFI sistem bölümü ve taşınabilir bellekler için).

use crate::blockdev::{BlockFile, SECTOR, random16};
use crate::vec;

const RESERVED: u64 = 32;
const FATS: u64 = 2;

fn cluster_bytes(bytes: u64) -> u64 {
    match bytes {
        b if b <= 260 * 1024 * 1024 => 512,
        b if b <= 8 * 1024 * 1024 * 1024 => 4096,
        b if b <= 16 * 1024 * 1024 * 1024 => 8192,
        b if b <= 32 * 1024 * 1024 * 1024 => 16384,
        _ => 32768,
    }
}

/// Aygıtı FAT32 ile biçimlendirir (en az ~33 MiB; 4Kn disklerde ~260 MiB). Sektör
/// boyutu aygıtın mantıksal blok boyutudur, çünkü UEFI bunu bekler.
pub fn format(device: &BlockFile, label: &str) -> Result<(), &'static str> {
    let bps = device.block_size();
    let f = bps / SECTOR;
    let total = device.sectors() / f;
    let spc = (cluster_bytes(total * bps) / bps).max(1);
    if total <= RESERVED + 2 {
        return Err("FAT32 için aygıt çok küçük");
    }
    let fat_size = (total - RESERVED).div_ceil((bps / 2 * spc + FATS) / 2);
    let data_start = RESERVED + FATS * fat_size;
    let clusters = total.saturating_sub(data_start) / spc;
    if clusters < 65525 {
        return Err(if f > 1 {
            "FAT32 için aygıt çok küçük (4K sektörlü diskte en az 260 MiB)"
        } else {
            "FAT32 için aygıt çok küçük (en az 33 MiB)"
        });
    }
    let io = |r: Result<(), i64>| r.map_err(|_| "aygıta yazılamadı");
    let at = |lba: u64| lba * f;

    let mut boot = vec![0u8; bps as usize];
    boot[0..3].copy_from_slice(&[0xeb, 0x58, 0x90]);
    boot[3..11].copy_from_slice(b"BANNAOS ");
    boot[11..13].copy_from_slice(&(bps as u16).to_le_bytes());
    boot[13] = spc as u8;
    boot[14..16].copy_from_slice(&(RESERVED as u16).to_le_bytes());
    boot[16] = FATS as u8;
    boot[21] = 0xf8;
    boot[24..26].copy_from_slice(&63u16.to_le_bytes());
    boot[26..28].copy_from_slice(&255u16.to_le_bytes());
    boot[32..36].copy_from_slice(&(total.min(u32::MAX as u64) as u32).to_le_bytes());
    boot[36..40].copy_from_slice(&(fat_size as u32).to_le_bytes());
    boot[44..48].copy_from_slice(&2u32.to_le_bytes());
    boot[48..50].copy_from_slice(&1u16.to_le_bytes());
    boot[50..52].copy_from_slice(&6u16.to_le_bytes());
    boot[64] = 0x80;
    boot[66] = 0x29;
    boot[67..71].copy_from_slice(&random16()[..4]);
    let mut name = [b' '; 11];
    for (i, b) in label.bytes().take(11).enumerate() {
        name[i] = b.to_ascii_uppercase();
    }
    boot[71..82].copy_from_slice(&name);
    boot[82..90].copy_from_slice(b"FAT32   ");
    boot[510] = 0x55;
    boot[511] = 0xaa;

    let mut info = vec![0u8; bps as usize];
    info[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
    info[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
    // Boş küme sayısı ve sonraki boş küme "bilinmiyor": sürücü FSInfo'yu güncellemediği
    // için yanlış bir ipucu yerine diğer sistemlerin yeniden hesaplaması istenir.
    info[488..492].copy_from_slice(&u32::MAX.to_le_bytes());
    info[492..496].copy_from_slice(&u32::MAX.to_le_bytes());
    info[508..512].copy_from_slice(&0xaa55_0000u32.to_le_bytes());

    // Ayrılmış alan ve FAT'ler sıfırlanır.
    let zeros = vec![0u8; 64 * 1024];
    let mut lba = 0;
    while lba < data_start {
        let n = ((data_start - lba) * bps).min(zeros.len() as u64) as usize;
        io(device.write_at(at(lba), &zeros[..n]))?;
        lba += n as u64 / bps;
    }
    io(device.write_at(0, &boot))?;
    io(device.write_at(at(1), &info))?;
    io(device.write_at(at(6), &boot))?;
    io(device.write_at(at(7), &info))?;
    let mut fat = vec![0u8; bps as usize];
    fat[0..4].copy_from_slice(&0x0fff_fff8u32.to_le_bytes());
    fat[4..8].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    fat[8..12].copy_from_slice(&0x0fff_ffffu32.to_le_bytes());
    for i in 0..FATS {
        io(device.write_at(at(RESERVED + i * fat_size), &fat))?;
    }
    // Kök dizin kümesi (2): yalnızca birim etiketi girdisi.
    let mut root = vec![0u8; (spc * bps) as usize];
    root[0..11].copy_from_slice(&name);
    root[11] = 0x08;
    io(device.write_at(at(data_start), &root))?;
    Ok(())
}
