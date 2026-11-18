//! ACPI Machine Language yorumlayıcısı ve ad alanı.

pub mod interp;
pub mod namespace;
pub mod region;
pub mod value;

use alloc::collections::BTreeSet;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::drivers::{acpi, ec};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::sched::Mutex;
use interp::{AmlError, Interpreter};
pub use namespace::NodeId;
use namespace::{NamePath, Namespace, ROOT};
use value::Object;
pub use value::Value;

struct State {
    ns: Namespace,
    int_mask: u64,
}

static STATE: Mutex<Option<State>> = Mutex::new(None);
/// AML `Notify` çağrıları (düğüm, değer); `take_notifications` ile alınır.
static NOTIFICATIONS: crate::sync::SpinLock<Vec<(NodeId, u64)>> =
    crate::sync::SpinLock::new(Vec::new());

pub fn take_notifications() -> Vec<(NodeId, u64)> {
    core::mem::take(&mut *NOTIFICATIONS.lock())
}

const OSI_STRINGS: &[&str] = &[
    "Windows 2000",
    "Windows 2001",
    "Windows 2001 SP1",
    "Windows 2001.1",
    "Windows 2006",
    "Windows 2006.1",
    "Windows 2009",
    "Windows 2012",
    "Windows 2013",
    "Windows 2015",
    "Windows 2016",
    "Windows 2017",
    "Windows 2017.2",
    "Windows 2018",
    "Windows 2018.2",
    "Windows 2019",
    "Windows 2020",
    "Windows 2021",
    "Windows 2022",
    "Module Device",
    "Processor Device",
    "Processor Aggregator Device",
    "3.0 _SCP Extensions",
];

fn osi(args: &[Value]) -> Value {
    let supported = args
        .first()
        .and_then(|v| v.as_str())
        .is_some_and(|s| OSI_STRINGS.contains(&s));
    Value::Integer(if supported { u64::MAX } else { 0 })
}

pub fn eisa_id_to_string(id: u64) -> String {
    let id = (id as u32).swap_bytes();
    let c1 = (((id >> 26) & 0x1f) as u8 + b'@') as char;
    let c2 = (((id >> 21) & 0x1f) as u8 + b'@') as char;
    let c3 = (((id >> 16) & 0x1f) as u8 + b'@') as char;
    format!("{}{}{}{:04X}", c1, c2, c3, id & 0xffff)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Resource {
    Io {
        base: u16,
        length: u8,
    },
    Irq(u8),
    Memory {
        base: u64,
        length: u64,
    },
    /// I2cSerialBus: bağlı aygıtın adresi, hızı (Hz) ve denetleyicinin ACPI yolu.
    I2c {
        address: u16,
        speed: u32,
        ten_bit: bool,
        controller: String,
    },
    GpioInt,
}

pub fn parse_resources(bytes: &[u8]) -> Vec<Resource> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let tag = bytes[i];
        if tag & 0x80 == 0 {
            let kind = (tag >> 3) & 0x0f;
            let len = (tag & 0x07) as usize;
            let body = &bytes[(i + 1).min(bytes.len())..(i + 1 + len).min(bytes.len())];
            match kind {
                0x04 if body.len() >= 2 => {
                    let mask = u16::from_le_bytes([body[0], body[1]]);
                    for irq in 0..16 {
                        if mask & (1 << irq) != 0 {
                            out.push(Resource::Irq(irq));
                        }
                    }
                }
                0x08 if body.len() >= 7 => out.push(Resource::Io {
                    base: u16::from_le_bytes([body[1], body[2]]),
                    length: body[6],
                }),
                0x09 if body.len() >= 3 => out.push(Resource::Io {
                    base: u16::from_le_bytes([body[0], body[1]]) & 0x3ff,
                    length: body[2],
                }),
                0x0f => break,
                _ => {}
            }
            i += 1 + len;
        } else {
            if i + 3 > bytes.len() {
                break;
            }
            let len = u16::from_le_bytes([bytes[i + 1], bytes[i + 2]]) as usize;
            let body = &bytes[i + 3..(i + 3 + len).min(bytes.len())];
            match tag {
                0x86 if body.len() >= 9 => out.push(Resource::Memory {
                    base: u32::from_le_bytes(body[1..5].try_into().unwrap()) as u64,
                    length: u32::from_le_bytes(body[5..9].try_into().unwrap()) as u64,
                }),
                0x85 if body.len() >= 17 => out.push(Resource::Memory {
                    base: u32::from_le_bytes(body[1..5].try_into().unwrap()) as u64,
                    length: u32::from_le_bytes(body[13..17].try_into().unwrap()) as u64,
                }),
                0x87 if body.len() >= 23 && body[0] == 0 => out.push(Resource::Memory {
                    base: u32::from_le_bytes(body[7..11].try_into().unwrap()) as u64,
                    length: u32::from_le_bytes(body[19..23].try_into().unwrap()) as u64,
                }),
                0x8a if body.len() >= 43 && body[0] == 0 => out.push(Resource::Memory {
                    base: u64::from_le_bytes(body[11..19].try_into().unwrap()),
                    length: u64::from_le_bytes(body[35..43].try_into().unwrap()),
                }),
                0x8c => out.push(Resource::GpioInt),
                0x8e if body.len() >= 15 && body[2] == 1 => {
                    let type_len = u16::from_le_bytes([body[7], body[8]]) as usize;
                    let source = body.get(9 + type_len..).unwrap_or(&[]);
                    let end = source.iter().position(|&b| b == 0).unwrap_or(source.len());
                    out.push(Resource::I2c {
                        address: u16::from_le_bytes([body[13], body[14]]),
                        speed: u32::from_le_bytes(body[9..13].try_into().unwrap()),
                        ten_bit: body[4] & 1 != 0,
                        controller: String::from_utf8_lossy(&source[..end]).into_owned(),
                    });
                }
                0x89 if body.len() >= 6 => {
                    let count = body[1] as usize;
                    for n in 0..count {
                        if let Some(irq) = body.get(2 + n * 4..6 + n * 4) {
                            out.push(Resource::Irq(
                                u32::from_le_bytes(irq.try_into().unwrap()) as u8
                            ));
                        }
                    }
                }
                _ => {}
            }
            i += 3 + len;
        }
    }
    out
}

fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    let mut guard = STATE.lock();
    guard.as_mut().map(f)
}

fn run<R>(f: impl FnOnce(&mut Interpreter) -> R) -> Option<R> {
    with_state(|state| {
        let mut interp = Interpreter::new(&mut state.ns, state.int_mask);
        f(&mut interp)
    })
}

fn child_value(
    interp: &mut Interpreter,
    node: NodeId,
    name: &str,
) -> Option<Result<Value, AmlError>> {
    let path = NamePath::parse(name)?;
    let child = interp.ns.child(node, path.segments[0])?;
    Some(interp.invoke(child, Vec::new()))
}

fn hid_of(interp: &mut Interpreter, node: NodeId) -> Option<String> {
    match child_value(interp, node, "_HID")?.ok()? {
        Value::Integer(id) => Some(eisa_id_to_string(id)),
        Value::String(s) => Some(s),
        _ => None,
    }
}

fn status_of(interp: &mut Interpreter, node: NodeId) -> u64 {
    match child_value(interp, node, "_STA") {
        Some(Ok(value)) => value.as_integer().unwrap_or(0),
        Some(Err(_)) => 0,
        None => 0x0f,
    }
}

fn devices(interp: &Interpreter) -> Vec<NodeId> {
    let mut out = Vec::new();
    interp.ns.walk(|id, node| {
        if matches!(node.object, Object::Device) {
            out.push(id);
        }
    });
    out
}

/// _HID (veya _CID) değeri verilen kimlikle eşleşen ve mevcut olan aygıtlar.
pub fn find_devices(hid: &str) -> Vec<NodeId> {
    run(|interp| {
        devices(interp)
            .into_iter()
            .filter(|&node| {
                let id_matches = |v: &Value| match v {
                    Value::Integer(id) => eisa_id_to_string(*id) == hid,
                    Value::String(s) => s == hid,
                    _ => false,
                };
                let matches = hid_of(interp, node).is_some_and(|h| h == hid)
                    || match child_value(interp, node, "_CID") {
                        Some(Ok(Value::Package(ids))) => ids.iter().any(id_matches),
                        Some(Ok(v)) => id_matches(&v),
                        _ => false,
                    };
                matches && status_of(interp, node) & 1 != 0
            })
            .collect()
    })
    .unwrap_or_default()
}

pub fn evaluate(path: &str, args: Vec<Value>) -> Result<Value, AmlError> {
    let parsed = NamePath::parse(path).ok_or(AmlError::Type("invalid path"))?;
    run(|interp| {
        let node = interp
            .ns
            .resolve(ROOT, &parsed)
            .ok_or_else(|| AmlError::NameNotFound(String::from(path)))?;
        interp.invoke(node, args)
    })
    .unwrap_or(Err(AmlError::Type("AML not initialised")))
}

pub fn evaluate_child(
    node: NodeId,
    name: &str,
    args: Vec<Value>,
) -> Option<Result<Value, AmlError>> {
    let path = NamePath::parse(name)?;
    run(|interp| {
        let child = interp.ns.child(node, path.segments[0])?;
        Some(interp.invoke(child, args))
    })
    .flatten()
}

/// `name` adlı nesneyi taşıyan düğümler (ör. "_BCM" → parlaklığı ayarlanabilen ekranlar).
pub fn find_parents_with(name: &str) -> Vec<NodeId> {
    let mut seg = [b'_'; 4];
    for (i, b) in name.bytes().take(4).enumerate() {
        seg[i] = b;
    }
    run(|interp| {
        let mut out = Vec::new();
        interp.ns.walk(|_, node| {
            if node.name == seg && !out.contains(&node.parent) {
                out.push(node.parent);
            }
        });
        out
    })
    .unwrap_or_default()
}

/// PCI kesme yönlendirme girdisi (_PRT): aygıt numarası, INTx pini (0 = INTA) ve
/// ya doğrudan GSI ya da bağlantı aygıtı (PNP0C0F).
pub struct PciRoute {
    pub slot: u16,
    pub pin: u8,
    pub link: Option<NodeId>,
    pub index: u32,
}

pub fn pci_routes(node: NodeId) -> Vec<PciRoute> {
    let Some(Ok(Value::Package(entries))) = evaluate_child(node, "_PRT", Vec::new()) else {
        return Vec::new();
    };
    entries
        .iter()
        .filter_map(|entry| {
            let Value::Package(f) = entry else {
                return None;
            };
            let link = match f.get(2)? {
                Value::Node(n) => Some(*n),
                Value::Reference(t) => match t.as_ref() {
                    value::Target::Node(n) => Some(*n),
                    _ => None,
                },
                Value::String(path) => Some(resolve_from(node, path)?),
                _ => None,
            };
            Some(PciRoute {
                slot: (f.first()?.as_integer()? >> 16) as u16,
                pin: f.get(1)?.as_integer()? as u8,
                link,
                index: f.get(3)?.as_integer()? as u32,
            })
        })
        .collect()
}

/// Adı `scope`'tan başlayarak (tek segmentli adlarda yukarı doğru) çözer.
pub fn resolve_from(scope: NodeId, path: &str) -> Option<NodeId> {
    let parsed = NamePath::parse(path)?;
    run(|interp| interp.ns.resolve(scope, &parsed)).flatten()
}

/// Kaynak şablonundaki kesmeler: (GSI, düzey tetiklemeli, aktif düşük).
pub fn parse_interrupts(bytes: &[u8]) -> Vec<(u32, bool, bool)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let tag = bytes[i];
        if tag & 0x80 == 0 {
            let len = (tag & 0x07) as usize;
            let body = bytes.get(i + 1..i + 1 + len).unwrap_or(&[]);
            match (tag >> 3) & 0x0f {
                0x04 if body.len() >= 2 => {
                    let mask = u16::from_le_bytes([body[0], body[1]]);
                    let flags = body.get(2).copied().unwrap_or(1);
                    for irq in (0..16).filter(|irq| mask & (1 << irq) != 0) {
                        out.push((irq, flags & 1 == 0, flags & 8 != 0));
                    }
                }
                0x0f => break,
                _ => {}
            }
            i += 1 + len;
        } else {
            let Some(len) = bytes.get(i + 1..i + 3) else {
                break;
            };
            let len = u16::from_le_bytes([len[0], len[1]]) as usize;
            let body = bytes.get(i + 3..i + 3 + len).unwrap_or(&[]);
            if tag == 0x89 && body.len() >= 6 {
                let flags = body[0];
                for n in 0..body[1] as usize {
                    if let Some(gsi) = body.get(2 + n * 4..6 + n * 4) {
                        out.push((
                            u32::from_le_bytes(gsi.try_into().unwrap()),
                            flags & 2 == 0,
                            flags & 4 != 0,
                        ));
                    }
                }
            }
            i += 3 + len;
        }
    }
    out
}

/// Bağlantı aygıtının (PNP0C0F) şu anki kesmesi.
pub fn link_interrupt(node: NodeId) -> Option<(u32, bool, bool)> {
    match evaluate_child(node, "_CRS", Vec::new())?.ok()? {
        Value::Buffer(bytes) => parse_interrupts(&bytes).into_iter().find(|i| i.0 != 0),
        _ => None,
    }
}

/// `parent` altında _ADR değeri `adr` olan aygıt (PCI köprüsünü bulmak için).
pub fn child_with_adr(parent: NodeId, adr: u64) -> Option<NodeId> {
    let children: Vec<NodeId> = run(|interp| {
        interp
            .ns
            .node(parent)
            .children
            .values()
            .copied()
            .filter(|&c| matches!(interp.ns.object(c), Object::Device))
            .collect()
    })?;
    children.into_iter().find(|&c| {
        evaluate_child(c, "_ADR", Vec::new())
            .and_then(|r| r.ok())
            .and_then(|v| v.as_integer())
            == Some(adr)
    })
}

pub fn exists(path: &str) -> bool {
    let Some(parsed) = NamePath::parse(path) else {
        return false;
    };
    run(|interp| interp.ns.resolve(ROOT, &parsed).is_some()).unwrap_or(false)
}

/// Düğümün ebeveyni (kök için kendisi).
pub fn parent_of(node: NodeId) -> NodeId {
    run(|interp| interp.ns.node(node).parent).unwrap_or(node)
}

/// Yolu verilen düğüm.
pub fn node_at(path: &str) -> Option<NodeId> {
    let parsed = NamePath::parse(path)?;
    run(|interp| interp.ns.resolve(ROOT, &parsed)).flatten()
}

/// Aygıtın _HID değeri (EISA kimliği metne çevrilmiş).
pub fn hid(node: NodeId) -> Option<String> {
    run(|interp| hid_of(interp, node)).flatten()
}

pub fn path_of(node: NodeId) -> String {
    run(|interp| interp.ns.path_of(node)).unwrap_or_default()
}

pub fn device_status(node: NodeId) -> u64 {
    run(|interp| status_of(interp, node)).unwrap_or(0)
}

pub fn object_count() -> usize {
    run(|interp| interp.ns.len()).unwrap_or(0)
}

/// \_Sx paketinden SLP_TYPa/SLP_TYPb.
pub fn sleep_type(state: u8) -> Option<(u8, u8)> {
    let value = evaluate(&format!("\\_S{}_", state), Vec::new()).ok()?;
    let items = value.as_package()?;
    let a = items.first()?.as_integer()? as u8;
    let b = items
        .get(1)
        .and_then(|v| v.as_integer())
        .unwrap_or(a as u64) as u8;
    Some((a, b))
}

pub fn prepare_to_sleep(state: u8) {
    if exists("\\_PTS") {
        let _ = evaluate("\\_PTS", alloc::vec![Value::Integer(state as u64)]);
    }
}

fn setup_embedded_controller(interp: &mut Interpreter) -> Option<NodeId> {
    let ec_node = devices(interp)
        .into_iter()
        .find(|&node| hid_of(interp, node).as_deref() == Some("PNP0C09"))?;

    if !ec::present() {
        let crs = child_value(interp, ec_node, "_CRS")?.ok()?;
        let resources = parse_resources(&crs.as_bytes()?);
        let ports: Vec<u16> = resources
            .iter()
            .filter_map(|r| match r {
                Resource::Io { base, .. } => Some(*base),
                _ => None,
            })
            .collect();
        if ports.len() >= 2 {
            ec::configure(ports[0], ports[1]);
        }
    }
    Some(ec_node)
}

fn run_reg_methods(interp: &mut Interpreter) {
    let mut regs = Vec::new();
    interp.ns.walk(|id, node| {
        if &node.name == b"_REG" {
            regs.push((id, node.parent));
        }
    });
    for (reg, scope) in regs {
        let mut spaces = BTreeSet::new();
        for &child in interp.ns.node(scope).children.values() {
            if let Object::Region(region) = interp.ns.object(child) {
                spaces.insert(region.space.id());
            }
        }
        for space in spaces {
            if space == 3 && !ec::present() {
                continue;
            }
            if let Err(error) = interp.invoke(
                reg,
                alloc::vec![Value::Integer(space as u64), Value::Integer(1)],
            ) {
                crate::log!(
                    "aml: {}({}) failed: {:?}",
                    interp.ns.path_of(reg),
                    space,
                    error
                );
            }
        }
    }
}

fn initialise_devices(interp: &mut Interpreter) -> (usize, usize) {
    if let Some(sb_ini) = interp
        .ns
        .resolve(ROOT, &NamePath::parse("\\_SB_._INI").unwrap())
    {
        let _ = interp.invoke(sb_ini, Vec::new());
    }
    let mut present = 0;
    let mut initialised = 0;
    let mut stack = alloc::vec![ROOT];
    while let Some(scope) = stack.pop() {
        let children: Vec<NodeId> = interp.ns.node(scope).children.values().copied().collect();
        for child in children {
            match interp.ns.object(child) {
                Object::Device | Object::Processor | Object::ThermalZone => {
                    let status = status_of(interp, child);
                    if status & 1 != 0 {
                        present += 1;
                        if let Some(result) = child_value(interp, child, "_INI") {
                            match result {
                                Ok(_) => initialised += 1,
                                Err(error) => {
                                    crate::log!(
                                        "aml: {}._INI failed: {:?}",
                                        interp.ns.path_of(child),
                                        error
                                    )
                                }
                            }
                        }
                        stack.push(child);
                    } else if status & 8 != 0 {
                        stack.push(child);
                    }
                }
                Object::Scope => stack.push(child),
                _ => {}
            }
        }
    }
    (present, initialised)
}

/// Mevcut aygıtların salt-okunur tanı metodlarını çalıştırır; yorumlayıcı için gerçek AML sınaması.
fn probe_methods(interp: &mut Interpreter) -> (usize, usize) {
    let mut evaluated = 0;
    let mut failed = 0;
    for device in devices(interp) {
        if status_of(interp, device) & 1 == 0 {
            continue;
        }
        for name in ["_HID", "_UID", "_ADR", "_CRS", "_PRT", "_STA"] {
            if let Some(result) = child_value(interp, device, name) {
                evaluated += 1;
                if let Err(error) = result {
                    failed += 1;
                    crate::log!(
                        "aml: {}.{} failed: {:?}",
                        interp.ns.path_of(device),
                        name,
                        error
                    );
                }
            }
        }
    }
    (evaluated, failed)
}

fn self_test(interp: &mut Interpreter) -> bool {
    let ok_eisa = eisa_id_to_string(0x0a0c_d041) == "PNP0C0A";
    let ok_res = parse_resources(&[0x47, 0x01, 0x62, 0x00, 0x62, 0x00, 0x00, 0x01, 0x79, 0x00])
        == alloc::vec![Resource::Io {
            base: 0x62,
            length: 1
        }];
    // I2cSerialBusV2(0x15, 400 kHz, "\\_SB.I2CX") + bitiş etiketi.
    let ok_i2c = parse_resources(&[
        0x8e, 0x19, 0x00, 0x02, 0x00, 0x01, 0x02, 0x00, 0x00, 0x01, 0x06, 0x00, 0x80, 0x1a, 0x06,
        0x00, 0x15, 0x00, 0x5c, 0x5f, 0x53, 0x42, 0x2e, 0x49, 0x32, 0x43, 0x58, 0x00, 0x79, 0x00,
    ]) == alloc::vec![Resource::I2c {
        address: 0x15,
        speed: 400_000,
        ten_bit: false,
        controller: String::from("\\_SB.I2CX"),
    }];
    let osi_ok = interp
        .ns
        .resolve(ROOT, &NamePath::parse("\\_OSI").unwrap())
        .and_then(|node| {
            interp
                .invoke(
                    node,
                    alloc::vec![Value::String(String::from("Windows 2015"))],
                )
                .ok()
        })
        .and_then(|v| v.as_integer())
        .is_some_and(|v| v != 0);
    ok_eisa && ok_res && ok_i2c && osi_ok
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    let blocks = acpi::aml_tables();
    if blocks.is_empty() {
        return Err("no DSDT/SSDT");
    }
    let int_mask = if blocks[0].0 >= 2 {
        u64::MAX
    } else {
        u32::MAX as u64
    };

    let mut ns = Namespace::new();
    ns.add(ROOT, *b"_OSI", Object::NativeMethod { args: 1, func: osi });

    let mut loaded = 0;
    {
        let mut interp = Interpreter::new(&mut ns, int_mask);
        for (revision, signature, code) in &blocks {
            match interp.load_table(code) {
                Ok(()) => loaded += 1,
                Err(error) => crate::log!(
                    "aml: {} rev {} failed to load: {:?}",
                    signature,
                    revision,
                    error
                ),
            }
        }
        check("aml", self_test(&mut interp))?;
    }
    let objects = ns.len();
    *STATE.lock() = Some(State { ns, int_mask });

    let ec_node = run(setup_embedded_controller).flatten();
    run(run_reg_methods);
    // _PIC(1): firmware _PRT'de 8259 bağlantıları yerine I/O APIC GSI'lerini verir.
    if exists("\\_PIC") {
        let mode = crate::arch::apic::active() as u64;
        let _ = evaluate("\\_PIC", alloc::vec![Value::Integer(mode)]);
    }
    let (present, initialised) = run(initialise_devices).unwrap_or((0, 0));
    let (evaluated, failed) = run(probe_methods).unwrap_or((0, 0));
    crate::log!(
        "aml: probe evaluated {} methods, {} failed",
        evaluated,
        failed
    );

    crate::log!(
        "aml: {} tables, {} objects, {} present devices, {} _INI ran, EC {}",
        loaded,
        objects,
        present,
        initialised,
        if ec_node.is_some() { "found" } else { "none" }
    );
    let _ = write!(detail, "{} objects, {} devices", objects, present);
    Ok(())
}

crate::initcall!(driver, 05, "ACPI AML interpreter", init);
