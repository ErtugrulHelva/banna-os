//! PS/2 fare (IntelliMouse tekerlek desteğiyle).

use core::sync::atomic::{AtomicBool, AtomicI32, AtomicU8, Ordering};

use crate::arch::{cpu, idt};
use crate::kernel::initcall::{Detail, self_test as check};
use crate::sync::SpinLock;

use super::ps2;

static X: AtomicI32 = AtomicI32::new(0);
static Y: AtomicI32 = AtomicI32::new(0);
static MAX_X: AtomicI32 = AtomicI32::new(1023);
static MAX_Y: AtomicI32 = AtomicI32::new(767);
static BUTTONS: AtomicU8 = AtomicU8::new(0);
static WHEEL: AtomicI32 = AtomicI32::new(0);
static HAS_WHEEL: AtomicBool = AtomicBool::new(false);
static PRESENT: AtomicBool = AtomicBool::new(false);
static PS2_ACTIVE: AtomicBool = AtomicBool::new(false);
/// Göreli hareket çarpanı (yüzde) ve sol el kipi (birincil/ikincil düğme yer değiştirir).
static SPEED: AtomicI32 = AtomicI32::new(100);
static LEFT_HANDED: AtomicBool = AtomicBool::new(false);
static REMAINDER: SpinLock<(i32, i32)> = SpinLock::new((0, 0));

pub const LEFT: u8 = 1;
pub const RIGHT: u8 = 2;

struct Decoder {
    packet: [u8; 4],
    index: usize,
    last_ns: u64,
}

static DECODER: SpinLock<Decoder> = SpinLock::new(Decoder {
    packet: [0; 4],
    index: 0,
    last_ns: 0,
});

/// Bir paketin baytları arasında bundan uzun boşluk varsa paket yarım kalmıştır.
const PACKET_GAP_NS: u64 = 30_000_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct State {
    pub x: i32,
    pub y: i32,
    pub buttons: u8,
}

pub fn state() -> State {
    State {
        x: X.load(Ordering::Relaxed),
        y: Y.load(Ordering::Relaxed),
        buttons: BUTTONS.load(Ordering::Relaxed),
    }
}

/// Son okumadan beri biriken tekerlek hareketi (aşağı pozitif).
pub fn take_wheel() -> i32 {
    WHEEL.swap(0, Ordering::Relaxed)
}

pub fn present() -> bool {
    PRESENT.load(Ordering::Relaxed)
}

pub fn set_bounds(width: i32, height: i32) {
    MAX_X.store(width - 1, Ordering::Relaxed);
    MAX_Y.store(height - 1, Ordering::Relaxed);
    X.store(width / 2, Ordering::Relaxed);
    Y.store(height / 2, Ordering::Relaxed);
}

/// Bir paketi çözer: (dx, dy, butonlar, tekerlek); dy ekran yönündedir (aşağı pozitif).
fn decode(packet: &[u8]) -> Option<(i32, i32, u8, i32)> {
    let flags = packet[0];
    if flags & 0xc0 != 0 {
        return None;
    }
    let dx = packet[1] as i32 - (((flags as i32) << 4) & 0x100);
    let dy = packet[2] as i32 - (((flags as i32) << 3) & 0x100);
    let wheel = packet
        .get(3)
        .map(|&w| (w as i8 as i32).clamp(-8, 7))
        .unwrap_or(0);
    Some((dx, -dy, flags & 0x07, wheel))
}

/// USB fare: göreli hareket (dy aşağı pozitif).
pub fn inject_relative(dx: i32, dy: i32, buttons: u8, wheel: i32) {
    PRESENT.store(true, Ordering::Relaxed);
    apply(dx, dy, buttons, wheel);
    crate::gui::wake();
}

/// USB tablet: `x`, `y` 0..=`range` aralığında mutlak konum.
pub fn inject_absolute(x: u32, y: u32, range: u32, buttons: u8, wheel: i32) {
    PRESENT.store(true, Ordering::Relaxed);
    let range = range.max(1) as i64;
    let px = (x as i64 * (MAX_X.load(Ordering::Relaxed) as i64 + 1) / (range + 1)) as i32;
    let py = (y as i64 * (MAX_Y.load(Ordering::Relaxed) as i64 + 1) / (range + 1)) as i32;
    X.store(
        px.clamp(0, MAX_X.load(Ordering::Relaxed)),
        Ordering::Relaxed,
    );
    Y.store(
        py.clamp(0, MAX_Y.load(Ordering::Relaxed)),
        Ordering::Relaxed,
    );
    BUTTONS.store(map_buttons(buttons), Ordering::Relaxed);
    WHEEL.fetch_add(wheel, Ordering::Relaxed);
    crate::gui::wake();
}

pub fn set_speed(percent: i32) {
    SPEED.store(percent.clamp(20, 400), Ordering::Relaxed);
}

pub fn speed() -> i32 {
    SPEED.load(Ordering::Relaxed)
}

pub fn set_left_handed(on: bool) {
    LEFT_HANDED.store(on, Ordering::Relaxed);
}

pub fn left_handed() -> bool {
    LEFT_HANDED.load(Ordering::Relaxed)
}

fn map_buttons(buttons: u8) -> u8 {
    if LEFT_HANDED.load(Ordering::Relaxed) {
        (buttons & !3) | (buttons & 1) << 1 | (buttons & 2) >> 1
    } else {
        buttons
    }
}

/// Hız çarpanı ve hafif ivme: hızlı hareketler biraz daha uzağa gider.
fn scale(dx: i32, dy: i32) -> (i32, i32) {
    let speed = SPEED.load(Ordering::Relaxed);
    let magnitude = dx.abs().max(dy.abs());
    let accel = if magnitude > 6 {
        100 + (magnitude - 6).min(20) * 4
    } else {
        100
    };
    let factor = speed * accel / 100;
    let mut rem = REMAINDER.lock();
    let sx = dx * factor + rem.0;
    let sy = dy * factor + rem.1;
    *rem = (sx % 100, sy % 100);
    (sx / 100, sy / 100)
}

fn apply(dx: i32, dy: i32, buttons: u8, wheel: i32) {
    let (dx, dy) = scale(dx, dy);
    let buttons = map_buttons(buttons);
    let x = (X.load(Ordering::Relaxed) + dx).clamp(0, MAX_X.load(Ordering::Relaxed));
    let y = (Y.load(Ordering::Relaxed) + dy).clamp(0, MAX_Y.load(Ordering::Relaxed));
    X.store(x, Ordering::Relaxed);
    Y.store(y, Ordering::Relaxed);
    BUTTONS.store(buttons, Ordering::Relaxed);
    WHEEL.fetch_add(wheel, Ordering::Relaxed);
}

pub(super) fn on_byte(byte: u8) {
    if !PS2_ACTIVE.load(Ordering::Relaxed) {
        return;
    }
    let size = if HAS_WHEEL.load(Ordering::Relaxed) {
        4
    } else {
        3
    };
    let mut decoder = DECODER.lock();
    let now = crate::arch::tsc::now_ns();
    if decoder.index > 0 && now.saturating_sub(decoder.last_ns) > PACKET_GAP_NS {
        decoder.index = 0;
    }
    decoder.last_ns = now;
    if decoder.index == 0 && byte & 0x08 == 0 {
        return;
    }
    let index = decoder.index;
    decoder.packet[index] = byte;
    decoder.index += 1;
    if decoder.index == size {
        decoder.index = 0;
        let packet = decoder.packet;
        drop(decoder);
        if let Some((dx, dy, buttons, wheel)) = decode(&packet[..size]) {
            apply(dx, dy, buttons, wheel);
            crate::gui::wake();
        }
    }
}

fn command(byte: u8) -> Option<u8> {
    if !ps2::command(0xd4) || !ps2::write_data(byte) {
        return None;
    }
    ps2::read_data()
}

fn command_ack(byte: u8) -> bool {
    command(byte) == Some(0xfa)
}

fn init_device() -> Result<bool, &'static str> {
    ps2::enable_second_port()?;
    if !command_ack(0xff) {
        return Err("mouse did not acknowledge reset");
    }
    if ps2::read_reset_result() != Some(0xaa) {
        return Err("mouse self-test failed");
    }
    let _ = ps2::read_data();
    if !command_ack(0xf6) {
        return Err("mouse rejected defaults");
    }
    for rate in [200, 100, 80] {
        if !(command_ack(0xf3) && command_ack(rate)) {
            break;
        }
    }
    let wheel = command_ack(0xf2) && ps2::read_data() == Some(3);
    if !(command_ack(0xf3) && command_ack(100)) {
        return Err("mouse rejected sample rate");
    }
    if !command_ack(0xf4) {
        return Err("mouse did not enable reporting");
    }
    Ok(wheel)
}

fn self_test() -> bool {
    decode(&[0x08 | 0x10, 0xfb, 0x05]) == Some((-5, -5, 0, 0))
        && decode(&[0x29, 0x02, 0xfe, 0x01]) == Some((2, 2, LEFT, 1))
        && decode(&[0x48, 0, 0]).is_none()
}

fn init(detail: &mut Detail) -> Result<(), &'static str> {
    check("mouse", self_test())?;
    let wheel = cpu::without_interrupts(|| {
        ps2::pause_keyboard();
        let result = init_device();
        ps2::resume_keyboard();
        result
    })?;
    HAS_WHEEL.store(wheel, Ordering::Relaxed);
    PS2_ACTIVE.store(true, Ordering::Relaxed);
    PRESENT.store(true, Ordering::Relaxed);
    idt::register_irq(12, ps2::on_irq);
    use core::fmt::Write;
    let _ = write!(
        detail,
        "{}",
        if wheel {
            "3 buttons + wheel"
        } else {
            "3 buttons"
        }
    );
    Ok(())
}

crate::initcall!(driver, 25, "PS/2 mouse", init);
