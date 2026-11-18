//! 2B grafik: dikdörtgenler, yüzeyler (RAM'de 32 bpp) ve kırpmalı çizim.

// Uygulamalarla ortak; bazı yardımcıları yalnızca kullanıcı alanı kullanır.
#[allow(dead_code)]
mod canvas;
pub mod cursor;
pub mod display;
pub mod fast;
pub mod glass;
pub mod parallel;
pub mod png;
pub mod splash;
pub mod text;

pub use canvas::*;
pub use glass::Glass;

use crate::drivers::font;

/// Konsolun bitmap fontu (yalnızca çekirdek).
impl Canvas<'_> {
    pub fn glyph(&mut self, x: i32, y: i32, byte: u8, color: Argb) {
        let cell = Rect::new(x, y, font::WIDTH as i32, font::HEIGHT as i32);
        let r = cell.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let glyph = font::glyph(byte);
        let opaque = color >> 24 == 255;
        for py in r.y..r.bottom() {
            let bits = glyph[(py - y) as usize];
            if bits == 0 {
                continue;
            }
            let row = self.span(py, r.x, r.right());
            for (i, p) in row.iter_mut().enumerate() {
                let px = r.x + i as i32;
                if bits & (0x80 >> (px - x)) != 0 {
                    *p = if opaque {
                        color & 0x00ff_ffff
                    } else {
                        blend(*p, color)
                    };
                }
            }
        }
    }

    /// Bitmap glifi tam sayı katında büyüterek çizer (yüksek çözünürlüklü terminal).
    pub fn glyph_scaled(&mut self, x: i32, y: i32, byte: u8, color: Argb, scale: i32) {
        if scale <= 1 {
            self.glyph(x, y, byte, color);
            return;
        }
        let glyph = font::glyph(byte);
        for (row, bits) in glyph.iter().enumerate() {
            if *bits == 0 {
                continue;
            }
            for col in 0..font::WIDTH as i32 {
                if bits & (0x80 >> col) != 0 {
                    self.fill(
                        Rect::new(x + col * scale, y + row as i32 * scale, scale, scale),
                        color,
                    );
                }
            }
        }
    }
}
