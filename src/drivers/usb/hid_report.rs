//! HID rapor tanımlayıcısı ayrıştırıcısı: işaretçi aygıtları için X/Y, tekerlek ve
//! düğme alanlarının rapordaki konumlarını çıkarır (rapor kimlikleri desteklenir).

use alloc::vec::Vec;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Field {
    pub report_id: u8,
    pub bit_offset: u32,
    pub bit_size: u32,
    pub page: u16,
    pub usage: u16,
    pub logical_min: i32,
    pub logical_max: i32,
    pub relative: bool,
}

impl Field {
    /// Rapordan alanın değerini (işaretli alanlarda işaret genişletilerek) okur.
    pub fn read(&self, report: &[u8]) -> Option<i32> {
        if self.bit_size == 0 || self.bit_size > 32 {
            return None;
        }
        let end = self.bit_offset + self.bit_size;
        if end.div_ceil(8) as usize > report.len() {
            return None;
        }
        let mut value: u64 = 0;
        for i in 0..self.bit_size {
            let bit = self.bit_offset + i;
            if report[(bit / 8) as usize] >> (bit % 8) & 1 != 0 {
                value |= 1 << i;
            }
        }
        if self.logical_min < 0 && self.bit_size < 32 && value >> (self.bit_size - 1) & 1 != 0 {
            value |= !0u64 << self.bit_size;
        }
        Some(value as i64 as i32)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Pointer {
    pub report_id: u8,
    pub x: Option<Field>,
    pub y: Option<Field>,
    pub wheel: Option<Field>,
    pub buttons: Vec<Field>,
    pub uses_report_ids: bool,
}

impl Pointer {
    pub fn absolute(&self) -> bool {
        self.x.is_some_and(|f| !f.relative)
    }
}

#[derive(Clone, Copy, Default)]
struct Globals {
    page: u16,
    logical_min: i32,
    logical_max: i32,
    report_size: u32,
    report_count: u32,
    report_id: u8,
}

fn item_value(data: &[u8], signed: bool) -> i32 {
    match data.len() {
        1 if signed => data[0] as i8 as i32,
        1 => data[0] as i32,
        2 if signed => i16::from_le_bytes([data[0], data[1]]) as i32,
        2 => u16::from_le_bytes([data[0], data[1]]) as i32,
        4 => i32::from_le_bytes([data[0], data[1], data[2], data[3]]),
        _ => 0,
    }
}

/// Tanımlayıcıdaki tüm giriş (Input) alanlarını çıkarır.
pub fn input_fields(desc: &[u8]) -> (Vec<Field>, bool) {
    let mut fields = Vec::new();
    let mut g = Globals::default();
    let mut stack: Vec<Globals> = Vec::new();
    let mut usages: Vec<(u16, u16)> = Vec::new();
    let mut usage_min: Option<(u16, u16)> = None;
    let mut usage_max: Option<u16> = None;
    let mut offsets: [u32; 256] = [0; 256];
    let mut uses_ids = false;
    let mut i = 0;
    while i < desc.len() {
        let prefix = desc[i];
        if prefix == 0xfe {
            let len = desc.get(i + 1).copied().unwrap_or(0) as usize;
            i += 3 + len;
            continue;
        }
        let size = match prefix & 3 {
            3 => 4,
            n => n as usize,
        };
        let kind = (prefix >> 2) & 3;
        let tag = prefix >> 4;
        let Some(data) = desc.get(i + 1..i + 1 + size) else {
            break;
        };
        i += 1 + size;
        match (kind, tag) {
            // Global
            (1, 0) => g.page = item_value(data, false) as u16,
            (1, 1) => g.logical_min = item_value(data, true),
            (1, 2) => {
                g.logical_max = if g.logical_min >= 0 {
                    item_value(data, false)
                } else {
                    item_value(data, true)
                }
            }
            (1, 7) => g.report_size = item_value(data, false) as u32,
            (1, 8) => {
                g.report_id = item_value(data, false) as u8;
                uses_ids = true;
            }
            (1, 9) => g.report_count = item_value(data, false) as u32,
            (1, 10) => stack.push(g),
            (1, 11) => g = stack.pop().unwrap_or(g),
            // Local
            (2, 0) => {
                let v = item_value(data, false) as u32;
                let page = if size == 4 { (v >> 16) as u16 } else { g.page };
                usages.push((page, v as u16));
            }
            (2, 1) => {
                let v = item_value(data, false) as u32;
                let page = if size == 4 { (v >> 16) as u16 } else { g.page };
                usage_min = Some((page, v as u16));
            }
            (2, 2) => usage_max = Some(item_value(data, false) as u16),
            // Main
            (0, 8) => {
                let flags = item_value(data, false) as u32;
                let constant = flags & 1 != 0;
                let variable = flags & 2 != 0;
                let relative = flags & 4 != 0;
                let id = g.report_id as usize;
                for n in 0..g.report_count {
                    let usage = if let Some((page, min)) = usage_min {
                        let max = usage_max.unwrap_or(min);
                        Some((page, (min as u32 + n).min(max as u32) as u16))
                    } else {
                        usages.get(n as usize).or(usages.last()).copied()
                    };
                    if !constant
                        && variable
                        && let Some((page, usage)) = usage
                    {
                        fields.push(Field {
                            report_id: g.report_id,
                            bit_offset: offsets[id],
                            bit_size: g.report_size,
                            page,
                            usage,
                            logical_min: g.logical_min,
                            logical_max: g.logical_max,
                            relative,
                        });
                    }
                    offsets[id] += g.report_size;
                }
                usages.clear();
                usage_min = None;
                usage_max = None;
            }
            (0, 9) | (0, 11) | (0, 10) | (0, 12) => {
                usages.clear();
                usage_min = None;
                usage_max = None;
            }
            _ => {}
        }
    }
    (fields, uses_ids)
}

/// Fare/tablet alanlarını bulur; X ve Y yoksa `None`.
pub fn pointer(desc: &[u8]) -> Option<Pointer> {
    let (fields, uses_ids) = input_fields(desc);
    let find = |page: u16, usage: u16| {
        fields
            .iter()
            .find(|f| f.page == page && f.usage == usage)
            .copied()
    };
    // Dokunmatik yüzeyler hem göreli fare hem mutlak parmak raporu sunar: fare yeğlenir.
    let x = fields
        .iter()
        .find(|f| f.page == 0x01 && f.usage == 0x30 && f.relative)
        .copied()
        .or_else(|| find(0x01, 0x30))?;
    let y = fields
        .iter()
        .find(|f| f.page == 0x01 && f.usage == 0x31 && f.report_id == x.report_id)
        .copied()?;
    let wheel = find(0x01, 0x38).filter(|f| f.report_id == x.report_id);
    let buttons = fields
        .iter()
        .filter(|f| f.page == 0x09 && f.report_id == x.report_id && (1..=5).contains(&f.usage))
        .copied()
        .collect();
    Some(Pointer {
        report_id: x.report_id,
        x: Some(x),
        y: Some(y),
        wheel,
        buttons,
        uses_report_ids: uses_ids,
    })
}

pub fn self_test() -> bool {
    // QEMU usb-tablet tanımlayıcısı: 3 düğme, mutlak X/Y (0..32767), tekerlek.
    let tablet: &[u8] = &[
        0x05, 0x01, 0x09, 0x02, 0xa1, 0x01, 0x09, 0x01, 0xa1, 0x00, 0x05, 0x09, 0x19, 0x01, 0x29,
        0x03, 0x15, 0x00, 0x25, 0x01, 0x95, 0x03, 0x75, 0x01, 0x81, 0x02, 0x95, 0x01, 0x75, 0x05,
        0x81, 0x01, 0x05, 0x01, 0x09, 0x30, 0x09, 0x31, 0x15, 0x00, 0x26, 0xff, 0x7f, 0x35, 0x00,
        0x46, 0xff, 0x7f, 0x75, 0x10, 0x95, 0x02, 0x81, 0x02, 0x05, 0x01, 0x09, 0x38, 0x15, 0x81,
        0x25, 0x7f, 0x35, 0x00, 0x45, 0x00, 0x75, 0x08, 0x95, 0x01, 0x81, 0x06, 0xc0, 0xc0,
    ];
    let Some(p) = pointer(tablet) else {
        return false;
    };
    let report = [0x01, 0x00, 0x40, 0xff, 0x7f, 0xff];
    let x = p.x.and_then(|f| f.read(&report));
    let y = p.y.and_then(|f| f.read(&report));
    let wheel = p.wheel.and_then(|f| f.read(&report));
    let left = p.buttons.first().and_then(|f| f.read(&report));
    p.absolute()
        && x == Some(0x4000)
        && y == Some(0x7fff)
        && wheel == Some(-1)
        && left == Some(1)
        && p.x.is_some_and(|f| f.logical_max == 0x7fff)
        && !p.uses_report_ids
}
