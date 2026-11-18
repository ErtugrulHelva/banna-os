//! Uygulama başlatıcı: paneldeki "B" logosuna (ya da Super tuşuna) basınca açılan menü.
//! Arama kutusu, uygulama ızgarası, oturum ve güç düğmeleri; açılıp kapanırken kayarak
//! belirir. İçerik kendi yüzeyine çizilir, sahnede saydamlıkla bindirilir.

use alloc::string::String;
use alloc::vec::Vec;

use super::apps::{self, App};
use super::icons;
use super::m;
use super::prefs::Theme;
use crate::drivers::keyboard::{Key, KeyEvent};
use crate::gfx::text::UiFonts;
use crate::gfx::{Canvas, Rect, Surface, with_alpha};

const OPEN_MS: u64 = 200;
const CLOSE_MS: u64 = 140;
const COLUMNS: usize = 4;

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Target {
    App(String),
    Logout,
    Reboot,
    Shutdown,
}

pub enum Outcome {
    None,
    Redraw,
    Close,
    Run(Target),
}

/// Başlatıcının cam arka planı: (konum, görünüm, arka plan kuşağı) → pikseller.
type BackgroundKey = (Rect, crate::gfx::Glass, u64);
static BACKGROUND: crate::sync::SpinLock<Option<(BackgroundKey, Vec<u32>)>> =
    crate::sync::SpinLock::new(None);

pub struct Launcher {
    pub query: String,
    selected: usize,
    hovered: Option<Target>,
    installed: Vec<App>,
    opened_ms: u64,
    closing_ms: Option<u64>,
    animate: bool,
    pub surface: Surface,
    hits: Vec<(Rect, Target)>,
    dirty: bool,
    keyboard: bool,
}

fn ease_out(t: u64, duration: u64) -> u32 {
    let p = (t.min(duration) * 256 / duration.max(1)) as i64;
    let inv = 256 - p;
    (256 - inv * inv * inv / (256 * 256)) as u32
}

/// Arama için katlama: küçük harf ve Türkçe harflerin ASCII karşılıkları (ı/i, ş/s...).
fn lower(text: &str) -> String {
    text.chars()
        .flat_map(|c| c.to_lowercase())
        .map(|c| match c {
            'ı' => 'i',
            'ş' => 's',
            'ğ' => 'g',
            'ü' => 'u',
            'ö' => 'o',
            'ç' => 'c',
            '\u{307}' => '\0',
            c => c,
        })
        .filter(|&c| c != '\0')
        .collect()
}

impl Launcher {
    pub fn new(now_ms: u64, animate: bool) -> Self {
        let installed = apps::installed();
        let size = Self::size(installed.len());
        Self {
            query: String::new(),
            selected: 0,
            hovered: None,
            installed,
            opened_ms: now_ms,
            closing_ms: None,
            animate,
            surface: Surface::new(size.0, size.1, 0),
            hits: Vec::new(),
            dirty: true,
            keyboard: false,
        }
    }

    fn size(count: usize) -> (i32, i32) {
        let u = m().unit;
        let rows = count.div_ceil(COLUMNS).max(1) as i32;
        (
            COLUMNS as i32 * 124 * u + 2 * 20 * u,
            20 * u + 40 * u + 20 * u + rows * 112 * u + 16 * u + 64 * u,
        )
    }

    /// Ekrandaki konumu (panelin altında, sol üstte).
    pub fn frame(&self) -> Rect {
        let u = m().unit;
        Rect::new(
            8 * u,
            m().panel_h + 8 * u,
            self.surface.width,
            self.surface.height,
        )
    }

    /// Animasyon ve gölge dahil çizim alanı.
    pub fn footprint(&self) -> Rect {
        let u = m().unit;
        self.frame()
            .inset(-m().shadow)
            .union(&self.frame().offset(0, -14 * u))
    }

    fn matches(&self) -> Vec<App> {
        let q = lower(self.query.trim());
        self.installed
            .iter()
            .filter(|app| {
                q.is_empty()
                    || lower(&app.name).contains(&q)
                    || lower(&app.description).contains(&q)
            })
            .cloned()
            .collect()
    }

    /// 0..=256 görünürlük; kapanma bittiyse `None`.
    pub fn progress(&self, now_ms: u64) -> Option<u32> {
        if !self.animate {
            return self.closing_ms.is_none().then_some(256);
        }
        match self.closing_ms {
            Some(start) => {
                let t = now_ms.saturating_sub(start);
                (t < CLOSE_MS).then(|| 256 - ease_out(t, CLOSE_MS))
            }
            None => Some(ease_out(now_ms.saturating_sub(self.opened_ms), OPEN_MS)),
        }
    }

    pub fn animating(&self, now_ms: u64) -> bool {
        self.animate
            && (self.closing_ms.is_some() || now_ms.saturating_sub(self.opened_ms) < OPEN_MS)
    }

    pub fn closing(&self) -> bool {
        self.closing_ms.is_some()
    }

    pub fn close(&mut self, now_ms: u64) {
        if self.closing_ms.is_none() {
            self.closing_ms = Some(now_ms);
        }
    }

    /// Cam arka planı önceden hesaplar (masaüstü açılışında ve duvar kağıdı değişince),
    /// böylece başlatıcı ilk açılışta beklemez.
    pub fn prewarm(theme: &Theme, f: &UiFonts, backdrop: &crate::gfx::Surface, backdrop_gen: u64) {
        let mut l = Self::new(0, false);
        l.render_if_needed(theme, f, backdrop, backdrop_gen);
    }

    pub fn render_if_needed(
        &mut self,
        theme: &Theme,
        f: &UiFonts,
        backdrop: &crate::gfx::Surface,
        backdrop_gen: u64,
    ) {
        if !self.dirty {
            return;
        }
        self.dirty = false;
        let u = m().unit;
        let matches = self.matches();
        self.selected = self.selected.min(matches.len().saturating_sub(1));
        let (w, h) = (self.surface.width, self.surface.height);
        self.hits.clear();
        let key = (self.frame(), super::draw::sheet_glass(theme), backdrop_gen);
        {
            let mut cached = BACKGROUND.lock();
            if !matches!(&*cached, Some((k, _)) if *k == key) {
                self.surface.canvas().glass(
                    Rect::new(0, 0, w, h),
                    16 * u,
                    backdrop,
                    (key.0.x, key.0.y),
                    &key.1,
                );
                *cached = Some((key, self.surface.pixels.clone()));
            } else if let Some((_, bg)) = &*cached {
                self.surface.pixels.copy_from_slice(bg);
            }
        }
        let mut c = self.surface.canvas();

        // Arama kutusu.
        let pad = 20 * u;
        let search = Rect::new(pad, pad, w - 2 * pad, 40 * u);
        icons::rounded_rect(
            &mut c,
            search,
            search.h / 2,
            with_alpha(
                if theme.dark { 0xffffff } else { 0 },
                if theme.dark { 22 } else { 14 },
            ),
        );
        icons::circle_outline(
            &mut c,
            search.x + 22 * u,
            search.y + 18 * u,
            7 * u,
            2 * u,
            theme.muted,
        );
        icons::rounded_rect(
            &mut c,
            Rect::new(search.x + 26 * u, search.y + 24 * u, 7 * u, 3 * u),
            u,
            theme.muted,
        );
        let ty = search.y + (search.h - f.body.ascent - f.body.descent) / 2;
        if self.query.is_empty() {
            f.body.draw(
                &mut c,
                search.x + 40 * u,
                ty,
                "Uygulama ara...",
                theme.muted,
            );
        } else {
            let end = f
                .body
                .draw(&mut c, search.x + 40 * u, ty, &self.query, theme.text);
            c.fill(
                Rect::new(end + 2 * u, ty, 2 * u, f.body.ascent + f.body.descent),
                theme.accent,
            );
        }

        // Uygulama ızgarası.
        let grid_y = search.bottom() + 20 * u;
        let tile_w = 124 * u;
        let tile_h = 112 * u;
        if matches.is_empty() {
            let text = "Eşleşen uygulama yok";
            let tw = f.body.measure(text);
            f.body
                .draw(&mut c, w / 2 - tw / 2, grid_y + 40 * u, text, theme.muted);
        }
        for (i, app) in matches.iter().enumerate() {
            let col = (i % COLUMNS) as i32;
            let row = (i / COLUMNS) as i32;
            let tile = Rect::new(
                pad + col * tile_w,
                grid_y + row * tile_h,
                tile_w,
                tile_h - 8 * u,
            );
            let target = Target::App(app.id.clone());
            let selected = i == self.selected && (self.keyboard || !self.query.is_empty());
            if self.hovered.as_ref() == Some(&target) || selected {
                icons::rounded_rect(&mut c, tile.inset(2 * u), 14 * u, theme.hover);
            }
            let size = 56 * u;
            let icon = Rect::new(tile.x + tile.w / 2 - size / 2, tile.y + 10 * u, size, size);
            super::apps::draw_icon_cached(&mut c, icon, app, f.title);
            let label_w = f.small.measure(&app.name).min(tile.w - 8 * u);
            f.small.draw_clipped(
                &mut c,
                tile.x + tile.w / 2 - label_w / 2,
                icon.bottom() + 8 * u,
                &app.name,
                tile.w - 8 * u,
                theme.text,
            );
            self.hits.push((tile, target));
        }

        // Alt şerit: kullanıcı ve güç düğmeleri.
        let footer = Rect::new(0, h - 64 * u, w, 64 * u);
        c.fill(
            footer,
            with_alpha(
                if theme.dark { 0 } else { 0xffffff },
                if theme.dark { 70 } else { 90 },
            ),
        );
        c.fill(
            Rect::new(0, footer.y, w, 1),
            with_alpha(0xffffff, if theme.dark { 16 } else { 90 }),
        );
        let name = crate::kernel::users::session()
            .map(|s| s.name)
            .unwrap_or_default();
        let avatar_r = 16 * u;
        let (ax, ay) = (pad + avatar_r, footer.y + footer.h / 2);
        icons::disc(&mut c, ax, ay, avatar_r, theme.accent);
        let initial: String = name
            .chars()
            .take(1)
            .flat_map(|c| c.to_uppercase())
            .collect();
        let iw = f.title.measure(&initial);
        f.title.draw(
            &mut c,
            ax - iw / 2,
            ay - (f.title.ascent + f.title.descent) / 2,
            &initial,
            crate::gfx::rgb(0xff, 0xff, 0xff),
        );
        f.body.draw(
            &mut c,
            ax + avatar_r + 12 * u,
            ay - (f.body.ascent + f.body.descent) / 2,
            &name,
            theme.text,
        );
        let mut bx = w - pad;
        for (label, target) in [
            ("Kapat", Target::Shutdown),
            ("Yeniden başlat", Target::Reboot),
            ("Oturumu kapat", Target::Logout),
        ] {
            let bw = f.small.measure(label) + 24 * u;
            let button = Rect::new(bx - bw, ay - 15 * u, bw, 30 * u);
            let hovered = self.hovered.as_ref() == Some(&target);
            let color = if hovered {
                if target == Target::Shutdown {
                    crate::gfx::rgb(0xe0, 0x4a, 0x42)
                } else {
                    theme.control
                }
            } else {
                with_alpha(
                    if theme.dark { 0xffffff } else { 0 },
                    if theme.dark { 20 } else { 12 },
                )
            };
            icons::rounded_rect(&mut c, button, button.h / 2, color);
            let fg = if hovered && target == Target::Shutdown {
                crate::gfx::rgb(0xff, 0xff, 0xff)
            } else {
                theme.text
            };
            f.small.draw(
                &mut c,
                button.x + 12 * u,
                button.y + (button.h - f.small.ascent - f.small.descent) / 2,
                label,
                fg,
            );
            self.hits.push((button, target));
            bx = button.x - 8 * u;
        }
    }

    /// Sahneye bindirir: aşağı kayarak ve saydamlaşarak belirir.
    pub fn draw(&self, canvas: &mut Canvas, progress: u32) {
        let u = m().unit;
        let frame = self.frame();
        let offset = -((256 - progress as i32) * 14 * u / 256);
        let at = frame.offset(0, offset);
        let alpha = progress.min(255) as u8;
        canvas.shadow(at, 16 * u, m().shadow, (110 * progress / 256) as u8);
        canvas.blit_masked(&self.surface, at.x, at.y, 16 * u, alpha);
    }

    fn hit(&self, x: i32, y: i32) -> Option<Target> {
        let frame = self.frame();
        let (lx, ly) = (x - frame.x, y - frame.y);
        self.hits
            .iter()
            .find(|(r, _)| r.contains(lx, ly))
            .map(|(_, t)| t.clone())
    }

    pub fn contains(&self, x: i32, y: i32) -> bool {
        self.frame().contains(x, y)
    }

    pub fn hover(&mut self, x: i32, y: i32) -> bool {
        let hovered = self.hit(x, y);
        if hovered != self.hovered {
            self.hovered = hovered;
            self.dirty = true;
            return true;
        }
        false
    }

    pub fn click(&mut self, x: i32, y: i32) -> Outcome {
        if !self.contains(x, y) {
            return Outcome::Close;
        }
        match self.hit(x, y) {
            Some(t) => Outcome::Run(t),
            None => Outcome::None,
        }
    }

    pub fn key(&mut self, event: KeyEvent) -> Outcome {
        let count = self.matches().len();
        match event.key {
            Key::Escape | Key::Super => return Outcome::Close,
            Key::Enter => {
                return match self.matches().get(self.selected) {
                    Some(app) => Outcome::Run(Target::App(app.id.clone())),
                    None => Outcome::None,
                };
            }
            Key::Backspace => {
                self.query.pop();
                self.selected = 0;
            }
            Key::Left => self.selected = self.selected.saturating_sub(1),
            Key::Right => self.selected = (self.selected + 1).min(count.saturating_sub(1)),
            Key::Up => self.selected = self.selected.saturating_sub(COLUMNS),
            Key::Down => self.selected = (self.selected + COLUMNS).min(count.saturating_sub(1)),
            Key::Char(ch) if !ch.is_control() && self.query.chars().count() < 32 => {
                self.query.push(ch);
                self.selected = 0;
            }
            _ => return Outcome::None,
        }
        self.keyboard = true;
        self.dirty = true;
        Outcome::Redraw
    }
}
