//! PCI yapılandırma alanı (mekanizma #1) ve veri yolu taraması.

use alloc::vec::Vec;
use core::fmt::Write;

use crate::arch::cpu::{inl, outl};
use crate::kernel::initcall::Detail;
use crate::sync::SpinLock;

const CONFIG_ADDRESS: u16 = 0xcf8;
const CONFIG_DATA: u16 = 0xcfc;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
    pub header_type: u8,
    pub interrupt_line: u8,
}

impl Device {
    pub fn read(&self, offset: u8) -> u32 {
        read_config(self.bus, self.slot, self.function, offset)
    }

    pub fn write(&self, offset: u8, value: u32) {
        write_config(self.bus, self.slot, self.function, offset, value)
    }

    /// Bellek/G-Ç erişimini ve DMA (bus master) yetkisini açar.
    pub fn enable(&self) {
        let command = self.read(0x04);
        self.write(0x04, command | 0x7);
    }

    /// PCI yetenek listesinde `id` kimlikli girdilerin konumları.
    pub fn capabilities(&self, id: u8) -> Vec<u8> {
        let mut out = Vec::new();
        if self.read(0x04) >> 16 & 0x10 == 0 {
            return out;
        }
        let mut offset = (self.read(0x34) & 0xfc) as u8;
        let mut guard = 0;
        while offset != 0 && guard < 48 {
            let header = self.read(offset);
            if header as u8 == id {
                out.push(offset);
            }
            offset = ((header >> 8) & 0xfc) as u8;
            guard += 1;
        }
        out
    }

    /// Aygıtın kesmesini MSI (yoksa MSI-X) ile `handler`'a bağlar; INTx kapatılır.
    /// Gerçek makinelerde PCI INTx hatları çoğunlukla GSI ≥ 16'dır ve yönlendirilmez.
    pub fn enable_msi(&self, handler: fn()) -> bool {
        if crate::boot::has_flag("nomsi") {
            return false;
        }
        let msi = self.capabilities(0x05).first().copied();
        let msix = self.capabilities(0x11).first().copied();
        if msi.is_none() && msix.is_none() {
            return false;
        }
        let Some(vector) = crate::arch::idt::alloc_msi_vector(handler) else {
            return false;
        };
        let destination = crate::arch::apic::lapic_id();
        if destination > 0xff {
            return false;
        }
        let address = 0xfee0_0000u32 | destination << 12;
        let data = vector as u32;
        if let Some(cap) = msi {
            let control = (self.read(cap) >> 16) as u16;
            self.write(cap + 4, address);
            if control & (1 << 7) != 0 {
                self.write(cap + 8, 0);
                self.write(cap + 12, data);
            } else {
                self.write(cap + 8, data);
            }
            // Tek ileti (MME = 0), etkin.
            let control = (control & !(0x7 << 4)) | 1;
            let header = self.read(cap) & 0xffff;
            self.write(cap, header | (control as u32) << 16);
        } else if let Some(cap) = msix {
            let table = self.read(cap + 4);
            let Some(bar) = self.bar((table & 0x7) as u8) else {
                return false;
            };
            let entry = bar + (table & !0x7) as u64;
            let Ok(regs) =
                crate::memory::paging::map_physical(entry, 16, crate::memory::paging::NO_CACHE)
            else {
                return false;
            };
            let header = self.read(cap);
            // Önce işlev maskesiyle etkinleştirilir, giriş 0 kurulur, sonra maske kalkar.
            self.write(cap, header | (1 << 31) | (1 << 30));
            unsafe {
                let words = regs as *mut u32;
                words.write_volatile(address);
                words.add(1).write_volatile(0);
                words.add(2).write_volatile(data);
                words.add(3).write_volatile(0);
            }
            self.write(cap, (header | (1 << 31)) & !(1 << 30));
        }
        let command = self.read(0x04);
        self.write(0x04, command | (1 << 10));
        crate::log!(
            "pci: {:02x}:{:02x}.{} {} vector {}",
            self.bus,
            self.slot,
            self.function,
            if msi.is_some() { "MSI" } else { "MSI-X" },
            vector
        );
        true
    }

    /// Kesmeyi açar: önce MSI/MSI-X, sonra ACPI _PRT ile INTx (GSI ≥ 16 dahil), en son
    /// firmware'in yazdığı ISA hattı. Hiçbiri olmazsa `false` (sürücü yoklamayla çalışır).
    pub fn enable_interrupt(&self, handler: fn()) -> bool {
        if self.enable_msi(handler) {
            return true;
        }
        let pin = (self.read(0x3c) >> 8) as u8;
        if pin != 0
            && let Some((gsi, level, active_low)) = intx_route(self, pin - 1)
        {
            let routed = if gsi < 16 && !level {
                crate::arch::idt::register_irq(gsi as u8, handler);
                true
            } else {
                crate::arch::idt::register_gsi(gsi, level, active_low, handler)
            };
            if routed {
                let command = self.read(0x04);
                self.write(0x04, command & !(1 << 10));
                crate::log!(
                    "pci: {:02x}:{:02x}.{} INT{} -> GSI {}{}",
                    self.bus,
                    self.slot,
                    self.function,
                    (b'A' + pin - 1) as char,
                    gsi,
                    if level { " (level)" } else { "" }
                );
                return true;
            }
        }
        let irq = self.interrupt_line;
        if irq != 0 && irq < 16 {
            crate::arch::idt::register_irq(irq, handler);
            return true;
        }
        if irq != 0 && irq != 0xff {
            return crate::arch::idt::register_gsi(irq as u32, true, true, handler);
        }
        false
    }

    pub fn bar(&self, index: u8) -> Option<u64> {
        if self.header_type & 0x7f != 0 || index > 5 {
            return None;
        }
        let low = self.read(0x10 + index * 4);
        if low & 1 != 0 {
            return Some((low & !0x3) as u64);
        }
        let base = (low & !0xf) as u64;
        if (low >> 1) & 0x3 == 0x2 && index < 5 {
            let high = self.read(0x14 + index * 4) as u64;
            return Some(base | (high << 32));
        }
        Some(base)
    }

    pub fn class_name(&self) -> &'static str {
        class_name(self.class, self.subclass, self.prog_if)
    }
}

static DEVICES: SpinLock<Vec<Device>> = SpinLock::new(Vec::new());

fn address(bus: u8, slot: u8, function: u8, offset: u8) -> u32 {
    (1 << 31)
        | ((bus as u32) << 16)
        | ((slot as u32) << 11)
        | ((function as u32) << 8)
        | (offset as u32 & 0xfc)
}

pub fn read_config(bus: u8, slot: u8, function: u8, offset: u8) -> u32 {
    crate::arch::cpu::without_interrupts(|| unsafe {
        outl(CONFIG_ADDRESS, address(bus, slot, function, offset));
        inl(CONFIG_DATA)
    })
}

pub fn write_config(bus: u8, slot: u8, function: u8, offset: u8, value: u32) {
    crate::arch::cpu::without_interrupts(|| unsafe {
        outl(CONFIG_ADDRESS, address(bus, slot, function, offset));
        outl(CONFIG_DATA, value);
    })
}

fn probe(bus: u8, slot: u8, function: u8) -> Option<Device> {
    let id = read_config(bus, slot, function, 0x00);
    let vendor_id = id as u16;
    if vendor_id == 0xffff {
        return None;
    }
    let class = read_config(bus, slot, function, 0x08);
    let header = read_config(bus, slot, function, 0x0c);
    let interrupt = read_config(bus, slot, function, 0x3c);
    Some(Device {
        bus,
        slot,
        function,
        vendor_id,
        device_id: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
        header_type: (header >> 16) as u8,
        interrupt_line: interrupt as u8,
    })
}

fn scan() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255u8 {
        for slot in 0..32u8 {
            let Some(first) = probe(bus, slot, 0) else {
                continue;
            };
            let multifunction = first.header_type & 0x80 != 0;
            devices.push(first);
            if multifunction {
                for function in 1..8u8 {
                    if let Some(device) = probe(bus, slot, function) {
                        devices.push(device);
                    }
                }
            }
        }
    }
    devices
}

pub fn find_all(class: u8, subclass: u8) -> Vec<Device> {
    DEVICES
        .lock()
        .iter()
        .filter(|d| d.class == class && d.subclass == subclass)
        .copied()
        .collect()
}

pub fn find_id(vendor: u16, device: u16) -> Option<Device> {
    DEVICES
        .lock()
        .iter()
        .find(|d| d.vendor_id == vendor && d.device_id == device)
        .copied()
}

pub fn devices() -> Vec<Device> {
    DEVICES.lock().clone()
}

pub fn class_name(class: u8, subclass: u8, prog_if: u8) -> &'static str {
    match (class, subclass, prog_if) {
        (0x01, 0x01, _) => "IDE controller",
        (0x01, 0x06, 0x01) => "AHCI SATA controller",
        (0x01, 0x06, _) => "SATA controller",
        (0x01, 0x08, 0x02) => "NVMe controller",
        (0x01, 0x00, _) => "SCSI controller",
        (0x01, _, _) => "Storage controller",
        (0x02, 0x00, _) => "Ethernet controller",
        (0x02, _, _) => "Network controller",
        (0x03, 0x00, _) => "VGA compatible controller",
        (0x03, _, _) => "Display controller",
        (0x04, 0x01, _) => "Audio device",
        (0x04, 0x03, _) => "HD Audio controller",
        (0x04, _, _) => "Multimedia controller",
        (0x05, _, _) => "Memory controller",
        (0x06, 0x00, _) => "Host bridge",
        (0x06, 0x01, _) => "ISA bridge",
        (0x06, 0x04, _) => "PCI-to-PCI bridge",
        (0x06, 0x80, _) => "Bridge",
        (0x06, _, _) => "Bridge device",
        (0x07, 0x00, _) => "Serial controller",
        (0x07, _, _) => "Communication controller",
        (0x08, _, _) => "System peripheral",
        (0x09, _, _) => "Input device controller",
        (0x0c, 0x03, 0x00) => "USB UHCI controller",
        (0x0c, 0x03, 0x10) => "USB OHCI controller",
        (0x0c, 0x03, 0x20) => "USB EHCI controller",
        (0x0c, 0x03, 0x30) => "USB xHCI controller",
        (0x0c, 0x05, _) => "SMBus controller",
        (0x0c, _, _) => "Serial bus controller",
        (0x0d, _, _) => "Wireless controller",
        _ => "Unknown device",
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let found = scan();
    if found.is_empty() {
        return Err("no PCI devices (config mechanism #1 missing?)");
    }
    for d in &found {
        crate::log!(
            "pci: {:02x}:{:02x}.{} {:04x}:{:04x} class {:02x}.{:02x}.{:02x} irq {} bar0 {:#x} {}",
            d.bus,
            d.slot,
            d.function,
            d.vendor_id,
            d.device_id,
            d.class,
            d.subclass,
            d.prog_if,
            d.interrupt_line,
            d.bar(0).unwrap_or(0),
            d.class_name()
        );
    }
    let _ = write!(detail, "{} devices", found.len());
    *DEVICES.lock() = found;
    Ok(())
}

crate::initcall!(driver, 02, "PCI bus", init);

/// `bus` numaralı veri yolunun ACPI düğümü: kök köprü ya da _ADR ile bulunan PCI köprüsü.
fn bus_node(bus: u8, depth: u8) -> Option<crate::aml::NodeId> {
    let roots: Vec<_> = ["PNP0A08", "PNP0A03"]
        .iter()
        .flat_map(|hid| crate::aml::find_devices(hid))
        .collect();
    for &root in &roots {
        let base = crate::aml::evaluate_child(root, "_BBN", Vec::new())
            .and_then(|r| r.ok())
            .and_then(|v| v.as_integer())
            .unwrap_or(0);
        if base == bus as u64 {
            return Some(root);
        }
    }
    if depth > 8 {
        return None;
    }
    let bridge = upstream_bridge(bus)?;
    let parent = bus_node(bridge.bus, depth + 1)?;
    crate::aml::child_with_adr(parent, (bridge.slot as u64) << 16 | bridge.function as u64)
}

fn upstream_bridge(bus: u8) -> Option<Device> {
    devices().into_iter().find(|d| {
        d.class == 0x06 && d.subclass == 0x04 && (d.read(0x18) >> 8) as u8 == bus && d.bus != bus
    })
}

/// INTx (0 = INTA) → (GSI, düzey, aktif düşük). Köprünün _PRT'si yoksa pin PCI kuralıyla
/// döndürülüp (swizzle) bir üst veri yolunda aranır.
fn intx_route(device: &Device, pin: u8) -> Option<(u32, bool, bool)> {
    let (mut bus, mut slot, mut pin) = (device.bus, device.slot, pin);
    for _ in 0..8 {
        if let Some(node) = bus_node(bus, 0) {
            let routes = crate::aml::pci_routes(node);
            if let Some(r) = routes
                .iter()
                .find(|r| r.slot == slot as u16 && r.pin == pin)
            {
                return match r.link {
                    Some(link) => crate::aml::link_interrupt(link),
                    None => Some((r.index, true, true)),
                };
            }
            if !routes.is_empty() {
                return None;
            }
        }
        let bridge = upstream_bridge(bus)?;
        pin = (pin + slot) % 4;
        bus = bridge.bus;
        slot = bridge.slot;
    }
    None
}
