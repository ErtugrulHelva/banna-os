//! `gpt <disk>`: bölüm tablosunu gösterir.
//! `gpt new <disk> [ext2|fat] [etiket]`: diski tek bölümlü GPT yapar, biçimlendirir ve
//! `/media/<disk>p1` altına bağlar (ikinci bir USB belleği kullanıma hazırlamak için).

#![no_std]
#![no_main]

use banna::blockdev::{self, BlockFile};
use banna::system::{self, size_text};
use banna::{eprintln, fat, fs, gpt, io, println, process};

banna::entry!(main);

fn show(disk: &str) -> i32 {
    let dev = match BlockFile::open(disk) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("gpt: /dev/{}: {}", disk, fs::error_text(e));
            return 1;
        }
    };
    println!("/dev/{}: {}", disk, size_text(dev.sectors() * 512));
    match gpt::read(&dev) {
        None => println!("  GPT yok (bölümsüz ya da MBR)"),
        Some(entries) => {
            for e in entries {
                let kind = match e.kind.as_str() {
                    gpt::ESP => "EFI sistem",
                    gpt::LINUX_ROOT => "Linux kök",
                    gpt::LINUX_DATA => "Linux veri",
                    gpt::BASIC_DATA => "Veri (FAT)",
                    other => other,
                };
                println!(
                    "  {}p{}  {:>10}  {:<12} {:<10} {}",
                    disk,
                    e.index,
                    size_text((e.last - e.first + 1) * 512),
                    kind,
                    e.name,
                    e.unique
                );
            }
        }
    }
    0
}

fn new(disk: &str, use_fat: bool, label: &str) -> i32 {
    if process::uid() != 0 {
        eprintln!("gpt: yönetici gerekir: sudo gpt new {}", disk);
        return 1;
    }
    let storage = system::storage();
    if !storage.disks.iter().any(|d| d.name == disk) {
        eprintln!("gpt: {} bir disk değil (lsblk ile bak)", disk);
        return 1;
    }
    let mounts = storage.disk_mounts(disk);
    if mounts.iter().any(|m| m.point == "/" || m.point == "/boot") {
        eprintln!("gpt: {} sistemin çalıştığı disk", disk);
        return 1;
    }
    let dev = match BlockFile::open(disk) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("gpt: /dev/{}: {}", disk, fs::error_text(e));
            return 1;
        }
    };
    if !io::confirm(&banna::format!(
        "/dev/{} ({}) üzerindeki TÜM veriler silinecek. Emin misin?",
        disk,
        size_text(dev.sectors() * 512)
    )) {
        println!("İptal edildi.");
        return 1;
    }
    for m in &mounts {
        if let Err(e) = fs::umount(&m.point) {
            eprintln!("gpt: {} ayrılamadı: {}", m.point, fs::error_text(e));
            return 1;
        }
    }
    let kind = if use_fat {
        gpt::BASIC_DATA
    } else {
        gpt::LINUX_DATA
    };
    let ranges = match gpt::write(
        &dev,
        &[gpt::NewPartition {
            kind,
            name: label,
            sectors: None,
        }],
    ) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("gpt: yazılamadı: {}", fs::error_text(e));
            return 1;
        }
    };
    drop(dev);
    if let Err(e) = fs::rescan(disk) {
        eprintln!("gpt: yeniden taranamadı: {}", fs::error_text(e));
        return 1;
    }
    let part = banna::format!("{}p1", disk);
    let point = banna::format!("/media/{}", part);
    let _ = fs::umount(&point);
    let dev = match BlockFile::open(&part) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("gpt: /dev/{}: {}", part, fs::error_text(e));
            return 1;
        }
    };
    println!(
        "Bölüm /dev/{} ({}) {} ile biçimlendiriliyor...",
        part,
        size_text((ranges[0].1 - ranges[0].0 + 1) * 512),
        if use_fat { "FAT32" } else { "ext2" }
    );
    let result = if use_fat {
        fat::format(&dev, label).map_err(banna::String::from)
    } else {
        blockdev::format_ext2(&dev, label).map_err(|e| banna::format!("{:?}", e))
    };
    drop(dev);
    if let Err(e) = result {
        eprintln!("gpt: biçimlendirilemedi: {}", e);
        return 1;
    }
    let _ = fs::rescan(disk);
    if !use_fat {
        // Yeni ext2'nin kökü root'a ait; oturum kullanıcısı da yazabilsin.
        let _ = fs::chmod(&point, 0o1777);
    }
    println!("Hazır: {} (etiket '{}').", point, label);
    0
}

fn main(args: &[&str]) -> i32 {
    match args.get(1..).unwrap_or(&[]) {
        [disk] | ["show", disk] => show(disk.trim_start_matches("/dev/")),
        ["new", disk, rest @ ..] => {
            let use_fat = rest.first().is_some_and(|f| *f == "fat");
            let label = rest
                .iter()
                .copied()
                .find(|a| *a != "fat" && *a != "ext2")
                .unwrap_or(if use_fat { "BANNAUSB" } else { "banna-usb" });
            new(disk.trim_start_matches("/dev/"), use_fat, label)
        }
        _ => {
            eprintln!("usage: gpt <disk>");
            eprintln!("       sudo gpt new <disk> [ext2|fat] [etiket]   (diski sıfırdan hazırlar)");
            1
        }
    }
}
