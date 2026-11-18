//! Sürücü raporu (Sürücü Yöneticisi için `info` konu 3): PCI aygıtları ve onları süren
//! sürücüler, yüklü modüller ve USB aygıtları. Satırlar sekmeyle ayrılır:
//!   pci  <bdf> <satıcı:aygıt> <sınıf> <sürücü|-> <durum>
//!   mod  <ad> <boyut KiB>
//!   usb  <port> <satıcı:aygıt> <ad> <sürücüler>

use alloc::format;
use alloc::string::String;

use super::pci;

/// Aygıtı süren yerleşik sürücü ve durumu (sürücü dosya adıyla, ör. "xhci").
fn driver_of(d: &pci::Device) -> (&'static str, String) {
    let net = crate::net::report();
    let modules = crate::kernel::module::claimed(d);
    if let Some(name) = modules {
        return (name, String::from("modül"));
    }
    match (d.class, d.subclass, d.prog_if) {
        (0x02, 0x80, _) => ("-", String::from("sürücü yok (kablosuz)")),
        (0x02, _, _) => {
            let model = net.as_ref().map(|r| r.model).unwrap_or("");
            let driver = match d.vendor_id {
                0x8086 => "e1000",
                0x10ec if matches!(d.device_id, 0x8168 | 0x8161 | 0x8169 | 0x8167 | 0x8136) => {
                    "rtl8169"
                }
                0x1af4 => "virtio_net",
                _ => "-",
            };
            let state = match &net {
                Some(r) if driver != "-" && model_matches(model, driver) => {
                    if r.link_up {
                        format!("çalışıyor, bağlı {} Mb/s", r.speed)
                    } else {
                        String::from("çalışıyor, kablo takılı değil")
                    }
                }
                _ if driver == "-" => String::from("sürücü yok"),
                _ => String::from("başlatılamadı"),
            };
            (driver, state)
        }
        (0x0c, 0x03, 0x30) => ("xhci", String::from("çalışıyor")),
        (0x0c, 0x03, 0x20) => ("ehci", String::from("çalışıyor")),
        (0x0c, 0x03, 0x10) => ("ohci", String::from("çalışıyor")),
        (0x0c, 0x03, 0x00) => ("uhci", String::from("çalışıyor")),
        (0x01, 0x06, _) => ("ahci", String::from("çalışıyor")),
        (0x01, 0x08, _) => ("nvme", String::from("çalışıyor")),
        (0x01, 0x01, _) => ("ata", String::from("çalışıyor")),
        (0x04, 0x03 | 0x01, _) => (
            "hda",
            String::from(if crate::drivers::audio::present() {
                "çalışıyor"
            } else {
                "kullanılmıyor"
            }),
        ),
        (0x03, _, _) => match (d.vendor_id, crate::drivers::gpu::adapter()) {
            (0x10de, _) => ("dgpu", String::from("güç tasarrufu (kapalı)")),
            (0x1234, crate::drivers::gpu::Adapter::Bga) => ("bga", String::from("çalışıyor")),
            (0x1af4, crate::drivers::gpu::Adapter::VirtioGpu) => {
                ("virtio_gpu", String::from("çalışıyor"))
            }
            _ => ("framebuffer", String::from("firmware çerçeve arabelleği")),
        },
        (0x0c, 0x80, _) | (0x0c, 0x05, _) if d.vendor_id == 0x8086 => {
            ("i2c", String::from("dokunmatik yüzey denetleyicisi"))
        }
        (0x0d, _, _) => ("-", String::from("sürücü yok (kablosuz)")),
        (0x06, _, _) => ("pci", String::from("köprü")),
        (0x08, _, _) | (0x11, _, _) | (0x05, _, _) => ("-", String::from("sistem aygıtı")),
        _ => ("-", String::from("sürücü yok")),
    }
}

fn model_matches(model: &str, driver: &str) -> bool {
    match driver {
        "e1000" => model.contains("Intel"),
        "rtl8169" => {
            model.contains("8111")
                || model.contains("8168")
                || model.contains("8169")
                || model.contains("8101")
        }
        "virtio_net" => model.contains("virtio"),
        _ => false,
    }
}

pub fn text() -> String {
    let mut out = String::new();
    for d in pci::devices() {
        let (driver, state) = driver_of(&d);
        out.push_str(&format!(
            "pci\t{:02x}:{:02x}.{}\t{:04x}:{:04x}\t{}\t{}\t{}\n",
            d.bus,
            d.slot,
            d.function,
            d.vendor_id,
            d.device_id,
            d.class_name(),
            driver,
            state
        ));
    }
    for (name, _, size) in crate::kernel::module::loaded() {
        out.push_str(&format!("mod\t{}\t{}\n", name, size / 1024));
    }
    for a in crate::drivers::usb::devices() {
        out.push_str(&format!(
            "usb\t{}\t{:04x}:{:04x}\t{}\t{}\n",
            a.device.port,
            a.device.vendor,
            a.device.product,
            a.device.name.replace('\t', " "),
            if a.drivers.is_empty() {
                String::from("-")
            } else {
                a.drivers.join(",")
            }
        ));
    }
    out
}
