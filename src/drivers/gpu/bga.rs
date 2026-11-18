//! Bochs/QEMU standart VGA (BGA, "DISPI"): çözünürlük ayarı.

use crate::arch::cpu::{inw, outw};
use crate::drivers::framebuffer::Framebuffer;
use crate::drivers::pci::{self, Device};
use crate::gfx::display;

const INDEX: u16 = 0x01ce;
const DATA: u16 = 0x01cf;
const REG_ID: u16 = 0;
const REG_XRES: u16 = 1;
const REG_YRES: u16 = 2;
const REG_BPP: u16 = 3;
const REG_ENABLE: u16 = 4;
const REG_VIRT_WIDTH: u16 = 6;
const REG_VIRT_HEIGHT: u16 = 7;
const REG_X_OFFSET: u16 = 8;
const REG_Y_OFFSET: u16 = 9;
const ENABLED: u16 = 0x01;
const LFB: u16 = 0x40;

pub const MODES: &[(u32, u32)] = &[
    (640, 480),
    (800, 600),
    (1024, 768),
    (1280, 720),
    (1280, 800),
    (1280, 1024),
    (1366, 768),
    (1440, 900),
    (1600, 900),
    (1680, 1050),
    (1920, 1080),
    (1920, 1200),
    (2560, 1440),
];

fn write(reg: u16, value: u16) {
    unsafe {
        outw(INDEX, reg);
        outw(DATA, value);
    }
}

fn read(reg: u16) -> u16 {
    unsafe {
        outw(INDEX, reg);
        inw(DATA)
    }
}

pub fn detect() -> Option<Device> {
    let device = pci::find_id(0x1234, 0x1111).or_else(|| pci::find_id(0x80ee, 0xbeef))?;
    let id = read(REG_ID);
    (0xb0c0..=0xb0c5).contains(&id).then_some(device)
}

/// Yeni kipi kurar ve ekranı yeni framebuffer'a taşır.
pub fn set_mode(device: &Device, width: u32, height: u32) -> Result<(), &'static str> {
    let lfb = device.bar(0).ok_or("no linear framebuffer BAR")?;
    let vram = 16 * 1024 * 1024u64;
    if width as u64 * height as u64 * 4 > vram {
        return Err("mode does not fit in video memory");
    }
    write(REG_ENABLE, 0);
    write(REG_XRES, width as u16);
    write(REG_YRES, height as u16);
    write(REG_BPP, 32);
    write(REG_VIRT_WIDTH, width as u16);
    write(REG_VIRT_HEIGHT, height as u16);
    write(REG_X_OFFSET, 0);
    write(REG_Y_OFFSET, 0);
    write(REG_ENABLE, ENABLED | LFB);
    if read(REG_XRES) != width as u16 || read(REG_YRES) != height as u16 {
        return Err("adapter rejected the mode");
    }
    let pitch = width as usize * 4;
    let size = pitch * height as usize;
    let base = display::map_write_combining(lfb, size)
        .or_else(|| {
            crate::memory::paging::map_physical(lfb, size as u64, crate::memory::paging::NO_CACHE)
                .ok()
        })
        .ok_or("cannot map the framebuffer")?;
    display::switch_output(
        Framebuffer::xrgb(base, width as usize, height as usize, pitch),
        None,
        None,
    );
    Ok(())
}
