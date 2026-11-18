//! 8259 PIC: IRQ'ları 32-47 vektörlerine taşır, varsayılan olarak hepsini maskeler.

use super::cpu::{inb, io_wait, outb};
use super::idt::IRQ_BASE;

const MASTER_COMMAND: u16 = 0x20;
const MASTER_DATA: u16 = 0x21;
const SLAVE_COMMAND: u16 = 0xa0;
const SLAVE_DATA: u16 = 0xa1;

const ICW1_INIT: u8 = 0x11;
const ICW4_8086: u8 = 0x01;
const EOI: u8 = 0x20;
const READ_ISR: u8 = 0x0b;
const CASCADE_IRQ: u8 = 2;

pub fn init() {
    unsafe {
        outb(MASTER_COMMAND, ICW1_INIT);
        io_wait();
        outb(SLAVE_COMMAND, ICW1_INIT);
        io_wait();
        outb(MASTER_DATA, IRQ_BASE);
        io_wait();
        outb(SLAVE_DATA, IRQ_BASE + 8);
        io_wait();
        outb(MASTER_DATA, 1 << CASCADE_IRQ);
        io_wait();
        outb(SLAVE_DATA, CASCADE_IRQ);
        io_wait();
        outb(MASTER_DATA, ICW4_8086);
        io_wait();
        outb(SLAVE_DATA, ICW4_8086);
        io_wait();

        outb(MASTER_DATA, !(1 << CASCADE_IRQ));
        outb(SLAVE_DATA, 0xff);
    }
}

pub fn unmask(irq: u8) {
    let (port, bit) = if irq < 8 {
        (MASTER_DATA, irq)
    } else {
        (SLAVE_DATA, irq - 8)
    };
    unsafe { outb(port, inb(port) & !(1 << bit)) };
}

#[allow(dead_code)]
pub fn mask(irq: u8) {
    let (port, bit) = if irq < 8 {
        (MASTER_DATA, irq)
    } else {
        (SLAVE_DATA, irq - 8)
    };
    unsafe { outb(port, inb(port) | (1 << bit)) };
}

fn in_service(command_port: u16) -> u8 {
    unsafe {
        outb(command_port, READ_ISR);
        inb(command_port)
    }
}

pub fn is_spurious(irq: u8) -> bool {
    match irq {
        7 => in_service(MASTER_COMMAND) & 0x80 == 0,
        15 if in_service(SLAVE_COMMAND) & 0x80 == 0 => {
            unsafe { outb(MASTER_COMMAND, EOI) };
            true
        }
        _ => false,
    }
}

pub fn end_of_interrupt(irq: u8) {
    unsafe {
        if irq >= 8 {
            outb(SLAVE_COMMAND, EOI);
        }
        outb(MASTER_COMMAND, EOI);
    }
}
