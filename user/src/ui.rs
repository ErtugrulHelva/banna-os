//! Uygulama arayüz öğeleri: masaüstü temasıyla kartlar, satırlar, anahtarlar, kaydırıcılar,
//! seçim düğmeleri ve renk örnekleri. Öğeler çizilirken tıklama bölgeleri `hits` listesine
//! kaydedilir; uygulama olay gelince `hit_at` ile hangi denetime basıldığını bulur.

use crate::gfx::text::{self, UiFonts};
use crate::gfx::{Argb, Canvas, Rect, rgb, with_alpha};
use crate::icons;
use crate::theme::Theme;
use crate::{String, Vec};

pub const WHITE: Argb = rgb(0xff, 0xff, 0xff);
pub const DANGER: Argb = rgb(0xe0, 0x4a, 0x42);

/// Arayüz ölçeği: yüksek çözünürlükte (≥1300 satır) her şey iki kat.
pub fn unit() -> i32 {
    if crate::process::sysinfo().screen_height >= 1300 {
        2
    } else {
        1
    }
}

pub fn fonts() -> UiFonts {
    text::ui_fonts(crate::process::sysinfo().screen_height as i32)
}

/// Tıklanabilir bölge; kaydırıcılar iz dikdörtgenini ve aralığını da taşır.
#[derive(Clone)]
pub struct Hit<T> {
    pub rect: Rect,
    pub control: T,
    pub slider: Option<(Rect, i32, i32)>,
}

pub fn hit_at<T>(hits: &[Hit<T>], x: i32, y: i32) -> Option<&Hit<T>> {
    hits.iter().rev().find(|h| h.rect.contains(x, y))
}

/// Basılı denetim: çizilmiş bölge merkezine doğru 0.97'ye küçülür (Emil Kowalski: basılan her
/// şey 0.95-0.98). Boşalan kenar, bölgenin köşe pikseliyle (yuvarlak denetimlerde arka plan)
/// doldurulur; köşeli satırlarda kenar denetimin kendi rengi olur, iz kalmaz.
pub fn press_effect(c: &mut Canvas, rect: Rect) {
    let r = rect.intersect(&c.clip);
    if r.w < 8 || r.h < 8 {
        return;
    }
    let (dx, dy) = (((r.w * 3 + 100) / 200).max(1), ((r.h * 3 + 100) / 200).max(1));
    let src: Vec<u32> = (r.y..r.bottom())
        .flat_map(|y| c.span(y, r.x, r.right()).to_vec())
        .collect();
    let bg = src[0];
    let (iw, ih) = (r.w - 2 * dx, r.h - 2 * dy);
    for row in 0..r.h {
        let line = c.span(r.y + row, r.x, r.right());
        if row < dy || row >= dy + ih {
            line.fill(bg);
            continue;
        }
        let sy = ((row - dy) * r.h / ih) as usize;
        for (col, px) in line.iter_mut().enumerate() {
            let col = col as i32;
            *px = if col < dx || col >= dx + iw {
                bg
            } else {
                src[sy * r.w as usize + ((col - dx) * r.w / iw) as usize]
            };
        }
    }
}

/// Kaydırıcıda `x` konumuna karşılık gelen değer.
pub fn slider_value(track: Rect, min: i32, max: i32, x: i32) -> i32 {
    let t = (x - track.x).clamp(0, track.w.max(1));
    min + (max - min) * t / track.w.max(1)
}

/// Yukarıdan aşağı yerleşen sayfa çizicisi.
pub struct Ui<'a, 'b, T> {
    pub c: &'a mut Canvas<'b>,
    pub t: Theme,
    pub f: &'a UiFonts,
    pub u: i32,
    pub x: i32,
    pub w: i32,
    pub y: i32,
    pub hits: &'a mut Vec<Hit<T>>,
    pub clip: Rect,
    pub hover: Option<Rect>,
    rows_left: usize,
    card_rows: usize,
}

impl<'a, 'b, T: Clone + PartialEq> Ui<'a, 'b, T> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        c: &'a mut Canvas<'b>,
        t: Theme,
        f: &'a UiFonts,
        u: i32,
        area: Rect,
        y: i32,
        hits: &'a mut Vec<Hit<T>>,
        hover: Option<Rect>,
    ) -> Self {
        c.clip = area;
        Self {
            c,
            t,
            f,
            u,
            x: area.x,
            w: area.w,
            y,
            hits,
            clip: area,
            hover,
            rows_left: 0,
            card_rows: 0,
        }
    }

    fn row_h(&self) -> i32 {
        56 * self.u
    }

    pub fn hit(&mut self, rect: Rect, control: T) {
        self.push(rect, control, None);
    }

    fn push(&mut self, rect: Rect, control: T, slider: Option<(Rect, i32, i32)>) {
        let visible = rect.intersect(&self.clip);
        if !visible.is_empty() {
            self.hits.push(Hit {
                rect: visible,
                control,
                slider,
            });
        }
    }

    pub fn hovered(&self, rect: Rect) -> bool {
        self.hover.is_some_and(|h| h == rect.intersect(&self.clip))
    }

    pub fn heading(&mut self, text: &str) {
        self.f
            .heading
            .draw(self.c, self.x, self.y, text, self.t.text);
        self.y += self.f.heading.line_height() + 10 * self.u;
    }

    pub fn section(&mut self, text: &str) {
        self.y += 6 * self.u;
        self.f
            .small
            .draw(self.c, self.x + 4 * self.u, self.y, text, self.t.muted);
        self.y += self.f.small.line_height() + 6 * self.u;
    }

    pub fn note(&mut self, text: &str) {
        self.f.small.draw_clipped(
            self.c,
            self.x + 4 * self.u,
            self.y,
            text,
            self.w,
            self.t.muted,
        );
        self.y += self.f.small.line_height() + 14 * self.u;
    }

    /// `rows` satırlık kart arka planı; ardından gelen satırlar içine yerleşir.
    pub fn card(&mut self, rows: usize) {
        let rect = Rect::new(self.x, self.y, self.w, rows as i32 * self.row_h());
        icons::rounded_rect(self.c, rect, 12 * self.u, self.t.card);
        self.rows_left = rows;
        self.card_rows = rows;
    }

    /// Sonraki satırın dikdörtgeni; kartın son satırından sonra boşluk bırakır.
    pub fn row(&mut self) -> Rect {
        let rect = Rect::new(self.x, self.y, self.w, self.row_h());
        if self.rows_left < self.card_rows {
            self.c.fill(
                Rect::new(rect.x + 16 * self.u, rect.y, rect.w - 32 * self.u, 1),
                with_alpha(if self.t.dark { 0xffffff } else { 0 }, 14),
            );
        }
        self.y += self.row_h();
        self.rows_left = self.rows_left.saturating_sub(1);
        if self.rows_left == 0 {
            self.y += 14 * self.u;
        }
        rect
    }

    pub fn label(&mut self, row: Rect, label: &str, desc: Option<&str>, max: i32) {
        let u = self.u;
        let x = row.x + 18 * u;
        match desc {
            Some(desc) => {
                let total = self.f.body.line_height() + self.f.small.line_height();
                let y = row.y + (row.h - total) / 2;
                self.f
                    .body
                    .draw_clipped(self.c, x, y, label, max, self.t.text);
                self.f.small.draw_clipped(
                    self.c,
                    x,
                    y + self.f.body.line_height(),
                    desc,
                    max,
                    self.t.muted,
                );
            }
            None => {
                let y = row.y + (row.h - self.f.body.ascent - self.f.body.descent) / 2;
                self.f
                    .body
                    .draw_clipped(self.c, x, y, label, max, self.t.text);
            }
        }
    }

    pub fn info(&mut self, label: &str, value: &str) {
        let row = self.row();
        let u = self.u;
        let vw = self.f.body.measure(value).min(row.w * 3 / 5);
        self.label(row, label, None, row.w - vw - 48 * u);
        self.f.body.draw_clipped(
            self.c,
            row.right() - 18 * u - vw,
            row.y + (row.h - self.f.body.ascent - self.f.body.descent) / 2,
            value,
            vw,
            self.t.muted,
        );
    }

    pub fn toggle(&mut self, label: &str, desc: Option<&str>, on: bool, control: T) {
        let row = self.row();
        let u = self.u;
        let track = Rect::new(
            row.right() - 18 * u - 46 * u,
            row.y + row.h / 2 - 13 * u,
            46 * u,
            26 * u,
        );
        self.label(row, label, desc, track.x - row.x - 36 * u);
        let color = if on {
            self.t.accent
        } else if self.hovered(row) {
            self.t.muted
        } else {
            self.t.control
        };
        icons::rounded_rect(self.c, track, 13 * u, color);
        let knob_x = if on {
            track.right() - 13 * u
        } else {
            track.x + 13 * u
        };
        icons::disc(self.c, knob_x, track.y + 13 * u, 10 * u, WHITE);
        self.hit(row, control);
    }

    #[allow(clippy::too_many_arguments)]
    pub fn slider(
        &mut self,
        label: &str,
        desc: Option<&str>,
        value: i32,
        min: i32,
        max: i32,
        text: &str,
        control: T,
    ) {
        let row = self.row();
        let u = self.u;
        let value_w = 64 * u;
        let track_w = (row.w / 2 - value_w).min(260 * u);
        let track = Rect::new(
            row.right() - 18 * u - value_w - track_w,
            row.y + row.h / 2 - 3 * u,
            track_w,
            6 * u,
        );
        self.label(row, label, desc, track.x - row.x - 40 * u);
        icons::rounded_rect(self.c, track, 3 * u, self.t.control);
        let t = ((value - min) * track.w / (max - min).max(1)).clamp(0, track.w);
        if t > 0 {
            icons::rounded_rect(
                self.c,
                Rect::new(track.x, track.y, t.max(6 * u), track.h),
                3 * u,
                self.t.accent,
            );
        }
        let hit = Rect::new(track.x - 12 * u, row.y, track.w + 24 * u, row.h);
        let knob = if self.hovered(hit) { 11 * u } else { 10 * u };
        icons::disc(
            self.c,
            track.x + t,
            track.y + track.h / 2,
            knob + u,
            with_alpha(0, 60),
        );
        icons::disc(self.c, track.x + t, track.y + track.h / 2, knob, WHITE);
        let tw = self.f.body.measure(text);
        self.f.body.draw(
            self.c,
            row.right() - 18 * u - tw,
            row.y + (row.h - self.f.body.ascent - self.f.body.descent) / 2,
            text,
            self.t.muted,
        );
        self.push(hit, control, Some((track, min, max)));
    }

    /// Yan yana seçenekler; `current` seçili olanın denetimi.
    pub fn choice(&mut self, label: &str, desc: Option<&str>, options: &[(&str, T)], current: &T) {
        let row = self.row();
        let u = self.u;
        let pad = 14 * u;
        let widths: Vec<i32> = options
            .iter()
            .map(|(t, _)| self.f.small.measure(t) + 2 * pad)
            .collect();
        let total: i32 = widths.iter().sum::<i32>() + 4 * u;
        let h = 32 * u;
        let group = Rect::new(
            row.right() - 18 * u - total,
            row.y + row.h / 2 - h / 2,
            total,
            h,
        );
        self.label(row, label, desc, group.x - row.x - 36 * u);
        icons::rounded_rect(self.c, group, 9 * u, self.t.control);
        let mut x = group.x + 2 * u;
        for ((text, control), w) in options.iter().zip(widths) {
            let seg = Rect::new(x, group.y + 2 * u, w, h - 4 * u);
            let selected = control == current;
            if selected {
                icons::rounded_rect(self.c, seg, 7 * u, self.t.accent);
            } else if self.hovered(seg) {
                icons::rounded_rect(self.c, seg, 7 * u, self.t.hover);
            }
            self.f.small.draw(
                self.c,
                seg.x + pad,
                seg.y + (seg.h - self.f.small.ascent - self.f.small.descent) / 2,
                text,
                if selected { WHITE } else { self.t.text },
            );
            self.hit(seg, control.clone());
            x += w;
        }
    }

    /// Satır satır sarılan seçim düğmeleri (ör. çözünürlük listesi).
    pub fn chips(&mut self, options: &[(String, T)], current: &T) {
        let u = self.u;
        let h = 34 * u;
        let gap = 10 * u;
        let mut x = self.x;
        for (text, control) in options {
            let w = self.f.small.measure(text) + 28 * u;
            if x + w > self.x + self.w {
                x = self.x;
                self.y += h + gap;
            }
            let chip = Rect::new(x, self.y, w, h);
            let selected = control == current;
            let color = if selected {
                self.t.accent
            } else if self.hovered(chip) {
                self.t.hover
            } else {
                self.t.card
            };
            icons::rounded_rect(self.c, chip, 10 * u, color);
            self.f.small.draw(
                self.c,
                chip.x + 14 * u,
                chip.y + (h - self.f.small.ascent - self.f.small.descent) / 2,
                text,
                if selected { WHITE } else { self.t.text },
            );
            self.hit(chip, control.clone());
            x += w + gap;
        }
        self.y += h + 20 * u;
    }

    /// Renk örnekleri satırı; `current` seçili indeks.
    pub fn swatches(&mut self, label: &str, colors: &[(Argb, T)], current: usize) {
        let row = self.row();
        let u = self.u;
        let r = 12 * u;
        let step = 2 * r + 12 * u;
        let total = colors.len() as i32 * step - 12 * u;
        let start = row.right() - 18 * u - total;
        self.label(row, label, None, start - row.x - 36 * u);
        for (i, (color, control)) in colors.iter().enumerate() {
            let cx = start + i as i32 * step + r;
            let cy = row.y + row.h / 2;
            let area = Rect::new(cx - r - 3 * u, cy - r - 3 * u, 2 * r + 6 * u, 2 * r + 6 * u);
            if i == current {
                icons::circle_outline(self.c, cx, cy, r + 4 * u, 2 * u, *color);
            } else if self.hovered(area) {
                icons::circle_outline(self.c, cx, cy, r + 4 * u, 2 * u, self.t.control);
            }
            icons::disc(self.c, cx, cy, r, *color);
            if i == current {
                icons::check(
                    self.c,
                    Rect::new(cx - r / 2 - u, cy - r / 2 - u, r + 2 * u, r + 2 * u),
                    WHITE,
                );
            }
            self.hit(area, control.clone());
        }
    }

    /// Sağa yaslı düğmeler; `danger` kırmızı (kapat, sıfırla...).
    pub fn buttons(&mut self, label: &str, desc: Option<&str>, buttons: &[(&str, T, bool)]) {
        let row = self.row();
        let u = self.u;
        let h = 32 * u;
        let mut x = row.right() - 18 * u;
        let mut rects = Vec::new();
        for (text, control, danger) in buttons.iter().rev() {
            let w = (self.f.small.measure(text) + 32 * u).max(44 * u);
            let b = Rect::new(x - w, row.y + row.h / 2 - h / 2, w, h);
            rects.push((b, *text, control.clone(), *danger));
            x = b.x - 10 * u;
        }
        self.label(row, label, desc, x - row.x - 26 * u);
        for (b, text, control, danger) in rects {
            let hovered = self.hovered(b);
            let color = match (danger, hovered) {
                (true, true) => DANGER,
                (true, false) => with_alpha(DANGER, 40),
                (false, true) => self.t.hover,
                (false, false) => self.t.control,
            };
            icons::rounded_rect(self.c, b, 9 * u, color);
            let tw = self.f.small.measure(text);
            let fg = if danger && hovered {
                WHITE
            } else if danger {
                rgb(0xff, 0x7b, 0x72)
            } else {
                self.t.text
            };
            self.f.small.draw(
                self.c,
                b.x + b.w / 2 - tw / 2,
                b.y + (h - self.f.small.ascent - self.f.small.descent) / 2,
                text,
                fg,
            );
            self.hit(b, control);
        }
    }

    /// Tek satırlık metin alanı (`secret` ise noktalarla); tıklanınca `control`.
    #[allow(clippy::too_many_arguments)]
    pub fn text_field(
        &mut self,
        rect: Rect,
        placeholder: &str,
        value: &str,
        secret: bool,
        focused: bool,
        control: T,
    ) {
        let u = self.u;
        let border = if focused {
            self.t.accent
        } else {
            self.t.control
        };
        icons::rounded_rect(self.c, rect, 10 * u, border);
        icons::rounded_rect(
            self.c,
            rect.inset(if focused { 2 * u } else { u }),
            9 * u,
            self.t.card,
        );
        let shown: String = if secret {
            value.chars().map(|_| '•').collect()
        } else {
            String::from(value)
        };
        let ty = rect.y + (rect.h - self.f.body.ascent - self.f.body.descent) / 2;
        let x = rect.x + 14 * u;
        if shown.is_empty() && !focused {
            self.f.body.draw(self.c, x, ty, placeholder, self.t.muted);
        } else {
            let end = self
                .f
                .body
                .draw_clipped(self.c, x, ty, &shown, rect.w - 28 * u, self.t.text);
            if focused {
                self.c.fill(
                    Rect::new(end + u, ty, 2 * u, self.f.body.ascent + self.f.body.descent),
                    self.t.accent,
                );
            }
        }
        self.hit(rect, control);
    }

    /// İmleçli ve seçimli tek satırlık metin alanı (`TextInput`).
    #[allow(clippy::too_many_arguments)]
    pub fn input_field(
        &mut self,
        rect: Rect,
        placeholder: &str,
        input: &TextInput,
        secret: bool,
        focused: bool,
        control: T,
    ) {
        let u = self.u;
        let border = if focused {
            self.t.accent
        } else {
            self.t.control
        };
        icons::rounded_rect(self.c, rect, 10 * u, border);
        icons::rounded_rect(
            self.c,
            rect.inset(if focused { 2 * u } else { u }),
            9 * u,
            self.t.card,
        );
        let shown: Vec<char> = if secret {
            input.text.chars().map(|_| '•').collect()
        } else {
            input.text.chars().collect()
        };
        let ty = rect.y + (rect.h - self.f.body.ascent - self.f.body.descent) / 2;
        let x = rect.x + 14 * u;
        let avail = rect.w - 28 * u;
        if shown.is_empty() && !focused {
            self.f.body.draw(self.c, x, ty, placeholder, self.t.muted);
            self.hit(rect, control);
            return;
        }
        let font = self.f.body;
        let width = |from: usize, to: usize| -> i32 {
            let piece: String = shown[from..to].iter().collect();
            font.measure(&piece)
        };
        // İmleç görünür kalacak şekilde metin sola kaydırılır.
        let cursor = if focused { input.cursor() } else { 0 };
        let mut start = 0;
        while start < cursor && width(start, cursor) > avail {
            start += 1;
        }
        let saved = self.c.clip;
        self.c.clip = saved.intersect(&Rect::new(x, rect.y, avail + 2 * u, rect.h));
        let height = font.ascent + font.descent;
        if focused && let Some((a, b)) = input.selection() {
            let (a, b) = (a.max(start), b.max(start));
            let left = x + width(start, a);
            let right = x + width(start, b);
            self.c.fill(
                Rect::new(left, ty, right - left, height),
                with_alpha(self.t.accent & 0x00ff_ffff, 90),
            );
        }
        let text: String = shown[start..].iter().collect();
        font.draw(self.c, x, ty, &text, self.t.text);
        if focused {
            let at = x + width(start, cursor);
            self.c.fill(Rect::new(at, ty, 2 * u, height), self.t.accent);
        }
        self.c.clip = saved;
        self.hit(rect, control);
    }

    /// Vurgulu birincil düğme.
    pub fn primary_button(&mut self, rect: Rect, text: &str, control: T) {
        let u = self.u;
        let color = if self.hovered(rect) {
            crate::gfx::blend(self.t.accent, with_alpha(0xffffff, 40)) | 0xff00_0000
        } else {
            self.t.accent
        };
        icons::rounded_rect(self.c, rect, 10 * u, color);
        let tw = self.f.title.measure(text);
        self.f.title.draw(
            self.c,
            rect.x + rect.w / 2 - tw / 2,
            rect.y + (rect.h - self.f.title.ascent - self.f.title.descent) / 2,
            text,
            WHITE,
        );
        self.hit(rect, control);
    }

    /// İkincil (nötr) düğme.
    pub fn secondary_button(&mut self, rect: Rect, text: &str, control: T) {
        let u = self.u;
        let color = if self.hovered(rect) {
            self.t.hover
        } else {
            self.t.control
        };
        icons::rounded_rect(self.c, rect, 10 * u, color);
        let tw = self.f.body.measure(text);
        self.f.body.draw(
            self.c,
            rect.x + rect.w / 2 - tw / 2,
            rect.y + (rect.h - self.f.body.ascent - self.f.body.descent) / 2,
            text,
            self.t.text,
        );
        self.hit(rect, control);
    }
}

/// Tek satırlık düzenlenebilir metin: imleç (karakter sırası), Shift ile seçim, kelime
/// atlama ve pano. `String` gibi kullanılır (Deref); dışarıdan değişirse imleç sona sıkışır.
#[derive(Clone, Default, Debug)]
pub struct TextInput {
    pub text: String,
    cursor: Option<usize>,
    anchor: Option<usize>,
}

impl PartialEq for TextInput {
    fn eq(&self, other: &Self) -> bool {
        self.text == other.text
    }
}

impl core::ops::Deref for TextInput {
    type Target = String;
    fn deref(&self) -> &String {
        &self.text
    }
}

impl core::ops::DerefMut for TextInput {
    fn deref_mut(&mut self) -> &mut String {
        &mut self.text
    }
}

impl From<String> for TextInput {
    fn from(text: String) -> Self {
        Self {
            text,
            cursor: None,
            anchor: None,
        }
    }
}

impl From<&str> for TextInput {
    fn from(text: &str) -> Self {
        Self::from(String::from(text))
    }
}

impl core::fmt::Display for TextInput {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(&self.text)
    }
}

impl TextInput {
    fn len(&self) -> usize {
        self.text.chars().count()
    }

    /// İmlecin karakter sırası (metnin sonunu aşmaz).
    pub fn cursor(&self) -> usize {
        self.cursor.map_or(self.len(), |c| c.min(self.len()))
    }

    /// Seçili aralık (başlangıç, bitiş), karakter sırasıyla.
    pub fn selection(&self) -> Option<(usize, usize)> {
        let a = self.anchor?.min(self.len());
        let c = self.cursor();
        (a != c).then(|| (a.min(c), a.max(c)))
    }

    fn byte(&self, index: usize) -> usize {
        self.text
            .char_indices()
            .nth(index)
            .map_or(self.text.len(), |(i, _)| i)
    }

    pub fn set(&mut self, text: &str) {
        self.text = String::from(text);
        self.cursor = None;
        self.anchor = None;
    }

    pub fn clear_text(&mut self) {
        self.set("");
    }

    pub fn select_all(&mut self) {
        self.anchor = Some(0);
        self.cursor = Some(self.len());
    }

    pub fn into_string(self) -> String {
        self.text
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else {
            return false;
        };
        let (start, end) = (self.byte(a), self.byte(b));
        self.text.replace_range(start..end, "");
        self.cursor = Some(a);
        self.anchor = None;
        true
    }

    fn insert(&mut self, piece: &str, max: usize) -> bool {
        self.delete_selection();
        let room = max.saturating_sub(self.len());
        let piece: String = piece
            .chars()
            .filter(|c| !c.is_control())
            .take(room)
            .collect();
        if piece.is_empty() {
            return false;
        }
        let at = self.cursor();
        let byte = self.byte(at);
        self.text.insert_str(byte, &piece);
        self.cursor = Some(at + piece.chars().count());
        true
    }

    fn word_left(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from;
        while i > 0 && !chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        while i > 0 && chars[i - 1].is_alphanumeric() {
            i -= 1;
        }
        i
    }

    fn word_right(&self, from: usize) -> usize {
        let chars: Vec<char> = self.text.chars().collect();
        let mut i = from;
        while i < chars.len() && !chars[i].is_alphanumeric() {
            i += 1;
        }
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
        i
    }

    /// Klavye olayını uygular; metin ya da imleç değiştiyse `true`.
    pub fn key(&mut self, key: u32, max: usize) -> bool {
        use crate::sys::{
            KEY_CTRL, KEY_DELETE, KEY_END, KEY_HOME, KEY_LEFT, KEY_PASTE, KEY_RIGHT, KEY_SHIFT,
        };
        let shift = key & KEY_SHIFT != 0;
        let ctrl = key & KEY_CTRL != 0;
        let base = key & !(KEY_SHIFT | KEY_CTRL);
        let moved = match base {
            KEY_LEFT if !shift && self.selection().is_some() => self.selection().map(|s| s.0),
            KEY_RIGHT if !shift && self.selection().is_some() => self.selection().map(|s| s.1),
            KEY_LEFT if ctrl => Some(self.word_left(self.cursor())),
            KEY_RIGHT if ctrl => Some(self.word_right(self.cursor())),
            KEY_LEFT => Some(self.cursor().saturating_sub(1)),
            KEY_RIGHT => Some((self.cursor() + 1).min(self.len())),
            KEY_HOME => Some(0),
            KEY_END => Some(self.len()),
            _ => None,
        };
        if let Some(to) = moved {
            if shift {
                if self.anchor.is_none() {
                    self.anchor = Some(self.cursor());
                }
            } else {
                self.anchor = None;
            }
            self.cursor = Some(to);
            return true;
        }
        match key {
            8 => {
                if self.delete_selection() {
                    return true;
                }
                let at = self.cursor();
                if at == 0 {
                    return false;
                }
                let (start, end) = (self.byte(at - 1), self.byte(at));
                self.text.replace_range(start..end, "");
                self.cursor = Some(at - 1);
                true
            }
            KEY_DELETE => {
                if self.delete_selection() {
                    return true;
                }
                let at = self.cursor();
                if at >= self.len() {
                    return false;
                }
                let (start, end) = (self.byte(at), self.byte(at + 1));
                self.text.replace_range(start..end, "");
                self.cursor = Some(at);
                true
            }
            // Ctrl+A tümünü seç, Ctrl+C kopyala, Ctrl+X kes, Ctrl+V yapıştır.
            1 => {
                self.anchor = Some(0);
                self.cursor = Some(self.len());
                true
            }
            3 | 24 => {
                let Some((a, b)) = self.selection() else {
                    return false;
                };
                let piece = String::from(&self.text[self.byte(a)..self.byte(b)]);
                crate::gui::clipboard::set(&piece);
                key == 24 && self.delete_selection()
            }
            22 | KEY_PASTE => {
                let pasted = crate::gui::clipboard::get();
                self.insert(pasted.trim_end_matches(['\r', '\n']), max)
            }
            k if (0x20..0xf700).contains(&k) => match char::from_u32(k) {
                Some(c) if !c.is_control() => {
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b), max)
                }
                _ => false,
            },
            _ => false,
        }
    }
}

/// Klavye olayını metin alanına uygular: harf ekler, Backspace siler. Değiştiyse `true`.
pub fn edit_text(value: &mut String, key: u32, max: usize) -> bool {
    match key {
        8 => value.pop().is_some(),
        k if (0x20..0xf700).contains(&k) && value.chars().count() < max => {
            match char::from_u32(k) {
                Some(c) if !c.is_control() => {
                    value.push(c);
                    true
                }
                _ => false,
            }
        }
        _ => false,
    }
}

/// Uygulama içi sağ tık menüsü: `EVENT_CONTEXT` gelince açılır; sol tık seçer ya da kapatır.
pub struct ContextMenu<T> {
    pub rect: Rect,
    items: Vec<(String, Option<T>)>,
    pub hover: Option<usize>,
}

impl<T: Clone> ContextMenu<T> {
    fn row_height(f: &UiFonts) -> i32 {
        f.body.line_height() + 12 * unit()
    }

    fn separator_height() -> i32 {
        9 * unit()
    }

    /// `items`: (etiket, eylem); eylemi `None` olan öğe ayırıcı çizgidir.
    pub fn new(x: i32, y: i32, items: Vec<(&str, Option<T>)>, bounds: (i32, i32)) -> Self {
        let u = unit();
        let f = fonts();
        let width = items
            .iter()
            .map(|(l, _)| f.body.measure(l))
            .max()
            .unwrap_or(0)
            .max(120 * u)
            + 40 * u;
        let height = items
            .iter()
            .map(|(_, a)| {
                if a.is_some() {
                    Self::row_height(&f)
                } else {
                    Self::separator_height()
                }
            })
            .sum::<i32>()
            + 12 * u;
        let x = x.min(bounds.0 - width - 2 * u).max(2 * u);
        let y = if y + height > bounds.1 - 2 * u {
            (y - height).max(2 * u)
        } else {
            y
        };
        Self {
            rect: Rect::new(x, y, width, height),
            items: items
                .into_iter()
                .map(|(l, a)| (String::from(l), a))
                .collect(),
            hover: None,
        }
    }

    fn rows(&self) -> Vec<(Rect, usize)> {
        let u = unit();
        let f = fonts();
        let mut y = self.rect.y + 6 * u;
        let mut out = Vec::new();
        for (i, (_, action)) in self.items.iter().enumerate() {
            let h = if action.is_some() {
                Self::row_height(&f)
            } else {
                Self::separator_height()
            };
            out.push((
                Rect::new(self.rect.x + 6 * u, y, self.rect.w - 12 * u, h),
                i,
            ));
            y += h;
        }
        out
    }

    fn index_at(&self, x: i32, y: i32) -> Option<usize> {
        self.rows()
            .into_iter()
            .find(|(r, i)| r.contains(x, y) && self.items[*i].1.is_some())
            .map(|(_, i)| i)
    }

    /// Tıklanan öğenin eylemi (dışarıya tıklandıysa `None`).
    pub fn pick(&self, x: i32, y: i32) -> Option<T> {
        self.index_at(x, y).and_then(|i| self.items[i].1.clone())
    }

    /// Üzerine gelinen öğeyi günceller; değiştiyse `true`.
    pub fn hover_at(&mut self, x: i32, y: i32) -> bool {
        let hover = self.index_at(x, y);
        let changed = hover != self.hover;
        self.hover = hover;
        changed
    }

    pub fn draw(&self, c: &mut Canvas, t: &Theme) {
        let u = unit();
        let f = fonts();
        icons::rounded_rect(c, self.rect.inset(-u), 11 * u, with_alpha(0, 90));
        icons::rounded_rect(c, self.rect, 10 * u, t.card | 0xff00_0000);
        for (row, i) in self.rows() {
            let (label, action) = &self.items[i];
            if action.is_none() {
                c.fill(
                    Rect::new(row.x + 8 * u, row.y + row.h / 2, row.w - 16 * u, u.max(1)),
                    t.control,
                );
                continue;
            }
            let hovered = self.hover == Some(i);
            if hovered {
                icons::rounded_rect(c, row, 7 * u, t.accent);
            }
            f.body.draw(
                c,
                row.x + 12 * u,
                row.y + (row.h - f.body.ascent - f.body.descent) / 2,
                label,
                if hovered { WHITE } else { t.text },
            );
        }
    }
}
