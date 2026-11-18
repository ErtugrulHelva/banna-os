//! Çizim listesini tuvale aktarır: kaydırma, ölçek (CSS → cihaz pikseli), kırpma,
//! yuvarlak köşeli arka plan ve kenarlıklar, metin, görseller ve form denetimleri.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::css::Color;
use super::dom::{Document, NodeId};
use super::layout::{Fonts, Item, RectF};
use super::style::Style;
use crate::gfx::png::Image;
use crate::gfx::{Argb, Canvas, Rect};

pub struct Images {
    pub map: BTreeMap<String, Image>,
    /// Ölçeklenmiş kopyalar: (adres, genişlik, yükseklik) → pikseller.
    pub scaled: BTreeMap<(String, i32, i32), Vec<u32>>,
}

impl Default for Images {
    fn default() -> Self {
        Self::new()
    }
}

impl Images {
    pub fn new() -> Self {
        Self {
            map: BTreeMap::new(),
            scaled: BTreeMap::new(),
        }
    }
}

impl super::layout::Images for Images {
    fn size(&self, src: &str) -> Option<(f32, f32)> {
        self.map.get(src).map(|i| (i.width as f32, i.height as f32))
    }
}

pub struct Painter<'a> {
    pub canvas: Canvas<'a>,
    pub fonts: &'a dyn Fonts,
    pub scale: f32,
    /// Sayfanın çizildiği alan (cihaz pikseli) ve dikey kaydırma (CSS pikseli).
    pub origin: (i32, i32),
    pub scroll: f32,
    pub viewport: Rect,
    pub caret: bool,
}

fn argb(c: Color, opacity: f32) -> Argb {
    if opacity >= 0.999 {
        return c.0;
    }
    let a = ((c.0 >> 24) as f32 * opacity) as u32;
    (a << 24) | (c.0 & 0x00ff_ffff)
}

impl Painter<'_> {
    fn px(&self, r: RectF) -> Rect {
        let s = self.scale;
        let x = self.origin.0 + (r.x * s) as i32;
        let y = self.origin.1 + ((r.y - self.scroll) * s) as i32;
        let x2 = self.origin.0 + ((r.x + r.w) * s + 0.5) as i32;
        let y2 = self.origin.1 + ((r.y + r.h - self.scroll) * s + 0.5) as i32;
        Rect::new(x, y, (x2 - x).max(0), (y2 - y).max(0))
    }

    fn visible(&self, r: &Rect) -> bool {
        !r.intersect(&self.viewport).is_empty()
    }

    pub fn paint(&mut self, items: &[Item], doc: &Document, styles: &[Style], images: &mut Images) {
        let base_clip = self.canvas.clip;
        let mut clips: Vec<Rect> = alloc::vec![self.viewport.intersect(&base_clip)];
        self.canvas.clip = clips[0];
        let mut opacity = 1.0f32;
        for item in items {
            match item {
                Item::Opacity(o) => opacity = *o,
                Item::ClipPush(r) => {
                    let c = self.px(*r).intersect(clips.last().unwrap());
                    clips.push(c);
                    self.canvas.clip = c;
                }
                Item::ClipPop => {
                    if clips.len() > 1 {
                        clips.pop();
                    }
                    self.canvas.clip = *clips.last().unwrap();
                }
                Item::Fill { rect, color, radius } => {
                    let r = self.px(*rect);
                    if !self.visible(&r) || color.alpha() == 0 {
                        continue;
                    }
                    let radius = (*radius * self.scale) as i32;
                    if radius > 0 {
                        self.canvas.fill_rounded(r, radius, argb(*color, opacity));
                    } else {
                        self.canvas.fill(r, argb(*color, opacity));
                    }
                }
                Item::Border {
                    rect,
                    widths,
                    colors,
                    radius,
                } => {
                    let r = self.px(*rect);
                    if !self.visible(&r) {
                        continue;
                    }
                    let w: [i32; 4] = core::array::from_fn(|i| {
                        let v = widths[i] * self.scale;
                        if v > 0.0 { (v + 0.5).max(1.0) as i32 } else { 0 }
                    });
                    let radius = (*radius * self.scale) as i32;
                    if radius > 2 && w.iter().all(|&x| x == w[0]) && colors.iter().all(|c| *c == colors[0]) {
                        // Yuvarlak kenarlık: dış ve iç yuvarlak dikdörtgen farkı.
                        self.ring(r, radius, w[0], argb(colors[0], opacity));
                        continue;
                    }
                    let c: [Argb; 4] = core::array::from_fn(|i| argb(colors[i], opacity));
                    self.canvas.fill(Rect::new(r.x, r.y, r.w, w[0]), c[0]);
                    self.canvas.fill(Rect::new(r.x, r.bottom() - w[2], r.w, w[2]), c[2]);
                    self.canvas.fill(Rect::new(r.x, r.y, w[3], r.h), c[3]);
                    self.canvas.fill(Rect::new(r.right() - w[1], r.y, w[1], r.h), c[1]);
                }
                Item::Text {
                    x,
                    y,
                    text,
                    size,
                    bold,
                    color,
                    underline,
                    strike,
                } => {
                    let font = self.fonts.pick(size * self.scale, *bold);
                    let px = self.origin.0 + (x * self.scale) as i32;
                    let py = self.origin.1 + ((y - self.scroll) * self.scale) as i32;
                    let h = font.line_height();
                    if py > self.viewport.bottom() || py + h < self.viewport.y {
                        continue;
                    }
                    let end = font.draw(&mut self.canvas, px, py, text, argb(*color, opacity));
                    let thick = (self.scale as i32).max(1);
                    if *underline {
                        self.canvas.fill(Rect::new(px, py + font.ascent + thick, end - px, thick), argb(*color, opacity));
                    }
                    if *strike {
                        self.canvas.fill(Rect::new(px, py + font.ascent * 2 / 3, end - px, thick), argb(*color, opacity));
                    }
                }
                Item::Image { rect, src, radius } => {
                    let r = self.px(*rect);
                    if !self.visible(&r) || r.w <= 0 || r.h <= 0 {
                        continue;
                    }
                    self.image(r, src, (*radius * self.scale) as i32, images);
                }
                Item::Control { rect, node } => {
                    let r = self.px(*rect);
                    if self.visible(&r) {
                        self.control(r, *node, doc, &styles[*node]);
                    }
                }
            }
        }
        self.canvas.clip = base_clip;
    }

    fn ring(&mut self, r: Rect, radius: i32, width: i32, color: Argb) {
        // Üst/alt şeritler ve yanlar; köşeler yuvarlak dolgu ile yaklaşık.
        let w = width.max(1);
        self.canvas.fill(Rect::new(r.x + radius, r.y, r.w - 2 * radius, w), color);
        self.canvas.fill(Rect::new(r.x + radius, r.bottom() - w, r.w - 2 * radius, w), color);
        self.canvas.fill(Rect::new(r.x, r.y + radius, w, r.h - 2 * radius), color);
        self.canvas.fill(Rect::new(r.right() - w, r.y + radius, w, r.h - 2 * radius), color);
        for (cx, cy) in [
            (r.x + radius, r.y + radius),
            (r.right() - radius - 1, r.y + radius),
            (r.x + radius, r.bottom() - radius - 1),
            (r.right() - radius - 1, r.bottom() - radius - 1),
        ] {
            let left = cx < r.x + r.w / 2;
            let top = cy < r.y + r.h / 2;
            for dy in 0..=radius {
                for dx in 0..=radius {
                    let d2 = dx * dx + dy * dy;
                    let outer = radius * radius;
                    let inner = (radius - w).max(0) * (radius - w).max(0);
                    if d2 <= outer && d2 >= inner {
                        let x = if left { cx - dx } else { cx + dx };
                        let y = if top { cy - dy } else { cy + dy };
                        self.canvas.fill(Rect::new(x, y, 1, 1), color);
                    }
                }
            }
        }
    }

    fn image(&mut self, r: Rect, src: &str, radius: i32, images: &mut Images) {
        let Some(img) = images.map.get(src) else { return };
        let key = (String::from(src), r.w, r.h);
        if !images.scaled.contains_key(&key) {
            // En yakın komşu yerine kutu örnekleme (küçültmede daha düzgün).
            let (sw, sh) = (img.width as i32, img.height as i32);
            let mut out = alloc::vec![0u32; (r.w * r.h) as usize];
            for y in 0..r.h {
                let y0 = y * sh / r.h;
                let y1 = ((y + 1) * sh / r.h).max(y0 + 1).min(sh);
                for x in 0..r.w {
                    let x0 = x * sw / r.w;
                    let x1 = ((x + 1) * sw / r.w).max(x0 + 1).min(sw);
                    let (mut a, mut rr, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                    let step = ((y1 - y0) * (x1 - x0) / 16).max(1) as usize;
                    let mut k = 0usize;
                    for sy in y0..y1 {
                        for sx in x0..x1 {
                            k += 1;
                            if !k.is_multiple_of(step) {
                                continue;
                            }
                            let p = img.pixels[(sy * sw + sx) as usize];
                            a += p >> 24;
                            rr += (p >> 16) & 0xff;
                            g += (p >> 8) & 0xff;
                            b += p & 0xff;
                            n += 1;
                        }
                    }
                    let n = n.max(1);
                    out[(y * r.w + x) as usize] = (a / n) << 24 | (rr / n) << 16 | (g / n) << 8 | (b / n);
                }
            }
            if images.scaled.len() > 200 {
                images.scaled.clear();
            }
            images.scaled.insert(key.clone(), out);
        }
        let pixels = &images.scaled[&key];
        if radius > 0 {
            // Yuvarlak köşe: köşe dışındaki pikselleri atla.
            let clip = self.canvas.clip;
            for y in 0..r.h {
                for x in 0..r.w {
                    let cx = if x < radius { radius - x } else if x >= r.w - radius { x - (r.w - radius - 1) } else { 0 };
                    let cy = if y < radius { radius - y } else if y >= r.h - radius { y - (r.h - radius - 1) } else { 0 };
                    if cx * cx + cy * cy > radius * radius {
                        continue;
                    }
                    let (px, py) = (r.x + x, r.y + y);
                    if clip.contains(px, py) {
                        let p = pixels[(y * r.w + x) as usize];
                        self.canvas.fill(Rect::new(px, py, 1, 1), p);
                    }
                }
            }
        } else {
            self.canvas.blend_image(pixels, r.w, r.h, r.x, r.y);
        }
    }

    fn control(&mut self, r: Rect, node: NodeId, doc: &Document, style: &Style) {
        let tag = doc.tag(node).unwrap_or("");
        let kind = doc.attr(node, "type").unwrap_or("text").to_ascii_lowercase();
        let font = self.fonts.pick(style.font_size * self.scale, style.bold);
        let color = style.color.0;
        let ty = r.y + (r.h - font.line_height()) / 2;
        match (tag, kind.as_str()) {
            ("input", "checkbox" | "radio") => {
                let on = doc.attr(node, "checked").is_some();
                let accent = 0xff0b57d0;
                let radius = if kind == "radio" { r.w / 2 } else { (3.0 * self.scale) as i32 };
                self.canvas.fill_rounded(r, radius, if on { accent } else { 0xff8f8f9d });
                let inner = Rect::new(r.x + 1, r.y + 1, r.w - 2, r.h - 2);
                self.canvas.fill_rounded(inner, (radius - 1).max(0), if on { accent } else { 0xffffffff });
                if on {
                    if kind == "radio" {
                        let d = r.w / 3;
                        self.canvas.fill_rounded(Rect::new(r.x + d, r.y + d, r.w - 2 * d, r.h - 2 * d), r.w, 0xffffffff);
                    } else {
                        let f = self.fonts.pick(r.h as f32 * 0.9, true);
                        f.draw(&mut self.canvas, r.x + (r.w - f.measure("✓")) / 2, r.y + (r.h - f.line_height()) / 2, "✓", 0xffffffff);
                    }
                }
            }
            ("input", "submit" | "button" | "reset") | ("button", _) => {
                let label = if tag == "button" {
                    super::dom::collapse(&doc.visible_text(node))
                } else {
                    String::from(doc.attr(node, "value").unwrap_or(if kind == "reset" { "Sıfırla" } else { "Gönder" }))
                };
                let w = font.measure(&label);
                font.draw(&mut self.canvas, r.x + (r.w - w) / 2, ty, &label, color);
            }
            ("select", _) => {
                let value = selected_text(doc, node);
                font.draw_clipped(&mut self.canvas, r.x, ty, &value, r.w - font.size, color);
                let arrow = "▼";
                let small = self.fonts.pick(style.font_size * self.scale * 0.6, false);
                small.draw(&mut self.canvas, r.right() - small.measure(arrow), r.y + (r.h - small.line_height()) / 2, arrow, 0xff606060);
            }
            _ => {
                let password = kind == "password";
                let value = if tag == "textarea" {
                    doc.attr(node, "data-banna-value").map(String::from).unwrap_or_else(|| doc.text_content(node))
                } else {
                    String::from(doc.attr(node, "value").unwrap_or(""))
                };
                let focused = doc.focus == Some(node);
                let saved = self.canvas.clip;
                self.canvas.clip = self.canvas.clip.intersect(&r);
                if value.is_empty() {
                    if let Some(ph) = doc.attr(node, "placeholder") {
                        font.draw(&mut self.canvas, r.x, ty, ph, 0xff8a8a8a);
                    }
                    if focused && self.caret {
                        self.canvas.fill(Rect::new(r.x, ty, (self.scale as i32).max(1), font.line_height()), color);
                    }
                } else {
                    let shown: String = if password { value.chars().map(|_| '•').collect() } else { value };
                    if tag == "textarea" {
                        let mut y = r.y;
                        for line in shown.split('\n') {
                            font.draw(&mut self.canvas, r.x, y, line, color);
                            y += font.line_height();
                        }
                    } else {
                        let width = font.measure(&shown);
                        // Uzun metinde sonu görünsün.
                        let x = if width > r.w && focused { r.right() - width - 2 } else { r.x };
                        let end = font.draw(&mut self.canvas, x, ty, &shown, color);
                        if focused && self.caret {
                            self.canvas.fill(Rect::new(end, ty, (self.scale as i32).max(1), font.line_height()), color);
                        }
                    }
                }
                self.canvas.clip = saved;
            }
        }
    }
}

fn selected_text(doc: &Document, node: NodeId) -> String {
    let options: Vec<NodeId> = doc.descendants(node).into_iter().filter(|&n| doc.is(n, "option")).collect();
    let chosen = options
        .iter()
        .copied()
        .find(|&o| doc.attr(o, "selected").is_some())
        .or(options.first().copied());
    chosen.map(|o| super::dom::collapse(&doc.text_content(o))).unwrap_or_default()
}
