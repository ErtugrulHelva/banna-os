//! Dosyalar: klasörlerde gezinme, açma (resimler Görüntüleyici'de, metinler Metin
//! Düzenleyici'de), yeni klasör, yeniden adlandırma, silme, kopyala/yapıştır.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::fs;
use banna::gfx::text::UiFonts;
use banna::gfx::{Rect, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_CONTEXT, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE,
    EVENT_WHEEL, KEY_DELETE, KEY_DOWN, KEY_UP, Window,
};
use banna::icons::{self, Icon};
use banna::sys::Stat;
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, Ui, WHITE};
use banna::{desktop, process, time};

banna::entry!(main);

#[derive(Clone, PartialEq)]
enum Control {
    Place(String),
    Back,
    Up,
    NewFolder,
    Rename,
    Delete,
    Copy,
    Paste,
    Item(usize),
    Field,
    Confirm,
    Cancel,
    Open(usize),
    Terminal,
    NewWindow,
    Refresh,
}

#[derive(PartialEq)]
enum Mode {
    Browse,
    NewFolder,
    Rename(usize),
}

struct Files {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    cwd: String,
    history: Vec<String>,
    entries: Vec<(String, Stat)>,
    selected: Option<usize>,
    last_click: (usize, u64),
    scroll: i32,
    max_scroll: i32,
    mode: Mode,
    field: ui::TextInput,
    clipboard: Option<String>,
    message: Option<(String, bool)>,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    home: String,
    dirty: bool,
    menu: Option<ui::ContextMenu<Control>>,
}

fn join(dir: &str, name: &str) -> String {
    if dir == "/" {
        format!("/{}", name)
    } else {
        format!("{}/{}", dir, name)
    }
}

fn parent(path: &str) -> String {
    match path.trim_end_matches('/').rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

fn size_text(bytes: u64) -> String {
    if bytes >= 1 << 20 {
        format!("{}.{} MB", bytes >> 20, ((bytes % (1 << 20)) * 10) >> 20)
    } else if bytes >= 1024 {
        format!("{} KB", bytes.div_ceil(1024))
    } else {
        format!("{} B", bytes)
    }
}

fn extension(name: &str) -> String {
    name.rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn is_image(name: &str) -> bool {
    matches!(extension(name).as_str(), "png" | "bmp" | "ppm")
}

fn is_audio(name: &str) -> bool {
    extension(name) == "mp3"
}

fn is_text(name: &str) -> bool {
    matches!(
        extension(name).as_str(),
        "txt"
            | "md"
            | "rs"
            | "conf"
            | "toml"
            | "json"
            | "ini"
            | "log"
            | "sh"
            | "c"
            | "h"
            | "py"
            | "csv"
            | ""
    )
}

/// Dizini (alt dizinleriyle) siler.
fn remove_all(path: &str) -> Result<(), i64> {
    if let Ok(st) = fs::stat(path)
        && st.kind == fs::KIND_DIRECTORY
    {
        for (name, _) in fs::read_dir(path)? {
            remove_all(&join(path, &name))?;
        }
    }
    fs::remove(path)
}

/// Dosya ya da dizini kopyalar.
fn copy_all(from: &str, to: &str) -> Result<(), i64> {
    let st = fs::stat(from)?;
    if st.kind == fs::KIND_DIRECTORY {
        fs::mkdir(to)?;
        for (name, _) in fs::read_dir(from)? {
            copy_all(&join(from, &name), &join(to, &name))?;
        }
        Ok(())
    } else {
        fs::write(to, &fs::read(from)?)
    }
}

impl Files {
    fn open_dir(&mut self, path: &str, remember: bool) {
        match fs::read_dir(path) {
            Ok(mut list) => {
                list.retain(|(n, _)| n != "." && n != "..");
                list.sort_by(|a, b| {
                    (b.1.kind == fs::KIND_DIRECTORY)
                        .cmp(&(a.1.kind == fs::KIND_DIRECTORY))
                        .then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
                });
                if remember && self.cwd != path {
                    self.history.push(self.cwd.clone());
                }
                self.cwd = String::from(path);
                self.entries = list;
                self.selected = None;
                self.scroll = 0;
                self.mode = Mode::Browse;
            }
            Err(e) => self.message = Some((format!("{}: {}", path, fs::error_text(e)), true)),
        }
        self.dirty = true;
    }

    fn refresh(&mut self) {
        let cwd = self.cwd.clone();
        let selected = self.selected;
        self.open_dir(&cwd, false);
        self.selected = selected.filter(|&i| i < self.entries.len());
    }

    fn open_entry(&mut self, index: usize) {
        let Some((name, st)) = self.entries.get(index).cloned() else {
            return;
        };
        let path = join(&self.cwd, &name);
        if st.kind == fs::KIND_DIRECTORY {
            self.open_dir(&path, true);
        } else if is_image(&name) {
            let _ = process::spawn("/apps/viewer/viewer", &["viewer", &path]);
        } else if is_audio(&name) {
            let _ = process::spawn("/apps/b-player/b-player", &["b-player", &path]);
        } else if is_text(&name) || st.size < 256 * 1024 {
            let _ = process::spawn("/apps/notes/notes", &["notes", &path]);
        } else {
            self.message = Some((format!("{} için uygulama yok", name), true));
        }
    }

    fn confirm(&mut self) {
        let name = String::from(self.field.trim());
        if name.is_empty() || name.contains('/') {
            self.message = Some((String::from("Geçersiz ad"), true));
            return;
        }
        let result = match self.mode {
            Mode::NewFolder => fs::mkdir(&join(&self.cwd, &name)),
            Mode::Rename(i) => {
                let old = self.entries.get(i).map(|e| e.0.clone()).unwrap_or_default();
                fs::rename(&join(&self.cwd, &old), &join(&self.cwd, &name))
            }
            Mode::Browse => Ok(()),
        };
        match result {
            Ok(()) => self.message = None,
            Err(e) => self.message = Some((String::from(fs::error_text(e)), true)),
        }
        self.mode = Mode::Browse;
        self.refresh();
    }

    fn press(&mut self, x: i32, y: i32) {
        if let Some(menu) = self.menu.take() {
            if let Some(control) = menu.pick(x, y) {
                self.activate(control);
            }
            self.dirty = true;
            return;
        }
        let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() else {
            return;
        };
        self.activate(hit.control);
    }

    /// Sağ tık: öğe üzerindeyse öğe menüsü, değilse klasör menüsü.
    fn context(&mut self, x: i32, y: i32) {
        let item = match ui::hit_at(&self.hits, x, y).map(|h| h.control.clone()) {
            Some(Control::Item(i)) => {
                self.selected = Some(i);
                Some(i)
            }
            _ => None,
        };
        let mut items: Vec<(&str, Option<Control>)> = Vec::new();
        if let Some(i) = item {
            items.push(("Aç", Some(Control::Open(i))));
            items.push(("Kopyala", Some(Control::Copy)));
            items.push(("Yeniden adlandır", Some(Control::Rename)));
            items.push(("Sil", Some(Control::Delete)));
            items.push(("", None));
        }
        items.push(("Yeni klasör", Some(Control::NewFolder)));
        if self.clipboard.is_some() {
            items.push(("Yapıştır", Some(Control::Paste)));
        }
        items.push(("Burada terminal aç", Some(Control::Terminal)));
        items.push(("", None));
        items.push(("Yeni pencere", Some(Control::NewWindow)));
        items.push(("Yenile", Some(Control::Refresh)));
        let bounds = (self.window.width as i32, self.window.height as i32);
        self.menu = Some(ui::ContextMenu::new(x, y, items, bounds));
        self.dirty = true;
    }

    fn activate(&mut self, control: Control) {
        match control {
            Control::Open(i) => self.open_entry(i),
            Control::Terminal => {
                let _ = process::spawn("/apps/terminal/terminal", &["terminal", "-d", &self.cwd]);
            }
            Control::NewWindow => {
                let _ = process::spawn("/apps/files/files", &["files", &self.cwd]);
            }
            Control::Refresh => self.refresh(),
            Control::Place(path) => self.open_dir(&path, true),
            Control::Back => {
                if let Some(prev) = self.history.pop() {
                    self.open_dir(&prev, false);
                }
            }
            Control::Up => {
                let up = parent(&self.cwd);
                self.open_dir(&up, true);
            }
            Control::NewFolder => {
                self.mode = Mode::NewFolder;
                self.field = String::from("Yeni klasör").into();
            }
            Control::Rename => {
                if let Some(i) = self.selected {
                    self.mode = Mode::Rename(i);
                    self.field = self.entries[i].0.clone().into();
                }
            }
            Control::Delete => self.delete_selected(),
            Control::Copy => self.copy_selected(),
            Control::Paste => self.paste(),
            Control::Item(i) => {
                let now = time::uptime_ms();
                if self.last_click.0 == i && now - self.last_click.1 < 450 {
                    self.open_entry(i);
                } else {
                    self.selected = Some(i);
                }
                self.last_click = (i, now);
            }
            Control::Field => {}
            Control::Confirm => self.confirm(),
            Control::Cancel => self.mode = Mode::Browse,
        }
        self.dirty = true;
    }

    fn delete_selected(&mut self) {
        let Some(i) = self.selected else {
            return;
        };
        let path = join(&self.cwd, &self.entries[i].0);
        match remove_all(&path) {
            Ok(()) => self.message = Some((format!("Silindi: {}", self.entries[i].0), false)),
            Err(e) => self.message = Some((String::from(fs::error_text(e)), true)),
        }
        self.selected = None;
        self.refresh();
    }

    fn paste(&mut self) {
        let Some(from) = self.clipboard.clone() else {
            return;
        };
        let name = from.rsplit('/').next().unwrap_or("kopya");
        let mut target = join(&self.cwd, name);
        if fs::stat(&target).is_ok() {
            target = join(&self.cwd, &format!("{} (kopya)", name));
        }
        match copy_all(&from, &target) {
            Ok(()) => self.message = Some((format!("Yapıştırıldı: {}", name), false)),
            Err(e) => self.message = Some((String::from(fs::error_text(e)), true)),
        }
        self.refresh();
    }

    fn key(&mut self, key: u32) -> bool {
        if self.mode != Mode::Browse {
            match key {
                13 => self.confirm(),
                27 => self.mode = Mode::Browse,
                k => {
                    self.field.key(k, 120);
                }
            }
            self.dirty = true;
            return true;
        }
        let count = self.entries.len();
        match key {
            27 => return false,
            KEY_DOWN if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| (i + 1).min(count - 1)))
            }
            KEY_UP if count > 0 => {
                self.selected = Some(self.selected.map_or(0, |i| i.saturating_sub(1)))
            }
            13 => {
                if let Some(i) = self.selected {
                    self.open_entry(i);
                }
            }
            8 => {
                let up = parent(&self.cwd);
                self.open_dir(&up, true);
            }
            KEY_DELETE => self.delete_selected(),
            3 => self.copy_selected(),
            22 => self.paste(),
            _ => {}
        }
        self.dirty = true;
        true
    }

    fn copy_selected(&mut self) {
        if let Some(i) = self.selected {
            self.clipboard = Some(join(&self.cwd, &self.entries[i].0));
            self.message = Some((format!("Kopyalandı: {}", self.entries[i].0), false));
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let t = self.theme;
        let mut hits = Vec::new();
        let mut c = self.window.canvas();
        let f = &self.fonts;
        c.fill(Rect::new(0, 0, w, h), t.window_bg);
        let sidebar = 200 * u;
        c.fill(Rect::new(0, 0, sidebar, h), t.sidebar);
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

        // Yerler.
        let mut places: Vec<(String, String)> = alloc::vec![
            (String::from("Ev"), self.home.clone()),
            (String::from("Resimler"), join(&self.home, "Resimler")),
            (String::from("Kök dizin"), String::from("/")),
            (String::from("Uygulamalar"), String::from("/apps")),
        ];
        if let Ok(media) = fs::read_dir("/media") {
            for (name, _) in media {
                places.push((format!("USB: {}", name), format!("/media/{}", name)));
            }
        }
        let mut y = 20 * u;
        ui.f.small.draw(ui.c, 20 * u, y, "Yerler", t.muted);
        y += 26 * u;
        for (label, path) in places {
            let r = Rect::new(10 * u, y, sidebar - 20 * u, 34 * u);
            if self.cwd == path {
                icons::rounded_rect(ui.c, r, 9 * u, with_alpha(t.accent, 70));
            } else if ui.hovered(r) {
                icons::rounded_rect(ui.c, r, 9 * u, t.hover);
            }
            folder_icon(
                &mut ui,
                Rect::new(r.x + 10 * u, r.y + 7 * u, 20 * u, 20 * u),
                t.accent,
            );
            ui.f.body.draw_clipped(
                ui.c,
                r.x + 40 * u,
                r.y + (r.h - ui.f.body.ascent - ui.f.body.descent) / 2,
                &label,
                r.w - 48 * u,
                t.text,
            );
            ui.hit(r, Control::Place(path));
            y += 38 * u;
        }

        // Araç çubuğu.
        let x0 = sidebar + 16 * u;
        let bar_y = 14 * u;
        let mut bx = x0;
        for (label, control) in [("←", Control::Back), ("↑", Control::Up)] {
            ui.secondary_button(Rect::new(bx, bar_y, 40 * u, 34 * u), label, control);
            bx += 46 * u;
        }
        ui.f.title.draw_clipped(
            ui.c,
            bx + 8 * u,
            bar_y + 7 * u,
            &self.cwd,
            w - bx - 420 * u,
            t.text,
        );
        let mut rx = w - 16 * u;
        for (label, control) in [
            ("Sil", Control::Delete),
            ("Yeniden adlandır", Control::Rename),
            ("Yapıştır", Control::Paste),
            ("Kopyala", Control::Copy),
            ("Yeni klasör", Control::NewFolder),
        ] {
            let bw = ui.f.small.measure(label) + 24 * u;
            rx -= bw;
            ui.secondary_button(Rect::new(rx, bar_y, bw, 34 * u), label, control);
            rx -= 8 * u;
        }

        // Liste.
        let list_top = 64 * u;
        let status_h = 30 * u;
        let area = Rect::new(sidebar, list_top, w - sidebar, h - list_top - status_h);
        ui.clip = area;
        ui.c.clip = area;
        let row_h = 40 * u;
        let mut y = list_top - self.scroll;
        if let Mode::NewFolder = self.mode {
            let r = Rect::new(x0, y + 4 * u, w - x0 - 300 * u, 34 * u);
            ui.input_field(r, "Klasör adı", &self.field, false, true, Control::Field);
            ui.primary_button(
                Rect::new(r.right() + 8 * u, r.y, 90 * u, 34 * u),
                "Oluştur",
                Control::Confirm,
            );
            ui.secondary_button(
                Rect::new(r.right() + 104 * u, r.y, 90 * u, 34 * u),
                "Vazgeç",
                Control::Cancel,
            );
            y += row_h + 4 * u;
        }
        for (i, (name, st)) in self.entries.iter().enumerate() {
            let r = Rect::new(x0, y, w - x0 - 16 * u, row_h - 4 * u);
            if self.selected == Some(i) {
                icons::rounded_rect(ui.c, r, 8 * u, with_alpha(t.accent, 90));
            } else if ui.hovered(r) {
                icons::rounded_rect(ui.c, r, 8 * u, t.hover);
            }
            let icon = Rect::new(r.x + 8 * u, r.y + 6 * u, 24 * u, 24 * u);
            if st.kind == fs::KIND_DIRECTORY {
                folder_icon(&mut ui, icon, t.accent);
            } else if is_image(name) {
                icons::draw(ui.c, icon, Icon::Wallpaper);
            } else {
                file_icon(&mut ui, icon, &extension(name));
            }
            if self.mode == Mode::Rename(i) {
                let fr = Rect::new(r.x + 42 * u, r.y + 2 * u, r.w / 2, r.h - 4 * u);
                ui.input_field(fr, "Ad", &self.field, false, true, Control::Field);
                ui.primary_button(
                    Rect::new(fr.right() + 8 * u, fr.y, 90 * u, fr.h),
                    "Kaydet",
                    Control::Confirm,
                );
                ui.secondary_button(
                    Rect::new(fr.right() + 104 * u, fr.y, 90 * u, fr.h),
                    "Vazgeç",
                    Control::Cancel,
                );
            } else {
                ui.f.body.draw_clipped(
                    ui.c,
                    r.x + 42 * u,
                    r.y + (r.h - ui.f.body.ascent - ui.f.body.descent) / 2,
                    name,
                    r.w - 220 * u,
                    t.text,
                );
                let meta = if st.kind == fs::KIND_DIRECTORY {
                    String::from("Klasör")
                } else {
                    size_text(st.size)
                };
                let mw = ui.f.small.measure(&meta);
                ui.f.small
                    .draw(ui.c, r.right() - 16 * u - mw, r.y + 12 * u, &meta, t.muted);
                ui.hit(r, Control::Item(i));
            }
            y += row_h;
        }
        if self.entries.is_empty() {
            ui.f.body
                .draw(ui.c, x0, y + 20 * u, "Bu klasör boş.", t.muted);
        }
        let bottom = y + self.scroll - list_top;
        self.max_scroll = (bottom - area.h).max(0);

        // Durum çubuğu.
        ui.clip = Rect::new(0, 0, w, h);
        ui.c.clip = ui.clip;
        let bar = Rect::new(sidebar, h - status_h, w - sidebar, status_h);
        ui.c.fill(bar, t.sidebar);
        let text = match &self.message {
            Some((m, _)) => m.clone(),
            None => format!("{} öğe", self.entries.len()),
        };
        let color = if self.message.as_ref().is_some_and(|m| m.1) {
            ui::DANGER
        } else {
            t.muted
        };
        ui.f.small.draw_clipped(
            ui.c,
            bar.x + 16 * u,
            bar.y + 7 * u,
            &text,
            bar.w - 32 * u,
            color,
        );
        drop(ui);
        if let Some(menu) = &self.menu {
            menu.draw(&mut self.window.canvas(), &self.theme);
        }
        self.window.present();
        self.hits = hits;
    }
}

fn folder_icon(ui: &mut Ui<Control>, r: Rect, color: u32) {
    let u = ui.u;
    icons::rounded_rect(
        ui.c,
        Rect::new(r.x, r.y + r.h / 8, r.w / 2, r.h / 3),
        3 * u,
        color,
    );
    icons::rounded_rect(
        ui.c,
        Rect::new(r.x, r.y + r.h / 4, r.w, r.h * 3 / 4),
        4 * u,
        color,
    );
    icons::rounded_rect(
        ui.c,
        Rect::new(r.x, r.y + r.h * 3 / 8, r.w, r.h * 5 / 8),
        4 * u,
        with_alpha(WHITE, 40),
    );
}

fn file_icon(ui: &mut Ui<Control>, r: Rect, ext: &str) {
    let u = ui.u;
    let page = Rect::new(r.x + r.w / 6, r.y, r.w * 2 / 3, r.h);
    icons::rounded_rect(ui.c, page, 3 * u, ui.t.text);
    icons::rounded_rect(
        ui.c,
        Rect::new(page.x, page.bottom() - r.h / 3, page.w, r.h / 3),
        3 * u,
        ui.t.accent,
    );
    let label: String = ext.chars().take(3).collect();
    let lw = ui.f.small.measure(&label);
    if !label.is_empty() && lw < page.w + 8 * u {
        ui.f.small.draw(
            ui.c,
            page.x + page.w / 2 - lw / 2,
            page.bottom() - r.h / 3 - u,
            &label,
            WHITE,
        );
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("Dosyalar", (900 * u) as usize, (560 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("files: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let info = desktop::query();
    let prefs = info
        .as_ref()
        .map(|i| Prefs::parse(&i.text()))
        .unwrap_or_default();
    let home = info
        .as_ref()
        .map(|i| String::from(i.get("user.home")))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("/home/{}", process::username()));
    let mut app = Files {
        window,
        fonts: ui::fonts(),
        u,
        theme: Theme::from(&prefs),
        cwd: home.clone(),
        history: Vec::new(),
        entries: Vec::new(),
        selected: None,
        last_click: (usize::MAX, 0),
        scroll: 0,
        max_scroll: 0,
        mode: Mode::Browse,
        field: ui::TextInput::default(),
        clipboard: None,
        message: None,
        hits: Vec::new(),
        hover: None,
        home: home.clone(),
        dirty: true,
        menu: None,
    };
    let start = args.get(1).map(|s| String::from(*s)).unwrap_or(home);
    app.open_dir(&start, false);
    banna::println!("files: ready");
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
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_CONTEXT => app.context(event.x, event.y),
            EVENT_MOUSE_MOVE if app.menu.is_some() => {
                if app
                    .menu
                    .as_mut()
                    .is_some_and(|m| m.hover_at(event.x, event.y))
                {
                    app.dirty = true;
                }
            }
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    app.dirty = true;
                }
            }
            EVENT_WHEEL => {
                app.scroll = (app.scroll + event.key as i32 * 40 * u).clamp(0, app.max_scroll);
                app.dirty = true;
            }
            EVENT_KEY => {
                if !app.key(event.key) {
                    return 0;
                }
            }
            _ => {}
        }
    }
}
