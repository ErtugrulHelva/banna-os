//! Sıvı cam malzemesi: arkadaki görüntünün bulanık hâli, kenarlarda kırılma (mercek),
//! üst kenarda ışık çizgisi ve hafif ton.
//!
//! Bulanık arka plan (`backdrop`) duvar kağıdından bir kez üretilir; cam yüzeyler her karede
//! yalnızca onu örnekler, bu yüzden bulanıklık kare başına maliyet getirmez.

use super::{Argb, Canvas, Rect, Surface, fast, isqrt};

/// Cam görünümü.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Glass {
    /// Ton rengi; alfa, tonun gücüdür (0 = saf cam).
    pub tint: Argb,
    /// Kenar ışığının gücü (0-255).
    pub rim: u8,
    /// Kenardaki en büyük kırılma kayması (piksel).
    pub refraction: i32,
    /// Kırılmanın etkili olduğu kenar kuşağı (piksel).
    pub edge: i32,
    /// Üst yarıdaki parlaklık (0-255).
    pub sheen: u8,
}

/// Kutu bulanıklığı (yatay + dikey, kayan toplamla).
fn box_blur(pixels: &mut [u32], width: usize, height: usize, radius: usize) {
    if radius == 0 || width == 0 || height == 0 {
        return;
    }
    let mut line = alloc::vec![0u32; width.max(height)];
    let pass = |pixels: &mut [u32], line: &mut [u32], len: usize, at: &dyn Fn(usize) -> usize| {
        let div = (2 * radius + 1) as u32;
        let get = |pixels: &[u32], i: isize| pixels[at(i.clamp(0, len as isize - 1) as usize)];
        let (mut r, mut g, mut b) = (0u32, 0u32, 0u32);
        for i in -(radius as isize)..=radius as isize {
            let c = get(pixels, i);
            r += c >> 16 & 0xff;
            g += c >> 8 & 0xff;
            b += c & 0xff;
        }
        for (i, out) in line.iter_mut().enumerate().take(len) {
            *out = (r / div) << 16 | (g / div) << 8 | (b / div);
            let add = get(pixels, i as isize + radius as isize + 1);
            let sub = get(pixels, i as isize - radius as isize);
            r = r + (add >> 16 & 0xff) - (sub >> 16 & 0xff);
            g = g + (add >> 8 & 0xff) - (sub >> 8 & 0xff);
            b = b + (add & 0xff) - (sub & 0xff);
        }
        for (i, &c) in line.iter().enumerate().take(len) {
            pixels[at(i)] = c;
        }
    };
    for y in 0..height {
        pass(pixels, &mut line, width, &|x| y * width + x);
    }
    for x in 0..width {
        pass(pixels, &mut line, height, &|y| y * width + x);
    }
}

/// Duvar kağıdından cam arka planı: 1/4 çözünürlükte üç kez bulanıklaştırılır, renk
/// doygunluğu artırılır ve tam çözünürlüğe çift doğrusal olarak büyütülür.
pub fn backdrop(src: &Surface, blur_px: i32) -> Surface {
    const SCALE: i32 = 4;
    let (w, h) = (src.width / SCALE + 1, src.height / SCALE + 1);
    let mut small = alloc::vec![0u32; (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = (
                (x * SCALE).min(src.width - 1),
                (y * SCALE).min(src.height - 1),
            );
            small[(y * w + x) as usize] = src.pixels[(sy * src.width + sx) as usize] & 0xff_ffff;
        }
    }
    let radius = (blur_px / SCALE / 2).max(1) as usize;
    for _ in 0..3 {
        box_blur(&mut small, w as usize, h as usize, radius);
    }
    for c in small.iter_mut() {
        let (r, g, b) = (
            (*c >> 16 & 0xff) as i32,
            (*c >> 8 & 0xff) as i32,
            (*c & 0xff) as i32,
        );
        let gray = (r * 77 + g * 150 + b * 29) >> 8;
        let sat = |v: i32| (gray + (v - gray) * 3 / 2).clamp(0, 255) as u32;
        *c = sat(r) << 16 | sat(g) << 8 | sat(b);
    }
    let mut out = Surface::new(src.width, src.height, 0);
    for y in 0..src.height {
        let fy = (y * 256 / SCALE) as u32;
        let (y0, ty) = ((fy >> 8) as i32, fy & 0xff);
        let y1 = (y0 + 1).min(h - 1);
        for x in 0..src.width {
            let fx = (x * 256 / SCALE) as u32;
            let (x0, tx) = ((fx >> 8) as i32, fx & 0xff);
            let x1 = (x0 + 1).min(w - 1);
            let p = |xx: i32, yy: i32| small[(yy * w + xx) as usize];
            let lerp = |a: u32, b: u32, t: u32| {
                let rb = ((a & 0xff00ff) * (256 - t) + (b & 0xff00ff) * t) >> 8 & 0xff00ff;
                let g = ((a & 0xff00) * (256 - t) + (b & 0xff00) * t) >> 8 & 0xff00;
                rb | g
            };
            let top = lerp(p(x0, y0), p(x1, y0), tx);
            let bottom = lerp(p(x0, y1), p(x1, y1), tx);
            out.pixels[(y * src.width + x) as usize] = lerp(top, bottom, ty);
        }
    }
    out
}

#[inline]
fn mix(a: u32, b: u32, t: u32) -> u32 {
    let rb = ((a & 0xff00ff) * (256 - t) + (b & 0xff00ff) * t) >> 8 & 0xff00ff;
    let g = ((a & 0xff00) * (256 - t) + (b & 0xff00) * t) >> 8 & 0xff00;
    rb | g
}

impl Canvas<'_> {
    /// `rect` içinde yuvarlak köşeli cam çizer; `backdrop` ekranla aynı boyuttadır ve
    /// tuvalin (x, y) noktası için `(x + origin.0, y + origin.1)` örneklenir.
    pub fn glass(
        &mut self,
        rect: Rect,
        radius: i32,
        backdrop: &Surface,
        origin: (i32, i32),
        style: &Glass,
    ) {
        self.glass_impl(rect, radius, backdrop, origin, style, false);
    }

    fn glass_impl(
        &mut self,
        rect: Rect,
        radius: i32,
        backdrop: &Surface,
        origin: (i32, i32),
        style: &Glass,
        layer: bool,
    ) {
        let r = rect.intersect(&self.clip);
        if r.is_empty() {
            return;
        }
        let radius = radius.min(rect.w / 2).min(rect.h / 2).max(0);
        let (cx0, cx1) = ((rect.x + radius) * 16, (rect.right() - radius) * 16);
        let (cy0, cy1) = ((rect.y + radius) * 16, (rect.bottom() - radius) * 16);
        let edge16 = (style.edge * 16).max(16);
        let tint_a = style.tint >> 24;
        let tint = style.tint & 0xff_ffff;
        let (bw, bh) = (backdrop.width, backdrop.height);
        let sheen_rows = (rect.h * 2 / 5).max(1);
        for y in r.y..r.bottom() {
            let fy = y * 16 + 8;
            let qy = if fy < cy0 {
                -(cy0 - fy)
            } else if fy > cy1 {
                fy - cy1
            } else {
                0
            };
            let sheen = if y - rect.y < sheen_rows {
                style.sheen as u32 * (sheen_rows - (y - rect.y)) as u32 / sheen_rows as u32
            } else {
                0
            };
            // Kenarlardan uzak iç kısım düz camdır: satır kopyalanıp tonlanır (hızlı yol).
            // Yavaş yol yalnızca kenar kuşağında (kırılma + ışık) ve köşe kutularında gerekir.
            let band = style.edge.max(3) + 1;
            let from_edge = (y - rect.y).min(rect.bottom() - 1 - y);
            let side = if from_edge < radius {
                radius.max(band)
            } else {
                band
            };
            let (ix0, ix1) = if from_edge >= band {
                (
                    (rect.x + side).clamp(r.x, r.right()),
                    (rect.right() - side).clamp(r.x, r.right()),
                )
            } else {
                (r.right(), r.right())
            };
            let pixel = |x: i32, dst: u32| -> u32 {
                let fx = x * 16 + 8;
                let qx = if fx < cx0 {
                    -(cx0 - fx)
                } else if fx > cx1 {
                    fx - cx1
                } else {
                    0
                };
                // İşaretli uzaklık (içeride pozitif, 1/16 piksel) ve dışa doğru normal (1/256).
                let (d, nx, ny) = if qx != 0 && qy != 0 {
                    let len = isqrt((qx * qx + qy * qy) as u32).max(1) as i32;
                    (radius * 16 - len, qx * 256 / len, qy * 256 / len)
                } else {
                    let left = fx - rect.x * 16;
                    let right = rect.right() * 16 - fx;
                    let top = fy - rect.y * 16;
                    let bottom = rect.bottom() * 16 - fy;
                    let m = left.min(right).min(top).min(bottom);
                    if m == top {
                        (m, 0, -256)
                    } else if m == bottom {
                        (m, 0, 256)
                    } else if m == left {
                        (m, -256, 0)
                    } else {
                        (m, 256, 0)
                    }
                };
                if d <= -8 {
                    return if layer { 0 } else { dst };
                }
                // Kenar kuşağında dışarıdaki görüntü içeri doğru kırılır.
                let (mut sx, mut sy) = (x + origin.0, y + origin.1);
                if d < edge16 {
                    let t = ((edge16 - d.max(0)) * 256 / edge16) as u32;
                    let shift = ((style.refraction as u32 * t * t) >> 16) as i32;
                    sx += nx * shift / 256;
                    sy += ny * shift / 256;
                }
                let mut c =
                    backdrop.pixels[(sy.clamp(0, bh - 1) * bw + sx.clamp(0, bw - 1)) as usize];
                if tint_a > 0 {
                    c = mix(c, tint, tint_a);
                }
                if sheen > 0 {
                    c = mix(c, 0xff_ffff, sheen);
                }
                // Işık sol üstten: üst/sol kenar parlak, alt/sağ kenarda hafif yansıma.
                if d < 40 && style.rim > 0 {
                    let lit = (-ny * 3 - nx).max(0) / 4 + (ny + nx).max(0) / 6;
                    let fade = (40 - d.max(0)) as u32 * 256 / 40;
                    let s = (style.rim as u32 * lit as u32 / 256 * fade / 256).min(255);
                    c = mix(c, 0xff_ffff, s);
                }
                if layer {
                    // Katman: kenar yumuşatması saydamlıkta (0 = şeklin dışı).
                    c | (((d + 8) * 16).clamp(0, 255) as u32) << 24
                } else if d >= 8 {
                    c
                } else {
                    mix(dst & 0xff_ffff, c, ((d + 8) * 16) as u32)
                }
            };
            let row = self.span(y, r.x, r.right());
            for x in (r.x..ix0).chain(ix1..r.right()) {
                let px = &mut row[(x - r.x) as usize];
                *px = pixel(x, *px);
            }
            if ix1 > ix0 {
                let middle = &mut row[(ix0 - r.x) as usize..(ix1 - r.x) as usize];
                let sy = (y + origin.1).clamp(0, bh - 1);
                let (sx0, sx1) = (ix0 + origin.0, ix1 + origin.0);
                if sx0 >= 0 && sx1 <= bw {
                    let at = (sy * bw) as usize;
                    middle.copy_from_slice(&backdrop.pixels[at + sx0 as usize..at + sx1 as usize]);
                } else {
                    for (i, px) in middle.iter_mut().enumerate() {
                        let sx = (sx0 + i as i32).clamp(0, bw - 1);
                        *px = backdrop.pixels[(sy * bw + sx) as usize];
                    }
                }
                if tint_a > 0 {
                    fast::blend_const(middle, style.tint);
                }
                if sheen > 0 {
                    fast::blend_const(middle, (sheen.min(255) << 24) | 0xff_ffff);
                }
                if layer {
                    for p in middle.iter_mut() {
                        *p |= 0xff00_0000;
                    }
                }
            }
        }
    }
}

/// Camı `rect.w`×`rect.h` boyutunda saydamlık kanallı (0xAARRGGBB) bir katmana çizer:
/// sonuç `blend_image` ile bindirilerek aynı cam her karede yeniden hesaplanmadan çizilir.
/// `rect` ekran konumudur (arka plan oradan örneklenir).
pub fn layer(rect: Rect, radius: i32, backdrop: &Surface, style: &Glass) -> Surface {
    let mut surface = Surface::new(rect.w, rect.h, 0);
    surface.canvas().glass_impl(
        Rect::new(0, 0, rect.w, rect.h),
        radius,
        backdrop,
        (rect.x, rect.y),
        style,
        true,
    );
    surface
}
