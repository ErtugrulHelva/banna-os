//! PNG çözücü: zlib/DEFLATE (sabit ve dinamik Huffman), satır filtreleri, gri ton, RGB,
//! palet (tRNS ile saydamlık) ve RGBA; 8 bit (paletlerde 1/2/4/8 bit). Satır atlamalı
//! (Adam7) görüntüler desteklenmez. Çıktı: 0xAARRGGBB pikseller.
//!
//! Çekirdekle kullanıcı alanı aynı dosyayı kullanır.

use alloc::vec;
use alloc::vec::Vec;

pub struct Image {
    pub width: u32,
    pub height: u32,
    /// 0xAARRGGBB
    pub pixels: Vec<u32>,
}

impl Image {
    /// `w`×`h` boyutuna alan ortalamasıyla (ön çarpımlı alfa) küçültür; 0xAARRGGBB.
    pub fn scaled(&self, w: u32, h: u32) -> Vec<u32> {
        let (sw, sh) = (self.width, self.height);
        let mut out = alloc::vec![0u32; (w * h) as usize];
        for y in 0..h {
            let y0 = y * sh / h;
            let y1 = ((y + 1) * sh / h).max(y0 + 1).min(sh);
            for x in 0..w {
                let x0 = x * sw / w;
                let x1 = ((x + 1) * sw / w).max(x0 + 1).min(sw);
                let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for sy in y0..y1 {
                    for &p in &self.pixels[(sy * sw + x0) as usize..(sy * sw + x1) as usize] {
                        let pa = p >> 24;
                        a += pa;
                        r += (p >> 16 & 0xff) * pa;
                        g += (p >> 8 & 0xff) * pa;
                        b += (p & 0xff) * pa;
                        n += 1;
                    }
                }
                if a > 0 {
                    out[(y * w + x) as usize] =
                        (a / n.max(1)) << 24 | (r / a) << 16 | (g / a) << 8 | (b / a);
                }
            }
        }
        out
    }
}

// ---- DEFLATE ----

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u32,
    bits: u32,
}

impl Bits<'_> {
    fn need(&mut self, n: u32) -> Option<()> {
        while self.bits < n {
            let byte = *self.data.get(self.pos)? as u32;
            self.pos += 1;
            self.bit |= byte << self.bits;
            self.bits += 8;
        }
        Some(())
    }

    fn take(&mut self, n: u32) -> Option<u32> {
        if n == 0 {
            return Some(0);
        }
        self.need(n)?;
        let v = self.bit & ((1u32 << n) - 1);
        self.bit >>= n;
        self.bits -= n;
        Some(v)
    }

    fn align(&mut self) {
        self.bit = 0;
        self.bits = 0;
    }
}

/// Kanonik Huffman tablosu: uzunluk başına sayılar ve sıralı semboller.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &l in lengths {
            counts[l as usize] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for i in 1..16 {
            offsets[i] = offsets[i - 1] + counts[i - 1];
        }
        let mut symbols = vec![0u16; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                symbols[offsets[l as usize] as usize] = sym as u16;
                offsets[l as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.take(1)? as i32;
            let count = self.counts[len] as i32;
            if code - count < first {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        None
    }
}

const LEN_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
const LEN_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];

fn inflate_block(
    bits: &mut Bits,
    out: &mut Vec<u8>,
    lit: &Huffman,
    dist: &Huffman,
    limit: usize,
) -> Option<()> {
    loop {
        let sym = lit.decode(bits)?;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Some(()),
            257..=285 => {
                let i = (sym - 257) as usize;
                let len = LEN_BASE[i] as usize + bits.take(LEN_EXTRA[i] as u32)? as usize;
                let d = dist.decode(bits)? as usize;
                if d >= 30 {
                    return None;
                }
                let distance = DIST_BASE[d] as usize + bits.take(DIST_EXTRA[d] as u32)? as usize;
                if distance > out.len() {
                    return None;
                }
                let start = out.len() - distance;
                for k in 0..len {
                    out.push(out[start + k]);
                }
            }
            _ => return None,
        }
        if out.len() > limit {
            return None;
        }
    }
}

/// zlib akışını açar (`limit`: beklenen en büyük çıktı).
pub fn inflate_zlib(data: &[u8], limit: usize) -> Option<Vec<u8>> {
    if data.len() < 2 || data[0] & 0x0f != 8 || ((data[0] as u16) << 8 | data[1] as u16) % 31 != 0 {
        return None;
    }
    let mut bits = Bits {
        data: &data[2..],
        pos: 0,
        bit: 0,
        bits: 0,
    };
    let mut out = Vec::new();
    loop {
        let last = bits.take(1)?;
        match bits.take(2)? {
            0 => {
                bits.align();
                let p = bits.pos;
                let len = u16::from_le_bytes([*bits.data.get(p)?, *bits.data.get(p + 1)?]) as usize;
                out.extend_from_slice(bits.data.get(p + 4..p + 4 + len)?);
                bits.pos = p + 4 + len;
            }
            1 => {
                let mut lengths = [0u8; 288];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..].fill(8);
                let lit = Huffman::new(&lengths)?;
                let dist = Huffman::new(&[5u8; 30])?;
                inflate_block(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            2 => {
                let hlit = bits.take(5)? as usize + 257;
                let hdist = bits.take(5)? as usize + 1;
                let hclen = bits.take(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut code_lengths = [0u8; 19];
                for &o in ORDER.iter().take(hclen) {
                    code_lengths[o] = bits.take(3)? as u8;
                }
                let codes = Huffman::new(&code_lengths)?;
                let mut lengths = vec![0u8; hlit + hdist];
                let mut i = 0;
                while i < lengths.len() {
                    let sym = codes.decode(&mut bits)?;
                    let (value, repeat) = match sym {
                        0..=15 => (sym as u8, 1),
                        16 => (*lengths.get(i.checked_sub(1)?)?, 3 + bits.take(2)? as usize),
                        17 => (0, 3 + bits.take(3)? as usize),
                        18 => (0, 11 + bits.take(7)? as usize),
                        _ => return None,
                    };
                    for _ in 0..repeat {
                        *lengths.get_mut(i)? = value;
                        i += 1;
                    }
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..])?;
                inflate_block(&mut bits, &mut out, &lit, &dist, limit)?;
            }
            _ => return None,
        }
        if last == 1 {
            return Some(out);
        }
    }
}

// ---- PNG ----

fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = a as i16 + b as i16 - c as i16;
    let (pa, pb, pc) = (
        (p - a as i16).abs(),
        (p - b as i16).abs(),
        (p - c as i16).abs(),
    );
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.get(..8)? != b"\x89PNG\r\n\x1a\n" {
        return None;
    }
    let mut pos = 8;
    let (mut width, mut height, mut depth, mut color) = (0u32, 0u32, 0u8, 0u8);
    let mut palette: Vec<[u8; 4]> = Vec::new();
    let mut idat = Vec::new();
    let mut trns: Vec<u8> = Vec::new();
    while pos + 8 <= data.len() {
        let len = be32(data, pos)? as usize;
        let kind = data.get(pos + 4..pos + 8)?;
        let body = data.get(pos + 8..pos + 8 + len)?;
        match kind {
            b"IHDR" => {
                width = be32(body, 0)?;
                height = be32(body, 4)?;
                depth = *body.get(8)?;
                color = *body.get(9)?;
                if *body.get(12)? != 0 {
                    return None;
                }
            }
            b"PLTE" => {
                palette = body
                    .as_chunks::<3>()
                    .0
                    .iter()
                    .map(|c| [c[0], c[1], c[2], 255])
                    .collect()
            }
            b"tRNS" => trns = body.to_vec(),
            b"IDAT" => idat.extend_from_slice(body),
            b"IEND" => break,
            _ => {}
        }
        pos += 12 + len;
    }
    if width == 0 || height == 0 || width > 8192 || height > 8192 {
        return None;
    }
    for (i, &a) in trns.iter().enumerate() {
        if let Some(p) = palette.get_mut(i) {
            p[3] = a;
        }
    }
    let channels = match color {
        0 => 1,
        2 => 3,
        3 => 1,
        4 => 2,
        6 => 4,
        _ => return None,
    };
    if depth != 8
        && !(color == 3 && matches!(depth, 1 | 2 | 4))
        && !(color == 0 && matches!(depth, 1 | 2 | 4))
    {
        return None;
    }
    let bits_per_pixel = channels * depth as usize;
    let stride = (width as usize * bits_per_pixel).div_ceil(8);
    let bpp = bits_per_pixel.div_ceil(8);
    let raw = inflate_zlib(&idat, (stride + 1) * height as usize + 64)?;
    if raw.len() < (stride + 1) * height as usize {
        return None;
    }
    let mut prev = vec![0u8; stride];
    let mut line = vec![0u8; stride];
    let mut pixels = Vec::with_capacity((width * height) as usize);
    for y in 0..height as usize {
        let row = &raw[y * (stride + 1)..(y + 1) * (stride + 1)];
        let filter = row[0];
        line.copy_from_slice(&row[1..]);
        for i in 0..stride {
            let a = if i >= bpp { line[i - bpp] } else { 0 };
            let b = prev[i];
            let c = if i >= bpp { prev[i - bpp] } else { 0 };
            line[i] = match filter {
                0 => line[i],
                1 => line[i].wrapping_add(a),
                2 => line[i].wrapping_add(b),
                3 => line[i].wrapping_add(((a as u16 + b as u16) / 2) as u8),
                4 => line[i].wrapping_add(paeth(a, b, c)),
                _ => return None,
            };
        }
        for x in 0..width as usize {
            let argb = match (color, depth) {
                (2, _) => {
                    let p = &line[x * 3..];
                    0xff00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
                }
                (6, _) => {
                    let p = &line[x * 4..];
                    (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
                }
                (4, _) => {
                    let (g, a) = (line[x * 2] as u32, line[x * 2 + 1] as u32);
                    a << 24 | g << 16 | g << 8 | g
                }
                (0 | 3, d) => {
                    let per_byte = 8 / d as usize;
                    let byte = line[x / per_byte];
                    let shift = 8 - d as usize * (x % per_byte + 1);
                    let v = (byte >> shift) & ((1u16 << d) - 1) as u8;
                    if color == 3 {
                        let p = palette.get(v as usize).copied().unwrap_or([0, 0, 0, 255]);
                        (p[3] as u32) << 24 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32
                    } else {
                        let g = (v as u32 * 255) / ((1u32 << d) - 1);
                        0xff00_0000 | g << 16 | g << 8 | g
                    }
                }
                _ => return None,
            };
            pixels.push(argb);
        }
        core::mem::swap(&mut prev, &mut line);
    }
    Some(Image {
        width,
        height,
        pixels,
    })
}
