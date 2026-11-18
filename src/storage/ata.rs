//! Eski IDE/ATA denetleyicisi, PIO kipinde (LBA28/LBA48).

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;

use super::{BlockDevice, Result, SECTOR, register_disk};
use crate::arch::cpu::{inb, inw, outb, outw};
use crate::arch::tsc;
use crate::sched::Mutex;

const STATUS_ERR: u8 = 1 << 0;
const STATUS_DRQ: u8 = 1 << 3;
const STATUS_DF: u8 = 1 << 5;
const STATUS_BSY: u8 = 1 << 7;
const CONTROL_NIEN: u8 = 1 << 1;

const CMD_READ_PIO_EXT: u8 = 0x24;
const CMD_WRITE_PIO_EXT: u8 = 0x34;
const CMD_READ_PIO: u8 = 0x20;
const CMD_WRITE_PIO: u8 = 0x30;
const CMD_FLUSH: u8 = 0xe7;
const CMD_FLUSH_EXT: u8 = 0xea;
const CMD_IDENTIFY: u8 = 0xec;
const TIMEOUT_NS: u64 = 2_000_000_000;

struct Channel {
    io: u16,
    control: u16,
}

pub struct AtaDisk {
    channel: Channel,
    slave: bool,
    lba48: bool,
    sectors: u64,
    model: String,
    index: usize,
    lock: Mutex<()>,
}

impl Channel {
    fn status(&self) -> u8 {
        unsafe { inb(self.io + 7) }
    }

    fn delay(&self) {
        for _ in 0..4 {
            unsafe { inb(self.control) };
        }
    }

    fn wait_not_busy(&self) -> Result<u8> {
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        loop {
            let status = self.status();
            if status & STATUS_BSY == 0 {
                return Ok(status);
            }
            if tsc::now_ns() > deadline {
                return Err("ATA timeout (BSY)");
            }
            core::hint::spin_loop();
        }
    }

    fn wait_data(&self) -> Result<()> {
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        loop {
            let status = self.status();
            if status & (STATUS_ERR | STATUS_DF) != 0 {
                return Err("ATA device error");
            }
            if status & STATUS_BSY == 0 && status & STATUS_DRQ != 0 {
                return Ok(());
            }
            if tsc::now_ns() > deadline {
                return Err("ATA timeout (DRQ)");
            }
            core::hint::spin_loop();
        }
    }
}

impl AtaDisk {
    fn select(&self, lba: u64, count: u16) -> Result<()> {
        let c = &self.channel;
        c.wait_not_busy()?;
        let drive = if self.slave { 0x10 } else { 0x00 };
        unsafe {
            if self.lba48 {
                outb(c.io + 6, 0x40 | drive);
                c.delay();
                outb(c.io + 2, (count >> 8) as u8);
                outb(c.io + 3, (lba >> 24) as u8);
                outb(c.io + 4, (lba >> 32) as u8);
                outb(c.io + 5, (lba >> 40) as u8);
                outb(c.io + 2, count as u8);
                outb(c.io + 3, lba as u8);
                outb(c.io + 4, (lba >> 8) as u8);
                outb(c.io + 5, (lba >> 16) as u8);
            } else {
                outb(c.io + 6, 0xe0 | drive | ((lba >> 24) & 0x0f) as u8);
                c.delay();
                outb(c.io + 2, count as u8);
                outb(c.io + 3, lba as u8);
                outb(c.io + 4, (lba >> 8) as u8);
                outb(c.io + 5, (lba >> 16) as u8);
            }
        }
        Ok(())
    }

    fn transfer(
        &self,
        lba: u64,
        sectors: usize,
        mut f: impl FnMut(&Channel, usize) -> Result<()>,
        write: bool,
    ) -> Result<()> {
        let max = if self.lba48 { 65535 } else { 255 };
        let _guard = self.lock.lock();
        let mut done = 0;
        while done < sectors {
            let count = (sectors - done).min(max);
            self.select(lba + done as u64, count as u16)?;
            let command = match (write, self.lba48) {
                (false, true) => CMD_READ_PIO_EXT,
                (false, false) => CMD_READ_PIO,
                (true, true) => CMD_WRITE_PIO_EXT,
                (true, false) => CMD_WRITE_PIO,
            };
            unsafe { outb(self.channel.io + 7, command) };
            for i in 0..count {
                self.channel.wait_data()?;
                f(&self.channel, done + i)?;
            }
            done += count;
        }
        if write {
            unsafe {
                outb(
                    self.channel.io + 7,
                    if self.lba48 { CMD_FLUSH_EXT } else { CMD_FLUSH },
                )
            };
            self.channel.wait_not_busy()?;
        }
        Ok(())
    }
}

impl BlockDevice for AtaDisk {
    fn name(&self) -> String {
        format!("ata{}", self.index)
    }

    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let sectors = buf.len() / SECTOR;
        if lba + sectors as u64 > self.sectors {
            return Err("read beyond end of disk");
        }
        self.transfer(
            lba,
            sectors,
            |c, i| {
                for w in 0..SECTOR / 2 {
                    let word = unsafe { inw(c.io) };
                    buf[i * SECTOR + w * 2..i * SECTOR + w * 2 + 2]
                        .copy_from_slice(&word.to_le_bytes());
                }
                Ok(())
            },
            false,
        )
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let sectors = buf.len() / SECTOR;
        if lba + sectors as u64 > self.sectors {
            return Err("write beyond end of disk");
        }
        self.transfer(
            lba,
            sectors,
            |c, i| {
                for w in 0..SECTOR / 2 {
                    let at = i * SECTOR + w * 2;
                    unsafe { outw(c.io, u16::from_le_bytes([buf[at], buf[at + 1]])) };
                }
                Ok(())
            },
            true,
        )
    }
}

fn identify(io: u16, control: u16, slave: bool, index: usize) -> Option<AtaDisk> {
    let channel = Channel { io, control };
    unsafe {
        outb(control, CONTROL_NIEN);
        if inb(io + 7) == 0xff {
            return None;
        }
        outb(io + 6, if slave { 0xb0 } else { 0xa0 });
        channel.delay();
        outb(io + 2, 0);
        outb(io + 3, 0);
        outb(io + 4, 0);
        outb(io + 5, 0);
        outb(io + 7, CMD_IDENTIFY);
        if inb(io + 7) == 0 {
            return None;
        }
    }
    channel.wait_not_busy().ok()?;
    let (mid, high) = unsafe { (inb(io + 4), inb(io + 5)) };
    if mid != 0 || high != 0 {
        return None;
    }
    channel.wait_data().ok()?;
    let mut words = [0u16; 256];
    for w in words.iter_mut() {
        *w = unsafe { inw(io) };
    }
    let lba48 = words[83] & (1 << 10) != 0;
    let sectors = if lba48 {
        words[100] as u64
            | (words[101] as u64) << 16
            | (words[102] as u64) << 32
            | (words[103] as u64) << 48
    } else {
        words[60] as u64 | (words[61] as u64) << 16
    };
    let mut model = String::new();
    for w in &words[27..47] {
        model.push((w >> 8) as u8 as char);
        model.push(*w as u8 as char);
    }
    Some(AtaDisk {
        channel,
        slave,
        lba48,
        sectors,
        model: String::from(model.trim()),
        index,
        lock: Mutex::new(()),
    })
}

pub fn probe() {
    let mut index = 0;
    for (io, control) in [(0x1f0u16, 0x3f6u16), (0x170, 0x376)] {
        for slave in [false, true] {
            if let Some(disk) = identify(io, control, slave, index) {
                index += 1;
                if disk.sectors > 0 {
                    register_disk(Arc::new(disk));
                }
            }
        }
    }
}
