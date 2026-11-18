//! Uygulama sayfası (Android'deki Banna Store'un tanıtım sayfasıyla aynı düzen): simge, ad ve
//! kısa açıklama, geniş kurulum düğmesi, "Hakkında", "Bu sürümde yenilikler" (sürüm notları) ve
//! sürüm, boyut, yayın tarihi gibi bilgiler. Banna OS paketleri, diğer platformların uygulamaları
//! ve sistem güncellemesi için kullanılır.
//!
//! Ayrıca "Yenilikler" penceresi: Store açılınca, son açılıştan bu yana güncellenen paketlerin ve
//! sistemin sürüm notlarını bir kez gösterir.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::gfx::text::Font;
use banna::gfx::{Rect, with_alpha};
use banna::gui::Window;
use banna::icons::{self, Icon};
use banna::ui::{Hit, Ui, WHITE};
use banna::{fs, process};

use super::{Control, Platform, Store, newer, size_text};

/// Açık uygulama sayfası.
#[derive(Clone, PartialEq)]
pub enum Detail {
    Package(String),
    Remote(String, Platform),
    System,
}

enum Picture {
    Builtin(Icon),
    Pixels(String),
    Logo,
    Letter,
}

/// Sayfanın içeriği (çizimden bağımsız).
struct View {
    title: String,
    tagline: String,
    meta: String,
    picture: Picture,
    /// (yazı, denetim, birincil mi)
    actions: Vec<(String, Control, bool)>,
    /// Düğme yerine gösterilen durum (ör. "Yeniden başlatınca kurulacak").
    state: Option<String>,
    note: Option<String>,
    about: String,
    notes_title: String,
    notes: Vec<String>,
    extra: Option<(String, Vec<String>)>,
    info: Vec<(String, String)>,
}

/// Metni `width` genişliğine sözcük sözcük böler.
pub fn wrap(font: &Font, text: &str, width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for paragraph in text.split('\n') {
        let mut line = String::new();
        for word in paragraph.split_whitespace() {
            let candidate = if line.is_empty() {
                String::from(word)
            } else {
                format!("{} {}", line, word)
            };
            if !line.is_empty() && font.measure(&candidate) > width {
                lines.push(core::mem::replace(&mut line, String::from(word)));
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

/// "2026-11-01" ya da "2026-11-01T20:15:29Z" → "01.11.2026".
pub fn date_text(s: &str) -> String {
    let d = s.get(..10).unwrap_or("");
    let mut parts = d.split('-');
    match (parts.next(), parts.next(), parts.next()) {
        (Some(y), Some(m), Some(day)) if y.len() == 4 => format!("{}.{}.{}", day, m, y),
        _ => String::new(),
    }
}

/// API'nin "• madde" satırlarını maddelere çevirir.
pub fn split_notes(text: &str) -> Vec<String> {
    text.lines()
        .map(|l| l.trim().trim_start_matches(['•', '-']).trim())
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

fn app_installed(name: &str) -> bool {
    fs::stat(&format!("/apps/{0}/{0}", name)).is_ok()
}

pub fn launch(name: &str) -> bool {
    process::spawn(&format!("/apps/{0}/{0}", name), &[name]).is_ok()
}

// ---- "Yenilikler" ----

/// Store'un en son gösterdiği sürümler: "<paket> <sürüm>" ve "system <sürüm>".
fn seen_path() -> String {
    let home = banna::desktop::query()
        .map(|i| String::from(i.get("user.home")))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("/home/{}", process::username()));
    format!("{}/.config/banna/store-seen", home)
}

fn current_state(installed: &[(String, String)]) -> String {
    let mut text = format!("system {}\n", banna::sysupdate::current_version());
    for (name, version) in installed {
        text.push_str(&format!("{} {}\n", name, version));
    }
    text
}

pub fn remember(installed: &[(String, String)]) {
    let path = seen_path();
    if let Some((dir, _)) = path.rsplit_once('/') {
        let config = dir.rsplit_once('/').map_or(dir, |(c, _)| c);
        let _ = fs::mkdir(config);
        let _ = fs::mkdir(dir);
    }
    let _ = fs::write(&path, current_state(installed).as_bytes());
}

impl Store {
    /// Son açılıştan bu yana güncellenenlerin notları; ilk açılışta yalnızca durum kaydedilir.
    pub fn collect_whats_new(&mut self) {
        let Ok(data) = fs::read(&seen_path()) else {
            remember(&self.installed);
            return;
        };
        let seen = String::from_utf8_lossy(&data).into_owned();
        let seen_version = |name: &str| {
            seen.lines()
                .filter_map(|l| l.split_once(' '))
                .find(|(n, _)| *n == name)
                .map(|(_, v)| String::from(v.trim()))
        };
        let mut news: Vec<(String, Vec<String>)> = Vec::new();
        let system = banna::sysupdate::current_version();
        if seen_version("system").is_some_and(|v| v != system)
            && let Ok(m) = banna::sysupdate::cached()
            && m.version == system
        {
            let title = if m.notes.is_empty() {
                format!("Banna OS {}", system)
            } else {
                format!("Banna OS — {}", m.notes)
            };
            news.push((title, m.changes));
        }
        for (name, version) in &self.installed {
            let Some(before) = seen_version(name) else {
                continue;
            };
            if before == *version || !newer(version, &before) {
                continue;
            }
            if let Some(p) = self
                .packages
                .iter()
                .find(|p| &p.name == name && &p.version == version)
            {
                news.push((format!("{} {}", p.title, p.version), p.notes.clone()));
            }
        }
        if news.is_empty() {
            remember(&self.installed);
        } else {
            self.whats_new = Some(news);
        }
    }

    fn view(&self, detail: &Detail) -> Option<View> {
        match detail {
            Detail::Package(name) => {
                let p = self.packages.iter().find(|p| &p.name == name)?;
                let installed = self
                    .installed
                    .iter()
                    .find(|(n, _)| n == name)
                    .map(|(_, v)| v.clone());
                let mut actions = Vec::new();
                match &installed {
                    None => {
                        actions.push((String::from("Yükle"), Control::Install(name.clone()), true))
                    }
                    Some(v) if newer(&p.version, v) => {
                        actions.push((
                            String::from("Güncelle"),
                            Control::Upgrade(name.clone()),
                            true,
                        ));
                        actions.push((
                            String::from("Kaldır"),
                            Control::Remove(name.clone()),
                            false,
                        ));
                    }
                    Some(_) => {
                        if app_installed(name) {
                            actions.push((String::from("Aç"), Control::Launch(name.clone()), true));
                        }
                        actions.push((
                            String::from("Kaldır"),
                            Control::Remove(name.clone()),
                            false,
                        ));
                    }
                }
                let mut info = alloc::vec![(String::from("Sürüm"), p.version.clone())];
                if let Some(v) = &installed {
                    info.push((String::from("Kurulu sürüm"), v.clone()));
                }
                info.push((String::from("Boyut"), size_text(p.size)));
                if !p.date.is_empty() {
                    info.push((String::from("Yayın tarihi"), date_text(&p.date)));
                }
                info.push((String::from("Paket"), p.name.clone()));
                if !p.depends.is_empty() {
                    info.push((String::from("Bağımlılıklar"), p.depends.join(", ")));
                }
                info.push((
                    String::from("Doğrulama"),
                    String::from("SHA-256 + RSA imzalı depo"),
                ));
                if p.brct {
                    info.push((String::from("Yapım"), String::from("brct ile paketlendi")));
                }
                Some(View {
                    title: p.title.clone(),
                    tagline: p.description.clone(),
                    meta: format!("{} • {} • Banna OS", p.version, size_text(p.size)),
                    picture: p.icon.map_or(Picture::Letter, Picture::Builtin),
                    actions,
                    state: installed
                        .as_ref()
                        .filter(|v| !newer(&p.version, v))
                        .map(|_| String::from("Yüklü")),
                    note: None,
                    about: if p.about.is_empty() {
                        p.description.clone()
                    } else {
                        p.about.clone()
                    },
                    notes_title: String::from("Bu sürümde yenilikler"),
                    notes: p.notes.clone(),
                    extra: None,
                    info,
                })
            }
            Detail::Remote(id, platform) => {
                let a = self
                    .remote
                    .iter()
                    .find(|a| &a.id == id && a.platforms.contains(platform))?;
                let mut info = alloc::vec![(String::from("Sürüm"), a.version.clone())];
                if a.size > 0 {
                    info.push((String::from("Boyut"), size_text(a.size as usize)));
                }
                if !a.date.is_empty() {
                    info.push((String::from("Yayın tarihi"), date_text(&a.date)));
                }
                info.push((String::from("Platform"), String::from(platform.label())));
                if !a.detail.is_empty() {
                    info.push((String::from("Mimari"), a.detail.clone()));
                }
                let mut meta = a.version.clone();
                if a.size > 0 {
                    meta = format!("{} • {}", meta, size_text(a.size as usize));
                }
                Some(View {
                    title: a.name.clone(),
                    tagline: a.tagline.clone(),
                    meta: format!("{} • {}", meta, platform.label()),
                    picture: match a.icon {
                        Some(i) => Picture::Builtin(i),
                        None if self.remote_icons.iter().any(|(i, _)| i == id) => {
                            Picture::Pixels(id.clone())
                        }
                        None => Picture::Letter,
                    },
                    actions: if a.link.is_empty() {
                        Vec::new()
                    } else {
                        alloc::vec![(
                            String::from("Bağlantıyı kopyala"),
                            Control::CopyLink(a.link.clone()),
                            true
                        )]
                    },
                    state: None,
                    note: Some(format!(
                        "{} Bu cihaz Banna OS çalıştırıyor; bağlantıyı kopyalayıp o cihazda açın.",
                        platform.hint()
                    )),
                    about: if a.description.is_empty() {
                        a.tagline.clone()
                    } else {
                        a.description.clone()
                    },
                    notes_title: String::from("Bu sürümde yenilikler"),
                    notes: a.notes.clone(),
                    extra: None,
                    info,
                })
            }
            Detail::System => {
                let s = self.system.as_ref()?;
                let mut info = alloc::vec![
                    (
                        String::from("Kurulu sürüm"),
                        banna::sysupdate::current_version()
                    ),
                    (String::from("Yeni sürüm"), s.version.clone()),
                    (String::from("Değişen dosya"), format!("{}", s.files)),
                    (String::from("İndirme"), size_text(s.size)),
                ];
                if !s.date.is_empty() {
                    info.push((String::from("Yayın tarihi"), date_text(&s.date)));
                }
                info.push((
                    String::from("Doğrulama"),
                    String::from("SHA-256 + RSA imzalı bildirim"),
                ));
                info.push((
                    String::from("Kurulum"),
                    String::from("Yeniden başlatınca, oturumdan önce"),
                ));
                Some(View {
                    title: String::from("Banna OS sistem güncellemesi"),
                    tagline: if s.notes.is_empty() {
                        String::from("Çekirdek, Banna Store ve yerleşik uygulamalar")
                    } else {
                        s.notes.clone()
                    },
                    meta: format!("{} • {} dosya • {}", s.version, s.files, size_text(s.size)),
                    picture: Picture::Logo,
                    actions: if s.ready {
                        Vec::new()
                    } else {
                        alloc::vec![(String::from("Güncelle"), Control::UpgradeAll, true)]
                    },
                    state: s
                        .ready
                        .then(|| String::from("Yeniden başlatınca kurulacak")),
                    note: None,
                    about: String::from(
                        "Sistem güncellemesi çekirdeği, UEFI yükleyicisini, sürücüleri, Banna Store'u ve \
                         yerleşik uygulamaları birlikte günceller. Değişen dosyalar şimdi indirilip \
                         doğrulanır; bilgisayar yeniden başlatılınca oturum açılmadan kurulur. Sorun \
                         çıkarsa önyükleme menüsünden önceki sürüme dönebilirsiniz.",
                    ),
                    notes_title: String::from("Bu güncellemede"),
                    notes: s.changes.clone(),
                    extra: (!s.apps.is_empty())
                        .then(|| (String::from("Güncellenen uygulamalar"), s.apps.clone())),
                    info,
                })
            }
        }
    }

    /// Uygulama sayfasını çizer; `true` dönerse sayfa açıktır (liste çizilmez).
    pub fn render_detail(&mut self) -> bool {
        let Some(detail) = self.detail.clone() else {
            return false;
        };
        let Some(v) = self.view(&detail) else {
            // Kayıt artık yok (ör. katalog yenilendi): listeye dön.
            self.detail = None;
            return false;
        };
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let t = self.theme;
        let mut hits: Vec<Hit<Control>> = Vec::new();
        let busy = self.task.is_some();
        {
            // Çizim boyunca pencereye başka erişim yok: tuval pencereden ayrı ödünç alınır.
            let window: *mut Window = &mut self.window;
            let mut c = unsafe { (*window).canvas() };
            let f = &self.fonts;
            c.fill(Rect::new(0, 0, w, h), t.window_bg);

            let status_h = 36 * u;
            let bar_h = 56 * u;
            let content_w = (w - 80 * u).min(760 * u);
            let x0 = (w - content_w) / 2;
            let body = Rect::new(0, bar_h, w, h - bar_h - status_h);
            c.clip = body;
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
            ui.clip = body;
            let mut y = bar_h + 24 * u - self.detail_scroll;

            // Başlık: simge, ad, kısa açıklama, sürüm bilgisi.
            let icon = Rect::new(x0, y, 84 * u, 84 * u);
            match &v.picture {
                Picture::Builtin(i) => icons::draw(ui.c, icon, *i),
                Picture::Logo => icons::logo(ui.c, icon, f.heading, t.accent, WHITE),
                Picture::Letter => icons::letter(ui.c, icon, &v.title, f.heading),
                Picture::Pixels(id) => {
                    if let Some((_, px)) = self.remote_icons.iter().find(|(i, _)| i == id) {
                        // Simgeler 48u boyutunda saklanır: ortalanır.
                        let s = 48 * u;
                        icons::rounded_rect(ui.c, icon, 20 * u, t.card);
                        ui.c.blend_image(
                            px,
                            s,
                            s,
                            icon.x + (icon.w - s) / 2,
                            icon.y + (icon.h - s) / 2,
                        );
                    }
                }
            }
            let tx = icon.right() + 18 * u;
            let tw = x0 + content_w - tx;
            f.heading
                .draw_clipped(ui.c, tx, y + 2 * u, &v.title, tw, t.text);
            let mut ty = y + f.heading.line_height() + 8 * u;
            for line in wrap(f.body, &v.tagline, tw).iter().take(2) {
                f.body.draw_clipped(ui.c, tx, ty, line, tw, t.muted);
                ty += f.body.line_height();
            }
            f.small
                .draw_clipped(ui.c, tx, ty + 4 * u, &v.meta, tw, t.muted);
            y = (icon.bottom()).max(ty + f.small.line_height() + 4 * u) + 20 * u;

            // Geniş düğmeler.
            if !v.actions.is_empty() {
                let gap = 10 * u;
                let n = v.actions.len() as i32;
                let primary_w = if n == 1 {
                    content_w
                } else {
                    content_w - (n - 1) * (160 * u + gap)
                };
                let mut bx = x0;
                for (label, control, primary) in &v.actions {
                    let bw = if *primary { primary_w } else { 160 * u };
                    let r = Rect::new(bx, y, bw, 40 * u);
                    if busy {
                        icons::rounded_rect(ui.c, r, 12 * u, with_alpha(t.text, 20));
                        let lw = f.body.measure(label);
                        f.body.draw(
                            ui.c,
                            r.x + (r.w - lw) / 2,
                            r.y + (r.h - f.body.ascent - f.body.descent) / 2,
                            label,
                            t.muted,
                        );
                    } else if *primary {
                        ui.primary_button(r, label, control.clone());
                    } else {
                        ui.secondary_button(r, label, control.clone());
                    }
                    bx += bw + gap;
                }
                y += 40 * u + 10 * u;
            }
            if let Some(state) = &v.state {
                f.small.draw(ui.c, x0, y, state, t.accent);
                y += f.small.line_height() + 6 * u;
            }
            if let Some(note) = &v.note {
                for line in wrap(f.small, note, content_w) {
                    f.small.draw(ui.c, x0, y, &line, t.muted);
                    y += f.small.line_height();
                }
                y += 6 * u;
            }
            y += 8 * u;

            // Kartlar.
            let pad = 18 * u;
            let inner = content_w - 2 * pad;
            let card = |ui: &mut Ui<Control>, y: i32, height: i32| {
                icons::rounded_rect(ui.c, Rect::new(x0, y, content_w, height), 16 * u, t.card);
            };
            if !v.about.is_empty() {
                let lines = wrap(f.body, &v.about, inner);
                let lh = f.body.line_height() + 3 * u;
                let height = pad * 2 + f.title.line_height() + 8 * u + lines.len() as i32 * lh;
                card(&mut ui, y, height);
                f.title.draw(ui.c, x0 + pad, y + pad, "Hakkında", t.text);
                let mut ly = y + pad + f.title.line_height() + 8 * u;
                for line in &lines {
                    f.body.draw(ui.c, x0 + pad, ly, line, t.text);
                    ly += lh;
                }
                y += height + 12 * u;
            }
            let list_card =
                |ui: &mut Ui<Control>, y: &mut i32, title: &str, items: &[String], check: bool| {
                    let indent = if check { 30 * u } else { 0 };
                    let wrapped: Vec<Vec<String>> = items
                        .iter()
                        .map(|i| wrap(f.body, i, inner - indent))
                        .collect();
                    let lh = f.body.line_height() + 2 * u;
                    let rows: i32 = wrapped.iter().map(|l| l.len() as i32).sum();
                    let height = pad * 2
                        + f.title.line_height()
                        + 10 * u
                        + rows * lh
                        + items.len() as i32 * 8 * u;
                    card(ui, *y, height);
                    f.title.draw(ui.c, x0 + pad, *y + pad, title, t.text);
                    let mut ly = *y + pad + f.title.line_height() + 10 * u;
                    for lines in &wrapped {
                        if check {
                            let mark = Rect::new(x0 + pad, ly + 1 * u, 18 * u, 18 * u);
                            icons::disc(
                                ui.c,
                                mark.x + mark.w / 2,
                                mark.y + mark.h / 2,
                                9 * u,
                                t.accent,
                            );
                            icons::check(ui.c, mark.inset(4 * u), WHITE);
                        }
                        for line in lines {
                            f.body.draw(ui.c, x0 + pad + indent, ly, line, t.text);
                            ly += lh;
                        }
                        ly += 8 * u;
                    }
                    *y += height + 12 * u;
                };
            if !v.notes.is_empty() {
                list_card(&mut ui, &mut y, &v.notes_title, &v.notes, true);
            }
            if let Some((title, items)) = &v.extra {
                list_card(&mut ui, &mut y, title, items, false);
            }
            // Bilgi satırları.
            let row = f.body.line_height() + 16 * u;
            let height = 12 * u + v.info.len() as i32 * row;
            card(&mut ui, y, height);
            let mut ry = y + 6 * u;
            for (i, (label, value)) in v.info.iter().enumerate() {
                if i > 0 {
                    ui.c.fill(
                        Rect::new(x0 + pad, ry, inner, u.max(1)),
                        with_alpha(t.text, 18),
                    );
                }
                let ly = ry + (row - f.body.ascent - f.body.descent) / 2;
                f.body.draw(ui.c, x0 + pad, ly, label, t.muted);
                let vw = f.body.measure(value).min(inner * 3 / 5);
                f.body.draw_clipped(
                    ui.c,
                    x0 + content_w - pad - vw,
                    ly,
                    value,
                    inner * 3 / 5,
                    t.text,
                );
                ry += row;
            }
            y += height + 24 * u;
            let content_bottom = y + self.detail_scroll;
            drop(ui);
            self.detail_max_scroll = (content_bottom - (h - status_h)).max(0);

            // Üst çubuk: geri ve başlık (kaydırılan içeriğin üstünde).
            c.clip = Rect::new(0, 0, w, h);
            c.fill(Rect::new(0, 0, w, bar_h), t.sidebar);
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
            ui.secondary_button(
                Rect::new(16 * u, 11 * u, 90 * u, 34 * u),
                "← Geri",
                Control::Back,
            );
            let title_w = f.title.measure(&v.title).min(w - 260 * u);
            f.title.draw_clipped(
                ui.c,
                (w - title_w) / 2,
                (bar_h - f.title.ascent - f.title.descent) / 2,
                &v.title,
                w - 260 * u,
                t.text,
            );
            drop(ui);
            self.draw_status(&mut c, w, h);
            self.render_whats_new(&mut c, &mut hits);
            self.draw_unlock(&mut c, &mut hits);
            self.draw_pressed(&mut c);
        }
        self.window.present();
        self.hits = hits;
        true
    }

    /// "Yenilikler" penceresi (liste ya da sayfanın üstüne).
    pub fn render_whats_new(&self, c: &mut banna::gfx::Canvas, hits: &mut Vec<Hit<Control>>) {
        let Some(news) = &self.whats_new else {
            return;
        };
        let u = self.u;
        let t = self.theme;
        let f = &self.fonts;
        let (w, h) = (c.width, c.height);
        c.fill(Rect::new(0, 0, w, h), with_alpha(0, 120));
        let cw = (w - 80 * u).min(520 * u);
        let inner = cw - 48 * u;
        let mut lines: Vec<(String, bool)> = Vec::new();
        for (title, items) in news {
            lines.push((title.clone(), true));
            if items.is_empty() {
                lines.push((
                    String::from("• Hata düzeltmeleri ve iyileştirmeler."),
                    false,
                ));
            }
            for item in items {
                for (k, l) in wrap(f.small, &format!("• {}", item), inner)
                    .into_iter()
                    .enumerate()
                {
                    lines.push((if k == 0 { l } else { format!("   {}", l) }, false));
                }
            }
        }
        let max_lines = ((h - 260 * u) / (f.small.line_height() + 2 * u)).max(3) as usize;
        let shown = lines.len().min(max_lines);
        let ch = 140 * u + shown as i32 * (f.small.line_height() + 2 * u);
        let card = Rect::new((w - cw) / 2, (h - ch) / 2, cw, ch);
        icons::rounded_rect(c, card, 16 * u, t.window_bg);
        let mut ui = Ui::new(c, t, f, u, card, card.y, hits, self.hover);
        ui.f.title
            .draw(ui.c, card.x + 24 * u, card.y + 22 * u, "Yenilikler", t.text);
        ui.f.small.draw(
            ui.c,
            card.x + 24 * u,
            card.y + 50 * u,
            "Son açılıştan bu yana güncellenenler",
            t.muted,
        );
        let mut y = card.y + 80 * u;
        for (line, heading) in lines.iter().take(shown) {
            let font = if *heading { ui.f.body } else { ui.f.small };
            font.draw_clipped(
                ui.c,
                card.x + 24 * u,
                y,
                line,
                inner,
                if *heading { t.text } else { t.muted },
            );
            y += ui.f.small.line_height() + 2 * u;
        }
        let bw = 120 * u;
        ui.primary_button(
            Rect::new(
                card.right() - 24 * u - bw,
                card.bottom() - 56 * u,
                bw,
                38 * u,
            ),
            "Tamam",
            Control::DismissNews,
        );
    }
}

/// Liste kartlarının platformu: Store kartlarında hangi sayfayı açacağını bilmek için.
pub fn remote_detail(id: &str, platform: Platform) -> Detail {
    Detail::Remote(String::from(id), platform)
}
