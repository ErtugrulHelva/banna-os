//! Yerleşim: hesaplanmış stillerden çizim listesi üretir. Blok akışı (kenar boşluğu
//! birleştirme dahil), satır içi akış ve satır kırma, `inline-block`, flex (satır/sütun,
//! sarma, büyüme/küçülme, hizalama), basit grid, tablolar, liste işaretleri, mutlak/sabit
//! konumlandırma, görseller ve form denetimleri. Ölçüler CSS pikselidir; yazı tipleri
//! cihaz pikselinde ölçülüp `scale` ile CSS pikseline çevrilir.

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;

use super::css::Color;
use super::dom::{Document, Kind, NodeId};
use super::fmath::ceilf;
use super::style::{Align, Display, Length, Position, Style};
use crate::gfx::text::Font;

/// Uygulamanın sağladığı yazı tipleri (cihaz pikseli boyutuyla).
pub trait Fonts {
    fn pick(&self, device_px: f32, bold: bool) -> &'static Font;
}

/// Yüklenmiş görsellerin doğal boyutu.
pub trait Images {
    fn size(&self, src: &str) -> Option<(f32, f32)>;
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RectF {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl RectF {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        x >= self.x && y >= self.y && x < self.x + self.w && y < self.y + self.h
    }
}

#[derive(Clone, Debug)]
pub enum Item {
    Fill {
        rect: RectF,
        color: Color,
        radius: f32,
    },
    Border {
        rect: RectF,
        widths: [f32; 4],
        colors: [Color; 4],
        radius: f32,
    },
    Text {
        x: f32,
        y: f32,
        text: String,
        size: f32,
        bold: bool,
        color: Color,
        underline: bool,
        strike: bool,
    },
    Image {
        rect: RectF,
        src: String,
        radius: f32,
    },
    /// Form denetimi içeriği (değer, yer tutucu, imleç) çizim anında eklenir.
    Control {
        rect: RectF,
        node: NodeId,
    },
    ClipPush(RectF),
    ClipPop,
    Opacity(f32),
}

pub struct Layout {
    pub items: Vec<Item>,
    /// Her öğenin kenarlık kutusu (isabet testi, getBoundingClientRect).
    pub boxes: Vec<(NodeId, RectF)>,
    pub height: f32,
    pub width: f32,
}

impl Layout {
    /// (x, y) noktasındaki en derin öğe.
    pub fn hit(&self, x: f32, y: f32) -> Option<NodeId> {
        self.boxes
            .iter()
            .rev()
            .find(|(_, r)| r.contains(x, y))
            .map(|(n, _)| *n)
    }

    pub fn rect_of(&self, node: NodeId) -> Option<RectF> {
        self.boxes.iter().find(|(n, _)| *n == node).map(|(_, r)| *r)
    }
}

struct Ctx<'a> {
    doc: &'a Document,
    styles: &'a [Style],
    fonts: &'a dyn Fonts,
    images: &'a dyn Images,
    scale: f32,
    viewport: (f32, f32),
    items: Vec<Item>,
    boxes: Vec<(NodeId, RectF)>,
    /// Ölçüm kipinde çizim yapılmaz.
    measuring: u32,
    /// Satır içi öğelerin kapsayan blok genişliği (`width: 100%` gibi yüzdeler için).
    cb_width: f32,
    max_content: BTreeMap<NodeId, f32>,
    /// Konumlandırılmış ataların kutuları (mutlak konum için).
    positioned: Vec<RectF>,
    deferred: Vec<(NodeId, RectF)>,
}

pub fn layout(
    doc: &Document,
    styles: &[Style],
    fonts: &dyn Fonts,
    images: &dyn Images,
    scale: f32,
    viewport: (f32, f32),
) -> Layout {
    let mut c = Ctx {
        doc,
        styles,
        fonts,
        images,
        scale,
        viewport,
        items: Vec::new(),
        boxes: Vec::new(),
        measuring: 0,
        cb_width: viewport.0,
        max_content: BTreeMap::new(),
        positioned: alloc::vec![RectF {
            x: 0.0,
            y: 0.0,
            w: viewport.0,
            h: viewport.1
        }],
        deferred: Vec::new(),
    };
    let root = doc
        .nodes
        .iter()
        .position(|n| matches!(&n.kind, Kind::Element { tag, .. } if tag == "html"))
        .unwrap_or(0);
    // Sayfa arka planı: body ya da html'in rengi tüm görünüme yayılır.
    let body = doc.body();
    let page_bg = [Some(root), body]
        .into_iter()
        .flatten()
        .map(|n| styles[n].background)
        .find(|c| c.alpha() > 0)
        .unwrap_or(Color(0xffffffff));
    c.items.push(Item::Fill {
        rect: RectF {
            x: 0.0,
            y: 0.0,
            w: viewport.0,
            h: 1e7,
        },
        color: page_bg,
        radius: 0.0,
    });
    let height = c.block(root, 0.0, 0.0, viewport.0, true);
    while let Some((node, container)) = c.deferred.pop() {
        c.positioned.push(container);
        c.absolute(node, container);
        c.positioned.pop();
    }
    let height = c
        .boxes
        .iter()
        .map(|(_, r)| r.y + r.h)
        .fold(height, f32::max);
    Layout {
        items: c.items,
        boxes: c.boxes,
        height,
        width: viewport.0,
    }
}

/// Satır içi akıştaki bir parça.
enum Piece {
    Word {
        text: String,
        width: f32,
        style: NodeId,
        space_before: bool,
    },
    Break,
    Atomic {
        node: NodeId,
        width: f32,
        height: f32,
        margin: [f32; 4],
        space_before: bool,
    },
}

struct Placed {
    piece: usize,
    x: f32,
    width: f32,
    height: f32,
    ascent: f32,
}

impl Ctx<'_> {
    fn st(&self, node: NodeId) -> &Style {
        &self.styles[node]
    }

    fn emit(&mut self, item: Item) {
        if self.measuring == 0 {
            self.items.push(item);
        }
    }

    fn font(&self, style: &Style) -> &'static Font {
        self.fonts.pick(style.font_size * self.scale, style.bold)
    }

    fn text_width(&self, font: &Font, text: &str) -> f32 {
        font.measure(text) as f32 / self.scale
    }

    fn line_height(&self, style: &Style) -> f32 {
        (style.font_size * style.line_height).max(style.font_size)
    }

    fn is_block_level(&self, node: NodeId) -> bool {
        match &self.doc.nodes[node].kind {
            Kind::Element { .. } => {
                let s = self.st(node);
                matches!(
                    s.display,
                    Display::Block
                        | Display::ListItem
                        | Display::Flex
                        | Display::Grid
                        | Display::Table
                        | Display::TableRow
                ) && !matches!(s.position, Position::Absolute | Position::Fixed)
            }
            _ => false,
        }
    }

    fn out_of_flow(&self, node: NodeId) -> bool {
        matches!(self.doc.nodes[node].kind, Kind::Element { .. })
            && matches!(self.st(node).position, Position::Absolute | Position::Fixed)
            && self.st(node).display != Display::None
    }

    fn margins(&self, s: &Style, cw: f32) -> [f32; 4] {
        [
            s.margin[0].or_zero(cw),
            s.margin[1].or_zero(cw),
            s.margin[2].or_zero(cw),
            s.margin[3].or_zero(cw),
        ]
    }

    fn paddings(&self, s: &Style, cw: f32) -> [f32; 4] {
        [
            s.padding[0].or_zero(cw),
            s.padding[1].or_zero(cw),
            s.padding[2].or_zero(cw),
            s.padding[3].or_zero(cw),
        ]
    }

    /// Kutunun içerik genişliği (kenarlık kutusu genişliğinden).
    fn resolve_width(&self, s: &Style, avail: f32, cw: f32, shrink: Option<f32>) -> f32 {
        let pad = self.paddings(s, cw);
        let extra = pad[1] + pad[3] + s.border[1] + s.border[3];
        let m = self.margins(s, cw);
        let mut width = match s.width.px(cw) {
            Some(w) if s.box_sizing_border => (w - extra).max(0.0),
            Some(w) => w,
            None => match shrink {
                Some(content) => content.min((avail - m[1] - m[3] - extra).max(0.0)),
                None => (avail - m[1] - m[3] - extra).max(0.0),
            },
        };
        if let Some(max) = s.max_width.px(cw) {
            let max = if s.box_sizing_border { max - extra } else { max };
            width = width.min(max.max(0.0));
        }
        if let Some(min) = s.min_width.px(cw) {
            let min = if s.box_sizing_border { min - extra } else { min };
            width = width.max(min);
        }
        width
    }

    /// Blok kutusu: (x, y) kenar boşluğu kutusunun sol üstü; döndürülen yükseklik
    /// kenar boşlukları dahil.
    fn block(&mut self, node: NodeId, x: f32, y: f32, avail: f32, root: bool) -> f32 {
        let s = self.st(node).clone();
        if s.display == Display::None {
            return 0.0;
        }
        let cw = avail;
        let m = self.margins(&s, cw);
        let pad = self.paddings(&s, cw);
        let b = s.border;
        let width = self.resolve_width(&s, avail, cw, None);
        let mut left = x + m[3];
        // `margin: 0 auto` ile ortalama.
        let outer = width + pad[1] + pad[3] + b[1] + b[3];
        if s.width.px(cw).is_some() || s.max_width.px(cw).is_some() {
            let free = avail - outer - m[1] - m[3];
            // <center> ve align=center içindeki genişliği verilmiş bloklar da ortalanır.
            let legacy_center = self.doc.nodes[node].parent.is_some_and(|p| {
                self.doc.is(p, "center")
                    || self.doc.attr(p, "align").is_some_and(|a| a.eq_ignore_ascii_case("center"))
            });
            match (s.margin[3], s.margin[1]) {
                (Length::Auto, Length::Auto) => left = x + (avail - outer) / 2.0,
                (Length::Px(l), Length::Px(r)) if legacy_center && l == 0.0 && r == 0.0 => {
                    left = x + (avail - outer) / 2.0
                }
                (Length::Auto, _) => left = x + free + m[3],
                _ => {}
            }
        }
        let top = y + m[0];
        let content_x = left + b[3] + pad[3];
        let content_y = top + b[0] + pad[0];
        let item_start = self.items.len();
        let slot = self.reserve_box(node);
        let relative = self.relative_offset(&s, cw);
        let content_h = self.contents(node, &s, content_x, content_y, width);
        let mut height = match s.height.px(self.viewport.1) {
            Some(h) if !matches!(s.height, Length::Percent(_)) || root => {
                if s.box_sizing_border {
                    (h - pad[0] - pad[2] - b[0] - b[2]).max(0.0)
                } else {
                    h
                }
            }
            _ => content_h,
        };
        if let Some(min) = fixed(s.min_height) {
            height = height.max(if s.box_sizing_border {
                min - pad[0] - pad[2] - b[0] - b[2]
            } else {
                min
            });
        }
        if let Some(max) = fixed(s.max_height) {
            height = height.min(max);
        }
        let rect = RectF {
            x: left,
            y: top,
            w: outer,
            h: height + pad[0] + pad[2] + b[0] + b[2],
        };
        self.decorate_slot(node, &s, rect, item_start, content_h > height && s.overflow_hidden, slot);
        if let Some((dx, dy)) = relative {
            self.shift(item_start, dx, dy);
        }
        self.list_marker(node, &s, content_x, content_y);
        rect.h + m[0] + m[2]
    }

    fn relative_offset(&self, s: &Style, cw: f32) -> Option<(f32, f32)> {
        if s.position != Position::Relative {
            return None;
        }
        let dx = s.offsets[3]
            .px(cw)
            .or_else(|| s.offsets[1].px(cw).map(|r| -r))
            .unwrap_or(0.0);
        let dy = s.offsets[0]
            .px(self.viewport.1)
            .or_else(|| s.offsets[2].px(self.viewport.1).map(|b| -b))
            .unwrap_or(0.0);
        (dx != 0.0 || dy != 0.0).then_some((dx, dy))
    }

    fn shift(&mut self, from: usize, dx: f32, dy: f32) {
        for item in &mut self.items[from..] {
            match item {
                Item::Fill { rect, .. }
                | Item::Border { rect, .. }
                | Item::Image { rect, .. }
                | Item::Control { rect, .. }
                | Item::ClipPush(rect) => {
                    rect.x += dx;
                    rect.y += dy;
                }
                Item::Text { x, y, .. } => {
                    *x += dx;
                    *y += dy;
                }
                _ => {}
            }
        }
    }

    /// Arka plan ve kenarlık içeriğin altına (önce) eklenir; kutu kaydedilir.
    /// Kutunun isabet listesindeki yerini (ön sıralı: ebeveyn çocuklarından önce) ayırır.
    fn reserve_box(&mut self, node: NodeId) -> Option<usize> {
        if self.measuring > 0 {
            return None;
        }
        self.boxes.push((node, RectF::default()));
        Some(self.boxes.len() - 1)
    }

    fn decorate(&mut self, node: NodeId, s: &Style, rect: RectF, at: usize, clip: bool) {
        self.decorate_slot(node, s, rect, at, clip, None);
    }

    fn decorate_slot(&mut self, node: NodeId, s: &Style, rect: RectF, at: usize, clip: bool, slot: Option<usize>) {
        if self.measuring > 0 {
            return;
        }
        match slot {
            Some(i) => self.boxes[i] = (node, rect),
            None => self.boxes.push((node, rect)),
        }
        if !s.visible {
            return;
        }
        let radius = s.radius.min(rect.w / 2.0).min(rect.h / 2.0);
        let mut pre = Vec::new();
        if s.opacity < 0.99 {
            pre.push(Item::Opacity(s.opacity));
        }
        if s.background.alpha() > 0 {
            pre.push(Item::Fill {
                rect,
                color: s.background,
                radius,
            });
        }
        if let Some(src) = &s.background_image {
            pre.push(Item::Image {
                rect,
                src: src.clone(),
                radius,
            });
        }
        if s.border.iter().any(|&w| w > 0.0) {
            pre.push(Item::Border {
                rect,
                widths: s.border,
                colors: s.border_color,
                radius,
            });
        }
        if clip {
            pre.push(Item::ClipPush(rect));
        }
        let n = pre.len();
        self.items.splice(at..at, pre);
        if clip {
            self.items.push(Item::ClipPop);
        }
        if s.opacity < 0.99 && n > 0 {
            self.items.push(Item::Opacity(1.0));
        }
    }

    fn list_marker(&mut self, node: NodeId, s: &Style, x: f32, y: f32) {
        if s.display != Display::ListItem || s.list_style == 0 || !s.visible {
            return;
        }
        let marker = if s.list_style == 2 {
            let index = self.doc.nodes[node]
                .parent
                .map(|p| {
                    self.doc
                        .element_children(p)
                        .iter()
                        .position(|&c| c == node)
                        .unwrap_or(0)
                })
                .unwrap_or(0);
            alloc::format!("{}.", index + 1)
        } else {
            String::from("•")
        };
        let font = self.font(s);
        let w = self.text_width(font, &marker);
        self.emit(Item::Text {
            x: x - w - s.font_size * 0.4,
            y: y + (self.line_height(s) - s.font_size * 1.2) / 2.0,
            text: marker,
            size: s.font_size,
            bold: s.bold,
            color: s.color,
            underline: false,
            strike: false,
        });
    }

    /// Kutunun içeriği (çocuklar); içerik yüksekliğini döndürür.
    fn contents(&mut self, node: NodeId, s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let tag = self.doc.tag(node).unwrap_or("");
        if matches!(tag, "img" | "input" | "button" | "select" | "textarea" | "video" | "iframe" | "canvas" | "svg")
        {
            let (w, h) = self.replaced_size(node, s, width);
            // Blok düzeyinde verilmiş genişlik (`display:block; width:100%`) zaten içerik
            // genişliğine çözüldü; yüzdeyi ve dolguyu ikinci kez uygulama.
            let w = if s.width != Length::Auto { width } else { w };
            self.replaced(node, s, x, y, w, h);
            return h;
        }
        // Konumlu kutunun içerik yüksekliği CSS'te verilmişse mutlak çocukların yüzdeleri
        // (`top: 20%`) ona göre çözülür; çocukların hepsi mutlaksa akış yüksekliği 0 olur.
        let declared_h = match s.height {
            Length::Px(h) if s.box_sizing_border => {
                let pad = self.paddings(s, width);
                Some((h - pad[0] - pad[2] - s.border[0] - s.border[2]).max(0.0))
            }
            Length::Px(h) => Some(h),
            _ => None,
        }
        .map(|h| fixed(s.min_height).map_or(h, |m| h.max(m)))
        .or_else(|| fixed(s.min_height));
        if s.position != Position::Static {
            self.positioned.push(RectF {
                x,
                y,
                w: width,
                h: declared_h.unwrap_or(self.viewport.1),
            });
        }
        let h = match s.display {
            Display::Flex | Display::InlineFlex => self.flex(node, s, x, y, width),
            Display::Grid => self.grid(node, s, x, y, width),
            Display::Table => self.table(node, s, x, y, width),
            Display::TableRow => self.table_row(node, x, y, width, &[]),
            _ => self.flow(node, x, y, width),
        };
        if s.position != Position::Static {
            let container = self.positioned.pop().unwrap_or_default();
            let container = RectF {
                h: declared_h.map_or(h, |d| d.max(h)).max(1.0),
                ..container
            };
            for child in self.doc.nodes[node].children.clone() {
                if self.out_of_flow(child) && self.measuring == 0 {
                    self.deferred.push((child, container));
                }
            }
        } else if self.measuring == 0 {
            for child in self.doc.nodes[node].children.clone() {
                if self.out_of_flow(child) {
                    let container = *self.positioned.last().unwrap();
                    let container = if self.st(child).position == Position::Fixed {
                        RectF {
                            x: 0.0,
                            y: 0.0,
                            w: self.viewport.0,
                            h: self.viewport.1,
                        }
                    } else {
                        container
                    };
                    self.deferred.push((child, container));
                }
            }
        }
        h
    }

    fn absolute(&mut self, node: NodeId, container: RectF) {
        let s = self.st(node).clone();
        let cw = container.w;
        let content = self.max_content_width(node);
        let left = s.offsets[3].px(cw);
        let right = s.offsets[1].px(cw);
        let shrink = match (left, right, s.width) {
            (Some(l), Some(r), Length::Auto) => Some((cw - l - r).max(0.0)),
            _ => Some(content),
        };
        let pad = self.paddings(&s, cw);
        let width = self.resolve_width(&s, cw, cw, shrink);
        let outer = width + pad[1] + pad[3] + s.border[1] + s.border[3];
        let x = match (left, right) {
            (Some(l), _) => container.x + l,
            (None, Some(r)) => container.x + cw - r - outer,
            _ => container.x,
        };
        let top = s.offsets[0].px(container.h);
        let bottom = s.offsets[2].px(container.h);
        let at = self.items.len();
        let mut style = s.clone();
        style.margin = [Length::Px(0.0); 4];
        let h = self.block_with(node, &style, width, x, container.y);
        let y = match (top, bottom) {
            (Some(t), _) => container.y + t,
            (None, Some(b)) => container.y + container.h - b - h,
            _ => container.y,
        };
        self.shift(at, 0.0, y - container.y);
    }

    /// Genişliği önceden belirlenmiş blok (mutlak konum, flex öğesi, hücre).
    fn block_with(&mut self, node: NodeId, s: &Style, width: f32, x: f32, y: f32) -> f32 {
        let pad = self.paddings(s, width);
        let b = s.border;
        let m = self.margins(s, width);
        let item_start = self.items.len();
        let slot = self.reserve_box(node);
        let cx = x + m[3] + b[3] + pad[3];
        let cy = y + m[0] + b[0] + pad[0];
        let content_h = self.contents(node, s, cx, cy, width);
        let mut h = match s.height {
            Length::Px(h) if s.box_sizing_border => (h - pad[0] - pad[2] - b[0] - b[2]).max(0.0),
            Length::Px(h) => h,
            _ => content_h,
        };
        if let Some(min) = fixed(s.min_height) {
            h = h.max(min);
        }
        let rect = RectF {
            x: x + m[3],
            y: y + m[0],
            w: width + pad[1] + pad[3] + b[1] + b[3],
            h: h + pad[0] + pad[2] + b[0] + b[2],
        };
        self.decorate_slot(node, s, rect, item_start, s.overflow_hidden && content_h > h, slot);
        rect.h + m[0] + m[2]
    }

    /// Blok bağlamı: blok çocuklar alt alta, satır içi olanlar satır kutularında.
    fn flow(&mut self, node: NodeId, x: f32, y: f32, width: f32) -> f32 {
        let children = self.doc.nodes[node].children.clone();
        let mut cursor = y;
        let mut prev_margin = 0.0f32;
        let mut inline_run: Vec<NodeId> = Vec::new();
        let mut first = true;
        for &child in children.iter().chain(core::iter::once(&usize::MAX)) {
            let block = child != usize::MAX && self.is_block_level(child);
            if (child == usize::MAX || block) && !inline_run.is_empty() {
                let run = core::mem::take(&mut inline_run);
                let h = self.inline(node, &run, x, cursor, width);
                if h > 0.0 {
                    cursor += h;
                    prev_margin = 0.0;
                    first = false;
                }
            }
            if child == usize::MAX {
                break;
            }
            if self.out_of_flow(child) {
                continue;
            }
            if block {
                let cs = self.st(child).clone();
                if cs.display == Display::None {
                    continue;
                }
                let top_margin = cs.margin[0].or_zero(width);
                // Kardeş kenar boşlukları birleşir (büyük olan geçerli).
                let collapse = if first { 0.0 } else { prev_margin.min(top_margin) };
                let h = self.block(child, x, cursor - collapse, width, false);
                cursor += h - collapse;
                prev_margin = cs.margin[2].or_zero(width);
                first = false;
            } else {
                inline_run.push(child);
            }
        }
        cursor - y
    }

    /// Satır içi parçaları toplar (metin kelimelere bölünür).
    fn collect(&mut self, node: NodeId, pieces: &mut Vec<Piece>, pending_space: &mut bool) {
        match &self.doc.nodes[node].kind {
            Kind::Text(text) => {
                let parent = self.doc.nodes[node].parent.unwrap_or(node);
                let s = self.st(node).clone();
                let font = self.font(&s);
                let text = transform(text, s.text_transform);
                if s.white_space_pre {
                    for (i, line) in text.split('\n').enumerate() {
                        if i > 0 {
                            pieces.push(Piece::Break);
                        }
                        let line = line.replace('\t', "    ");
                        if !line.is_empty() {
                            let width = self.text_width(font, &line);
                            pieces.push(Piece::Word {
                                text: line,
                                width,
                                style: node,
                                space_before: false,
                            });
                        }
                    }
                    let _ = parent;
                    return;
                }
                if text.starts_with(char::is_whitespace) {
                    *pending_space = true;
                }
                for word in text.split_whitespace() {
                    let word = word.replace('\u{a0}', " ");
                    let width = self.text_width(font, &word);
                    pieces.push(Piece::Word {
                        text: word,
                        width,
                        style: node,
                        space_before: core::mem::take(pending_space),
                    });
                    *pending_space = true;
                }
                if !text.ends_with(char::is_whitespace) {
                    *pending_space = false;
                }
            }
            Kind::Element { tag, .. } => {
                let s = self.st(node).clone();
                if s.display == Display::None || self.out_of_flow(node) {
                    return;
                }
                if tag == "br" {
                    pieces.push(Piece::Break);
                    *pending_space = false;
                    return;
                }
                let atomic = matches!(
                    s.display,
                    Display::InlineBlock | Display::InlineFlex | Display::Table
                ) || matches!(
                    tag.as_str(),
                    "img" | "input" | "button" | "select" | "textarea" | "video" | "iframe" | "svg" | "canvas"
                );
                if atomic || self.is_block_level(node) {
                    let (width, height, margin) = self.atomic_size(node, &s);
                    pieces.push(Piece::Atomic {
                        node,
                        width,
                        height,
                        margin,
                        space_before: core::mem::take(pending_space),
                    });
                    return;
                }
                // Satır içi öğenin yatay dolgusu/kenarlığı: kelimelere boşluk olarak.
                let lead = s.padding[3].or_zero(0.0) + s.border[3] + s.margin[3].or_zero(0.0);
                if lead > 0.5 {
                    pieces.push(Piece::Word {
                        text: String::new(),
                        width: lead,
                        style: node,
                        space_before: core::mem::take(pending_space),
                    });
                }
                for child in self.doc.nodes[node].children.clone() {
                    self.collect(child, pieces, pending_space);
                }
                let trail = s.padding[1].or_zero(0.0) + s.border[1] + s.margin[1].or_zero(0.0);
                if trail > 0.5 {
                    pieces.push(Piece::Word {
                        text: String::new(),
                        width: trail,
                        style: node,
                        space_before: false,
                    });
                }
            }
            _ => {}
        }
    }

    fn atomic_size(&mut self, node: NodeId, s: &Style) -> (f32, f32, [f32; 4]) {
        let cb = self.cb_width;
        let m = self.margins(s, cb);
        let pad = self.paddings(s, cb);
        let extra_w = pad[1] + pad[3] + s.border[1] + s.border[3];
        let extra_h = pad[0] + pad[2] + s.border[0] + s.border[2];
        let tag = self.doc.tag(node).unwrap_or("");
        if matches!(tag, "img" | "input" | "button" | "select" | "textarea" | "video" | "iframe" | "svg" | "canvas") {
            // replaced_size içerik boyutunu verir (border-box'ta dolgu düşülmüş olarak).
            let (w, h) = self.replaced_size(node, s, cb);
            return (w + extra_w, h + extra_h, m);
        }
        let content = self.max_content_width(node);
        let width = self.resolve_width(s, cb, cb, Some(content));
        self.measuring += 1;
        let h = self.block_with(node, s, width, 0.0, 0.0) - m[0] - m[2];
        self.measuring -= 1;
        (width + extra_w, h, m)
    }

    /// Satır kırmadan en geniş içerik (shrink-to-fit için).
    fn max_content_width(&mut self, node: NodeId) -> f32 {
        if let Some(&w) = self.max_content.get(&node) {
            return w;
        }
        let s = self.st(node).clone();
        let w = if let Some(w) = s.width.px(self.viewport.0) {
            w
        } else {
            let tag = self.doc.tag(node).unwrap_or("");
            if matches!(tag, "img" | "input" | "button" | "select" | "textarea") {
                self.replaced_size(node, &s, self.viewport.0).0
            } else {
                let mut best = 0.0f32;
                let mut line = 0.0f32;
                let mut pieces = Vec::new();
                let mut space = false;
                for child in self.doc.nodes[node].children.clone() {
                    if self.is_block_level(child) {
                        let cw = self.max_content_width(child);
                        let cs = self.st(child).clone();
                        let pad = self.paddings(&cs, 0.0);
                        best = best.max(cw + pad[1] + pad[3] + cs.border[1] + cs.border[3]);
                    } else {
                        self.collect(child, &mut pieces, &mut space);
                    }
                }
                let flex_row = matches!(s.display, Display::Flex | Display::InlineFlex) && s.flex_row;
                let mut sum = 0.0;
                for p in &pieces {
                    match p {
                        Piece::Word {
                            width,
                            space_before,
                            style,
                            ..
                        } => {
                            let sp = if *space_before {
                                self.text_width(self.font(&self.st(*style).clone()), " ")
                            } else {
                                0.0
                            };
                            line += width + sp;
                        }
                        Piece::Atomic { width, margin, .. } => {
                            line += width + margin[1] + margin[3];
                            sum += width + margin[1] + margin[3] + s.gap;
                        }
                        Piece::Break => {
                            best = best.max(line);
                            line = 0.0;
                        }
                    }
                }
                let w = best.max(line);
                if flex_row { w.max(sum - s.gap) } else { w }
            }
        };
        let w = ceilf(w + 0.5);
        self.max_content.insert(node, w);
        w
    }

    fn inline(&mut self, block: NodeId, run: &[NodeId], x: f32, y: f32, width: f32) -> f32 {
        let mut pieces = Vec::new();
        let mut space = false;
        let outer_cb = core::mem::replace(&mut self.cb_width, width);
        for &n in run {
            self.collect(n, &mut pieces, &mut space);
        }
        self.cb_width = outer_cb;
        if pieces.is_empty() {
            return 0.0;
        }
        let bs = self.st(block).clone();
        let align = bs.text_align;
        let nowrap = bs.nowrap;
        let mut lines: Vec<Vec<Placed>> = Vec::new();
        let mut line: Vec<Placed> = Vec::new();
        let mut cx = 0.0f32;
        for (i, p) in pieces.iter().enumerate() {
            let (w, h, ascent, space_before) = match p {
                Piece::Break => {
                    if line.is_empty() {
                        let lh = self.line_height(&bs);
                        line.push(Placed {
                            piece: i,
                            x: 0.0,
                            width: 0.0,
                            height: lh,
                            ascent: lh * 0.8,
                        });
                    }
                    lines.push(core::mem::take(&mut line));
                    cx = 0.0;
                    continue;
                }
                Piece::Word {
                    width,
                    style,
                    space_before,
                    ..
                } => {
                    let s = self.st(*style).clone();
                    let lh = self.line_height(&s);
                    let font = self.font(&s);
                    let em = (font.ascent + font.descent) as f32 / self.scale;
                    let ascent = (lh - em) / 2.0 + font.ascent as f32 / self.scale;
                    (*width, lh, ascent, *space_before)
                }
                Piece::Atomic {
                    width,
                    height,
                    margin,
                    space_before,
                    node,
                } => {
                    let middle = self.st(*node).vertical_middle;
                    let full = height + margin[0] + margin[2];
                    (
                        width + margin[1] + margin[3],
                        full,
                        if middle { full / 2.0 } else { full },
                        *space_before,
                    )
                }
            };
            let sp = if space_before && !line.is_empty() {
                let style = match p {
                    Piece::Word { style, .. } => *style,
                    _ => block,
                };
                self.text_width(self.font(&self.st(style).clone()), " ")
            } else {
                0.0
            };
            if !line.is_empty() && cx + sp + w > width + 0.5 && !nowrap {
                lines.push(core::mem::take(&mut line));
                cx = 0.0;
            } else {
                cx += sp;
            }
            line.push(Placed {
                piece: i,
                x: cx,
                width: w,
                height: h,
                ascent,
            });
            cx += w;
        }
        if !line.is_empty() {
            lines.push(line);
        }
        let mut cursor = y;
        let count = lines.len();
        for (li, line) in lines.into_iter().enumerate() {
            let ascent = line.iter().map(|p| p.ascent).fold(0.0, f32::max);
            let descent = line
                .iter()
                .map(|p| p.height - p.ascent)
                .fold(0.0, f32::max);
            let height = (ascent + descent).max(self.line_height(&bs) * 0.5);
            let used = line.last().map(|p| p.x + p.width).unwrap_or(0.0);
            let offset = match align {
                Align::Center => (width - used) / 2.0,
                Align::End => width - used,
                _ => 0.0,
            };
            let justify = if align == Align::Justify && li + 1 < count && line.len() > 1 {
                (width - used) / (line.len() - 1) as f32
            } else {
                0.0
            };
            // Altı çizili ardışık sözcükler arasındaki boşluğun da altı çizilir (bağlantılar).
            let mut underlined_end: Option<f32> = None;
            for (k, p) in line.iter().enumerate() {
                let px = x + offset + p.x + justify * k as f32;
                let baseline = cursor + ascent;
                match &pieces[p.piece] {
                    Piece::Word { text, style, space_before, .. } => {
                        let s = self.st(*style).clone();
                        let underline = s.underline || self.underlined(*style);
                        if !text.is_empty() && s.visible {
                            let font_ascent = self.font(&s).ascent as f32 / self.scale;
                            let (tx, text) = match underlined_end {
                                Some(end) if underline && *space_before && px > end => {
                                    let space = self.text_width(self.font(&s), " ");
                                    (px - space, alloc::format!(" {}", text))
                                }
                                _ => (px, text.clone()),
                            };
                            self.emit(Item::Text {
                                x: tx,
                                y: baseline - font_ascent,
                                text,
                                size: s.font_size,
                                bold: s.bold,
                                color: s.color,
                                underline,
                                strike: s.line_through,
                            });
                        }
                        underlined_end = (underline && s.visible).then_some(px + p.width);
                        // Satır içi öğelerin kutuları (bağlantı tıklaması için).
                        let mut owner = self.doc.nodes[*style].parent;
                        let rect = RectF {
                            x: px,
                            y: cursor,
                            w: p.width,
                            h: height,
                        };
                        if matches!(self.doc.nodes[*style].kind, Kind::Element { .. }) {
                            owner = Some(*style);
                        }
                        while let Some(o) = owner {
                            if o == block || self.measuring > 0 {
                                break;
                            }
                            self.boxes.push((o, rect));
                            let os = self.st(o).clone();
                            if os.background.alpha() > 0 {
                                self.items.push(Item::Fill {
                                    rect,
                                    color: os.background,
                                    radius: os.radius.min(4.0),
                                });
                                // Arka plan metnin altında kalsın.
                                let n = self.items.len();
                                if n >= 2 && matches!(self.items[n - 2], Item::Text { .. }) {
                                    self.items.swap(n - 1, n - 2);
                                }
                            }
                            owner = self.doc.nodes[o].parent;
                        }
                    }
                    Piece::Atomic {
                        node,
                        width,
                        height: h,
                        margin,
                        ..
                    } => {
                        underlined_end = None;
                        let node = *node;
                        let s = self.st(node).clone();
                        let top = cursor + ascent - p.ascent + margin[0];
                        let pad = self.paddings(&s, width.max(1.0));
                        let inner = width - pad[1] - pad[3] - s.border[1] - s.border[3];
                        let at = self.items.len();
                        let tag = self.doc.tag(node).unwrap_or("");
                        if matches!(tag, "img" | "input" | "button" | "select" | "textarea" | "video" | "iframe" | "svg" | "canvas")
                        {
                            let rect = RectF {
                                x: px + margin[3],
                                y: top,
                                w: *width,
                                h: *h,
                            };
                            let cx = rect.x + s.border[3] + pad[3];
                            let cy = rect.y + s.border[0] + pad[0];
                            let iw = rect.w - s.border[1] - s.border[3] - pad[1] - pad[3];
                            let ih = rect.h - s.border[0] - s.border[2] - pad[0] - pad[2];
                            self.replaced(node, &s, cx, cy, iw, ih);
                            self.decorate(node, &s, rect, at, false);
                        } else {
                            let mut fixed = s.clone();
                            fixed.margin = [Length::Px(0.0); 4];
                            self.block_with(node, &fixed, inner.max(0.0), px + margin[3], top);
                        }
                        // Ebeveyn satır içi öğeler (ör. <a><img></a>) de tıklanabilir olsun.
                        let mut owner = self.doc.nodes[node].parent;
                        while let Some(o) = owner {
                            if o == block || self.measuring > 0 {
                                break;
                            }
                            self.boxes.insert(
                                self.boxes.len().saturating_sub(1),
                                (
                                    o,
                                    RectF {
                                        x: px,
                                        y: top,
                                        w: *width,
                                        h: *h,
                                    },
                                ),
                            );
                            owner = self.doc.nodes[o].parent;
                        }
                    }
                    Piece::Break => {}
                }
            }
            cursor += height;
        }
        cursor - y
    }

    fn underlined(&self, node: NodeId) -> bool {
        let mut n = self.doc.nodes[node].parent;
        while let Some(p) = n {
            let s = self.st(p);
            if s.underline {
                return true;
            }
            if matches!(s.display, Display::Block | Display::ListItem | Display::Flex) {
                return false;
            }
            n = self.doc.nodes[p].parent;
        }
        false
    }

    /// Görsel ve form denetimlerinin içerik boyutu.
    fn replaced_size(&mut self, node: NodeId, s: &Style, avail: f32) -> (f32, f32) {
        let tag = self.doc.tag(node).unwrap_or("");
        let font = s.font_size;
        let (iw, ih) = match tag {
            "img" | "video" | "canvas" | "iframe" | "svg" => {
                let natural = self
                    .doc
                    .attr(node, "src")
                    .and_then(|src| self.images.size(src));
                match (tag, natural) {
                    (_, Some(size)) => size,
                    ("iframe", _) => (300.0, 150.0),
                    ("svg", _) => (font, font),
                    _ => (0.0, 0.0),
                }
            }
            "textarea" => (font * 18.0, font * 1.3 * self.doc.attr(node, "rows").and_then(|r| r.parse().ok()).unwrap_or(3.0)),
            "select" => {
                let longest = self
                    .doc
                    .element_children(node)
                    .iter()
                    .map(|&o| {
                        let t = self.doc.text_content(o);
                        self.text_width(self.font(s), t.trim())
                    })
                    .fold(0.0, f32::max);
                (longest + font * 1.5, font * 1.3)
            }
            "button" => {
                let text = super::dom::collapse(&self.doc.visible_text(node));
                (self.text_width(self.font(s), &text), font * 1.3)
            }
            _ => {
                let kind = self.doc.attr(node, "type").unwrap_or("text").to_ascii_lowercase();
                match kind.as_str() {
                    "checkbox" | "radio" => (font * 0.9, font * 0.9),
                    "submit" | "button" | "reset" => {
                        let label = self.doc.attr(node, "value").unwrap_or(if kind == "reset" {
                            "Sıfırla"
                        } else {
                            "Gönder"
                        });
                        (self.text_width(self.font(s), label), font * 1.3)
                    }
                    "image" => (font * 2.0, font * 1.3),
                    _ => (font * 12.0, font * 1.3),
                }
            }
        };
        let cw = avail;
        let w = s.width.px(cw);
        let h = s.height.px(self.viewport.1).filter(|_| !matches!(s.height, Length::Percent(_)));
        // En-boy oranı yalnızca görsellerde korunur; form denetiminin yüksekliği genişlikle değişmez.
        let keep_ratio = matches!(tag, "img" | "video" | "canvas" | "iframe" | "svg");
        let (mut w, mut h) = match (w, h) {
            (Some(w), Some(h)) => (w, h),
            (Some(w), None) if iw > 0.0 && keep_ratio => (w, w * ih / iw),
            (None, Some(h)) if ih > 0.0 && keep_ratio => (h * iw / ih, h),
            (Some(w), None) => (w, ih),
            (None, Some(h)) => (iw, h),
            (None, None) => (iw, ih),
        };
        if let Some(max) = s.max_width.px(cw)
            && w > max
        {
            if s.height == Length::Auto && w > 0.0 && keep_ratio {
                h *= max / w;
            }
            w = max;
        }
        if s.box_sizing_border {
            let pad = self.paddings(s, cw);
            if s.width != Length::Auto {
                w = (w - pad[1] - pad[3] - s.border[1] - s.border[3]).max(0.0);
            }
            if matches!(s.height, Length::Px(_)) {
                h = (h - pad[0] - pad[2] - s.border[0] - s.border[2]).max(0.0);
            }
        }
        (w, h)
    }

    fn replaced(&mut self, node: NodeId, s: &Style, x: f32, y: f32, w: f32, h: f32) {
        if !s.visible {
            return;
        }
        let rect = RectF { x, y, w, h };
        match self.doc.tag(node).unwrap_or("") {
            "img" => {
                if let Some(src) = self.doc.attr(node, "src") {
                    let src = String::from(src);
                    if self.images.size(&src).is_some() {
                        self.emit(Item::Image {
                            rect,
                            src,
                            radius: s.radius,
                        });
                    } else if let Some(alt) = self.doc.attr(node, "alt").filter(|a| !a.is_empty()) {
                        let alt = String::from(alt);
                        self.emit(Item::Text {
                            x,
                            y,
                            text: alt,
                            size: s.font_size,
                            bold: false,
                            color: Color(0xff707070),
                            underline: false,
                            strike: false,
                        });
                    }
                }
            }
            "input" | "button" | "select" | "textarea" => self.emit(Item::Control { rect, node }),
            "iframe" | "video" => self.emit(Item::Fill {
                rect,
                color: Color(0xffe8e8ec),
                radius: 6.0,
            }),
            _ => {}
        }
    }

    fn flex(&mut self, node: NodeId, s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let mut items: Vec<NodeId> = self.doc.nodes[node]
            .children
            .iter()
            .copied()
            .filter(|&c| match &self.doc.nodes[c].kind {
                Kind::Element { .. } => self.st(c).display != Display::None && !self.out_of_flow(c),
                Kind::Text(t) => !t.trim().is_empty(),
                _ => false,
            })
            .collect();
        items.sort_by_key(|&c| self.st(c).order);
        if !s.flex_row {
            return self.flex_column(node, s, &items, x, y, width);
        }
        // Her öğenin temel genişliği.
        let mut bases: Vec<(NodeId, f32, f32)> = Vec::new();
        for &c in &items {
            let cs = self.st(c).clone();
            let pad = self.paddings(&cs, width);
            let m = self.margins(&cs, width);
            let extra = pad[1] + pad[3] + cs.border[1] + cs.border[3];
            let basis = match (cs.flex_basis.px(width), cs.width.px(width)) {
                (Some(b), _) if cs.flex_basis != Length::Px(0.0) || cs.flex_grow == 0.0 => b,
                (Some(_), _) => 0.0,
                (None, Some(w)) => {
                    if cs.box_sizing_border {
                        (w - extra).max(0.0)
                    } else {
                        w
                    }
                }
                (None, None) => {
                    if matches!(self.doc.nodes[c].kind, Kind::Text(_)) {
                        let mut ps = Vec::new();
                        let mut sp = false;
                        self.collect(c, &mut ps, &mut sp);
                        ps.iter()
                            .map(|p| match p {
                                Piece::Word { width, .. } => *width + 4.0,
                                _ => 0.0,
                            })
                            .sum()
                    } else if matches!(self.doc.tag(c), Some("img" | "input" | "button" | "select" | "textarea")) {
                        self.replaced_size(c, &cs, width).0
                    } else {
                        self.max_content_width(c)
                    }
                }
            };
            let basis = match cs.max_width.px(width) {
                Some(max) => basis.min(max),
                None => basis,
            };
            bases.push((c, basis, extra + m[1] + m[3]));
        }
        // Satırlara böl (flex-wrap).
        let mut lines: Vec<Vec<(NodeId, f32, f32)>> = Vec::new();
        let mut current = Vec::new();
        let mut used = 0.0;
        for b in bases {
            let need = b.1 + b.2 + if current.is_empty() { 0.0 } else { s.gap };
            if s.flex_wrap && !current.is_empty() && used + need > width {
                lines.push(core::mem::take(&mut current));
                used = 0.0;
            }
            used += b.1 + b.2 + if current.is_empty() { 0.0 } else { s.gap };
            current.push(b);
        }
        if !current.is_empty() {
            lines.push(current);
        }
        let mut cursor = y;
        for line in lines {
            let gaps = s.gap * (line.len().saturating_sub(1)) as f32;
            let total: f32 = line.iter().map(|(_, b, e)| b + e).sum::<f32>() + gaps;
            let free = width - total;
            let grow: f32 = line.iter().map(|(c, ..)| self.st(*c).flex_grow).sum();
            let shrink: f32 = line
                .iter()
                .map(|(c, b, _)| self.st(*c).flex_shrink * b)
                .sum();
            let mut sizes: Vec<f32> = line
                .iter()
                .map(|(c, b, _)| {
                    let cs = self.st(*c);
                    if free > 0.0 && grow > 0.0 {
                        b + free * cs.flex_grow / grow
                    } else if free < 0.0 && shrink > 0.0 {
                        (b + free * cs.flex_shrink * b / shrink).max(0.0)
                    } else {
                        *b
                    }
                })
                .collect();
            for (i, (c, _, _)) in line.iter().enumerate() {
                if let Some(min) = self.st(*c).min_width.px(width) {
                    sizes[i] = sizes[i].max(min);
                }
            }
            let used: f32 = sizes.iter().sum::<f32>()
                + line.iter().map(|(_, _, e)| e).sum::<f32>()
                + gaps;
            let leftover = (width - used).max(0.0);
            let n = line.len() as f32;
            let (mut cx, between) = match s.justify {
                Align::Center => (x + leftover / 2.0, s.gap),
                Align::End => (x + leftover, s.gap),
                Align::SpaceBetween if n > 1.0 => (x, s.gap + leftover / (n - 1.0)),
                Align::SpaceAround => (x + leftover / n / 2.0, s.gap + leftover / n),
                Align::SpaceEvenly => (x + leftover / (n + 1.0), s.gap + leftover / (n + 1.0)),
                _ => (x, s.gap),
            };
            // Önce yükseklikleri ölç, sonra çapraz eksende hizala.
            let mut heights = Vec::new();
            for (i, (c, _, _)) in line.iter().enumerate() {
                self.measuring += 1;
                let h = self.flex_item(*c, sizes[i], 0.0, 0.0);
                self.measuring -= 1;
                heights.push(h);
            }
            let line_h = match fixed(s.height) {
                Some(h) if !s.flex_wrap => h.max(heights.iter().copied().fold(0.0, f32::max)),
                _ => heights.iter().copied().fold(0.0, f32::max),
            };
            for (i, (c, _, extra)) in line.iter().enumerate() {
                let cs = self.st(*c).clone();
                let align = cs.align_self.unwrap_or(s.align_items);
                let dy = match align {
                    Align::Center => (line_h - heights[i]) / 2.0,
                    Align::End => line_h - heights[i],
                    _ => 0.0,
                };
                let stretch = align == Align::Stretch && cs.height == Length::Auto;
                if stretch && matches!(self.doc.nodes[*c].kind, Kind::Element { .. }) {
                    let mut st = cs.clone();
                    let m = self.margins(&cs, width);
                    let pad = self.paddings(&cs, width);
                    st.min_height = Length::Px(
                        line_h - m[0] - m[2] - pad[0] - pad[2] - cs.border[0] - cs.border[2],
                    );
                    self.flex_item_styled(*c, &st, sizes[i], cx, cursor);
                } else {
                    self.flex_item(*c, sizes[i], cx, cursor + dy);
                }
                cx += sizes[i] + extra + between;
            }
            cursor += line_h + s.row_gap;
        }
        (cursor - y - s.row_gap).max(0.0)
    }

    fn flex_item(&mut self, c: NodeId, width: f32, x: f32, y: f32) -> f32 {
        let cs = self.st(c).clone();
        self.flex_item_styled(c, &cs, width, x, y)
    }

    fn flex_item_styled(&mut self, c: NodeId, cs: &Style, width: f32, x: f32, y: f32) -> f32 {
        if let Kind::Text(_) = self.doc.nodes[c].kind {
            let parent = self.doc.nodes[c].parent.unwrap_or(c);
            return self.inline(parent, &[c], x, y, width + 1.0);
        }
        if matches!(self.doc.tag(c), Some("img" | "input" | "button" | "select" | "textarea")) {
            let (w, h) = self.replaced_size(c, cs, width);
            let m = self.margins(cs, width);
            let pad = self.paddings(cs, width);
            let rect = RectF {
                x: x + m[3],
                y: y + m[0],
                w: width + pad[1] + pad[3] + cs.border[1] + cs.border[3],
                h: h + pad[0] + pad[2] + cs.border[0] + cs.border[2],
            };
            let at = self.items.len();
            self.replaced(c, cs, rect.x + cs.border[3] + pad[3], rect.y + cs.border[0] + pad[0], w.min(width), h);
            self.decorate(c, cs, rect, at, false);
            return rect.h + m[0] + m[2];
        }
        self.block_with(c, cs, width, x, y)
    }

    fn flex_column(&mut self, _node: NodeId, s: &Style, items: &[NodeId], x: f32, y: f32, width: f32) -> f32 {
        let mut cursor = y;
        for (i, &c) in items.iter().enumerate() {
            if i > 0 {
                cursor += s.row_gap.max(s.gap);
            }
            let cs = self.st(c).clone();
            let align = cs.align_self.unwrap_or(s.align_items);
            if align == Align::Stretch || matches!(self.doc.nodes[c].kind, Kind::Text(_)) {
                cursor += if matches!(self.doc.nodes[c].kind, Kind::Text(_)) {
                    self.flex_item(c, width, x, cursor)
                } else {
                    self.block(c, x, cursor, width, false)
                };
                continue;
            }
            let content = self.max_content_width(c);
            let w = self.resolve_width(&cs, width, width, Some(content));
            let pad = self.paddings(&cs, width);
            let m = self.margins(&cs, width);
            let outer = w + pad[1] + pad[3] + cs.border[1] + cs.border[3] + m[1] + m[3];
            let dx = match align {
                Align::Center => (width - outer) / 2.0,
                Align::End => width - outer,
                _ => 0.0,
            };
            cursor += self.flex_item(c, w, x + dx, cursor);
        }
        let h = cursor - y;
        if let Some(fixed) = fixed(s.height)
            && s.justify == Align::Center
            && fixed > h
        {
            return fixed;
        }
        h
    }

    fn grid(&mut self, node: NodeId, s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let items: Vec<NodeId> = self
            .doc
            .element_children(node)
            .into_iter()
            .filter(|&c| self.st(c).display != Display::None && !self.out_of_flow(c))
            .collect();
        let cols = s.grid_columns.max(1);
        let cell = (width - s.gap * (cols - 1) as f32) / cols as f32;
        let mut cursor = y;
        for row in items.chunks(cols) {
            let mut h = 0.0f32;
            for (i, &c) in row.iter().enumerate() {
                let cx = x + i as f32 * (cell + s.gap);
                h = h.max(self.block(c, cx, cursor, cell, false));
            }
            cursor += h + s.row_gap;
        }
        (cursor - y - s.row_gap).max(0.0)
    }

    fn rows_of(&self, table: NodeId) -> Vec<NodeId> {
        let mut rows = Vec::new();
        for c in self.doc.element_children(table) {
            match self.doc.tag(c) {
                Some("tr") => rows.push(c),
                Some("thead" | "tbody" | "tfoot") => {
                    rows.extend(self.doc.element_children(c).into_iter().filter(|&r| self.doc.is(r, "tr")))
                }
                _ => {}
            }
        }
        rows
    }

    fn table(&mut self, node: NodeId, s: &Style, x: f32, y: f32, width: f32) -> f32 {
        let rows = self.rows_of(node);
        let mut cursor = y;
        for caption in self.doc.element_children(node) {
            if self.doc.is(caption, "caption") {
                cursor += self.block(caption, x, cursor, width, false);
            }
        }
        // Izgara: her satırdaki hücreler (hücre, sütun, sütun aralığı).
        let grid: Vec<Vec<(NodeId, usize, usize)>> = rows
            .iter()
            .map(|&r| {
                let mut col = 0;
                self.doc
                    .element_children(r)
                    .into_iter()
                    .map(|c| {
                        let span = self
                            .doc
                            .attr(c, "colspan")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                            .unwrap_or(1)
                            .clamp(1, 64);
                        let cell = (c, col, span);
                        col += span;
                        cell
                    })
                    .collect()
            })
            .collect();
        let columns = grid
            .iter()
            .filter_map(|r| r.last().map(|&(_, col, span)| col + span))
            .max()
            .unwrap_or(0);
        if columns == 0 {
            return cursor - y;
        }
        // HTML'de varsayılan hücre aralığı 2px (`cellspacing` ya da `border-spacing`).
        let spacing = s.border_spacing.unwrap_or(2.0).max(0.0);
        let spacing_total = spacing * (columns + 1) as f32;
        // Sütunların en dar (en uzun sözcük) ve en geniş (satır kırmadan) içerik genişlikleri.
        let mut mins = alloc::vec![0.0f32; columns];
        let mut maxs = alloc::vec![0.0f32; columns];
        let mut fixed: Vec<Option<f32>> = alloc::vec![None; columns];
        let mut percent: Vec<Option<f32>> = alloc::vec![None; columns];
        let mut spanned = Vec::new();
        for row in &grid {
            for &(c, col, span) in row {
                let cs = self.st(c).clone();
                let pad = self.paddings(&cs, width);
                let extra = pad[1] + pad[3] + cs.border[1] + cs.border[3];
                let mut cmin = self.min_content_width(c) + extra;
                let mut cmax = self.max_content_width(c) + extra;
                if let Length::Px(w) = cs.width {
                    let w = if cs.box_sizing_border { w } else { w + extra };
                    cmin = cmin.max(w);
                    cmax = cmin;
                    if span == 1 {
                        fixed[col] = Some(fixed[col].map_or(cmin, |f: f32| f.max(cmin)));
                    }
                } else if let Length::Percent(p) = cs.width
                    && span == 1
                {
                    percent[col] = Some(percent[col].map_or(p, |q: f32| q.max(p)));
                }
                cmax = cmax.max(cmin);
                if span == 1 {
                    mins[col] = mins[col].max(cmin);
                    maxs[col] = maxs[col].max(cmax);
                } else {
                    spanned.push((col, span, cmin, cmax));
                }
            }
        }
        // Birden çok sütunu kaplayan hücrelerin eksiği sütunlara eşit dağıtılır.
        for (col, span, cmin, cmax) in spanned {
            let end = (col + span).min(columns);
            let n = (end - col) as f32;
            let gaps = spacing * (end - col - 1) as f32;
            let have_min: f32 = mins[col..end].iter().sum::<f32>() + gaps;
            if cmin > have_min {
                mins[col..end].iter_mut().for_each(|m| *m += (cmin - have_min) / n);
            }
            let have_max: f32 = maxs[col..end].iter().sum::<f32>() + gaps;
            if cmax > have_max {
                maxs[col..end].iter_mut().for_each(|m| *m += (cmax - have_max) / n);
            }
        }
        for i in 0..columns {
            maxs[i] = maxs[i].max(mins[i]);
        }
        let sum_min: f32 = mins.iter().sum();
        let sum_max: f32 = maxs.iter().sum();
        let table_w = if self.st(node).width != Length::Auto {
            width.max(sum_min + spacing_total)
        } else {
            (sum_max + spacing_total).min(width).max(sum_min + spacing_total)
        };
        let inner = table_w - spacing_total;
        // Önce en dar genişlikler; verilmiş (px / %) sütunlar hedeflerine çıkar.
        let mut widths = mins.clone();
        let mut auto = alloc::vec![true; columns];
        for i in 0..columns {
            if let Some(p) = percent[i] {
                widths[i] = widths[i].max(inner * p / 100.0);
                auto[i] = false;
            } else if let Some(f) = fixed[i] {
                widths[i] = widths[i].max(f);
                auto[i] = false;
            }
        }
        let mut extra = inner - widths.iter().sum::<f32>();
        if extra > 0.0 {
            // Kendiliğinden sütunlar en geniş içeriklerine doğru büyür.
            let room: f32 = (0..columns).filter(|&i| auto[i]).map(|i| (maxs[i] - widths[i]).max(0.0)).sum();
            if room > 0.0 {
                let k = (extra / room).min(1.0);
                for i in (0..columns).filter(|&i| auto[i]) {
                    let add = (maxs[i] - widths[i]).max(0.0) * k;
                    widths[i] += add;
                    extra -= add;
                }
            }
            // Hâlâ yer varsa (genişliği verilmiş tablo) en geniş içerik oranında dağıt.
            if extra > 0.5 {
                let targets: Vec<usize> = if auto.iter().any(|&a| a) {
                    (0..columns).filter(|&i| auto[i]).collect()
                } else {
                    (0..columns).collect()
                };
                let weight: f32 = targets.iter().map(|&i| maxs[i]).sum();
                for &i in &targets {
                    widths[i] += if weight > 0.0 {
                        extra * maxs[i] / weight
                    } else {
                        extra / targets.len() as f32
                    };
                }
            }
        }
        cursor += spacing;
        for (r, cells) in rows.iter().zip(&grid) {
            cursor += self.table_row_cells(*r, cells, x, cursor, table_w, &widths, spacing) + spacing;
        }
        cursor - y
    }

    /// Satır kırmadan sığmayan en dar genişlik: en uzun sözcük ya da satır içi kutu.
    fn min_content_width(&mut self, node: NodeId) -> f32 {
        let s = self.st(node).clone();
        if s.nowrap || s.white_space_pre {
            return self.max_content_width(node);
        }
        if let Length::Px(w) = s.width {
            return w;
        }
        let tag = self.doc.tag(node).unwrap_or("");
        if matches!(tag, "img" | "input" | "button" | "select" | "textarea") {
            return self.replaced_size(node, &s, self.cb_width).0;
        }
        let mut best = 0.0f32;
        let mut pieces = Vec::new();
        let mut space = false;
        for child in self.doc.nodes[node].children.clone() {
            if self.is_block_level(child) {
                let cs = self.st(child).clone();
                let pad = self.paddings(&cs, 0.0);
                let cw = self.min_content_width(child);
                best = best.max(cw + pad[1] + pad[3] + cs.border[1] + cs.border[3]);
            } else {
                self.collect(child, &mut pieces, &mut space);
            }
        }
        for p in &pieces {
            match p {
                Piece::Word { width, .. } => best = best.max(*width),
                Piece::Atomic { width, margin, .. } => best = best.max(width + margin[1] + margin[3]),
                Piece::Break => {}
            }
        }
        ceilf(best + 0.5)
    }

    fn table_row(&mut self, row: NodeId, x: f32, y: f32, width: f32, widths: &[f32]) -> f32 {
        let cells: Vec<(NodeId, usize, usize)> = self
            .doc
            .element_children(row)
            .into_iter()
            .enumerate()
            .map(|(i, c)| (c, i, 1))
            .collect();
        self.table_row_cells(row, &cells, x, y, width, widths, 0.0)
    }

    #[allow(clippy::too_many_arguments)]
    fn table_row_cells(
        &mut self,
        row: NodeId,
        cells: &[(NodeId, usize, usize)],
        x: f32,
        y: f32,
        width: f32,
        widths: &[f32],
        spacing: f32,
    ) -> f32 {
        let n = cells.len().max(1);
        // Hücrenin konumu ve genişliği (sütun aralığı + aradaki boşluklar).
        let place = |col: usize, span: usize| -> (f32, f32) {
            if widths.is_empty() {
                let w = width / n as f32;
                return (x + w * col as f32, w);
            }
            let end = (col + span).min(widths.len());
            let left: f32 = widths[..col.min(widths.len())].iter().sum();
            let w: f32 = widths[col.min(widths.len())..end].iter().sum::<f32>() + spacing * (end.saturating_sub(col + 1)) as f32;
            (x + spacing + left + spacing * col as f32, w)
        };
        let rs = self.st(row).clone();
        let mut h = fixed(rs.height).unwrap_or(0.0);
        let mut heights = Vec::new();
        for &(c, col, span) in cells {
            let (_, w) = place(col, span);
            self.measuring += 1;
            let ch = self.block(c, 0.0, 0.0, w, false);
            self.measuring -= 1;
            heights.push(ch);
            h = h.max(ch);
        }
        let at = self.items.len();
        for (i, &(c, col, span)) in cells.iter().enumerate() {
            let (cx, w) = place(col, span);
            let mut cs = self.st(c).clone();
            cs.min_height = Length::Px(h - cs.padding[0].or_zero(w) - cs.padding[2].or_zero(w) - cs.border[0] - cs.border[2]);
            cs.margin = [Length::Px(0.0); 4];
            let pad = self.paddings(&cs, w);
            let inner = (w - pad[1] - pad[3] - cs.border[1] - cs.border[3]).max(0.0);
            let dy = if cs.vertical_middle { (h - heights[i]) / 2.0 } else { 0.0 };
            let before = self.items.len();
            self.block_with(c, &cs, inner, cx, y);
            if dy > 0.0 {
                self.shift(before, 0.0, dy);
            }
        }
        if rs.background.alpha() > 0 && self.measuring == 0 {
            self.items.insert(
                at,
                Item::Fill {
                    rect: RectF { x, y, w: width, h },
                    color: rs.background,
                    radius: 0.0,
                },
            );
        }
        if self.measuring == 0 {
            self.boxes.push((row, RectF { x, y, w: width, h }));
        }
        h
    }
}

/// Yalnızca kesin (piksel) uzunluk; yüzde yükseklikler üst öğe yüksekliği belirsiz
/// olduğundan `auto` sayılır.
fn fixed(l: Length) -> Option<f32> {
    match l {
        Length::Px(v) => Some(v),
        _ => None,
    }
}

fn transform(text: &str, mode: u8) -> String {
    match mode {
        1 => text.chars().flat_map(|c| c.to_uppercase()).collect(),
        2 => text.chars().flat_map(|c| c.to_lowercase()).collect(),
        3 => {
            let mut out = String::new();
            let mut start = true;
            for c in text.chars() {
                if start && c.is_alphabetic() {
                    out.extend(c.to_uppercase());
                    start = false;
                } else {
                    out.push(c);
                    if c.is_whitespace() {
                        start = true;
                    }
                }
            }
            out
        }
        _ => String::from(text),
    }
}
