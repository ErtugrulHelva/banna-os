//! Banna OS disk imajı aracı (ana makinede çalışır, `rustc` ile tek dosya derlenir).
//!
//!   mkimage disk <çıktı> <boyut-MiB> <esp.img> <kök-dizin>
//!       GPT: 1) EFI sistem bölümü (verilen FAT32 imajı), 2) Linux kök bölümü (ext2,
//!       dizin ağacından doldurulur; sahip root, kipler kurallarla atanır).
//!   mkimage ext2-test <imaj> <boyut-MiB>
//!       ext2 sürücüsünü yoğun biçimde dener (e2fsck ile doğrulamak için).

extern crate alloc;

#[path = "../src/fs/ext2.rs"]
#[allow(dead_code)]
mod ext2;

use std::cell::RefCell;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::process::exit;

const SECTOR: u64 = 512;
const ESP_TYPE: &str = "C12A7328-F81F-11D2-BA4B-00A0C93EC93B";
const ROOT_TYPE: &str = "4F68BCE3-E8CD-4DB1-96E7-FBCAF984B709";

struct FileDevice {
    file: RefCell<fs::File>,
    start: u64,
    sectors: u64,
}

impl ext2::Device for FileDevice {
    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<(), ()> {
        if lba + buf.len() as u64 / SECTOR > self.sectors {
            return Err(());
        }
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start((self.start + lba) * SECTOR)).map_err(|_| ())?;
        f.read_exact(buf).map_err(|_| ())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<(), ()> {
        if lba + buf.len() as u64 / SECTOR > self.sectors {
            return Err(());
        }
        let mut f = self.file.borrow_mut();
        f.seek(SeekFrom::Start((self.start + lba) * SECTOR)).map_err(|_| ())?;
        f.write_all(buf).map_err(|_| ())
    }
}

fn now() -> u32 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as u32)
        .unwrap_or(0)
}

fn random_bytes<const N: usize>() -> [u8; N] {
    let mut out = [0u8; N];
    fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut out))
        .expect("/dev/urandom");
    out
}

/// "AABBCCDD-EEFF-..." metnini GPT'nin karışık sıralı 16 baytına çevirir.
fn guid_bytes(text: &str) -> [u8; 16] {
    let hex: String = text.chars().filter(|c| *c != '-').collect();
    let mut b = [0u8; 16];
    for i in 0..16 {
        b[i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap();
    }
    [
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13],
        b[14], b[15],
    ]
}

fn guid_text(b: &[u8; 16]) -> String {
    format!(
        "{:02X}{:02X}{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}-{:02X}{:02X}{:02X}{:02X}{:02X}{:02X}",
        b[3], b[2], b[1], b[0], b[5], b[4], b[7], b[6], b[8], b[9], b[10], b[11], b[12], b[13], b[14], b[15]
    )
}

fn random_guid() -> [u8; 16] {
    let mut g: [u8; 16] = random_bytes();
    g[7] = (g[7] & 0x0f) | 0x40;
    g[8] = (g[8] & 0x3f) | 0x80;
    g
}

fn crc32(data: &[u8]) -> u32 {
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

struct Part {
    kind: &'static str,
    unique: [u8; 16],
    first: u64,
    last: u64,
    name: &'static str,
}

fn write_gpt(file: &mut fs::File, total: u64, parts: &[Part]) {
    let entries = 128u64;
    let entry_size = 128u64;
    let entry_sectors = entries * entry_size / SECTOR;
    let mut table = vec![0u8; (entries * entry_size) as usize];
    for (i, p) in parts.iter().enumerate() {
        let e = &mut table[i * 128..(i + 1) * 128];
        e[0..16].copy_from_slice(&guid_bytes(p.kind));
        e[16..32].copy_from_slice(&p.unique);
        e[32..40].copy_from_slice(&p.first.to_le_bytes());
        e[40..48].copy_from_slice(&p.last.to_le_bytes());
        for (j, c) in p.name.encode_utf16().enumerate() {
            e[56 + j * 2..58 + j * 2].copy_from_slice(&c.to_le_bytes());
        }
    }
    let table_crc = crc32(&table);
    let disk_guid = random_guid();
    let header = |current: u64, backup: u64, table_lba: u64| -> Vec<u8> {
        let mut h = vec![0u8; SECTOR as usize];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&current.to_le_bytes());
        h[32..40].copy_from_slice(&backup.to_le_bytes());
        h[40..48].copy_from_slice(&(2 + entry_sectors).to_le_bytes());
        h[48..56].copy_from_slice(&(total - 2 - entry_sectors).to_le_bytes());
        h[56..72].copy_from_slice(&disk_guid);
        h[72..80].copy_from_slice(&table_lba.to_le_bytes());
        h[80..84].copy_from_slice(&(entries as u32).to_le_bytes());
        h[84..88].copy_from_slice(&(entry_size as u32).to_le_bytes());
        h[88..92].copy_from_slice(&table_crc.to_le_bytes());
        let crc = crc32(&h[0..92]);
        h[16..20].copy_from_slice(&crc.to_le_bytes());
        h
    };
    let mut mbr = vec![0u8; SECTOR as usize];
    mbr[446 + 1] = 0x00;
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
    let mut put = |lba: u64, data: &[u8]| {
        file.seek(SeekFrom::Start(lba * SECTOR)).unwrap();
        file.write_all(data).unwrap();
    };
    put(0, &mbr);
    put(1, &header(1, total - 1, 2));
    put(2, &table);
    put(total - 1 - entry_sectors, &table);
    put(total - 1, &header(total - 1, 1, total - 1 - entry_sectors));
}

/// Kök ağacındaki kip kuralları: çalıştırılabilirler 0755, /tmp 1777, /root 0700.
fn mode_for(path: &str, host: &fs::Metadata) -> u16 {
    use std::os::unix::fs::PermissionsExt;
    if host.is_dir() {
        return match path {
            "/tmp" => 0o1777,
            "/root" => 0o700,
            _ => 0o755,
        };
    }
    if path == "/etc/users" {
        return 0o600;
    }
    if host.permissions().mode() & 0o111 != 0 || path.starts_with("/bin/") {
        0o755
    } else {
        0o644
    }
}

fn populate(fs: &mut ext2::Ext2<FileDevice>, host: &Path, inner: &str) {
    let mut entries: Vec<_> = fs::read_dir(host).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name == ".DS_Store" {
            continue;
        }
        let path = if inner == "/" {
            format!("/{}", name)
        } else {
            format!("{}/{}", inner, name)
        };
        let meta = entry.metadata().unwrap();
        let mode = mode_for(&path, &meta);
        if meta.is_dir() {
            fs.create(&path, ext2::S_IFDIR | mode, 0, 0)
                .unwrap_or_else(|e| panic!("{}: {:?}", path, e));
            populate(fs, &entry.path(), &path);
        } else {
            let ino = fs
                .create(&path, ext2::S_IFREG | mode, 0, 0)
                .unwrap_or_else(|e| panic!("{}: {:?}", path, e));
            let data = fs::read(entry.path()).unwrap();
            if !data.is_empty() {
                fs.write(ino, 0, &data)
                    .unwrap_or_else(|e| panic!("{}: {:?}", path, e));
            }
        }
    }
}

fn mib(arg: &str) -> u64 {
    arg.parse::<u64>().expect("boyut MiB olarak verilmeli") * 1024 * 1024
}

fn cmd_disk(out: &str, size: u64, esp_path: &str, root_dir: &str) {
    let esp = fs::read(esp_path).expect("ESP imajı okunamadı");
    let total = size / SECTOR;
    let esp_first = 2048;
    let esp_last = esp_first + esp.len() as u64 / SECTOR - 1;
    let root_first = (esp_last + 1).div_ceil(2048) * 2048;
    let root_last = total - 34 - 1;
    let root_unique = random_guid();
    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(out)
        .unwrap();
    file.set_len(size).unwrap();
    write_gpt(
        &mut file,
        total,
        &[
            Part {
                kind: ESP_TYPE,
                unique: random_guid(),
                first: esp_first,
                last: esp_last,
                name: "EFI",
            },
            Part {
                kind: ROOT_TYPE,
                unique: root_unique,
                first: root_first,
                last: root_last,
                name: "BANNA",
            },
        ],
    );
    file.seek(SeekFrom::Start(esp_first * SECTOR)).unwrap();
    file.write_all(&esp).unwrap();
    let device = FileDevice {
        file: RefCell::new(file),
        start: root_first,
        sectors: root_last - root_first + 1,
    };
    let options = ext2::FormatOptions {
        label: "BANNA",
        uuid: random_bytes(),
        now: now(),
    };
    ext2::format(&device, &options).expect("ext2 biçimlendirilemedi");
    let mut fs = ext2::Ext2::mount(device, now).expect("ext2 bağlanamadı");
    populate(&mut fs, Path::new(root_dir), "/");
    let (total_bytes, free) = fs.capacity();
    println!(
        "mkimage: {} ({} MiB): EFI {} MiB + kök ext2 {} MiB ({} MiB boş), kök PARTUUID {}",
        out,
        size / 1024 / 1024,
        esp.len() / 1024 / 1024,
        total_bytes / 1024 / 1024,
        free / 1024 / 1024,
        guid_text(&root_unique)
    );
}

/// Sürücüyü zorlar: çok sayıda dosya, büyük dosya (çift dolaylı), kısaltma, silme, taşıma.
fn cmd_ext2_test(path: &str, size: u64) {
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .unwrap();
    file.set_len(size).unwrap();
    let device = FileDevice {
        file: RefCell::new(file),
        start: 0,
        sectors: size / SECTOR,
    };
    ext2::format(
        &device,
        &ext2::FormatOptions {
            label: "TEST",
            uuid: random_bytes(),
            now: now(),
        },
    )
    .unwrap();
    let mut fs = ext2::Ext2::mount(device, now).unwrap();
    let dir = ext2::S_IFDIR | 0o755;
    let reg = ext2::S_IFREG | 0o644;
    fs.create("/a", dir, 0, 0).unwrap();
    fs.create("/a/b", dir, 1000, 1000).unwrap();
    for i in 0..300 {
        let ino = fs.create(&format!("/a/f{}", i), reg, 0, 0).unwrap();
        fs.write(ino, 0, format!("dosya {}\n", i).as_bytes()).unwrap();
    }
    for i in (0..300).step_by(3) {
        fs.remove(&format!("/a/f{}", i)).unwrap();
    }
    let big: Vec<u8> = (0..3_000_000u32).map(|i| (i * 31 + 7) as u8).collect();
    let ino = fs.create("/a/b/big", reg, 1000, 1000).unwrap();
    fs.write(ino, 0, &big).unwrap();
    let mut back = vec![0u8; big.len()];
    assert_eq!(fs.read(ino, 0, &mut back).unwrap(), big.len());
    assert!(back == big, "büyük dosya içeriği bozuk");
    fs.write(ino, 5_000_000, b"seyrek").unwrap();
    fs.truncate(ino, 1_234_567).unwrap();
    let mut tail = vec![0u8; 10];
    fs.read(ino, 1_234_557, &mut tail).unwrap();
    assert_eq!(&tail[..], &big[1_234_557..1_234_567]);
    fs.rename("/a/f1", "/a/b/moved").unwrap();
    fs.rename("/a/b", "/c").unwrap();
    fs.create("/c/d", dir, 0, 0).unwrap();
    fs.rename("/c/d", "/a/d").unwrap();
    fs.remove("/a/d").unwrap();
    let small = fs.create("/c/small", reg, 0, 0).unwrap();
    fs.write(small, 0, b"hello").unwrap();
    fs.truncate(small, 0).unwrap();
    fs.write(small, 2, b"xy").unwrap();
    let mut s = [0u8; 4];
    fs.read(small, 0, &mut s).unwrap();
    assert_eq!(&s, b"\0\0xy");
    fs.set_mode(small, 0o600).unwrap();
    assert_eq!(fs.lookup("/c/moved").is_ok(), true);
    assert!(fs.remove("/c").is_err());
    let a = fs.lookup("/a").unwrap();
    let names = fs.read_dir(a).unwrap().len();
    let (total_bytes, free) = fs.capacity();
    println!(
        "ext2-test: ok ({} girdi /a altında, {} KiB / {} KiB boş)",
        names,
        free / 1024,
        total_bytes / 1024
    );
}

/// Başka araçla (mke2fs) oluşturulmuş bir ext2'yi bağlar, listeler ve üzerine yazar.
fn cmd_ext2_compat(path: &str) {
    let file = fs::OpenOptions::new().read(true).write(true).open(path).unwrap();
    let sectors = file.metadata().unwrap().len() / SECTOR;
    let device = FileDevice {
        file: RefCell::new(file),
        start: 0,
        sectors,
    };
    let mut fs = ext2::Ext2::mount(device, now).expect("bağlanamadı");
    let root = fs.lookup("/").unwrap();
    let names: Vec<String> = fs.read_dir(root).unwrap().into_iter().map(|e| e.name).collect();
    println!("ext2-compat: kök: {:?} (salt okunur: {})", names, fs.read_only);
    fs.create("/banna", ext2::S_IFDIR | 0o755, 0, 0).unwrap();
    for i in 0..50 {
        let ino = fs.create(&format!("/banna/f{}", i), ext2::S_IFREG | 0o644, 0, 0).unwrap();
        fs.write(ino, 0, &vec![i as u8; 5000 * i]).unwrap();
    }
    fs.remove("/banna/f7").unwrap();
    println!("ext2-compat: yazma tamam");
}

/// GPT'deki ilk `kind` tipli bölümün (ilk, son) LBA'sı.
fn find_partition(file: &mut fs::File, kind: &str) -> Option<(u64, u64)> {
    let mut header = vec![0u8; SECTOR as usize];
    file.seek(SeekFrom::Start(SECTOR)).ok()?;
    file.read_exact(&mut header).ok()?;
    if &header[0..8] != b"EFI PART" {
        return None;
    }
    let lba = u64::from_le_bytes(header[72..80].try_into().unwrap());
    let count = u32::from_le_bytes(header[80..84].try_into().unwrap()) as usize;
    let size = u32::from_le_bytes(header[84..88].try_into().unwrap()) as usize;
    let mut table = vec![0u8; count * size];
    file.seek(SeekFrom::Start(lba * SECTOR)).ok()?;
    file.read_exact(&mut table).ok()?;
    let want = guid_bytes(kind);
    table.chunks(size).find(|e| e[0..16] == want).map(|e| {
        (
            u64::from_le_bytes(e[32..40].try_into().unwrap()),
            u64::from_le_bytes(e[40..48].try_into().unwrap()),
        )
    })
}

/// Var olan imajda kök bölümünün `/bin` dizinini `dir` içeriğiyle günceller.
/// Kurulu imajda `target` klasörünü (yoksa oluşturur) `dir` içindeki dosyalarla günceller.
fn cmd_update(image: &str, dir: &str, target: &str) {
    let mut file = fs::OpenOptions::new().read(true).write(true).open(image).unwrap();
    let Some((first, last)) = find_partition(&mut file, ROOT_TYPE) else {
        eprintln!("mkimage: {} GPT/ext2 kök bölümü içermiyor", image);
        exit(1);
    };
    let device = FileDevice {
        file: RefCell::new(file),
        start: first,
        sectors: last - first + 1,
    };
    let mut fs = ext2::Ext2::mount(device, now).expect("ext2 bağlanamadı");
    if fs.lookup(target).is_err() {
        fs.create(target, ext2::S_IFDIR | 0o755, 0, 0).unwrap();
    }
    let mut count = 0;
    for entry in fs::read_dir(dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        let path = format!("{}/{}", target, name);
        let ino = match fs.lookup(&path) {
            Ok(ino) => {
                fs.truncate(ino, 0).unwrap();
                ino
            }
            Err(_) => fs.create(&path, ext2::S_IFREG | 0o755, 0, 0).unwrap(),
        };
        fs.write(ino, 0, &fs::read(entry.path()).unwrap()).unwrap();
        count += 1;
    }
    println!("mkimage: {} içinde {} güncellendi ({} dosya)", image, target, count);
}

/// GPT imajındaki kök bölümünü ya da düz ext2 imajını denetler (`-y` onarır).
fn cmd_fsck(image: &str, repair: bool) {
    let mut file = fs::OpenOptions::new().read(true).write(repair).open(image).unwrap();
    let (start, sectors) = match find_partition(&mut file, ROOT_TYPE) {
        Some((first, last)) => (first, last - first + 1),
        None => (0, file.metadata().unwrap().len() / SECTOR),
    };
    let device = FileDevice {
        file: RefCell::new(file),
        start,
        sectors,
    };
    let mut fs = ext2::Ext2::mount(device, now).expect("ext2 bağlanamadı");
    let r = fs.check(repair, |line| println!("  {}", line)).expect("denetim başarısız");
    println!(
        "fsck: {} dizin, {} dosya, {} inode; {} sorun, {} düzeltme; {}",
        r.directories,
        r.files,
        r.inodes,
        r.problems,
        r.fixed,
        if r.clean() {
            "temiz"
        } else if repair {
            "onarıldı"
        } else {
            "sorunlu"
        }
    );
    if !r.clean() && !repair {
        exit(4);
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("disk") if args.len() == 6 => cmd_disk(&args[2], mib(&args[3]), &args[4], &args[5]),
        Some("ext2-test") if args.len() == 4 => cmd_ext2_test(&args[2], mib(&args[3])),
        Some("ext2-compat") if args.len() == 3 => cmd_ext2_compat(&args[2]),
        Some("fsck") if args.len() == 3 => cmd_fsck(&args[2], false),
        Some("fsck") if args.len() == 4 && args[2] == "-y" => cmd_fsck(&args[3], true),
        Some("update") if args.len() == 4 => cmd_update(&args[2], &args[3], "/bin"),
        Some("update") if args.len() == 5 => cmd_update(&args[2], &args[3], &args[4]),
        Some("esp-offset") if args.len() == 3 => {
            let mut file = fs::File::open(&args[2]).unwrap();
            match find_partition(&mut file, ESP_TYPE) {
                Some((first, _)) => println!("{}", first * SECTOR),
                None => exit(1),
            }
        }
        _ => {
            eprintln!("kullanım: mkimage disk <çıktı> <MiB> <esp.img> <kök-dizin>");
            eprintln!("          mkimage ext2-test <imaj> <MiB>");
            exit(2);
        }
    }
}
