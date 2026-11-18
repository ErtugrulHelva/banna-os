//! B-Player: MP3 çalar. Ev dizinindeki müzik klasörlerini (Müzik, Music, İndirilenler,
//! Masaüstü…) tarar, ID3 etiketlerinden ad/sanatçı/kapak okur ve sıvı cam arayüzle çalar.
//! Hesap ya da giriş gerekmez; her şey yereldir.
//!
//! Ses `/dev/dsp`'ye (48 kHz, 16 bit, stereo) yazılır. Yazma çağrısı kuyruk dolunca
//! beklediği için aygıt kuyruğunda yalnızca ~300 ms ses tutulur: arayüz hiç takılmaz,
//! duraklatma ve atlama da hemen duyulur. Örnekleme hızı doğrusal aradeğerlemeyle 48 kHz'e
//! çevrilir.
//!
//! Klavye: boşluk çal/duraklat, ←/→ 5 sn geri/ileri, ↑/↓ ses, N sonraki, P önceki,
//! S karıştır, R tekrar.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::gfx::glass::{self, Glass};
use banna::gfx::png::Image;
use banna::gfx::text::UiFonts;
use banna::gfx::{Argb, Canvas, Rect, Surface, blend, rgb, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
    EVENT_WHEEL, KEY_DOWN, KEY_LEFT, KEY_RIGHT, KEY_UP, Window,
};
use banna::icons::{self, Icon};
use banna::io::{self, PollEntry};
use banna::mp3;
use banna::theme::{Prefs, Theme};
use banna::web::fmath;
use banna::{desktop, fs, process, time, ui, wallpaper_art};

banna::entry!(main);

/// Ses aygıtının hızı (kare/sn).
const RATE: u64 = 48_000;
/// Aygıt kuyruğunda tutulan ses: 300 ms.
const LEAD: u64 = RATE * 3 / 10;
const WHITE: Argb = rgb(0xff, 0xff, 0xff);

#[derive(Clone)]
struct Track {
    path: String,
    title: String,
    artist: String,
    album: String,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Repeat {
    Off,
    All,
    One,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Control {
    Track(usize),
    PlayPause,
    Previous,
    Next,
    Shuffle,
    Repeat,
    Seek,
    Volume,
    Rescan,
}

/// Çalınan parça: dosya baytları, çerçeve dizini, kod çözücü ve 48 kHz'e çevirici.
struct Playing {
    index: usize,
    data: Vec<u8>,
    stream: mp3::Stream,
    decoder: Box<mp3::Decoder>,
    /// Sıradaki çözülecek çerçeve.
    frame: usize,
    /// Çözülmüş ama henüz 48 kHz'e çevrilmemiş stereo örnekler.
    pending: Vec<(i16, i16)>,
    /// `pending` içindeki konum (16.16 sabit noktalı) ve adım (kaynak hızı / 48 kHz).
    phase: u64,
    step: u64,
    /// Atlamadan bu yana aygıta yazılan 48 kHz kare ve atlama noktası (ms).
    produced: u64,
    base_ms: u64,
    cover: Option<Image>,
}

/// Ses aygıtı ve oynatma saati: yazılan karelerden, ilk yazmadan bu yana geçen süreyle
/// çalınmış olanlar çıkarılınca kuyrukta bekleyen ses bulunur.
struct Output {
    dsp: Option<fs::File>,
    anchor_ms: u64,
    written: u64,
}

impl Output {
    fn played(&self, now: u64) -> u64 {
        (now.saturating_sub(self.anchor_ms) * RATE / 1000).min(self.written)
    }

    fn queued(&self, now: u64) -> u64 {
        self.written - self.played(now)
    }

    fn write(&mut self, pcm: &[u8], now: u64) {
        if self.queued(now) == 0 {
            // Kuyruk boşaldı (başlangıç, duraklatma ya da yetersiz besleme): saat yeniden.
            self.anchor_ms = now;
            self.written = 0;
        }
        if let Some(dsp) = self.dsp.as_mut()
            && dsp.write_all(pcm).is_err()
        {
            self.dsp = None;
        }
        self.written += (pcm.len() / 4) as u64;
    }
}

struct Player {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    home: String,
    tracks: Vec<Track>,
    order: Vec<usize>,
    playing: Option<Playing>,
    paused: bool,
    output: Output,
    volume: i32,
    shuffle: bool,
    repeat: Repeat,
    seed: u64,
    message: Option<String>,
    hits: Vec<(Rect, Control)>,
    hover: Option<Control>,
    drag: Option<Control>,
    /// Sürüklenirken gösterilen atlama noktası (0-1000).
    seek_preview: Option<i32>,
    seek_track: Rect,
    volume_track: Rect,
    scroll: i32,
    max_scroll: i32,
    /// Arka plan (bulanık kapak ya da renk geçişi) ve camın örneklediği daha bulanık hâli.
    background: Option<(i32, i32, Surface, Surface)>,
    /// Kapak resminin küçültülmüş hâli: (kenar, yüzey).
    cover_scaled: Option<(i32, Surface)>,
    last_second: u64,
    dirty: bool,
}

// ---- Kitaplık ----

fn join(dir: &str, name: &str) -> String {
    if dir.ends_with('/') {
        format!("{}{}", dir, name)
    } else {
        format!("{}/{}", dir, name)
    }
}

fn is_mp3(name: &str) -> bool {
    name.rsplit_once('.')
        .is_some_and(|(_, e)| e.eq_ignore_ascii_case("mp3"))
}

fn walk(dir: &str, depth: u32, out: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for (name, st) in entries {
        if name.starts_with('.') || out.len() >= 2000 {
            continue;
        }
        let path = join(dir, &name);
        if st.kind == fs::KIND_DIRECTORY {
            if depth > 0 {
                walk(&path, depth - 1, out);
            }
        } else if st.kind == fs::KIND_FILE && is_mp3(&name) && !out.contains(&path) {
            out.push(path);
        }
    }
}

/// Dosyanın başından (ID3v2) ve gerekirse sonundan (ID3v1) etiketleri okur.
fn read_track(path: &str) -> Track {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    let stem = file_name.rsplit_once('.').map_or(file_name, |(s, _)| s);
    let mut track = Track {
        path: String::from(path),
        title: String::from(stem),
        artist: String::new(),
        album: String::new(),
    };
    let Ok(mut file) = fs::File::open(path, fs::READ) else {
        return track;
    };
    let mut head = alloc::vec![0u8; 10];
    let got = file.read(&mut head).unwrap_or(0);
    head.truncate(got);
    let size = mp3::id3_size(&head).min(256 * 1024);
    if size > head.len() {
        let mut rest = alloc::vec![0u8; size - head.len()];
        let got = file.read(&mut rest).unwrap_or(0);
        head.extend_from_slice(&rest[..got]);
    }
    let mut tags = mp3::tags(&head, false);
    if tags.title.is_empty()
        && let Ok(st) = fs::stat(path)
        && st.size >= 128
    {
        file.seek(st.size - 128);
        let mut tail = alloc::vec![0u8; 128];
        if file.read(&mut tail).unwrap_or(0) == 128 {
            tags = mp3::tags(&tail, false);
        }
    }
    if !tags.title.is_empty() {
        track.title = tags.title;
    }
    track.artist = tags.artist;
    track.album = tags.album;
    track
}

fn scan_library(home: &str) -> Vec<Track> {
    let mut paths = Vec::new();
    for dir in [
        "Müzik",
        "Music",
        "İndirilenler",
        "Downloads",
        "Masaüstü",
        "Desktop",
        "Belgeler",
        "Documents",
    ] {
        walk(&join(home, dir), 3, &mut paths);
    }
    walk(home, 0, &mut paths);
    let mut tracks: Vec<Track> = paths.iter().map(|p| read_track(p)).collect();
    tracks.sort_by_key(|t| (t.artist.to_lowercase(), t.title.to_lowercase()));
    tracks
}

fn time_text(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

// ---- Çizim yardımcıları ----

/// İşaretli uzaklık fonksiyonuyla (piksel, içeride negatif) kenarı yumuşatılmış şekil.
fn shape(c: &mut Canvas, area: Rect, color: Argb, sdf: impl Fn(f32, f32) -> f32) {
    let r = area.intersect(&c.clip);
    if r.is_empty() {
        return;
    }
    let alpha = (color >> 24) as f32;
    for y in r.y..r.bottom() {
        let row = c.span(y, r.x, r.right());
        for (i, p) in row.iter_mut().enumerate() {
            let d = sdf((r.x + i as i32) as f32 + 0.5, y as f32 + 0.5);
            if d >= 0.5 {
                continue;
            }
            let coverage = (0.5 - d).min(1.0);
            *p = blend(*p, with_alpha(color, (alpha * coverage) as u8));
        }
    }
}

fn length(x: f32, y: f32) -> f32 {
    fmath::sqrt((x * x + y * y) as f64) as f32
}

fn segment(px: f32, py: f32, a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let t = (((px - a.0) * dx + (py - a.1) * dy) / (dx * dx + dy * dy).max(1e-6)).clamp(0.0, 1.0);
    length(px - a.0 - dx * t, py - a.1 - dy * t)
}

/// Üçgen (köşeler saat yönünde): kenarlara olan uzaklıkların en büyüğü.
fn triangle(c: &mut Canvas, p: [(f32, f32); 3], color: Argb) {
    let x0 = p.iter().map(|q| q.0).fold(f32::MAX, f32::min) as i32 - 1;
    let y0 = p.iter().map(|q| q.1).fold(f32::MAX, f32::min) as i32 - 1;
    let x1 = p.iter().map(|q| q.0).fold(f32::MIN, f32::max) as i32 + 2;
    let y1 = p.iter().map(|q| q.1).fold(f32::MIN, f32::max) as i32 + 2;
    let edge = move |a: (f32, f32), b: (f32, f32), x: f32, y: f32| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        ((x - a.0) * dy - (y - a.1) * dx) / length(dx, dy).max(1e-6)
    };
    shape(
        c,
        Rect::new(x0, y0, x1 - x0, y1 - y0),
        color,
        move |x, y| {
            edge(p[0], p[1], x, y)
                .max(edge(p[1], p[2], x, y))
                .max(edge(p[2], p[0], x, y))
        },
    );
}

fn line(c: &mut Canvas, a: (f32, f32), b: (f32, f32), width: f32, color: Argb) {
    let pad = width as i32 + 2;
    let x0 = a.0.min(b.0) as i32 - pad;
    let y0 = a.1.min(b.1) as i32 - pad;
    let x1 = a.0.max(b.0) as i32 + pad;
    let y1 = a.1.max(b.1) as i32 + pad;
    shape(
        c,
        Rect::new(x0, y0, x1 - x0, y1 - y0),
        color,
        move |x, y| segment(x, y, a, b) - width / 2.0,
    );
}

/// Kenar yumuşatmalı yuvarlak dikdörtgen çerçeve (halka).
fn rounded_outline(c: &mut Canvas, r: Rect, radius: f32, width: f32, color: Argb) {
    let (cx, cy) = (r.x as f32 + r.w as f32 / 2.0, r.y as f32 + r.h as f32 / 2.0);
    let (hx, hy) = (r.w as f32 / 2.0 - radius, r.h as f32 / 2.0 - radius);
    shape(c, r.inset(-2), color, move |x, y| {
        let qx = (x - cx).abs() - hx;
        let qy = (y - cy).abs() - hy;
        let d = length(qx.max(0.0), qy.max(0.0)) + qx.max(qy).min(0.0) - radius;
        d.abs() - width / 2.0
    });
}

impl Player {
    fn glass_style(&self) -> Glass {
        let u = self.u;
        if self.theme.dark {
            Glass {
                tint: with_alpha(rgb(0x16, 0x14, 0x22), 120),
                rim: 140,
                refraction: 10 * u,
                edge: 10 * u,
                sheen: 26,
            }
        } else {
            Glass {
                tint: with_alpha(rgb(0xff, 0xff, 0xff), 120),
                rim: 220,
                refraction: 10 * u,
                edge: 10 * u,
                sheen: 50,
            }
        }
    }

    fn text(&self) -> Argb {
        if self.theme.dark {
            WHITE
        } else {
            rgb(0x15, 0x14, 0x1a)
        }
    }

    fn muted(&self) -> Argb {
        if self.theme.dark {
            with_alpha(WHITE, 170)
        } else {
            with_alpha(rgb(0x15, 0x14, 0x1a), 160)
        }
    }

    // ---- Arka plan ----

    /// Kapaktan (ya da parçaya özgü renklerden) bulanık arka planı ve cam örneğini üretir.
    fn build_background(&mut self) {
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        if w <= 0 || h <= 0 {
            return;
        }
        let u = self.u;
        let mut base = Surface::new(w, h, 0);
        match self.playing.as_ref().and_then(|p| p.cover.as_ref()) {
            Some(img) => {
                // Kapağı pencereyi dolduracak biçimde ortadan kırp, küçült.
                let (iw, ih) = (img.width.max(1), img.height.max(1));
                let scale_w = w as u64 * 1024 / iw as u64;
                let scale_h = h as u64 * 1024 / ih as u64;
                let scale = scale_w.max(scale_h).max(1);
                let cw = ((w as u64 * 1024 / scale) as u32).clamp(1, iw);
                let ch = ((h as u64 * 1024 / scale) as u32).clamp(1, ih);
                let (ox, oy) = ((iw - cw) / 2, (ih - ch) / 2);
                let mut crop = Vec::with_capacity((cw * ch) as usize);
                for y in 0..ch {
                    let start = ((oy + y) * iw + ox) as usize;
                    crop.extend_from_slice(&img.pixels[start..start + cw as usize]);
                }
                let small = wallpaper_art::scale_argb(
                    &crop,
                    cw,
                    ch,
                    (w / 8).max(1) as u32,
                    (h / 8).max(1) as u32,
                );
                let full = wallpaper_art::scale_argb(
                    &small,
                    (w / 8).max(1) as u32,
                    (h / 8).max(1) as u32,
                    w as u32,
                    h as u32,
                );
                for (d, s) in base.pixels.iter_mut().zip(full) {
                    *d = s & 0xff_ffff;
                }
            }
            None => {
                // Kapak yok: parçanın adından renkler, üç yumuşak ışık lekesi.
                let key = self
                    .playing
                    .as_ref()
                    .and_then(|p| self.tracks.get(p.index))
                    .map_or(7u32, |t| {
                        t.title
                            .bytes()
                            .fold(2166136261u32, |a, b| (a ^ b as u32).wrapping_mul(16777619))
                    });
                let palette = [
                    (
                        rgb(0x3b, 0x1d, 0x8f),
                        rgb(0xff, 0x4f, 0x9a),
                        rgb(0x2b, 0xc4, 0xff),
                    ),
                    (
                        rgb(0x0f, 0x3d, 0x5c),
                        rgb(0x2b, 0xe0, 0xb0),
                        rgb(0x8a, 0x5c, 0xff),
                    ),
                    (
                        rgb(0x4a, 0x13, 0x2b),
                        rgb(0xff, 0x8a, 0x3d),
                        rgb(0xff, 0x3d, 0x6e),
                    ),
                    (
                        rgb(0x12, 0x1a, 0x4f),
                        rgb(0x5f, 0x7c, 0xff),
                        rgb(0xff, 0x6a, 0xd5),
                    ),
                ];
                let (bg, a, b) = palette[(key % palette.len() as u32) as usize];
                let mut c = base.canvas();
                c.gradient_v(Rect::new(0, 0, w, h), bg, rgb(0x0c, 0x0b, 0x14));
                let spots = [
                    (w / 5 + (key >> 4) as i32 % (w / 4).max(1), h / 3, w / 3, a),
                    (
                        w * 3 / 4,
                        h / 4 + (key >> 8) as i32 % (h / 4).max(1),
                        w / 4,
                        b,
                    ),
                    (w / 2, h * 5 / 6, w / 3, a),
                ];
                for (x, y, r, col) in spots {
                    icons::disc(&mut c, x, y, r.max(8), with_alpha(col, 200));
                }
            }
        }
        let mut background = glass::backdrop(&base, 64 * u);
        // Okunabilirlik: koyu temada karart, açık temada aç.
        let (over, alpha) = if self.theme.dark {
            (0x0a_0912u32, 90u32)
        } else {
            (0xf4_f2f8u32, 110u32)
        };
        for p in background.pixels.iter_mut() {
            *p = blend(*p, over | alpha << 24);
        }
        let sample = glass::backdrop(&background, 28 * u);
        self.background = Some((w, h, background, sample));
    }

    // ---- Oynatma ----

    fn load(&mut self, index: usize) {
        let Some(track) = self.tracks.get(index).cloned() else {
            return;
        };
        self.playing = None;
        self.cover_scaled = None;
        let data = match fs::read(&track.path) {
            Ok(d) => d,
            Err(e) => {
                self.message = Some(format!("{} açılamadı: {}", track.title, fs::error_text(e)));
                self.dirty = true;
                return;
            }
        };
        let Some(stream) = mp3::scan(&data) else {
            self.message = Some(format!("{}: geçerli bir MP3 değil", track.title));
            self.dirty = true;
            return;
        };
        let tags = mp3::tags(&data, true);
        let cover = tags.cover.and_then(|bytes| {
            banna::gfx::png::decode(&bytes).or_else(|| banna::web::jpeg::decode(&bytes))
        });
        let step = ((stream.hz as u64) << 16) / RATE;
        self.playing = Some(Playing {
            index,
            data,
            stream,
            decoder: mp3::Decoder::new(),
            frame: 0,
            pending: Vec::new(),
            phase: 0,
            step,
            produced: 0,
            base_ms: 0,
            cover,
        });
        self.message = None;
        self.paused = false;
        self.build_background();
        self.dirty = true;
    }

    fn position_ms(&self, now: u64) -> u64 {
        let Some(p) = &self.playing else { return 0 };
        let queued = self.output.queued(now).min(p.produced);
        p.base_ms + (p.produced - queued) * 1000 / RATE
    }

    fn duration_ms(&self) -> u64 {
        self.playing.as_ref().map_or(0, |p| p.stream.duration_ms())
    }

    fn seek(&mut self, ms: u64) {
        let Some(p) = self.playing.as_mut() else {
            return;
        };
        let frame_ms = p.stream.samples_per_frame as u64 * 1000 / p.stream.hz.max(1) as u64;
        let frame = (ms / frame_ms.max(1)) as usize;
        p.frame = frame.min(p.stream.frames.len());
        p.decoder.reset();
        p.pending.clear();
        p.phase = 0;
        p.produced = 0;
        p.base_ms =
            p.frame as u64 * p.stream.samples_per_frame as u64 * 1000 / p.stream.hz.max(1) as u64;
        // Kuyruktaki eski ses hesaba katılmasın: saat yeniden başlar.
        self.output.written = 0;
        self.dirty = true;
    }

    fn random(&mut self) -> u64 {
        self.seed ^= self.seed << 13;
        self.seed ^= self.seed >> 7;
        self.seed ^= self.seed << 17;
        self.seed
    }

    fn reshuffle(&mut self) {
        self.order = (0..self.tracks.len()).collect();
        if self.shuffle {
            for i in (1..self.order.len()).rev() {
                let j = (self.random() % (i as u64 + 1)) as usize;
                self.order.swap(i, j);
            }
        }
    }

    fn neighbour(&self, forward: bool) -> Option<usize> {
        if self.order.is_empty() {
            return None;
        }
        let current = self.playing.as_ref().map(|p| p.index);
        let at = current.and_then(|c| self.order.iter().position(|&i| i == c));
        let len = self.order.len();
        let next = match (at, forward) {
            (None, _) => 0,
            (Some(a), true) if a + 1 < len => a + 1,
            (Some(_), true) if self.repeat == Repeat::Off => return None,
            (Some(_), true) => 0,
            (Some(0), false) => len - 1,
            (Some(a), false) => a - 1,
        };
        Some(self.order[next])
    }

    fn next(&mut self) {
        match self.neighbour(true) {
            Some(i) => self.load(i),
            None => {
                self.paused = true;
                self.seek(0);
            }
        }
    }

    fn previous(&mut self, now: u64) {
        if self.position_ms(now) > 3000 {
            self.seek(0);
        } else if let Some(i) = self.neighbour(false) {
            self.load(i);
        }
    }

    fn toggle(&mut self) {
        if self.playing.is_none() {
            if let Some(&first) = self.order.first() {
                self.load(first);
            }
            return;
        }
        self.paused = !self.paused;
        self.dirty = true;
    }

    /// Aygıt kuyruğunu `LEAD` kadar dolu tutar; parça bitince sıradakine geçer.
    fn pump(&mut self, now: u64) {
        if self.paused || self.playing.is_none() {
            return;
        }
        let queued = self.output.queued(now);
        if queued >= LEAD {
            return;
        }
        let want = LEAD - queued + RATE / 50;
        let gain = self.volume;
        let mut out: Vec<u8> = Vec::with_capacity(want as usize * 4);
        let mut ended = false;
        {
            let p = self.playing.as_mut().unwrap();
            let mut pcm = [0i16; mp3::MAX_SAMPLES_PER_FRAME];
            while (out.len() / 4) < want as usize {
                // Aradeğerleme için iki örnek gerekir.
                if (p.phase >> 16) as usize + 1 >= p.pending.len() {
                    let Some(&(at, len)) = p.stream.frames.get(p.frame) else {
                        ended = true;
                        break;
                    };
                    p.frame += 1;
                    let frame = &p.data[at as usize..at as usize + len as usize];
                    let (n, ch) = p.decoder.decode_frame(frame, &mut pcm);
                    let consumed = (p.phase >> 16) as usize;
                    if consumed > 0 {
                        p.pending.drain(..consumed.min(p.pending.len()));
                        p.phase -= (consumed as u64) << 16;
                    }
                    for i in 0..n {
                        let l = pcm[i * ch];
                        let r = pcm[i * ch + ch - 1];
                        p.pending.push((l, r));
                    }
                    continue;
                }
                let i = (p.phase >> 16) as usize;
                let f = (p.phase & 0xffff) as i32;
                let (a, b) = (p.pending[i], p.pending[i + 1]);
                let mix = |x: i16, y: i16| {
                    let v = x as i32 + (((y as i32 - x as i32) * f) >> 16);
                    (v * gain / 100).clamp(-32768, 32767) as i16
                };
                out.extend_from_slice(&mix(a.0, b.0).to_le_bytes());
                out.extend_from_slice(&mix(a.1, b.1).to_le_bytes());
                p.phase += p.step;
            }
            p.produced += (out.len() / 4) as u64;
        }
        if !out.is_empty() {
            self.output.write(&out, now);
        }
        if ended && self.output.queued(time::uptime_ms()) == 0 {
            if self.repeat == Repeat::One {
                self.seek(0);
            } else {
                self.next();
            }
        }
    }

    // ---- Olaylar ----

    fn press(&mut self, x: i32, y: i32, now: u64) {
        let Some(&(_, control)) = self.hits.iter().rev().find(|(r, _)| r.contains(x, y)) else {
            return;
        };
        match control {
            Control::Track(i) => self.load(i),
            Control::PlayPause => self.toggle(),
            Control::Previous => self.previous(now),
            Control::Next => self.next(),
            Control::Shuffle => {
                self.shuffle = !self.shuffle;
                self.reshuffle();
            }
            Control::Repeat => {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }
            }
            Control::Seek => {
                self.drag = Some(Control::Seek);
                self.drag_to(x);
            }
            Control::Volume => {
                self.drag = Some(Control::Volume);
                self.drag_to(x);
            }
            Control::Rescan => {
                let current = self
                    .playing
                    .as_ref()
                    .and_then(|p| self.tracks.get(p.index))
                    .map(|t| t.path.clone());
                self.tracks = scan_library(&self.home);
                self.reshuffle();
                if let Some(path) = current {
                    match self.tracks.iter().position(|t| t.path == path) {
                        Some(i) => {
                            if let Some(p) = self.playing.as_mut() {
                                p.index = i;
                            }
                        }
                        None => self.playing = None,
                    }
                }
            }
        }
        self.dirty = true;
    }

    fn drag_to(&mut self, x: i32) {
        match self.drag {
            Some(Control::Seek) => {
                let r = self.seek_track;
                self.seek_preview = Some(((x - r.x) * 1000 / r.w.max(1)).clamp(0, 1000));
            }
            Some(Control::Volume) => {
                let r = self.volume_track;
                self.volume = ((x - r.x) * 100 / r.w.max(1)).clamp(0, 100);
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn release(&mut self) {
        if self.drag == Some(Control::Seek)
            && let Some(permille) = self.seek_preview.take()
        {
            let ms = self.duration_ms() * permille as u64 / 1000;
            self.seek(ms);
        }
        self.drag = None;
        self.dirty = true;
    }

    fn key(&mut self, key: u32, now: u64) {
        match key {
            32 => self.toggle(),
            KEY_LEFT => {
                let at = self.position_ms(now);
                self.seek(at.saturating_sub(5000));
            }
            KEY_RIGHT => {
                let at = self.position_ms(now) + 5000;
                if at < self.duration_ms() {
                    self.seek(at);
                }
            }
            KEY_UP => self.volume = (self.volume + 5).min(100),
            KEY_DOWN => self.volume = (self.volume - 5).max(0),
            k if k == 'n' as u32 || k == 'N' as u32 => self.next(),
            k if k == 'p' as u32 || k == 'P' as u32 => self.previous(now),
            k if k == 's' as u32 || k == 'S' as u32 => {
                self.shuffle = !self.shuffle;
                self.reshuffle();
            }
            k if k == 'r' as u32 || k == 'R' as u32 => {
                self.repeat = match self.repeat {
                    Repeat::Off => Repeat::All,
                    Repeat::All => Repeat::One,
                    Repeat::One => Repeat::Off,
                }
            }
            _ => return,
        }
        self.dirty = true;
    }

    // ---- Çizim ----

    fn glass_panel(&self, c: &mut Canvas, r: Rect, radius: i32) {
        if let Some((_, _, _, sample)) = &self.background {
            c.shadow(r, radius, 14 * self.u, 60);
            c.glass(r, radius, sample, (0, 0), &self.glass_style());
        }
    }

    fn render(&mut self, now: u64) {
        self.dirty = false;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        if self
            .background
            .as_ref()
            .is_none_or(|(bw, bh, _, _)| (*bw, *bh) != (w, h))
        {
            self.build_background();
        }
        let u = self.u;
        let text = self.text();
        let muted = self.muted();
        let accent = self.theme.accent;
        let mut hits: Vec<(Rect, Control)> = Vec::new();
        let position = self.position_ms(now);
        let duration = self.duration_ms();
        // Çizim boyunca pencereye başka erişim yok: tuval ayrı ödünç alınır.
        let window: *mut Window = &mut self.window;
        let mut c = unsafe { (*window).canvas() };
        let f = &self.fonts;
        match &self.background {
            Some((_, _, bg, _)) => c.blit(bg, Rect::new(0, 0, w, h), 0, 0),
            None => c.fill(Rect::new(0, 0, w, h), rgb(0x12, 0x10, 0x1c)),
        }

        let m = 16 * u;
        let side_w = (300 * u).min(w * 2 / 5);
        let library = Rect::new(m, m, side_w, h - 2 * m);
        let stage = Rect::new(
            library.right() + m,
            m,
            w - library.right() - 2 * m,
            h - 2 * m,
        );

        // ---- Kitaplık ----
        self.glass_panel(&mut c, library, 22 * u);
        icons::draw(
            &mut c,
            Rect::new(library.x + 18 * u, library.y + 18 * u, 36 * u, 36 * u),
            Icon::Music,
        );
        f.title.draw(
            &mut c,
            library.x + 66 * u,
            library.y + 18 * u,
            "B-Player",
            text,
        );
        let count = format!("{} parça", self.tracks.len());
        f.small.draw(
            &mut c,
            library.x + 66 * u,
            library.y + 40 * u,
            &count,
            muted,
        );
        let rescan = Rect::new(
            library.right() - 18 * u - 72 * u,
            library.y + 22 * u,
            72 * u,
            28 * u,
        );
        icons::rounded_rect(
            &mut c,
            rescan,
            14 * u,
            with_alpha(
                text,
                if self.hover == Some(Control::Rescan) {
                    46
                } else {
                    26
                },
            ),
        );
        let lw = f.small.measure("Yenile");
        f.small.draw(
            &mut c,
            rescan.x + (rescan.w - lw) / 2,
            rescan.y + (rescan.h - f.small.ascent - f.small.descent) / 2,
            "Yenile",
            text,
        );
        hits.push((rescan, Control::Rescan));

        let list = Rect::new(
            library.x + 8 * u,
            library.y + 70 * u,
            library.w - 16 * u,
            library.h - 78 * u,
        );
        let row_h = 54 * u;
        let saved_clip = c.clip;
        c.clip = list.intersect(&saved_clip);
        if self.tracks.is_empty() {
            let lines = [
                "Kitaplıkta MP3 yok.",
                "MP3 dosyalarını ev dizinindeki",
                "Müzik klasörüne koyup",
                "Yenile'ye basın.",
            ];
            for (i, l) in lines.iter().enumerate() {
                let lw = f.small.measure(l);
                f.small.draw(
                    &mut c,
                    list.x + (list.w - lw) / 2,
                    list.y + 30 * u + i as i32 * 20 * u,
                    l,
                    muted,
                );
            }
        }
        let current = self.playing.as_ref().map(|p| p.index);
        for (i, t) in self.tracks.iter().enumerate() {
            let r = Rect::new(
                list.x,
                list.y + i as i32 * row_h - self.scroll,
                list.w,
                row_h - 4 * u,
            );
            if r.bottom() < list.y || r.y > list.bottom() {
                continue;
            }
            let playing_row = current == Some(i);
            if playing_row {
                icons::rounded_rect(&mut c, r, 14 * u, with_alpha(accent, 150));
            } else if self.hover == Some(Control::Track(i)) {
                icons::rounded_rect(&mut c, r, 14 * u, with_alpha(text, 24));
            }
            // Küçük nota ya da çalıyor göstergesi.
            let mark = Rect::new(r.x + 10 * u, r.y + (r.h - 30 * u) / 2, 30 * u, 30 * u);
            icons::rounded_rect(
                &mut c,
                mark,
                9 * u,
                with_alpha(text, if playing_row { 60 } else { 22 }),
            );
            if playing_row && !self.paused {
                for (k, bar) in [10, 16, 7].iter().enumerate() {
                    let bh = bar * u;
                    let bx = mark.x + 8 * u + k as i32 * 5 * u;
                    icons::rounded_rect(
                        &mut c,
                        Rect::new(bx, mark.bottom() - 7 * u - bh, 3 * u, bh),
                        u,
                        text,
                    );
                }
            } else {
                let (cx, cy) = ((mark.x + 13 * u) as f32, (mark.y + 19 * u) as f32);
                icons::disc(&mut c, cx as i32, cy as i32, 4 * u, text);
                line(
                    &mut c,
                    (cx + 3.5 * u as f32, cy),
                    (cx + 3.5 * u as f32, cy - 12.0 * u as f32),
                    2.0 * u as f32,
                    text,
                );
                line(
                    &mut c,
                    (cx + 3.5 * u as f32, cy - 12.0 * u as f32),
                    (cx + 9.0 * u as f32, cy - 9.0 * u as f32),
                    2.0 * u as f32,
                    text,
                );
            }
            let tx = mark.right() + 12 * u;
            let tw = r.right() - tx - 10 * u;
            f.body
                .draw_clipped(&mut c, tx, r.y + 8 * u, &t.title, tw, text);
            let sub = if t.artist.is_empty() {
                "Bilinmeyen sanatçı"
            } else {
                &t.artist
            };
            f.small
                .draw_clipped(&mut c, tx, r.y + 29 * u, sub, tw, muted);
            hits.push((r.intersect(&list), Control::Track(i)));
        }
        c.clip = saved_clip;
        self.max_scroll = (self.tracks.len() as i32 * row_h - list.h).max(0);

        // ---- Çalan parça ----
        self.glass_panel(&mut c, stage, 26 * u);
        let track = self.playing.as_ref().and_then(|p| self.tracks.get(p.index));
        // Kapağın altı: başlık, alt başlık, ilerleme, denetimler, ses düzeyi ve bilgi satırı.
        let controls_h = 290 * u;
        let side = (stage.h - controls_h - 40 * u)
            .min(stage.w - 80 * u)
            .clamp(80 * u, 380 * u);
        let cover = Rect::new(stage.x + (stage.w - side) / 2, stage.y + 30 * u, side, side);
        c.shadow(cover, 20 * u, 18 * u, 90);
        match self.playing.as_ref().and_then(|p| p.cover.as_ref()) {
            Some(img) => {
                if self.cover_scaled.as_ref().is_none_or(|(s, _)| *s != side) {
                    // Kare kırpılmış kapak.
                    let edge = img.width.min(img.height);
                    let (ox, oy) = ((img.width - edge) / 2, (img.height - edge) / 2);
                    let mut square = Vec::with_capacity((edge * edge) as usize);
                    for y in 0..edge {
                        let start = ((oy + y) * img.width + ox) as usize;
                        square.extend_from_slice(&img.pixels[start..start + edge as usize]);
                    }
                    let sq = Image {
                        width: edge,
                        height: edge,
                        pixels: square,
                    };
                    let mut s = Surface::new(side, side, 0);
                    for (d, p) in s.pixels.iter_mut().zip(sq.scaled(side as u32, side as u32)) {
                        *d = p & 0xff_ffff;
                    }
                    self.cover_scaled = Some((side, s));
                }
                if let Some((_, s)) = &self.cover_scaled {
                    c.blit_masked(s, cover.x, cover.y, 20 * u, 255);
                }
            }
            None => {
                // Kapaksız: cam kare üzerinde büyük nota.
                icons::rounded_rect(&mut c, cover, 20 * u, with_alpha(text, 28));
                rounded_outline(
                    &mut c,
                    cover,
                    20.0 * u as f32,
                    1.5 * u as f32,
                    with_alpha(WHITE, 70),
                );
                let note = side * 2 / 5;
                icons::draw(
                    &mut c,
                    Rect::new(
                        cover.x + (side - note) / 2,
                        cover.y + (side - note) / 2,
                        note,
                        note,
                    ),
                    Icon::Music,
                );
            }
        }

        let mut y = cover.bottom() + 22 * u;
        let (title, subtitle) = match track {
            Some(t) => {
                let mut sub = if t.artist.is_empty() {
                    String::from("Bilinmeyen sanatçı")
                } else {
                    t.artist.clone()
                };
                if !t.album.is_empty() {
                    sub = format!("{} — {}", sub, t.album);
                }
                (t.title.clone(), sub)
            }
            None if self.tracks.is_empty() => (
                String::from("Müzik yok"),
                String::from("~/Müzik klasörüne MP3 ekleyin"),
            ),
            None => (
                String::from("Bir parça seçin"),
                String::from("Kitaplıktan bir parçaya tıklayın ya da boşluğa basın"),
            ),
        };
        let max_text = stage.w - 60 * u;
        let tw = f.heading.measure(&title).min(max_text);
        f.heading.draw_clipped(
            &mut c,
            stage.x + (stage.w - tw) / 2,
            y,
            &title,
            max_text,
            text,
        );
        y += f.heading.line_height() + 4 * u;
        let sw = f.body.measure(&subtitle).min(max_text);
        f.body.draw_clipped(
            &mut c,
            stage.x + (stage.w - sw) / 2,
            y,
            &subtitle,
            max_text,
            muted,
        );
        y += f.body.line_height() + 18 * u;

        // İlerleme çubuğu.
        let bar_w = (stage.w - 120 * u).min(520 * u);
        let bar = Rect::new(stage.x + (stage.w - bar_w) / 2, y, bar_w, 6 * u);
        self.seek_track = bar;
        let permille = match self.seek_preview {
            Some(p) => p,
            None if duration > 0 => (position * 1000 / duration).min(1000) as i32,
            None => 0,
        };
        icons::rounded_rect(&mut c, bar, 3 * u, with_alpha(text, 50));
        let filled = bar.w * permille / 1000;
        if filled > 0 {
            icons::rounded_rect(
                &mut c,
                Rect::new(bar.x, bar.y, filled.max(6 * u), bar.h),
                3 * u,
                text,
            );
        }
        if self.playing.is_some() {
            let knob = if self.drag == Some(Control::Seek) || self.hover == Some(Control::Seek) {
                8
            } else {
                6
            };
            icons::disc(&mut c, bar.x + filled, bar.y + bar.h / 2, knob * u, WHITE);
        }
        hits.push((bar.inset(-10 * u), Control::Seek));
        let shown = match self.seek_preview {
            Some(p) => duration * p as u64 / 1000,
            None => position,
        };
        let left = time_text(shown);
        let right = time_text(duration);
        f.small
            .draw(&mut c, bar.x, bar.bottom() + 8 * u, &left, muted);
        let rw = f.small.measure(&right);
        f.small.draw(
            &mut c,
            bar.right() - rw,
            bar.bottom() + 8 * u,
            &right,
            muted,
        );
        y = bar.bottom() + 34 * u;

        // Denetimler: karıştır, önceki, çal/duraklat, sonraki, tekrar.
        let cy = y + 32 * u;
        let cx = stage.x + stage.w / 2;
        let us = u as f32;
        let play = Rect::new(cx - 32 * u, cy - 32 * u, 64 * u, 64 * u);
        icons::disc(
            &mut c,
            cx,
            cy,
            32 * u,
            if self.hover == Some(Control::PlayPause) {
                WHITE
            } else {
                with_alpha(WHITE, 235)
            },
        );
        let ink = rgb(0x1a, 0x16, 0x26);
        if self.paused || self.playing.is_none() {
            let (fx, fy) = (cx as f32, cy as f32);
            triangle(
                &mut c,
                [
                    (fx - 9.0 * us, fy - 13.0 * us),
                    (fx + 14.0 * us, fy),
                    (fx - 9.0 * us, fy + 13.0 * us),
                ],
                ink,
            );
        } else {
            for dx in [-9, 4] {
                icons::rounded_rect(
                    &mut c,
                    Rect::new(cx + dx * u, cy - 12 * u, 6 * u, 24 * u),
                    2 * u,
                    ink,
                );
            }
        }
        hits.push((play, Control::PlayPause));

        let skip = |c: &mut Canvas, x: i32, forward: bool, hovered: bool| {
            let col = if hovered { WHITE } else { text };
            let (fx, fy) = (x as f32, cy as f32);
            let dir = if forward { 1.0 } else { -1.0 };
            for k in [0.0, 1.0] {
                let ox = fx + dir * (k * 11.0 - 6.0) * us;
                triangle(
                    c,
                    if forward {
                        [
                            (ox - 6.0 * us, fy - 9.0 * us),
                            (ox + 7.0 * us, fy),
                            (ox - 6.0 * us, fy + 9.0 * us),
                        ]
                    } else {
                        [
                            (ox + 6.0 * us, fy + 9.0 * us),
                            (ox - 7.0 * us, fy),
                            (ox + 6.0 * us, fy - 9.0 * us),
                        ]
                    },
                    col,
                );
            }
            let bx = fx + dir * 13.0 * us;
            line(c, (bx, fy - 9.0 * us), (bx, fy + 9.0 * us), 3.0 * us, col);
        };
        let prev_x = cx - 82 * u;
        let next_x = cx + 82 * u;
        skip(&mut c, prev_x, false, self.hover == Some(Control::Previous));
        skip(&mut c, next_x, true, self.hover == Some(Control::Next));
        hits.push((
            Rect::new(prev_x - 24 * u, cy - 24 * u, 48 * u, 48 * u),
            Control::Previous,
        ));
        hits.push((
            Rect::new(next_x - 24 * u, cy - 24 * u, 48 * u, 48 * u),
            Control::Next,
        ));

        // Karıştır.
        let sx = cx - 150 * u;
        let on = |active: bool, hovered: bool| {
            if active {
                accent
            } else if hovered {
                text
            } else {
                muted
            }
        };
        let col = on(self.shuffle, self.hover == Some(Control::Shuffle));
        let (fx, fy) = (sx as f32, cy as f32);
        line(
            &mut c,
            (fx - 10.0 * us, fy - 7.0 * us),
            (fx + 7.0 * us, fy + 7.0 * us),
            2.2 * us,
            col,
        );
        line(
            &mut c,
            (fx - 10.0 * us, fy + 7.0 * us),
            (fx + 7.0 * us, fy - 7.0 * us),
            2.2 * us,
            col,
        );
        triangle(
            &mut c,
            [
                (fx + 5.0 * us, fy - 11.0 * us),
                (fx + 12.0 * us, fy - 7.0 * us),
                (fx + 5.0 * us, fy - 3.0 * us),
            ],
            col,
        );
        triangle(
            &mut c,
            [
                (fx + 5.0 * us, fy + 3.0 * us),
                (fx + 12.0 * us, fy + 7.0 * us),
                (fx + 5.0 * us, fy + 11.0 * us),
            ],
            col,
        );
        if self.shuffle {
            icons::disc(&mut c, sx, cy + 18 * u, 2 * u, accent);
        }
        hits.push((
            Rect::new(sx - 20 * u, cy - 20 * u, 40 * u, 40 * u),
            Control::Shuffle,
        ));

        // Tekrar.
        let rx = cx + 150 * u;
        let col = on(
            self.repeat != Repeat::Off,
            self.hover == Some(Control::Repeat),
        );
        rounded_outline(
            &mut c,
            Rect::new(rx - 11 * u, cy - 8 * u, 22 * u, 16 * u),
            6.0 * us,
            2.2 * us,
            col,
        );
        let (fx, fy) = (rx as f32, cy as f32);
        triangle(
            &mut c,
            [
                (fx + 1.0 * us, fy - 13.0 * us),
                (fx + 7.0 * us, fy - 8.0 * us),
                (fx + 1.0 * us, fy - 3.0 * us),
            ],
            col,
        );
        if self.repeat == Repeat::One {
            let one = "1";
            let ow = f.small.measure(one);
            f.small.draw(
                &mut c,
                rx - ow / 2,
                cy - (f.small.ascent + f.small.descent) / 2,
                one,
                col,
            );
        }
        if self.repeat != Repeat::Off {
            icons::disc(&mut c, rx, cy + 18 * u, 2 * u, accent);
        }
        hits.push((
            Rect::new(rx - 20 * u, cy - 20 * u, 40 * u, 40 * u),
            Control::Repeat,
        ));

        // Ses düzeyi.
        let vy = cy + 56 * u;
        let vol_w = 150 * u;
        let vol = Rect::new(cx - vol_w / 2 + 14 * u, vy, vol_w, 4 * u);
        self.volume_track = vol;
        let (fx, fy) = ((vol.x - 22 * u) as f32, (vy + 2 * u) as f32);
        icons::rounded_rect(
            &mut c,
            Rect::new(fx as i32 - 6 * u, fy as i32 - 4 * u, 5 * u, 8 * u),
            u,
            muted,
        );
        triangle(
            &mut c,
            [
                (fx - 3.0 * us, fy - 4.0 * us),
                (fx + 5.0 * us, fy - 9.0 * us),
                (fx + 5.0 * us, fy + 9.0 * us),
            ],
            muted,
        );
        triangle(
            &mut c,
            [
                (fx - 3.0 * us, fy - 4.0 * us),
                (fx + 5.0 * us, fy + 9.0 * us),
                (fx - 3.0 * us, fy + 4.0 * us),
            ],
            muted,
        );
        icons::rounded_rect(&mut c, vol, 2 * u, with_alpha(text, 50));
        let vf = vol.w * self.volume / 100;
        if vf > 0 {
            icons::rounded_rect(
                &mut c,
                Rect::new(vol.x, vol.y, vf.max(4 * u), vol.h),
                2 * u,
                text,
            );
        }
        icons::disc(&mut c, vol.x + vf, vol.y + vol.h / 2, 6 * u, WHITE);
        hits.push((vol.inset(-10 * u), Control::Volume));

        // Bilgi / hata satırı.
        let note = match &self.message {
            Some(m) => Some((m.clone(), rgb(0xff, 0x8a, 0x80))),
            None if self.output.dsp.is_none() => Some((
                String::from("Ses aygıtı bulunamadı; çalma sessiz sürer."),
                muted,
            )),
            None => None,
        };
        if let Some((m, col)) = note {
            let mw = f.small.measure(&m).min(stage.w - 40 * u);
            f.small.draw_clipped(
                &mut c,
                stage.x + (stage.w - mw) / 2,
                stage.bottom() - 28 * u,
                &m,
                stage.w - 40 * u,
                col,
            );
        }

        drop(c);
        self.window.present();
        self.hits = hits;
        self.last_second = position / 1000;
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("B-Player", (980 * u) as usize, (640 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("b-player: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let info = desktop::query();
    let theme = info
        .as_ref()
        .map(|i| Theme::from(&Prefs::parse(&i.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    let home = info
        .as_ref()
        .map(|i| String::from(i.get("user.home")))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("/home/{}", process::username()));
    let mut tracks = scan_library(&home);
    // Komut satırında verilen dosya (Dosyalar'dan "aç") kitaplıkta yoksa başa eklenir.
    let requested = args.get(1).map(|p| fs::normalize(&process::cwd(), p));
    let mut start = None;
    let mut missing = None;
    if let Some(path) = &requested {
        if let Some(i) = tracks.iter().position(|t| &t.path == path) {
            start = Some(i);
        } else if fs::stat(path).is_ok_and(|st| st.kind == fs::KIND_FILE) {
            tracks.insert(0, read_track(path));
            start = Some(0);
        } else {
            missing = Some(format!("{} bulunamadı", path));
        }
    }
    let dsp = fs::File::open("/dev/dsp", fs::WRITE).ok();
    let mut player = Player {
        window,
        fonts: ui::fonts(),
        u,
        theme,
        home,
        tracks,
        order: Vec::new(),
        playing: None,
        paused: false,
        output: Output {
            dsp,
            anchor_ms: 0,
            written: 0,
        },
        volume: 100,
        shuffle: false,
        repeat: Repeat::Off,
        seed: time::now_ns() | 1,
        message: missing,
        hits: Vec::new(),
        hover: None,
        drag: None,
        seek_preview: None,
        seek_track: Rect::new(0, 0, 0, 0),
        volume_track: Rect::new(0, 0, 0, 0),
        scroll: 0,
        max_scroll: 0,
        background: None,
        cover_scaled: None,
        last_second: u64::MAX,
        dirty: true,
    };
    player.reshuffle();
    if let Some(i) = start {
        player.load(i);
    }
    banna::println!("b-player: ready ({} parça)", player.tracks.len());
    loop {
        let now = time::uptime_ms();
        player.pump(now);
        if player.dirty
            || (player.playing.is_some() && player.position_ms(now) / 1000 != player.last_second)
        {
            player.render(now);
        }
        let active = player.playing.is_some() && !player.paused;
        let mut entries = [PollEntry::window(player.window.id())];
        io::poll(&mut entries, Some(if active { 40 } else { 1000 }));
        while let Some(event) = player.window.event(0) {
            let now = time::uptime_ms();
            match event.kind {
                EVENT_CLOSE => return 0,
                EVENT_RESIZE => {
                    player.window.resize(event.x as usize, event.y as usize);
                    player.cover_scaled = None;
                    player.dirty = true;
                }
                EVENT_MOUSE_DOWN => player.press(event.x, event.y, now),
                EVENT_MOUSE_UP => player.release(),
                EVENT_MOUSE_MOVE => {
                    if player.drag.is_some() {
                        player.drag_to(event.x);
                    }
                    let hovered = player
                        .hits
                        .iter()
                        .rev()
                        .find(|(r, _)| r.contains(event.x, event.y))
                        .map(|(_, c)| *c);
                    if hovered != player.hover {
                        player.hover = hovered;
                        player.dirty = true;
                    }
                }
                EVENT_WHEEL => {
                    player.scroll =
                        (player.scroll + event.key as i32 * 54 * u).clamp(0, player.max_scroll);
                    player.dirty = true;
                }
                EVENT_KEY => player.key(event.key, now),
                _ => {}
            }
        }
    }
}
