//! Ayarlar: kenar çubuğunda sayfalar, sağda anahtarlar, kaydırıcılar ve seçim düğmeleri.
//!
//! Ayarları masaüstünden okur (`desktop::query`) ve değişiklikleri ona gönderir
//! (`desktop::set`); masaüstü uygular ve `/etc/banna/desktop.conf`'a kaydeder.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use banna::desktop::{self, Info};
use banna::gfx::text::UiFonts;
use banna::gfx::{Rect, Surface, with_alpha};
use banna::gui::clipboard;
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
    EVENT_WHEEL, Window,
};
use banna::icons::{self, Icon};
use banna::theme::{ACCENTS, Prefs, Theme, Wallpaper};
use banna::ui::{self, Hit, Ui, WHITE};
use banna::wallpaper_art::{self as art, PRESETS, SOLIDS};
use banna::{fs, process, time};

banna::entry!(main);

const KEY_UP: u32 = 0xf700;
const KEY_DOWN: u32 = 0xf701;
const KEY_PAGE_UP: u32 = 0xf707;
const KEY_PAGE_DOWN: u32 = 0xf708;
const KEY_PASTE: u32 = 0xf710;
const USERS_PAGE: usize = 9;

const PAGES: [(&str, Icon); 11] = [
    ("Görünüm", Icon::Appearance),
    ("Duvar kağıdı", Icon::Wallpaper),
    ("Ekran", Icon::Display),
    ("Ses", Icon::Sound),
    ("Güç", Icon::Power),
    ("Klavye ve fare", Icon::Input),
    ("Tarih ve saat", Icon::Clock),
    ("Dock ve panel", Icon::Dock),
    ("Ağ", Icon::Network),
    ("Kullanıcılar", Icon::Users),
    ("Hakkında", Icon::Info),
];

/// Canlı bilgi gösteren sayfalar saniyede bir yenilenir.
const LIVE_PAGES: [usize; 4] = [4, 6, 8, 10];

#[derive(Clone, PartialEq)]
enum Control {
    Page(usize),
    /// `desktop::set(anahtar, değer)`
    Set(&'static str, String),
    /// Kaydırıcı: değer konumdan hesaplanır.
    Slider(&'static str),
    Account(AccountControl),
}

#[derive(Clone, PartialEq)]
enum AccountAction {
    Add,
    Password(String),
    Remove(String),
}

#[derive(Clone, PartialEq)]
enum AccountControl {
    Start(AccountAction),
    Field(usize),
    Admin,
    Submit,
    Cancel,
}

#[derive(Default)]
struct AccountForm {
    action: Option<AccountAction>,
    fields: [ui::TextInput; 4],
    focus: usize,
    admin: bool,
    message: Option<(String, bool)>,
}

impl AccountForm {
    /// Etiketler; ilk alan dışında hepsi parola.
    fn labels(&self, me: &str, root: bool) -> Vec<&'static str> {
        let mut labels = match &self.action {
            Some(AccountAction::Add) => vec!["Kullanıcı adı", "Parola", "Parola (tekrar)"],
            Some(AccountAction::Password(name)) if name == me && !root => {
                vec!["Mevcut parola", "Yeni parola", "Yeni parola (tekrar)"]
            }
            Some(AccountAction::Password(_)) => vec!["Yeni parola", "Yeni parola (tekrar)"],
            Some(AccountAction::Remove(_)) => vec![],
            None => return vec![],
        };
        let own = matches!(&self.action, Some(AccountAction::Password(n)) if n == me);
        if !root && !own {
            labels.push("Yönetici parolanız");
        }
        labels
    }
}

fn set(key: &'static str, value: impl Into<String>) -> Control {
    Control::Set(key, value.into())
}

fn flag(on: bool) -> &'static str {
    if on { "1" } else { "0" }
}

struct Settings {
    window: Window,
    fonts: UiFonts,
    u: i32,
    page: usize,
    scroll: i32,
    max_scroll: i32,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    info: Info,
    prefs: Prefs,
    theme: Theme,
    drag: Option<(&'static str, Rect, i32, i32)>,
    thumbs: Vec<(Wallpaper, Surface)>,
    files: Option<Vec<String>>,
    account: AccountForm,
    dirty: bool,
    last_query: u64,
}

impl Settings {
    fn refresh(&mut self) {
        if let Some(info) = desktop::query() {
            self.prefs = Prefs::parse(&info.text());
            self.theme = Theme::from(&self.prefs);
            self.info = info;
        }
        self.last_query = time::uptime_ms();
        self.dirty = true;
    }

    /// Değişikliği masaüstüne gönderir ve yerel kopyayı hemen günceller.
    fn send(&mut self, key: &'static str, value: &str) {
        desktop::set(key, value);
        let mut text = self.prefs.serialize();
        text.push_str(&format!("{}={}\n", key, value));
        self.prefs = Prefs::parse(&text);
        self.theme = Theme::from(&self.prefs);
        self.dirty = true;
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() else {
            return;
        };
        match hit.control {
            Control::Page(p) => {
                self.page = p;
                self.scroll = 0;
                self.refresh();
            }
            Control::Set(key, value) => {
                self.send(key, &value);
                // Eylemler ve sistem ayarları canlı bilgiyi değiştirir.
                if key == "action" || key == "resolution" || key == "layout" {
                    time::sleep_ms(30);
                    self.refresh();
                }
            }
            Control::Account(c) => self.account_press(c),
            Control::Slider(key) => {
                if let Some((track, min, max)) = hit.slider {
                    self.drag = Some((key, track, min, max));
                    let v = ui::slider_value(track, min, max, x);
                    self.send(key, &format!("{}", v));
                }
            }
        }
    }

    fn account_press(&mut self, c: AccountControl) {
        let form = &mut self.account;
        match c {
            AccountControl::Start(action) => {
                *form = AccountForm {
                    action: Some(action),
                    ..AccountForm::default()
                };
            }
            AccountControl::Field(i) => form.focus = i,
            AccountControl::Admin => form.admin = !form.admin,
            AccountControl::Cancel => form.action = None,
            AccountControl::Submit => self.account_submit(),
        }
        self.dirty = true;
    }

    fn account_submit(&mut self) {
        let me = String::from(self.info.get("user.name"));
        let root = process::uid() == 0;
        let form = &mut self.account;
        let Some(action) = form.action.clone() else {
            return;
        };
        let labels = form.labels(&me, root);
        let value = |label: &str| {
            labels
                .iter()
                .position(|l| *l == label)
                .map(|i| form.fields[i].clone())
                .unwrap_or_default()
        };
        let admin_password = value("Yönetici parolanız");
        let result = match &action {
            AccountAction::Add => {
                if value("Parola") != value("Parola (tekrar)") {
                    Err(String::from("parolalar eşleşmiyor"))
                } else {
                    process::account(
                        0,
                        &[
                            value("Kullanıcı adı").trim(),
                            &value("Parola"),
                            if form.admin { "1" } else { "0" },
                            &admin_password,
                        ],
                    )
                }
            }
            AccountAction::Password(name) => {
                if value("Yeni parola") != value("Yeni parola (tekrar)") {
                    Err(String::from("parolalar eşleşmiyor"))
                } else {
                    let auth = if *name == me && !root {
                        value("Mevcut parola")
                    } else {
                        admin_password.clone()
                    };
                    process::account(1, &[name, &value("Yeni parola"), &auth])
                }
            }
            AccountAction::Remove(name) => process::account(2, &[name, &admin_password]),
        };
        match result {
            Ok(()) => {
                let done = match &action {
                    AccountAction::Add => format!("{} oluşturuldu", value("Kullanıcı adı").trim()),
                    AccountAction::Password(n) => format!("{} için parola değiştirildi", n),
                    AccountAction::Remove(n) => format!("{} silindi (ev dizini korunur)", n),
                };
                self.account = AccountForm {
                    message: Some((done, false)),
                    ..AccountForm::default()
                };
                self.refresh();
            }
            Err(e) => {
                let mut chars = e.chars();
                let first: String = chars
                    .next()
                    .map(|c| c.to_uppercase().collect())
                    .unwrap_or_default();
                form.message = Some((first + chars.as_str(), true));
            }
        }
        self.dirty = true;
    }

    /// Hesap formu açıksa tuşu alır.
    fn account_key(&mut self, key: u32) -> bool {
        let me = String::from(self.info.get("user.name"));
        let count = self.account.labels(&me, process::uid() == 0).len();
        if self.page != USERS_PAGE || self.account.action.is_none() {
            return false;
        }
        let form = &mut self.account;
        match key {
            27 => form.action = None,
            13 => self.account_submit(),
            9 if count > 0 => form.focus = (form.focus + 1) % count,
            KEY_PASTE | 22 if form.focus < count => {
                form.fields[form.focus].push_str(clipboard::get().trim())
            }
            k if form.focus < count => {
                form.fields[form.focus].key(k, 64);
            }
            _ => {}
        }
        self.dirty = true;
        true
    }

    fn drag_to(&mut self, x: i32) {
        if let Some((key, track, min, max)) = self.drag {
            let v = ui::slider_value(track, min, max, x);
            self.send(key, &format!("{}", v));
        }
    }

    fn release(&mut self) {
        if let Some((key, ..)) = self.drag.take() {
            desktop::set("release", key);
        }
    }

    fn hover_at(&mut self, x: i32, y: i32) {
        let hovered = ui::hit_at(&self.hits, x, y).map(|h| h.rect);
        if hovered != self.hover {
            self.hover = hovered;
            self.dirty = true;
        }
    }

    fn scroll_by(&mut self, delta: i32) {
        let next = (self.scroll + delta).clamp(0, self.max_scroll);
        if next != self.scroll {
            self.scroll = next;
            self.dirty = true;
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let sidebar_w = 220 * u;
        let t = self.theme;
        let mut hits = Vec::new();
        let hover = self.hover;
        let page = self.page;
        let scroll = self.scroll;
        let bottom;
        {
            // Pencere tamponu ve uygulama durumu ayrı alanlar olarak ödünç alınır.
            let mut c = self.window.canvas();
            let f = &self.fonts;
            c.fill(Rect::new(0, 0, w, h), t.window_bg);

            // Kenar çubuğu.
            c.fill(Rect::new(0, 0, sidebar_w, h), t.sidebar);
            c.fill(
                Rect::new(sidebar_w - 1, 0, 1, h),
                with_alpha(if t.dark { 0xffffff } else { 0 }, 14),
            );
            let name = String::from(self.info.get("user.name"));
            let (ax, ay, ar) = (40 * u, 46 * u, 20 * u);
            icons::disc(&mut c, ax, ay, ar, t.accent);
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
                WHITE,
            );
            f.title.draw(
                &mut c,
                ax + ar + 12 * u,
                ay - f.title.line_height() / 2 - 2 * u,
                &name,
                t.text,
            );
            let role = if self.info.flag("user.admin") {
                "Yönetici"
            } else {
                "Standart kullanıcı"
            };
            f.small
                .draw(&mut c, ax + ar + 12 * u, ay + 2 * u, role, t.muted);
            let mut y = 86 * u;
            let item_h = 38 * u;
            for (i, (label, icon)) in PAGES.iter().enumerate() {
                let item = Rect::new(10 * u, y, sidebar_w - 20 * u, item_h - 4 * u);
                if i == page {
                    icons::rounded_rect(
                        &mut c,
                        item,
                        9 * u,
                        with_alpha(t.accent, if t.dark { 70 } else { 60 }),
                    );
                } else if hover == Some(item) {
                    icons::rounded_rect(&mut c, item, 9 * u, t.hover);
                }
                let is = 24 * u;
                icons::draw(
                    &mut c,
                    Rect::new(item.x + 8 * u, item.y + (item.h - is) / 2, is, is),
                    *icon,
                );
                f.body.draw(
                    &mut c,
                    item.x + 8 * u + is + 12 * u,
                    item.y + (item.h - f.body.ascent - f.body.descent) / 2,
                    label,
                    t.text,
                );
                hits.push(Hit {
                    rect: item,
                    control: Control::Page(i),
                    slider: None,
                });
                y += item_h;
            }

            // İçerik.
            let pad = 28 * u;
            let area = Rect::new(sidebar_w + pad, 0, w - sidebar_w - 2 * pad, h);
            let mut ui = Ui::new(&mut c, t, f, u, area, 22 * u - scroll, &mut hits, hover);
            ui.clip = Rect::new(sidebar_w, 0, w - sidebar_w, h);
            ui.c.clip = ui.clip;
            ui.heading(PAGES[page].0);
            match page {
                0 => page_appearance(&mut ui, &self.prefs),
                1 => page_wallpaper(
                    &mut ui,
                    &self.prefs,
                    &mut self.thumbs,
                    &mut self.files,
                    &self.info,
                ),
                2 => page_display(&mut ui, &self.prefs, &self.info),
                3 => page_sound(&mut ui, &self.prefs, &self.info),
                4 => page_power(&mut ui, &self.prefs, &self.info),
                5 => page_input(&mut ui, &self.prefs, &self.info),
                6 => page_clock(&mut ui, &self.prefs, &self.info),
                7 => page_dock(&mut ui, &self.prefs),
                8 => page_network(&mut ui, &self.info),
                9 => page_users(&mut ui, &self.info, &self.account),
                _ => page_about(&mut ui, &self.info),
            }
            bottom = ui.y + scroll + 24 * u;
            c.clip = Rect::new(0, 0, w, h);
            let max_scroll = (bottom - h).max(0);
            if max_scroll > 0 {
                let track_h = h - 16 * u;
                let bar_h = (track_h * h / bottom.max(1)).max(30 * u);
                let bar_y = 8 * u + (track_h - bar_h) * scroll.min(max_scroll) / max_scroll.max(1);
                icons::rounded_rect(
                    &mut c,
                    Rect::new(w - 8 * u, bar_y, 4 * u, bar_h),
                    2 * u,
                    with_alpha(t.muted, 120),
                );
            }
        }
        self.window.present();
        self.hits = hits;
        self.max_scroll = (bottom - h).max(0);
        if self.scroll > self.max_scroll {
            self.scroll = self.max_scroll;
            self.dirty = true;
        }
    }
}

fn page_appearance(ui: &mut Ui<Control>, p: &Prefs) {
    ui.section("Tema");
    ui.card(2);
    ui.choice(
        "Görünüm",
        None,
        &[("Koyu", set("dark", "1")), ("Açık", set("dark", "0"))],
        &set("dark", flag(p.dark)),
    );
    let colors: Vec<_> = ACCENTS
        .iter()
        .enumerate()
        .map(|(i, (_, c))| (*c, set("accent", format!("{}", i))))
        .collect();
    ui.swatches("Vurgu rengi", &colors, p.accent as usize);
    ui.section("Efektler");
    ui.card(3);
    ui.toggle(
        "Animasyonlar",
        Some("Başlatıcı ve dock hareketleri"),
        p.animations,
        set("animations", flag(!p.animations)),
    );
    ui.toggle(
        "Pencere gölgeleri",
        None,
        p.shadows,
        set("shadows", flag(!p.shadows)),
    );
    ui.slider(
        "Panel ve dock opaklığı",
        None,
        p.panel_opacity as i32,
        30,
        100,
        &format!("%{}", p.panel_opacity),
        Control::Slider("panel_opacity"),
    );
    ui.section("Sıfırla");
    ui.card(1);
    ui.buttons(
        "Varsayılan ayarlar",
        Some("Görünüm, dock ve saat ayarlarını ilk hallerine döndürür"),
        &[("Sıfırla", set("action", "reset"), true)],
    );
}

/// Duvar kağıdı önizlemesi: desenler doğrudan, resimler dosyadan üretilir.
fn thumbnail(choice: &Wallpaper, w: i32, h: i32) -> Option<Surface> {
    let mut s = Surface::new(w, h, 0);
    match choice {
        Wallpaper::Preset(p) => {
            for y in 0..h {
                for x in 0..w {
                    s.pixels[(y * w + x) as usize] = art::pixel(*p, x, y, w, h);
                }
            }
        }
        Wallpaper::Solid(i) => {
            for y in 0..h {
                let c = art::solid_pixel(*i, y, h);
                s.pixels[(y * w) as usize..((y + 1) * w) as usize].fill(c);
            }
        }
        Wallpaper::Image(path) => {
            let image = art::decode(&fs::read(path).ok()?)?;
            let sample = art::cover_sampler(&image, w, h);
            for y in 0..h {
                for x in 0..w {
                    s.pixels[(y * w + x) as usize] = sample(x, y);
                }
            }
        }
    }
    Some(s)
}

fn wallpaper_files(info: &Info) -> Vec<String> {
    let mut out = Vec::new();
    let mut dirs = alloc::vec![String::from("/usr/share/wallpapers")];
    if !info.get("user.home").is_empty() {
        dirs.push(format!("{}/Resimler", info.get("user.home")));
    }
    for dir in dirs {
        if let Ok(entries) = fs::read_dir(&dir) {
            for (name, stat) in entries {
                let lower = name.to_ascii_lowercase();
                if stat.kind == fs::KIND_FILE
                    && (lower.ends_with(".png")
                        || lower.ends_with(".bmp")
                        || lower.ends_with(".ppm"))
                {
                    out.push(format!("{}/{}", dir, name));
                }
            }
        }
    }
    out.sort();
    out
}

fn page_wallpaper(
    ui: &mut Ui<Control>,
    p: &Prefs,
    thumbs: &mut Vec<(Wallpaper, Surface)>,
    files: &mut Option<Vec<String>>,
    info: &Info,
) {
    let u = ui.u;
    let gap = 14 * u;
    let cols = 4;
    let tile_w = (ui.w - gap * (cols - 1)) / cols;
    let tile_h = tile_w * 9 / 16;
    let mut grid = |ui: &mut Ui<Control>, items: Vec<(Wallpaper, String)>| {
        for (i, (choice, label)) in items.into_iter().enumerate() {
            let col = (i % cols as usize) as i32;
            if col == 0 && i > 0 {
                ui.y += tile_h + 34 * u;
            }
            let tile = Rect::new(ui.x + col * (tile_w + gap), ui.y, tile_w, tile_h);
            if !thumbs.iter().any(|(w, _)| *w == choice)
                && let Some(s) = thumbnail(&choice, tile_w, tile_h)
            {
                thumbs.push((choice.clone(), s));
            }
            if p.wallpaper == choice {
                icons::rounded_rect(ui.c, tile.inset(-4 * u), 14 * u, ui.t.accent);
                icons::rounded_rect(ui.c, tile.inset(-u), 11 * u, ui.t.window_bg);
            } else if ui.hovered(tile) {
                icons::rounded_rect(ui.c, tile.inset(-3 * u), 13 * u, ui.t.control);
            }
            match thumbs.iter().find(|(w, _)| *w == choice) {
                Some((_, s)) => ui.c.blit_masked(s, tile.x, tile.y, 10 * u, 255),
                None => icons::rounded_rect(ui.c, tile, 10 * u, ui.t.card),
            }
            ui.f.small.draw_clipped(
                ui.c,
                tile.x + 2 * u,
                tile.bottom() + 6 * u,
                &label,
                tile_w,
                ui.t.muted,
            );
            let value = match &choice {
                Wallpaper::Preset(i) => format!("preset:{}", i),
                Wallpaper::Solid(i) => format!("solid:{}", i),
                Wallpaper::Image(path) => format!("image:{}", path),
            };
            ui.hit(tile, set("wallpaper", value));
        }
        ui.y += tile_h + 40 * u;
    };
    ui.section("Hazır desenler");
    grid(
        ui,
        (0..PRESETS.len())
            .map(|i| (Wallpaper::Preset(i as u8), String::from(PRESETS[i])))
            .collect(),
    );
    ui.section("Düz renkler");
    grid(
        ui,
        (0..SOLIDS.len())
            .map(|i| (Wallpaper::Solid(i as u8), String::from(SOLIDS[i].0)))
            .collect(),
    );
    ui.section("Resimler");
    let list = files.get_or_insert_with(|| wallpaper_files(info)).clone();
    if list.is_empty() {
        ui.note(
            "PNG, BMP ya da PPM dosyalarını /usr/share/wallpapers veya ~/Resimler klasörüne koyun.",
        );
    } else {
        grid(
            ui,
            list.iter()
                .map(|path| {
                    let name = path.rsplit('/').next().unwrap_or(path);
                    let name = name.rsplit_once('.').map(|(n, _)| n).unwrap_or(name);
                    (Wallpaper::Image(path.clone()), String::from(name))
                })
                .collect(),
        );
    }
    ui.section("Ayarlar");
    ui.card(1);
    ui.slider(
        "Karartma",
        Some("Yazıların okunması için duvar kağıdını koyulaştırır"),
        p.wallpaper_dim as i32,
        0,
        80,
        &format!("%{}", p.wallpaper_dim),
        Control::Slider("wallpaper_dim"),
    );
}

fn page_display(ui: &mut Ui<Control>, p: &Prefs, info: &Info) {
    let size = String::from(info.get("display.size"));
    ui.section("Çözünürlük");
    let native = info.get("display.native");
    let modes: Vec<(String, Control)> = info
        .get("display.modes")
        .split(',')
        .filter_map(|m| {
            let (w, h) = m.split_once('x')?;
            let (w, h): (u32, u32) = (w.parse().ok()?, h.parse().ok()?);
            let label = if m == native {
                format!("{}×{} (önerilen)", w, h)
            } else {
                format!("{}×{}", w, h)
            };
            (w >= 1024 && h >= 720).then(|| (label, set("resolution", m)))
        })
        .collect();
    if modes.len() > 1 {
        ui.chips(&modes, &set("resolution", size.as_str()));
        let notice = info.get("notice");
        if notice.starts_with("Çözünürlük") {
            ui.card(1);
            ui.info("Durum", notice);
        } else if info.get("display.restart") == "1" {
            ui.card(1);
            ui.info("Not", "UEFI kipi yeniden başlatınca değişir");
        }
    } else {
        ui.card(1);
        ui.info(
            "Geçerli kip",
            &format!("{} (firmware, sabit)", size.replace('x', "×")),
        );
    }
    ui.section("Parlaklık ve renk");
    ui.card(3);
    match info.opt("display.brightness") {
        Some(level) => {
            let level = p.brightness.map(|b| b as i64).unwrap_or(level);
            ui.slider(
                "Parlaklık",
                None,
                level as i32,
                5,
                100,
                &format!("%{}", level),
                Control::Slider("brightness"),
            )
        }
        None => ui.info("Parlaklık", "ACPI parlaklık denetimi yok"),
    }
    ui.toggle(
        "Gece ışığı",
        Some("Ekran renklerini akşamları göz yormayan sıcak tona çeker"),
        p.night_light,
        set("night_light", flag(!p.night_light)),
    );
    ui.slider(
        "Gece ışığı yoğunluğu",
        None,
        p.night_strength as i32,
        10,
        80,
        &format!("%{}", p.night_strength),
        Control::Slider("night_strength"),
    );
    ui.section("Grafik");
    let vsync = info.get("display.vsync");
    ui.card(if vsync.is_empty() { 4 } else { 5 });
    ui.info("Grafik bağdaştırıcısı", info.get("display.adapter"));
    ui.toggle(
        "Çok çekirdekli çizim",
        Some(&format!(
            "Evrensel hızlandırma: {} işçi thread'i + SSE2",
            info.get("display.workers")
        )),
        p.parallel,
        set("parallel", flag(!p.parallel)),
    );
    ui.toggle(
        "Panelde FPS göster",
        None,
        p.show_fps,
        set("show_fps", flag(!p.show_fps)),
    );
    if !vsync.is_empty() {
        ui.toggle(
            "Dikey eşitleme (V-Sync)",
            Some(&format!("Yırtılmayı önler: {}", vsync)),
            p.vsync,
            set("vsync", flag(!p.vsync)),
        );
    }
    let notice = info.get("notice");
    ui.buttons(
        "Performans testi",
        Some(if notice.starts_with("Test:") {
            notice
        } else {
            "Tam ekran çizimi tek ve çok çekirdekte ölçer"
        }),
        &[("Çalıştır", set("action", "benchmark"), false)],
    );
}

fn page_sound(ui: &mut Ui<Control>, p: &Prefs, info: &Info) {
    ui.card(3);
    ui.info(
        "Çıkış aygıtı",
        if info.flag("audio.present") {
            "Intel HD Audio"
        } else {
            "PC hoparlörü (yalnızca bip)"
        },
    );
    let volume = p
        .volume
        .map(|v| v as i64)
        .unwrap_or_else(|| info.num("audio.volume")) as i32;
    ui.slider(
        "Ses düzeyi",
        None,
        if p.muted { 0 } else { volume },
        0,
        100,
        &if p.muted {
            String::from("Sessiz")
        } else {
            format!("%{}", volume)
        },
        Control::Slider("volume"),
    );
    ui.toggle("Sessiz", None, p.muted, set("muted", flag(!p.muted)));
    ui.section("Sistem sesleri");
    ui.card(2);
    ui.toggle(
        "Arayüz sesleri",
        Some("Başlatıcı açılırken ve ses ayarlanırken kısa ses"),
        p.ui_sounds,
        set("ui_sounds", flag(!p.ui_sounds)),
    );
    ui.buttons(
        "Hoparlörü dene",
        Some("İki tonlu deneme sesi çalar"),
        &[("Çal", set("action", "test_sound"), false)],
    );
}

fn page_power(ui: &mut Ui<Control>, p: &Prefs, info: &Info) {
    let profile = p
        .profile
        .map(|v| v as i64)
        .unwrap_or_else(|| info.num("power.profile"));
    ui.section("İşlemci");
    ui.card(2);
    ui.choice(
        "Güç profili",
        Some(if info.flag("power.hwp") {
            "Intel HWP (Speed Shift)"
        } else {
            "Bu işlemcide frekansı firmware yönetiyor"
        }),
        &[
            ("Performans", set("profile", "0")),
            ("Dengeli", set("profile", "1")),
            ("Tasarruf", set("profile", "2")),
        ],
        &set("profile", format!("{}", profile)),
    );
    ui.info(
        "Sıcaklık",
        &match info.opt("power.temperature") {
            Some(t) => format!("{} °C", t),
            None => String::from("Okunamıyor"),
        },
    );
    ui.section("Pil");
    match info.opt("battery.percent") {
        Some(percent) => {
            ui.card(2);
            ui.info(
                "Şarj",
                &format!("%{} ({})", percent, info.get("battery.state")),
            );
            ui.info(
                "Kalan süre",
                &match info.opt("battery.minutes") {
                    Some(m) => format!("{} sa {} dk", m / 60, m % 60),
                    None => String::from("-"),
                },
            );
        }
        None => {
            ui.card(1);
            ui.info("Durum", "Pil yok, prizden besleniyor");
        }
    }
    ui.section("Davranış");
    ui.card(3);
    ui.choice(
        "Ekranı kapat",
        Some("Bu süre boyunca hareketsiz kalınca"),
        &[
            ("Asla", set("screen_off", "0")),
            ("1 dk", set("screen_off", "1")),
            ("5 dk", set("screen_off", "5")),
            ("15 dk", set("screen_off", "15")),
        ],
        &set("screen_off", format!("{}", p.screen_off_min)),
    );
    ui.choice(
        "Kapak kapanınca",
        None,
        &[
            ("Ekranı kapat", set("lid_action", "0")),
            ("Hiçbir şey", set("lid_action", "1")),
            ("Bilgisayarı kapat", set("lid_action", "2")),
        ],
        &set("lid_action", format!("{}", p.lid_action)),
    );
    ui.choice(
        "Güç düğmesi",
        None,
        &[
            ("Kapat", set("power_button", "0")),
            ("Hiçbir şey", set("power_button", "1")),
        ],
        &set("power_button", format!("{}", p.power_button)),
    );
    ui.section("Oturum");
    ui.card(1);
    ui.buttons(
        "Bilgisayar",
        None,
        &[
            ("Yeniden başlat", set("action", "reboot"), false),
            ("Kapat", set("action", "shutdown"), true),
        ],
    );
}

fn page_input(ui: &mut Ui<Control>, p: &Prefs, info: &Info) {
    ui.section("Klavye");
    ui.card(1);
    let us = p.layout_us.unwrap_or(info.get("input.layout") == "us");
    ui.choice(
        "Klavye düzeni",
        Some("AltGr ile @ # { [ ] } \\ | ~"),
        &[
            ("Türkçe Q", set("layout", "tr")),
            ("ABD", set("layout", "us")),
        ],
        &set("layout", if us { "us" } else { "tr" }),
    );
    ui.section("Fare ve dokunmatik yüzey");
    ui.card(2);
    let speed = p
        .mouse_speed
        .map(|v| v as i64)
        .unwrap_or_else(|| info.num("input.mouse_speed")) as i32;
    ui.slider(
        "İmleç hızı",
        None,
        speed,
        25,
        300,
        &format!("%{}", speed),
        Control::Slider("mouse_speed"),
    );
    let left = p.left_handed.unwrap_or(info.flag("input.left_handed"));
    ui.toggle(
        "Sol el kipi",
        Some("Birincil ve ikincil düğmeler yer değiştirir"),
        left,
        set("left_handed", flag(!left)),
    );
}

fn page_clock(ui: &mut Ui<Control>, p: &Prefs, info: &Info) {
    ui.card(3);
    ui.info("Şu an", info.get("clock.now"));
    ui.info(
        "Eşitleme",
        if info.flag("clock.synced") {
            "İnternet saatiyle eşitlendi (NTP)"
        } else {
            "Donanım saati (ağ bekleniyor)"
        },
    );
    ui.buttons(
        "Saat dilimi",
        Some(&format!(
            "{}{}",
            info.get("clock.zone"),
            if info.num("clock.offset") == 180 {
                " (İstanbul)"
            } else {
                ""
            }
        )),
        &[
            ("-", set("action", "tz_minus"), false),
            ("+", set("action", "tz_plus"), false),
        ],
    );
    ui.section("Biçim");
    ui.card(3);
    ui.toggle(
        "24 saat biçimi",
        None,
        p.clock_24h,
        set("clock_24h", flag(!p.clock_24h)),
    );
    ui.toggle(
        "Saniyeleri göster",
        None,
        p.clock_seconds,
        set("clock_seconds", flag(!p.clock_seconds)),
    );
    ui.toggle(
        "Tarihi göster",
        None,
        p.show_date,
        set("show_date", flag(!p.show_date)),
    );
}

fn page_dock(ui: &mut Ui<Control>, p: &Prefs) {
    ui.section("Dock");
    ui.card(3);
    ui.choice(
        "Simge boyutu",
        None,
        &[
            ("Küçük", set("dock_size", "0")),
            ("Orta", set("dock_size", "1")),
            ("Büyük", set("dock_size", "2")),
        ],
        &set("dock_size", format!("{}", p.dock_size)),
    );
    ui.toggle(
        "Büyütme efekti",
        Some("Fare üzerine gelince simgeler büyür"),
        p.dock_magnify,
        set("dock_magnify", flag(!p.dock_magnify)),
    );
    ui.toggle(
        "Açık uygulamaları göster",
        Some("Sabitlenmemiş açık uygulamalar da dock'ta görünür"),
        p.dock_running,
        set("dock_running", flag(!p.dock_running)),
    );
    ui.section("Üst panel");
    ui.card(2);
    ui.toggle(
        "Durum bilgileri",
        Some("Ağ, pil ve çekirdek sayısı"),
        p.status_items,
        set("status_items", flag(!p.status_items)),
    );
    ui.toggle(
        "FPS göster",
        None,
        p.show_fps,
        set("show_fps", flag(!p.show_fps)),
    );
}

fn page_network(ui: &mut Ui<Control>, info: &Info) {
    ui.card(1);
    ui.info("Durum", info.get("net.status"));
    if info.get("net.model").is_empty() {
        ui.note("Desteklenen ağ kartı bulunamadı (Intel e1000/e1000e). Kablosuz ağ henüz yok.");
        return;
    }
    let or_dash = |s: &str| String::from(if s.is_empty() { "-" } else { s });
    ui.section("Ethernet");
    ui.card(6);
    ui.info("Aygıt", info.get("net.model"));
    ui.info("MAC adresi", info.get("net.mac"));
    ui.info(
        "Bağlantı",
        &if info.flag("net.link") {
            format!(
                "{} Mbit/s{}",
                info.get("net.speed"),
                if info.flag("net.duplex") {
                    ", tam çift yönlü"
                } else {
                    ""
                }
            )
        } else {
            String::from("Kablo takılı değil")
        },
    );
    ui.info(
        "IP adresi",
        &if info.get("net.address").is_empty() {
            String::from("-")
        } else {
            format!("{} / {}", info.get("net.address"), info.get("net.netmask"))
        },
    );
    ui.info("Ağ geçidi", &or_dash(info.get("net.gateway")));
    ui.info("DNS", &or_dash(info.get("net.dns")));
    ui.section("DHCP");
    ui.card(1);
    let notice = info.get("notice");
    ui.buttons(
        &format!("Durum: {}", info.get("net.dhcp")),
        Some(if notice.starts_with("DHCP") {
            notice
        } else {
            "Adres kirasını yeniler"
        }),
        &[("Yenile", set("action", "dhcp_renew"), false)],
    );
}

fn page_users(ui: &mut Ui<Control>, info: &Info, form: &AccountForm) {
    let me = info.num("user.uid");
    let my_name = info.get("user.name");
    let root = process::uid() == 0;
    let i_am_admin = info.flag("user.admin") || root;
    let accounts: Vec<(&str, i64, bool)> = info
        .get("users")
        .split(';')
        .filter_map(|a| {
            let mut parts = a.split(':');
            Some((
                parts.next()?,
                parts.next()?.parse().ok()?,
                parts.next()? == "1",
            ))
        })
        .collect();
    let start = |a: AccountAction| Control::Account(AccountControl::Start(a));
    ui.section("Hesaplar");
    ui.card(accounts.len().max(1) + usize::from(i_am_admin));
    for (name, uid, admin) in &accounts {
        let mut buttons: Vec<(&str, Control, bool)> = Vec::new();
        if i_am_admin || *uid == me {
            buttons.push((
                "Parola",
                start(AccountAction::Password(String::from(*name))),
                false,
            ));
        }
        if i_am_admin && *uid != me {
            buttons.push((
                "Sil",
                start(AccountAction::Remove(String::from(*name))),
                true,
            ));
        }
        ui.buttons(
            &format!("{}{}", name, if *uid == me { " (siz)" } else { "" }),
            Some(&format!(
                "{} • uid {}",
                if *admin { "Yönetici" } else { "Standart" },
                uid
            )),
            &buttons,
        );
    }
    if accounts.is_empty() {
        ui.info("Hesap", "Live oturumu");
    }
    if i_am_admin {
        ui.buttons(
            "Yeni kullanıcı",
            Some("Bu bilgisayara bir hesap ekler"),
            &[("Ekle", start(AccountAction::Add), false)],
        );
    }
    if let Some(action) = &form.action {
        let u = ui.u;
        let labels = form.labels(my_name, root);
        ui.section(&match action {
            AccountAction::Add => String::from("Yeni kullanıcı"),
            AccountAction::Password(n) => format!("{} için parola", n),
            AccountAction::Remove(n) => format!("{} silinsin mi? Ev dizini korunur.", n),
        });
        let extra = usize::from(*action == AccountAction::Add) + 1;
        ui.card(labels.len() + extra);
        for (i, label) in labels.iter().enumerate() {
            let row = ui.row();
            ui.label(row, label, None, 200 * u);
            let field = Rect::new(
                row.x + 230 * u,
                row.y + 10 * u,
                row.w - 248 * u,
                row.h - 20 * u,
            );
            ui.input_field(
                field,
                label,
                &form.fields[i],
                i > 0 || *action != AccountAction::Add,
                form.focus == i,
                Control::Account(AccountControl::Field(i)),
            );
        }
        if *action == AccountAction::Add {
            ui.toggle(
                "Yönetici",
                Some("sudo ve sistem ayarlarını kullanabilir"),
                form.admin,
                Control::Account(AccountControl::Admin),
            );
        }
        let submit = match action {
            AccountAction::Add => "Oluştur",
            AccountAction::Password(_) => "Değiştir",
            AccountAction::Remove(_) => "Sil",
        };
        ui.buttons(
            "",
            None,
            &[
                ("Vazgeç", Control::Account(AccountControl::Cancel), false),
                (
                    submit,
                    Control::Account(AccountControl::Submit),
                    matches!(action, AccountAction::Remove(_)),
                ),
            ],
        );
    }
    if let Some((message, error)) = &form.message {
        let color = if *error { ui::DANGER } else { ui.t.accent };
        ui.f.body
            .draw_clipped(ui.c, ui.x + 4 * ui.u, ui.y, message, ui.w, color);
        ui.y += ui.f.body.line_height() + 14 * ui.u;
    }
    ui.section("Oturum");
    ui.card(1);
    ui.buttons(
        "Oturumu kapat",
        Some("Açık uygulamalar kapatılır ve giriş ekranına dönülür"),
        &[("Oturumu kapat", set("action", "logout"), true)],
    );
}

fn hypervisor(name: &str) -> &str {
    match name {
        "" => "Gerçek donanım",
        "TCGTCGTCGTCG" => "QEMU (TCG)",
        "KVMKVMKVM" => "KVM",
        "VMwareVMware" => "VMware",
        "Microsoft Hv" => "Hyper-V",
        "VBoxVBoxVBox" => "VirtualBox",
        other => other,
    }
}

fn page_about(ui: &mut Ui<Control>, info: &Info) {
    let u = ui.u;
    let logo = Rect::new(ui.x, ui.y, 64 * u, 64 * u);
    icons::logo(ui.c, logo, ui.f.heading, WHITE, banna::gfx::rgb(0, 0, 0));
    ui.f.title.draw(
        ui.c,
        logo.right() + 18 * u,
        logo.y + 8 * u,
        &format!("Banna OS {}", info.get("sys.version")),
        ui.t.text,
    );
    ui.f.small.draw(
        ui.c,
        logo.right() + 18 * u,
        logo.y + 36 * u,
        "Rust ile yazılmış x86_64 işletim sistemi",
        ui.t.muted,
    );
    ui.y += 84 * u;
    let uptime = info.num("sys.uptime_ms") / 1000;
    ui.card(7);
    ui.info("İşlemci", info.get("sys.cpu"));
    ui.info(
        "Çekirdekler",
        &format!("{} çevrimiçi", info.get("sys.cores")),
    );
    ui.info(
        "Bellek",
        &format!(
            "{} MiB ({} MiB boş)",
            info.num("sys.mem_total") >> 20,
            info.num("sys.mem_free") >> 20
        ),
    );
    ui.info("Ekran", &info.get("display.size").replace('x', "×"));
    ui.info(
        "Ses",
        if info.flag("audio.present") {
            "Intel HD Audio"
        } else {
            "PC hoparlörü"
        },
    );
    ui.info("Sanallaştırma", hypervisor(info.get("sys.hypervisor")));
    ui.info(
        "Çalışma süresi",
        &format!("{} sa {} dk", uptime / 3600, (uptime / 60) % 60),
    );
}

fn main(args: &[&str]) -> i32 {
    let Some(info) = desktop::query() else {
        banna::eprintln!("settings: masaüstü çalışmıyor");
        return 1;
    };
    let u = ui::unit();
    let window = match Window::new("Ayarlar", (880 * u) as usize, (580 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("settings: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let prefs = Prefs::parse(&info.text());
    let page = args
        .get(1)
        .and_then(|a| {
            let a = a.to_lowercase();
            PAGES
                .iter()
                .position(|(n, _)| n.to_lowercase() == a)
                .or_else(|| {
                    PAGES
                        .iter()
                        .position(|(n, _)| n.to_lowercase().starts_with(&a))
                })
        })
        .unwrap_or(0);
    let mut app = Settings {
        window,
        fonts: ui::fonts(),
        u,
        page,
        scroll: 0,
        max_scroll: 0,
        hits: Vec::new(),
        hover: None,
        theme: Theme::from(&prefs),
        prefs,
        info,
        drag: None,
        thumbs: Vec::new(),
        files: None,
        account: AccountForm::default(),
        dirty: true,
        last_query: time::uptime_ms(),
    };
    banna::println!("settings: ready");
    loop {
        if app.dirty {
            app.render();
        }
        let live = LIVE_PAGES.contains(&app.page);
        let Some(event) = app.window.event(if live { 1000 } else { 5000 }) else {
            if live || time::uptime_ms() - app.last_query > 5000 {
                app.refresh();
            }
            continue;
        };
        match event.kind {
            EVENT_CLOSE => return 0,
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                if app.drag.is_some() {
                    app.drag_to(event.x);
                } else {
                    app.hover_at(event.x, event.y);
                }
            }
            EVENT_MOUSE_UP => app.release(),
            EVENT_WHEEL => app.scroll_by(event.key as i32 * 48 * u),
            EVENT_KEY if app.account_key(event.key) => {}
            EVENT_KEY => match event.key & 0xffff {
                27 => return 0,
                KEY_DOWN => app.scroll_by(48 * u),
                KEY_UP => app.scroll_by(-48 * u),
                KEY_PAGE_DOWN => app.scroll_by(288 * u),
                KEY_PAGE_UP => app.scroll_by(-288 * u),
                _ => {}
            },
            _ => {}
        }
    }
}
