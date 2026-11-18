//! Sürücü Yöneticisi: aygıtları ve onları süren sürücüleri gösterir, çekirdek modüllerini
//! yükler, sürücü ve sistem güncellemelerini (`banna::sysupdate`, hatch deposu) denetler,
//! indirip yeniden başlatınca kurulmak üzere hazırlar. Banna Store'dan kurulur.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::gfx::{Argb, Rect, rgb, with_alpha};
use banna::gui::{EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, EVENT_WHEEL, Window};
use banna::icons::{self, Icon};
use banna::sysupdate;
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, TextInput, Ui};
use banna::{desktop, fs, process, system, time};

banna::entry!(main);

const MODULES: &str = "/lib/modules";
const OK: Argb = rgb(0x3f, 0xb9, 0x6b);
const WARN: Argb = rgb(0xe0, 0x9a, 0x2a);
const BAD: Argb = rgb(0xe0, 0x4a, 0x42);

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Overview,
    Devices,
    Modules,
    Updates,
}

const NAV: [(Page, &str, Icon); 4] = [
    (Page::Overview, "Genel Bakış", Icon::Info),
    (Page::Devices, "Aygıtlar", Icon::Display),
    (Page::Modules, "Modüller", Icon::Store),
    (Page::Updates, "Güncellemeler", Icon::Network),
];

#[derive(Clone, PartialEq)]
enum Control {
    Nav(Page),
    Refresh,
    Check,
    Install,
    LoadModule(String),
    Password,
    Unlock,
}

struct Device {
    bdf: String,
    id: String,
    class: String,
    driver: String,
    state: String,
}

struct Update {
    version: String,
    files: usize,
    size: usize,
    drivers: Vec<String>,
    kernel: bool,
    firmware: usize,
    modules: usize,
}

struct App {
    window: Window,
    u: i32,
    page: Page,
    devices: Vec<Device>,
    loaded: Vec<(String, String)>,
    available: Vec<String>,
    usb: Vec<(String, String, String)>,
    driver_versions: Vec<(String, String)>,
    update: Option<Update>,
    status: Option<(String, Argb)>,
    password: TextInput,
    password_focus: bool,
    pending_action: Option<Control>,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    scroll: i32,
    max_scroll: i32,
}

fn state_color(state: &str, driver: &str) -> Argb {
    if driver == "-" {
        if state.contains("sistem") { rgb(0x8a, 0x8a, 0x95) } else { BAD }
    } else if state.starts_with("çalışıyor") || state == "modül" || state == "köprü" {
        OK
    } else {
        WARN
    }
}

impl App {
    fn reload(&mut self) {
        self.devices.clear();
        self.loaded.clear();
        self.usb.clear();
        for line in system::drivers_report().lines() {
            let f: Vec<&str> = line.split('\t').collect();
            match f.as_slice() {
                ["pci", bdf, id, class, driver, state] => self.devices.push(Device {
                    bdf: String::from(*bdf),
                    id: String::from(*id),
                    class: String::from(*class),
                    driver: String::from(*driver),
                    state: String::from(*state),
                }),
                ["mod", name, size] => self.loaded.push((String::from(*name), format!("{} KiB", size))),
                ["usb", port, id, name, drivers] => {
                    self.usb.push((format!("port {} – {}", port, id), String::from(*name), String::from(*drivers)))
                }
                _ => {}
            }
        }
        self.available = fs::read_dir(MODULES)
            .map(|l| {
                l.into_iter()
                    .map(|(n, _)| n)
                    .filter(|n| n.ends_with(".bko"))
                    .map(|n| String::from(n.trim_end_matches(".bko")))
                    .collect()
            })
            .unwrap_or_default();
        self.driver_versions = fs::read(sysupdate::DRIVERS_FILE)
            .map(|d| {
                String::from_utf8_lossy(&d)
                    .lines()
                    .filter_map(|l| l.split_once(' ').map(|(a, b)| (String::from(a), String::from(b))))
                    .collect()
            })
            .unwrap_or_default();
        if self.update.is_none()
            && let Ok(m) = sysupdate::cached()
        {
            self.update = Some(summarize(&m));
        }
    }

    fn admin(&mut self) -> bool {
        if process::uid() == 0 {
            return true;
        }
        let pw = String::from(self.password.as_str());
        if pw.is_empty() {
            self.status = Some((String::from("Bu işlem için yönetici şifresini girin."), WARN));
            self.password_focus = true;
            return false;
        }
        match process::elevate(&pw) {
            Ok(()) => {
                self.password.clear_text();
                self.password_focus = false;
                true
            }
            Err(-1) => {
                self.status = Some((String::from("Bu hesap yönetici değil."), BAD));
                false
            }
            Err(_) => {
                self.status = Some((String::from("Yönetici şifresi yanlış."), BAD));
                self.password.clear_text();
                false
            }
        }
    }

    fn check(&mut self) {
        self.status = Some((String::from("Banna deposu denetleniyor…"), WARN));
        self.paint();
        let result = sysupdate::repo_base().and_then(|repo| sysupdate::refresh(&repo));
        match result {
            Ok(m) => {
                let u = summarize(&m);
                self.status = Some(if u.files == 0 {
                    (format!("Sürücüler ve sistem güncel ({}).", m.version), OK)
                } else {
                    (format!("Güncelleme var: {} ({} dosya).", m.version, u.files), WARN)
                });
                self.update = Some(u);
            }
            Err(e) => self.status = Some((format!("Denetlenemedi: {}", e), BAD)),
        }
    }

    fn install(&mut self) {
        if !self.admin() {
            self.pending_action = Some(Control::Install);
            return;
        }
        self.status = Some((String::from("Güncelleme indiriliyor…"), WARN));
        self.paint();
        let result = sysupdate::repo_base().and_then(|repo| {
            let m = sysupdate::refresh(&repo)?;
            let n = sysupdate::stage(&repo, &m)?;
            Ok((m.version, n))
        });
        self.status = Some(match result {
            Ok((_, 0)) => (String::from("Kurulacak bir şey yok; sürücüler güncel."), OK),
            Ok((v, n)) => (format!("{} hazır ({} dosya). Bilgisayarı yeniden başlatınca kurulacak.", v, n), OK),
            Err(e) => (format!("Güncelleme hazırlanamadı: {}", e), BAD),
        });
    }

    fn load_module(&mut self, name: &str) {
        if !self.admin() {
            self.pending_action = Some(Control::LoadModule(String::from(name)));
            return;
        }
        let before = self.loaded.len();
        let _ = process::kcmd(&format!("insmod {}", name));
        time::sleep_ms(50);
        self.reload();
        self.status = Some(if self.loaded.len() > before {
            (format!("{} modülü yüklendi.", name), OK)
        } else {
            (format!("{} yüklenemedi (aygıt yok ya da zaten yüklü; ayrıntı: dmesg).", name), WARN)
        });
    }

    fn run(&mut self, control: Control) {
        match control {
            Control::Nav(p) => {
                self.page = p;
                self.scroll = 0;
            }
            Control::Refresh => {
                self.reload();
                self.status = Some((String::from("Aygıt listesi yenilendi."), OK));
            }
            Control::Check => self.check(),
            Control::Install => self.install(),
            Control::LoadModule(name) => self.load_module(&name),
            Control::Password => self.password_focus = true,
            Control::Unlock => {
                if let Some(action) = self.pending_action.take() {
                    self.run(action);
                } else if self.admin() {
                    self.status = Some((String::from("Yönetici yetkisi alındı."), OK));
                }
            }
        }
    }

    fn paint(&mut self) {
        let prefs = desktop::query().map(|i| Prefs::parse(&i.text())).unwrap_or_default();
        let t = Theme::from(&prefs);
        let u = self.u;
        let fonts = ui::fonts();
        let w = self.window.width as i32;
        let h = self.window.height as i32;
        let mut hits = core::mem::take(&mut self.hits);
        hits.clear();
        // Çizim boyunca `self.window`'a başka erişim yok.
        let window: *mut Window = &mut self.window;
        let mut c = unsafe { (*window).canvas() };
        c.fill(Rect::new(0, 0, w, h), t.window_bg);
        // Kenar çubuğu.
        let side = Rect::new(0, 0, 210 * u, h);
        c.fill(side, t.sidebar);
        fonts.title.draw(&mut c, 20 * u, 22 * u, "Sürücü Yöneticisi", t.text);
        for (i, (page, label, icon)) in NAV.iter().enumerate() {
            let r = Rect::new(10 * u, 64 * u + i as i32 * 44 * u, side.w - 20 * u, 38 * u);
            if self.page == *page {
                c.fill_rounded(r, 10 * u, with_alpha(t.accent & 0xffffff, 60));
            } else if self.hover == Some(r) {
                c.fill_rounded(r, 10 * u, with_alpha(t.text & 0xffffff, 18));
            }
            icons::draw(&mut c, Rect::new(r.x + 8 * u, r.y + 7 * u, 24 * u, 24 * u), *icon);
            fonts.body.draw(&mut c, r.x + 42 * u, r.y + (r.h - fonts.body.line_height()) / 2, label, t.text);
            hits.push(Hit { rect: r, control: Control::Nav(*page), slider: None });
        }
        let area = Rect::new(side.w + 24 * u, 0, w - side.w - 48 * u, h);
        let top = 20 * u - self.scroll;
        let end_y;
        {
            let mut g = Ui::new(&mut c, t.clone(), &fonts, u, area, top, &mut hits, self.hover);
            match self.page {
                Page::Overview => self.overview(&mut g),
                Page::Devices => self.devices_page(&mut g),
                Page::Modules => self.modules_page(&mut g),
                Page::Updates => self.updates_page(&mut g),
            }
            end_y = g.y + self.scroll;
        }
        self.max_scroll = (end_y - h + 80 * u).max(0);
        c.clip = Rect::new(0, 0, w, h);
        // Durum ve yönetici şifresi çubuğu.
        let bar = Rect::new(side.w, h - 52 * u, w - side.w, 52 * u);
        c.fill(bar, t.sidebar);
        if let Some((text, color)) = &self.status {
            c.fill_rounded(Rect::new(bar.x + 18 * u, bar.y + 20 * u, 10 * u, 10 * u), 5 * u, *color);
            fonts.body.draw_clipped(&mut c, bar.x + 36 * u, bar.y + (bar.h - fonts.body.line_height()) / 2, text, bar.w - 380 * u, t.text);
        }
        if process::uid() != 0 && (self.pending_action.is_some() || self.password_focus) {
            let field = Rect::new(bar.right() - 330 * u, bar.y + 8 * u, 220 * u, 36 * u);
            let button = Rect::new(bar.right() - 100 * u, bar.y + 8 * u, 86 * u, 36 * u);
            let mut g = Ui::new(&mut c, t.clone(), &fonts, u, bar, bar.y, &mut hits, self.hover);
            g.input_field(field, "Yönetici şifresi", &self.password, true, self.password_focus, Control::Password);
            g.primary_button(button, "Onayla", Control::Unlock);
        }
        drop(c);
        self.hits = hits;
        self.window.present();
    }

    fn overview(&self, g: &mut Ui<Control>) {
        g.heading("Genel Bakış");
        let with_driver = self.devices.iter().filter(|d| d.driver != "-" && d.driver != "pci").count();
        let missing: Vec<&Device> = self
            .devices
            .iter()
            .filter(|d| d.driver == "-" && !d.state.contains("sistem"))
            .collect();
        g.card(4);
        g.info("PCI aygıtları", &format!("{}", self.devices.len()));
        g.info("Sürücüsüyle çalışan", &format!("{}", with_driver));
        g.info("Yüklü modüller", &format!("{} / {}", self.loaded.len(), self.available.len()));
        g.info("USB aygıtları", &format!("{}", self.usb.len()));
        if !missing.is_empty() {
            g.section("Sürücüsü olmayan aygıtlar");
            g.card(missing.len());
            for d in missing {
                g.info(&d.class, &format!("{} – {}", d.id, d.state));
            }
            g.note("Bu aygıtlar için sürücüler geliştiriliyor; hazır olduklarında Güncellemeler sayfasından gelir.");
        }
        g.section("Güncellemeler");
        g.card(1);
        let text = match &self.update {
            Some(u) if u.files == 0 => format!("Güncel ({})", u.version),
            Some(u) => format!("{} hazır – {} sürücü", u.version, u.drivers.len()),
            None => String::from("Henüz denetlenmedi"),
        };
        g.buttons("Sürücü ve sistem güncellemeleri", Some(&text), &[("Denetle", Control::Check, false), ("Yenile", Control::Refresh, false)]);
    }

    fn devices_page(&self, g: &mut Ui<Control>) {
        g.heading("Aygıtlar");
        g.note("Her donanım aygıtı ve onu süren sürücü. Renk: yeşil çalışıyor, sarı kısmi, kırmızı sürücü yok.");
        g.card(self.devices.len());
        for d in &self.devices {
            let row = g.row();
            let color = state_color(&d.state, &d.driver);
            let u = g.u;
            g.c.fill_rounded(Rect::new(row.x + 14 * u, row.y + row.h / 2 - 5 * u, 10 * u, 10 * u), 5 * u, color);
            let label = Rect::new(row.x + 18 * u, row.y, row.w - 18 * u, row.h);
            g.label(label, &d.class, Some(&format!("{} – {}", d.bdf, d.id)), row.w / 2);
            let right = if d.driver == "-" { d.state.clone() } else { format!("{} — {}", d.driver, d.state) };
            let f = g.f.small;
            let x = row.right() - 16 * u - f.measure(&right).min(row.w / 2 - 20 * u);
            f.draw_clipped(g.c, x, row.y + (row.h - f.line_height()) / 2, &right, row.w / 2 - 20 * u, g.t.muted);
        }
        if !self.usb.is_empty() {
            g.section("USB");
            g.card(self.usb.len());
            for (place, name, drivers) in &self.usb {
                g.info(name, &format!("{} – {}", place, drivers));
            }
        }
        g.buttons("Listeyi yenile", None, &[("Yenile", Control::Refresh, false)]);
    }

    fn modules_page(&self, g: &mut Ui<Control>) {
        g.heading("Modüller");
        g.note("Çekirdek modülleri (/lib/modules/*.bko) sürücüleri çekirdeği değiştirmeden ekler ve günceller. Açılışta otomatik yüklenirler.");
        if self.available.is_empty() {
            g.card(1);
            g.info("Modül yok", MODULES);
            return;
        }
        g.card(self.available.len());
        for name in &self.available {
            let loaded = self.loaded.iter().find(|(n, _)| n == name);
            match loaded {
                Some((_, size)) => g.info(name, &format!("yüklü – {}", size)),
                None => g.buttons(name, Some("yüklü değil (aygıtı yoksa yüklenmez)"), &[("Yükle", Control::LoadModule(name.clone()), false)]),
            }
        }
    }

    fn updates_page(&self, g: &mut Ui<Control>) {
        g.heading("Güncellemeler");
        g.note("Sürücü ve sistem güncellemeleri Banna deposundan imzalı olarak gelir; bilgisayar yeniden başlatılınca kurulur (hatch upgrade ile aynı).");
        g.card(1);
        g.buttons("Depoyu denetle", Some(&format!("Kurulu sürüm: {}", sysupdate::current_version())), &[("Denetle", Control::Check, false)]);
        if let Some(p) = sysupdate::pending() {
            g.card(1);
            g.info("Yeniden başlatınca kurulacak", &p);
        }
        if let Some(e) = sysupdate::last_error() {
            g.card(1);
            g.info("Son kurulum başarısız", &e);
        }
        if let Some(u) = &self.update {
            g.section(&format!("Yayındaki sürüm: {}", u.version));
            if u.files == 0 {
                g.card(1);
                g.info("Durum", "Her şey güncel");
            } else {
                g.card(4);
                g.info("Değişen dosya", &format!("{} ({} KiB)", u.files, u.size.div_ceil(1024)));
                g.info("Çekirdek", if u.kernel { "güncellenecek (yeniden başlatma gerekir)" } else { "aynı" });
                g.info("Sürücü yazılımı (firmware)", &format!("{}", u.firmware));
                g.info("Çekirdek modülleri", &format!("{}", u.modules));
                if !u.drivers.is_empty() {
                    g.section("Güncellenen sürücüler");
                    g.card(u.drivers.len());
                    for d in &u.drivers {
                        let local = self.driver_versions.iter().find(|(n, _)| n == d).map(|(_, h)| h.as_str()).unwrap_or("yeni");
                        g.info(d, &format!("yerel {}", &local[..local.len().min(8)]));
                    }
                }
                g.card(1);
                g.buttons("İndir ve hazırla", Some("Yeniden başlatınca kurulur"), &[("Kur", Control::Install, true)]);
            }
        }
    }
}

fn summarize(m: &sysupdate::Manifest) -> Update {
    let changed = sysupdate::changed(m);
    Update {
        version: m.version.clone(),
        files: changed.len(),
        size: changed.iter().map(|e| e.size).sum(),
        drivers: sysupdate::driver_updates(m).unwrap_or_default(),
        kernel: changed.iter().any(|e| e.path == sysupdate::KERNEL),
        firmware: changed.iter().filter(|e| e.path.starts_with("/lib/firmware/")).count(),
        modules: changed.iter().filter(|e| e.path.starts_with("/lib/modules/")).count(),
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let info = process::sysinfo();
    let width = (960 * u).min(info.screen_width as i32 - 40 * u).clamp(640, 1920);
    let height = (680 * u).min(info.screen_height as i32 - 110 * u).clamp(480, 1200);
    let window = match Window::new("Sürücü Yöneticisi", width as usize, height as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("driver-manager: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let mut app = App {
        window,
        u,
        page: Page::Overview,
        devices: Vec::new(),
        loaded: Vec::new(),
        available: Vec::new(),
        usb: Vec::new(),
        driver_versions: Vec::new(),
        update: None,
        status: None,
        password: TextInput::default(),
        password_focus: false,
        pending_action: None,
        hits: Vec::new(),
        hover: None,
        scroll: 0,
        max_scroll: 0,
    };
    app.reload();
    // `driver-manager aygitlar|moduller|guncellemeler`: o sayfayla açılır.
    app.page = match args.get(1).copied() {
        Some("aygitlar" | "devices") => Page::Devices,
        Some("moduller" | "modules") => Page::Modules,
        Some("guncellemeler" | "updates") => Page::Updates,
        _ => Page::Overview,
    };
    banna::println!("driver-manager: ready ({} devices)", app.devices.len());
    let mut dirty = true;
    loop {
        if dirty {
            app.paint();
            dirty = false;
        }
        let Some(event) = app.window.event(2000) else { continue };
        match event.kind {
            EVENT_CLOSE => return 0,
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                dirty = true;
            }
            EVENT_MOUSE_DOWN => {
                if let Some(hit) = ui::hit_at(&app.hits, event.x, event.y).cloned() {
                    if hit.control != Control::Password {
                        app.password_focus = app.pending_action.is_some() && matches!(hit.control, Control::Unlock);
                    }
                    app.run(hit.control);
                }
                dirty = true;
            }
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    dirty = true;
                }
            }
            EVENT_WHEEL => {
                app.scroll = (app.scroll + event.key as i32 * 60 * u).clamp(0, app.max_scroll);
                dirty = true;
            }
            EVENT_KEY => {
                if app.password_focus {
                    match event.key {
                        13 => app.run(Control::Unlock),
                        27 => app.password_focus = false,
                        k => {
                            app.password.key(k, 128);
                        }
                    }
                }
                dirty = true;
            }
            _ => {}
        }
    }
}
