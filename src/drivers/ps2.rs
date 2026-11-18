//! 8042 PS/2 denetleyicisi.

use crate::arch::cpu::{inb, outb};

const DATA: u16 = 0x60;
const STATUS: u16 = 0x64;
const COMMAND: u16 = 0x64;

const STATUS_OUTPUT_FULL: u8 = 1 << 0;
const STATUS_INPUT_FULL: u8 = 1 << 1;
const STATUS_AUX_DATA: u8 = 1 << 5;

const CONFIG_PORT1_IRQ: u8 = 1 << 0;
const CONFIG_PORT2_IRQ: u8 = 1 << 1;
const CONFIG_PORT1_CLOCK_OFF: u8 = 1 << 4;
const CONFIG_PORT2_CLOCK_OFF: u8 = 1 << 5;
const CONFIG_TRANSLATION: u8 = 1 << 6;

const TIMEOUT_NS: u64 = 50_000_000;
const RESET_TIMEOUT_NS: u64 = 1_000_000_000;

fn wait_status(ready: impl Fn(u8) -> bool) -> bool {
    wait_status_for(ready, TIMEOUT_NS)
}

fn wait_status_for(ready: impl Fn(u8) -> bool, timeout_ns: u64) -> bool {
    let deadline = crate::arch::tsc::now_ns() + timeout_ns;
    loop {
        if ready(unsafe { inb(STATUS) }) {
            return true;
        }
        if crate::arch::tsc::now_ns() > deadline {
            return false;
        }
        core::hint::spin_loop();
    }
}

fn wait_input_empty() -> bool {
    wait_status(|s| s & STATUS_INPUT_FULL == 0)
}

fn wait_output_full() -> bool {
    wait_status(|s| s & STATUS_OUTPUT_FULL != 0)
}

pub fn command(cmd: u8) -> bool {
    if !wait_input_empty() {
        return false;
    }
    unsafe { outb(COMMAND, cmd) };
    true
}

pub fn write_data(value: u8) -> bool {
    if !wait_input_empty() {
        return false;
    }
    unsafe { outb(DATA, value) };
    true
}

pub fn read_data() -> Option<u8> {
    wait_output_full().then(|| unsafe { inb(DATA) })
}

/// Sıfırlama sonrası self-test yanıtı için (PS/2 standardı 500 ms'ye kadar izin verir).
pub fn read_reset_result() -> Option<u8> {
    wait_status_for(|s| s & STATUS_OUTPUT_FULL != 0, RESET_TIMEOUT_NS).then(|| unsafe { inb(DATA) })
}

/// IRQ1 ve IRQ12'nin ortak işleyicisi: çıkış tamponu doluyken okur ve her baytı
/// durum yazmacındaki AUX bitine göre klavyeye ya da fareye yönlendirir. Tampon
/// boşken port okunmaz (son bayt tekrar döner); fare baytı klavyeye karışmaz.
pub fn on_irq() {
    for _ in 0..32 {
        let status = unsafe { inb(STATUS) };
        if status & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        let byte = unsafe { inb(DATA) };
        if status & STATUS_AUX_DATA != 0 {
            super::mouse::on_byte(byte);
        } else {
            super::keyboard::on_byte(byte);
        }
    }
}

fn flush() {
    for _ in 0..32 {
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA) };
    }
}

fn read_config() -> Option<u8> {
    command(0x20).then(read_data).flatten()
}

fn write_config(value: u8) -> bool {
    command(0x60) && write_data(value)
}

pub fn send_to_keyboard(byte: u8) -> Option<u8> {
    for _ in 0..3 {
        if !write_data(byte) {
            return None;
        }
        match read_data()? {
            0xfe => continue,
            response => return Some(response),
        }
    }
    None
}

/// Klavye portunu kapatır ve bekleyen baytları atar: fare kurulurken basılan tuşların
/// baytı (ör. açılışta B) denetleyici yanıtı yerine okunmasın.
pub fn pause_keyboard() {
    command(0xad);
    for _ in 0..32 {
        if unsafe { inb(STATUS) } & STATUS_OUTPUT_FULL == 0 {
            break;
        }
        unsafe { inb(DATA) };
    }
}

pub fn resume_keyboard() {
    command(0xae);
}

pub fn enable_second_port() -> Result<(), &'static str> {
    command(0xa8);
    if !command(0xa9) || read_data() != Some(0x00) {
        return Err("no second PS/2 port");
    }
    let mut config = read_config().ok_or("controller did not answer")?;
    config |= CONFIG_PORT2_IRQ;
    config &= !CONFIG_PORT2_CLOCK_OFF;
    write_config(config);
    Ok(())
}

/// Denetleyiciyi sıfırlar ve 1. portu (klavye) IRQ1 + set 1 çevirisiyle açar.
pub fn init() -> Result<(), &'static str> {
    if unsafe { inb(STATUS) } == 0xff {
        return Err("no 8042 controller");
    }
    command(0xad);
    command(0xa7);
    flush();

    let mut config = read_config().ok_or("controller did not answer")?;
    config &= !(CONFIG_PORT1_IRQ | CONFIG_PORT2_IRQ | CONFIG_TRANSLATION);
    write_config(config);

    if !command(0xaa) || read_data() != Some(0x55) {
        return Err("controller self-test failed");
    }
    write_config(config);

    if !command(0xab) || read_data() != Some(0x00) {
        return Err("keyboard port test failed");
    }

    command(0xae);
    config |= CONFIG_PORT1_IRQ | CONFIG_TRANSLATION;
    config &= !CONFIG_PORT1_CLOCK_OFF;
    write_config(config);

    if send_to_keyboard(0xff) != Some(0xfa) {
        return Err("keyboard reset not acknowledged");
    }
    if read_reset_result() != Some(0xaa) {
        return Err("keyboard self-test failed");
    }
    if send_to_keyboard(0xf4) != Some(0xfa) {
        return Err("keyboard did not enable scanning");
    }
    flush();
    Ok(())
}
