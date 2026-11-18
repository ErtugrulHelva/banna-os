//! Temel (baseline, Huffman) JPEG çözücü: gri ton ve YCbCr, her türlü örnekleme
//! (4:4:4, 4:2:2, 4:2:0), yeniden başlatma işaretleri, EXIF yönü yok sayılır.
//! Aşamalı (progressive) JPEG'ler desteklenmez (`None`).

use alloc::vec::Vec;

use crate::gfx::png::Image;

const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

#[derive(Clone, Default)]
struct Huffman {
    /// (uzunluk, kod) → değer, uzunluğa göre arama.
    maxcode: [i32; 18],
    valptr: [i32; 17],
    mincode: [i32; 17],
    values: Vec<u8>,
}

impl Huffman {
    fn new(counts: &[u8; 16], values: Vec<u8>) -> Self {
        let mut h = Huffman {
            values,
            ..Default::default()
        };
        let mut code = 0i32;
        let mut k = 0i32;
        for len in 1..=16 {
            let n = counts[len - 1] as i32;
            if n > 0 {
                h.valptr[len] = k;
                h.mincode[len] = code;
                code += n;
                k += n;
                h.maxcode[len] = code - 1;
            } else {
                h.maxcode[len] = -1;
            }
            code <<= 1;
        }
        h.maxcode[17] = i32::MAX;
        h
    }
}

#[derive(Clone, Copy, Default)]
struct Component {
    id: u8,
    h: usize,
    v: usize,
    tq: usize,
    td: usize,
    ta: usize,
    pred: i32,
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    acc: u32,
    count: u32,
}

impl Bits<'_> {
    fn bit(&mut self) -> u32 {
        if self.count == 0 {
            let mut byte = *self.data.get(self.pos).unwrap_or(&0);
            self.pos += 1;
            if byte == 0xff {
                let next = *self.data.get(self.pos).unwrap_or(&0);
                if next == 0 {
                    self.pos += 1;
                } else {
                    // İşaret: bundan sonra sıfır biti.
                    self.pos -= 1;
                    byte = 0;
                }
            }
            self.acc = byte as u32;
            self.count = 8;
        }
        self.count -= 1;
        (self.acc >> self.count) & 1
    }

    fn bits(&mut self, n: u32) -> i32 {
        let mut v = 0;
        for _ in 0..n {
            v = v << 1 | self.bit() as i32;
        }
        v
    }

    fn decode(&mut self, h: &Huffman) -> Option<u8> {
        let mut code = 0i32;
        for len in 1..=16 {
            code = code << 1 | self.bit() as i32;
            if h.maxcode[len] >= code && h.maxcode[len] != -1 {
                let idx = h.valptr[len] + code - h.mincode[len];
                return h.values.get(idx as usize).copied();
            }
        }
        None
    }

    fn reset(&mut self) {
        self.count = 0;
    }
}

fn extend(v: i32, n: u32) -> i32 {
    if n == 0 {
        0
    } else if v < 1 << (n - 1) {
        v - (1 << n) + 1
    } else {
        v
    }
}

/// cos((2x+1)·u·π/16) · C(u), C(0) = 1/√2.
fn cos_table() -> &'static [[f32; 8]; 8] {
    static mut TABLE: [[f32; 8]; 8] = [[0.0; 8]; 8];
    static mut READY: bool = false;
    unsafe {
        if !READY {
            let t = &mut *core::ptr::addr_of_mut!(TABLE);
            for (x, row) in t.iter_mut().enumerate() {
                for (u, slot) in row.iter_mut().enumerate() {
                    let c = if u == 0 { core::f64::consts::FRAC_1_SQRT_2 } else { 1.0 };
                    let angle = (2 * x + 1) as f64 * u as f64 * core::f64::consts::PI / 16.0;
                    *slot = (c * super::fmath::cos(angle)) as f32;
                }
            }
            READY = true;
        }
        &*core::ptr::addr_of!(TABLE)
    }
}

/// Ayrık kosinüs ters dönüşümü (satır sonra sütun).
fn idct(block: &mut [i32; 64]) {
    let cos = cos_table();
    let mut tmp = [0f32; 64];
    for v in 0..8 {
        let row = &block[v * 8..v * 8 + 8];
        if row.iter().all(|&c| c == 0) {
            continue;
        }
        for x in 0..8 {
            let mut s = 0f32;
            for u in 0..8 {
                s += row[u] as f32 * cos[x][u];
            }
            tmp[v * 8 + x] = s;
        }
    }
    for y in 0..8 {
        for x in 0..8 {
            let mut s = 0f32;
            for v in 0..8 {
                s += tmp[v * 8 + x] * cos[y][v];
            }
            block[y * 8 + x] = ((s / 4.0) as i32 + 128).clamp(0, 255);
        }
    }
}

pub fn decode(data: &[u8]) -> Option<Image> {
    if data.get(..2)? != [0xff, 0xd8] {
        return None;
    }
    let mut qt = [[0u16; 64]; 4];
    let mut dc: [Huffman; 4] = Default::default();
    let mut ac: [Huffman; 4] = Default::default();
    let mut comps: Vec<Component> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    let mut restart = 0usize;
    let mut pos = 2;
    loop {
        while *data.get(pos)? != 0xff {
            pos += 1;
        }
        while *data.get(pos)? == 0xff {
            pos += 1;
        }
        let marker = *data.get(pos)?;
        pos += 1;
        if marker == 0xd9 {
            return None;
        }
        if (0xd0..=0xd7).contains(&marker) || marker == 0x01 {
            continue;
        }
        let len = (*data.get(pos)? as usize) << 8 | *data.get(pos + 1)? as usize;
        let seg = data.get(pos + 2..pos + len)?;
        match marker {
            0xdb => {
                let mut i = 0;
                while i < seg.len() {
                    let precision = seg[i] >> 4;
                    let id = (seg[i] & 3) as usize;
                    i += 1;
                    for k in 0..64 {
                        qt[id][ZIGZAG[k]] = if precision == 0 {
                            *seg.get(i + k)? as u16
                        } else {
                            (*seg.get(i + 2 * k)? as u16) << 8 | *seg.get(i + 2 * k + 1)? as u16
                        };
                    }
                    i += if precision == 0 { 64 } else { 128 };
                }
            }
            0xc0 | 0xc1 => {
                height = (seg[1] as usize) << 8 | seg[2] as usize;
                width = (seg[3] as usize) << 8 | seg[4] as usize;
                let n = seg[5] as usize;
                for c in 0..n {
                    let b = seg.get(6 + c * 3..9 + c * 3)?;
                    comps.push(Component {
                        id: b[0],
                        h: (b[1] >> 4).max(1) as usize,
                        v: (b[1] & 15).max(1) as usize,
                        tq: (b[2] & 3) as usize,
                        ..Default::default()
                    });
                }
            }
            0xc2 | 0xc3 | 0xc5..=0xc7 | 0xc9..=0xcb | 0xcd..=0xcf => return None,
            0xc4 => {
                let mut i = 0;
                while i + 17 <= seg.len() {
                    let class = seg[i] >> 4;
                    let id = (seg[i] & 3) as usize;
                    let counts: [u8; 16] = seg[i + 1..i + 17].try_into().ok()?;
                    let total: usize = counts.iter().map(|&c| c as usize).sum();
                    let values = seg.get(i + 17..i + 17 + total)?.to_vec();
                    let h = Huffman::new(&counts, values);
                    if class == 0 {
                        dc[id] = h;
                    } else {
                        ac[id] = h;
                    }
                    i += 17 + total;
                }
            }
            0xdd => restart = (seg[0] as usize) << 8 | seg[1] as usize,
            0xda => {
                let n = seg[0] as usize;
                for k in 0..n {
                    let id = seg[1 + k * 2];
                    let t = seg[2 + k * 2];
                    if let Some(c) = comps.iter_mut().find(|c| c.id == id) {
                        c.td = (t >> 4) as usize;
                        c.ta = (t & 3) as usize;
                    }
                }
                pos += len;
                return scan(&data[pos..], width, height, &mut comps, &qt, &dc, &ac, restart);
            }
            _ => {}
        }
        pos += len;
    }
}

#[allow(clippy::too_many_arguments)]
fn scan(
    data: &[u8],
    width: usize,
    height: usize,
    comps: &mut [Component],
    qt: &[[u16; 64]; 4],
    dc: &[Huffman; 4],
    ac: &[Huffman; 4],
    restart: usize,
) -> Option<Image> {
    if width == 0 || height == 0 || width * height > 40_000_000 || comps.is_empty() {
        return None;
    }
    let hmax = comps.iter().map(|c| c.h).max()?;
    let vmax = comps.iter().map(|c| c.v).max()?;
    let mcu_w = 8 * hmax;
    let mcu_h = 8 * vmax;
    let mcus_x = width.div_ceil(mcu_w);
    let mcus_y = height.div_ceil(mcu_h);
    // Her bileşenin tam çözünürlüklü düzlemi.
    let planes_w: Vec<usize> = comps.iter().map(|c| mcus_x * c.h * 8).collect();
    let mut planes: Vec<Vec<u8>> = comps
        .iter()
        .enumerate()
        .map(|(i, c)| alloc::vec![0u8; planes_w[i] * mcus_y * c.v * 8])
        .collect();
    let mut bits = Bits {
        data,
        pos: 0,
        acc: 0,
        count: 0,
    };
    let mut block = [0i32; 64];
    let mut mcu = 0usize;
    for my in 0..mcus_y {
        for mx in 0..mcus_x {
            if restart > 0 && mcu > 0 && mcu.is_multiple_of(restart) {
                bits.reset();
                // RSTn işaretini atla.
                while bits.pos + 1 < data.len() && !(data[bits.pos] == 0xff && (0xd0..=0xd7).contains(&data[bits.pos + 1])) {
                    bits.pos += 1;
                }
                bits.pos += 2;
                for c in comps.iter_mut() {
                    c.pred = 0;
                }
            }
            mcu += 1;
            for (ci, c) in comps.iter_mut().enumerate() {
                for by in 0..c.v {
                    for bx in 0..c.h {
                        block.fill(0);
                        let t = bits.decode(&dc[c.td])? as u32;
                        let diff = extend(bits.bits(t), t);
                        c.pred += diff;
                        block[0] = c.pred * qt[c.tq][0] as i32;
                        let mut k = 1;
                        while k < 64 {
                            let rs = bits.decode(&ac[c.ta])?;
                            let r = (rs >> 4) as usize;
                            let s = (rs & 15) as u32;
                            if s == 0 {
                                if r != 15 {
                                    break;
                                }
                                k += 16;
                                continue;
                            }
                            k += r;
                            if k > 63 {
                                break;
                            }
                            let v = extend(bits.bits(s), s);
                            block[ZIGZAG[k]] = v * qt[c.tq][ZIGZAG[k]] as i32;
                            k += 1;
                        }
                        idct(&mut block);
                        let pw = planes_w[ci];
                        let ox = (mx * c.h + bx) * 8;
                        let oy = (my * c.v + by) * 8;
                        let plane = &mut planes[ci];
                        for y in 0..8 {
                            let row = (oy + y) * pw + ox;
                            for x in 0..8 {
                                plane[row + x] = block[y * 8 + x] as u8;
                            }
                        }
                    }
                }
            }
        }
    }
    let mut pixels = alloc::vec![0u32; width * height];
    for y in 0..height {
        for x in 0..width {
            let sample = |ci: usize| -> i32 {
                let c = &comps[ci];
                let sx = x * c.h / hmax;
                let sy = y * c.v / vmax;
                planes[ci][sy * planes_w[ci] + sx] as i32
            };
            let p = if comps.len() >= 3 {
                let yy = sample(0);
                let cb = sample(1) - 128;
                let cr = sample(2) - 128;
                let r = (yy + ((91881 * cr) >> 16)).clamp(0, 255) as u32;
                let g = (yy - ((22554 * cb + 46802 * cr) >> 16)).clamp(0, 255) as u32;
                let b = (yy + ((116130 * cb) >> 16)).clamp(0, 255) as u32;
                0xff00_0000 | r << 16 | g << 8 | b
            } else {
                let v = sample(0) as u32;
                0xff00_0000 | v << 16 | v << 8 | v
            };
            pixels[y * width + x] = p;
        }
    }
    Some(Image {
        width: width as u32,
        height: height as u32,
        pixels,
    })
}
