//! Kenar yumuşatmalı, orantılı arayüz fontu (Inter, OFL 1.1).
//!
//! Glifler derleme öncesinde `scripts/gen_ui_font.swift` ile 8 bit kaplama
//! haritalarına çevrilir ve çekirdeğe gömülür.

use super::{Argb, Canvas, Rect, fast};

pub struct Font {
    data: &'static [u8],
    pub size: i32,
    pub ascent: i32,
    pub descent: i32,
    count: usize,
}

#[derive(Clone, Copy)]
struct Glyph {
    advance_q6: i32,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    offset: usize,
}

fn u16_at(d: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([d[at], d[at + 1]])
}

fn i16_at(d: &[u8], at: usize) -> i32 {
    i16::from_le_bytes([d[at], d[at + 1]]) as i32
}

fn u32_at(d: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([d[at], d[at + 1], d[at + 2], d[at + 3]])
}

impl Font {
    /// `scripts/gen_ui_font.swift` atlası (uygulamalar kendi atlaslarını gömebilir).
    pub const fn parse(data: &'static [u8]) -> Self {
        Self {
            data,
            size: u16::from_le_bytes([data[4], data[5]]) as i32,
            ascent: i16::from_le_bytes([data[6], data[7]]) as i32,
            descent: i16::from_le_bytes([data[8], data[9]]) as i32,
            count: u16::from_le_bytes([data[12], data[13]]) as usize,
        }
    }

    pub fn line_height(&self) -> i32 {
        self.ascent + self.descent + self.size / 5
    }

    fn glyph(&self, ch: char) -> Option<Glyph> {
        let code = ch as u32;
        let (mut lo, mut hi) = (0, self.count);
        while lo < hi {
            let mid = (lo + hi) / 2;
            let at = 14 + mid * 18;
            let cp = u32_at(self.data, at);
            if cp == code {
                return Some(Glyph {
                    advance_q6: i16_at(self.data, at + 4),
                    left: i16_at(self.data, at + 6),
                    top: i16_at(self.data, at + 8),
                    width: u16_at(self.data, at + 10) as i32,
                    height: u16_at(self.data, at + 12) as i32,
                    offset: u32_at(self.data, at + 14) as usize,
                });
            }
            if cp < code {
                lo = mid + 1;
            } else {
                hi = mid;
            }
        }
        None
    }

    /// Atlas bu karakteri içeriyor mu (yoksa "?" çizilir).
    #[allow(dead_code)]
    pub fn has(&self, ch: char) -> bool {
        self.glyph(ch).is_some()
    }

    fn lookup(&self, ch: char) -> Option<Glyph> {
        self.glyph(ch).or_else(|| self.glyph('?'))
    }

    fn coverage(&self) -> &'static [u8] {
        &self.data[14 + self.count * 18..]
    }

    /// Metnin piksel genişliği.
    pub fn measure(&self, text: &str) -> i32 {
        let q6: i32 = text
            .chars()
            .filter_map(|c| self.lookup(c))
            .map(|g| g.advance_q6)
            .sum();
        (q6 + 32) / 64
    }

    /// `y` metnin üst kenarıdır (taban çizgisi `y + ascent`); bitiş x'ini döndürür.
    pub fn draw(&self, canvas: &mut Canvas, x: i32, y: i32, text: &str, color: Argb) -> i32 {
        let baseline = y + self.ascent;
        let mut pen_q6 = x * 64;
        let coverage = self.coverage();
        let alpha = color >> 24;
        for ch in text.chars() {
            let Some(g) = self.lookup(ch) else { continue };
            let gx = (pen_q6 + 32) / 64 + g.left;
            let gy = baseline - g.top;
            pen_q6 += g.advance_q6;
            let cell = Rect::new(gx, gy, g.width, g.height);
            let r = cell.intersect(&canvas.clip);
            if r.is_empty() {
                continue;
            }
            for py in r.y..r.bottom() {
                let src = g.offset + ((py - gy) * g.width + (r.x - gx)) as usize;
                let row = canvas.span(py, r.x, r.right());
                fast::blend_coverage(row, &coverage[src..src + r.w as usize], color, alpha);
            }
        }
        (pen_q6 + 32) / 64
    }

    /// Metni verilen genişliğe sığacak şekilde "…" ile kısaltır.
    pub fn draw_clipped(
        &self,
        canvas: &mut Canvas,
        x: i32,
        y: i32,
        text: &str,
        max_width: i32,
        color: Argb,
    ) -> i32 {
        if self.measure(text) <= max_width {
            return self.draw(canvas, x, y, text, color);
        }
        let ellipsis = self.measure("…");
        let mut end = 0;
        for (i, _) in text.char_indices() {
            if self.measure(&text[..i]) + ellipsis > max_width {
                break;
            }
            end = i;
        }
        let x = self.draw(canvas, x, y, &text[..end], color);
        self.draw(canvas, x, y, "…", color)
    }
}

pub static UI13: Font = Font::parse(include_bytes!("fonts/ui13.bin"));
pub static UI15: Font = Font::parse(include_bytes!("fonts/ui15.bin"));
pub static UI15_BOLD: Font = Font::parse(include_bytes!("fonts/ui15b.bin"));
pub static UI17: Font = Font::parse(include_bytes!("fonts/ui17.bin"));
pub static UI20: Font = Font::parse(include_bytes!("fonts/ui20.bin"));
pub static UI20_BOLD: Font = Font::parse(include_bytes!("fonts/ui20b.bin"));
pub static SPLASH: Font = Font::parse(include_bytes!("fonts/splash.bin"));
pub static CLOCK_L: Font = Font::parse(include_bytes!("fonts/clock_l.bin"));
pub static CLOCK_S: Font = Font::parse(include_bytes!("fonts/clock_s.bin"));
pub static HEAD_L: Font = Font::parse(include_bytes!("fonts/head_l.bin"));
pub static HEAD_S: Font = Font::parse(include_bytes!("fonts/head_s.bin"));

/// Ekran yüksekliğine göre seçilen arayüz yazı tipleri.
pub struct UiFonts {
    pub body: &'static Font,
    pub small: &'static Font,
    pub title: &'static Font,
    pub heading: &'static Font,
    pub clock: &'static Font,
}

pub fn ui_fonts(screen_height: i32) -> UiFonts {
    if screen_height >= 1300 {
        UiFonts {
            body: &UI20,
            small: &UI17,
            title: &UI20_BOLD,
            heading: &HEAD_L,
            clock: &CLOCK_L,
        }
    } else {
        UiFonts {
            body: &UI15,
            small: &UI13,
            title: &UI15_BOLD,
            heading: &HEAD_S,
            clock: &CLOCK_S,
        }
    }
}

/// Splash ekranı için büyük "B" (240 px, SemiBold): (genişlik, yükseklik, kaplama).
pub fn splash_logo() -> Option<(i32, i32, &'static [u8])> {
    let g = SPLASH.glyph('B')?;
    let coverage = SPLASH.coverage();
    Some((
        g.width,
        g.height,
        &coverage[g.offset..g.offset + (g.width * g.height) as usize],
    ))
}
