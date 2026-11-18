//! `banna-install`: Banna OS'u Live USB'den başka bir diske (USB bellek, SSD) kurar.
//!
//! Hedef disk GPT ile bölümlenir: 1) 64 MiB EFI sistem bölümü (FAT32: önyükleyici,
//! çekirdek, ayarlar), 2) kalan alan Linux kök bölümü (ext2). Kök ağacı kipler ve
//! sahiplikle kopyalanır; hesaplar kopyalanmaz (ilk açılışta yönetici oluşturulur).

#![no_std]
#![no_main]

use banna::blockdev::{self, BlockFile};
use banna::system::{self, size_text};
use banna::{String, Vec, eprintln, fat, fs, gpt, io, println, process};

banna::entry!(main);

const ESP_SECTORS: u64 = 64 * 1024 * 1024 / 512;
/// 4K sektörlü disklerde FAT32 en az 65525 × 4 KiB küme ister.
const ESP_SECTORS_4K: u64 = 300 * 1024 * 1024 / 512;
const MIN_DISK: u64 = 256 * 1024 * 1024;
const TARGET: &str = "/mnt/banna-install";
/// Kopyalanmayan yollar: sanal, geçici, bağlama noktaları ve Live oturum verisi.
const SKIP: &[&str] = &[
    "/dev",
    "/media",
    "/mnt",
    "/boot",
    "/tmp",
    "/home/live",
    "/etc/users",
];

struct Stats {
    files: usize,
    dirs: usize,
    bytes: u64,
}

/// Adım geçişlerini zamanla birlikte seri günlüğe yazar (yavaş aygıtları teşhis için).
fn step(n: u32) {
    io::log(&banna::format!(
        "banna-install: step {} at {} ms",
        n,
        banna::time::uptime_ms()
    ));
}

fn fail(message: &str) -> i32 {
    eprintln!("banna-install: {}", message);
    1
}

fn copy_file(source: &str, target: &str, stat: &banna::sys::Stat) -> Result<u64, String> {
    let data = fs::read(source).map_err(|e| banna::format!("{}: {}", source, fs::error_text(e)))?;
    fs::write(target, &data).map_err(|e| banna::format!("{}: {}", target, fs::error_text(e)))?;
    let _ = fs::chmod(target, stat.mode);
    let _ = fs::chown(target, stat.uid, stat.gid);
    Ok(data.len() as u64)
}

fn copy_tree(
    source: &str,
    target: &str,
    mounts: &[String],
    stats: &mut Stats,
) -> Result<(), String> {
    let entries =
        fs::read_dir(source).map_err(|e| banna::format!("{}: {}", source, fs::error_text(e)))?;
    for (name, stat) in entries {
        let from = if source == "/" {
            banna::format!("/{}", name)
        } else {
            banna::format!("{}/{}", source, name)
        };
        let to = banna::format!("{}{}", target, from);
        if SKIP.contains(&from.as_str()) || mounts.contains(&from) {
            continue;
        }
        match stat.kind {
            fs::KIND_DIRECTORY => {
                if fs::stat(&to).is_err() {
                    fs::mkdir(&to).map_err(|e| banna::format!("{}: {}", to, fs::error_text(e)))?;
                }
                let _ = fs::chmod(&to, stat.mode);
                let _ = fs::chown(&to, stat.uid, stat.gid);
                stats.dirs += 1;
                copy_tree(&from, target, mounts, stats)?;
            }
            fs::KIND_FILE => {
                stats.bytes += copy_file(&from, &to, &stat)?;
                stats.files += 1;
            }
            _ => {}
        }
    }
    Ok(())
}

fn mkdir_mode(path: &str, mode: u32) {
    let _ = fs::mkdir(path);
    let _ = fs::chmod(path, mode);
}

fn main(_: &[&str]) -> i32 {
    if process::uid() != 0 {
        return fail("yönetici gerekir: sudo banna-install");
    }
    let storage = system::storage();
    let Some(root) = storage.mounted_at("/").map(|m| m.device.clone()) else {
        return fail("kök dosya sistemi bulunamadı");
    };
    let live_disk = storage.disk_of(&root);
    let candidates: Vec<&system::Disk> = storage
        .disks
        .iter()
        .filter(|d| d.name != live_disk && d.sectors * 512 >= MIN_DISK)
        .collect();
    println!("\x1b[96mBanna OS kurulumu\x1b[0m");
    println!("Çalışan sistem: /dev/{} ({}).", root, live_disk);
    if candidates.is_empty() {
        return fail(
            "kurulabilecek başka disk yok (en az 256 MiB). Hedef USB'yi takıp tekrar dene.",
        );
    }
    println!("Hedef diskler:");
    for (i, d) in candidates.iter().enumerate() {
        println!(
            "  {}) {:<8} {:>10}  {}{}",
            i + 1,
            d.name,
            size_text(d.sectors * 512),
            d.model,
            if d.removable { " (USB)" } else { "" }
        );
    }
    let answer = io::prompt("Kurulacak disk numarası (iptal: boş bırak): ");
    let Some(disk) = answer
        .parse::<usize>()
        .ok()
        .and_then(|n| candidates.get(n.wrapping_sub(1)))
    else {
        println!("İptal edildi.");
        return 1;
    };
    let disk_name = disk.name.clone();
    println!(
        "\x1b[91mUYARI:\x1b[0m /dev/{} ({}) üzerindeki TÜM veriler silinecek.",
        disk_name,
        size_text(disk.sectors * 512)
    );
    let typed = io::prompt(&banna::format!(
        "Onaylamak için disk adını yaz ({}): ",
        disk_name
    ));
    if typed != disk_name {
        println!("İptal edildi.");
        return 1;
    }

    for m in storage.disk_mounts(&disk_name) {
        if let Err(e) = fs::umount(&m.point) {
            return fail(&banna::format!(
                "{} ayrılamadı: {}",
                m.point,
                fs::error_text(e)
            ));
        }
    }

    step(1);
    let dev = match BlockFile::open(&disk_name) {
        Ok(d) => d,
        Err(e) => return fail(&banna::format!("/dev/{}: {}", disk_name, fs::error_text(e))),
    };
    let esp_sectors = if dev.block_size() > 512 {
        ESP_SECTORS_4K
    } else {
        ESP_SECTORS
    };
    println!(
        "[1/5] Bölümleniyor (GPT: EFI {} MiB + kök)...",
        esp_sectors / 2048
    );
    if let Err(e) = gpt::write(
        &dev,
        &[
            gpt::NewPartition {
                kind: gpt::ESP,
                name: "EFI",
                sectors: Some(esp_sectors),
            },
            gpt::NewPartition {
                kind: gpt::LINUX_ROOT,
                name: "BANNA",
                sectors: None,
            },
        ],
    ) {
        return fail(&banna::format!(
            "bölüm tablosu yazılamadı: {}",
            fs::error_text(e)
        ));
    }
    drop(dev);
    if let Err(e) = fs::rescan(&disk_name) {
        return fail(&banna::format!(
            "disk yeniden taranamadı: {}",
            fs::error_text(e)
        ));
    }
    let esp = banna::format!("{}p1", disk_name);
    let root_part = banna::format!("{}p2", disk_name);
    for part in [&esp, &root_part] {
        let _ = fs::umount(&banna::format!("/media/{}", part));
    }

    step(2);
    println!("[2/5] EFI bölümü biçimlendiriliyor (FAT32)...");
    match BlockFile::open(&esp) {
        Ok(d) => {
            if let Err(e) = fat::format(&d, "BANNAEFI") {
                return fail(e);
            }
        }
        Err(e) => return fail(&banna::format!("/dev/{}: {}", esp, fs::error_text(e))),
    }
    step(3);
    println!("[3/5] Kök bölümü biçimlendiriliyor (ext2)...");
    match BlockFile::open(&root_part) {
        Ok(d) => {
            if let Err(e) = blockdev::format_ext2(&d, "BANNA") {
                return fail(&banna::format!("ext2: {:?}", e));
            }
        }
        Err(e) => return fail(&banna::format!("/dev/{}: {}", root_part, fs::error_text(e))),
    }

    step(4);
    println!("[4/5] Sistem kopyalanıyor...");
    mkdir_mode("/mnt", 0o755);
    mkdir_mode(TARGET, 0o755);
    if let Err(e) = fs::mount(&root_part, TARGET) {
        return fail(&banna::format!("kök bağlanamadı: {}", fs::error_text(e)));
    }
    let target_boot = banna::format!("{}/boot", TARGET);
    let mounts: Vec<String> = system::storage()
        .mounts
        .into_iter()
        .map(|m| m.point)
        .collect();
    let mut stats = Stats {
        files: 0,
        dirs: 0,
        bytes: 0,
    };
    let copied = copy_tree("/", TARGET, &mounts, &mut stats);
    for dir in ["/dev", "/media", "/mnt", "/boot", "/home"] {
        mkdir_mode(&banna::format!("{}{}", TARGET, dir), 0o755);
    }
    mkdir_mode(&banna::format!("{}/tmp", TARGET), 0o1777);
    if let Err(e) = copied {
        let _ = fs::umount(TARGET);
        return fail(&e);
    }
    println!(
        "  {} dizin, {} dosya, {}",
        stats.dirs,
        stats.files,
        size_text(stats.bytes)
    );

    step(5);
    println!("[5/5] Önyükleyici kuruluyor...");
    if let Err(e) = fs::mount(&esp, &target_boot) {
        let _ = fs::umount(TARGET);
        return fail(&banna::format!(
            "EFI bölümü bağlanamadı: {}",
            fs::error_text(e)
        ));
    }
    let boot_files = [
        ("/boot/EFI/BOOT/BOOTX64.EFI", "/EFI/BOOT/BOOTX64.EFI"),
        ("/boot/boot/banna_os", "/boot/banna_os"),
    ];
    let mut result = Ok(());
    for dir in ["/EFI", "/EFI/BOOT", "/boot"] {
        let _ = fs::mkdir(&banna::format!("{}{}", target_boot, dir));
    }
    for (from, to) in boot_files {
        let target = banna::format!("{}{}", target_boot, to);
        if let Err(e) = fs::read(from).and_then(|d| fs::write(&target, &d)) {
            result = Err(banna::format!("{}: {}", from, fs::error_text(e)));
            break;
        }
    }
    if result.is_ok() {
        let config = "# Banna OS önyükleme ayarları\ntimeout=3\ncmdline=\n";
        let path = banna::format!("{}/boot/banna.cfg", target_boot);
        result = fs::write(&path, config.as_bytes())
            .map_err(|e| banna::format!("{}: {}", path, fs::error_text(e)));
    }
    let _ = fs::umount(&target_boot);
    let _ = fs::umount(TARGET);
    if let Err(e) = result {
        return fail(&e);
    }
    let _ = fs::rescan(&disk_name);
    step(6);
    io::log("banna-install: complete");
    println!("\x1b[92mKurulum tamamlandı.\x1b[0m");
    println!(
        "Live USB'yi çıkar, bilgisayarı /dev/{} diskinden (UEFI) başlat.",
        disk_name
    );
    println!("İlk açılışta yönetici hesabı oluşturulur.");
    0
}
