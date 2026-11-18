//! Hesaplanmış stil: varsayılan (tarayıcı) stil sayfası + sayfa stil sayfaları + satır içi
//! `style` özniteliği, özgüllük ve `!important` sırasıyla uygulanır; yazı rengi, boyutu,
//! hizası gibi özellikler ebeveynden kalıtılır. Birimler px'e çevrilir (em, rem, %, vw, vh,
//! pt); yüzdeler yerleşimde çözülsün diye ayrı tutulur.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

use super::css::{self, Color, Declaration, Sheet};
use super::dom::{Document, Kind, NodeId};

pub const UA_SHEET: &str = r#"
html, address, blockquote, body, center, dd, details, dialog, dir, div, dl, dt, fieldset,
figcaption, figure, footer, form, h1, h2, h3, h4, h5, h6, header, hgroup, hr, legend, main,
menu, nav, ol, p, pre, section, summary, ul, article, aside, search { display: block; }
head, script, style, link, meta, title, template, noscript, [hidden], datalist, dialog:not([open]) { display: none; }
body { margin: 8px; line-height: 1.35; }
p, blockquote, figure, dl, pre { margin-top: 1em; margin-bottom: 1em; }
h1 { font-size: 2em; margin: 0.67em 0; font-weight: bold; }
h2 { font-size: 1.5em; margin: 0.83em 0; font-weight: bold; }
h3 { font-size: 1.17em; margin: 1em 0; font-weight: bold; }
h4 { margin: 1.33em 0; font-weight: bold; }
h5 { font-size: 0.83em; margin: 1.67em 0; font-weight: bold; }
h6 { font-size: 0.67em; margin: 2.33em 0; font-weight: bold; }
b, strong, th { font-weight: bold; }
i, em, cite, var, dfn { font-style: italic; }
u, ins { text-decoration: underline; }
s, strike, del { text-decoration: line-through; }
small { font-size: 0.83em; }
big { font-size: 1.17em; }
sub, sup { font-size: 0.75em; }
code, kbd, samp, tt { font-family: monospace; }
pre { white-space: pre; font-family: monospace; }
a { color: #0b57d0; text-decoration: underline; cursor: pointer; }
ul, ol, menu { padding-left: 40px; margin-top: 1em; margin-bottom: 1em; }
ul ul, ol ul, ul ol, ol ol { margin-top: 0; margin-bottom: 0; }
li { display: list-item; }
ol { list-style-type: decimal; }
ul { list-style-type: disc; }
dd { margin-left: 40px; }
blockquote { margin-left: 40px; margin-right: 40px; }
hr { border-top: 1px solid #c8c8c8; margin: 0.5em 0; }
center { text-align: center; }
table { display: table; text-align: left; }
tr { display: table-row; }
td, th { display: table-cell; padding: 1px; }
thead, tbody, tfoot { display: block; }
caption { display: block; text-align: center; }
th { text-align: center; }
img, video, canvas, svg, iframe { display: inline-block; }
input, button, select, textarea { display: inline-block; font-size: 13.3px; }
input, select, textarea { border: 1px solid #8f8f9d; background-color: #ffffff; padding: 3px 5px; border-radius: 4px; }
button, input[type=submit], input[type=button], input[type=reset] { border: 1px solid #8f8f9d; background-color: #e9e9ed; padding: 3px 8px; border-radius: 4px; }
input[type=hidden] { display: none; }
input[type=checkbox], input[type=radio] { border: none; padding: 0; background-color: transparent; border-radius: 0; margin: 3px 4px 3px 4px; }
textarea { white-space: pre; }
fieldset { border: 1px solid #c0c0c0; padding: 6px 10px; margin: 0 2px; }
mark { background-color: #ffff00; }
summary { display: list-item; }
"#;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Length {
    Auto,
    Px(f32),
    Percent(f32),
}

impl Length {
    pub fn px(self, of: f32) -> Option<f32> {
        match self {
            Length::Px(v) => Some(v),
            Length::Percent(p) => Some(of * p / 100.0),
            Length::Auto => None,
        }
    }

    pub fn or_zero(self, of: f32) -> f32 {
        self.px(of).unwrap_or(0.0)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Display {
    None,
    Block,
    Inline,
    InlineBlock,
    ListItem,
    Flex,
    InlineFlex,
    Grid,
    Table,
    TableRow,
    TableCell,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Position {
    Static,
    Relative,
    Absolute,
    Fixed,
    Sticky,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Align {
    Start,
    Center,
    End,
    Justify,
    SpaceBetween,
    SpaceAround,
    SpaceEvenly,
    Stretch,
}

#[derive(Clone, Debug)]
pub struct Style {
    pub display: Display,
    pub position: Position,
    pub color: Color,
    pub background: Color,
    /// `background-image: url(...)`
    pub background_image: Option<String>,
    pub font_size: f32,
    pub bold: bool,
    pub italic: bool,
    pub monospace: bool,
    pub underline: bool,
    pub line_through: bool,
    pub text_align: Align,
    pub text_transform: u8,
    pub white_space_pre: bool,
    pub nowrap: bool,
    pub line_height: f32,
    pub margin: [Length; 4],
    pub padding: [Length; 4],
    pub border: [f32; 4],
    pub border_color: [Color; 4],
    pub radius: f32,
    pub width: Length,
    pub height: Length,
    pub min_width: Length,
    pub max_width: Length,
    pub min_height: Length,
    pub max_height: Length,
    pub box_sizing_border: bool,
    pub offsets: [Length; 4],
    pub flex_row: bool,
    pub flex_wrap: bool,
    pub justify: Align,
    pub align_items: Align,
    pub align_self: Option<Align>,
    pub gap: f32,
    pub row_gap: f32,
    pub flex_grow: f32,
    pub flex_shrink: f32,
    pub flex_basis: Length,
    pub order: i32,
    pub grid_columns: usize,
    pub list_style: u8,
    pub visible: bool,
    pub opacity: f32,
    pub overflow_hidden: bool,
    pub z_index: i32,
    pub vertical_middle: bool,
    /// Tablo hücre aralığı (`border-spacing`, `cellspacing`); yoksa 2px.
    pub border_spacing: Option<f32>,
    pub float: u8,
    pub clear: bool,
    /// Kalıtılan CSS değişkenleri (`--ad`). Ebeveynle paylaşılır; öğe yeni bir değişken
    /// tanımlayınca kopyalanır (her öğede tüm değişkenleri kopyalamak yüzlerce MB tutuyordu).
    pub vars: Rc<BTreeMap<String, String>>,
}

impl Style {
    pub fn root(viewport_width: f32) -> Self {
        let _ = viewport_width;
        Self {
            display: Display::Block,
            position: Position::Static,
            color: Color(0xff1f1f1f),
            background: Color::TRANSPARENT,
            background_image: None,
            font_size: 16.0,
            bold: false,
            italic: false,
            monospace: false,
            underline: false,
            line_through: false,
            text_align: Align::Start,
            text_transform: 0,
            white_space_pre: false,
            nowrap: false,
            line_height: 1.25,
            margin: [Length::Px(0.0); 4],
            padding: [Length::Px(0.0); 4],
            border: [0.0; 4],
            border_color: [Color(0xff000000); 4],
            radius: 0.0,
            width: Length::Auto,
            height: Length::Auto,
            min_width: Length::Auto,
            max_width: Length::Auto,
            min_height: Length::Auto,
            max_height: Length::Auto,
            box_sizing_border: false,
            offsets: [Length::Auto; 4],
            flex_row: true,
            flex_wrap: false,
            justify: Align::Start,
            align_items: Align::Stretch,
            align_self: None,
            gap: 0.0,
            row_gap: 0.0,
            flex_grow: 0.0,
            flex_shrink: 1.0,
            flex_basis: Length::Auto,
            order: 0,
            grid_columns: 1,
            list_style: 1,
            visible: true,
            opacity: 1.0,
            overflow_hidden: false,
            z_index: 0,
            vertical_middle: false,
            border_spacing: None,
            float: 0,
            clear: false,
            vars: Rc::new(BTreeMap::new()),
        }
    }

    /// Kalıtılan özellikler korunur, diğerleri varsayılana döner.
    fn inherit(&self) -> Self {
        let mut s = Self::root(0.0);
        s.color = self.color;
        s.font_size = self.font_size;
        s.bold = self.bold;
        s.italic = self.italic;
        s.monospace = self.monospace;
        s.text_align = self.text_align;
        s.text_transform = self.text_transform;
        s.white_space_pre = self.white_space_pre;
        s.nowrap = self.nowrap;
        s.line_height = self.line_height;
        s.list_style = self.list_style;
        s.visible = self.visible;
        s.vars = Rc::clone(&self.vars);
        s.display = Display::Inline;
        s
    }
}

pub struct Context {
    pub viewport: (f32, f32),
    pub root_font: f32,
}

/// Belgedeki her düğümün stili (dizin = düğüm numarası).
pub fn compute(doc: &Document, sheets: &[Sheet], ctx: &Context) -> Vec<Style> {
    let ua = ua_sheet();
    let mut out: Vec<Option<Style>> = alloc::vec![None; doc.nodes.len()];
    let mut root = Style::root(ctx.viewport.0);
    root.display = Display::Block;
    // Kural listesi: (özgüllük, sıra, kural) — bir kez düzleştirilir.
    let mut rules: Vec<(&css::Selector, &Vec<Declaration>, u32)> = Vec::new();
    let mut order = 0;
    // UA kuralları özgüllükten bağımsız olarak sayfanın kurallarından önce gelir (köken sırası).
    let ua_rules = ua.rules.len() as u32;
    for sheet in core::iter::once(ua).chain(sheets.iter()) {
        for rule in &sheet.rules {
            if let Some((min, max)) = rule.media {
                let w = ctx.viewport.0;
                if min.is_some_and(|m| w < m) || max.is_some_and(|m| w > m) {
                    continue;
                }
            }
            for sel in &rule.selectors {
                rules.push((sel, &rule.declarations, order));
            }
            order += 1;
        }
    }
    // Kural kovaları: seçicinin en sağdaki parçasındaki kimlik/sınıf/etiket anahtarına göre.
    let mut buckets: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (i, (sel, ..)) in rules.iter().enumerate() {
        let key = sel.parts[0]
            .0
            .iter()
            .find_map(|s| match s {
                css::Simple::Id(v) => Some(alloc::format!("#{}", v)),
                css::Simple::Class(v) => Some(alloc::format!(".{}", v)),
                css::Simple::Tag(v) => Some(v.clone()),
                _ => None,
            })
            .unwrap_or_else(|| String::from("*"));
        buckets.entry(key).or_default().push(i);
    }
    let mut stack: Vec<(NodeId, Style)> = alloc::vec![(super::dom::ROOT, root)];
    while let Some((node, parent_style)) = stack.pop() {
        for &child in doc.nodes[node].children.iter().rev() {
            let style = match &doc.nodes[child].kind {
                Kind::Element { .. } => {
                    let mut candidates: Vec<usize> = Vec::new();
                    let mut add = |key: &str| {
                        if let Some(list) = buckets.get(key) {
                            candidates.extend_from_slice(list);
                        }
                    };
                    add("*");
                    if let Some(tag) = doc.tag(child) {
                        add(tag);
                    }
                    if let Some(id) = doc.attr(child, "id") {
                        add(&alloc::format!("#{}", id));
                    }
                    for class in doc.classes(child) {
                        add(&alloc::format!(".{}", class));
                    }
                    candidates.sort_unstable();
                    candidates.dedup();
                    element_style(doc, child, &parent_style, &rules, &candidates, ua_rules, ctx)
                }
                _ => {
                    let mut s = parent_style.inherit();
                    s.display = Display::Inline;
                    s
                }
            };
            stack.push((child, style.clone()));
            out[child] = Some(style);
        }
        if out[node].is_none() {
            out[node] = Some(parent_style);
        }
    }
    out.into_iter()
        .map(|s| s.unwrap_or_else(|| Style::root(ctx.viewport.0)))
        .collect()
}

fn ua_sheet() -> &'static Sheet {
    static mut UA: Option<Sheet> = None;
    // Tek iş parçacıklı uygulama: ilk çağrıda bir kez ayrıştırılır.
    unsafe {
        let slot = &mut *core::ptr::addr_of_mut!(UA);
        slot.get_or_insert_with(|| css::parse_sheet(UA_SHEET))
    }
}

fn element_style(
    doc: &Document,
    node: NodeId,
    parent: &Style,
    rules: &[(&css::Selector, &Vec<Declaration>, u32)],
    candidates: &[usize],
    ua_rules: u32,
    ctx: &Context,
) -> Style {
    let mut matched: Vec<(u32, u32, &Declaration, bool)> = Vec::new();
    for &i in candidates {
        let (sel, decls, order) = &rules[i];
        if css::matches(doc, node, sel) {
            for d in decls.iter() {
                matched.push((sel.specificity, *order, d, d.important));
            }
        }
    }
    let inline = doc
        .attr(node, "style")
        .map(css::parse_declarations)
        .unwrap_or_default();
    for d in &inline {
        matched.push((1 << 24, u32::MAX, d, d.important));
    }
    matched.sort_by_key(|(spec, order, _, important)| (*important, *order >= ua_rules, *spec, *order));
    let mut style = parent.inherit();
    // Eski öznitelikler (bgcolor, align, width) en düşük öncelikte.
    presentational(doc, node, &mut style);
    // Önce değişkenler, sonra diğer özellikler (değişkenler aynı öğede kullanılabilir).
    for (_, _, d, _) in &matched {
        if d.name.starts_with("--") && style.vars.get(&d.name) != Some(&d.value) {
            Rc::make_mut(&mut style.vars).insert(d.name.clone(), d.value.clone());
        }
    }
    for (_, _, d, _) in &matched {
        if !d.name.starts_with("--") {
            let value = substitute_vars(&d.value, &style.vars);
            apply(&mut style, parent, &d.name, &value, ctx);
        }
    }
    if matches!(style.position, Position::Absolute | Position::Fixed)
        && matches!(style.display, Display::Inline)
    {
        style.display = Display::Block;
    }
    if style.float != 0 && style.display == Display::Inline {
        style.display = Display::InlineBlock;
    }
    style
}

fn presentational(doc: &Document, node: NodeId, s: &mut Style) {
    if let Some(c) = doc.attr(node, "bgcolor").and_then(css::parse_color) {
        s.background = c;
    }
    if let Some(c) = doc
        .attr(node, "color")
        .filter(|_| doc.is(node, "font"))
        .and_then(css::parse_color)
    {
        s.color = c;
    }
    if doc.is(node, "table") {
        if let Some(v) = doc.attr(node, "cellspacing").and_then(|v| v.trim().trim_end_matches("px").parse().ok()) {
            s.border_spacing = Some(v);
        }
        // <table align=center>: tablo ortalanır, hücre metni değil.
        if doc.attr(node, "align").is_some_and(|a| a.eq_ignore_ascii_case("center")) {
            s.margin[1] = Length::Auto;
            s.margin[3] = Length::Auto;
        }
    }
    match doc.attr(node, "align").map(|a| a.to_ascii_lowercase()).filter(|_| !doc.is(node, "table")) {
        Some(a) if a == "center" => s.text_align = Align::Center,
        Some(a) if a == "right" => s.text_align = Align::End,
        _ => {}
    }
    for (attr, slot) in [("width", 0), ("height", 1)] {
        if let Some(v) = doc.attr(node, attr) {
            let len = if let Some(p) = v.strip_suffix('%') {
                p.trim().parse().ok().map(Length::Percent)
            } else {
                v.trim().trim_end_matches("px").parse().ok().map(Length::Px)
            };
            if let Some(len) = len {
                if slot == 0 {
                    s.width = len;
                } else {
                    s.height = len;
                }
            }
        }
    }
}

fn substitute_vars(value: &str, vars: &BTreeMap<String, String>) -> String {
    let mut out = String::from(value);
    for _ in 0..8 {
        let Some(start) = out.find("var(") else { break };
        let mut depth = 0;
        let mut end = out.len();
        for (i, c) in out[start..].char_indices() {
            if c == '(' {
                depth += 1;
            } else if c == ')' {
                depth -= 1;
                if depth == 0 {
                    end = start + i;
                    break;
                }
            }
        }
        let inner = &out[start + 4..end];
        let (name, fallback) = match inner.split_once(',') {
            Some((n, f)) => (n.trim(), Some(f.trim())),
            None => (inner.trim(), None),
        };
        let replacement = vars
            .get(name)
            .map(|s| s.as_str())
            .or(fallback)
            .unwrap_or("")
            .trim()
            .to_owned_string();
        out.replace_range(start..(end + 1).min(out.len()), &replacement);
    }
    out
}

trait OwnedString {
    fn to_owned_string(&self) -> String;
}

impl OwnedString for str {
    fn to_owned_string(&self) -> String {
        String::from(self)
    }
}

/// Uzunluk değeri; `font` göreli birimler için öğenin yazı boyutudur.
pub fn length(v: &str, font: f32, ctx: &Context) -> Option<Length> {
    let v = v.trim();
    if v == "auto" || v.is_empty() {
        return Some(Length::Auto);
    }
    if let Some(inner) = v
        .strip_prefix("calc(")
        .or_else(|| v.strip_prefix("min("))
        .or_else(|| v.strip_prefix("max("))
        .or_else(|| v.strip_prefix("clamp("))
    {
        // calc/min/max/clamp: ilk anlamlı uzunluk kullanılır (yaklaşık).
        let inner = inner.trim_end_matches(')');
        return inner
            .split([',', ' '])
            .find_map(|t| length(t, font, ctx).filter(|l| *l != Length::Auto));
    }
    let split = v
        .find(|c: char| !(c.is_ascii_digit() || c == '.' || c == '-' || c == '+'))
        .unwrap_or(v.len());
    let number: f32 = v[..split].parse().ok()?;
    Some(match &v[split..] {
        "" | "px" => Length::Px(number),
        "%" => Length::Percent(number),
        "em" => Length::Px(number * font),
        "rem" => Length::Px(number * ctx.root_font),
        "pt" => Length::Px(number * 4.0 / 3.0),
        "vw" | "svw" | "dvw" => Length::Px(number * ctx.viewport.0 / 100.0),
        "vh" | "svh" | "dvh" => Length::Px(number * ctx.viewport.1 / 100.0),
        "vmin" => Length::Px(number * ctx.viewport.0.min(ctx.viewport.1) / 100.0),
        "vmax" => Length::Px(number * ctx.viewport.0.max(ctx.viewport.1) / 100.0),
        "ch" | "ex" => Length::Px(number * font * 0.5),
        "fr" => Length::Px(number),
        _ => return None,
    })
}

fn px(v: &str, font: f32, ctx: &Context) -> Option<f32> {
    match length(v, font, ctx)? {
        Length::Px(p) => Some(p),
        _ => None,
    }
}

/// 1-4 değerli kısaltma (üst, sağ, alt, sol).
fn four<T: Copy>(values: &[T]) -> Option<[T; 4]> {
    Some(match values.len() {
        1 => [values[0]; 4],
        2 => [values[0], values[1], values[0], values[1]],
        3 => [values[0], values[1], values[2], values[1]],
        4 => [values[0], values[1], values[2], values[3]],
        _ => return None,
    })
}

fn tokens(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut start = None;
    for (i, c) in v.char_indices() {
        match c {
            '(' => {
                depth += 1;
                start.get_or_insert(i);
            }
            ')' => depth -= 1,
            c if c.is_whitespace() && depth == 0 => {
                if let Some(s) = start.take() {
                    out.push(&v[s..i]);
                }
            }
            _ => {
                start.get_or_insert(i);
            }
        }
    }
    if let Some(s) = start {
        out.push(&v[s..]);
    }
    out
}

fn side(name: &str) -> Option<usize> {
    if name.ends_with("-top") || name.ends_with("-block-start") {
        Some(0)
    } else if name.ends_with("-right") || name.ends_with("-inline-end") {
        Some(1)
    } else if name.ends_with("-bottom") || name.ends_with("-block-end") {
        Some(2)
    } else if name.ends_with("-left") || name.ends_with("-inline-start") {
        Some(3)
    } else {
        None
    }
}

fn align(v: &str) -> Option<Align> {
    Some(match v.trim() {
        "flex-start" | "start" | "left" | "top" | "self-start" | "baseline" | "normal" => {
            Align::Start
        }
        "center" => Align::Center,
        "flex-end" | "end" | "right" | "bottom" | "self-end" => Align::End,
        "justify" => Align::Justify,
        "space-between" => Align::SpaceBetween,
        "space-around" => Align::SpaceAround,
        "space-evenly" => Align::SpaceEvenly,
        "stretch" => Align::Stretch,
        _ => return None,
    })
}

fn apply(s: &mut Style, parent: &Style, name: &str, value: &str, ctx: &Context) {
    let v = value.trim();
    let lower = v.to_ascii_lowercase();
    if lower == "inherit" {
        match name {
            "color" => s.color = parent.color,
            "font-size" => s.font_size = parent.font_size,
            "background-color" => s.background = parent.background,
            _ => {}
        }
        return;
    }
    let font = s.font_size;
    match name {
        "display" => {
            s.display = match lower.split_whitespace().next().unwrap_or("") {
                "none" => Display::None,
                "block" | "flow-root" | "contents" => Display::Block,
                "inline" => Display::Inline,
                "inline-block" | "inline-table" => Display::InlineBlock,
                "list-item" => Display::ListItem,
                "flex" => Display::Flex,
                "inline-flex" => Display::InlineFlex,
                "grid" | "inline-grid" => Display::Grid,
                "table" => Display::Table,
                "table-row" => Display::TableRow,
                "table-cell" => Display::TableCell,
                "table-row-group" | "table-header-group" | "table-footer-group" => Display::Block,
                _ => s.display,
            }
        }
        "position" => {
            s.position = match lower.as_str() {
                "relative" => Position::Relative,
                "absolute" => Position::Absolute,
                "fixed" => Position::Fixed,
                "sticky" => Position::Sticky,
                _ => Position::Static,
            }
        }
        "top" | "right" | "bottom" | "left" => {
            if let (Some(i), Some(l)) = (side(&alloc::format!("-{}", name)), length(v, font, ctx)) {
                s.offsets[i] = l;
            }
        }
        "inset" => {
            let ls: Vec<Length> = tokens(v).iter().filter_map(|t| length(t, font, ctx)).collect();
            if let Some(f) = four(&ls) {
                s.offsets = f;
            }
        }
        "color" => {
            if let Some(c) = css::parse_color(v) {
                s.color = c;
            }
        }
        "background-color" => {
            if let Some(c) = css::parse_color(v) {
                s.background = c;
            }
        }
        "background" | "background-image" => {
            for t in tokens(v) {
                if let Some(c) = css::parse_color(t) {
                    s.background = c;
                } else if let Some(url) = t.strip_prefix("url(") {
                    s.background_image =
                        Some(String::from(url.trim_end_matches(')').trim_matches(['"', '\''])));
                } else if t.contains("gradient(") {
                    // Degrade: ilk renk durağı düz renk olarak.
                    let inner = &t[t.find('(').unwrap_or(0) + 1..];
                    if let Some(c) = inner
                        .split(',')
                        .find_map(|p| tokens(p.trim()).into_iter().find_map(css::parse_color))
                    {
                        s.background = c;
                    }
                }
            }
            if lower == "none" || lower == "transparent" {
                s.background = Color::TRANSPARENT;
                s.background_image = None;
            }
        }
        "font-size" => {
            let size = match lower.as_str() {
                "xx-small" => Some(9.0),
                "x-small" => Some(10.0),
                "small" => Some(13.0),
                "medium" => Some(16.0),
                "large" => Some(18.0),
                "x-large" => Some(24.0),
                "xx-large" => Some(32.0),
                "smaller" => Some(parent.font_size * 0.83),
                "larger" => Some(parent.font_size * 1.2),
                _ => match length(v, parent.font_size, ctx) {
                    Some(Length::Px(p)) => Some(p),
                    Some(Length::Percent(p)) => Some(parent.font_size * p / 100.0),
                    _ => None,
                },
            };
            if let Some(size) = size {
                s.font_size = size.clamp(4.0, 200.0);
            }
        }
        "font-weight" => {
            s.bold = match lower.as_str() {
                "bold" | "bolder" => true,
                "normal" | "lighter" => false,
                n => n.parse::<u32>().map_or(s.bold, |w| w >= 600),
            }
        }
        "font-style" => s.italic = lower.contains("italic") || lower.contains("oblique"),
        "font-family" => s.monospace = lower.contains("mono") || lower.contains("courier"),
        "font" => {
            for t in tokens(v) {
                let t = t.split('/').next().unwrap_or(t);
                if t == "bold" || t.parse::<u32>().is_ok_and(|w| w >= 600) {
                    s.bold = true;
                } else if let Some(Length::Px(p)) = length(t, parent.font_size, ctx)
                    && t.chars().any(|c| c.is_alphabetic() || c == '%') {
                        s.font_size = p;
                    }
            }
        }
        "text-decoration" | "text-decoration-line" => {
            s.underline = lower.contains("underline");
            s.line_through = lower.contains("line-through");
        }
        "text-align" => {
            if let Some(a) = align(&lower) {
                s.text_align = a;
            }
        }
        "text-transform" => {
            s.text_transform = match lower.as_str() {
                "uppercase" => 1,
                "lowercase" => 2,
                "capitalize" => 3,
                _ => 0,
            }
        }
        "white-space" | "white-space-collapse" => {
            s.white_space_pre = lower.starts_with("pre");
            s.nowrap = lower == "nowrap" || lower == "pre";
        }
        "line-height" => {
            if lower == "normal" {
                s.line_height = 1.25;
            } else if let Ok(n) = lower.parse::<f32>() {
                s.line_height = n;
            } else if let Some(l) = length(v, font, ctx)
                && let Some(p) = l.px(font * 100.0) {
                    s.line_height = (p / font.max(1.0)).clamp(0.5, 4.0);
                }
        }
        "margin" | "padding" => {
            let ls: Vec<Length> = tokens(v).iter().filter_map(|t| length(t, font, ctx)).collect();
            if let Some(f) = four(&ls) {
                if name == "margin" {
                    s.margin = f;
                } else {
                    s.padding = f;
                }
            }
        }
        "margin-inline" | "padding-inline" | "margin-block" | "padding-block" => {
            let ls: Vec<Length> = tokens(v).iter().filter_map(|t| length(t, font, ctx)).collect();
            let (a, b) = match ls.as_slice() {
                [x] => (*x, *x),
                [x, y] => (*x, *y),
                _ => return,
            };
            let target = if name.starts_with("margin") {
                &mut s.margin
            } else {
                &mut s.padding
            };
            if name.ends_with("inline") {
                target[3] = a;
                target[1] = b;
            } else {
                target[0] = a;
                target[2] = b;
            }
        }
        n if (n.starts_with("margin-") || n.starts_with("padding-")) && side(n).is_some() => {
            if let Some(l) = length(v, font, ctx) {
                let i = side(n).unwrap();
                if n.starts_with("margin") {
                    s.margin[i] = l;
                } else {
                    s.padding[i] = l;
                }
            }
        }
        "border" | "border-top" | "border-right" | "border-bottom" | "border-left" | "outline" => {
            if name == "outline" {
                return;
            }
            let mut width = None;
            let mut color = None;
            let mut none = false;
            for t in tokens(v) {
                match t {
                    "none" | "hidden" => none = true,
                    "thin" => width = Some(1.0),
                    "medium" => width = Some(3.0),
                    "thick" => width = Some(5.0),
                    "solid" | "dashed" | "dotted" | "double" | "groove" | "ridge" | "inset"
                    | "outset" => {
                        width.get_or_insert(3.0);
                    }
                    t => {
                        if let Some(c) = css::parse_color(t) {
                            color = Some(c);
                        } else if let Some(p) = px(t, font, ctx) {
                            width = Some(p);
                        }
                    }
                }
            }
            let sides: &[usize] = match side(name) {
                Some(i) => &[i][..],
                None => &[0, 1, 2, 3][..],
            };
            let sides: Vec<usize> = sides.to_vec();
            for i in sides {
                s.border[i] = if none { 0.0 } else { width.unwrap_or(3.0) };
                if let Some(c) = color {
                    s.border_color[i] = c;
                } else {
                    s.border_color[i] = s.color;
                }
            }
        }
        "border-width" => {
            let ws: Vec<f32> = tokens(v).iter().filter_map(|t| px(t, font, ctx)).collect();
            if let Some(f) = four(&ws) {
                s.border = f;
            }
        }
        "border-color" => {
            let cs: Vec<Color> = tokens(v).iter().filter_map(|t| css::parse_color(t)).collect();
            if let Some(f) = four(&cs) {
                s.border_color = f;
            }
        }
        "border-style" => {
            if lower == "none" || lower == "hidden" {
                s.border = [0.0; 4];
            }
        }
        n if n.starts_with("border-") && n.ends_with("-width") => {
            if let (Some(i), Some(p)) = (side(&n[..n.len() - 6]), px(v, font, ctx)) {
                s.border[i] = p;
            }
        }
        n if n.starts_with("border-") && n.ends_with("-color") => {
            if let (Some(i), Some(c)) = (side(&n[..n.len() - 6]), css::parse_color(v)) {
                s.border_color[i] = c;
            }
        }
        "border-radius" => {
            if let Some(t) = tokens(v).first() {
                match length(t.split('/').next().unwrap_or(t), font, ctx) {
                    Some(Length::Px(p)) => s.radius = p,
                    Some(Length::Percent(p)) if p >= 50.0 => s.radius = 9999.0,
                    _ => {}
                }
            }
        }
        "width" | "height" | "min-width" | "max-width" | "min-height" | "max-height" => {
            let l = if lower == "none" || lower.contains("content") {
                Some(Length::Auto)
            } else {
                length(v, font, ctx)
            };
            if let Some(l) = l {
                match name {
                    "width" => s.width = l,
                    "height" => s.height = l,
                    "min-width" => s.min_width = l,
                    "max-width" => s.max_width = l,
                    "min-height" => s.min_height = l,
                    _ => s.max_height = l,
                }
            }
        }
        "box-sizing" => s.box_sizing_border = lower == "border-box",
        "flex-direction" => s.flex_row = !lower.starts_with("column"),
        "flex-wrap" => s.flex_wrap = lower != "nowrap",
        "flex-flow" => {
            s.flex_row = !lower.contains("column");
            s.flex_wrap = lower.contains("wrap") && !lower.contains("nowrap");
        }
        "justify-content" => {
            if let Some(a) = align(&lower) {
                s.justify = a;
            }
        }
        "align-items" => {
            if let Some(a) = align(&lower) {
                s.align_items = a;
            }
        }
        "align-self" => s.align_self = align(&lower),
        "place-items" => {
            if let Some(a) = tokens(&lower).first().and_then(|t| align(t)) {
                s.align_items = a;
            }
        }
        "gap" | "grid-gap" => {
            let gs: Vec<f32> = tokens(v).iter().filter_map(|t| px(t, font, ctx)).collect();
            if let Some(&g) = gs.first() {
                s.row_gap = g;
                s.gap = *gs.get(1).unwrap_or(&g);
            }
        }
        "column-gap" => {
            if let Some(g) = px(v, font, ctx) {
                s.gap = g;
            }
        }
        "row-gap" => {
            if let Some(g) = px(v, font, ctx) {
                s.row_gap = g;
            }
        }
        "flex" => {
            let ts = tokens(&lower);
            match ts.as_slice() {
                ["none"] => {
                    s.flex_grow = 0.0;
                    s.flex_shrink = 0.0;
                }
                ["auto"] => {
                    s.flex_grow = 1.0;
                    s.flex_shrink = 1.0;
                }
                [g, rest @ ..] => {
                    s.flex_grow = g.parse().unwrap_or(0.0);
                    if let Some(sh) = rest.first().and_then(|t| t.parse().ok()) {
                        s.flex_shrink = sh;
                    }
                    s.flex_basis = rest
                        .iter()
                        .find_map(|t| length(t, font, ctx).filter(|_| t.parse::<f32>().is_err()))
                        .unwrap_or(Length::Px(0.0));
                }
                _ => {}
            }
        }
        "flex-grow" => s.flex_grow = lower.parse().unwrap_or(0.0),
        "flex-shrink" => s.flex_shrink = lower.parse().unwrap_or(1.0),
        "flex-basis" => {
            if let Some(l) = length(v, font, ctx) {
                s.flex_basis = l;
            }
        }
        "order" => s.order = lower.parse().unwrap_or(0),
        "grid-template-columns" => {
            let t = tokens(&lower);
            s.grid_columns = if let Some(rep) = t.first().filter(|t| t.starts_with("repeat(")) {
                let inner = &rep[7..];
                inner
                    .split(',')
                    .next()
                    .and_then(|n| n.trim().parse().ok())
                    .unwrap_or(3)
            } else {
                t.len().max(1)
            };
        }
        "list-style" | "list-style-type" => {
            s.list_style = if lower.contains("none") {
                0
            } else if lower.contains("decimal") {
                2
            } else if lower.contains("circle") || lower.contains("square") || lower.contains("disc") {
                1
            } else {
                s.list_style
            }
        }
        "visibility" => s.visible = lower != "hidden" && lower != "collapse",
        "opacity" => {
            s.opacity = lower
                .strip_suffix('%')
                .map(|p| p.parse::<f32>().unwrap_or(100.0) / 100.0)
                .unwrap_or_else(|| lower.parse().unwrap_or(1.0))
        }
        "overflow" | "overflow-x" | "overflow-y" => {
            s.overflow_hidden = lower != "visible";
        }
        "z-index" => s.z_index = lower.parse().unwrap_or(0),
        "vertical-align" => s.vertical_middle = lower == "middle",
        "border-spacing" => {
            if let Some(Length::Px(v)) = length(v.split_whitespace().next().unwrap_or(""), font, ctx) {
                s.border_spacing = Some(v);
            }
        }
        "border-collapse" if lower == "collapse" => s.border_spacing = Some(0.0),
        "float" => {
            s.float = match lower.as_str() {
                "left" => 1,
                "right" => 2,
                _ => 0,
            }
        }
        "clear" => s.clear = lower != "none",
        _ => {}
    }
}
