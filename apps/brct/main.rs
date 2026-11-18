//! Brct: Banna-Rust Compatibility Tool'un Banna OS tarafı.
//!
//! Dock'tan açılınca pencere: brct ile yapılmış (app.conf'ta `builder=brct`) uygulamalar
//! ve hızlı başlangıç. Terminalden çalıştırılınca komut satırı aracı (`brct yardım`):
//! listeleme, çalıştırma, bilgi, kurma/kaldırma, yeni proje ve denetim.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::gfx::png::{self, Image};
use banna::gfx::{Rect, rgb};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, Window,
};
use banna::icons;
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, Ui};
use banna::{desktop, fs, process};

banna::entry!(main);

mod cli;

const LOGO: &str = "/apps/brct/logo.png";

#[derive(Clone, PartialEq)]
enum Control {
    Open(String),
}

struct App {
    name: String,
    description: String,
    exec: String,
}

/// `/apps` altında `builder=brct` işaretli uygulamalar.
fn brct_apps() -> Vec<App> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/apps") else {
        return out;
    };
    for (id, _) in entries {
        let Ok(data) = fs::read(&format!("/apps/{}/app.conf", id)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&data);
        let mut app = App {
            name: id.clone(),
            description: String::new(),
            exec: format!("/apps/{}/{}", id, id),
        };
        let mut brct = false;
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "builder" => brct = value == "brct",
                "name" => app.name = String::from(value),
                "description" => app.description = String::from(value),
                "exec" if !value.starts_with('/') => app.exec = format!("/apps/{}/{}", id, value),
                _ => {}
            }
        }
        if brct {
            out.push(app);
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

const STEPS: [(&str, &str); 6] = [
    (
        "make brct-install",
        "brct'yi bilgisayarına kurar (macOS, Linux; banna_os klasöründe)",
    ),
    (
        "brct new merhaba",
        "yeni masaüstü uygulaması (--cli: komut satırı programı)",
    ),
    (
        "brct merhaba --release",
        "Banna OS için derler: merhaba/target/banna/",
    ),
    (
        "brct package merhaba",
        "paketler; ardından make repo ile depoya eklenir",
    ),
    (
        "hatch install merhaba",
        "Banna OS'ta depodan kurar (Banna Store'da 'brct' rozetiyle görünür)",
    ),
    (
        "sudo brct install /usb0/merhaba",
        "ya da USB bellekten doğrudan kurar (terminalde: brct yardım)",
    ),
];

struct Brct {
    window: Window,
    theme: Theme,
    logo: Option<Image>,
    logo_scaled: Option<(u32, u32, Vec<u32>)>,
    apps: Vec<App>,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    status: String,
}

impl Brct {
    fn render(&mut self) {
        let u = ui::unit();
        let f = ui::fonts();
        let t = self.theme;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let mut hits = Vec::new();
        // Logo, başlık kartının yüksekliğine göre bir kez küçültülür.
        let card_h = 112 * u;
        let logo_h = (card_h - 24 * u) as u32;
        if let Some(logo) = &self.logo
            && self
                .logo_scaled
                .as_ref()
                .is_none_or(|(_, lh, _)| *lh != logo_h)
        {
            let lw = logo.width * logo_h / logo.height.max(1);
            self.logo_scaled = Some((lw, logo_h, logo.scaled(lw, logo_h)));
        }
        {
            let mut c = self.window.canvas();
            c.fill(Rect::new(0, 0, w, h), t.window_bg);
            let pad = 24 * u;
            let card = Rect::new(pad, pad, w - 2 * pad, card_h);
            icons::rounded_rect(&mut c, card, 18 * u, rgb(0xff, 0xff, 0xff));
            let mut tx = card.x + 24 * u;
            if let Some((lw, lh, pixels)) = &self.logo_scaled {
                let (lw, lh) = (*lw as i32, *lh as i32);
                c.blend_image(pixels, lw, lh, tx, card.y + (card.h - lh) / 2);
                tx += lw + 28 * u;
            }
            let ink = rgb(0x1d, 0x3f, 0x66);
            let soft = rgb(0x5a, 0x66, 0x78);
            f.title.draw(&mut c, tx, card.y + 26 * u, "brct", ink);
            f.small.draw_clipped(
                &mut c,
                tx,
                card.y + 52 * u,
                "Bilgisayarında yazdığın Rust uygulamalarını",
                card.right() - tx - 16 * u,
                soft,
            );
            f.small.draw_clipped(
                &mut c,
                tx,
                card.y + 72 * u,
                "Banna OS için derler ve paketler.",
                card.right() - tx - 16 * u,
                soft,
            );

            let area = Rect::new(
                pad,
                card.bottom() + 8 * u,
                w - 2 * pad,
                h - card.bottom() - 8 * u,
            );
            let mut ui = Ui::new(&mut c, t, &f, u, area, area.y, &mut hits, self.hover);
            ui.section("brct ile yapılmış uygulamalar");
            if self.apps.is_empty() {
                ui.note("Henüz yok. brct ile paketleyip kurduğun uygulamalar burada görünür.");
            } else {
                ui.card(self.apps.len());
                for app in &self.apps {
                    let row = ui.row();
                    let desc = (!app.description.is_empty()).then_some(app.description.as_str());
                    ui.label(row, &app.name, desc, row.w - 140 * u);
                    let b = Rect::new(
                        row.right() - 100 * u,
                        row.y + (row.h - 32 * u) / 2,
                        84 * u,
                        32 * u,
                    );
                    ui.secondary_button(b, "Aç", Control::Open(app.exec.clone()));
                }
            }
            ui.section("Hızlı başlangıç");
            ui.card(STEPS.len());
            for (command, what) in STEPS {
                let row = ui.row();
                ui.label(row, command, Some(what), row.w - 16 * u);
            }
            if !self.status.is_empty() {
                ui.note(&self.status);
            }
        }
        self.hits = hits;
        self.window.present();
    }

    fn press(&mut self, x: i32, y: i32) -> bool {
        let Some(hit) = ui::hit_at(&self.hits, x, y) else {
            return false;
        };
        let Control::Open(exec) = hit.control.clone();
        let name = String::from(exec.rsplit('/').next().unwrap_or(&exec));
        self.status = match process::spawn(&exec, &[&name]) {
            Ok(_) => format!("{} açıldı", name),
            Err(e) => format!("{} açılamadı ({})", name, e),
        };
        true
    }
}

fn main(args: &[&str]) -> i32 {
    // Terminalden çalıştırılınca komut satırı aracı; `brct open` ya da dock'tan pencere.
    if banna::tty::is_tty(1) && args.get(1).is_none_or(|a| !matches!(*a, "open" | "gui")) {
        return cli::run(&args[1.min(args.len())..]);
    }
    let u = ui::unit();
    let window = match Window::new("Brct", (680 * u) as usize, (560 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("brct: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let theme = desktop::query()
        .map(|i| Theme::from(&Prefs::parse(&i.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    let logo = fs::read(LOGO).ok().and_then(|d| png::decode(&d));
    if logo.is_none() {
        banna::eprintln!("brct: {} okunamadı", LOGO);
    }
    let mut app = Brct {
        window,
        theme,
        logo,
        logo_scaled: None,
        apps: brct_apps(),
        hits: Vec::new(),
        hover: None,
        status: String::new(),
    };
    banna::println!("brct: ready ({} app(s))", app.apps.len());
    let mut dirty = true;
    loop {
        if dirty {
            app.render();
            dirty = false;
        }
        let Some(event) = app.window.event(60_000) else {
            continue;
        };
        match event.kind {
            EVENT_CLOSE => return 0,
            EVENT_KEY if event.key == 27 => return 0,
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                dirty = true;
            }
            EVENT_MOUSE_DOWN => dirty = app.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    dirty = true;
                }
            }
            _ => {}
        }
    }
}
