//! MP3 çözücü: MPEG-1/2/2.5 Layer III. minimp3'ün (lieff, CC0) Layer III yolunun kayan
//! noktalı, SIMD'siz aktarımı; tablolar `tables.rs` (minimp3'ten betikle çıkarıldı).
//!
//! `scan` dosyadaki çerçeveleri (ID3 etiketleri ve Xing/Info çerçevesi atlanarak) bir kez
//! dizinler; `Decoder::decode_frame` tek çerçeveyi 16 bit PCM'e (kanallar dönüşümlü) çözer.
//! Bit haznesi (bit reservoir) çerçeveler arasında kod çözücüde tutulur; atlamadan sonra
//! `reset` çağrılır (ilk bir iki çerçeve sessiz çıkabilir).
//!
//! `tags` ID3v2 (2.2-2.4) ve ID3v1 etiketlerinden ad, sanatçı, albüm ve kapak resmini okur.

mod tables;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tables::*;

/// Bir çerçevenin en fazla örneği (1152 × 2 kanal).
pub const MAX_SAMPLES_PER_FRAME: usize = 1152 * 2;

const MAX_RESERVOIR: usize = 511;
const MAX_PAYLOAD: usize = 2304;
const SHORT_BLOCK: u8 = 2;
const STOP_BLOCK: u8 = 3;
const BITS_DEQUANTIZER_OUT: i32 = -1;
const MAX_SCF: i32 = 255 + BITS_DEQUANTIZER_OUT * 4 - 210;
const MAX_SCFI: i32 = (MAX_SCF + 3) & !3;

// ---- Çerçeve başlığı ----

fn is_mono(h: &[u8]) -> bool {
    h[3] & 0xc0 == 0xc0
}
fn is_ms_stereo(h: &[u8]) -> bool {
    h[3] & 0xe0 == 0x60
}
fn is_free_format(h: &[u8]) -> bool {
    h[2] & 0xf0 == 0
}
fn is_crc(h: &[u8]) -> bool {
    h[1] & 1 == 0
}
fn has_padding(h: &[u8]) -> bool {
    h[2] & 2 != 0
}
fn is_mpeg1(h: &[u8]) -> bool {
    h[1] & 8 != 0
}
fn is_not_mpeg25(h: &[u8]) -> bool {
    h[1] & 0x10 != 0
}
fn has_i_stereo(h: &[u8]) -> bool {
    h[3] & 0x10 != 0
}
fn has_ms_stereo(h: &[u8]) -> bool {
    h[3] & 0x20 != 0
}
fn layer_bits(h: &[u8]) -> u8 {
    (h[1] >> 1) & 3
}
fn bitrate_index(h: &[u8]) -> usize {
    (h[2] >> 4) as usize
}
fn sample_rate_index(h: &[u8]) -> usize {
    ((h[2] >> 2) & 3) as usize
}
/// Örnekleme hızı tablosu satırı: MPEG-2.5 0-2, MPEG-2 3-5, MPEG-1 6-8.
fn my_sample_rate(h: &[u8]) -> usize {
    sample_rate_index(h) + ((((h[1] >> 3) & 1) + ((h[1] >> 4) & 1)) * 3) as usize
}
fn is_frame_576(h: &[u8]) -> bool {
    h[1] & 14 == 2
}
fn is_layer1(h: &[u8]) -> bool {
    h[1] & 6 == 6
}

fn header_valid(h: &[u8]) -> bool {
    h.len() >= 4
        && h[0] == 0xff
        && ((h[1] & 0xf0) == 0xf0 || (h[1] & 0xfe) == 0xe2)
        && layer_bits(h) != 0
        && bitrate_index(h) != 15
        && sample_rate_index(h) != 3
}

fn header_compare(h1: &[u8], h2: &[u8]) -> bool {
    header_valid(h2)
        && (h1[1] ^ h2[1]) & 0xfe == 0
        && (h1[2] ^ h2[2]) & 0x0c == 0
        && is_free_format(h1) == is_free_format(h2)
}

fn bitrate_kbps(h: &[u8]) -> u32 {
    2 * HALFRATE[is_mpeg1(h) as usize][layer_bits(h) as usize - 1][bitrate_index(h)] as u32
}

/// Örnekleme hızı (Hz).
pub fn sample_rate_hz(h: &[u8]) -> u32 {
    const HZ: [u32; 3] = [44100, 48000, 32000];
    HZ[sample_rate_index(h)] >> (!is_mpeg1(h) as u32) >> (!is_not_mpeg25(h) as u32)
}

/// Çerçevedeki kanal başına örnek sayısı.
pub fn frame_samples(h: &[u8]) -> usize {
    if is_layer1(h) {
        384
    } else {
        1152 >> is_frame_576(h) as usize
    }
}

/// Dolgu dahil çerçeve uzunluğu (bayt); serbest biçimde 0.
fn frame_len(h: &[u8]) -> usize {
    let bytes = frame_samples(h) as u32 * bitrate_kbps(h) * 125 / sample_rate_hz(h);
    if bytes == 0 {
        return 0;
    }
    bytes as usize + has_padding(h) as usize
}

pub fn channels(h: &[u8]) -> usize {
    if is_mono(h) { 1 } else { 2 }
}

// ---- Dosya tarama ----

/// Dosyadaki Layer III çerçeveleri.
pub struct Stream {
    /// (konum, uzunluk) — her biri tam bir çerçeve.
    pub frames: Vec<(u32, u16)>,
    pub hz: u32,
    pub channels: usize,
    /// Kanal başına çerçeve örneği (1152 ya da 576).
    pub samples_per_frame: usize,
    pub bitrate_kbps: u32,
}

impl Stream {
    pub fn duration_ms(&self) -> u64 {
        self.frames.len() as u64 * self.samples_per_frame as u64 * 1000 / self.hz.max(1) as u64
    }
}

fn id3v2_len(data: &[u8]) -> usize {
    if data.len() < 10 || &data[..3] != b"ID3" {
        return 0;
    }
    let size = syncsafe(&data[6..10]);
    10 + size + if data[5] & 0x10 != 0 { 10 } else { 0 }
}

fn syncsafe(b: &[u8]) -> usize {
    b.iter()
        .fold(0usize, |a, &x| (a << 7) | (x & 0x7f) as usize)
}

/// Çerçeveleri dizinler; MP3 (Layer III) değilse `None`.
pub fn scan(data: &[u8]) -> Option<Stream> {
    let mut pos = id3v2_len(data);
    let n = data.len();
    // İlk çerçeve: geçerli başlık ve hemen ardından uyumlu ikinci başlık.
    let first = loop {
        if pos + 4 > n {
            return None;
        }
        let h = &data[pos..pos + 4];
        if header_valid(h) && layer_bits(h) == 1 && !is_free_format(h) {
            let len = frame_len(h);
            let next = pos + len;
            if len > 4 && (next == n || (next + 4 <= n && header_compare(h, &data[next..next + 4])))
            {
                break pos;
            }
        }
        pos += 1;
    };
    let head = [
        data[first],
        data[first + 1],
        data[first + 2],
        data[first + 3],
    ];
    let mut frames = Vec::new();
    pos = first;
    while pos + 4 <= n {
        let h = &data[pos..pos + 4];
        if header_compare(&head, h) && !is_free_format(h) {
            let len = frame_len(h);
            if len <= 4 || pos + len > n {
                break;
            }
            frames.push((pos as u32, len as u16));
            pos += len;
        } else {
            // Bozuk ya da etiket (ör. sondaki ID3v1): sonraki eşleşen başlığa kadar ilerle.
            pos += 1;
        }
    }
    // Xing/Info/VBRI çerçevesi ses taşımaz (yalnızca kodlayıcı bilgisi).
    if let Some(&(at, len)) = frames.first() {
        let f = &data[at as usize..at as usize + len as usize];
        let probe = &f[..f.len().min(64)];
        if probe
            .windows(4)
            .any(|w| w == b"Xing" || w == b"Info" || w == b"VBRI")
        {
            frames.remove(0);
        }
    }
    if frames.is_empty() {
        return None;
    }
    Some(Stream {
        frames,
        hz: sample_rate_hz(&head),
        channels: channels(&head),
        samples_per_frame: frame_samples(&head),
        bitrate_kbps: bitrate_kbps(&head),
    })
}

// ---- Bit okuyucu ----

struct Bs<'a> {
    buf: &'a [u8],
    pos: usize,
    limit: usize,
}

impl<'a> Bs<'a> {
    fn new(buf: &'a [u8], bytes: usize) -> Self {
        Self {
            buf,
            pos: 0,
            limit: bytes * 8,
        }
    }

    fn byte(&self, i: usize) -> u32 {
        self.buf.get(i).copied().unwrap_or(0) as u32
    }

    fn get_bits(&mut self, n: u32) -> u32 {
        let s = (self.pos & 7) as i32;
        let mut shl = n as i32 + s;
        let mut p = self.pos >> 3;
        self.pos += n as usize;
        if self.pos > self.limit {
            return 0;
        }
        let mut next = self.byte(p) & (255 >> s);
        p += 1;
        let mut cache = 0u32;
        loop {
            shl -= 8;
            if shl <= 0 {
                break;
            }
            cache |= next << shl;
            next = self.byte(p);
            p += 1;
        }
        cache | (next >> -shl)
    }
}

/// Huffman çözümü için 32 bitlik önbellekli okuyucu (minimp3'ün PEEK/FLUSH/CHECK'i).
struct Cache<'a> {
    buf: &'a [u8],
    next: usize,
    cache: u32,
    sh: i32,
}

impl<'a> Cache<'a> {
    fn new(buf: &'a [u8], pos: usize) -> Self {
        let p = pos / 8;
        let b = |i: usize| buf.get(i).copied().unwrap_or(0) as u32;
        let cache = (((b(p) * 256 + b(p + 1)) * 256 + b(p + 2)) * 256 + b(p + 3)) << (pos & 7);
        Self {
            buf,
            next: p + 4,
            cache,
            sh: (pos & 7) as i32 - 8,
        }
    }

    #[inline]
    fn peek(&self, n: u32) -> u32 {
        if n == 0 { 0 } else { self.cache >> (32 - n) }
    }

    #[inline]
    fn flush(&mut self, n: u32) {
        self.cache = self.cache.checked_shl(n).unwrap_or(0);
        self.sh += n as i32;
    }

    #[inline]
    fn check(&mut self) {
        while self.sh >= 0 {
            let b = self.buf.get(self.next).copied().unwrap_or(0) as u32;
            self.cache |= b.checked_shl(self.sh as u32).unwrap_or(0);
            self.next += 1;
            self.sh -= 8;
        }
    }

    fn position(&self) -> isize {
        (self.next * 8) as isize - 24 + self.sh as isize
    }

    fn negative(&self) -> bool {
        (self.cache as i32) < 0
    }
}

// ---- Yan bilgi ----

#[derive(Clone, Copy)]
struct GrInfo {
    sfbtab: &'static [u8],
    part_23_length: u16,
    big_values: u16,
    scalefac_compress: u16,
    global_gain: u8,
    block_type: u8,
    mixed_block_flag: u8,
    n_long_sfb: u8,
    n_short_sfb: u8,
    table_select: [u8; 3],
    region_count: [u8; 3],
    subblock_gain: [u8; 3],
    preflag: u8,
    scalefac_scale: u8,
    count1_table: u8,
    scfsi: u8,
}

impl Default for GrInfo {
    fn default() -> Self {
        Self {
            sfbtab: &SCF_LONG[0],
            part_23_length: 0,
            big_values: 0,
            scalefac_compress: 0,
            global_gain: 0,
            block_type: 0,
            mixed_block_flag: 0,
            n_long_sfb: 0,
            n_short_sfb: 0,
            table_select: [0; 3],
            region_count: [0; 3],
            subblock_gain: [0; 3],
            preflag: 0,
            scalefac_scale: 0,
            count1_table: 0,
            scfsi: 0,
        }
    }
}

/// Yan bilgiyi okur; `main_data_begin` ya da bozuksa `None`.
fn read_side_info(bs: &mut Bs, grs: &mut [GrInfo; 4], hdr: &[u8]) -> Option<usize> {
    let mut sr_idx = my_sample_rate(hdr);
    sr_idx -= (sr_idx != 0) as usize;
    let mut gr_count = if is_mono(hdr) { 1 } else { 2 };
    let main_data_begin;
    let mut scfsi = 0u32;
    if is_mpeg1(hdr) {
        gr_count *= 2;
        main_data_begin = bs.get_bits(9);
        scfsi = bs.get_bits(7 + gr_count);
    } else {
        main_data_begin = bs.get_bits(8 + gr_count) >> gr_count;
    }
    let mut part_23_sum = 0usize;
    for gr in grs.iter_mut().take(gr_count as usize) {
        if is_mono(hdr) {
            scfsi <<= 4;
        }
        gr.part_23_length = bs.get_bits(12) as u16;
        part_23_sum += gr.part_23_length as usize;
        gr.big_values = bs.get_bits(9) as u16;
        if gr.big_values > 288 {
            return None;
        }
        gr.global_gain = bs.get_bits(8) as u8;
        gr.scalefac_compress = bs.get_bits(if is_mpeg1(hdr) { 4 } else { 9 }) as u16;
        gr.sfbtab = &SCF_LONG[sr_idx];
        gr.n_long_sfb = 22;
        gr.n_short_sfb = 0;
        let tables;
        if bs.get_bits(1) != 0 {
            gr.block_type = bs.get_bits(2) as u8;
            if gr.block_type == 0 {
                return None;
            }
            gr.mixed_block_flag = bs.get_bits(1) as u8;
            gr.region_count = [7, 255, 0];
            if gr.block_type == SHORT_BLOCK {
                scfsi &= 0x0f0f;
                if gr.mixed_block_flag == 0 {
                    gr.region_count[0] = 8;
                    gr.sfbtab = &SCF_SHORT[sr_idx];
                    gr.n_long_sfb = 0;
                    gr.n_short_sfb = 39;
                } else {
                    gr.sfbtab = &SCF_MIXED[sr_idx];
                    gr.n_long_sfb = if is_mpeg1(hdr) { 8 } else { 6 };
                    gr.n_short_sfb = 30;
                }
            }
            tables = bs.get_bits(10) << 5;
            gr.subblock_gain = [
                bs.get_bits(3) as u8,
                bs.get_bits(3) as u8,
                bs.get_bits(3) as u8,
            ];
        } else {
            gr.block_type = 0;
            gr.mixed_block_flag = 0;
            tables = bs.get_bits(15);
            gr.region_count[0] = bs.get_bits(4) as u8;
            gr.region_count[1] = bs.get_bits(3) as u8;
            gr.region_count[2] = 255;
        }
        gr.table_select = [
            (tables >> 10) as u8,
            ((tables >> 5) & 31) as u8,
            (tables & 31) as u8,
        ];
        gr.preflag = if is_mpeg1(hdr) {
            bs.get_bits(1) as u8
        } else {
            (gr.scalefac_compress >= 500) as u8
        };
        gr.scalefac_scale = bs.get_bits(1) as u8;
        gr.count1_table = bs.get_bits(1) as u8;
        gr.scfsi = ((scfsi >> 12) & 15) as u8;
        scfsi <<= 4;
    }
    if part_23_sum + bs.pos > bs.limit + main_data_begin as usize * 8 {
        return None;
    }
    Some(main_data_begin as usize)
}

// ---- Ölçek faktörleri ----

fn read_scalefactors(
    iscf: &mut [u8; 40],
    ist_pos: &mut [u8; 39],
    scf_size: &[u8; 4],
    scf_count: &[u8],
    bs: &mut Bs,
    mut scfsi: i32,
) {
    let mut off = 0;
    for i in 0..4 {
        let cnt = scf_count[i] as usize;
        if cnt == 0 {
            break;
        }
        if scfsi & 8 != 0 {
            let (a, b) = (&mut iscf[off..off + cnt], &ist_pos[off..off + cnt]);
            a.copy_from_slice(b);
        } else {
            let bits = scf_size[i] as u32;
            if bits == 0 {
                iscf[off..off + cnt].fill(0);
                ist_pos[off..off + cnt].fill(0);
            } else {
                let max_scf = if scfsi < 0 { (1 << bits) - 1 } else { -1 };
                for k in 0..cnt {
                    let s = bs.get_bits(bits) as i32;
                    ist_pos[off + k] = if s == max_scf { 255 } else { s as u8 };
                    iscf[off + k] = s as u8;
                }
            }
        }
        off += cnt;
        scfsi *= 2;
    }
    iscf[off..off + 3].fill(0);
}

fn ldexp_q2(mut y: f32, mut exp_q2: i32) -> f32 {
    loop {
        let e = exp_q2.min(30 * 4);
        y *= EXPFRAC[(e & 3) as usize] * ((1i32 << 30) >> (e >> 2)) as f32;
        exp_q2 -= e;
        if exp_q2 <= 0 {
            return y;
        }
    }
}

fn decode_scalefactors(
    hdr: &[u8],
    ist_pos: &mut [u8; 39],
    bs: &mut Bs,
    gr: &GrInfo,
    scf: &mut [f32; 40],
    ch: usize,
) {
    let row = (gr.n_short_sfb != 0) as usize + (gr.n_long_sfb == 0) as usize;
    let mut partition: &[u8] = &SCF_PARTITIONS[row];
    let mut scf_size = [0u8; 4];
    let mut iscf = [0u8; 40];
    let scf_shift = gr.scalefac_scale as i32 + 1;
    let mut scfsi = gr.scfsi as i32;

    if is_mpeg1(hdr) {
        let part = SCFC_DECODE[gr.scalefac_compress as usize & 15];
        scf_size[0] = part >> 2;
        scf_size[1] = part >> 2;
        scf_size[2] = part & 3;
        scf_size[3] = part & 3;
    } else {
        let ist = (has_i_stereo(hdr) && ch != 0) as usize;
        let mut sfc = (gr.scalefac_compress >> ist) as i32;
        let mut k = ist * 3 * 4;
        while sfc >= 0 && k + 4 <= MOD.len() {
            let mut modprod = 1i32;
            for i in (0..4).rev() {
                scf_size[i] = (sfc / modprod % MOD[k + i] as i32) as u8;
                modprod *= MOD[k + i] as i32;
            }
            sfc -= modprod;
            k += 4;
        }
        partition = &partition[k.min(partition.len() - 4)..];
        scfsi = -16;
    }
    read_scalefactors(&mut iscf, ist_pos, &scf_size, partition, bs, scfsi);

    let n_long = gr.n_long_sfb as usize;
    if gr.n_short_sfb != 0 {
        let sh = 3 - scf_shift;
        let mut i = 0;
        while i < gr.n_short_sfb as usize {
            for w in 0..3 {
                iscf[n_long + i + w] = iscf[n_long + i + w].wrapping_add(gr.subblock_gain[w] << sh);
            }
            i += 3;
        }
    } else if gr.preflag != 0 {
        for (i, p) in PREAMP.iter().enumerate() {
            iscf[11 + i] = iscf[11 + i].wrapping_add(*p);
        }
    }

    let gain_exp = gr.global_gain as i32 + BITS_DEQUANTIZER_OUT * 4
        - 210
        - if is_ms_stereo(hdr) { 2 } else { 0 };
    let gain = ldexp_q2((1 << (MAX_SCFI / 4)) as f32, MAX_SCFI - gain_exp);
    for i in 0..(n_long + gr.n_short_sfb as usize) {
        scf[i] = ldexp_q2(gain, (iscf[i] as i32) << scf_shift);
    }
}

// ---- Huffman ----

fn pow_43(mut x: i32) -> f32 {
    if x < 129 {
        return POW43[(16 + x) as usize];
    }
    let mut mult = 256.0;
    if x < 1024 {
        mult = 16.0;
        x <<= 3;
    }
    let sign = (2 * x) & 64;
    let frac = ((x & 63) - sign) as f32 / ((x & !63) + sign) as f32;
    let index = (16 + ((x + sign) >> 6)).min(144) as usize;
    POW43[index] * (1.0 + frac * ((4.0 / 3.0) + frac * (2.0 / 9.0))) * mult
}

fn huffman(dst: &mut [f32], bs: &mut Bs, gr: &GrInfo, scf: &[f32; 40], limit: usize) {
    let sfb = gr.sfbtab;
    let mut sfb_i = 0usize;
    let mut scf_i = 0usize;
    let mut r = Cache::new(bs.buf, bs.pos);
    let mut d = 0usize;
    let mut one = 0.0f32;
    let mut ireg = 0usize;
    let mut big = gr.big_values as i32;
    let put = |dst: &mut [f32], d: usize, v: f32| {
        if let Some(slot) = dst.get_mut(d) {
            *slot = v;
        }
    };

    'big: while big > 0 && ireg < 3 {
        let tab_num = gr.table_select[ireg] as usize;
        let mut sfb_cnt = gr.region_count[ireg] as i32;
        ireg += 1;
        let cb = TABINDEX[tab_num] as i32;
        let linbits = LINBITS[tab_num] as u32;
        loop {
            let np = (sfb.get(sfb_i).copied().unwrap_or(0) / 2) as i32;
            sfb_i += 1;
            if np == 0 {
                break 'big;
            }
            let mut pairs = big.min(np);
            one = scf.get(scf_i).copied().unwrap_or(0.0);
            scf_i += 1;
            while pairs > 0 {
                let mut w = 5;
                let mut leaf = TABS[(cb + r.peek(w) as i32) as usize] as i32;
                while leaf < 0 {
                    r.flush(w);
                    w = (leaf & 7) as u32;
                    leaf = TABS[(cb + r.peek(w) as i32 - (leaf >> 3)) as usize] as i32;
                }
                r.flush((leaf >> 8) as u32);
                for _ in 0..2 {
                    let mut lsb = leaf & 0x0f;
                    if linbits != 0 && lsb == 15 {
                        lsb += r.peek(linbits) as i32;
                        r.flush(linbits);
                        r.check();
                        let sign = if r.negative() { -1.0 } else { 1.0 };
                        put(dst, d, one * pow_43(lsb) * sign);
                    } else {
                        let index = 16 + lsb - 16 * (r.cache >> 31) as i32;
                        put(dst, d, POW43[index as usize] * one);
                    }
                    r.flush((lsb != 0) as u32);
                    d += 1;
                    leaf >>= 4;
                }
                r.check();
                pairs -= 1;
            }
            big -= np;
            sfb_cnt -= 1;
            if big <= 0 || sfb_cnt < 0 {
                break;
            }
        }
    }

    let codebook: &[u8] = if gr.count1_table != 0 { &TAB33 } else { &TAB32 };
    let mut np = 1 - big;
    'quads: loop {
        let mut leaf = codebook[r.peek(4) as usize] as i32;
        if leaf & 8 == 0 {
            let extra = (r.cache << 4)
                .checked_shr(32 - (leaf & 3) as u32)
                .unwrap_or(0);
            leaf = codebook[((leaf >> 3) + extra as i32) as usize] as i32;
        }
        r.flush((leaf & 7) as u32);
        if r.position() > limit as isize || d + 4 > 576 {
            break;
        }
        for half in 0..2 {
            np -= 1;
            if np == 0 {
                np = (sfb.get(sfb_i).copied().unwrap_or(0) / 2) as i32;
                sfb_i += 1;
                if np == 0 {
                    break 'quads;
                }
                one = scf.get(scf_i).copied().unwrap_or(0.0);
                scf_i += 1;
            }
            for s in half * 2..half * 2 + 2 {
                if leaf & (128 >> s) != 0 {
                    put(dst, d + s, if r.negative() { -one } else { one });
                    r.flush(1);
                }
            }
        }
        r.check();
        d += 4;
    }
    bs.pos = limit;
}

// ---- Stereo ----

fn midside_stereo(g: &mut [f32], start: usize, n: usize) {
    for i in start..start + n {
        let (a, b) = (g[i], g[i + 576]);
        g[i] = a + b;
        g[i + 576] = a - b;
    }
}

fn intensity_stereo_band(g: &mut [f32], start: usize, n: usize, kl: f32, kr: f32) {
    for i in start..start + n {
        g[i + 576] = g[i] * kr;
        g[i] *= kl;
    }
}

fn stereo_top_band(g: &[f32], sfb: &[u8], nbands: usize, max_band: &mut [i32; 3]) {
    *max_band = [-1; 3];
    let mut right = 576;
    for (i, &width) in sfb.iter().enumerate().take(nbands) {
        let width = width as usize;
        let mut k = 0;
        while k < width {
            if g[right + k] != 0.0 || g[right + k + 1] != 0.0 {
                max_band[i % 3] = i as i32;
                break;
            }
            k += 2;
        }
        right += width;
    }
}

fn stereo_process(
    g: &mut [f32],
    ist_pos: &[u8; 39],
    sfb: &[u8],
    hdr: &[u8],
    max_band: &[i32; 3],
    mpeg2_sh: u32,
) {
    let max_pos = if is_mpeg1(hdr) { 7 } else { 64 };
    let mut left = 0usize;
    let mut i = 0usize;
    while i < sfb.len() && sfb[i] != 0 && i < ist_pos.len() {
        let width = sfb[i] as usize;
        let ipos = ist_pos[i] as u32;
        if i as i32 > max_band[i % 3] && ipos < max_pos {
            let s = if has_ms_stereo(hdr) {
                core::f32::consts::SQRT_2
            } else {
                1.0
            };
            let (kl, kr) = if is_mpeg1(hdr) {
                (PAN[2 * ipos as usize], PAN[2 * ipos as usize + 1])
            } else {
                let k = ldexp_q2(1.0, (((ipos + 1) >> 1) << mpeg2_sh) as i32);
                if ipos & 1 != 0 { (k, 1.0) } else { (1.0, k) }
            };
            intensity_stereo_band(g, left, width, kl * s, kr * s);
        } else if has_ms_stereo(hdr) {
            midside_stereo(g, left, width);
        }
        left += width;
        i += 1;
    }
}

fn intensity_stereo(g: &mut [f32], ist_pos: &mut [u8; 39], gr: &[GrInfo], hdr: &[u8]) {
    let mut max_band = [0i32; 3];
    let n_sfb = gr[0].n_long_sfb as usize + gr[0].n_short_sfb as usize;
    let max_blocks = if gr[0].n_short_sfb != 0 { 3 } else { 1 };
    stereo_top_band(g, gr[0].sfbtab, n_sfb, &mut max_band);
    if gr[0].n_long_sfb != 0 {
        let m = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [m; 3];
    }
    for i in 0..max_blocks {
        let default_pos = if is_mpeg1(hdr) { 3 } else { 0 };
        let itop = n_sfb - max_blocks + i;
        let prev = itop - max_blocks;
        ist_pos[itop] = if max_band[i] >= prev as i32 {
            default_pos
        } else {
            ist_pos[prev]
        };
    }
    stereo_process(
        g,
        ist_pos,
        gr[0].sfbtab,
        hdr,
        &max_band,
        (gr[1].scalefac_compress & 1) as u32,
    );
}

// ---- Yeniden sıralama, örtüşme giderme, IMDCT ----

fn reorder(g: &mut [f32], start: usize, scratch: &mut [f32], sfb: &[u8]) {
    let mut src = start;
    let mut dst = 0;
    let mut j = 0;
    while j < sfb.len() && sfb[j] != 0 {
        let len = sfb[j] as usize;
        for i in 0..len {
            scratch[dst] = g[src + i];
            scratch[dst + 1] = g[src + i + len];
            scratch[dst + 2] = g[src + i + 2 * len];
            dst += 3;
        }
        src += 3 * len;
        j += 3;
    }
    g[start..start + dst].copy_from_slice(&scratch[..dst]);
}

fn antialias(g: &mut [f32], start: usize, nbands: i32) {
    for b in 0..nbands.max(0) as usize {
        let base = start + 18 * b;
        for i in 0..8 {
            let u = g[base + 18 + i];
            let d = g[base + 17 - i];
            g[base + 18 + i] = u * AA[0][i] - d * AA[1][i];
            g[base + 17 - i] = u * AA[1][i] + d * AA[0][i];
        }
    }
}

fn dct3_9(y: &mut [f32; 9]) {
    let (mut s0, mut s2, mut s4, mut s6, mut s8) = (y[0], y[2], y[4], y[6], y[8]);
    let mut t0 = s0 + s6 * 0.5;
    s0 -= s6;
    let mut t4 = (s4 + s2) * 0.939_692_6;
    let mut t2 = (s8 + s2) * 0.766_044_4;
    s6 = (s4 - s8) * 0.173_648_18;
    s4 += s8 - s2;

    s2 = s0 - s4 * 0.5;
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;

    let (mut s1, mut s3, mut s5, mut s7) = (y[1], y[3], y[5], y[7]);
    s3 *= 0.866_025_4;
    t0 = (s5 + s1) * 0.984_807_8;
    t4 = (s5 - s7) * 0.342_020_14;
    t2 = (s1 + s7) * 0.642_787_6;
    s1 = (s1 - s5 - s7) * 0.866_025_4;

    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;

    y[0] = s4 - s7;
    y[1] = s2 + s1;
    y[2] = s0 - s3;
    y[3] = s8 + s5;
    y[5] = s8 - s5;
    y[6] = s0 + s3;
    y[7] = s2 - s1;
    y[8] = s4 + s7;
}

fn imdct36(
    g: &mut [f32],
    start: usize,
    overlap: &mut [f32],
    ov: usize,
    window: &[f32; 18],
    nbands: usize,
) {
    for j in 0..nbands {
        let b = start + 18 * j;
        let o = ov + 9 * j;
        let mut co = [0.0f32; 9];
        let mut si = [0.0f32; 9];
        co[0] = -g[b];
        si[0] = g[b + 17];
        for i in 0..4 {
            si[8 - 2 * i] = g[b + 4 * i + 1] - g[b + 4 * i + 2];
            co[1 + 2 * i] = g[b + 4 * i + 1] + g[b + 4 * i + 2];
            si[7 - 2 * i] = g[b + 4 * i + 4] - g[b + 4 * i + 3];
            co[2 + 2 * i] = -(g[b + 4 * i + 3] + g[b + 4 * i + 4]);
        }
        dct3_9(&mut co);
        dct3_9(&mut si);
        si[1] = -si[1];
        si[3] = -si[3];
        si[5] = -si[5];
        si[7] = -si[7];
        for i in 0..9 {
            let ovl = overlap[o + i];
            let sum = co[i] * TWID9[9 + i] + si[i] * TWID9[i];
            overlap[o + i] = co[i] * TWID9[i] - si[i] * TWID9[9 + i];
            g[b + i] = ovl * window[i] - sum * window[9 + i];
            g[b + 17 - i] = ovl * window[9 + i] + sum * window[i];
        }
    }
}

fn idct3(x0: f32, x1: f32, x2: f32) -> [f32; 3] {
    let m1 = x1 * 0.866_025_4;
    let a1 = x0 - x2 * 0.5;
    [a1 + m1, x0 + x2, a1 - m1]
}

/// `x` 3 adımlı (x[0], x[3], ... x[15]); `dst` 6, `ov` 3 öğe.
fn imdct12(x: &[f32], dst: &mut [f32], ov: &mut [f32]) {
    let co = idct3(-x[0], x[6] + x[3], x[12] + x[9]);
    let mut si = idct3(x[15], x[12] - x[9], x[6] - x[3]);
    si[1] = -si[1];
    for i in 0..3 {
        let ovl = ov[i];
        let sum = co[i] * TWID3[3 + i] + si[i] * TWID3[i];
        ov[i] = co[i] * TWID3[i] - si[i] * TWID3[3 + i];
        dst[i] = ovl * TWID3[2 - i] - sum * TWID3[5 - i];
        dst[5 - i] = ovl * TWID3[5 - i] + sum * TWID3[2 - i];
    }
}

fn imdct_short(g: &mut [f32], start: usize, overlap: &mut [f32], ov: usize, nbands: usize) {
    for j in 0..nbands {
        let b = start + 18 * j;
        let o = ov + 9 * j;
        let mut tmp = [0.0f32; 18];
        tmp.copy_from_slice(&g[b..b + 18]);
        g[b..b + 6].copy_from_slice(&overlap[o..o + 6]);
        imdct12(&tmp, &mut g[b + 6..b + 12], &mut overlap[o + 6..o + 9]);
        imdct12(
            &tmp[1..],
            &mut g[b + 12..b + 18],
            &mut overlap[o + 6..o + 9],
        );
        let (dst, ovl) = overlap[o..o + 9].split_at_mut(6);
        imdct12(&tmp[2..], dst, ovl);
    }
}

fn change_sign(g: &mut [f32], start: usize) {
    let mut base = start + 18;
    for _ in 0..16 {
        for i in (1..18).step_by(2) {
            g[base + i] = -g[base + i];
        }
        base += 36;
    }
}

fn imdct_gr(g: &mut [f32], start: usize, overlap: &mut [f32; 288], block_type: u8, n_long: usize) {
    if n_long > 0 {
        imdct36(g, start, overlap, 0, &MDCT_WINDOW[0], n_long);
    }
    let (s, o) = (start + 18 * n_long, 9 * n_long);
    if block_type == SHORT_BLOCK {
        imdct_short(g, s, overlap, o, 32 - n_long);
    } else {
        let window = &MDCT_WINDOW[(block_type == STOP_BLOCK) as usize];
        imdct36(g, s, overlap, o, window, 32 - n_long);
    }
}

// ---- Sentez filtre bankası ----

fn dct_ii(g: &mut [f32], n: usize) {
    for k in 0..n {
        let mut t = [[0.0f32; 8]; 4];
        for i in 0..8 {
            let x0 = g[k + i * 18];
            let x1 = g[k + (15 - i) * 18];
            let x2 = g[k + (16 + i) * 18];
            let x3 = g[k + (31 - i) * 18];
            let t0 = x0 + x3;
            let t1 = x1 + x2;
            let t2 = (x1 - x2) * SEC[3 * i];
            let t3 = (x0 - x3) * SEC[3 * i + 1];
            t[0][i] = t0 + t1;
            t[1][i] = (t0 - t1) * SEC[3 * i + 2];
            t[2][i] = t3 + t2;
            t[3][i] = (t3 - t2) * SEC[3 * i + 2];
        }
        for x in t.iter_mut() {
            let (mut x0, mut x1, mut x2, mut x3) = (x[0], x[1], x[2], x[3]);
            let (mut x4, mut x5, mut x6, mut x7) = (x[4], x[5], x[6], x[7]);
            let mut xt = x0 - x7;
            x0 += x7;
            x7 = x1 - x6;
            x1 += x6;
            x6 = x2 - x5;
            x2 += x5;
            x5 = x3 - x4;
            x3 += x4;
            x4 = x0 - x3;
            x0 += x3;
            x3 = x1 - x2;
            x1 += x2;
            x[0] = x0 + x1;
            x[4] = (x0 - x1) * 0.707_106_77;
            x5 += x6;
            x6 = (x6 + x7) * 0.707_106_77;
            x7 += xt;
            x3 = (x3 + x4) * 0.707_106_77;
            x5 -= x7 * 0.198_912_37;
            x7 += x5 * 0.382_683_43;
            x5 -= x7 * 0.198_912_37;
            x0 = xt - x6;
            xt += x6;
            x[1] = (xt + x7) * 0.509_795_6;
            x[2] = (x4 + x3) * 0.541_196_1;
            x[3] = (x0 - x5) * 0.601_344_9;
            x[5] = (x0 + x5) * 0.899_976_2;
            x[6] = (x4 - x3) * 1.306_563;
            x[7] = (xt - x7) * 2.562_915_6;
        }
        let mut y = k;
        for i in 0..7 {
            g[y] = t[0][i];
            g[y + 18] = t[2][i] + t[3][i] + t[3][i + 1];
            g[y + 36] = t[1][i] + t[1][i + 1];
            g[y + 54] = t[2][i + 1] + t[3][i] + t[3][i + 1];
            y += 72;
        }
        g[y] = t[0][7];
        g[y + 18] = t[2][7] + t[3][7];
        g[y + 36] = t[1][7];
        g[y + 54] = t[3][7];
    }
}

fn scale_pcm(sample: f32) -> i16 {
    if sample >= 32766.5 {
        return 32767;
    }
    if sample <= -32767.5 {
        return -32768;
    }
    let s = (sample + 0.5) as i16;
    s - (s < 0) as i16
}

fn synth_pair(pcm: &mut [i16], at: usize, nch: usize, z: &[f32], zi: usize) {
    let z0 = |k: usize| z[zi + k * 64];
    let mut a = (z0(14) - z0(0)) * 29.0;
    a += (z0(1) + z0(13)) * 213.0;
    a += (z0(12) - z0(2)) * 459.0;
    a += (z0(3) + z0(11)) * 2037.0;
    a += (z0(10) - z0(4)) * 5153.0;
    a += (z0(5) + z0(9)) * 6574.0;
    a += (z0(8) - z0(6)) * 37489.0;
    a += z0(7) * 75038.0;
    pcm[at] = scale_pcm(a);

    let z2 = |k: usize| z[zi + 2 + k * 64];
    let mut a = z2(14) * 104.0;
    a += z2(12) * 1567.0;
    a += z2(10) * 9727.0;
    a += z2(8) * 64019.0;
    a += z2(6) * -9975.0;
    a += z2(4) * -45.0;
    a += z2(2) * 146.0;
    a += z2(0) * -5.0;
    pcm[at + 16 * nch] = scale_pcm(a);
}

fn synth(
    g: &[f32],
    xl: usize,
    pcm: &mut [i16],
    dl: usize,
    nch: usize,
    lins: &mut [f32],
    li: usize,
) {
    let xr = xl + 576 * (nch - 1);
    let dr = dl + (nch - 1);
    let zlin = (li + 15 * 64) as isize;
    let at = |k: isize| (zlin + k) as usize;

    lins[at(4 * 15)] = g[xl + 18 * 16];
    lins[at(4 * 15 + 1)] = g[xr + 18 * 16];
    lins[at(4 * 15 + 2)] = g[xl];
    lins[at(4 * 15 + 3)] = g[xr];
    lins[at(4 * 31)] = g[xl + 1 + 18 * 16];
    lins[at(4 * 31 + 1)] = g[xr + 1 + 18 * 16];
    lins[at(4 * 31 + 2)] = g[xl + 1];
    lins[at(4 * 31 + 3)] = g[xr + 1];

    synth_pair(pcm, dr, nch, lins, li + 4 * 15 + 1);
    synth_pair(pcm, dr + 32 * nch, nch, lins, li + 4 * 15 + 64 + 1);
    synth_pair(pcm, dl, nch, lins, li + 4 * 15);
    synth_pair(pcm, dl + 32 * nch, nch, lins, li + 4 * 15 + 64);

    let mut w = 0usize;
    for i in (0..15isize).rev() {
        let iu = i as usize;
        lins[at(4 * i)] = g[xl + 18 * (31 - iu)];
        lins[at(4 * i + 1)] = g[xr + 18 * (31 - iu)];
        lins[at(4 * i + 2)] = g[xl + 1 + 18 * (31 - iu)];
        lins[at(4 * i + 3)] = g[xr + 1 + 18 * (31 - iu)];
        lins[at(4 * (i + 16))] = g[xl + 1 + 18 * (1 + iu)];
        lins[at(4 * (i + 16) + 1)] = g[xr + 1 + 18 * (1 + iu)];
        lins[at(4 * (i - 16) + 2)] = g[xl + 18 * (1 + iu)];
        lins[at(4 * (i - 16) + 3)] = g[xr + 18 * (1 + iu)];

        let mut a = [0.0f32; 4];
        let mut b = [0.0f32; 4];
        for k in 0..8isize {
            let w0 = WIN[w];
            let w1 = WIN[w + 1];
            w += 2;
            let vz = at(4 * i - k * 64);
            let vy = at(4 * i - (15 - k) * 64);
            for j in 0..4 {
                let (z, y) = (lins[vz + j], lins[vy + j]);
                let bj = z * w1 + y * w0;
                // k = 0: ilk değer; tek k'larda a'nın işareti ters (minimp3'ün S0/S1/S2'si).
                let aj = if k % 2 == 1 {
                    y * w1 - z * w0
                } else {
                    z * w0 - y * w1
                };
                if k == 0 {
                    b[j] = bj;
                    a[j] = aj;
                } else {
                    b[j] += bj;
                    a[j] += aj;
                }
            }
        }
        pcm[dr + (15 - iu) * nch] = scale_pcm(a[1]);
        pcm[dr + (17 + iu) * nch] = scale_pcm(b[1]);
        pcm[dl + (15 - iu) * nch] = scale_pcm(a[0]);
        pcm[dl + (17 + iu) * nch] = scale_pcm(b[0]);
        pcm[dr + (47 - iu) * nch] = scale_pcm(a[3]);
        pcm[dr + (49 + iu) * nch] = scale_pcm(b[3]);
        pcm[dl + (47 - iu) * nch] = scale_pcm(a[2]);
        pcm[dl + (49 + iu) * nch] = scale_pcm(b[2]);
    }
}

fn synth_granule(
    qmf_state: &mut [f32; 960],
    g: &mut [f32; 1152],
    nbands: usize,
    nch: usize,
    pcm: &mut [i16],
    pcm_at: usize,
    lins: &mut [f32; 33 * 64],
) {
    for ch in 0..nch {
        dct_ii(&mut g[576 * ch..576 * ch + 576], nbands);
    }
    lins[..960].copy_from_slice(qmf_state);
    for i in (0..nbands).step_by(2) {
        synth(g, i, pcm, pcm_at + 32 * nch * i, nch, lins, i * 64);
    }
    if nch == 1 {
        for i in (0..960).step_by(2) {
            qmf_state[i] = lins[nbands * 64 + i];
        }
    } else {
        qmf_state.copy_from_slice(&lins[nbands * 64..nbands * 64 + 960]);
    }
}

// ---- Kod çözücü ----

pub struct Decoder {
    mdct_overlap: [[f32; 288]; 2],
    qmf_state: [f32; 960],
    reserv: usize,
    reserv_buf: [u8; MAX_RESERVOIR],
    maindata: Vec<u8>,
    grbuf: [f32; 1152],
    scf: [f32; 40],
    syn: [f32; 33 * 64],
    ist_pos: [[u8; 39]; 2],
}

impl Decoder {
    pub fn new() -> Box<Self> {
        Box::new(Self {
            mdct_overlap: [[0.0; 288]; 2],
            qmf_state: [0.0; 960],
            reserv: 0,
            reserv_buf: [0; MAX_RESERVOIR],
            maindata: vec![0; MAX_RESERVOIR + MAX_PAYLOAD],
            grbuf: [0.0; 1152],
            scf: [0.0; 40],
            syn: [0.0; 33 * 64],
            ist_pos: [[0; 39]; 2],
        })
    }

    /// Süzgeç durumunu ve bit haznesini sıfırlar (atlamadan sonra).
    pub fn reset(&mut self) {
        self.mdct_overlap = [[0.0; 288]; 2];
        self.qmf_state = [0.0; 960];
        self.reserv = 0;
    }

    /// Tek bir çerçeveyi (`scan`'in verdiği dilim) `pcm`'e çözer; kanal başına örnek
    /// sayısı ve kanal sayısı. Bit haznesi yetersizse (atlamadan hemen sonra) 0 örnek.
    pub fn decode_frame(
        &mut self,
        frame: &[u8],
        pcm: &mut [i16; MAX_SAMPLES_PER_FRAME],
    ) -> (usize, usize) {
        if frame.len() < 4 || !header_valid(frame) || layer_bits(frame) != 1 {
            return (0, 0);
        }
        let hdr = [frame[0], frame[1], frame[2], frame[3]];
        let nch = channels(&hdr);
        let payload = &frame[4..];
        let mut bs = Bs::new(payload, payload.len());
        if is_crc(&hdr) {
            bs.get_bits(16);
        }
        let mut gr = [GrInfo::default(); 4];
        let Some(main_data_begin) = read_side_info(&mut bs, &mut gr, &hdr) else {
            self.reset();
            return (0, nch);
        };
        if bs.pos > bs.limit {
            self.reset();
            return (0, nch);
        }

        // Bit haznesini geri yükle: önceki çerçevelerin artığı + bu çerçevenin ana verisi.
        let frame_bytes = ((bs.limit - bs.pos) / 8).min(MAX_PAYLOAD);
        let have = self.reserv.min(main_data_begin);
        let from = self.reserv - have;
        self.maindata[..have].copy_from_slice(&self.reserv_buf[from..from + have]);
        let start = bs.pos / 8;
        self.maindata[have..have + frame_bytes]
            .copy_from_slice(&payload[start..start + frame_bytes]);
        let success = self.reserv >= main_data_begin;

        let granules = if is_mpeg1(&hdr) { 2 } else { 1 };
        let mut mbs = Bs::new(&self.maindata, have + frame_bytes);
        if success {
            for igr in 0..granules {
                self.grbuf.fill(0.0);
                let grs = &gr[igr * nch..igr * nch + nch];
                for (ch, g) in grs.iter().enumerate() {
                    let limit = mbs.pos + g.part_23_length as usize;
                    decode_scalefactors(
                        &hdr,
                        &mut self.ist_pos[ch],
                        &mut mbs,
                        g,
                        &mut self.scf,
                        ch,
                    );
                    huffman(
                        &mut self.grbuf[576 * ch..576 * ch + 576],
                        &mut mbs,
                        g,
                        &self.scf,
                        limit,
                    );
                }
                if nch == 2 && has_i_stereo(&hdr) {
                    intensity_stereo(&mut self.grbuf, &mut self.ist_pos[1], grs, &hdr);
                } else if nch == 2 && is_ms_stereo(&hdr) {
                    midside_stereo(&mut self.grbuf, 0, 576);
                }
                for (ch, g) in grs.iter().enumerate() {
                    let n_long_bands = (if g.mixed_block_flag != 0 { 2 } else { 0 })
                        << (my_sample_rate(&hdr) == 2) as usize;
                    let mut aa_bands = 31;
                    if g.n_short_sfb != 0 {
                        aa_bands = n_long_bands as i32 - 1;
                        reorder(
                            &mut self.grbuf,
                            576 * ch + n_long_bands * 18,
                            &mut self.syn,
                            &g.sfbtab[g.n_long_sfb as usize..],
                        );
                    }
                    antialias(&mut self.grbuf, 576 * ch, aa_bands);
                    imdct_gr(
                        &mut self.grbuf,
                        576 * ch,
                        &mut self.mdct_overlap[ch],
                        g.block_type,
                        n_long_bands,
                    );
                    change_sign(&mut self.grbuf, 576 * ch);
                }
                synth_granule(
                    &mut self.qmf_state,
                    &mut self.grbuf,
                    18,
                    nch,
                    pcm,
                    igr * 576 * nch,
                    &mut self.syn,
                );
            }
        }

        // Bit haznesini sakla (en çok 511 bayt).
        let mut pos = mbs.pos.div_ceil(8);
        let mut remains = (mbs.limit / 8) as isize - pos as isize;
        if remains > MAX_RESERVOIR as isize {
            pos += remains as usize - MAX_RESERVOIR;
            remains = MAX_RESERVOIR as isize;
        }
        let remains = remains.max(0) as usize;
        if remains > 0 {
            self.reserv_buf[..remains].copy_from_slice(&self.maindata[pos..pos + remains]);
        }
        self.reserv = remains;
        if success {
            (frame_samples(&hdr), nch)
        } else {
            (0, nch)
        }
    }
}

// ---- Etiketler ----

#[derive(Default)]
pub struct Tags {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Kapak resmi (JPEG ya da PNG baytları).
    pub cover: Option<Vec<u8>>,
}

fn latin1(b: &[u8]) -> String {
    b.iter().map(|&c| c as char).collect()
}

fn utf16(b: &[u8], big_endian: bool) -> String {
    let units = b.as_chunks::<2>().0.iter().map(|p| {
        if big_endian {
            u16::from_be_bytes([p[0], p[1]])
        } else {
            u16::from_le_bytes([p[0], p[1]])
        }
    });
    char::decode_utf16(units)
        .map(|c| c.unwrap_or('\u{fffd}'))
        .collect()
}

/// ID3 metin alanı: ilk bayt kodlama (0 Latin-1, 1 BOM'lu UTF-16, 2 UTF-16BE, 3 UTF-8).
fn id3_text(b: &[u8]) -> String {
    let Some((&enc, body)) = b.split_first() else {
        return String::new();
    };
    let text = match enc {
        1 => {
            let big = body.starts_with(&[0xfe, 0xff]);
            let body = if body.starts_with(&[0xfe, 0xff]) || body.starts_with(&[0xff, 0xfe]) {
                &body[2..]
            } else {
                body
            };
            utf16(body, big)
        }
        2 => utf16(body, true),
        3 => String::from_utf8_lossy(body).into_owned(),
        _ => latin1(body),
    };
    String::from(
        text.trim_end_matches('\0')
            .split('\0')
            .next()
            .unwrap_or("")
            .trim(),
    )
}

/// Kodlamaya göre sonlandırıcıyı atlar: (metinden sonraki konum).
fn skip_terminated(b: &[u8], enc: u8) -> usize {
    if enc == 1 || enc == 2 {
        let mut i = 0;
        while i + 1 < b.len() {
            if b[i] == 0 && b[i + 1] == 0 {
                return i + 2;
            }
            i += 2;
        }
        b.len()
    } else {
        b.iter().position(|&c| c == 0).map_or(b.len(), |i| i + 1)
    }
}

/// ID3v2 ve ID3v1 etiketlerini okur. `data` dosyanın başı olabilir (yalnızca ID3v2 için
/// yeterli); kapak `want_cover` ile istenirse alınır.
pub fn tags(data: &[u8], want_cover: bool) -> Tags {
    let mut tags = Tags::default();
    if data.len() >= 10 && &data[..3] == b"ID3" {
        let version = data[3];
        let end = id3v2_len(data).min(data.len());
        let mut pos = 10;
        // Genişletilmiş başlık.
        if data[5] & 0x40 != 0 && pos + 4 <= end {
            let size = if version >= 4 {
                syncsafe(&data[pos..pos + 4])
            } else {
                u32::from_be_bytes([data[pos], data[pos + 1], data[pos + 2], data[pos + 3]])
                    as usize
                    + 4
            };
            pos += size;
        }
        let (id_len, head_len) = if version <= 2 { (3, 6) } else { (4, 10) };
        while pos + head_len <= end {
            let id = &data[pos..pos + id_len];
            if id[0] == 0 {
                break;
            }
            let size = if version <= 2 {
                (data[pos + 3] as usize) << 16
                    | (data[pos + 4] as usize) << 8
                    | data[pos + 5] as usize
            } else if version >= 4 {
                syncsafe(&data[pos + 4..pos + 8])
            } else {
                u32::from_be_bytes([data[pos + 4], data[pos + 5], data[pos + 6], data[pos + 7]])
                    as usize
            };
            let body_start = pos + head_len;
            let body_end = (body_start + size).min(end);
            let body = &data[body_start..body_end];
            match id {
                b"TIT2" | b"TT2" => tags.title = id3_text(body),
                b"TPE1" | b"TP1" => tags.artist = id3_text(body),
                b"TALB" | b"TAL" => tags.album = id3_text(body),
                b"APIC" if want_cover && tags.cover.is_none() && !body.is_empty() => {
                    let enc = body[0];
                    let mut i = 1 + body[1..]
                        .iter()
                        .position(|&c| c == 0)
                        .map_or(body.len(), |p| p + 1);
                    i += 1; // resim türü
                    if i < body.len() {
                        i += skip_terminated(&body[i..], enc);
                        if i < body.len() {
                            tags.cover = Some(body[i..].to_vec());
                        }
                    }
                }
                b"PIC" if want_cover && tags.cover.is_none() && body.len() > 5 => {
                    let enc = body[0];
                    let i = 5 + skip_terminated(&body[5..], enc);
                    if i < body.len() {
                        tags.cover = Some(body[i..].to_vec());
                    }
                }
                _ => {}
            }
            if size == 0 {
                break;
            }
            pos = body_end;
        }
    }
    if tags.title.is_empty() && data.len() >= 128 {
        let v1 = &data[data.len() - 128..];
        if &v1[..3] == b"TAG" {
            let field = |b: &[u8]| String::from(latin1(b).trim_end_matches(['\0', ' ']).trim());
            tags.title = field(&v1[3..33]);
            if tags.artist.is_empty() {
                tags.artist = field(&v1[33..63]);
            }
            if tags.album.is_empty() {
                tags.album = field(&v1[63..93]);
            }
        }
    }
    tags
}

/// ID3v2 etiketinin toplam uzunluğu (etiket yoksa 0); başlıktan okumak için yeterli bayt.
pub fn id3_size(head: &[u8]) -> usize {
    id3v2_len(head)
}
