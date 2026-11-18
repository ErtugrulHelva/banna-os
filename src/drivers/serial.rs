//! 16550 UART (COM1) sürücüsü.

use core::fmt;

use crate::arch::cpu::{inb, outb};
use crate::sync::SpinLock;

const COM1: u16 = 0x3f8;

pub struct SerialPort {
    base: u16,
    present: bool,
}

impl SerialPort {
    const fn new(base: u16) -> Self {
        Self {
            base,
            present: false,
        }
    }

    fn init(&mut self) -> bool {
        let base = self.base;
        unsafe {
            outb(base + 1, 0x00);
            outb(base + 3, 0x80);
            outb(base, 0x01);
            outb(base + 1, 0x00);
            outb(base + 3, 0x03);
            outb(base + 2, 0xc7);
            outb(base + 4, 0x1e);
            outb(base, 0xae);
            if inb(base) != 0xae {
                return false;
            }
            outb(base + 4, 0x0f);
        }
        self.present = true;
        true
    }

    fn write_byte(&mut self, byte: u8) {
        if !self.present {
            return;
        }
        unsafe {
            while inb(self.base + 5) & 0x20 == 0 {}
            outb(self.base, byte);
        }
    }
}

impl fmt::Write for SerialPort {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for byte in s.bytes() {
            if byte == b'\n' {
                self.write_byte(b'\r');
            }
            self.write_byte(byte);
        }
        Ok(())
    }
}

pub static COM1_PORT: SpinLock<SerialPort> = SpinLock::new(SerialPort::new(COM1));

pub fn init() -> bool {
    COM1_PORT.lock().init()
}
