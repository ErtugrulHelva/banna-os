//! Giriş ekranı: saat, avatar, kullanıcı adı/parola alanları; ilk açılışta yönetici
//! hesabı oluşturma formu. Parola özeti (PBKDF2) arayüz donmasın diye ayrı thread'de
//! hesaplanır.

use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;

use super::m;
use crate::drivers::keyboard::{Key, KeyEvent};
use crate::drivers::rtc;
use crate::gfx::text::{self, Font};
use crate::gfx::{Argb, Canvas, Rect, Surface, rgb, with_alpha};
use crate::kernel::users::{self, User};
use crate::sync::SpinLock;

const ACCENT: Argb = rgb(0x5f, 0xb4, 0xff);
const TEXT: Argb = rgb(0xf2, 0xf4, 0xf8);
const MUTED: Argb = rgb(0x9a, 0xa3, 0xb5);
const ERROR: Argb = rgb(0xff, 0x7b, 0x72);
const MAX_INPUT: usize = 64;

const MONTHS: [&str; 12] = [
    "Ocak", "Şubat", "Mart", "Nisan", "Mayıs", "Haziran", "Temmuz", "Ağustos", "Eylül", "Ekim",
    "Kasım", "Aralık",
];
const DAYS: [&str; 7] = [
    "Pazar",
    "Pazartesi",
    "Salı",
    "Çarşamba",
    "Perşembe",
    "Cuma",
    "Cumartesi",
];

struct Field {
    label: &'static str,
    value: String,
    secret: bool,
    /// İmleç (karakter sırası); `usize::MAX` = metnin sonu.
    cursor: usize,
}

impl Field {
    fn len(&self) -> usize {
        self.value.chars().count()
    }

    fn at(&self) -> usize {
        self.cursor.min(self.len())
    }

    fn byte(&self, index: usize) -> usize {
        self.value
            .char_indices()
            .nth(index)
            .map_or(self.value.len(), |(i, _)| i)
    }
}

enum Outcome {
    Success(User),
    Failure(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Shutdown,
    Reboot,
}

pub struct Login {
    pub setup: bool,
    fields: Vec<Field>,
    focus: usize,
    message: Option<(String, bool)>,
    pending: Option<Arc<SpinLock<Option<Outcome>>>>,
}

struct Layout {
    card: Rect,
    avatar: Rect,
    fields: Vec<Rect>,
    button: Rect,
    message_y: i32,
    actions: [(Rect, Action); 2],
}

impl Login {
    pub fn new() -> Self {
        let setup = users::needs_setup();
        let fields = if setup {
            alloc::vec![
                Field {
                    label: "Kullanıcı adı",
                    value: String::new(),
                    secret: false,
                    cursor: usize::MAX,
                },
                Field {
                    label: "Parola",
                    value: String::new(),
                    secret: true,
                    cursor: usize::MAX,
                },
                Field {
                    label: "Parola (tekrar)",
                    value: String::new(),
                    secret: true,
                    cursor: usize::MAX,
                },
            ]
        } else {
            let accounts = users::list();
            let single = (accounts.len() == 1).then(|| accounts[0].0.clone());
            alloc::vec![
                Field {
                    label: "Kullanıcı adı",
                    value: single.clone().unwrap_or_default(),
                    secret: false,
                    cursor: usize::MAX,
                },
                Field {
                    label: "Parola",
                    value: String::new(),
                    secret: true,
                    cursor: usize::MAX,
                },
            ]
        };
        let focus = if !setup && !fields[0].value.is_empty() {
            1
        } else {
            0
        };
        crate::log!(
            "login: {}",
            if setup { "setup required" } else { "waiting" }
        );
        Self {
            setup,
            fields,
            focus,
            message: None,
            pending: None,
        }
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Giriş kartının ve düğmelerin kapladığı alan (gölge dahil): değişince yalnızca burası
    /// yeniden çizilir.
    /// Klavye ve tıklamalar yalnızca kartı değiştirir (köşedeki düğmeler çizilmez; tüm
    /// karttan ekranın köşesine uzanan birleşim her tuşta ~2 milyon piksel ederdi).
    pub fn card_area(&self, width: i32, height: i32) -> Rect {
        self.layout(width, height).card.inset(-30 * m().unit)
    }

    pub fn dirty_area(&self, width: i32, height: i32) -> Rect {
        let l = self.layout(width, height);
        let mut area = l.card.inset(-30 * m().unit);
        for (rect, _) in l.actions {
            area = area.union(&rect.inset(-4 * m().unit));
        }
        area
    }

    fn layout(&self, width: i32, height: i32) -> Layout {
        let u = m().unit;
        let card_w = 400 * u;
        let field_h = 42 * u;
        let gap = 12 * u;
        let avatar = 88 * u;
        let title_h = 40 * u;
        let body_h = avatar
            + 18 * u
            + title_h
            + 16 * u
            + self.fields.len() as i32 * (field_h + gap)
            + 8 * u
            + field_h
            + 40 * u;
        let card = Rect::new(
            width / 2 - card_w / 2,
            (height / 2 - body_h / 2 + 60 * u).max(m().panel_h),
            card_w,
            body_h + 2 * 28 * u,
        );
        let pad = 28 * u;
        let avatar_rect = Rect::new(width / 2 - avatar / 2, card.y + pad, avatar, avatar);
        let mut y = avatar_rect.bottom() + 18 * u + title_h + 16 * u;
        let fields = self
            .fields
            .iter()
            .map(|_| {
                let r = Rect::new(card.x + pad, y, card_w - 2 * pad, field_h);
                y += field_h + gap;
                r
            })
            .collect();
        let button = Rect::new(card.x + pad, y + 8 * u, card_w - 2 * pad, field_h);
        let action_w = 140 * u;
        let action_h = 36 * u;
        let ay = height - action_h - 24 * u;
        Layout {
            card,
            avatar: avatar_rect,
            fields,
            button,
            message_y: button.bottom() + 12 * u,
            actions: [
                (
                    Rect::new(width - 2 * (action_w + 12 * u), ay, action_w, action_h),
                    Action::Reboot,
                ),
                (
                    Rect::new(width - action_w - 12 * u, ay, action_w, action_h),
                    Action::Shutdown,
                ),
            ],
        }
    }

    /// Fare tıklaması; ekranın yeniden çizilmesi gerekiyorsa `true`.
    pub fn click(&mut self, x: i32, y: i32, width: i32, height: i32) -> bool {
        let layout = self.layout(width, height);
        if let Some(i) = layout.fields.iter().position(|r| r.contains(x, y)) {
            self.focus = i;
            return true;
        }
        if layout.button.contains(x, y) {
            self.submit();
            return true;
        }
        for (rect, action) in layout.actions {
            if rect.contains(x, y) && !self.busy() {
                match action {
                    Action::Shutdown => crate::drivers::power::shutdown(),
                    Action::Reboot => crate::drivers::power::reboot(),
                }
            }
        }
        false
    }

    pub fn key(&mut self, event: KeyEvent) -> bool {
        if self.busy() {
            return false;
        }
        let last = self.fields.len() - 1;
        match event.key {
            Key::Tab | Key::Down => self.focus = (self.focus + 1) % self.fields.len(),
            Key::Up => self.focus = (self.focus + last) % self.fields.len(),
            Key::Enter if self.focus < last && self.fields[self.focus].value.is_empty() => {}
            Key::Enter if self.focus < last => self.focus += 1,
            Key::Enter => self.submit(),
            Key::Backspace => {
                let field = &mut self.fields[self.focus];
                let at = field.at();
                if at > 0 {
                    let (start, end) = (field.byte(at - 1), field.byte(at));
                    field.value.replace_range(start..end, "");
                    field.cursor = at - 1;
                }
            }
            Key::Delete => {
                let field = &mut self.fields[self.focus];
                let at = field.at();
                if at < field.len() {
                    let (start, end) = (field.byte(at), field.byte(at + 1));
                    field.value.replace_range(start..end, "");
                    field.cursor = at;
                }
            }
            Key::Left => {
                let field = &mut self.fields[self.focus];
                field.cursor = field.at().saturating_sub(1);
            }
            Key::Right => {
                let field = &mut self.fields[self.focus];
                field.cursor = (field.at() + 1).min(field.len());
            }
            Key::Home => self.fields[self.focus].cursor = 0,
            Key::End => self.fields[self.focus].cursor = usize::MAX,
            Key::Escape => {
                self.fields[self.focus].value.clear();
                self.fields[self.focus].cursor = usize::MAX;
                self.message = None;
            }
            Key::Char(c) if !c.is_control() && !event.modifiers.ctrl => {
                let field = &mut self.fields[self.focus];
                if field.len() < MAX_INPUT {
                    let at = field.at();
                    let byte = field.byte(at);
                    field.value.insert(byte, c);
                    field.cursor = at + 1;
                }
            }
            _ => return false,
        }
        true
    }

    fn submit(&mut self) {
        if self.busy() {
            return;
        }
        let name = String::from(self.fields[0].value.trim());
        let password = self.fields[1].value.clone();
        if name.is_empty() {
            self.fail("Kullanıcı adını yazın.", 0);
            return;
        }
        if self.setup {
            if !users::valid_name(&name) {
                self.fail(
                    "Kullanıcı adı küçük harfle başlamalı; a-z, 0-9, _ ve - kullanılabilir.",
                    0,
                );
                return;
            }
            if password.chars().count() < users::MIN_PASSWORD {
                self.fail("Parola en az 4 karakter olmalı.", 1);
                return;
            }
            if password != self.fields[2].value {
                self.fields[2].value.clear();
                self.fail("Parolalar eşleşmiyor.", 2);
                return;
            }
        }
        let slot = Arc::new(SpinLock::new(None));
        let result = slot.clone();
        let setup = self.setup;
        self.message = Some((
            String::from(if setup {
                "Hesap oluşturuluyor…"
            } else {
                "Giriş yapılıyor…"
            }),
            false,
        ));
        let started = crate::sched::spawn_detached("login", move || {
            let outcome = if setup {
                match users::create(&name, &password, true) {
                    Ok(user) => Outcome::Success(user),
                    Err(e) => Outcome::Failure(String::from(e)),
                }
            } else {
                match users::authenticate(&name, &password) {
                    Some(user) => Outcome::Success(user),
                    None => {
                        crate::log!("login: failed attempt for '{}'", name);
                        crate::sched::sleep_ms(600);
                        Outcome::Failure(String::from("Kullanıcı adı ya da parola hatalı."))
                    }
                }
            };
            *result.lock() = Some(outcome);
            super::wake();
            0
        });
        if started.is_some() {
            self.pending = Some(slot);
        } else {
            self.fail("Giriş başlatılamadı.", 1);
        }
    }

    fn fail(&mut self, message: &str, focus: usize) {
        self.message = Some((String::from(message), true));
        self.focus = focus.min(self.fields.len() - 1);
    }

    /// Arka plan işi bittiyse sonucu uygular; oturum açıldıysa `Some(user)`.
    pub fn poll(&mut self) -> (bool, Option<User>) {
        let Some(slot) = self.pending.as_ref() else {
            return (false, None);
        };
        let Some(outcome) = slot.lock().take() else {
            return (false, None);
        };
        self.pending = None;
        match outcome {
            Outcome::Success(user) => (true, Some(user)),
            Outcome::Failure(message) => {
                for f in self.fields.iter_mut().filter(|f| f.secret) {
                    f.value.clear();
                }
                self.fail(&message, 1);
                (true, None)
            }
        }
    }

    pub fn draw(&self, canvas: &mut Canvas, wallpaper: &Surface, width: i32, height: i32) {
        let u = m().unit;
        let f = text::ui_fonts(height);
        let clip = canvas.clip;
        canvas.blit(wallpaper, clip, clip.x, clip.y);
        canvas.fill(
            Rect::new(0, 0, width, height),
            with_alpha(rgb(0x05, 0x07, 0x10), 120),
        );

        if let Some(now) = rtc::now() {
            let l = now.to_local();
            let time = format!("{:02}:{:02}", l.hour, l.minute);
            let days = l.unix_timestamp().div_euclid(86_400);
            let weekday = DAYS[((days + 4).rem_euclid(7)) as usize];
            let date = format!(
                "{} {} {}, {}",
                l.day,
                MONTHS[(l.month as usize).clamp(1, 12) - 1],
                l.year,
                weekday
            );
            let top = height / 10;
            let tw = f.clock.measure(&time);
            f.clock.draw(canvas, width / 2 - tw / 2, top, &time, TEXT);
            let dw = f.body.measure(&date);
            f.body.draw(
                canvas,
                width / 2 - dw / 2,
                top + f.clock.ascent + f.clock.descent + 6 * u,
                &date,
                MUTED,
            );
        }

        let layout = self.layout(width, height);
        canvas.shadow(layout.card, 24 * u, 24 * u, 110);
        canvas.fill_rounded(layout.card, 20 * u, with_alpha(rgb(0x14, 0x19, 0x26), 225));

        let initial = self.fields[0]
            .value
            .chars()
            .next()
            .map(|c| c.to_uppercase().next().unwrap_or(c))
            .unwrap_or('B');
        canvas.fill_rounded(layout.avatar, layout.avatar.w / 2, rgb(0x2a, 0x5c, 0xb8));
        let mut buf = [0u8; 4];
        let letter = initial.encode_utf8(&mut buf);
        let lw = f.heading.measure(letter);
        f.heading.draw(
            canvas,
            layout.avatar.x + layout.avatar.w / 2 - lw / 2,
            layout.avatar.y + (layout.avatar.h - f.heading.ascent - f.heading.descent) / 2,
            letter,
            TEXT,
        );

        let title = if self.setup {
            "BannaOS'a hoş geldin"
        } else {
            "Tekrar hoş geldin"
        };
        let subtitle = if self.setup {
            "Başlamak için bir yönetici hesabı oluştur"
        } else {
            "Devam etmek için giriş yap"
        };
        let ty = layout.avatar.bottom() + 14 * u;
        let tw = f.heading.measure(title);
        f.heading.draw(canvas, width / 2 - tw / 2, ty, title, TEXT);
        let sw = f.small.measure(subtitle);
        f.small.draw(
            canvas,
            width / 2 - sw / 2,
            ty + f.heading.ascent + f.heading.descent + 4 * u,
            subtitle,
            MUTED,
        );

        for (i, (field, rect)) in self.fields.iter().zip(layout.fields.iter()).enumerate() {
            let focused = i == self.focus && !self.busy();
            draw_field(canvas, field, *rect, focused, f.body);
        }

        let label = match (self.busy(), self.setup) {
            (true, _) => "Lütfen bekleyin…",
            (false, true) => "Hesabı oluştur",
            (false, false) => "Giriş yap",
        };
        let button_color = if self.busy() {
            with_alpha(ACCENT, 120)
        } else {
            ACCENT
        };
        canvas.fill_rounded(layout.button, 10 * u, button_color);
        let bw = f.title.measure(label);
        f.title.draw(
            canvas,
            layout.button.x + layout.button.w / 2 - bw / 2,
            layout.button.y + (layout.button.h - f.title.ascent - f.title.descent) / 2,
            label,
            rgb(0x06, 0x12, 0x24),
        );

        if let Some((message, error)) = &self.message {
            let mw = f.small.measure(message);
            f.small.draw_clipped(
                canvas,
                (width / 2 - mw / 2).max(layout.card.x + 12 * u),
                layout.message_y,
                message,
                layout.card.w - 24 * u,
                if *error { ERROR } else { MUTED },
            );
        }

        for (rect, action) in layout.actions {
            canvas.fill_rounded(rect, 10 * u, with_alpha(0xffffff, 22));
            let label = match action {
                Action::Shutdown => "Kapat",
                Action::Reboot => "Yeniden başlat",
            };
            let lw = f.small.measure(label);
            f.small.draw(
                canvas,
                rect.x + rect.w / 2 - lw / 2,
                rect.y + (rect.h - f.small.ascent - f.small.descent) / 2,
                label,
                TEXT,
            );
        }
        let hint = format!(
            "Banna OS   •   klavye: {}",
            match crate::drivers::keyboard::layout() {
                crate::drivers::keyboard::Layout::TurkishQ => "Türkçe Q",
                crate::drivers::keyboard::Layout::Us => "ABD",
            }
        );
        f.small.draw(
            canvas,
            24 * u,
            height - 24 * u - f.small.ascent - f.small.descent - 8 * u,
            &hint,
            MUTED,
        );
    }
}

fn draw_field(canvas: &mut Canvas, field: &Field, rect: Rect, focused: bool, font: &Font) {
    let u = m().unit;
    if focused {
        canvas.fill_rounded(rect.inset(-2 * u), 12 * u, ACCENT);
    }
    canvas.fill_rounded(rect, 10 * u, rgb(0x1e, 0x25, 0x35));
    let pad = 14 * u;
    let ty = rect.y + (rect.h - font.ascent - font.descent) / 2;
    if field.value.is_empty() {
        font.draw(canvas, rect.x + pad, ty, field.label, MUTED);
        if focused {
            canvas.fill(
                Rect::new(rect.x + pad, ty, 2 * u, font.ascent + font.descent),
                TEXT,
            );
        }
        return;
    }
    let shown: String = if field.secret {
        field.value.chars().map(|_| '•').collect()
    } else {
        field.value.clone()
    };
    let chars: Vec<char> = shown.chars().collect();
    let width = |from: usize, to: usize| font.measure(&chars[from..to].iter().collect::<String>());
    let avail = rect.w - 2 * pad - 4 * u;
    let cursor = if focused { field.at() } else { 0 };
    let mut start = 0;
    while start < cursor && width(start, cursor) > avail {
        start += 1;
    }
    let visible: String = chars[start..].iter().collect();
    font.draw_clipped(canvas, rect.x + pad, ty, &visible, avail, TEXT);
    if focused {
        let x = rect.x + pad + width(start, cursor);
        canvas.fill(
            Rect::new(x + u, ty, 2 * u, font.ascent + font.descent),
            TEXT,
        );
    }
}
