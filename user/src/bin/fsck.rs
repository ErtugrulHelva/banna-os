//! `fsck [-y] <aygıt | bağlama noktası>`: ext2 dosya sistemini denetler, `-y` ile onarır.
//! Bağlı birimler çekirdeğin kendi sürücüsüyle (aynı kod) denetlenir; bağlı olmayan
//! aygıtlar doğrudan okunur.

#![no_std]
#![no_main]

use banna::blockdev::BlockFile;
use banna::ext2::Ext2;
use banna::{eprintln, format, fs, println, process, system, time};

banna::entry!(main);

fn clock() -> u32 {
    time::unix_time() as u32
}

fn main(args: &[&str]) -> i32 {
    let repair = args.contains(&"-y");
    let Some(target) = args[1..].iter().find(|a| !a.starts_with('-')).copied() else {
        eprintln!(
            "usage: fsck [-y] <aygıt | bağlama noktası>   (ör. fsck /, sudo fsck -y /dev/usb1p1)"
        );
        return 1;
    };
    if repair && process::uid() != 0 {
        eprintln!(
            "fsck: onarım için yönetici gerekir: sudo fsck -y {}",
            target
        );
        return 1;
    }
    let storage = system::storage();
    let device = target.trim_start_matches("/dev/");
    let mounted = storage
        .mounted_at(target)
        .or_else(|| storage.mount_of(device))
        .map(|m| m.point.clone());
    if let Some(point) = mounted {
        let line = format!("fsck {}{}", if repair { "-y " } else { "" }, point);
        return match process::kcmd(&line) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("fsck: {}", fs::error_text(e));
                1
            }
        };
    }
    let dev = match BlockFile::open(device) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("fsck: /dev/{}: {}", device, fs::error_text(e));
            return 1;
        }
    };
    let mut volume = match Ext2::mount(dev, clock) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "fsck: /dev/{}: ext2 değil ya da okunamadı ({:?})",
                device, e
            );
            return 1;
        }
    };
    println!(
        "fsck: /dev/{} denetleniyor{}",
        device,
        if repair { " (onarım açık)" } else { "" }
    );
    match volume.check(repair, |line| println!("  {}", line)) {
        Ok(r) => {
            println!(
                "fsck: {} dizin, {} dosya, {} inode; boş {} blok / {} inode",
                r.directories, r.files, r.inodes, r.free_blocks, r.free_inodes
            );
            if r.clean() {
                println!("fsck: temiz");
                0
            } else if repair {
                println!(
                    "fsck: {} sorun bulundu, {} düzeltme yapıldı",
                    r.problems, r.fixed
                );
                0
            } else {
                println!(
                    "fsck: sorunlar var; onarmak için: sudo fsck -y /dev/{}",
                    device
                );
                4
            }
        }
        Err(e) => {
            eprintln!("fsck: {:?}", e);
            8
        }
    }
}
