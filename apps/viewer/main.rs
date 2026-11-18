//! Görüntüleyici: PNG, BMP ve PPM resimlerini pencereye sığdırarak gösterir. Oklar aynı
//! klasördeki önceki/sonraki resme geçer; +/- yakınlaştırır; D duvar kâğıdı yapar.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::gfx::text::UiFonts;
use banna::gfx::{Rect, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, EVENT_WHEEL,
    KEY_LEFT, KEY_RIGHT, Window,
};
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, Ui};
use banna::{desktop, fs, wallpaper_art};

banna::entry!(main);

#[derive(Clone, PartialEq)]
enum Control {
    Previous,
    Next,
    ZoomOut,
    ZoomIn,
    Fit,
    Wallpaper,
}

struct Picture {
    width: u32,
    height: u32,
    pixels: Vec<u32>,
}

struct Viewer {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    path: String,
    siblings: Vec<String>,
    picture: Option<Picture>,
    /// `None`: pencereye sığdır; aksi halde yüzde.
    zoom: Option<u32>,
    scaled: Option<(u32, u32, Vec<u32>)>,
    message: Option<String>,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    dirty: bool,
}

fn is_image(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".png") || lower.ends_with(".bmp") || lower.ends_with(".ppm")
}

fn load(path: &str) -> Result<Picture, String> {
    let data = fs::read(path).map_err(|e| String::from(fs::error_text(e)))?;
    if let Some(png) = banna::gfx::png::decode(&data) {
        return Ok(Picture {
            width: png.width,
            height: png.height,
            pixels: png.pixels,
        });
    }
    let image = wallpaper_art::decode(&data).ok_or_else(|| String::from("Desteklenmeyen biçim"))?;
    Ok(Picture {
        width: image.width as u32,
        height: image.height as u32,
        pixels: image.pixels.iter().map(|p| p | 0xff00_0000).collect(),
    })
}

/// En yakın komşu büyütme / alan ortalamalı küçültme.
fn scale(p: &Picture, w: u32, h: u32) -> Vec<u32> {
    if w <= p.width && h <= p.height {
        return wallpaper_art::scale_argb(&p.pixels, p.width, p.height, w, h);
    }
    let mut out = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        let row = (y * p.height / h) * p.width;
        for x in 0..w {
            out.push(p.pixels[(row + x * p.width / w) as usize]);
        }
    }
    out
}

impl Viewer {
    fn open(&mut self, path: &str) {
        self.path = String::from(path);
        self.zoom = None;
        self.scaled = None;
        match load(path) {
            Ok(p) => {
                self.picture = Some(p);
                self.message = None;
            }
            Err(e) => {
                self.picture = None;
                self.message = Some(format!("{}: {}", path, e));
            }
        }
        let dir = match path.rfind('/') {
            Some(0) | None => String::from("/"),
            Some(i) => String::from(&path[..i]),
        };
        let mut names: Vec<String> = fs::read_dir(&dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|(n, st)| st.kind == fs::KIND_FILE && is_image(n))
            .map(|(n, _)| {
                if dir == "/" {
                    format!("/{}", n)
                } else {
                    format!("{}/{}", dir, n)
                }
            })
            .collect();
        names.sort_by_key(|n| n.to_lowercase());
        self.siblings = names;
        self.dirty = true;
    }

    fn step(&mut self, delta: isize) {
        let count = self.siblings.len() as isize;
        if count == 0 {
            return;
        }
        let at = self
            .siblings
            .iter()
            .position(|p| *p == self.path)
            .unwrap_or(0) as isize;
        let next = self.siblings[((at + delta).rem_euclid(count)) as usize].clone();
        self.open(&next);
    }

    fn set_zoom(&mut self, zoom: Option<u32>) {
        self.zoom = zoom.map(|z| z.clamp(10, 800));
        self.scaled = None;
        self.dirty = true;
    }

    fn current_percent(&self, area: Rect) -> u32 {
        let Some(p) = &self.picture else {
            return 100;
        };
        match self.zoom {
            Some(z) => z,
            None => {
                let fx = area.w as u64 * 100 / p.width.max(1) as u64;
                let fy = area.h as u64 * 100 / p.height.max(1) as u64;
                fx.min(fy).clamp(1, 100) as u32
            }
        }
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(control) = ui::hit_at(&self.hits, x, y).map(|h| h.control.clone()) else {
            return;
        };
        let area = self.image_area();
        let now = self.current_percent(area);
        match control {
            Control::Previous => self.step(-1),
            Control::Next => self.step(1),
            Control::ZoomOut => self.set_zoom(Some(now * 4 / 5)),
            Control::ZoomIn => self.set_zoom(Some(now * 5 / 4 + 1)),
            Control::Fit => self.set_zoom(None),
            Control::Wallpaper => self.press_wallpaper(),
        }
    }

    fn key(&mut self, key: u32) -> bool {
        let area = self.image_area();
        let now = self.current_percent(area);
        match key {
            27 | 0x71 => return false,
            KEY_LEFT => self.step(-1),
            KEY_RIGHT | 0x20 => self.step(1),
            0x2b | 0x3d => self.set_zoom(Some(now * 5 / 4 + 1)),
            0x2d => self.set_zoom(Some(now * 4 / 5)),
            0x30 => self.set_zoom(None),
            0x31 => self.set_zoom(Some(100)),
            0x64 => self.press_wallpaper(),
            _ => {}
        }
        true
    }

    fn press_wallpaper(&mut self) {
        let ok = desktop::set("wallpaper", &format!("image:{}", self.path)) >= 0;
        self.message = Some(String::from(if ok {
            "Duvar kâğıdı olarak ayarlandı"
        } else {
            "Duvar kâğıdı ayarlanamadı"
        }));
        self.dirty = true;
    }

    fn image_area(&self) -> Rect {
        let bar = 52 * self.u;
        Rect::new(
            0,
            0,
            self.window.width as i32,
            self.window.height as i32 - bar,
        )
    }

    fn render(&mut self) {
        self.dirty = false;
        let u = self.u;
        let t = self.theme;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let area = self.image_area();
        let percent = self.current_percent(area);
        if let Some(p) = &self.picture {
            let sw = (p.width as u64 * percent as u64 / 100).max(1) as u32;
            let sh = (p.height as u64 * percent as u64 / 100).max(1) as u32;
            if self.scaled.as_ref().is_none_or(|s| s.0 != sw || s.1 != sh) {
                self.scaled = Some((sw, sh, scale(p, sw, sh)));
            }
        }
        let mut hits = Vec::new();
        let mut c = self.window.canvas();
        let background = if t.dark { 0xff0d_1017 } else { 0xffe9_ecf1 };
        c.fill(Rect::new(0, 0, w, h), background);
        c.clip = area;
        if let Some((sw, sh, pixels)) = &self.scaled {
            let (sw, sh) = (*sw as i32, *sh as i32);
            let x = area.x + (area.w - sw) / 2;
            let y = area.y + (area.h - sh) / 2;
            // Saydam resimler için dama deseni.
            let tile = 8 * u;
            let frame = Rect::new(x, y, sw, sh).intersect(&area);
            let mut ty = frame.y;
            while ty < frame.bottom() {
                let mut tx = frame.x;
                while tx < frame.right() {
                    let odd = ((tx - x) / tile + (ty - y) / tile) % 2 == 1;
                    let cell = Rect::new(tx, ty, tile, tile).intersect(&frame);
                    c.fill(cell, if odd { 0xffcc_cccc } else { 0xffff_ffff });
                    tx += tile;
                }
                ty += tile;
            }
            c.blend_image(pixels, sw, sh, x, y);
        } else if let Some(m) = &self.message {
            let mw = self.fonts.body.measure(m);
            self.fonts
                .body
                .draw(&mut c, w / 2 - mw / 2, area.h / 2, m, t.muted);
        }
        c.clip = Rect::new(0, 0, w, h);
        let bar = Rect::new(0, area.bottom(), w, h - area.bottom());
        c.fill(bar, t.sidebar);
        c.fill(Rect::new(0, bar.y, w, u), with_alpha(t.text, 30));
        let f = &self.fonts;
        let mut ui = Ui::new(
            &mut c,
            t,
            f,
            u,
            Rect::new(0, 0, w, h),
            0,
            &mut hits,
            self.hover,
        );
        let by = bar.y + 9 * u;
        let bh = 34 * u;
        let mut x = 12 * u;
        for (label, control) in [("←", Control::Previous), ("→", Control::Next)] {
            ui.secondary_button(Rect::new(x, by, 40 * u, bh), label, control);
            x += 46 * u;
        }
        let name = self.path.rsplit('/').next().unwrap_or("");
        let info = match (&self.picture, &self.message) {
            (_, Some(m)) if self.picture.is_some() => format!("{} — {}", name, m),
            (Some(p), _) => format!("{}  •  {}×{}  •  %{}", name, p.width, p.height, percent),
            _ => String::from(name),
        };
        let mut rx = w - 12 * u;
        for (label, control) in [
            ("Duvar kâğıdı yap", Control::Wallpaper),
            ("Sığdır", Control::Fit),
            ("+", Control::ZoomIn),
            ("–", Control::ZoomOut),
        ] {
            let bw = (ui.f.body.measure(label) + 24 * u).max(40 * u);
            rx -= bw;
            ui.secondary_button(Rect::new(rx, by, bw, bh), label, control);
            rx -= 6 * u;
        }
        ui.f.small
            .draw_clipped(ui.c, x + 8 * u, by + 9 * u, &info, rx - x - 16 * u, t.text);
        drop(ui);
        self.window.present();
        self.hits = hits;
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("Görüntüleyici", (820 * u) as usize, (600 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("viewer: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let theme = desktop::query()
        .map(|i| Theme::from(&Prefs::parse(&i.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    let mut app = Viewer {
        window,
        fonts: ui::fonts(),
        u,
        theme,
        path: String::new(),
        siblings: Vec::new(),
        picture: None,
        zoom: None,
        scaled: None,
        message: Some(String::from(
            "Açılacak resim yok (Dosyalar'dan bir resim seçin)",
        )),
        hits: Vec::new(),
        hover: None,
        dirty: true,
    };
    if let Some(path) = args.get(1) {
        app.open(path);
    }
    banna::println!("viewer: ready");
    loop {
        if app.dirty {
            app.render();
        }
        let Some(event) = app.window.event(10_000) else {
            continue;
        };
        match event.kind {
            EVENT_CLOSE => return 0,
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                app.scaled = None;
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    app.dirty = true;
                }
            }
            EVENT_WHEEL => {
                let now = app.current_percent(app.image_area());
                if (event.key as i32) < 0 {
                    app.set_zoom(Some(now * 5 / 4 + 1));
                } else {
                    app.set_zoom(Some(now * 4 / 5));
                }
            }
            EVENT_KEY => {
                if !app.key(event.key) {
                    return 0;
                }
                app.dirty = true;
            }
            _ => {}
        }
    }
}
