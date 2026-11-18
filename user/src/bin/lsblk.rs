//! `lsblk`: diskler, bölümler ve bağlama noktaları.

#![no_std]
#![no_main]

use banna::println;
use banna::system::{self, size_text};

banna::entry!(main);

fn main(_: &[&str]) -> i32 {
    let s = system::storage();
    println!(
        "{:<12} {:>10}  {:<11} {:<16} MODEL / PARTUUID",
        "AD", "BOYUT", "TÜR", "BAĞLI"
    );
    for d in &s.disks {
        let mount = s.mount_of(&d.name).map_or("", |m| m.point.as_str());
        println!(
            "{:<12} {:>10}  {:<11} {:<16} {}{}",
            d.name,
            size_text(d.sectors * 512),
            "disk",
            mount,
            d.model,
            if d.removable {
                " (çıkarılabilir)"
            } else {
                ""
            }
        );
        for p in s.parts.iter().filter(|p| p.disk == d.name) {
            let mount = s.mount_of(&p.name).map_or(banna::String::new(), |m| {
                banna::format!("{} ({})", m.point, m.kind)
            });
            println!(
                "  {:<10} {:>10}  {:<11} {:<16} {}",
                p.name,
                size_text(p.sectors * 512),
                p.kind,
                mount,
                p.partuuid
            );
        }
    }
    0
}
