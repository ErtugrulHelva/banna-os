//! `mkfs.ext2 <aygıt> [etiket]` ve `mkfs.fat <aygıt> [etiket]` (aynı program, adına göre).
//! Aygıt bağlıysa önce ayrılır; `-y` onay sormaz.

#![no_std]
#![no_main]

use banna::blockdev::BlockFile;
use banna::system::{self, size_text};
use banna::{String, eprintln, fat, fs, io, println, process};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    let tool = args[0].rsplit('/').next().unwrap_or(args[0]);
    let fat = tool.contains("fat") || args.contains(&"-t") && args.contains(&"fat");
    let rest: banna::Vec<&str> = args[1..]
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-') && *a != "fat" && *a != "ext2")
        .collect();
    let yes = args.contains(&"-y");
    let Some(device) = rest.first().map(|d| d.trim_start_matches("/dev/")) else {
        eprintln!(
            "usage: {} [-y] <aygıt> [etiket]   (ör. mkfs.ext2 /dev/usb1p1 VERI)",
            tool
        );
        return 1;
    };
    let label = rest
        .get(1)
        .copied()
        .unwrap_or(if fat { "BANNA" } else { "banna" });
    if process::uid() != 0 {
        eprintln!("{}: yönetici gerekir: sudo {} /dev/{}", tool, tool, device);
        return 1;
    }
    let storage = system::storage();
    if storage
        .mounts
        .iter()
        .any(|m| m.device == device && (m.point == "/" || m.point == "/boot"))
    {
        eprintln!(
            "{}: /dev/{} sistemin çalıştığı birim, biçimlendirilemez",
            tool, device
        );
        return 1;
    }
    if let Some(m) = storage.mount_of(device) {
        println!("{} ayrılıyor ({})", m.point, device);
        if let Err(e) = fs::umount(&m.point) {
            eprintln!("{}: {} ayrılamadı: {}", tool, m.point, fs::error_text(e));
            return 1;
        }
    }
    let dev = match BlockFile::open(device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("{}: /dev/{}: {}", tool, device, fs::error_text(e));
            return 1;
        }
    };
    let kind = if fat { "FAT32" } else { "ext2" };
    if !yes
        && !io::confirm(&banna::format!(
            "/dev/{} ({}) {} ile biçimlendirilecek, içindeki her şey silinecek. Emin misin?",
            device,
            size_text(dev.sectors() * 512),
            kind
        ))
    {
        println!("İptal edildi.");
        return 1;
    }
    println!("/dev/{} {} ile biçimlendiriliyor...", device, kind);
    let result = if fat {
        fat::format(&dev, label).map_err(String::from)
    } else {
        banna::blockdev::format_ext2(&dev, label).map_err(|e| banna::format!("{:?}", e))
    };
    if let Err(e) = result {
        eprintln!("{}: {}", tool, e);
        return 1;
    }
    drop(dev);
    let disk = storage.disk_of(device);
    let _ = fs::rescan(&disk);
    println!("Tamam: /dev/{} {} ('{}').", device, kind, label);
    let point = banna::format!("/media/{}", device);
    if fs::stat(&point).is_ok() {
        println!("Bağlandı: {}", point);
    }
    0
}
