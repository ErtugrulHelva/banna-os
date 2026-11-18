//! ACPI tabloları: RSDP, RSDT/XSDT, FADT, MADT ve DSDT/SSDT bayt erişimi.

use alloc::vec::Vec;
use core::fmt::Write;
use core::ptr::read_unaligned;
use core::slice;

use crate::arch::cpu::{inw, outb};
use crate::boot;
use crate::kernel::initcall::Detail;
use crate::memory::hhdm_offset;
use crate::memory::paging::map_physical;
use crate::sync::SpinLock;

use super::rtc;

#[derive(Clone, Copy)]
pub struct Table {
    pub signature: [u8; 4],
    pub phys: u64,
    pub length: u32,
    pub revision: u8,
    pub oem_id: [u8; 6],
}

impl Table {
    pub fn signature_str(&self) -> &str {
        core::str::from_utf8(&self.signature).unwrap_or("????")
    }

    pub fn oem_str(&self) -> &str {
        core::str::from_utf8(&self.oem_id).unwrap_or("").trim_end()
    }

    pub fn bytes(&self) -> &'static [u8] {
        let ptr = map_physical(self.phys, self.length as u64, 0).expect("ACPI table vanished");
        unsafe { slice::from_raw_parts(ptr, self.length as usize) }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct GenericAddress {
    pub space: u8,
    pub address: u64,
}

#[derive(Clone, Copy, Default, Debug)]
pub struct Fadt {
    pub revision: u8,
    pub sci_interrupt: u16,
    pub smi_command: u32,
    pub acpi_enable: u8,
    pub pm1a_control: u32,
    pub pm1b_control: u32,
    /// PM1 olay blokları (durum + etkinleştirme yarıları) ve GPE0 bloğu.
    pub pm1a_event: u32,
    pub pm1_event_len: u8,
    pub gpe0_block: u32,
    pub gpe0_len: u8,
    pub century: u8,
    pub boot_arch: u16,
    pub flags: u32,
    pub preferred_profile: u8,
    pub reset_register: Option<GenericAddress>,
    pub reset_value: u8,
    pub dsdt: u64,
    /// ACPI PM zamanlayıcısının G/Ç portu (3.579545 MHz); yoksa 0.
    pub pm_timer: u16,
}

impl Fadt {
    pub const FLAG_TMR_VAL_EXT: u32 = 1 << 8;
    pub const FLAG_RESET_REG_SUPPORTED: u32 = 1 << 10;
    pub const FLAG_HW_REDUCED: u32 = 1 << 20;
    pub const BOOT_ARCH_8042: u16 = 1 << 1;

    pub fn profile_name(&self) -> &'static str {
        match self.preferred_profile {
            1 => "desktop",
            2 => "mobile",
            3 => "workstation",
            4 => "enterprise server",
            5 => "SOHO server",
            6 => "appliance",
            7 => "performance server",
            8 => "tablet",
            _ => "unspecified",
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Madt {
    pub local_apic_address: u64,
    pub cpus: u32,
    pub enabled_cpus: u32,
    pub io_apics: u32,
    pub overrides: u32,
    pub legacy_pics: bool,
    pub io_apic_list: [IoApicEntry; 4],
    pub override_list: [InterruptOverride; 16],
    /// Etkin işlemcilerin LAPIC kimlikleri (xAPIC ve x2APIC kayıtları).
    pub lapic_ids: [u32; 256],
    pub lapic_count: usize,
}

impl Madt {
    fn add_lapic(&mut self, id: u32) {
        let known = &self.lapic_ids[..self.lapic_count];
        if id != 0xff
            && id != u32::MAX
            && !known.contains(&id)
            && self.lapic_count < self.lapic_ids.len()
        {
            self.lapic_ids[self.lapic_count] = id;
            self.lapic_count += 1;
        }
    }
}

impl Default for Madt {
    fn default() -> Self {
        Self {
            local_apic_address: 0,
            cpus: 0,
            enabled_cpus: 0,
            io_apics: 0,
            overrides: 0,
            legacy_pics: false,
            io_apic_list: Default::default(),
            override_list: Default::default(),
            lapic_ids: [0; 256],
            lapic_count: 0,
        }
    }
}

#[derive(Clone, Copy, Default, Debug)]
pub struct IoApicEntry {
    pub id: u8,
    pub address: u32,
    pub gsi_base: u32,
}

/// ISA IRQ → GSI yönlendirmesi; `flags`: bit 0-1 kutup, bit 2-3 tetikleme.
#[derive(Clone, Copy, Default, Debug)]
pub struct InterruptOverride {
    pub source: u8,
    pub gsi: u32,
    pub flags: u16,
}

#[derive(Clone, Copy, Debug)]
pub struct SleepType {
    pub a: u8,
    pub b: u8,
}

pub struct Acpi {
    pub revision: u8,
    pub oem_id: [u8; 6],
    pub tables: Vec<Table>,
    pub fadt: Option<Fadt>,
    pub madt: Option<Madt>,
    pub s5: Option<SleepType>,
}

static ACPI: SpinLock<Option<Acpi>> = SpinLock::new(None);

fn read<T: Copy>(bytes: &[u8], offset: usize) -> Option<T> {
    (offset + size_of::<T>() <= bytes.len())
        .then(|| unsafe { read_unaligned(bytes.as_ptr().add(offset) as *const T) })
}

fn checksum_ok(bytes: &[u8]) -> bool {
    bytes.iter().fold(0u8, |sum, b| sum.wrapping_add(*b)) == 0
}

fn map_bytes(phys: u64, len: usize) -> Result<&'static [u8], &'static str> {
    let ptr = map_physical(phys, len as u64, 0).map_err(|_| "cannot map ACPI memory")?;
    Ok(unsafe { slice::from_raw_parts(ptr, len) })
}

fn load_table(phys: u64) -> Result<Table, &'static str> {
    let header = map_bytes(phys, 36)?;
    let length = read::<u32>(header, 4).ok_or("short header")?;
    if length < 36 {
        return Err("table length too small");
    }
    let bytes = map_bytes(phys, length as usize)?;
    if !checksum_ok(bytes) {
        return Err("bad table checksum");
    }
    Ok(Table {
        signature: header[0..4].try_into().unwrap(),
        phys,
        length,
        revision: header[8],
        oem_id: header[10..16].try_into().unwrap(),
    })
}

fn parse_gas(bytes: &[u8], offset: usize) -> Option<GenericAddress> {
    Some(GenericAddress {
        space: read(bytes, offset)?,
        address: read(bytes, offset + 4)?,
    })
}

fn parse_fadt(bytes: &[u8]) -> Fadt {
    let dsdt32 = read::<u32>(bytes, 40).unwrap_or(0) as u64;
    let x_dsdt = read::<u64>(bytes, 140).unwrap_or(0);
    let flags = read::<u32>(bytes, 112).unwrap_or(0);
    let reset_register = parse_gas(bytes, 116)
        .filter(|gas| flags & Fadt::FLAG_RESET_REG_SUPPORTED != 0 && gas.address != 0);
    Fadt {
        revision: bytes[8],
        sci_interrupt: read(bytes, 46).unwrap_or(0),
        smi_command: read(bytes, 48).unwrap_or(0),
        acpi_enable: read(bytes, 52).unwrap_or(0),
        pm1a_control: read(bytes, 64).unwrap_or(0),
        pm1b_control: read(bytes, 68).unwrap_or(0),
        pm1a_event: read(bytes, 56).unwrap_or(0),
        pm1_event_len: read(bytes, 88).unwrap_or(0),
        gpe0_block: read(bytes, 80).unwrap_or(0),
        gpe0_len: read(bytes, 92).unwrap_or(0),
        century: read(bytes, 108).unwrap_or(0),
        boot_arch: read(bytes, 109).unwrap_or(0),
        flags,
        preferred_profile: read(bytes, 45).unwrap_or(0),
        reset_register,
        reset_value: read(bytes, 128).unwrap_or(0),
        dsdt: if x_dsdt != 0 { x_dsdt } else { dsdt32 },
        pm_timer: parse_gas(bytes, 208)
            .filter(|gas| gas.space == 1 && gas.address != 0 && gas.address <= 0xffff)
            .map(|gas| gas.address as u16)
            .unwrap_or_else(|| read::<u32>(bytes, 76).unwrap_or(0).min(0xffff) as u16),
    }
}

fn parse_madt(bytes: &[u8]) -> Madt {
    let mut madt = Madt {
        local_apic_address: read::<u32>(bytes, 36).unwrap_or(0) as u64,
        legacy_pics: read::<u32>(bytes, 40).unwrap_or(0) & 1 != 0,
        ..Madt::default()
    };
    let mut offset = 44;
    while offset + 2 <= bytes.len() {
        let kind = bytes[offset];
        let len = bytes[offset + 1] as usize;
        if len < 2 || offset + len > bytes.len() {
            break;
        }
        let entry = &bytes[offset..offset + len];
        match kind {
            0 => {
                madt.cpus += 1;
                let flags = read::<u32>(entry, 4).unwrap_or(0);
                if flags & 1 != 0 {
                    madt.enabled_cpus += 1;
                    madt.add_lapic(entry[3] as u32);
                }
            }
            1 => {
                if let Some(slot) = madt.io_apic_list.get_mut(madt.io_apics as usize) {
                    *slot = IoApicEntry {
                        id: entry[2],
                        address: read(entry, 4).unwrap_or(0),
                        gsi_base: read(entry, 8).unwrap_or(0),
                    };
                }
                madt.io_apics += 1;
            }
            2 => {
                if let Some(slot) = madt.override_list.get_mut(madt.overrides as usize) {
                    *slot = InterruptOverride {
                        source: entry[3],
                        gsi: read(entry, 4).unwrap_or(0),
                        flags: read(entry, 8).unwrap_or(0),
                    };
                }
                madt.overrides += 1;
            }
            5 => madt.local_apic_address = read(entry, 4).unwrap_or(madt.local_apic_address),
            9 => {
                madt.cpus += 1;
                if read::<u32>(entry, 8).unwrap_or(0) & 1 != 0 {
                    madt.enabled_cpus += 1;
                    madt.add_lapic(read(entry, 4).unwrap_or(u32::MAX));
                }
            }
            _ => {}
        }
        offset += len;
    }
    madt
}

fn aml_integer(bytes: &[u8], at: &mut usize) -> Option<u64> {
    let op = *bytes.get(*at)?;
    *at += 1;
    match op {
        0x00 => Some(0),
        0x01 => Some(1),
        0xff => Some(u64::MAX),
        0x0a => {
            let v = *bytes.get(*at)? as u64;
            *at += 1;
            Some(v)
        }
        0x0b => {
            let v = read::<u16>(bytes, *at)? as u64;
            *at += 2;
            Some(v)
        }
        0x0c => {
            let v = read::<u32>(bytes, *at)? as u64;
            *at += 4;
            Some(v)
        }
        _ => None,
    }
}

/// `Name(_S5, Package() {a, b, ...})` tanımını AML içinde arar.
fn find_s5(aml: &[u8]) -> Option<SleepType> {
    let position = aml.windows(4).position(|w| w == b"_S5_")?;
    let name_op = position >= 1 && aml[position - 1] == 0x08
        || position >= 2 && aml[position - 2] == 0x08 && aml[position - 1] == b'\\';
    if !name_op {
        return None;
    }
    let mut at = position + 4;
    if *aml.get(at)? != 0x12 {
        return None;
    }
    at += 1;
    let extra = (aml.get(at)? >> 6) as usize;
    at += 1 + extra + 1;
    let a = aml_integer(aml, &mut at)? as u8;
    let b = aml_integer(aml, &mut at)? as u8;
    Some(SleepType { a, b })
}

fn enable_acpi_mode(fadt: &Fadt) {
    if fadt.flags & Fadt::FLAG_HW_REDUCED != 0 || fadt.pm1a_control == 0 {
        return;
    }
    let sci_enabled = || unsafe { inw(fadt.pm1a_control as u16) } & 1 != 0;
    if sci_enabled() || fadt.smi_command == 0 || fadt.acpi_enable == 0 {
        return;
    }
    unsafe { outb(fadt.smi_command as u16, fadt.acpi_enable) };
    let enabled = (0..1_000_000).any(|_| sci_enabled());
    crate::log!("acpi: switched to ACPI mode: {}", enabled);
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let response = boot::RSDP.response().ok_or("no RSDP from bootloader")?;
    let rsdp_phys = if response.address >= hhdm_offset() {
        response.address - hhdm_offset()
    } else {
        response.address
    };
    let rsdp = map_bytes(rsdp_phys, 36)?;
    if &rsdp[0..8] != b"RSD PTR " || !checksum_ok(&rsdp[0..20]) {
        return Err("invalid RSDP");
    }
    let revision = rsdp[15];
    let oem_id: [u8; 6] = rsdp[9..15].try_into().unwrap();

    let xsdt = read::<u64>(rsdp, 24).filter(|&x| revision >= 2 && x != 0);
    let (root_phys, entry_size) = match xsdt {
        Some(x) => (x, 8),
        None => (read::<u32>(rsdp, 16).unwrap() as u64, 4),
    };
    let root = load_table(root_phys)?;
    let root_bytes = root.bytes();
    let count = (root.length as usize - 36) / entry_size;

    let mut tables = Vec::with_capacity(count + 1);
    for i in 0..count {
        let phys = if entry_size == 8 {
            read::<u64>(root_bytes, 36 + i * 8).unwrap()
        } else {
            read::<u32>(root_bytes, 36 + i * 4).unwrap() as u64
        };
        match load_table(phys) {
            Ok(table) => tables.push(table),
            Err(reason) => crate::log!("acpi: skipping table at {:#x}: {}", phys, reason),
        }
    }

    let fadt = tables
        .iter()
        .find(|t| &t.signature == b"FACP")
        .map(|t| parse_fadt(t.bytes()));
    let madt = tables
        .iter()
        .find(|t| &t.signature == b"APIC")
        .map(|t| parse_madt(t.bytes()));

    let mut s5 = None;
    if let Some(ecdt) = tables.iter().find(|t| &t.signature == b"ECDT") {
        let bytes = ecdt.bytes();
        if let (Some(control), Some(data)) = (parse_gas(bytes, 36), parse_gas(bytes, 48))
            && control.space == 1
            && data.space == 1
            && control.address != 0
        {
            super::ec::configure(data.address as u16, control.address as u16);
        }
    }

    if let Some(fadt) = &fadt {
        if fadt.dsdt != 0 {
            match load_table(fadt.dsdt) {
                Ok(dsdt) => {
                    s5 = find_s5(dsdt.bytes());
                    tables.push(dsdt);
                }
                Err(reason) => crate::log!("acpi: DSDT unusable: {}", reason),
            }
        }
        if fadt.century != 0 {
            rtc::set_century_register(fadt.century);
        }
        enable_acpi_mode(fadt);
    }

    for table in &tables {
        crate::log!(
            "acpi: {} rev {} at {:#x} ({} bytes, OEM '{}')",
            table.signature_str(),
            table.revision,
            table.phys,
            table.length,
            table.oem_str()
        );
    }
    if let Some(fadt) = &fadt {
        crate::log!(
            "acpi: FADT rev {} profile {} sci {} pm1a_cnt {:#x} century {} reset {:?} s5 {:?}",
            fadt.revision,
            fadt.profile_name(),
            fadt.sci_interrupt,
            fadt.pm1a_control,
            fadt.century,
            fadt.reset_register
                .map(|r| (r.space, r.address, fadt.reset_value)),
            s5
        );
    }
    if let Some(madt) = &madt {
        crate::log!(
            "acpi: MADT {} CPU(s) ({} enabled), {} I/O APIC(s), LAPIC {:#x}",
            madt.cpus,
            madt.enabled_cpus,
            madt.io_apics,
            madt.local_apic_address
        );
    }

    let _ = write!(
        detail,
        "rev {}, {} tables, {} CPU(s)",
        revision,
        tables.len(),
        madt.map(|m| m.enabled_cpus).unwrap_or(1)
    );
    *ACPI.lock() = Some(Acpi {
        revision,
        oem_id,
        tables,
        fadt,
        madt,
        s5,
    });
    Ok(())
}

crate::initcall!(core, 40, "ACPI tables", init);

pub fn with<R>(f: impl FnOnce(&Acpi) -> R) -> Option<R> {
    ACPI.lock().as_ref().map(f)
}

pub fn fadt() -> Option<Fadt> {
    with(|acpi| acpi.fadt).flatten()
}

pub fn s5() -> Option<SleepType> {
    with(|acpi| acpi.s5).flatten()
}

/// DSDT önce, ardından SSDT'ler: (revizyon, imza, AML gövdesi).
pub fn aml_tables() -> Vec<(u8, &'static str, &'static [u8])> {
    with(|acpi| {
        let mut out: Vec<(u8, &'static str, &'static [u8])> = Vec::new();
        for want in [b"DSDT", b"SSDT"] {
            for t in acpi.tables.iter().filter(|t| &t.signature == want) {
                let name = if want == b"DSDT" { "DSDT" } else { "SSDT" };
                out.push((t.revision, name, &t.bytes()[36..]));
            }
        }
        out
    })
    .unwrap_or_default()
}
