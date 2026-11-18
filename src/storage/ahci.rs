//! AHCI (SATA) denetleyicisi: DMA ile okuma/yazma, port başına tek komut yuvası.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::ptr::{read_volatile, write_bytes, write_volatile};

use super::{BlockDevice, Result, SECTOR, register_disk};
use crate::arch::tsc;
use crate::drivers::pci;
use crate::memory::paging::{NO_CACHE, map_physical};
use crate::memory::{PAGE_SIZE, phys_to_virt, pmm};
use crate::sched::Mutex;

const GHC: usize = 0x04;
const PI: usize = 0x0c;
const GHC_AE: u32 = 1 << 31;

const PORT_CLB: usize = 0x00;
const PORT_FB: usize = 0x08;
const PORT_IS: usize = 0x10;
const PORT_IE: usize = 0x14;
const PORT_CMD: usize = 0x18;
const PORT_TFD: usize = 0x20;
const PORT_SIG: usize = 0x24;
const PORT_SSTS: usize = 0x28;
const PORT_SERR: usize = 0x30;
const PORT_CI: usize = 0x38;

const CMD_ST: u32 = 1 << 0;
const CMD_FRE: u32 = 1 << 4;
const CMD_FR: u32 = 1 << 14;
const CMD_CR: u32 = 1 << 15;
const IS_TFES: u32 = 1 << 30;

const SIG_SATA: u32 = 0x0000_0101;
const BOUNCE_PAGES: u64 = 16;
const MAX_SECTORS: usize = (BOUNCE_PAGES * PAGE_SIZE) as usize / SECTOR;
const TIMEOUT_NS: u64 = 5_000_000_000;

struct Port {
    regs: *mut u8,
    command_list: u64,
    command_table: u64,
    bounce: u64,
}

unsafe impl Send for Port {}

impl Port {
    fn read(&self, reg: usize) -> u32 {
        unsafe { read_volatile(self.regs.add(reg) as *const u32) }
    }

    fn write(&self, reg: usize, value: u32) {
        unsafe { write_volatile(self.regs.add(reg) as *mut u32, value) }
    }

    fn wait_clear(&self, reg: usize, mask: u32) -> Result<()> {
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        while self.read(reg) & mask != 0 {
            if tsc::now_ns() > deadline {
                return Err("AHCI timeout");
            }
            core::hint::spin_loop();
        }
        Ok(())
    }

    fn stop(&self) -> Result<()> {
        let cmd = self.read(PORT_CMD);
        self.write(PORT_CMD, cmd & !(CMD_ST | CMD_FRE));
        self.wait_clear(PORT_CMD, CMD_CR | CMD_FR)
    }

    fn start(&self) -> Result<()> {
        self.wait_clear(PORT_CMD, CMD_CR)?;
        let cmd = self.read(PORT_CMD);
        self.write(PORT_CMD, cmd | CMD_FRE);
        self.write(PORT_CMD, cmd | CMD_FRE | CMD_ST);
        Ok(())
    }

    /// Yuva 0'a bir ATA komutu kurar ve tamamlanmasını bekler.
    fn issue(&self, command: u8, lba: u64, sectors: usize, write: bool) -> Result<()> {
        let header = phys_to_virt(self.command_list) as *mut u32;
        let table = phys_to_virt(self.command_table);
        let bytes = (sectors * SECTOR).max(512) as u32;
        unsafe {
            write_bytes(table, 0, 0x80 + 16);
            let fis = table;
            *fis = 0x27;
            *fis.add(1) = 0x80;
            *fis.add(2) = command;
            *fis.add(4) = lba as u8;
            *fis.add(5) = (lba >> 8) as u8;
            *fis.add(6) = (lba >> 16) as u8;
            *fis.add(7) = 1 << 6;
            *fis.add(8) = (lba >> 24) as u8;
            *fis.add(9) = (lba >> 32) as u8;
            *fis.add(10) = (lba >> 40) as u8;
            *fis.add(12) = sectors as u8;
            *fis.add(13) = (sectors >> 8) as u8;

            let prdt = table.add(0x80) as *mut u32;
            write_volatile(prdt, self.bounce as u32);
            write_volatile(prdt.add(1), (self.bounce >> 32) as u32);
            write_volatile(prdt.add(3), bytes - 1);

            let prdt_entries = if sectors > 0 { 1 << 16 } else { 0 };
            let flags = 5 | if write { 1 << 6 } else { 0 } | prdt_entries;
            write_volatile(header, flags);
            write_volatile(header.add(1), 0);
            write_volatile(header.add(2), self.command_table as u32);
            write_volatile(header.add(3), (self.command_table >> 32) as u32);
        }

        self.wait_clear(PORT_TFD, 0x88)?;
        self.write(PORT_IS, u32::MAX);
        self.write(PORT_CI, 1);
        let deadline = tsc::now_ns() + TIMEOUT_NS;
        loop {
            if self.read(PORT_IS) & IS_TFES != 0 {
                return Err("AHCI task file error");
            }
            if self.read(PORT_CI) & 1 == 0 {
                break;
            }
            if tsc::now_ns() > deadline {
                return Err("AHCI command timeout");
            }
            core::hint::spin_loop();
        }
        if self.read(PORT_TFD) & 0x01 != 0 {
            return Err("AHCI device error");
        }
        Ok(())
    }
}

pub struct AhciDisk {
    port: Mutex<Port>,
    sectors: u64,
    model: String,
    index: usize,
}

impl BlockDevice for AhciDisk {
    fn name(&self) -> String {
        format!("sata{}", self.index)
    }

    fn sectors(&self) -> u64 {
        self.sectors
    }

    fn model(&self) -> String {
        self.model.clone()
    }

    fn read(&self, lba: u64, buf: &mut [u8]) -> Result<()> {
        let port = self.port.lock();
        let total = buf.len() / SECTOR;
        let mut done = 0;
        while done < total {
            let count = (total - done).min(MAX_SECTORS);
            port.issue(0x25, lba + done as u64, count, false)?;
            let src = phys_to_virt(port.bounce);
            let bytes = count * SECTOR;
            unsafe {
                core::ptr::copy_nonoverlapping(src, buf[done * SECTOR..].as_mut_ptr(), bytes);
            }
            done += count;
        }
        Ok(())
    }

    fn write(&self, lba: u64, buf: &[u8]) -> Result<()> {
        let port = self.port.lock();
        let total = buf.len() / SECTOR;
        let mut done = 0;
        while done < total {
            let count = (total - done).min(MAX_SECTORS);
            let dst = phys_to_virt(port.bounce);
            unsafe {
                core::ptr::copy_nonoverlapping(buf[done * SECTOR..].as_ptr(), dst, count * SECTOR);
            }
            port.issue(0x35, lba + done as u64, count, true)?;
            done += count;
        }
        port.issue(0xea, 0, 0, false)
    }
}

fn setup_port(abar: *mut u8, number: usize) -> Result<Port> {
    let command_list = pmm::alloc_dma_frame().ok_or("out of memory")?;
    let command_table = pmm::alloc_dma_frame().ok_or("out of memory")?;
    let bounce = pmm::alloc_contiguous(BOUNCE_PAGES).ok_or("no contiguous DMA memory")?;
    let port = Port {
        regs: unsafe { abar.add(0x100 + number * 0x80) },
        command_list,
        command_table,
        bounce,
    };
    port.stop()?;
    port.write(PORT_CLB, command_list as u32);
    port.write(PORT_CLB + 4, (command_list >> 32) as u32);
    let fis = command_list + 1024;
    port.write(PORT_FB, fis as u32);
    port.write(PORT_FB + 4, (fis >> 32) as u32);
    port.write(PORT_SERR, u32::MAX);
    port.write(PORT_IS, u32::MAX);
    port.write(PORT_IE, 0);
    port.start()?;
    Ok(port)
}

fn identify(port: &Port) -> Result<(u64, String)> {
    port.issue(0xec, 0, 1, false)?;
    let data = phys_to_virt(port.bounce) as *const u16;
    let word = |i: usize| unsafe { read_volatile(data.add(i)) };
    let lba48 = word(83) & (1 << 10) != 0;
    let sectors = if lba48 {
        (0..4).fold(0u64, |acc, i| acc | (word(100 + i) as u64) << (16 * i))
    } else {
        word(60) as u64 | (word(61) as u64) << 16
    };
    let mut model = String::new();
    for i in 27..47 {
        let w = word(i);
        model.push((w >> 8) as u8 as char);
        model.push(w as u8 as char);
    }
    Ok((sectors, String::from(model.trim())))
}

pub fn probe() {
    let mut index = 0;
    for controller in pci::find_all(0x01, 0x06) {
        if controller.prog_if != 0x01 {
            continue;
        }
        let Some(base) = controller.bar(5) else {
            continue;
        };
        controller.enable();
        let Ok(abar) = map_physical(base, 0x1100, NO_CACHE) else {
            continue;
        };
        unsafe {
            let ghc = abar.add(GHC) as *mut u32;
            write_volatile(ghc, read_volatile(ghc) | GHC_AE);
        }
        let implemented = unsafe { read_volatile(abar.add(PI) as *const u32) };
        for number in 0..32 {
            if implemented & (1 << number) == 0 {
                continue;
            }
            let regs = unsafe { abar.add(0x100 + number * 0x80) };
            let ssts = unsafe { read_volatile(regs.add(PORT_SSTS) as *const u32) };
            let signature = unsafe { read_volatile(regs.add(PORT_SIG) as *const u32) };
            if ssts & 0xf != 3 || signature != SIG_SATA {
                continue;
            }
            match setup_port(abar, number).and_then(|port| identify(&port).map(|id| (port, id))) {
                Ok((port, (sectors, model))) if sectors > 0 => {
                    register_disk(Arc::new(AhciDisk {
                        port: Mutex::new(port),
                        sectors,
                        model,
                        index,
                    }));
                    index += 1;
                }
                Ok(_) => {}
                Err(error) => crate::log!("ahci: port {} failed: {}", number, error),
            }
        }
    }
}
