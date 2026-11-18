//! ACPI kontrol-metodlu batarya (PNP0C0A) ve AC adaptör (ACPI0003).
//!
//! Değerler her sorguda AML ile okunur: `_STA`, `_BIX`/`_BIF` ve `_BST`.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write;

use crate::aml::{self, Value};
use crate::kernel::initcall::{Detail, self_test as check};

const UNKNOWN: u64 = 0xffff_ffff;

#[derive(Clone, Debug, Default)]
pub struct Battery {
    pub path: String,
    pub present: bool,
    pub power_unit_mwh: bool,
    pub design_capacity: Option<u64>,
    pub full_capacity: Option<u64>,
    pub design_voltage: Option<u64>,
    pub model: String,
    pub serial: String,
    pub technology: String,
    pub oem: String,
    pub charging: bool,
    pub discharging: bool,
    pub critical: bool,
    pub rate: Option<u64>,
    pub remaining: Option<u64>,
    pub voltage: Option<u64>,
}

impl Battery {
    pub fn percent(&self) -> Option<u8> {
        let full = self
            .full_capacity
            .or(self.design_capacity)
            .filter(|&f| f > 0)?;
        let remaining = self.remaining?;
        Some((remaining.min(full) * 100 / full) as u8)
    }

    pub fn state_name(&self) -> &'static str {
        match (self.charging, self.discharging) {
            (true, _) => "charging",
            (false, true) => "discharging",
            _ => "idle/full",
        }
    }

    /// Kalan süre (dakika): deşarjda boşalmaya, şarjda dolmaya kadar.
    pub fn minutes_left(&self) -> Option<u64> {
        let rate = self.rate.filter(|&r| r > 0)?;
        let remaining = self.remaining?;
        if self.discharging {
            Some(remaining * 60 / rate)
        } else if self.charging {
            let full = self.full_capacity.or(self.design_capacity)?;
            Some(full.saturating_sub(remaining) * 60 / rate)
        } else {
            None
        }
    }
}

fn known(value: Option<&Value>) -> Option<u64> {
    value
        .and_then(|v| v.as_integer())
        .filter(|&v| v != UNKNOWN && v != u64::MAX)
}

fn text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(s)) => String::from(s.trim()),
        Some(Value::Buffer(b)) => b
            .iter()
            .take_while(|&&c| c != 0)
            .map(|&c| c as char)
            .collect::<String>()
            .trim()
            .into(),
        _ => String::new(),
    }
}

/// `_BIF` ya da `_BIX` paketinden statik bilgileri doldurur.
fn apply_info(battery: &mut Battery, info: &[Value], extended: bool) {
    let base = if extended { 1 } else { 0 };
    battery.power_unit_mwh = info.get(base).and_then(|v| v.as_integer()) == Some(0);
    battery.design_capacity = known(info.get(base + 1));
    battery.full_capacity = known(info.get(base + 2));
    battery.design_voltage = known(info.get(base + 4));
    let strings = if extended { 16 } else { 9 };
    battery.model = text(info.get(strings));
    battery.serial = text(info.get(strings + 1));
    battery.technology = text(info.get(strings + 2));
    battery.oem = text(info.get(strings + 3));
}

fn apply_status(battery: &mut Battery, status: &[Value]) {
    let state = status.first().and_then(|v| v.as_integer()).unwrap_or(0);
    battery.discharging = state & 1 != 0;
    battery.charging = state & 2 != 0;
    battery.critical = state & 4 != 0;
    battery.rate = known(status.get(1));
    battery.remaining = known(status.get(2));
    battery.voltage = known(status.get(3));
}

fn read_battery(node: usize) -> Battery {
    let mut battery = Battery {
        path: aml::path_of(node),
        ..Battery::default()
    };
    battery.present = aml::device_status(node) & 0x10 != 0;
    if !battery.present {
        return battery;
    }
    if let Some(Ok(Value::Package(info))) = aml::evaluate_child(node, "_BIX", Vec::new()) {
        apply_info(&mut battery, &info, true);
    } else if let Some(Ok(Value::Package(info))) = aml::evaluate_child(node, "_BIF", Vec::new()) {
        apply_info(&mut battery, &info, false);
    }
    match aml::evaluate_child(node, "_BST", Vec::new()) {
        Some(Ok(Value::Package(status))) => apply_status(&mut battery, &status),
        Some(Err(error)) => crate::log!("battery: {}._BST failed: {:?}", battery.path, error),
        _ => {}
    }
    battery
}

pub fn batteries() -> Vec<Battery> {
    aml::find_devices("PNP0C0A")
        .into_iter()
        .map(read_battery)
        .collect()
}

/// `Some(true)` = prize takılı; AC adaptörü tanımlı değilse `None`.
pub fn ac_online() -> Option<bool> {
    let adapters = aml::find_devices("ACPI0003");
    if adapters.is_empty() {
        return None;
    }
    Some(adapters.into_iter().any(|node| {
        matches!(
            aml::evaluate_child(node, "_PSR", Vec::new()),
            Some(Ok(Value::Integer(v))) if v != 0
        )
    }))
}

fn self_test() -> bool {
    let mut battery = Battery::default();
    apply_info(
        &mut battery,
        &[
            Value::Integer(1),
            Value::Integer(5000),
            Value::Integer(4000),
            Value::Integer(1),
            Value::Integer(11100),
        ],
        false,
    );
    apply_status(
        &mut battery,
        &[
            Value::Integer(1),
            Value::Integer(1000),
            Value::Integer(3000),
            Value::Integer(UNKNOWN),
        ],
    );
    battery.percent() == Some(75)
        && battery.minutes_left() == Some(180)
        && battery.voltage.is_none()
        && battery.state_name() == "discharging"
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    check("battery", self_test())?;
    let found = batteries();
    let ac = ac_online();
    for battery in &found {
        crate::log!(
            "battery: {} present={} {}% {} remaining={:?} full={:?} rate={:?} model='{}'",
            battery.path,
            battery.present,
            battery.percent().map(|p| p as i32).unwrap_or(-1),
            battery.state_name(),
            battery.remaining,
            battery.full_capacity,
            battery.rate,
            battery.model
        );
    }
    crate::log!("battery: AC adapter {:?}", ac);

    match found.iter().find(|b| b.present) {
        Some(battery) => {
            let _ = write!(
                detail,
                "{} {}%, {}",
                battery.path.rsplit('.').next().unwrap_or("BAT"),
                battery.percent().unwrap_or(0),
                battery.state_name()
            );
        }
        None => {
            let _ = write!(detail, "no battery (AC powered)");
        }
    }
    Ok(())
}

crate::initcall!(driver, 50, "Battery / AC adapter", init);
