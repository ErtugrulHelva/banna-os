//! HID over I2C (dizüstü dokunmatik yüzeyleri): ACPI'de PNP0C50 aygıtlarını bulur,
//! bağlı oldukları DesignWare I2C denetleyicisini (PCI ya da ACPI MMIO) açar, HID
//! tanımlayıcısını okur ve giriş yazmacını yoklayarak fare olaylarına çevirir.
//! GPIO kesmesi kullanılmaz: aygıt rapor yokken sıfır uzunluk döndürür.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::fmt::Write;
use core::sync::atomic::{AtomicUsize, Ordering};

use super::i2c::{self, Controller, Timing};
use super::mouse;
use super::pci;
use super::usb::hid;
use super::usb::hid_report::{self, Field, Pointer};
use crate::aml::{self, Resource, value::Value};
use crate::kernel::initcall::Detail;
use crate::memory::paging::{NO_CACHE, map_physical};

/// HID I2C _DSM kimliği 3CDFF6F7-4267-4555-AD05-B30A3D8938DE.
const DSM_UUID: [u8; 16] = [
    0xf7, 0xf6, 0xdf, 0x3c, 0x67, 0x42, 0x55, 0x45, 0xad, 0x05, 0xb3, 0x0a, 0x3d, 0x89, 0x38, 0xde,
];
const OP_RESET: u8 = 0x01;
const OP_SET_POWER: u8 = 0x08;
const POLL_MS: u64 = 8;
/// Mutlak parmak konumu → piksel: pad genişliğinde bir kaydırma ≈ bu kadar piksel.
const PAD_SPAN_PX: i64 = 1600;

static FOUND: AtomicUsize = AtomicUsize::new(0);

enum Mode {
    Pointer(Pointer),
    /// Yalnızca mutlak parmak raporu veren yüzey: konum farkı göreli harekete çevrilir.
    Touchpad {
        pointer: Pointer,
        tip: Field,
        last: Option<(i32, i32)>,
        rest: (i64, i64),
    },
}

struct Device {
    name: String,
    bus: Arc<Controller>,
    address: u16,
    ten_bit: bool,
    input_len: usize,
    mode: Mode,
}

impl Device {
    fn poll(&mut self, buf: &mut [u8]) {
        let buf = &mut buf[..self.input_len];
        if self
            .bus
            .transfer(self.address, self.ten_bit, &[], buf)
            .is_err()
        {
            return;
        }
        let len = u16::from_le_bytes([buf[0], buf[1]]) as usize;
        if len <= 2 || len > buf.len() {
            return;
        }
        let report = &buf[2..len];
        match &mut self.mode {
            Mode::Pointer(pointer) => hid::pointer_report(pointer, report),
            Mode::Touchpad {
                pointer,
                tip,
                last,
                rest,
            } => touchpad_report(pointer, *tip, last, rest, report),
        }
    }
}

fn touchpad_report(
    pointer: &Pointer,
    tip: Field,
    last: &mut Option<(i32, i32)>,
    rest: &mut (i64, i64),
    report: &[u8],
) {
    let data = if pointer.uses_report_ids {
        match report.split_first() {
            Some((&id, rest)) if id == pointer.report_id => rest,
            _ => return,
        }
    } else {
        report
    };
    let (Some(xf), Some(yf)) = (pointer.x, pointer.y) else {
        return;
    };
    let mut buttons = 0u8;
    for f in &pointer.buttons {
        if f.read(data).unwrap_or(0) != 0 {
            buttons |= 1 << (f.usage - 1);
        }
    }
    let touching = tip.read(data).unwrap_or(0) != 0;
    let pos = xf.read(data).zip(yf.read(data));
    let (mut dx, mut dy) = (0, 0);
    match (touching, pos, *last) {
        (true, Some((x, y)), Some((lx, ly))) => {
            let range = (xf.logical_max - xf.logical_min).max(1) as i64;
            let sx = (x - lx) as i64 * PAD_SPAN_PX + rest.0;
            let sy = (y - ly) as i64 * PAD_SPAN_PX + rest.1;
            dx = (sx / range) as i32;
            dy = (sy / range) as i32;
            *rest = (sx % range, sy % range);
            *last = Some((x, y));
        }
        (true, Some(p), None) => {
            *last = Some(p);
            *rest = (0, 0);
        }
        _ => *last = None,
    }
    mouse::inject_relative(dx, dy, buttons, 0);
}

fn integer(value: &Value) -> Option<u64> {
    value.as_integer()
}

/// ACPI SSCN/FMCN paketi: (HCNT, LCNT, SDA bekletme).
fn acpi_timing(controller: aml::namespace::NodeId, name: &str) -> Option<Timing> {
    let value = aml::evaluate_child(controller, name, Vec::new())?.ok()?;
    let items = value.as_package()?;
    let hcnt = integer(items.first()?)?;
    let lcnt = integer(items.get(1)?)?;
    if hcnt == 0 || lcnt == 0 || hcnt > 0xffff || lcnt > 0xffff {
        return None;
    }
    Some(Timing {
        hcnt: hcnt as u16,
        lcnt: lcnt as u16,
        sda_hold: items.get(2).and_then(integer).map(|h| h as u32),
    })
}

/// PCI aygıtını D0 güç durumuna alır.
fn power_up_pci(dev: &pci::Device) {
    if let Some(&cap) = dev.capabilities(0x01).first() {
        let pmcsr = dev.read(cap + 4);
        if pmcsr & 3 != 0 {
            dev.write(cap + 4, pmcsr & !3);
            crate::sched::sleep_ms(10);
        }
    }
    dev.enable();
}

/// Denetleyicinin yazmaç alanını bulur ve eşler: önce _CRS bellek kaynağı (AMD,
/// Intel ACPI kipi), yoksa _ADR ile PCI aygıtı (Intel LPSS).
fn map_controller(node: aml::namespace::NodeId) -> Result<*mut u8, &'static str> {
    if let Some(Ok(crs)) = aml::evaluate_child(node, "_CRS", Vec::new())
        && let Some(bytes) = crs.as_bytes()
        && let Some((base, _)) = aml::parse_resources(&bytes)
            .into_iter()
            .find_map(|r| match r {
                Resource::Memory { base, length } if base != 0 && length >= 0x100 => {
                    Some((base, length))
                }
                _ => None,
            })
    {
        return map_physical(base, 0x1000, NO_CACHE).map_err(|_| "cannot map registers");
    }
    let adr = aml::evaluate_child(node, "_ADR", Vec::new())
        .and_then(|r| r.ok())
        .and_then(|v| v.as_integer())
        .ok_or("controller has neither MMIO nor _ADR")?;
    let (slot, function) = (((adr >> 16) & 0x1f) as u8, (adr & 0x7) as u8);
    let devices = pci::devices();
    let dev = devices
        .iter()
        .find(|d| d.bus == 0 && d.slot == slot && d.function == function)
        .or_else(|| {
            devices.iter().find(|d| {
                d.slot == slot && d.function == function && matches!(d.class, 0x0c | 0x11)
            })
        })
        .ok_or("controller PCI device not found")?;
    power_up_pci(dev);
    let base = dev
        .bar(0)
        .filter(|&b| b != 0)
        .ok_or("controller has no BAR0")?;
    let regs = map_physical(base, 0x1000, NO_CACHE).map_err(|_| "cannot map registers")?;
    if dev.vendor_id == 0x8086 {
        // LPSS özel yazmaçları: işlev ve iDMA sıfırlamasını kaldır, adres eşlemesi.
        unsafe {
            let private = regs.add(0x200);
            core::ptr::write_volatile(private.add(0x40) as *mut u32, base as u32);
            core::ptr::write_volatile(private.add(0x44) as *mut u32, (base >> 32) as u32);
            core::ptr::write_volatile(private.add(0x04) as *mut u32, 0x7);
        }
        crate::sched::sleep_ms(1);
    }
    Ok(regs)
}

fn open_controller(
    node: aml::namespace::NodeId,
    speed: u32,
) -> Result<Arc<Controller>, &'static str> {
    if aml::evaluate_child(node, "_PS0", Vec::new()).is_some() {
        crate::sched::sleep_ms(1);
    }
    let regs = map_controller(node)?;
    let standard = acpi_timing(node, "SSCN").unwrap_or(i2c::DEFAULT_STANDARD);
    let fast = if speed >= 400_000 {
        Some(acpi_timing(node, "FMCN").unwrap_or(i2c::DEFAULT_FAST))
    } else {
        None
    };
    Controller::new(regs, standard, fast).map(Arc::new)
}

fn hid_descriptor_register(node: aml::namespace::NodeId) -> u16 {
    let args = vec![
        Value::Buffer(DSM_UUID.to_vec()),
        Value::Integer(1),
        Value::Integer(1),
        Value::Package(Vec::new()),
    ];
    match aml::evaluate_child(node, "_DSM", args) {
        Some(Ok(v)) => v.as_integer().map_or(1, |r| r as u16),
        _ => 1,
    }
}

fn command(bus: &Controller, address: u16, ten_bit: bool, reg: u16, op: u8, arg: u8) -> bool {
    let [lo, hi] = reg.to_le_bytes();
    bus.transfer(address, ten_bit, &[lo, hi, arg, op], &mut [])
        .is_ok()
}

fn probe(
    node: aml::namespace::NodeId,
    controllers: &mut BTreeMap<String, Arc<Controller>>,
) -> Result<Device, &'static str> {
    let path = aml::path_of(node);
    let crs = aml::evaluate_child(node, "_CRS", Vec::new())
        .and_then(|r| r.ok())
        .and_then(|v| v.as_bytes())
        .ok_or("no _CRS")?;
    let (address, speed, ten_bit, controller) = aml::parse_resources(&crs)
        .into_iter()
        .find_map(|r| match r {
            Resource::I2c {
                address,
                speed,
                ten_bit,
                controller,
            } => Some((address, speed, ten_bit, controller)),
            _ => None,
        })
        .ok_or("no I2cSerialBus resource")?;
    let controller_node = aml::node_at(&controller)
        .or_else(|| Some(aml::parent_of(node)))
        .ok_or("controller not in namespace")?;
    let key = aml::path_of(controller_node);
    let bus = match controllers.get(&key) {
        Some(bus) => bus.clone(),
        None => {
            let bus = open_controller(controller_node, speed)?;
            controllers.insert(key.clone(), bus.clone());
            crate::log!("i2c: controller {} ready", key);
            bus
        }
    };
    if aml::evaluate_child(node, "_PS0", Vec::new()).is_some() {
        crate::sched::sleep_ms(1);
    }

    let reg = hid_descriptor_register(node);
    let mut desc = [0u8; 30];
    bus.transfer(address, ten_bit, &reg.to_le_bytes(), &mut desc)?;
    let field = |at: usize| u16::from_le_bytes([desc[at], desc[at + 1]]);
    if field(0) != 30 || field(2) != 0x0100 {
        crate::log!(
            "i2c-hid: {} bad HID descriptor (length {}, version {:#x})",
            path,
            field(0),
            field(2)
        );
        return Err("bad HID descriptor");
    }
    let report_len = field(4) as usize;
    let report_reg = field(6);
    let input_len = (field(10) as usize).clamp(4, 1024);
    let command_reg = field(16);
    let (vendor, product) = (field(20), field(22));

    if !command(&bus, address, ten_bit, command_reg, OP_SET_POWER, 0) {
        return Err("SET_POWER failed");
    }
    crate::sched::sleep_ms(60);
    if !command(&bus, address, ten_bit, command_reg, OP_RESET, 0) {
        return Err("RESET failed");
    }
    // Sıfırlama tamamlanınca aygıt sıfır uzunluklu bir rapor bırakır; okunup atılır.
    crate::sched::sleep_ms(100);
    let mut scratch = vec![0u8; input_len];
    for _ in 0..4 {
        if bus.transfer(address, ten_bit, &[], &mut scratch).is_err() {
            break;
        }
        if u16::from_le_bytes([scratch[0], scratch[1]]) == 0 {
            break;
        }
        crate::sched::sleep_ms(10);
    }

    let mut report = vec![0u8; report_len.clamp(1, 8192)];
    bus.transfer(address, ten_bit, &report_reg.to_le_bytes(), &mut report)?;
    let pointer = hid_report::pointer(&report).ok_or("not a pointing device")?;
    let (fields, _) = hid_report::input_fields(&report);
    let tip = fields
        .iter()
        .find(|f| f.page == 0x0d && f.usage == 0x42 && f.report_id == pointer.report_id)
        .copied();
    let (mode, kind) = match tip {
        Some(tip) if pointer.absolute() => (
            Mode::Touchpad {
                pointer,
                tip,
                last: None,
                rest: (0, 0),
            },
            "touchpad (absolute)",
        ),
        _ if pointer.absolute() => (Mode::Pointer(pointer), "tablet"),
        _ => (Mode::Pointer(pointer), "touchpad (mouse mode)"),
    };
    let hid = aml::hid(node).unwrap_or_default();
    crate::log!(
        "i2c-hid: {} '{}' {:04x}:{:04x} at {}:{:#x}, {} kHz, {}",
        path,
        hid,
        vendor,
        product,
        key,
        address,
        speed / 1000,
        kind
    );
    Ok(Device {
        name: path,
        bus,
        address,
        ten_bit,
        input_len,
        mode,
    })
}

fn i2chidd() -> i64 {
    let mut nodes = aml::find_devices("PNP0C50");
    for node in aml::find_devices("ACPI0C50") {
        if !nodes.contains(&node) {
            nodes.push(node);
        }
    }
    let mut controllers = BTreeMap::new();
    let mut devices: Vec<Device> = Vec::new();
    for node in nodes {
        match probe(node, &mut controllers) {
            Ok(d) => devices.push(d),
            Err(e) => crate::log!("i2c-hid: {}: {}", aml::path_of(node), e),
        }
    }
    if devices.is_empty() {
        return 0;
    }
    crate::log!(
        "i2c-hid: polling {} device(s): {}",
        devices.len(),
        devices
            .iter()
            .map(|d| d.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    );
    let mut buf = vec![0u8; devices.iter().map(|d| d.input_len).max().unwrap_or(4)];
    loop {
        for d in devices.iter_mut() {
            d.poll(&mut buf);
        }
        crate::sched::sleep_ms(POLL_MS);
    }
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let count = aml::find_devices("PNP0C50").len() + aml::find_devices("ACPI0C50").len();
    FOUND.store(count, Ordering::Relaxed);
    if count == 0 {
        let _ = write!(detail, "no devices");
        return Ok(());
    }
    // Aygıt açma (sıfırlama beklemeleri) açılışı geciktirmesin diye arka planda.
    crate::sched::spawn_detached("i2c-hid", i2chidd).ok_or("could not start i2c-hid")?;
    let _ = write!(detail, "{} device(s) in ACPI", count);
    Ok(())
}

crate::initcall!(driver, 28, "I2C HID (touchpad)", init);
