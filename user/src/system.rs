//! Çekirdek komut satırı ve depolama raporu (`info` syscall'ı).

use crate::sys;
use crate::{String, Vec};

fn info(topic: u64) -> String {
    let mut buf = crate::vec![0u8; 4096];
    loop {
        let n = sys::call(
            sys::INFO,
            &[topic, buf.as_mut_ptr() as u64, buf.len() as u64],
        );
        if n < 0 {
            return String::new();
        }
        if (n as usize) <= buf.len() {
            return String::from_utf8_lossy(&buf[..n as usize]).into_owned();
        }
        buf.resize(n as usize, 0);
    }
}

pub fn cmdline() -> String {
    info(0)
}

pub fn has_flag(flag: &str) -> bool {
    cmdline().split_whitespace().any(|w| w == flag)
}

#[derive(Clone, Debug)]
pub struct Disk {
    pub name: String,
    pub sectors: u64,
    pub removable: bool,
    pub model: String,
}

#[derive(Clone, Debug)]
pub struct Part {
    pub name: String,
    pub disk: String,
    pub sectors: u64,
    /// `efi`, `linux-root`, `gpt` ya da MBR türü.
    pub kind: String,
    pub partuuid: String,
}

#[derive(Clone, Debug)]
pub struct Mount {
    pub point: String,
    pub device: String,
    pub kind: String,
    pub label: String,
}

#[derive(Default, Debug)]
pub struct Storage {
    pub disks: Vec<Disk>,
    pub parts: Vec<Part>,
    pub mounts: Vec<Mount>,
}

impl Storage {
    /// Aygıtın (disk ya da bölüm) bağlandığı yer.
    pub fn mount_of(&self, device: &str) -> Option<&Mount> {
        self.mounts.iter().find(|m| m.device == device)
    }

    /// `point` noktasına bağlı birim.
    pub fn mounted_at(&self, point: &str) -> Option<&Mount> {
        self.mounts.iter().find(|m| m.point == point)
    }

    /// Bölümün diski (bölüm değilse kendisi).
    pub fn disk_of(&self, device: &str) -> String {
        self.parts
            .iter()
            .find(|p| p.name == device)
            .map_or_else(|| String::from(device), |p| p.disk.clone())
    }

    /// Diskin kendisi ya da bölümlerinden biri bağlı mı.
    pub fn disk_mounts(&self, disk: &str) -> Vec<&Mount> {
        self.mounts
            .iter()
            .filter(|m| self.disk_of(&m.device) == disk)
            .collect()
    }
}

pub fn storage() -> Storage {
    let mut out = Storage::default();
    for line in info(1).lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        match f.as_slice() {
            ["disk", name, sectors, removable, model] => out.disks.push(Disk {
                name: String::from(*name),
                sectors: sectors.parse().unwrap_or(0),
                removable: *removable == "1",
                model: model.replace('_', " "),
            }),
            ["part", name, disk, sectors, kind, id] => out.parts.push(Part {
                name: String::from(*name),
                disk: String::from(*disk),
                sectors: sectors.parse().unwrap_or(0),
                kind: String::from(*kind),
                partuuid: String::from(*id),
            }),
            ["mount", point, device, kind, label] => out.mounts.push(Mount {
                point: String::from(*point),
                device: String::from(*device),
                kind: String::from(*kind),
                label: label.replace('_', " "),
            }),
            _ => {}
        }
    }
    out
}

/// İnsan için boyut: "512 MiB", "14.6 GiB".
pub fn size_text(bytes: u64) -> String {
    const GIB: u64 = 1024 * 1024 * 1024;
    if bytes >= GIB {
        crate::format!("{}.{} GiB", bytes / GIB, bytes % GIB * 10 / GIB)
    } else {
        crate::format!("{} MiB", bytes / (1024 * 1024))
    }
}

/// Sürücü raporu (aygıtlar, sürücüler, modüller, USB): `pci`/`mod`/`usb` satırları.
pub fn drivers_report() -> String {
    info(3)
}
