//! CSS ayrıştırıcısı: kurallar, seçiciler (tür, sınıf, kimlik, öznitelik, `*`, alt öğe,
//! doğrudan çocuk, kardeş, `:hover`/`:first-child`/`:last-child`/`:not()`/`:nth-child`),
//! özgüllük, `@media` (genişlik koşulları), `!important` ve `var(--x)` değişkenleri.

use alloc::string::String;
use alloc::vec::Vec;

use super::dom::{Document, NodeId};

#[derive(Clone, Debug, PartialEq)]
pub enum Simple {
    Universal,
    Tag(String),
    Class(String),
    Id(String),
    /// [ad], [ad=değer], [ad^=], [ad$=], [ad*=], [ad~=]
    Attr(String, Option<(char, String)>),
    Hover,
    Focus,
    FirstChild,
    LastChild,
    NthChild(i32, i32),
    Not(Vec<Simple>),
    /// Desteklenmeyen sözde sınıf: hiç eşleşmez.
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Combinator {
    Descendant,
    Child,
    Adjacent,
    Sibling,
}

/// Sağdan sola: `parts[0]` hedef öğe, sonrakiler ataları/kardeşleri.
#[derive(Clone, Debug)]
pub struct Selector {
    pub parts: Vec<(Vec<Simple>, Combinator)>,
    pub specificity: u32,
}

#[derive(Clone, Debug)]
pub struct Declaration {
    pub name: String,
    pub value: String,
    pub important: bool,
}

#[derive(Clone, Debug)]
pub struct Rule {
    pub selectors: Vec<Selector>,
    pub declarations: Vec<Declaration>,
    /// `@media` koşulu (en küçük, en büyük genişlik).
    pub media: Option<(Option<f32>, Option<f32>)>,
}

#[derive(Default, Clone, Debug)]
pub struct Sheet {
    pub rules: Vec<Rule>,
}

fn strip_comments(src: &str) -> String {
    let mut out = String::with_capacity(src.len());
    let mut rest = src;
    while let Some(i) = rest.find("/*") {
        out.push_str(&rest[..i]);
        rest = match rest[i + 2..].find("*/") {
            Some(e) => &rest[i + 2 + e + 2..],
            None => "",
        };
    }
    out.push_str(rest);
    out
}

/// Eşleşen kapanış parantezine kadar olan bloğu bulur.
fn block_end(src: &str, open: usize) -> usize {
    let mut depth = 0;
    let mut quote = None;
    for (i, c) in src[open..].char_indices() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), _) => {}
            (None, '"' | '\'') => quote = Some(c),
            (None, '{') => depth += 1,
            (None, '}') => {
                depth -= 1;
                if depth == 0 {
                    return open + i;
                }
            }
            _ => {}
        }
    }
    src.len()
}

pub fn parse_sheet(src: &str) -> Sheet {
    let src = strip_comments(src);
    let mut sheet = Sheet::default();
    parse_rules(&src, None, &mut sheet);
    sheet
}

fn parse_rules(src: &str, media: Option<(Option<f32>, Option<f32>)>, sheet: &mut Sheet) {
    let mut pos = 0;
    while pos < src.len() {
        let rest = &src[pos..];
        let Some(open) = rest.find('{') else { break };
        let prelude = rest[..open].trim();
        // `@import url(...);` gibi bloksuz at-kuralları.
        if prelude.starts_with('@') && prelude.contains(';') {
            pos += prelude.find(';').unwrap() + 1 + (rest.len() - rest.trim_start().len());
            continue;
        }
        let end = block_end(rest, open);
        let body = &rest[open + 1..end.min(rest.len())];
        if let Some(at) = prelude.strip_prefix('@') {
            if at.starts_with("media") {
                let cond = parse_media(&at[5..]);
                if let Some(cond) = cond {
                    parse_rules(body, Some(cond), sheet);
                }
            } else if at.starts_with("supports") || at.starts_with("layer") || at.starts_with("container") {
                parse_rules(body, media, sheet);
            }
        } else {
            let selectors: Vec<Selector> = prelude
                .split(',')
                .filter_map(|s| parse_selector(s.trim()))
                .collect();
            if !selectors.is_empty() {
                sheet.rules.push(Rule {
                    selectors,
                    declarations: parse_declarations(body),
                    media,
                });
            }
        }
        pos += end + 1;
    }
}

/// `screen and (max-width: 600px)` → (en küçük, en büyük). `print` gibi kipler atlanır.
fn parse_media(cond: &str) -> Option<(Option<f32>, Option<f32>)> {
    let cond = cond.trim().to_ascii_lowercase();
    if cond.contains("print") && !cond.contains("screen") || cond.starts_with("not") {
        return None;
    }
    let mut min = None;
    let mut max = None;
    for part in cond.split('(').skip(1) {
        let part = part.split(')').next().unwrap_or("");
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let px = parse_length_px(value.trim());
        match name.trim() {
            "min-width" => min = px,
            "max-width" => max = px,
            "prefers-color-scheme" if value.trim() == "dark" => return None,
            "hover" | "pointer" | "orientation" | "prefers-reduced-motion" => {}
            _ => {}
        }
    }
    Some((min, max))
}

fn parse_length_px(v: &str) -> Option<f32> {
    let num: String = v
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    let n: f32 = num.parse().ok()?;
    Some(if v.ends_with("em") { n * 16.0 } else { n })
}

pub fn parse_declarations(body: &str) -> Vec<Declaration> {
    let mut out = Vec::new();
    let mut depth = 0;
    let mut quote = None;
    let bytes: Vec<char> = body.chars().collect();
    let mut parts = Vec::new();
    let mut current = String::new();
    for &c in &bytes {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (None, '"' | '\'') => quote = Some(c),
            (None, '(') => depth += 1,
            (None, ')') => depth -= 1,
            (None, ';') if depth == 0 => {
                parts.push(core::mem::take(&mut current));
                continue;
            }
            _ => {}
        }
        current.push(c);
    }
    parts.push(current);
    for part in parts {
        let Some((name, value)) = part.split_once(':') else {
            continue;
        };
        let name = name.trim().to_ascii_lowercase();
        let mut value = value.trim();
        let mut important = false;
        if let Some(v) = value.strip_suffix("!important") {
            value = v.trim();
            important = true;
        }
        if name.is_empty() || value.is_empty() {
            continue;
        }
        out.push(Declaration {
            name: if name.starts_with("--") {
                String::from(part.split_once(':').unwrap().0.trim())
            } else {
                name
            },
            value: String::from(value),
            important,
        });
    }
    out
}

fn ident(s: &[char], i: &mut usize) -> String {
    let mut out = String::new();
    while *i < s.len() && (s[*i].is_alphanumeric() || s[*i] == '-' || s[*i] == '_' || s[*i] == '\\')
    {
        if s[*i] == '\\' && *i + 1 < s.len() {
            *i += 1;
        }
        out.push(s[*i]);
        *i += 1;
    }
    out
}

fn compound(s: &[char], i: &mut usize) -> Option<Vec<Simple>> {
    let mut out = Vec::new();
    while *i < s.len() {
        match s[*i] {
            '*' => {
                *i += 1;
                out.push(Simple::Universal);
            }
            '.' => {
                *i += 1;
                out.push(Simple::Class(ident(s, i)));
            }
            '#' => {
                *i += 1;
                out.push(Simple::Id(ident(s, i)));
            }
            '[' => {
                *i += 1;
                let end = s[*i..].iter().position(|&c| c == ']')? + *i;
                let inner: String = s[*i..end].iter().collect();
                *i = end + 1;
                let op = ['^', '$', '*', '~', '|']
                    .into_iter()
                    .find(|op| inner.contains(&alloc::format!("{}=", op)));
                let (name, value) = match op {
                    Some(op) => {
                        let (n, v) = inner.split_once(&alloc::format!("{}=", op)).unwrap();
                        (n, Some((op, v)))
                    }
                    None => match inner.split_once('=') {
                        Some((n, v)) => (n, Some(('=', v))),
                        None => (inner.as_str(), None),
                    },
                };
                let value = value.map(|(op, v)| {
                    let v = v.trim().trim_end_matches([' ', 'i', 's']).trim();
                    (op, String::from(v.trim_matches(['"', '\''])))
                });
                out.push(Simple::Attr(String::from(name.trim()).to_ascii_lowercase(), value));
            }
            ':' => {
                *i += 1;
                if *i < s.len() && s[*i] == ':' {
                    // Sözde öğe (::before): içerik üretilmez, kural atlanır.
                    return None;
                }
                let name = ident(s, i).to_ascii_lowercase();
                let mut arg = String::new();
                if *i < s.len() && s[*i] == '(' {
                    let mut depth = 0;
                    let start = *i + 1;
                    while *i < s.len() {
                        if s[*i] == '(' {
                            depth += 1;
                        } else if s[*i] == ')' {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        *i += 1;
                    }
                    arg = s[start..(*i).min(s.len())].iter().collect();
                    *i += 1;
                }
                out.push(match name.as_str() {
                    "hover" => Simple::Hover,
                    "focus" | "focus-within" | "focus-visible" => Simple::Focus,
                    "first-child" | "first-of-type" => Simple::FirstChild,
                    "last-child" | "last-of-type" => Simple::LastChild,
                    "root" => Simple::Tag(String::from("html")),
                    "link" | "visited" | "any-link" => Simple::Tag(String::from("a")),
                    "is" | "where" | "matches" => {
                        let inner: Vec<char> = arg.trim().chars().collect();
                        let mut j = 0;
                        match compound(&inner, &mut j) {
                            Some(c) if j >= inner.len() => {
                                out.extend(c);
                                continue;
                            }
                            _ => Simple::Never,
                        }
                    }
                    "not" => {
                        let inner: Vec<char> = arg.trim().chars().collect();
                        let mut j = 0;
                        Simple::Not(compound(&inner, &mut j).unwrap_or_default())
                    }
                    "nth-child" | "nth-of-type" => {
                        let (a, b) = parse_nth(arg.trim());
                        Simple::NthChild(a, b)
                    }
                    _ => Simple::Never,
                });
            }
            c if c.is_alphabetic() || c == '-' || c == '_' => {
                out.push(Simple::Tag(ident(s, i).to_ascii_lowercase()));
            }
            _ => break,
        }
    }
    (!out.is_empty()).then_some(out)
}

fn parse_nth(arg: &str) -> (i32, i32) {
    match arg {
        "odd" => (2, 1),
        "even" => (2, 0),
        _ => match arg.split_once('n') {
            Some((a, b)) => {
                let a = match a.trim() {
                    "" | "+" => 1,
                    "-" => -1,
                    a => a.parse().unwrap_or(1),
                };
                (a, b.replace(' ', "").parse().unwrap_or(0))
            }
            None => (0, arg.trim().parse().unwrap_or(0)),
        },
    }
}

pub fn parse_selector(text: &str) -> Option<Selector> {
    let s: Vec<char> = text.chars().collect();
    let mut i = 0;
    let mut parts: Vec<(Vec<Simple>, Combinator)> = Vec::new();
    let mut combinator = Combinator::Descendant;
    loop {
        while i < s.len() && s[i].is_whitespace() {
            i += 1;
        }
        if i >= s.len() {
            break;
        }
        match s[i] {
            '>' => {
                combinator = Combinator::Child;
                i += 1;
                continue;
            }
            '+' => {
                combinator = Combinator::Adjacent;
                i += 1;
                continue;
            }
            '~' => {
                combinator = Combinator::Sibling;
                i += 1;
                continue;
            }
            _ => {}
        }
        let c = compound(&s, &mut i)?;
        parts.push((c, combinator));
        combinator = Combinator::Descendant;
    }
    if parts.is_empty() {
        return None;
    }
    parts.reverse();
    // Birleştirici sağdaki parçaya aitti: sola kaydır.
    let combinators: Vec<Combinator> = parts.iter().map(|p| p.1).collect();
    for (k, part) in parts.iter_mut().enumerate() {
        part.1 = if k == 0 {
            Combinator::Descendant
        } else {
            combinators[k - 1]
        };
    }
    let mut specificity = 0;
    for (simples, _) in &parts {
        for simple in simples {
            specificity += weight(simple);
        }
    }
    Some(Selector { parts, specificity })
}

fn weight(s: &Simple) -> u32 {
    match s {
        Simple::Id(_) => 1 << 16,
        Simple::Class(_)
        | Simple::Attr(..)
        | Simple::Hover
        | Simple::Focus
        | Simple::FirstChild
        | Simple::LastChild
        | Simple::NthChild(..) => 1 << 8,
        Simple::Not(inner) => inner.iter().map(weight).sum(),
        Simple::Tag(_) => 1,
        _ => 0,
    }
}

fn matches_simple(doc: &Document, node: NodeId, s: &Simple) -> bool {
    match s {
        Simple::Universal => true,
        Simple::Tag(t) => doc.tag(node) == Some(t.as_str()),
        Simple::Class(c) => doc.has_class(node, c),
        Simple::Id(id) => doc.attr(node, "id") == Some(id.as_str()),
        Simple::Attr(name, cond) => match (doc.attr(node, name), cond) {
            (None, _) => false,
            (Some(_), None) => true,
            (Some(v), Some((op, want))) => match op {
                '=' => v == want,
                '^' => v.starts_with(want.as_str()),
                '$' => v.ends_with(want.as_str()),
                '*' => v.contains(want.as_str()),
                '~' => v.split_whitespace().any(|w| w == want),
                '|' => v == want || v.starts_with(&alloc::format!("{}-", want)),
                _ => false,
            },
        },
        Simple::Hover => {
            let mut h = doc.hover;
            while let Some(n) = h {
                if n == node {
                    return true;
                }
                h = doc.nodes[n].parent;
            }
            false
        }
        Simple::Focus => doc.focus == Some(node),
        Simple::FirstChild => sibling_index(doc, node).0 == 0,
        Simple::LastChild => {
            let (i, n) = sibling_index(doc, node);
            i + 1 == n
        }
        Simple::NthChild(a, b) => {
            let pos = sibling_index(doc, node).0 as i32 + 1;
            if *a == 0 {
                pos == *b
            } else {
                (pos - b) % a == 0 && (pos - b) / a >= 0
            }
        }
        Simple::Not(inner) => !inner.iter().all(|s| matches_simple(doc, node, s)),
        Simple::Never => false,
    }
}

fn sibling_index(doc: &Document, node: NodeId) -> (usize, usize) {
    let Some(parent) = doc.nodes[node].parent else {
        return (0, 1);
    };
    let siblings = doc.element_children(parent);
    (
        siblings.iter().position(|&s| s == node).unwrap_or(0),
        siblings.len(),
    )
}

fn matches_compound(doc: &Document, node: NodeId, c: &[Simple]) -> bool {
    doc.tag(node).is_some() && c.iter().all(|s| matches_simple(doc, node, s))
}

fn previous_element(doc: &Document, node: NodeId) -> Option<NodeId> {
    let parent = doc.nodes[node].parent?;
    let siblings = doc.element_children(parent);
    let i = siblings.iter().position(|&s| s == node)?;
    i.checked_sub(1).map(|j| siblings[j])
}

fn matches_from(doc: &Document, node: NodeId, parts: &[(Vec<Simple>, Combinator)]) -> bool {
    let Some(((first, _), rest)) = parts.split_first() else {
        return true;
    };
    if !matches_compound(doc, node, first) {
        return false;
    }
    let Some((_, combinator)) = rest.first() else {
        return true;
    };
    match combinator {
        Combinator::Child => doc.nodes[node]
            .parent
            .is_some_and(|p| matches_from(doc, p, rest)),
        Combinator::Descendant => {
            let mut p = doc.nodes[node].parent;
            while let Some(n) = p {
                if matches_from(doc, n, rest) {
                    return true;
                }
                p = doc.nodes[n].parent;
            }
            false
        }
        Combinator::Adjacent => {
            previous_element(doc, node).is_some_and(|s| matches_from(doc, s, rest))
        }
        Combinator::Sibling => {
            let mut s = previous_element(doc, node);
            while let Some(n) = s {
                if matches_from(doc, n, rest) {
                    return true;
                }
                s = previous_element(doc, n);
            }
            false
        }
    }
}

pub fn matches(doc: &Document, node: NodeId, selector: &Selector) -> bool {
    matches_from(doc, node, &selector.parts)
}

/// `querySelectorAll`: virgüllü seçici listesiyle eşleşen öğeler (belge sırasıyla).
pub fn select(doc: &Document, scope: NodeId, selectors: &str) -> Vec<NodeId> {
    let list: Vec<Selector> = selectors
        .split(',')
        .filter_map(|s| parse_selector(s.trim()))
        .collect();
    doc.descendants(scope)
        .into_iter()
        .skip(1)
        .filter(|&n| list.iter().any(|s| matches(doc, n, s)))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub u32);

impl Color {
    pub const TRANSPARENT: Color = Color(0);
    pub fn alpha(self) -> u32 {
        self.0 >> 24
    }
}

fn hex_digit(c: u8) -> Option<u32> {
    (c as char).to_digit(16)
}

fn number_list(inner: &str) -> Vec<f32> {
    inner
        .split([',', ' ', '/'])
        .filter(|s| !s.is_empty())
        .map(|s| {
            let t = s.trim();
            if let Some(p) = t.strip_suffix('%') {
                p.parse::<f32>().unwrap_or(0.0) * 2.55
            } else {
                t.trim_end_matches("deg").parse().unwrap_or(0.0)
            }
        })
        .collect()
}

pub fn parse_color(v: &str) -> Option<Color> {
    let v = v.trim();
    let lower = v.to_ascii_lowercase();
    if let Some(hex) = lower.strip_prefix('#') {
        let b = hex.as_bytes();
        let digits: Option<Vec<u32>> = b.iter().map(|&c| hex_digit(c)).collect();
        let d = digits?;
        let (r, g, bl, a) = match d.len() {
            3 => (d[0] * 17, d[1] * 17, d[2] * 17, 255),
            4 => (d[0] * 17, d[1] * 17, d[2] * 17, d[3] * 17),
            6 => (d[0] * 16 + d[1], d[2] * 16 + d[3], d[4] * 16 + d[5], 255),
            8 => (
                d[0] * 16 + d[1],
                d[2] * 16 + d[3],
                d[4] * 16 + d[5],
                d[6] * 16 + d[7],
            ),
            _ => return None,
        };
        return Some(Color(a << 24 | r << 16 | g << 8 | bl));
    }
    if let Some(inner) = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))
    {
        let inner = inner.trim_end_matches(')');
        let raw: Vec<&str> = inner.split([',', ' ', '/']).filter(|s| !s.is_empty()).collect();
        let n = number_list(inner);
        if n.len() < 3 {
            return None;
        }
        let a = match raw.get(3) {
            Some(t) if t.ends_with('%') => n[3],
            Some(_) => n[3] * 255.0,
            None => 255.0,
        };
        let c = |x: f32| x.clamp(0.0, 255.0) as u32;
        return Some(Color(c(a) << 24 | c(n[0]) << 16 | c(n[1]) << 8 | c(n[2])));
    }
    if let Some(inner) = lower
        .strip_prefix("hsla(")
        .or_else(|| lower.strip_prefix("hsl("))
    {
        let inner = inner.trim_end_matches(')');
        let parts: Vec<&str> = inner.split([',', ' ', '/']).filter(|s| !s.is_empty()).collect();
        if parts.len() < 3 {
            return None;
        }
        let h = parts[0].trim_end_matches("deg").parse::<f32>().ok()?;
        let s = parts[1].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let l = parts[2].trim_end_matches('%').parse::<f32>().ok()? / 100.0;
        let a = parts
            .get(3)
            .map(|t| {
                if let Some(p) = t.strip_suffix('%') {
                    p.parse::<f32>().unwrap_or(100.0) / 100.0
                } else {
                    t.parse().unwrap_or(1.0)
                }
            })
            .unwrap_or(1.0);
        let (r, g, b) = hsl(h, s, l);
        return Some(Color(
            ((a * 255.0) as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32,
        ));
    }
    let named = match lower.as_str() {
        "transparent" => return Some(Color::TRANSPARENT),
        "currentcolor" | "inherit" | "initial" | "unset" => return None,
        "black" => 0x000000,
        "white" => 0xffffff,
        "red" => 0xff0000,
        "green" => 0x008000,
        "blue" => 0x0000ff,
        "yellow" => 0xffff00,
        "orange" => 0xffa500,
        "purple" => 0x800080,
        "gray" | "grey" => 0x808080,
        "silver" => 0xc0c0c0,
        "maroon" => 0x800000,
        "navy" => 0x000080,
        "teal" => 0x008080,
        "olive" => 0x808000,
        "lime" => 0x00ff00,
        "aqua" | "cyan" => 0x00ffff,
        "fuchsia" | "magenta" => 0xff00ff,
        "pink" => 0xffc0cb,
        "brown" => 0xa52a2a,
        "gold" => 0xffd700,
        "indigo" => 0x4b0082,
        "violet" => 0xee82ee,
        "coral" => 0xff7f50,
        "salmon" => 0xfa8072,
        "tomato" => 0xff6347,
        "crimson" => 0xdc143c,
        "khaki" => 0xf0e68c,
        "beige" => 0xf5f5dc,
        "ivory" => 0xfffff0,
        "lavender" => 0xe6e6fa,
        "skyblue" => 0x87ceeb,
        "steelblue" => 0x4682b4,
        "royalblue" => 0x4169e1,
        "dodgerblue" => 0x1e90ff,
        "deepskyblue" => 0x00bfff,
        "lightblue" => 0xadd8e6,
        "darkblue" => 0x00008b,
        "darkgreen" => 0x006400,
        "darkred" => 0x8b0000,
        "darkgray" | "darkgrey" => 0xa9a9a9,
        "lightgray" | "lightgrey" => 0xd3d3d3,
        "gainsboro" => 0xdcdcdc,
        "whitesmoke" => 0xf5f5f5,
        "dimgray" | "dimgrey" => 0x696969,
        "slategray" | "slategrey" => 0x708090,
        "seagreen" => 0x2e8b57,
        "forestgreen" => 0x228b22,
        "limegreen" => 0x32cd32,
        "orangered" => 0xff4500,
        "hotpink" => 0xff69b4,
        "tan" => 0xd2b48c,
        "chocolate" => 0xd2691e,
        "firebrick" => 0xb22222,
        "midnightblue" => 0x191970,
        "aliceblue" => 0xf0f8ff,
        "ghostwhite" => 0xf8f8ff,
        "honeydew" => 0xf0fff0,
        "mintcream" => 0xf5fffa,
        "linen" => 0xfaf0e6,
        "snow" => 0xfffafa,
        "wheat" => 0xf5deb3,
        "turquoise" => 0x40e0d0,
        _ => return None,
    };
    Some(Color(0xff00_0000 | named))
}

fn hsl(h: f32, s: f32, l: f32) -> (f32, f32, f32) {
    let h = ((h % 360.0) + 360.0) % 360.0 / 360.0;
    let q = if l < 0.5 { l * (1.0 + s) } else { l + s - l * s };
    let p = 2.0 * l - q;
    let f = |mut t: f32| {
        if t < 0.0 {
            t += 1.0;
        }
        if t > 1.0 {
            t -= 1.0;
        }
        let v = if t < 1.0 / 6.0 {
            p + (q - p) * 6.0 * t
        } else if t < 0.5 {
            q
        } else if t < 2.0 / 3.0 {
            p + (q - p) * (2.0 / 3.0 - t) * 6.0
        } else {
            p
        };
        (v * 255.0).clamp(0.0, 255.0)
    };
    (f(h + 1.0 / 3.0), f(h), f(h - 1.0 / 3.0))
}
