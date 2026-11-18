//! Masaüstü tercihleri ve tema renkleri: `anahtar=değer` biçimi, varsayılanlar.
//!
//! Çekirdeğe bağımlı değildir; Ayarlar uygulaması (`banna::theme`) aynı dosyayı kullanır.

use alloc::format;
use alloc::string::{String, ToString};

use crate::gfx::{Argb, rgb};

pub const ACCENTS: [(&str, Argb); 8] = [
    ("Mavi", rgb(0x5f, 0xb4, 0xff)),
    ("Mor", rgb(0xa2, 0x7b, 0xff)),
    ("Pembe", rgb(0xff, 0x6f, 0xb1)),
    ("Kırmızı", rgb(0xff, 0x6b, 0x6b)),
    ("Turuncu", rgb(0xff, 0xa2, 0x4d)),
    ("Sarı", rgb(0xf2, 0xc8, 0x4b)),
    ("Yeşil", rgb(0x4f, 0xd1, 0x8b)),
    ("Turkuaz", rgb(0x3c, 0xc9, 0xd6)),
];

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum Wallpaper {
    Preset(u8),
    Solid(u8),
    Image(String),
}

#[derive(Clone, Debug)]
pub struct Prefs {
    pub dark: bool,
    pub accent: u8,
    pub wallpaper: Wallpaper,
    pub wallpaper_dim: u8,
    pub panel_opacity: u8,
    pub animations: bool,
    pub shadows: bool,
    pub night_light: bool,
    pub night_strength: u8,
    pub show_fps: bool,
    /// Kare kopyalamasını dikey boşluğa denk getir (destekleyen ekranlarda).
    pub vsync: bool,
    pub parallel: bool,
    pub screen_off_min: u8,
    pub clock_24h: bool,
    pub clock_seconds: bool,
    pub show_date: bool,
    pub dock_size: u8,
    pub dock_magnify: bool,
    pub dock_running: bool,
    /// Kullanıcının dock'a eklediği / varsayılandan kaldırdığı uygulamalar (virgülle).
    pub dock_add: String,
    pub dock_remove: String,
    /// Masaüstünde `~/Desktop` simgeleri.
    pub desktop_icons: bool,
    pub status_items: bool,
    pub ui_sounds: bool,
    pub lid_action: u8,
    pub power_button: u8,
    pub muted: bool,
    /// Sistem ayarları: dosyada yoksa `None` (sürücünün varsayılanı geçerli).
    pub volume: Option<u8>,
    pub brightness: Option<u8>,
    pub profile: Option<u8>,
    pub layout_us: Option<bool>,
    pub mouse_speed: Option<u16>,
    pub left_handed: Option<bool>,
    pub tz_minutes: Option<i16>,
    pub resolution: Option<(u32, u32)>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            dark: true,
            accent: 0,
            wallpaper: Wallpaper::Preset(super::wallpaper_art::DEFAULT_PRESET),
            wallpaper_dim: 0,
            panel_opacity: 80,
            animations: true,
            shadows: true,
            night_light: false,
            night_strength: 35,
            show_fps: true,
            vsync: true,
            parallel: true,
            screen_off_min: 0,
            clock_24h: true,
            clock_seconds: true,
            show_date: true,
            dock_size: 1,
            dock_magnify: true,
            dock_running: true,
            dock_add: String::new(),
            dock_remove: String::new(),
            desktop_icons: true,
            status_items: true,
            ui_sounds: true,
            lid_action: 0,
            power_button: 0,
            muted: false,
            volume: None,
            brightness: None,
            profile: None,
            layout_us: None,
            mouse_speed: None,
            left_handed: None,
            tz_minutes: None,
            resolution: None,
        }
    }
}

fn flag(v: bool) -> &'static str {
    if v { "1" } else { "0" }
}

impl Prefs {
    pub fn accent_color(&self) -> Argb {
        ACCENTS[self.accent as usize % ACCENTS.len()].1
    }

    pub fn serialize(&self) -> String {
        let wallpaper = match &self.wallpaper {
            Wallpaper::Preset(i) => format!("preset:{}", i),
            Wallpaper::Solid(i) => format!("solid:{}", i),
            Wallpaper::Image(path) => format!("image:{}", path),
        };
        let mut out = String::from("# Banna OS masaüstü ayarları (Ayarlar uygulaması yazar)\n");
        let mut put = |k: &str, v: String| {
            out.push_str(k);
            out.push('=');
            out.push_str(&v);
            out.push('\n');
        };
        put("dark", flag(self.dark).into());
        put("accent", self.accent.to_string());
        put("wallpaper", wallpaper);
        put("wallpaper_dim", self.wallpaper_dim.to_string());
        put("panel_opacity", self.panel_opacity.to_string());
        put("animations", flag(self.animations).into());
        put("shadows", flag(self.shadows).into());
        put("night_light", flag(self.night_light).into());
        put("night_strength", self.night_strength.to_string());
        put("show_fps", flag(self.show_fps).into());
        put("vsync", flag(self.vsync).into());
        put("parallel", flag(self.parallel).into());
        put("screen_off", self.screen_off_min.to_string());
        put("clock_24h", flag(self.clock_24h).into());
        put("clock_seconds", flag(self.clock_seconds).into());
        put("show_date", flag(self.show_date).into());
        put("dock_size", self.dock_size.to_string());
        put("dock_magnify", flag(self.dock_magnify).into());
        put("dock_running", flag(self.dock_running).into());
        put("dock_add", self.dock_add.clone());
        put("dock_remove", self.dock_remove.clone());
        put("desktop_icons", flag(self.desktop_icons).into());
        put("status_items", flag(self.status_items).into());
        put("ui_sounds", flag(self.ui_sounds).into());
        put("lid_action", self.lid_action.to_string());
        put("power_button", self.power_button.to_string());
        put("muted", flag(self.muted).into());
        if let Some(v) = self.volume {
            put("volume", v.to_string());
        }
        if let Some(v) = self.brightness {
            put("brightness", v.to_string());
        }
        if let Some(v) = self.profile {
            put("profile", v.to_string());
        }
        if let Some(v) = self.layout_us {
            put("layout", if v { "us" } else { "tr" }.into());
        }
        if let Some(v) = self.mouse_speed {
            put("mouse_speed", v.to_string());
        }
        if let Some(v) = self.left_handed {
            put("left_handed", flag(v).into());
        }
        if let Some(v) = self.tz_minutes {
            put("timezone", v.to_string());
        }
        if let Some((w, h)) = self.resolution {
            put("resolution", format!("{}x{}", w, h));
        }
        out
    }

    pub fn parse(text: &str) -> Self {
        let mut p = Self::default();
        for line in text.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let (key, value) = (key.trim(), value.trim());
            let b = value == "1";
            let n = value.parse::<i64>().ok();
            let u8v = n.map(|v| v.clamp(0, 255) as u8);
            match key {
                "dark" => p.dark = b,
                "accent" => p.accent = u8v.unwrap_or(0) % ACCENTS.len() as u8,
                "wallpaper" => {
                    p.wallpaper = match value.split_once(':') {
                        Some(("preset", i)) => Wallpaper::Preset(i.parse().unwrap_or(0)),
                        Some(("solid", i)) => Wallpaper::Solid(i.parse().unwrap_or(0)),
                        Some(("image", path)) if path.starts_with('/') => {
                            Wallpaper::Image(path.into())
                        }
                        _ => Wallpaper::Preset(0),
                    }
                }
                "wallpaper_dim" => p.wallpaper_dim = u8v.unwrap_or(0).min(80),
                "panel_opacity" => p.panel_opacity = u8v.unwrap_or(80).clamp(30, 100),
                "animations" => p.animations = b,
                "shadows" => p.shadows = b,
                "night_light" => p.night_light = b,
                "night_strength" => p.night_strength = u8v.unwrap_or(35).clamp(10, 80),
                "show_fps" => p.show_fps = b,
                "vsync" => p.vsync = b,
                "parallel" => p.parallel = b,
                "screen_off" => p.screen_off_min = u8v.unwrap_or(0),
                "clock_24h" => p.clock_24h = b,
                "clock_seconds" => p.clock_seconds = b,
                "show_date" => p.show_date = b,
                "dock_size" => p.dock_size = u8v.unwrap_or(1).min(2),
                "dock_magnify" => p.dock_magnify = b,
                "dock_running" => p.dock_running = b,
                "dock_add" => p.dock_add = value.into(),
                "dock_remove" => p.dock_remove = value.into(),
                "desktop_icons" => p.desktop_icons = b,
                "status_items" => p.status_items = b,
                "ui_sounds" => p.ui_sounds = b,
                "lid_action" => p.lid_action = u8v.unwrap_or(0).min(2),
                "power_button" => p.power_button = u8v.unwrap_or(0).min(1),
                "muted" => p.muted = b,
                "volume" => p.volume = u8v.map(|v| v.min(100)),
                "brightness" => p.brightness = u8v.map(|v| v.min(100)),
                "profile" => p.profile = u8v.map(|v| v.min(2)),
                "layout" => p.layout_us = Some(value == "us"),
                "mouse_speed" => p.mouse_speed = n.map(|v| v.clamp(20, 400) as u16),
                "left_handed" => p.left_handed = Some(b),
                "timezone" => p.tz_minutes = n.map(|v| v.clamp(-720, 840) as i16),
                "resolution" => {
                    p.resolution = value
                        .split_once('x')
                        .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
                }
                _ => {}
            }
        }
        p
    }
}

/// Tema renkleri (koyu/açık + vurgu rengi).
#[derive(Clone, Copy)]
pub struct Theme {
    pub dark: bool,
    pub accent: Argb,
    pub window_bg: Argb,
    /// Uygulamalar kullanır (`banna::theme`).
    #[allow(dead_code)]
    pub sidebar: Argb,
    pub title_bg: Argb,
    pub title_active: Argb,
    pub text: Argb,
    pub muted: Argb,
    #[allow(dead_code)]
    pub card: Argb,
    pub control: Argb,
    pub hover: Argb,
    pub panel: Argb,
    pub dock: Argb,
    pub shadows: bool,
}

impl Theme {
    pub fn from(p: &Prefs) -> Self {
        let alpha = (p.panel_opacity as u32 * 255 / 100) as u8;
        if p.dark {
            Self {
                dark: true,
                accent: p.accent_color(),
                window_bg: rgb(0x1c, 0x1c, 0x1f),
                sidebar: rgb(0x16, 0x16, 0x19),
                title_bg: rgb(0x2a, 0x2a, 0x2e),
                title_active: rgb(0x34, 0x34, 0x39),
                text: rgb(0xf2, 0xf2, 0xf4),
                muted: rgb(0x9a, 0x9a, 0xa2),
                card: rgb(0x26, 0x26, 0x2a),
                control: rgb(0x3c, 0x3c, 0x42),
                hover: rgb(0x30, 0x30, 0x35),
                panel: crate::gfx::with_alpha(rgb(0x12, 0x12, 0x15), alpha / 2),
                dock: crate::gfx::with_alpha(rgb(0x18, 0x18, 0x1c), alpha / 2),
                shadows: p.shadows,
            }
        } else {
            Self {
                dark: false,
                accent: p.accent_color(),
                window_bg: rgb(0xf6, 0xf6, 0xf8),
                sidebar: rgb(0xee, 0xee, 0xf1),
                title_bg: rgb(0xec, 0xec, 0xef),
                title_active: rgb(0xf8, 0xf8, 0xfa),
                text: rgb(0x1d, 0x1d, 0x1f),
                muted: rgb(0x6e, 0x6e, 0x76),
                card: rgb(0xff, 0xff, 0xff),
                control: rgb(0xd8, 0xd8, 0xdd),
                hover: rgb(0xe8, 0xe8, 0xec),
                panel: crate::gfx::with_alpha(rgb(0xff, 0xff, 0xff), alpha / 2),
                dock: crate::gfx::with_alpha(rgb(0xff, 0xff, 0xff), alpha / 2),
                shadows: p.shadows,
            }
        }
    }
}
