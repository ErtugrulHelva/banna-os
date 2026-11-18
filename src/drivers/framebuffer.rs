//! Doğrusal 32 bpp framebuffer: ham piksel yazımı ve satır kopyalama.

use core::ptr::{copy_nonoverlapping, write_volatile};

use super::font;
use crate::boot::limine;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Color(pub u8, pub u8, pub u8);

impl Color {
    pub const fn argb(self) -> u32 {
        0xff00_0000 | (self.0 as u32) << 16 | (self.1 as u32) << 8 | self.2 as u32
    }
}

#[derive(Clone)]
pub struct Framebuffer {
    base: *mut u8,
    pub width: usize,
    pub height: usize,
    pub pitch: usize,
    red_shift: u8,
    green_shift: u8,
    blue_shift: u8,
}

unsafe impl Send for Framebuffer {}
// Yazmalar `&mut self` ile yapılır; paralel kopyalamada her çekirdek kendi kopyasıyla
// framebuffer'ın ayrık satırlarına yazar.
unsafe impl Sync for Framebuffer {}

impl Framebuffer {
    pub fn from_limine(info: &limine::Framebuffer) -> Option<Self> {
        if info.bpp != 32 || info.memory_model != 1 || info.address.is_null() {
            return None;
        }
        Some(Self {
            base: info.address,
            width: info.width as usize,
            height: info.height as usize,
            pitch: info.pitch as usize,
            red_shift: info.red_mask_shift,
            green_shift: info.green_mask_shift,
            blue_shift: info.blue_mask_shift,
        })
    }

    /// 0x00RRGGBB (XRGB8888) düzeninde bir doğrusal framebuffer.
    pub fn xrgb(base: *mut u8, width: usize, height: usize, pitch: usize) -> Self {
        Self {
            base,
            width,
            height,
            pitch,
            red_shift: 16,
            green_shift: 8,
            blue_shift: 0,
        }
    }

    pub fn base(&self) -> *mut u8 {
        self.base
    }

    pub fn size_bytes(&self) -> usize {
        self.pitch * self.height
    }

    /// Aynı belleğe başka bir sanal adresten (ör. write-combining eşleme) erişir.
    pub fn rebase(&mut self, base: *mut u8) {
        self.base = base;
    }

    fn is_xrgb(&self) -> bool {
        self.red_shift == 16 && self.green_shift == 8 && self.blue_shift == 0
    }

    #[inline]
    fn encode(&self, argb: u32) -> u32 {
        if self.is_xrgb() {
            return argb & 0x00ff_ffff;
        }
        ((argb >> 16) & 0xff) << self.red_shift
            | ((argb >> 8) & 0xff) << self.green_shift
            | (argb & 0xff) << self.blue_shift
    }

    /// RAM'deki 0x00RRGGBB satırını ekrana yazar.
    pub fn write_row(&mut self, x: usize, y: usize, pixels: &[u32]) {
        if y >= self.height || x >= self.width {
            return;
        }
        let count = pixels.len().min(self.width - x);
        let dst = unsafe { self.base.add(y * self.pitch + x * 4) as *mut u32 };
        if self.is_xrgb() {
            unsafe { copy_nonoverlapping(pixels.as_ptr(), dst, count) };
        } else {
            for (i, &px) in pixels[..count].iter().enumerate() {
                unsafe { write_volatile(dst.add(i), self.encode(px)) };
            }
        }
    }

    pub fn fill_rect(&mut self, x: usize, y: usize, w: usize, h: usize, color: Color) {
        let value = self.encode(color.argb());
        for row in y..(y + h).min(self.height) {
            let line = unsafe { self.base.add(row * self.pitch) as *mut u32 };
            for col in x..(x + w).min(self.width) {
                unsafe { write_volatile(line.add(col), value) };
            }
        }
    }

    pub fn draw_glyph(&mut self, x: usize, y: usize, byte: u8, fg: Color, bg: Color, scale: usize) {
        let glyph = font::glyph(byte);
        let fg = self.encode(fg.argb());
        let bg = self.encode(bg.argb());
        for (row, bits) in glyph.iter().enumerate() {
            for sy in 0..scale {
                let py = y + row * scale + sy;
                if py >= self.height {
                    return;
                }
                let line = unsafe { self.base.add(py * self.pitch) as *mut u32 };
                for col in 0..font::WIDTH {
                    let value = if bits & (0x80 >> col) != 0 { fg } else { bg };
                    for sx in 0..scale {
                        let px = x + col * scale + sx;
                        if px < self.width {
                            unsafe { write_volatile(line.add(px), value) };
                        }
                    }
                }
            }
        }
    }
}
