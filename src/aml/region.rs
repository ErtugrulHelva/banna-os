//! OperationRegion erişimi ve alan (field) bit işlemleri.

use alloc::vec;
use alloc::vec::Vec;

use super::interp::{AmlError, Interpreter, Result};
use super::namespace::{NamePath, NodeId};
use super::value::{FieldKind, FieldUnit, Object, OpRegion, RegionSpace, Value};
use crate::arch::cpu::{inb, inl, inw, outb, outl, outw};
use crate::drivers::{ec, pci};
use crate::memory::paging::{NO_CACHE, map_physical};

pub fn extract_bits(bytes: &[u8], bit_offset: u64, bit_length: u64) -> Value {
    let out_len = bit_length.div_ceil(8) as usize;
    let mut out = vec![0u8; out_len.max(1)];
    for i in 0..bit_length {
        let src = bit_offset + i;
        let byte = bytes.get((src / 8) as usize).copied().unwrap_or(0);
        if byte & (1 << (src % 8)) != 0 {
            out[(i / 8) as usize] |= 1 << (i % 8);
        }
    }
    if bit_length <= 64 {
        let mut word = [0u8; 8];
        word[..out.len().min(8)].copy_from_slice(&out[..out.len().min(8)]);
        Value::Integer(u64::from_le_bytes(word))
    } else {
        Value::Buffer(out)
    }
}

pub fn insert_bits(bytes: &mut Vec<u8>, bit_offset: u64, bit_length: u64, data: &[u8]) {
    let needed = (bit_offset + bit_length).div_ceil(8) as usize;
    if bytes.len() < needed {
        bytes.resize(needed, 0);
    }
    for i in 0..bit_length {
        let bit = data
            .get((i / 8) as usize)
            .is_some_and(|b| b & (1 << (i % 8)) != 0);
        let dst = bit_offset + i;
        let byte = &mut bytes[(dst / 8) as usize];
        if bit {
            *byte |= 1 << (dst % 8);
        } else {
            *byte &= !(1 << (dst % 8));
        }
    }
}

fn region_of(interp: &Interpreter, node: NodeId) -> Result<OpRegion> {
    match interp.ns.object(node) {
        Object::Region(region) => Ok(region.clone()),
        _ => Err(AmlError::Type("field does not refer to an OperationRegion")),
    }
}

fn eval_in_scope(interp: &mut Interpreter, scope: NodeId, name: &str) -> Option<u64> {
    let path = NamePath::parse(name)?;
    let node = interp.ns.child(scope, path.segments[0])?;
    interp.invoke(node, Vec::new()).ok()?.as_integer()
}

/// PCI_Config bölgesinin ait olduğu aygıtın bus/aygıt/fonksiyon adresi.
fn pci_address(interp: &mut Interpreter, region_node: NodeId) -> (u8, u8, u8) {
    let mut device = interp.ns.node(region_node).parent;
    let adr = loop {
        if let Some(adr) = eval_in_scope(interp, device, "_ADR") {
            break adr;
        }
        if device == 0 {
            break 0;
        }
        device = interp.ns.node(device).parent;
    };
    let mut bus = 0u64;
    let mut node = device;
    while node != 0 {
        if let Some(bbn) = eval_in_scope(interp, node, "_BBN") {
            bus = bbn;
            break;
        }
        node = interp.ns.node(node).parent;
    }
    (bus as u8, (adr >> 16) as u8, adr as u8)
}

fn region_access(
    interp: &mut Interpreter,
    region_node: NodeId,
    offset: u64,
    width: u8,
    write: Option<u64>,
) -> Result<u64> {
    let mut region = region_of(interp, region_node)?;
    if region.space == RegionSpace::PciConfig && region.pci.is_none() {
        let address = pci_address(interp, region_node);
        region.pci = Some(address);
        interp.ns.node_mut(region_node).object = Object::Region(region.clone());
    }
    if region.length != 0 && offset + width as u64 > region.length {
        return Err(AmlError::Type("field access beyond OperationRegion"));
    }
    let address = region.base + offset;
    Ok(match region.space {
        RegionSpace::SystemMemory => {
            let ptr = map_physical(address, width as u64, NO_CACHE)
                .map_err(|_| AmlError::Type("unmappable region"))?;
            unsafe {
                match (width, write) {
                    (1, Some(v)) => ptr.write_volatile(v as u8),
                    (2, Some(v)) => (ptr as *mut u16).write_volatile(v as u16),
                    (4, Some(v)) => (ptr as *mut u32).write_volatile(v as u32),
                    (_, Some(v)) => (ptr as *mut u64).write_volatile(v),
                    _ => {}
                }
                match width {
                    1 => ptr.read_volatile() as u64,
                    2 => (ptr as *const u16).read_volatile() as u64,
                    4 => (ptr as *const u32).read_volatile() as u64,
                    _ => (ptr as *const u64).read_volatile(),
                }
            }
        }
        RegionSpace::SystemIo => unsafe {
            let port = address as u16;
            match (width, write) {
                (1, Some(v)) => outb(port, v as u8),
                (2, Some(v)) => outw(port, v as u16),
                (_, Some(v)) => outl(port, v as u32),
                (1, None) => return Ok(inb(port) as u64),
                (2, None) => return Ok(inw(port) as u64),
                (_, None) => return Ok(inl(port) as u64),
            }
            0
        },
        RegionSpace::PciConfig => {
            let (bus, dev, func) = region.pci.unwrap_or((0, 0, 0));
            let aligned = (address & !3) as u8;
            let shift = (address & 3) * 8;
            let mask = if width >= 4 {
                u32::MAX
            } else {
                (1u32 << (width as u32 * 8)) - 1
            };
            let current = pci::read_config(bus, dev, func, aligned);
            if let Some(v) = write {
                let merged = (current & !(mask << shift)) | (((v as u32) & mask) << shift);
                pci::write_config(bus, dev, func, aligned, merged);
                0
            } else {
                ((current >> shift) & mask) as u64
            }
        }
        RegionSpace::EmbeddedControl => {
            let mut result = 0u64;
            for i in 0..width as u64 {
                let reg = (address + i) as u8;
                match write {
                    Some(v) => {
                        ec::write(reg, (v >> (8 * i)) as u8).map_err(|_| AmlError::Timeout)?
                    }
                    None => {
                        result |= (ec::read(reg).map_err(|_| AmlError::Timeout)? as u64) << (8 * i);
                    }
                }
            }
            result
        }
        RegionSpace::Cmos => unsafe {
            outb(0x70, address as u8);
            match write {
                Some(v) => {
                    outb(0x71, v as u8);
                    0
                }
                None => inb(0x71) as u64,
            }
        },
        RegionSpace::SmBus | RegionSpace::Other(_) => {
            crate::log!("aml: unsupported region space {} access", region.space.id());
            0
        }
    })
}

fn unit_access(
    interp: &mut Interpreter,
    field: &FieldUnit,
    byte_offset: u64,
    write: Option<u64>,
) -> Result<u64> {
    let width = field.access_bytes;
    match &field.kind {
        FieldKind::Region(region) => region_access(interp, *region, byte_offset, width, write),
        FieldKind::Bank {
            region,
            bank,
            value,
        } => {
            let bank_field = match interp.ns.object(*bank) {
                Object::Field(f) => f.clone(),
                _ => return Err(AmlError::Type("bank selector is not a field")),
            };
            write_field(interp, &bank_field, &Value::Integer(*value))?;
            region_access(interp, *region, byte_offset, width, write)
        }
        FieldKind::Index { index, data } => {
            let (index_field, data_field) =
                match (interp.ns.object(*index), interp.ns.object(*data)) {
                    (Object::Field(i), Object::Field(d)) => (i.clone(), d.clone()),
                    _ => return Err(AmlError::Type("IndexField parts are not fields")),
                };
            write_field(interp, &index_field, &Value::Integer(byte_offset))?;
            match write {
                Some(v) => {
                    write_field(interp, &data_field, &Value::Integer(v))?;
                    Ok(0)
                }
                None => Ok(read_field(interp, &data_field)?.as_integer().unwrap_or(0)),
            }
        }
    }
}

fn unit_range(field: &FieldUnit) -> (u64, u64, u64) {
    let unit_bits = field.access_bytes as u64 * 8;
    let first = field.bit_offset / unit_bits;
    let last = (field.bit_offset + field.bit_length.max(1) - 1) / unit_bits;
    (first, last, unit_bits)
}

pub fn read_field(interp: &mut Interpreter, field: &FieldUnit) -> Result<Value> {
    let (first, last, unit_bits) = unit_range(field);
    let unit_bytes = field.access_bytes as usize;
    let mut raw = Vec::with_capacity(((last - first + 1) as usize) * unit_bytes);
    for unit in first..=last {
        let value = unit_access(interp, field, unit * unit_bytes as u64, None)?;
        raw.extend_from_slice(&value.to_le_bytes()[..unit_bytes]);
    }
    let bit_offset = field.bit_offset - first * unit_bits;
    Ok(extract_bits(&raw, bit_offset, field.bit_length))
}

pub fn write_field(interp: &mut Interpreter, field: &FieldUnit, value: &Value) -> Result<()> {
    let (first, last, unit_bits) = unit_range(field);
    let unit_bytes = field.access_bytes as usize;
    let data = match value {
        Value::Integer(v) => v.to_le_bytes().to_vec(),
        other => other.as_bytes().unwrap_or_default(),
    };
    let bit_offset = field.bit_offset - first * unit_bits;
    let whole_units = bit_offset == 0 && field.bit_length.is_multiple_of(unit_bits);

    let mut raw = Vec::with_capacity(((last - first + 1) as usize) * unit_bytes);
    for unit in first..=last {
        let initial = if whole_units {
            0
        } else {
            match field.update_rule {
                1 => u64::MAX,
                2 => 0,
                _ => unit_access(interp, field, unit * unit_bytes as u64, None)?,
            }
        };
        raw.extend_from_slice(&initial.to_le_bytes()[..unit_bytes]);
    }
    insert_bits(&mut raw, bit_offset, field.bit_length, &data);
    for (i, unit) in (first..=last).enumerate() {
        let mut word = [0u8; 8];
        word[..unit_bytes].copy_from_slice(&raw[i * unit_bytes..(i + 1) * unit_bytes]);
        unit_access(
            interp,
            field,
            unit * unit_bytes as u64,
            Some(u64::from_le_bytes(word)),
        )?;
    }
    Ok(())
}
