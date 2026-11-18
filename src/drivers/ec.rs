//! ACPI Embedded Controller (dizüstülerde batarya, fan ve termal bilgisinin kaynağı).

use core::sync::atomic::{AtomicU32, Ordering};

use crate::arch::cpu::{inb, outb};
use crate::arch::tsc;
use crate::sync::SpinLock;

const STATUS_OBF: u8 = 1 << 0;
const STATUS_IBF: u8 = 1 << 1;
const CMD_READ: u8 = 0x80;
const CMD_WRITE: u8 = 0x81;
const CMD_QUERY: u8 = 0x84;
const STATUS_SCI_EVT: u8 = 1 << 5;
const TIMEOUT_NS: u64 = 50_000_000;

/// Alt 16 bit veri portu, üst 16 bit komut/durum portu; 0 = EC yok.
static PORTS: AtomicU32 = AtomicU32::new(0);
static LOCK: SpinLock<()> = SpinLock::new(());

#[derive(Debug)]
pub struct Timeout;

pub fn configure(data: u16, command: u16) {
    PORTS.store(((command as u32) << 16) | data as u32, Ordering::Relaxed);
    crate::log!("ec: data port {:#x}, command port {:#x}", data, command);
}

pub fn present() -> bool {
    PORTS.load(Ordering::Relaxed) != 0
}

fn ports() -> Result<(u16, u16), Timeout> {
    let ports = PORTS.load(Ordering::Relaxed);
    if ports == 0 {
        return Err(Timeout);
    }
    Ok((ports as u16, (ports >> 16) as u16))
}

fn wait(command: u16, ready: impl Fn(u8) -> bool) -> Result<(), Timeout> {
    let deadline = tsc::now_ns() + TIMEOUT_NS;
    while !ready(unsafe { inb(command) }) {
        if tsc::now_ns() > deadline {
            return Err(Timeout);
        }
        core::hint::spin_loop();
    }
    Ok(())
}

pub fn read(register: u8) -> Result<u8, Timeout> {
    let (data, command) = ports()?;
    let _guard = LOCK.lock();
    wait(command, |s| s & STATUS_IBF == 0)?;
    unsafe { outb(command, CMD_READ) };
    wait(command, |s| s & STATUS_IBF == 0)?;
    unsafe { outb(data, register) };
    wait(command, |s| s & STATUS_OBF != 0)?;
    Ok(unsafe { inb(data) })
}

/// Bekleyen EC olayının sorgu numarası (`_Qxx`); olay yoksa `None`.
pub fn query() -> Option<u8> {
    let (data, command) = ports().ok()?;
    let _guard = LOCK.lock();
    if unsafe { inb(command) } & STATUS_SCI_EVT == 0 {
        return None;
    }
    wait(command, |s| s & STATUS_IBF == 0).ok()?;
    unsafe { outb(command, CMD_QUERY) };
    wait(command, |s| s & STATUS_OBF != 0).ok()?;
    let value = unsafe { inb(data) };
    (value != 0).then_some(value)
}

pub fn write(register: u8, value: u8) -> Result<(), Timeout> {
    let (data, command) = ports()?;
    let _guard = LOCK.lock();
    wait(command, |s| s & STATUS_IBF == 0)?;
    unsafe { outb(command, CMD_WRITE) };
    wait(command, |s| s & STATUS_IBF == 0)?;
    unsafe { outb(data, register) };
    wait(command, |s| s & STATUS_IBF == 0)?;
    unsafe { outb(data, value) };
    Ok(())
}
