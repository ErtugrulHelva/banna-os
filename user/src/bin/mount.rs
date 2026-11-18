//! `mount`: bağlı dosya sistemleri; `mount <aygıt> <dizin>` (root) aygıtı bağlar.

#![no_std]
#![no_main]

use banna::{eprintln, fs, println, system};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    match args {
        [_] => {
            for m in system::storage().mounts {
                println!("/dev/{} {} ({}, '{}')", m.device, m.point, m.kind, m.label);
            }
            0
        }
        [_, device, point] => match fs::mount(device, point) {
            Ok(()) => 0,
            Err(e) => {
                eprintln!("mount: {} -> {}: {}", device, point, fs::error_text(e));
                if e == -1 {
                    eprintln!("mount: yönetici gerekir: sudo mount {} {}", device, point);
                }
                1
            }
        },
        _ => {
            eprintln!("usage: mount [<aygıt> <dizin>]   (ör. sudo mount /dev/usb1p1 /mnt)");
            1
        }
    }
}
