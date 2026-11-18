//! 2B çizim çekirdeği: renkler, dikdörtgenler, yüzeyler ve kırpmalı tuval.
//!
//! Çekirdeğe bağımlı değildir: kullanıcı kütüphanesi (`banna::gfx`) aynı dosyayı kullanır.

use alloc::vec;
use alloc::vec::Vec;

use super::fast;

/// 0xAARRGGBB
pub type Argb = u32;

pub const fn rgb(r: u8, g: u8, b: u8) -> Argb {
    0xff00_0000 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

pub const fn with_alpha(color: Argb, alpha: u8) -> Argb {
    (color & 0x00ff_ffff) | (alpha as u32) << 24
}

#[inline]
pub fn blend(dst: u32, src: Argb) -> u32 {
    let a = src >> 24;
    if a == 255 {
        return src & 0x00ff_ffff;
    }
    if a == 0 {
        return dst;
    }
    let inv = 255 - a;
    let rb = ((src & 0x00ff_00ff) * a + (dst & 0x00ff_00ff) * inv) >> 8;
    let g = ((src & 0x0000_ff00) * a + (dst & 0x0000_ff00) * inv) >> 8;
    (rb & 0x00ff_00ff) | (g & 0x0000_ff00)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }

    pub fn is_empty(&self) -> bool {
        self.w <= 0 || self.h <= 0
    }

    pub fn right(&self) -> i32 {
        self.x + self.w
    }

    pub fn bottom(&self) -> i32 {
        self.y + self.h
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        x >= self.x && y >= self.y && x < self.right() && y < self.bottom()
    }

    pub fn intersect(&self, other: &Rect) -> Rect {
        let x = self.x.max(other.x);
        let y = self.y.max(other.y);
        let r = self.right().min(other.right());
        let b = self.bottom().min(other.bottom());
        Rect::new(x, y, (r - x).max(0), (b - y).max(0))
    }

    pub fn union(&self, other: &Rect) -> Rect {
        if self.is_empty() {
            return *other;
        }
        if other.is_empty() {
            return *self;
        }
        let x = self.x.min(other.x);
        let y = self.y.min(other.y);
        let r = self.right().max(other.right());
        let b = self.bottom().max(other.bottom());
        Rect::new(x, y, r - x, b - y)
    }

    pub fn offset(&self, dx: i32, dy: i32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w, self.h)
    }

    pub fn inset(&self, by: i32) -> Rect {
        Rect::new(self.x + by, self.y + by, self.w - 2 * by, self.h - 2 * by)
    }

    pub fn area(&self) -> i64 {
        if self.is_empty() {
            0
        } else {
            self.w as i64 * self.h as i64
        }
    }
}

/// Bölgeyi birkaç dikdörtgen olarak tutar; çok parçalanırsa birleştirir.
#[derive(Clone, Default)]
pub struct Damage {
    rects: Vec<Rect>,
}

impl Damage {
    const MAX_RECTS: usize = 16;

    pub fn add(&mut self, rect: Rect) {
        if rect.is_empty() {
            return;
        }
        for existing in self.rects.iter_mut() {
            let merged = existing.union(&rect);
            if merged.area() <= existing.area() + rect.area() + rect.area() / 2 {
                *existing = merged;
                return;
            }
        }
        if self.rects.len() >= Self::MAX_RECTS {
            let all = self.rects.iter().fold(rect, |acc, r| acc.union(r));
            self.rects.clear();
            self.rects.push(all);
        } else {
            self.rects.push(rect);
        }
    }

    pub fn take(&mut self) -> Vec<Rect> {
        core::mem::take(&mut self.rects)
    }

    pub fn is_empty(&self) -> bool {
        self.rects.is_empty()
    }
}

/// Dikdörtgenleri `band` satırlık ayrık parçalara böler: her şeritte kesişen yatay
/// aralıklar birleştirilir, böylece parçalar birbirine değmez (paralel çizim için).
pub fn split_bands(rects: &[Rect], band: i32) -> Vec<Rect> {
    let Some(top) = rects.iter().map(|r| r.y).min() else {
        return Vec::new();
    };
    let bottom = rects.iter().map(|r| r.bottom()).max().unwrap_or(top);
    let mut out = Vec::new();
    let mut y = top;
    while y < bottom {
        let strip = Rect::new(i32::MIN / 2, y, i32::MAX, band.min(bottom - y));
        let mut spans: Vec<(i32, i32, i32, i32)> = rects
            .iter()
            .map(|r| r.intersect(&strip))
            .filter(|r| !r.is_empty())
            .map(|r| (r.x, r.right(), r.y, r.bottom()))
            .collect();
        spans.sort_unstable();
        let mut current: Option<(i32, i32, i32, i32)> = None;
        for (x0, x1, y0, y1) in spans {
            match current {
                Some((cx0, cx1, cy0, cy1)) if x0 <= cx1 => {
                    current = Some((cx0, cx1.max(x1), cy0.min(y0), cy1.max(y1)));
                }
                Some((cx0, cx1, cy0, cy1)) => {
                    out.push(Rect::new(cx0, cy0, cx1 - cx0, cy1 - cy0));
                    current = Some((x0, x1, y0, y1));
                }
                None => current = Some((x0, x1, y0, y1)),
            }
        }
        if let Some((cx0, cx1, cy0, cy1)) = current {
            out.push(Rect::new(cx0, cy0, cx1 - cx0, cy1 - cy0));
        }
        y += band;
    }
    out
}

pub struct Surface {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

impl Surface {
    pub fn new(width: i32, height: i32, fill: u32) -> Self {
        Self {
            width,
            height,
            pixels: vec![fill; (width.max(0) * height.max(0)) as usize],
        }
    }

    pub fn bounds(&self) -> Rect {
        Rect::new(0, 0, self.width, self.height)
    }

    pub fn canvas(&mut self) -> Canvas<'_> {
        let clip = self.bounds();
        Canvas {
            pixels: self.pixels.as_mut_ptr(),
            width: self.width,
            height: self.height,
            clip,
            _owner: core::marker::PhantomData,
        }
    }

    pub fn row(&self, y: i32) -> &[u32] {
        let start = (y * self.width) as usize;
        &self.pixels[start..start + self.width as usize]
    }
}

/// Kırpma dikdörtgeniyle bir yüzeye çizim. Tuval yalnızca `clip` içine yazar; bu yüzden
/// aynı yüzeyin ayrık bölgelerine verilen tuvaller farklı çekirdeklerde birlikte çizebilir.
pub struct Canvas<'a> {
    pixels: *mut u32,
    pub width: i32,
    pub height: i32,
    pub clip: Rect,
    _owner: core::marker::PhantomData<&'a mut [u32]>,
}

unsafe impl Send for Canvas<'_> {}

impl Canvas<'_> {
    /// Aynı piksel arabelleğine daha kısa ömürlü ikinci bir görünüm.
    pub fn reborrow(&mut self) -> Canvas<'_> {
        Canvas {
            pixels: self.pixels,
            width: self.width,
            height: self.height,
            clip: self.clip,
            _owner: core::marker::PhantomData,
        }
    }
}

impl Canvas<'_> {
    /// # Safety
    /// `pixels` `width * height` piksellik bir yüzeyi göstermeli ve tuval yaşadıkça
    /// `clip` bölgesine başka kimse yazmamalıdır.
    pub unsafe fn from_raw(pixels: *mut u32, width: i32, height: i32, clip: Rect) -> Self {
        Self {
            pixels,
            width,
            height,
            clip: clip.intersect(&Rect::new(0, 0, width, height)),
            _owner: core::marker::PhantomData,
        }
    }

    /// `y` satırının [x0, x1) aralığı; çağıranlar aralığı kırpma içinde tutar.
    pub fn span(&mut self, y: i32, x0: i32, x1: i32) -> &mut [u32] {
        debug_assert!(y >= 0 && y < self.height && x0 >= 0 && x1 <= self.width && x0 <= x1);
        unsafe {
            core::slice::from_raw_parts_mut(
                self.pixels.add((y * self.width + x0) as usize),
                (x1 - x0) as usize,
            )
        }
    }

    pub fn fill(&mut self, rect: Rect, color: Argb) {
        let r = rect.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let opaque = color >> 24 == 255;
        for y in r.y..r.bottom() {
            let row = self.span(y, r.x, r.right());
            if opaque {
                row.fill(color & 0x00ff_ffff);
            } else {
                fast::blend_const(row, color);
            }
        }
    }

    pub fn fill_rounded(&mut self, rect: Rect, radius: i32, color: Argb) {
        let radius = radius.min(rect.w / 2).min(rect.h / 2).max(0);
        if radius == 0 {
            self.fill(rect, color);
            return;
        }
        let first = (self.clip.y - rect.y).clamp(0, rect.h);
        let last = (self.clip.bottom() - rect.y).clamp(0, rect.h);
        for dy in first..last {
            let inset = if dy < radius {
                corner_inset(radius, radius - dy - 1)
            } else if dy >= rect.h - radius {
                corner_inset(radius, dy - (rect.h - radius))
            } else {
                0
            };
            self.fill(
                Rect::new(rect.x + inset, rect.y + dy, rect.w - 2 * inset, 1),
                color,
            );
        }
    }

    pub fn gradient_v(&mut self, rect: Rect, top: Argb, bottom: Argb) {
        let r = rect.intersect(&self.clip);
        if r.is_empty() || rect.h <= 0 {
            return;
        }
        for y in r.y..r.bottom() {
            let t = ((y - rect.y) * 256 / rect.h.max(1)) as u32;
            let mix = |shift: u32| {
                let a = (top >> shift) & 0xff;
                let b = (bottom >> shift) & 0xff;
                ((a * (256 - t) + b * t) >> 8) & 0xff
            };
            let color = 0xff00_0000 | mix(16) << 16 | mix(8) << 8 | mix(0);
            self.fill(Rect::new(r.x, y, r.w, 1), color);
        }
    }

    /// Yumuşak gölge: yuvarlak köşeli dikdörtgenin dışında, uzaklıkla karesel azalan
    /// saydamlık (içi zaten opak yüzeyle örtülür). Gölge biraz aşağı kaydırılır.
    pub fn shadow(&mut self, rect: Rect, radius: i32, spread: i32, strength: u8) {
        if spread <= 0 || strength == 0 {
            return;
        }
        let body = rect.offset(0, spread / 3);
        let radius = radius.min(body.w / 2).min(body.h / 2).max(0);
        let area = body.inset(-spread).intersect(&self.clip);
        if area.is_empty() {
            return;
        }
        let (cx0, cx1) = (body.x + radius, body.right() - radius);
        let (cy0, cy1) = (body.y + radius, body.bottom() - radius);
        let max = (spread * 16) as u32;
        // Düz dikey kenarlı satırlarda (gövde ve pencere köşelerinin dışında) yan şeritlerin
        // saydamlığı yalnızca x'e bağlıdır: bir kez hesaplanır.
        let straight = (cy0.max(rect.y + radius), cy1.min(rect.bottom() - radius));
        let side_alpha: Vec<u8> = (area.x..area.right())
            .map(|x| {
                let dx = if x < cx0 {
                    cx0 - x
                } else if x >= cx1 {
                    x - cx1 + 1
                } else {
                    return 0;
                };
                let d = ((dx * 16) as u32).saturating_sub(radius as u32 * 16);
                if d >= max || (d == 0 && x >= rect.x && x < rect.right()) {
                    return 0;
                }
                let k = max - d;
                (strength as u32 * k / max * k / max / 2).min(255) as u8
            })
            .collect();
        for y in area.y..area.bottom() {
            if y >= straight.0 && y < straight.1 {
                let (mid0, mid1) = (
                    cx0.clamp(area.x, area.right()),
                    cx1.clamp(area.x, area.right()),
                );
                let row = self.span(y, area.x, area.right());
                for x in (area.x..mid0).chain(mid1..area.right()) {
                    let i = (x - area.x) as usize;
                    let a = side_alpha[i] as u32;
                    if a > 0 {
                        row[i] = blend(row[i], a << 24);
                    }
                }
                continue;
            }
            let dy = if y < cy0 {
                cy0 - y
            } else if y >= cy1 {
                y - cy1 + 1
            } else {
                0
            };
            let pixel = |x: i32, p: u32| -> u32 {
                let dx = if x < cx0 {
                    cx0 - x
                } else if x >= cx1 {
                    x - cx1 + 1
                } else {
                    0
                };
                let len = if dx > 0 && dy > 0 {
                    isqrt(((dx * dx + dy * dy) * 256) as u32)
                } else {
                    ((dx + dy) * 16) as u32
                };
                let d = len.saturating_sub(radius as u32 * 16);
                if d >= max || (d == 0 && rounded_contains(&rect, radius, x, y)) {
                    return p;
                }
                let k = max - d;
                let alpha = strength as u32 * k / max * k / max / 2;
                if alpha > 0 {
                    blend(p, alpha.min(255) << 24)
                } else {
                    p
                }
            };
            // Düz kenar boyunca (köşe sütunları arası) gölge satırda sabittir.
            let (mid0, mid1) = (
                cx0.clamp(area.x, area.right()),
                cx1.clamp(area.x, area.right()),
            );
            let row = self.span(y, area.x, area.right());
            for x in (area.x..mid0).chain(mid1..area.right()) {
                let p = &mut row[(x - area.x) as usize];
                *p = pixel(x, *p);
            }
            if mid1 <= mid0 {
                continue;
            }
            let middle = &mut row[(mid0 - area.x) as usize..(mid1 - area.x) as usize];
            let d = (dy * 16) as u32;
            let inside_rect = y >= rect.y + radius && y < rect.bottom() - radius;
            if dy == 0 && inside_rect {
                continue;
            }
            if d > 0 && d >= max + radius as u32 * 16 {
                continue;
            }
            let d = d.saturating_sub(radius as u32 * 16);
            if d == 0 {
                // Köşe yarıçapı içindeki satır: orta bölüm ya pencerenin içindedir ya da
                // tam güçte (strength / 2) sabit gölgedir.
                if y < rect.y || y >= rect.bottom() {
                    fast::blend_const(middle, (strength as u32 / 2) << 24);
                }
                continue;
            }
            let k = max - d;
            let alpha = strength as u32 * k / max * k / max / 2;
            if alpha > 0 {
                fast::blend_const(middle, alpha.min(255) << 24);
            }
        }
    }

    pub fn blit(&mut self, src: &Surface, src_rect: Rect, x: i32, y: i32) {
        let dst = Rect::new(x, y, src_rect.w, src_rect.h).intersect(&self.clip);
        if dst.is_empty() {
            return;
        }
        let sx = src_rect.x + (dst.x - x);
        let sy = src_rect.y + (dst.y - y);
        for row in 0..dst.h {
            let s = ((sy + row) * src.width + sx) as usize;
            self.span(dst.y + row, dst.x, dst.right())
                .copy_from_slice(&src.pixels[s..s + dst.w as usize]);
        }
    }

    /// Yüzeyi alt köşeleri `radius` yarıçapla kırpılmış olarak kopyalar (pencere içeriği:
    /// üst kenarı başlık çubuğuna bitişik, alt köşeleri pencere çerçevesiyle aynı eğride).
    pub fn blit_round_bottom(&mut self, src: &Surface, x: i32, y: i32, radius: i32) {
        let radius = radius.min(src.width / 2).min(src.height).max(0);
        let full = Rect::new(x, y, src.width, src.height - radius);
        self.blit(src, Rect::new(0, 0, src.width, full.h), x, y);
        for dy in (src.height - radius)..src.height {
            let inset = corner_inset(radius, dy - (src.height - radius));
            let row = Rect::new(x + inset, y + dy, src.width - 2 * inset, 1);
            self.blit(src, row.offset(-x, -y), row.x, row.y);
        }
    }

    /// 0xAARRGGBB pikselli görüntüyü kendi saydamlığıyla bindirir.
    pub fn blend_image(&mut self, pixels: &[u32], width: i32, height: i32, x: i32, y: i32) {
        let r = Rect::new(x, y, width, height).intersect(&self.clip);
        // Yatayda kesişmeyen görüntüde `r.w == 0` olur ama `r.x` görüntünün dışında kalır;
        // satırlar yine dolaşılırsa kaynak dilim indeksi taşar.
        if r.is_empty() {
            return;
        }
        for py in r.y..r.bottom() {
            let src = &pixels[((py - y) * width + (r.x - x)) as usize..][..r.w as usize];
            for (d, &s) in self.span(py, r.x, r.right()).iter_mut().zip(src) {
                match s >> 24 {
                    0 => {}
                    255 => *d = s & 0x00ff_ffff,
                    _ => *d = blend(*d, s),
                }
            }
        }
    }

    /// Yüzeyi yuvarlak köşe maskesiyle ve sabit saydamlıkla (255 = opak) bindirir.
    pub fn blit_masked(&mut self, src: &Surface, x: i32, y: i32, radius: i32, alpha: u8) {
        let frame = Rect::new(x, y, src.width, src.height);
        let r = frame.intersect(&self.clip);
        if r.is_empty() || alpha == 0 {
            return;
        }
        let radius = radius.min(src.width / 2).min(src.height / 2).max(0);
        let a = alpha as u32 + 1;
        for py in r.y..r.bottom() {
            let dy = py - y;
            let inset = if dy < radius {
                corner_inset(radius, radius - dy - 1)
            } else if dy >= src.height - radius {
                corner_inset(radius, dy - (src.height - radius))
            } else {
                0
            };
            let x0 = r.x.max(x + inset);
            let x1 = r.right().min(x + src.width - inset);
            if x0 >= x1 {
                continue;
            }
            let s = (dy * src.width + (x0 - x)) as usize;
            let source = &src.pixels[s..s + (x1 - x0) as usize];
            let row = self.span(py, x0, x1);
            if alpha == 255 {
                row.copy_from_slice(source);
                continue;
            }
            for (d, &sp) in row.iter_mut().zip(source) {
                let dp = *d;
                let rb = ((sp & 0xff00ff) * a + (dp & 0xff00ff) * (256 - a)) >> 8;
                let g = ((sp & 0x00ff00) * a + (dp & 0x00ff00) * (256 - a)) >> 8;
                *d = (rb & 0xff00ff) | (g & 0x00ff00);
            }
        }
    }
}

/// (x, y) yuvarlak köşeli dikdörtgenin içinde mi?
fn rounded_contains(rect: &Rect, radius: i32, x: i32, y: i32) -> bool {
    if !rect.contains(x, y) {
        return false;
    }
    let dx = (rect.x + radius - x)
        .max(x - (rect.right() - radius - 1))
        .max(0);
    let dy = (rect.y + radius - y)
        .max(y - (rect.bottom() - radius - 1))
        .max(0);
    dx * dx + dy * dy <= radius * radius
}

pub fn isqrt(n: u32) -> u32 {
    if n < 2 {
        return n;
    }
    let mut x = 1u32 << (32 - n.leading_zeros()).div_ceil(2);
    loop {
        let y = (x + n / x) / 2;
        if y >= x {
            return x;
        }
        x = y;
    }
}

/// Köşe dairesinin merkezinden `v` satır uzaktaki yatay girinti.
fn corner_inset(radius: i32, v: i32) -> i32 {
    let r2 = radius * radius;
    let mut dx = 0;
    while dx < radius && (radius - dx) * (radius - dx) + v * v > r2 {
        dx += 1;
    }
    dx
}
