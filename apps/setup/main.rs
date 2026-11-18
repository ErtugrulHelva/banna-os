//! Banna OS ilk kurulum sihirbazı: kullanıcının ilk oturumunda masaüstü tarafından açılır
//! (hoş geldin, görünüm, duvar kağıdı, Banna hesabı ve Banna Store adımları).
//! Banna Panel'in kendisi Banna Store'dan kurulur (`banna-panel` paketi).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::ecosystem::{Client, Login};
use banna::gfx::text::{Font, UiFonts};
use banna::gfx::{Rect, Surface, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, Window,
};
use banna::icons::{self, Icon};
use banna::json::Value;
use banna::theme::{ACCENTS, Prefs, Theme};
use banna::ui::{self, Hit, Ui, WHITE};
use banna::wallpaper_art::{self as art, PRESETS};
use banna::{desktop, fs, process, time};

banna::entry!(main);

const STEPS: usize = 5;

#[derive(Clone, PartialEq)]
enum Control {
    Next,
    Back,
    Skip,
    Finish,
    OpenStore,
    Dark(bool),
    Accent(usize),
    Wallpaper(u8),
    Field(usize),
    Login,
}

const F_ADDRESS: usize = 0;
const F_PASSWORD: usize = 1;

struct Setup {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    prefs: Prefs,
    client: Client,
    step: usize,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    fields: [ui::TextInput; 2],
    focus: usize,
    message: Option<(String, bool)>,
    pending: Option<(String, String)>,
    last_poll: u64,
    signed_in: bool,
    me: Value,
    thumbs: Vec<Surface>,
    dirty: bool,
}

fn welcome_marker() -> String {
    format!("/home/{}/.config/banna/welcome", process::username())
}

impl Setup {
    fn refresh_theme(&mut self) {
        if let Some(info) = desktop::query() {
            self.prefs = Prefs::parse(&info.text());
            self.theme = Theme::from(&self.prefs);
        }
        self.dirty = true;
    }

    fn finish(&mut self) {
        let home = format!("/home/{}", process::username());
        let _ = fs::mkdir(&format!("{}/.config", home));
        let _ = fs::mkdir(&format!("{}/.config/banna", home));
        let _ = fs::write(&welcome_marker(), b"1\n");
        banna::println!("setup: finished");
    }

    fn load_account(&mut self) {
        match self.client.get("/auth/me") {
            Ok(me) => {
                self.me = me;
                self.signed_in = true;
            }
            Err(e) => {
                self.signed_in = false;
                if e.status != 401 {
                    self.message = Some((e.text(), true));
                }
            }
        }
        self.dirty = true;
    }

    fn login(&mut self) {
        if self.fields[F_ADDRESS].trim().is_empty() || self.fields[F_PASSWORD].is_empty() {
            self.message = Some((String::from("E-Banna adresini ve şifreyi girin."), true));
            return;
        }
        self.message = Some((String::from("Giriş yapılıyor…"), false));
        self.render();
        let (address, password) = (
            self.fields[F_ADDRESS].trim().to_ascii_lowercase(),
            self.fields[F_PASSWORD].clone(),
        );
        match self.client.login(&address, &password) {
            Ok(Login::Done) => {
                self.fields[F_PASSWORD].clear();
                self.message = None;
                banna::println!("setup: signed in");
                self.load_account();
            }
            Ok(Login::Pending { approval_id, code }) => {
                self.fields[F_PASSWORD].clear();
                self.message = None;
                self.pending = Some((approval_id, code));
                self.last_poll = time::uptime_ms();
            }
            Err(e) => self.message = Some((e.text(), true)),
        }
        self.dirty = true;
    }

    fn poll_approval(&mut self) {
        let Some((id, _)) = self.pending.clone() else {
            return;
        };
        if time::uptime_ms() - self.last_poll < 2500 {
            return;
        }
        self.last_poll = time::uptime_ms();
        match self.client.approval_status(&id).as_deref() {
            Ok("APPROVED") => {
                self.pending = None;
                self.load_account();
            }
            Ok("DENIED") => {
                self.pending = None;
                self.message = Some((String::from("Giriş isteği reddedildi."), true));
            }
            Ok("EXPIRED") | Err(_) => {
                self.pending = None;
                self.message = Some((String::from("Onay süresi doldu, tekrar deneyin."), true));
            }
            _ => {}
        }
        self.dirty = true;
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() else {
            return;
        };
        match hit.control {
            Control::Next => self.step = (self.step + 1).min(STEPS - 1),
            Control::Back => self.step = self.step.saturating_sub(1),
            Control::Skip | Control::Finish => {
                self.finish();
                process::exit(0);
            }
            Control::OpenStore => {
                let _ = process::spawn("/apps/store/store", &["store", "--install", "banna-panel"]);
                self.finish();
                process::exit(0);
            }
            Control::Dark(dark) => {
                desktop::set("dark", if dark { "1" } else { "0" });
                self.refresh_theme();
            }
            Control::Accent(i) => {
                desktop::set("accent", &format!("{}", i));
                self.refresh_theme();
            }
            Control::Wallpaper(p) => {
                desktop::set("wallpaper", &format!("preset:{}", p));
                self.refresh_theme();
            }
            Control::Field(i) => self.focus = i,
            Control::Login => self.login(),
        }
        self.dirty = true;
    }

    fn key(&mut self, key: u32) -> bool {
        if key == 27 {
            self.finish();
            return false;
        }
        if self.step == 3 && !self.signed_in && self.pending.is_none() {
            match key {
                9 => self.focus = (self.focus + 1) % 2,
                13 if self.focus == 0 => self.focus = 1,
                13 => self.login(),
                k => {
                    let max = if self.focus == 0 { 64 } else { 128 };
                    self.fields[self.focus].key(k, max);
                }
            }
            self.dirty = true;
        } else if key == 13 {
            self.step = (self.step + 1).min(STEPS - 1);
            self.dirty = true;
        }
        true
    }

    fn thumbnails(&mut self, w: i32, h: i32) {
        if self.thumbs.len() == PRESETS.len() && self.thumbs[0].width == w {
            return;
        }
        self.thumbs = (0..PRESETS.len() as u8)
            .map(|p| {
                let mut s = Surface::new(w, h, 0);
                for y in 0..h {
                    for x in 0..w {
                        s.pixels[(y * w + x) as usize] = art::pixel(p, x, y, w, h);
                    }
                }
                s
            })
            .collect();
    }

    fn render(&mut self) {
        self.dirty = false;
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        if self.step == 2 {
            self.thumbnails(170 * u, 96 * u);
        }
        let t = self.theme;
        let mut hits = Vec::new();
        let v = View {
            step: self.step,
            prefs: &self.prefs,
            fields: &self.fields,
            focus: self.focus,
            message: &self.message,
            pending: &self.pending,
            me: &self.me,
            thumbs: &self.thumbs,
            signed_in: self.signed_in,
        };
        {
            let mut c = self.window.canvas();
            c.fill(Rect::new(0, 0, w, h), t.window_bg);
            let mut ui = Ui::new(
                &mut c,
                t,
                &self.fonts,
                u,
                Rect::new(0, 0, w, h),
                0,
                &mut hits,
                self.hover,
            );
            draw_wizard(v, &mut ui, w, h);
        }
        self.window.present();
        self.hits = hits;
    }
}

struct View<'a> {
    step: usize,
    prefs: &'a Prefs,
    fields: &'a [ui::TextInput; 2],
    focus: usize,
    message: &'a Option<(String, bool)>,
    pending: &'a Option<(String, String)>,
    me: &'a Value,
    thumbs: &'a [Surface],
    signed_in: bool,
}

fn centered(ui: &mut Ui<Control>, font: &Font, y: i32, text: &str, color: u32, w: i32) {
    let tw = font.measure(text);
    font.draw(ui.c, w / 2 - tw / 2, y, text, color);
}

fn draw_message(ui: &mut Ui<Control>, v: &View, x: i32, y: i32, width: i32) {
    if let Some((text, error)) = v.message {
        let color = if *error { ui::DANGER } else { ui.t.muted };
        ui.f.small.draw_clipped(ui.c, x, y, text, width, color);
    }
}

/// E-Banna giriş formu (sihirbazda ve tek başına).
fn draw_login_form(v: &View, ui: &mut Ui<Control>, x: i32, y: i32, width: i32) {
    let u = ui.u;
    if let Some((_, code)) = v.pending {
        ui.f.title.draw(ui.c, x, y, "Onay bekleniyor", ui.t.text);
        for (i, line) in [
            "Bu cihaz hesabınızda yeni. Onay cihazınızda (Banna uygulaması",
            "ya da Banna Panel) bu girişi onaylayın.",
        ]
        .iter()
        .enumerate()
        {
            ui.f.body.draw(
                ui.c,
                x,
                y + 34 * u + i as i32 * ui.f.body.line_height(),
                line,
                ui.t.muted,
            );
        }
        let box_ = Rect::new(x, y + 96 * u, width, 64 * u);
        icons::rounded_rect(ui.c, box_, 12 * u, ui.t.card);
        ui.f.small.draw(
            ui.c,
            box_.x + 16 * u,
            box_.y + 10 * u,
            "Yedek kod",
            ui.t.muted,
        );
        ui.f.heading
            .draw(ui.c, box_.x + 16 * u, box_.y + 26 * u, code, ui.t.accent);
        return;
    }
    ui.f.small.draw(ui.c, x, y, "E-Banna adresi", ui.t.muted);
    ui.input_field(
        Rect::new(x, y + 20 * u, width, 40 * u),
        "kullanici@banna.com",
        &v.fields[F_ADDRESS],
        false,
        v.focus == 0,
        Control::Field(0),
    );
    ui.f.small.draw(ui.c, x, y + 72 * u, "Şifre", ui.t.muted);
    ui.input_field(
        Rect::new(x, y + 92 * u, width, 40 * u),
        "Şifre",
        &v.fields[F_PASSWORD],
        true,
        v.focus == 1,
        Control::Field(1),
    );
    ui.primary_button(
        Rect::new(x, y + 148 * u, width, 42 * u),
        "Giriş yap",
        Control::Login,
    );
    draw_message(ui, v, x, y + 200 * u, width);
}

fn draw_wizard(v: View, ui: &mut Ui<Control>, w: i32, h: i32) {
    let u = ui.u;
    let t = ui.t;
    let content_x = 56 * u;
    let content_w = w - 112 * u;
    match v.step {
        0 => {
            let logo = Rect::new(w / 2 - 48 * u, 70 * u, 96 * u, 96 * u);
            icons::logo(ui.c, logo, ui.f.heading, t.accent, WHITE);
            let heading = ui.f.heading;
            centered(ui, heading, 190 * u, "Banna OS'e hoş geldin", t.text, w);
            let body = ui.f.body;
            let name = process::username();
            centered(
                ui,
                body,
                240 * u,
                &format!("Merhaba {}! Birkaç adımda sistemini hazırlayalım:", name),
                t.muted,
                w,
            );
            centered(
                ui,
                body,
                268 * u,
                "görünüm, duvar kağıdı, Banna hesabın ve uygulamalar.",
                t.muted,
                w,
            );
        }
        1 => {
            ui.f.heading
                .draw(ui.c, content_x, 40 * u, "Görünüm", t.text);
            ui.f.body.draw(
                ui.c,
                content_x,
                82 * u,
                "Koyu ya da açık temayı ve vurgu rengini seç.",
                t.muted,
            );
            let card_w = (content_w - 24 * u) / 2;
            for (i, (label, dark)) in [("Koyu", true), ("Açık", false)].iter().enumerate() {
                let r = Rect::new(
                    content_x + i as i32 * (card_w + 24 * u),
                    120 * u,
                    card_w,
                    150 * u,
                );
                let selected = v.prefs.dark == *dark;
                if selected {
                    icons::rounded_rect(ui.c, r.inset(-4 * u), 18 * u, t.accent);
                }
                let preview = Theme::from(&Prefs {
                    dark: *dark,
                    ..v.prefs.clone()
                });
                icons::rounded_rect(ui.c, r, 14 * u, preview.window_bg);
                icons::rounded_rect(
                    ui.c,
                    Rect::new(r.x, r.y, r.w, 30 * u),
                    14 * u,
                    preview.title_active,
                );
                icons::rounded_rect(
                    ui.c,
                    Rect::new(r.x + 16 * u, r.y + 48 * u, r.w / 2, 14 * u),
                    7 * u,
                    preview.text,
                );
                icons::rounded_rect(
                    ui.c,
                    Rect::new(r.x + 16 * u, r.y + 74 * u, r.w / 3, 10 * u),
                    5 * u,
                    preview.muted,
                );
                icons::rounded_rect(
                    ui.c,
                    Rect::new(r.x + 16 * u, r.bottom() - 40 * u, 90 * u, 24 * u),
                    12 * u,
                    t.accent,
                );
                ui.f.title.draw(
                    ui.c,
                    r.right() - 70 * u,
                    r.bottom() - 38 * u,
                    label,
                    preview.text,
                );
                ui.hit(r, Control::Dark(*dark));
            }
            ui.f.body
                .draw(ui.c, content_x, 300 * u, "Vurgu rengi", t.text);
            for (i, (_, color)) in ACCENTS.iter().enumerate() {
                let cx = content_x + 20 * u + i as i32 * 52 * u;
                let cy = 350 * u;
                if i == v.prefs.accent as usize {
                    icons::circle_outline(ui.c, cx, cy, 22 * u, 3 * u, *color);
                    icons::disc(ui.c, cx, cy, 16 * u, *color);
                    icons::check(
                        ui.c,
                        Rect::new(cx - 10 * u, cy - 10 * u, 20 * u, 20 * u),
                        WHITE,
                    );
                } else {
                    icons::disc(ui.c, cx, cy, 16 * u, *color);
                }
                ui.hit(
                    Rect::new(cx - 24 * u, cy - 24 * u, 48 * u, 48 * u),
                    Control::Accent(i),
                );
            }
        }
        2 => {
            ui.f.heading
                .draw(ui.c, content_x, 40 * u, "Duvar kağıdı", t.text);
            ui.f.body.draw(
                ui.c,
                content_x,
                82 * u,
                "Daha fazlası ve kendi resimlerin için: Ayarlar → Duvar kağıdı",
                t.muted,
            );
            let (tw, th) = (170 * u, 96 * u);
            let gap = (content_w - 4 * tw) / 3;
            for (i, thumb) in v.thumbs.iter().enumerate() {
                let col = (i % 4) as i32;
                let row = (i / 4) as i32;
                let r = Rect::new(
                    content_x + col * (tw + gap),
                    124 * u + row * (th + 46 * u),
                    tw,
                    th,
                );
                if v.prefs.wallpaper == banna::theme::Wallpaper::Preset(i as u8) {
                    icons::rounded_rect(ui.c, r.inset(-4 * u), 14 * u, t.accent);
                }
                ui.c.blit_masked(thumb, r.x, r.y, 10 * u, 255);
                ui.f.small
                    .draw(ui.c, r.x, r.bottom() + 8 * u, PRESETS[i], t.muted);
                ui.hit(r, Control::Wallpaper(i as u8));
            }
        }
        3 => {
            ui.f.heading
                .draw(ui.c, content_x, 40 * u, "Banna hesabı", t.text);
            ui.f.body.draw(
                ui.c,
                content_x,
                82 * u,
                "Banna Store ve Banna Panel için E-Banna adresinle giriş yap.",
                t.muted,
            );
            if v.signed_in {
                let addr = v.me.get("current").get("ebannaAddress").as_str();
                icons::check(
                    ui.c,
                    Rect::new(content_x, 140 * u, 40 * u, 40 * u),
                    t.accent,
                );
                ui.f.title.draw(
                    ui.c,
                    content_x + 52 * u,
                    148 * u,
                    &format!("{} olarak giriş yapıldı", addr),
                    t.text,
                );
            } else {
                draw_login_form(&v, ui, content_x, 130 * u, 420 * u);
                ui.f.small.draw(
                    ui.c,
                    content_x + 460 * u,
                    150 * u,
                    "Hesabın yok mu? ecosys.trybanna.com",
                    t.muted,
                );
                ui.f.small.draw(
                    ui.c,
                    content_x + 460 * u,
                    172 * u,
                    "Bu adımı atlayabilirsin.",
                    t.muted,
                );
            }
        }
        _ => {
            let icon = Rect::new(w / 2 - 44 * u, 60 * u, 88 * u, 88 * u);
            icons::draw(ui.c, icon, Icon::Store);
            let heading = ui.f.heading;
            centered(ui, heading, 170 * u, "Her şey hazır", t.text, w);
            let body = ui.f.body;
            centered(
                ui,
                body,
                220 * u,
                "Banna Store'dan Banna ekosisteminin uygulamalarını kurabilirsin.",
                t.muted,
                w,
            );
            centered(
                ui,
                body,
                248 * u,
                "Hesabın, Banna Mail ve bildirimler için Banna Panel'i oradan kur.",
                t.muted,
                w,
            );
            ui.primary_button(
                Rect::new(w / 2 - 130 * u, 300 * u, 260 * u, 46 * u),
                "Banna Panel'i kur",
                Control::OpenStore,
            );
        }
    }

    // Alt şerit: adımlar, geri/ileri.
    let bar_y = h - 76 * u;
    ui.c.fill(
        Rect::new(0, bar_y, w, 1),
        with_alpha(if t.dark { 0xffffff } else { 0 }, 18),
    );
    for i in 0..STEPS {
        let cx = w / 2 - (STEPS as i32 - 1) * 12 * u + i as i32 * 24 * u;
        let color = if i == v.step { t.accent } else { t.control };
        icons::disc(ui.c, cx, bar_y + 38 * u, 5 * u, color);
    }
    let bw = 130 * u;
    if v.step > 0 {
        ui.secondary_button(
            Rect::new(32 * u, bar_y + 18 * u, bw, 40 * u),
            "Geri",
            Control::Back,
        );
    } else {
        ui.secondary_button(
            Rect::new(32 * u, bar_y + 18 * u, bw, 40 * u),
            "Atla",
            Control::Skip,
        );
    }
    let next = Rect::new(w - 32 * u - bw, bar_y + 18 * u, bw, 40 * u);
    if v.step + 1 < STEPS {
        let label = if v.step == 3 && !v.signed_in {
            "Şimdi değil"
        } else {
            "İleri"
        };
        ui.primary_button(next, label, Control::Next);
    } else {
        ui.primary_button(next, "Bitir", Control::Finish);
    }
}

fn main(_args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("Banna OS Kurulumu", (900 * u) as usize, (600 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("setup: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let prefs = desktop::query()
        .map(|i| Prefs::parse(&i.text()))
        .unwrap_or_default();
    let mut setup = Setup {
        window,
        fonts: ui::fonts(),
        u,
        theme: Theme::from(&prefs),
        prefs,
        client: Client::load(),
        step: 0,
        hits: Vec::new(),
        hover: None,
        fields: core::array::from_fn(|_| ui::TextInput::default()),
        focus: 0,
        message: None,
        pending: None,
        last_poll: 0,
        signed_in: false,
        me: Value::Null,
        thumbs: Vec::new(),
        dirty: true,
    };
    banna::println!("setup: ready");
    loop {
        if setup.dirty {
            setup.render();
        }
        let timeout = if setup.pending.is_some() { 500 } else { 30_000 };
        let Some(event) = setup.window.event(timeout) else {
            setup.poll_approval();
            continue;
        };
        match event.kind {
            EVENT_CLOSE => {
                setup.finish();
                return 0;
            }
            EVENT_RESIZE => {
                setup.window.resize(event.x as usize, event.y as usize);
                setup.dirty = true;
            }
            EVENT_MOUSE_DOWN => setup.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&setup.hits, event.x, event.y).map(|h| h.rect);
                if hovered != setup.hover {
                    setup.hover = hovered;
                    setup.dirty = true;
                }
            }
            EVENT_KEY => {
                if !setup.key(event.key) {
                    return 0;
                }
            }
            _ => {}
        }
        setup.poll_approval();
    }
}
