//! USB HID: klavye (boot protokolü), fare ve tablet (rapor tanımlayıcısıyla).
//!
//! Klavye raporları PS/2 set 1 tarama kodlarına çevrilip mevcut klavye katmanına
//! verilir; böylece düzenler (Türkçe Q/ABD) ve değiştirici korumaları aynen çalışır.
//! USB klavyeler kendiliğinden tekrar etmez: tekrar `tick` ile yazılımda yapılır.

use alloc::sync::Arc;
use alloc::vec::Vec;

use super::hid_report::{self, Pointer};
use super::host::{self, EndpointConfig};
use super::{InterfaceInfo, UsbDevice};
use crate::drivers::{keyboard, mouse};
use crate::sync::SpinLock;

const REPEAT_DELAY_NS: u64 = 500_000_000;
const REPEAT_INTERVAL_NS: u64 = 33_000_000;

/// HID kullanım kodu (0x04..=0x65) → set 1 tarama kodu; 0xe0 öneki için bit 8.
const fn usage_to_set1(usage: u8) -> u16 {
    const E: u16 = 0x100;
    match usage {
        0x04 => 0x1e,
        0x05 => 0x30,
        0x06 => 0x2e,
        0x07 => 0x20,
        0x08 => 0x12,
        0x09 => 0x21,
        0x0a => 0x22,
        0x0b => 0x23,
        0x0c => 0x17,
        0x0d => 0x24,
        0x0e => 0x25,
        0x0f => 0x26,
        0x10 => 0x32,
        0x11 => 0x31,
        0x12 => 0x18,
        0x13 => 0x19,
        0x14 => 0x10,
        0x15 => 0x13,
        0x16 => 0x1f,
        0x17 => 0x14,
        0x18 => 0x16,
        0x19 => 0x2f,
        0x1a => 0x11,
        0x1b => 0x2d,
        0x1c => 0x15,
        0x1d => 0x2c,
        0x1e..=0x26 => (usage - 0x1e + 0x02) as u16,
        0x27 => 0x0b,
        0x28 => 0x1c,
        0x29 => 0x01,
        0x2a => 0x0e,
        0x2b => 0x0f,
        0x2c => 0x39,
        0x2d => 0x0c,
        0x2e => 0x0d,
        0x2f => 0x1a,
        0x30 => 0x1b,
        0x31 | 0x32 => 0x2b,
        0x33 => 0x27,
        0x34 => 0x28,
        0x35 => 0x29,
        0x36 => 0x33,
        0x37 => 0x34,
        0x38 => 0x35,
        0x39 => 0x3a,
        0x3a..=0x43 => (usage - 0x3a + 0x3b) as u16,
        0x44 => 0x57,
        0x45 => 0x58,
        0x47 => 0x46,
        0x49 => E | 0x52,
        0x4a => E | 0x47,
        0x4b => E | 0x49,
        0x4c => E | 0x53,
        0x4d => E | 0x4f,
        0x4e => E | 0x51,
        0x4f => E | 0x4d,
        0x50 => E | 0x4b,
        0x51 => E | 0x50,
        0x52 => E | 0x48,
        0x53 => 0x45,
        0x54 => E | 0x35,
        0x55 => 0x37,
        0x56 => 0x4a,
        0x57 => 0x4e,
        0x58 => E | 0x1c,
        0x59 => 0x4f,
        0x5a => 0x50,
        0x5b => 0x51,
        0x5c => 0x4b,
        0x5d => 0x4c,
        0x5e => 0x4d,
        0x5f => 0x47,
        0x60 => 0x48,
        0x61 => 0x49,
        0x62 => 0x52,
        0x63 => 0x53,
        0x64 => 0x56,
        _ => 0,
    }
}

/// Değiştirici bitleri (LCtrl, LShift, LAlt, LGUI, RCtrl, RShift, RAlt, RGUI).
const MODIFIER_CODES: [u16; 8] = [0x1d, 0x2a, 0x38, 0x15b, 0x11d, 0x36, 0x138, 0x15c];

fn scancode_bytes(code: u16, release: bool, out: &mut Vec<u8>) {
    if code == 0 {
        return;
    }
    if code & 0x100 != 0 {
        out.push(0xe0);
    }
    out.push((code as u8) | if release { 0x80 } else { 0 });
}

#[derive(Default)]
struct KeyboardState {
    modifiers: u8,
    keys: [u8; 6],
}

/// Önceki ve yeni boot raporunu karşılaştırıp basma/bırakma kodları üretir.
fn translate(state: &mut KeyboardState, report: &[u8], out: &mut Vec<u8>) -> Option<u16> {
    if report.len() < 8 {
        return None;
    }
    // Rollover hatası (tüm anahtarlar 0x01): yok say.
    if report[2..8].iter().all(|&k| k == 1) {
        return None;
    }
    let modifiers = report[0];
    let keys: [u8; 6] = report[2..8].try_into().ok()?;
    for (bit, &code) in MODIFIER_CODES.iter().enumerate() {
        let was = state.modifiers >> bit & 1 != 0;
        let now = modifiers >> bit & 1 != 0;
        if was != now {
            scancode_bytes(code, !now, out);
        }
    }
    for &old in state.keys.iter().filter(|&&k| k >= 4) {
        if !keys.contains(&old) {
            scancode_bytes(usage_to_set1(old), true, out);
        }
    }
    let mut pressed = None;
    for &new in keys.iter().filter(|&&k| k >= 4) {
        if !state.keys.contains(&new) {
            let code = usage_to_set1(new);
            scancode_bytes(code, false, out);
            if code != 0 {
                pressed = Some(code);
            }
        }
    }
    state.modifiers = modifiers;
    state.keys = keys;
    pressed
}

struct Repeat {
    code: u16,
    next_ns: u64,
    owner: (usize, u8),
}

static REPEAT: SpinLock<Option<Repeat>> = SpinLock::new(None);

fn keyboard_report(owner: (usize, u8), state: &SpinLock<KeyboardState>, report: &[u8]) {
    let mut bytes = Vec::new();
    let (pressed, still_held) = {
        let mut s = state.lock();
        let pressed = translate(&mut s, report, &mut bytes);
        let held: Vec<u16> = s
            .keys
            .iter()
            .filter(|&&k| k >= 4)
            .map(|&k| usage_to_set1(k))
            .collect();
        (pressed, held)
    };
    {
        let mut repeat = REPEAT.lock();
        let now = crate::arch::tsc::now_ns();
        if let Some(code) = pressed {
            *repeat = Some(Repeat {
                code,
                next_ns: now + REPEAT_DELAY_NS,
                owner,
            });
        } else if repeat
            .as_ref()
            .is_some_and(|r| r.owner == owner && !still_held.contains(&r.code))
        {
            *repeat = None;
        }
    }
    if !bytes.is_empty() {
        keyboard::inject(&bytes);
    }
}

/// `usbd` tarafından düzenli çağrılır: basılı tutulan tuşu tekrarlar.
pub fn tick() {
    let now = crate::arch::tsc::now_ns();
    let code = {
        let mut repeat = REPEAT.lock();
        match repeat.as_mut() {
            Some(r) if now >= r.next_ns => {
                r.next_ns = now + REPEAT_INTERVAL_NS;
                Some(r.code)
            }
            _ => None,
        }
    };
    if let Some(code) = code {
        let mut bytes = Vec::new();
        scancode_bytes(code, false, &mut bytes);
        keyboard::inject(&bytes);
    }
}

pub(crate) fn pointer_report(pointer: &Pointer, report: &[u8]) {
    let data = if pointer.uses_report_ids {
        match report.split_first() {
            Some((&id, rest)) if id == pointer.report_id => rest,
            _ => return,
        }
    } else {
        report
    };
    let mut buttons = 0u8;
    for f in &pointer.buttons {
        if f.read(data).unwrap_or(0) != 0 {
            buttons |= 1 << (f.usage - 1);
        }
    }
    let wheel = pointer.wheel.and_then(|f| f.read(data)).unwrap_or(0);
    let (Some(xf), Some(yf)) = (pointer.x, pointer.y) else {
        return;
    };
    let (Some(x), Some(y)) = (xf.read(data), yf.read(data)) else {
        return;
    };
    if pointer.absolute() {
        let range = (xf.logical_max - xf.logical_min).max(1) as u32;
        let yrange = (yf.logical_max - yf.logical_min).max(1) as u32;
        let ny = ((y - yf.logical_min).max(0) as u64 * range as u64 / yrange as u64) as u32;
        mouse::inject_absolute(
            (x - xf.logical_min).max(0) as u32,
            ny,
            range,
            buttons,
            -wheel,
        );
    } else {
        mouse::inject_relative(x, y, buttons, -wheel);
    }
}

/// Uç noktayı yapılandırır ve kesme aktarımını başlatır.
fn start(device: &UsbDevice, iface: &InterfaceInfo, handler: host::Handler) -> bool {
    let Some(ep) = iface
        .endpoints
        .iter()
        .find(|e| e.transfer_type() == 3 && e.is_in())
    else {
        return false;
    };
    let config = EndpointConfig {
        address: ep.address,
        transfer: 3,
        max_packet: ep.max_packet,
        interval: ep.interval,
    };
    host::configure_endpoints(device.controller, device.slot, device.speed, &[config])
        && host::start_interrupt_pipe(
            device.controller,
            device.slot,
            ep.address,
            (ep.max_packet & 0x7ff) as usize,
            handler,
        )
}

pub fn attach(device: &UsbDevice, iface: &InterfaceInfo) -> Option<&'static str> {
    let index = iface.number as u16;
    // SET_IDLE(0): yalnızca değişince rapor gönder.
    device.control_out(0x21, 0x0a, 0, index);
    if iface.subclass == 1 && iface.protocol == 1 {
        // SET_PROTOCOL(boot): 8 baytlık sabit rapor biçimi.
        device.control_out(0x21, 0x0b, 0, index);
        let state = Arc::new(SpinLock::new(KeyboardState::default()));
        let owner = (device.controller, device.slot);
        let handler: host::Handler = Arc::new(move |r: &[u8]| keyboard_report(owner, &state, r));
        return start(device, iface, handler).then_some("keyboard");
    }
    let length = if iface.hid_report_length == 0 {
        256
    } else {
        iface.hid_report_length as usize
    };
    let mut desc = alloc::vec![0u8; length.min(4096)];
    let n = device
        .control(
            host::Setup {
                request_type: 0x81,
                request: 6,
                value: 0x2200,
                index,
                length: desc.len() as u16,
            },
            &mut desc,
        )
        .ok()?;
    desc.truncate(n);
    let pointer = hid_report::pointer(&desc)?;
    let kind = if pointer.absolute() {
        "tablet"
    } else {
        "mouse"
    };
    let pointer = Arc::new(pointer);
    let handler: host::Handler = Arc::new(move |r: &[u8]| pointer_report(&pointer, r));
    start(device, iface, handler).then_some(kind)
}

pub fn detach(device: &UsbDevice) {
    let mut repeat = REPEAT.lock();
    if repeat
        .as_ref()
        .is_some_and(|r| r.owner == (device.controller, device.slot))
    {
        *repeat = None;
    }
}

pub fn self_test() -> bool {
    let mut s = KeyboardState::default();
    let mut out = Vec::new();
    // Shift + a, sonra a bırakılır, sonra Shift bırakılır.
    let p1 = translate(&mut s, &[0x02, 0, 0x04, 0, 0, 0, 0, 0], &mut out);
    translate(&mut s, &[0x02, 0, 0, 0, 0, 0, 0, 0], &mut out);
    translate(&mut s, &[0x00, 0, 0, 0, 0, 0, 0, 0], &mut out);
    // Sağ ok (E0 4D), sağ Alt (E0 38).
    translate(&mut s, &[0x40, 0, 0x4f, 0, 0, 0, 0, 0], &mut out);
    translate(&mut s, &[0, 0, 0, 0, 0, 0, 0, 0], &mut out);
    p1 == Some(0x1e)
        && out
            == [
                0x2a, 0x1e, 0x9e, 0xaa, 0xe0, 0x38, 0xe0, 0x4d, 0xe0, 0xb8, 0xe0, 0xcd,
            ]
}
