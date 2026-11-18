//! Duvar kağıdı desenleri ve görüntü çözücüleri (BMP, PPM): saf hesaplama.
//!
//! Çekirdeğe bağımlı değildir; Ayarlar uygulaması önizlemeler için aynı dosyayı kullanır.

use alloc::vec::Vec;

use crate::gfx::{Argb, rgb};

pub const PRESETS: [&str; 9] = [
    "Gece",
    "Aurora",
    "Gün batımı",
    "Okyanus",
    "Orman",
    "Grafit",
    "Dağlar",
    "Dalgalar",
    "Akış",
];

/// Varsayılan duvar kağıdı (cam yüzeylerin kırılmasını gösteren renkli şeritler).
pub const DEFAULT_PRESET: u8 = 8;

pub const SOLIDS: [(&str, Argb); 8] = [
    ("Lacivert", rgb(0x12, 0x1a, 0x30)),
    ("Siyah", rgb(0x05, 0x06, 0x08)),
    ("Arduvaz", rgb(0x2b, 0x33, 0x40)),
    ("Patlıcan", rgb(0x2e, 0x1a, 0x3d)),
    ("Çam", rgb(0x13, 0x33, 0x2a)),
    ("Bordo", rgb(0x45, 0x14, 0x22)),
    ("Kum", rgb(0xd9, 0xcf, 0xbd)),
    ("Gökyüzü", rgb(0x9c, 0xc6, 0xea)),
];

struct Blob {
    /// Merkez ve yarıçap ekran boyutunun binde biri cinsinden.
    cx: i32,
    cy: i32,
    r: i32,
    color: Argb,
    strength: u32,
}

struct Style {
    top: Argb,
    bottom: Argb,
    blobs: &'static [Blob],
}

const fn blob(cx: i32, cy: i32, r: i32, color: Argb, strength: u32) -> Blob {
    Blob {
        cx,
        cy,
        r,
        color,
        strength,
    }
}

const STYLES: [Style; 6] = [
    Style {
        top: rgb(0x0d, 0x14, 0x2b),
        bottom: rgb(0x1a, 0x10, 0x2e),
        blobs: &[
            blob(200, 333, 500, rgb(0x2a, 0x6c, 0xd8), 70),
            blob(800, 667, 500, rgb(0x9b, 0x4d, 0xe0), 60),
            blob(500, 1167, 500, rgb(0x00, 0xb3, 0xa4), 45),
        ],
    },
    Style {
        top: rgb(0x03, 0x0c, 0x16),
        bottom: rgb(0x06, 0x1a, 0x1f),
        blobs: &[
            blob(300, 250, 450, rgb(0x1f, 0xe0, 0x9a), 80),
            blob(700, 150, 420, rgb(0x2a, 0x9d, 0xf4), 60),
            blob(550, 450, 380, rgb(0x8f, 0x5c, 0xf7), 40),
        ],
    },
    Style {
        top: rgb(0x2b, 0x12, 0x40),
        bottom: rgb(0xf0, 0x7c, 0x4a),
        blobs: &[
            blob(500, 900, 550, rgb(0xff, 0xc8, 0x5a), 110),
            blob(150, 300, 450, rgb(0xd9, 0x46, 0x8a), 70),
            blob(850, 350, 400, rgb(0x6a, 0x3a, 0xc9), 60),
        ],
    },
    Style {
        top: rgb(0x02, 0x1b, 0x3a),
        bottom: rgb(0x02, 0x4a, 0x6e),
        blobs: &[
            blob(250, 750, 500, rgb(0x00, 0xc6, 0xd7), 70),
            blob(800, 250, 450, rgb(0x2d, 0x6c, 0xf5), 70),
        ],
    },
    Style {
        top: rgb(0x07, 0x18, 0x10),
        bottom: rgb(0x15, 0x30, 0x1c),
        blobs: &[
            blob(300, 400, 450, rgb(0x4c, 0xb0, 0x5a), 60),
            blob(750, 750, 500, rgb(0xb6, 0xd1, 0x4a), 35),
        ],
    },
    Style {
        top: rgb(0x22, 0x25, 0x2b),
        bottom: rgb(0x10, 0x11, 0x14),
        blobs: &[
            blob(500, 0, 600, rgb(0x5a, 0x62, 0x70), 60),
            blob(900, 900, 400, rgb(0x3a, 0x40, 0x4c), 50),
        ],
    },
];

pub fn mix(a: Argb, b: Argb, t: u32) -> Argb {
    let t = t.min(256);
    let ch = |s: u32| ((((a >> s) & 0xff) * (256 - t) + ((b >> s) & 0xff) * t) >> 8) & 0xff;
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

/// Tam sayı sinüs: faz 0..4096 bir tur, sonuç -1024..1024 (Bhaskara yaklaşımı).
fn sin(phase: i32) -> i32 {
    let p = phase.rem_euclid(4096);
    let (p, sign) = if p >= 2048 { (p - 2048, -1) } else { (p, 1) };
    let x = p as i64;
    let num = 16 * x * (2048 - x);
    let den = 5 * 2048 * 2048 - 4 * x * (2048 - x);
    sign * (num * 1024 / den) as i32
}

fn style_pixel(style: &Style, x: i32, y: i32, w: i32, h: i32) -> u32 {
    let mut color = mix(style.top, style.bottom, (y * 256 / h.max(1)) as u32);
    for b in style.blobs {
        let cx = b.cx * w / 1000;
        let cy = b.cy * h / 1000;
        let r = (b.r as i64 * h as i64 / 1000).max(1);
        let dx = (x - cx) as i64;
        let dy = (y - cy) as i64;
        let d2 = dx * dx + dy * dy;
        let r2 = r * r;
        if d2 >= r2 {
            continue;
        }
        let falloff = 256 - (d2 * 256 / r2) as u32;
        let alpha = (falloff * falloff / 256) * b.strength / 256;
        color = mix(color, b.color, alpha);
    }
    color
}

fn mountains(x: i32, y: i32, w: i32, h: i32) -> u32 {
    let sky = mix(
        rgb(0x1b, 0x2a, 0x4e),
        rgb(0xf3, 0x9c, 0x6b),
        (y * 256 / h.max(1)) as u32,
    );
    let layers: [(i32, i32, Argb); 4] = [
        (420, 90, rgb(0x5a, 0x4a, 0x78)),
        (560, 80, rgb(0x3c, 0x30, 0x5c)),
        (690, 70, rgb(0x25, 0x1e, 0x40)),
        (820, 60, rgb(0x14, 0x10, 0x26)),
    ];
    let fx = x * 4096 / w.max(1);
    let mut color = sky;
    // Güneş.
    let (sx, sy, sr) = (w * 68 / 100, h * 48 / 100, h / 9);
    let d2 = ((x - sx) as i64).pow(2) + ((y - sy) as i64).pow(2);
    if d2 < (sr as i64).pow(2) {
        color = mix(color, rgb(0xff, 0xe2, 0xa8), 220);
    } else if d2 < (sr as i64 * 3).pow(2) {
        let glow = 256 - (d2 * 256 / (sr as i64 * 3).pow(2)) as u32;
        color = mix(color, rgb(0xff, 0xd0, 0x90), glow * glow / 512);
    }
    for (i, &(base, amp, layer)) in layers.iter().enumerate() {
        let k = i as i32 + 1;
        let ridge = sin(fx * (2 + k) / 2 + k * 700) * amp / 1024
            + sin(fx * (5 + 2 * k) + k * 1300) * amp / 3072
            + sin(fx * (13 + k) + k * 300) * amp / 9000;
        let top = (base + ridge) * h / 1000;
        if y >= top {
            color = layer;
        }
    }
    color
}

fn waves(x: i32, y: i32, w: i32, h: i32) -> u32 {
    let mut color = mix(
        rgb(0x0b, 0x10, 0x2a),
        rgb(0x1c, 0x0c, 0x33),
        (y * 256 / h.max(1)) as u32,
    );
    let fx = x * 4096 / w.max(1);
    let colors = [
        rgb(0x3d, 0x7b, 0xff),
        rgb(0x7a, 0x4d, 0xff),
        rgb(0xc8, 0x4d, 0xd9),
        rgb(0x2a, 0xc3, 0xd9),
    ];
    for (i, &c) in colors.iter().enumerate() {
        let k = i as i32;
        let center = h * (45 + k * 8) / 100
            + sin(fx * 2 + k * 900) * h / (9 * 1024)
            + sin(fx * 5 + k * 400) * h / (40 * 1024);
        let dist = (y - center).abs();
        let width = h / 60 + 2;
        if dist < width * 6 {
            let t = 256 - (dist * 256 / (width * 6)) as u32;
            color = mix(color, c, t * t / 400);
        }
    }
    color
}

/// Kıvrımlı, ışıklı renk şeritleri: mürekkep → petrol → mercan → kehribar → krem.
fn flow(x: i32, y: i32, w: i32, h: i32) -> u32 {
    const PALETTE: [Argb; 6] = [
        rgb(0x0e, 0x13, 0x1c),
        rgb(0x0c, 0x4a, 0x5a),
        rgb(0x1f, 0x8a, 0x8a),
        rgb(0xe0, 0x5a, 0x2b),
        rgb(0xf6, 0x9e, 0x3c),
        rgb(0xf4, 0xe4, 0xc8),
    ];
    const BAND: i32 = 1100;
    let nx = x * 4096 / w.max(1);
    let ny = y * 4096 / h.max(1);
    let t = nx * 2 / 3
        + ny * 2 / 3
        + sin(ny * 3 / 2 + 700 + sin(nx) / 3) * 420 / 1024
        + sin(nx * 2 - ny + 300) * 260 / 1024;
    let t = t.max(0);
    let band = (t / BAND) as usize;
    let f = t % BAND;
    let i = band % (PALETTE.len() * 2 - 2);
    // Palet ileri-geri dolaşılır (ping-pong), ani renk sıçraması olmaz.
    let at = |k: usize| {
        let n = PALETTE.len() - 1;
        PALETTE[if k <= n { k } else { 2 * n - k }]
    };
    let s = (f * 256 / BAND) as u32;
    let s = (s * s * (768 - 2 * s)) >> 16;
    let mut c = mix(at(i), at(i + 1), s);
    // Şerit ortası parlak, kenarı gölgeli (kıvrılmış kumaş hissi).
    let light = sin(f * 2048 / BAND);
    c = mix(c, 0xff_ffff, (light.max(0) as u32 * 34) >> 10);
    c = mix(c, 0, ((1024 - light.max(0)) as u32 * 70) >> 10);
    // Üst panel ve alt dock bölgesinde hafif karartma.
    let edge = if ny < 400 {
        (400 - ny) * 50 / 400
    } else if ny > 3500 {
        (ny - 3500) * 80 / 596
    } else {
        0
    };
    mix(c, 0, edge as u32)
}

pub fn pixel(preset: u8, x: i32, y: i32, w: i32, h: i32) -> u32 {
    match preset {
        6 => mountains(x, y, w, h),
        7 => waves(x, y, w, h),
        8 => flow(x, y, w, h),
        p => style_pixel(&STYLES[p as usize % STYLES.len()], x, y, w, h),
    }
}

// ---- görüntü dosyaları ----

pub struct Image {
    pub width: i32,
    pub height: i32,
    pub pixels: Vec<u32>,
}

fn u16_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u16::from_le_bytes([*d.get(at)?, *d.get(at + 1)?]) as u32)
}

fn u32_at(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes([
        *d.get(at)?,
        *d.get(at + 1)?,
        *d.get(at + 2)?,
        *d.get(at + 3)?,
    ]))
}

/// Sıkıştırılmamış 24/32 bit BMP.
pub fn decode_bmp(d: &[u8]) -> Option<Image> {
    if d.get(0..2)? != b"BM" {
        return None;
    }
    let offset = u32_at(d, 10)? as usize;
    let width = u32_at(d, 18)? as i32;
    let raw_height = u32_at(d, 22)? as i32;
    let bpp = u16_at(d, 28)?;
    let compression = u32_at(d, 30)?;
    if width <= 0 || raw_height == 0 || width > 8192 || raw_height.abs() > 8192 {
        return None;
    }
    if !(bpp == 24 || bpp == 32) || !(compression == 0 || (compression == 3 && bpp == 32)) {
        return None;
    }
    let height = raw_height.abs();
    let bottom_up = raw_height > 0;
    let bytes = (bpp / 8) as usize;
    let stride = (width as usize * bytes).div_ceil(4) * 4;
    let mut pixels = alloc::vec![0u32; (width * height) as usize];
    for y in 0..height as usize {
        let src_row = if bottom_up {
            height as usize - 1 - y
        } else {
            y
        };
        let row =
            d.get(offset + src_row * stride..offset + src_row * stride + width as usize * bytes)?;
        for x in 0..width as usize {
            let p = &row[x * bytes..];
            pixels[y * width as usize + x] = (p[2] as u32) << 16 | (p[1] as u32) << 8 | p[0] as u32;
        }
    }
    Some(Image {
        width,
        height,
        pixels,
    })
}

/// İkili PPM (P6, 8 bit).
pub fn decode_ppm(d: &[u8]) -> Option<Image> {
    if d.get(0..2)? != b"P6" {
        return None;
    }
    let mut fields = [0usize; 3];
    let mut at = 2;
    for field in fields.iter_mut() {
        loop {
            match *d.get(at)? {
                b'#' => {
                    while *d.get(at)? != b'\n' {
                        at += 1;
                    }
                }
                c if c.is_ascii_whitespace() => at += 1,
                _ => break,
            }
        }
        let start = at;
        while d.get(at)?.is_ascii_digit() {
            at += 1;
        }
        *field = core::str::from_utf8(&d[start..at]).ok()?.parse().ok()?;
    }
    at += 1;
    let (width, height, max) = (fields[0], fields[1], fields[2]);
    if width == 0 || height == 0 || width > 8192 || height > 8192 || max != 255 {
        return None;
    }
    let data = d.get(at..at + width * height * 3)?;
    let pixels = data
        .as_chunks::<3>()
        .0
        .iter()
        .map(|p| (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32)
        .collect();
    Some(Image {
        width: width as i32,
        height: height as i32,
        pixels,
    })
}

/// PNG, BMP ya da PPM.
pub fn decode(data: &[u8]) -> Option<Image> {
    if let Some(png) = crate::gfx::png::decode(data) {
        return Some(Image {
            width: png.width as i32,
            height: png.height as i32,
            pixels: png.pixels.iter().map(|p| p & 0x00ff_ffff).collect(),
        });
    }
    decode_bmp(data).or_else(|| decode_ppm(data))
}

/// Görüntüyü `w`×`h` boyutuna (alfa korunarak, alan ortalamasıyla) küçültür.
pub fn scale_argb(src: &[u32], sw: u32, sh: u32, w: u32, h: u32) -> Vec<u32> {
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let y0 = y * sh / h;
        let y1 = ((y + 1) * sh / h).max(y0 + 1);
        for x in 0..w {
            let x0 = x * sw / w;
            let x1 = ((x + 1) * sw / w).max(x0 + 1);
            let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
            for yy in y0..y1.min(sh) {
                for xx in x0..x1.min(sw) {
                    let p = src[(yy * sw + xx) as usize];
                    let pa = p >> 24;
                    a += pa;
                    r += ((p >> 16) & 0xff) * pa;
                    g += ((p >> 8) & 0xff) * pa;
                    b += (p & 0xff) * pa;
                    n += 1;
                }
            }
            let n = n.max(1);
            let out_a = a / n;
            let (r, g, b) = match (r.checked_div(a), g.checked_div(a), b.checked_div(a)) {
                (Some(r), Some(g), Some(b)) => (r, g, b),
                _ => (0, 0, 0),
            };
            out.push(out_a << 24 | r << 16 | g << 8 | b);
        }
    }
    out
}

/// Görüntüyü `width`×`height` alanı kaplayacak şekilde (ortadan kırparak) örnekler:
/// dönen fonksiyon hedef piksel için çift doğrusal renk verir.
pub fn cover_sampler(
    image: &Image,
    width: i32,
    height: i32,
) -> impl Fn(i32, i32) -> u32 + Sync + '_ {
    let (iw, ih) = (image.width as i64, image.height as i64);
    let (tw, th) = (width.max(1) as i64, height.max(1) as i64);
    let (cw, ch) = if iw * th > ih * tw {
        (ih * tw / th, ih)
    } else {
        (iw, iw * th / tw)
    };
    let ox = (iw - cw) / 2;
    let oy = (ih - ch) / 2;
    let src = &image.pixels;
    move |x, y| {
        // 8 bit kesirli kaynak koordinatı.
        let fx = ox * 256 + (x as i64 * 256 + 128) * cw / tw - 128;
        let fy = oy * 256 + (y as i64 * 256 + 128) * ch / th - 128;
        let x0 = (fx >> 8).clamp(0, iw - 1);
        let y0 = (fy >> 8).clamp(0, ih - 1);
        let x1 = (x0 + 1).min(iw - 1);
        let y1 = (y0 + 1).min(ih - 1);
        let tx = (fx & 0xff).max(0) as u32;
        let ty = (fy & 0xff).max(0) as u32;
        let at = |xx: i64, yy: i64| src[(yy * iw + xx) as usize];
        let top = mix(at(x0, y0), at(x1, y0), tx);
        let bottom = mix(at(x0, y1), at(x1, y1), tx);
        mix(top, bottom, ty)
    }
}

/// Düz renk duvar kağıdı: hafif dikey geçiş.
pub fn solid_pixel(index: u8, y: i32, height: i32) -> u32 {
    let c = SOLIDS[index as usize % SOLIDS.len()].1;
    let lighter = mix(c, 0xffffff, 18);
    mix(lighter, c, (y * 256 / height.max(1)) as u32)
}
