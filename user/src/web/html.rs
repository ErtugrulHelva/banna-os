//! HTML5 ayrıştırıcısı: hoşgörülü tokenizer + ağaç kurucu. Tarayıcıların hata
//! kurtarmasının sık görülen kısımlarını uygular: kendiliğinden kapanan öğeler, örtük
//! kapanan `p`/`li`/`option`/tablo hücreleri, ham metin öğeleri (`script`, `style`,
//! `textarea`, `title`), adlandırılmış ve sayısal karakter başvuruları.

use alloc::string::String;
use alloc::vec::Vec;

use super::dom::{Document, Kind, NodeId, ROOT};

const VOID: &[&str] = &[
    "area", "base", "br", "col", "embed", "hr", "img", "input", "link", "meta", "param", "source",
    "track", "wbr",
];
const RAW_TEXT: &[&str] = &["script", "style", "textarea", "title", "xmp", "noscript"];
/// Açılınca açık bir `p`'yi kapatan blok öğeleri.
const CLOSES_P: &[&str] = &[
    "address", "article", "aside", "blockquote", "details", "div", "dl", "fieldset", "figcaption",
    "figure", "footer", "form", "h1", "h2", "h3", "h4", "h5", "h6", "header", "hr", "main", "menu",
    "nav", "ol", "p", "pre", "section", "table", "ul",
];

pub fn is_void(tag: &str) -> bool {
    VOID.contains(&tag)
}

/// Karakter başvurularını çözer (`&amp;`, `&#39;`, `&#x1F600;`, sık kullanılan adlar).
pub fn decode_entities(text: &str) -> String {
    if !text.contains('&') {
        return String::from(text);
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(i) = rest.find('&') {
        out.push_str(&rest[..i]);
        rest = &rest[i..];
        let end = rest[1..]
            .find(|c: char| c == ';' || c == '&' || c.is_whitespace() || c == '<')
            .map(|e| e + 1)
            .filter(|&e| e <= 32);
        let Some(end) = end.filter(|&e| rest.as_bytes().get(e) == Some(&b';')) else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let name = &rest[1..end];
        let ch = if let Some(num) = name.strip_prefix('#') {
            let value = match num.strip_prefix(['x', 'X']) {
                Some(hex) => u32::from_str_radix(hex, 16).ok(),
                None => num.parse().ok(),
            };
            value.and_then(char::from_u32)
        } else {
            named(name)
        };
        match ch {
            Some(c) => {
                out.push(c);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn named(name: &str) -> Option<char> {
    Some(match name {
        "amp" => '&',
        "lt" => '<',
        "gt" => '>',
        "quot" => '"',
        "apos" => '\'',
        "nbsp" => '\u{a0}',
        "copy" => '©',
        "reg" => '®',
        "trade" => '™',
        "hellip" => '…',
        "mdash" => '—',
        "ndash" => '–',
        "laquo" => '«',
        "raquo" => '»',
        "lsquo" => '‘',
        "rsquo" => '’',
        "ldquo" => '“',
        "rdquo" => '”',
        "bull" => '•',
        "middot" => '·',
        "times" => '×',
        "divide" => '÷',
        "euro" => '€',
        "pound" => '£',
        "deg" => '°',
        "rarr" => '→',
        "larr" => '←',
        "uarr" => '↑',
        "darr" => '↓',
        "check" => '✓',
        "ccedil" => 'ç',
        "Ccedil" => 'Ç',
        "ouml" => 'ö',
        "Ouml" => 'Ö',
        "uuml" => 'ü',
        "Uuml" => 'Ü',
        "eacute" => 'é',
        "iacute" => 'í',
        "shy" => '\u{ad}',
        "zwj" => '\u{200d}',
        _ => return None,
    })
}

enum Token {
    Start {
        tag: String,
        attrs: Vec<(String, String)>,
        self_closing: bool,
    },
    End(String),
    Text(String),
    Comment,
}

struct Tokenizer<'a> {
    src: &'a str,
    pos: usize,
    /// Ham metin öğesinin içindeyken kapanış etiketi.
    raw: Option<String>,
}

impl<'a> Tokenizer<'a> {
    fn peek(&self) -> Option<u8> {
        self.src.as_bytes().get(self.pos).copied()
    }

    fn rest(&self) -> &'a str {
        &self.src[self.pos..]
    }

    fn skip_ws(&mut self) {
        while self.peek().is_some_and(|b| b.is_ascii_whitespace()) {
            self.pos += 1;
        }
    }

    fn next(&mut self) -> Option<Token> {
        if self.pos >= self.src.len() {
            return None;
        }
        if let Some(tag) = self.raw.take() {
            let close = alloc::format!("</{}", tag);
            let lower = self.rest();
            let end = find_ci(lower, &close).unwrap_or(lower.len());
            let text = String::from(&lower[..end]);
            self.pos += end;
            if !text.is_empty() {
                return Some(Token::Text(text));
            }
        }
        let rest = self.rest();
        if let Some(body) = rest.strip_prefix("<!--") {
            let end = body.find("-->").map_or(body.len(), |e| e + 3);
            self.pos += 4 + end;
            return Some(Token::Comment);
        }
        if rest.starts_with("<!") || rest.starts_with("<?") {
            let end = rest.find('>').map_or(rest.len(), |e| e + 1);
            self.pos += end;
            return Some(Token::Comment);
        }
        if let Some(after) = rest.strip_prefix("</")
            && after.starts_with(|c: char| c.is_ascii_alphabetic()) {
                let end = after.find('>').map_or(after.len(), |e| e + 1);
                let name: String = after[..end]
                    .chars()
                    .take_while(|c| c.is_ascii_alphanumeric() || *c == '-')
                    .collect::<String>()
                    .to_ascii_lowercase();
                self.pos += 2 + end;
                return Some(Token::End(name));
            }
        if rest.starts_with('<') && rest[1..].starts_with(|c: char| c.is_ascii_alphabetic()) {
            return Some(self.start_tag());
        }
        let first = rest.chars().next().map_or(1, char::len_utf8);
        let end = rest[first..].find('<').map_or(rest.len(), |e| e + first);
        self.pos += end;
        Some(Token::Text(decode_entities(&rest[..end])))
    }

    fn start_tag(&mut self) -> Token {
        self.pos += 1;
        let start = self.pos;
        while self
            .peek()
            .is_some_and(|b| !b.is_ascii_whitespace() && b != b'>' && b != b'/')
        {
            self.pos += 1;
        }
        let tag = self.src[start..self.pos].to_ascii_lowercase();
        let mut attrs: Vec<(String, String)> = Vec::new();
        let mut self_closing = false;
        loop {
            self.skip_ws();
            match self.peek() {
                None => break,
                Some(b'>') => {
                    self.pos += 1;
                    break;
                }
                Some(b'/') => {
                    self.pos += 1;
                    if self.peek() == Some(b'>') {
                        self_closing = true;
                    }
                    continue;
                }
                _ => {}
            }
            let name_start = self.pos;
            while self
                .peek()
                .is_some_and(|b| !b.is_ascii_whitespace() && !matches!(b, b'=' | b'>' | b'/'))
            {
                self.pos += 1;
            }
            if self.pos == name_start {
                self.pos += 1;
                continue;
            }
            let name = self.src[name_start..self.pos].to_ascii_lowercase();
            self.skip_ws();
            let mut value = String::new();
            if self.peek() == Some(b'=') {
                self.pos += 1;
                self.skip_ws();
                match self.peek() {
                    Some(q @ (b'"' | b'\'')) => {
                        self.pos += 1;
                        let end = self.rest().find(q as char).unwrap_or(self.rest().len());
                        value = decode_entities(&self.rest()[..end]);
                        self.pos = (self.pos + end + 1).min(self.src.len());
                    }
                    _ => {
                        let s = self.pos;
                        while self
                            .peek()
                            .is_some_and(|b| !b.is_ascii_whitespace() && b != b'>')
                        {
                            self.pos += 1;
                        }
                        value = decode_entities(&self.src[s..self.pos]);
                    }
                }
            }
            if !attrs.iter().any(|(k, _)| *k == name) {
                attrs.push((name, value));
            }
        }
        if RAW_TEXT.contains(&tag.as_str()) && !self_closing {
            self.raw = Some(tag.clone());
        }
        Token::Start {
            tag,
            attrs,
            self_closing,
        }
    }
}

fn find_ci(haystack: &str, needle: &str) -> Option<usize> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    (0..=h.len().checked_sub(n.len())?).find(|&i| h[i..i + n.len()].eq_ignore_ascii_case(n))
}

struct Builder<'d> {
    doc: &'d mut Document,
    stack: Vec<NodeId>,
}

impl Builder<'_> {
    fn current(&self) -> NodeId {
        *self.stack.last().unwrap_or(&ROOT)
    }

    fn open_tag(&self, i: usize) -> &str {
        self.doc.tag(self.stack[i]).unwrap_or("")
    }

    fn in_scope(&self, tag: &str) -> Option<usize> {
        for i in (0..self.stack.len()).rev() {
            let t = self.open_tag(i);
            if t == tag {
                return Some(i);
            }
            if matches!(t, "table" | "html" | "td" | "th" | "button") && tag != t
                && (tag == "p" || tag == "li") {
                    return None;
                }
            // İç içe tablo: içteki <tr>/<td> dıştaki tablonun hücresini kapatmaz.
            if t == "table" && matches!(tag, "td" | "th" | "tr" | "tbody" | "thead" | "tfoot" | "caption") {
                return None;
            }
        }
        None
    }

    fn close(&mut self, tag: &str) {
        if let Some(i) = self.in_scope(tag) {
            self.stack.truncate(i);
        }
    }

    fn ensure_body(&mut self) {
        if self.stack.iter().any(|&n| self.doc.is(n, "body")) {
            return;
        }
        let html = self.ensure_html();
        if !self.doc.nodes[html]
            .children
            .iter()
            .any(|&c| self.doc.is(c, "head"))
        {
            let head = self.doc.element("head");
            self.doc.append(html, head);
        }
        let body = match self.doc.nodes[html]
            .children
            .iter()
            .copied()
            .find(|&c| self.doc.is(c, "body"))
        {
            Some(b) => b,
            None => {
                let b = self.doc.element("body");
                self.doc.append(html, b);
                b
            }
        };
        self.stack.retain(|&n| n != html);
        self.stack.clear();
        self.stack.push(html);
        self.stack.push(body);
    }

    fn ensure_html(&mut self) -> NodeId {
        if let Some(&html) = self.doc.nodes[ROOT]
            .children
            .iter()
            .find(|&&c| self.doc.is(c, "html"))
        {
            return html;
        }
        let html = self.doc.element("html");
        self.doc.append(ROOT, html);
        html
    }

    fn head(&mut self) -> NodeId {
        let html = self.ensure_html();
        if let Some(&head) = self.doc.nodes[html]
            .children
            .iter()
            .find(|&&c| self.doc.is(c, "head"))
        {
            return head;
        }
        let head = self.doc.element("head");
        self.doc.insert_before(html, head, self.doc.nodes[html].children.first().copied());
        head
    }

    fn start(&mut self, tag: String, attrs: Vec<(String, String)>, self_closing: bool) {
        match tag.as_str() {
            "html" => {
                let html = self.ensure_html();
                for (k, v) in attrs {
                    if self.doc.attr(html, &k).is_none() {
                        self.doc.set_attr(html, &k, &v);
                    }
                }
                if self.stack.is_empty() {
                    self.stack.push(html);
                }
                return;
            }
            "head" => {
                let head = self.head();
                self.stack = alloc::vec![self.ensure_html(), head];
                return;
            }
            "body" => {
                self.ensure_body();
                let body = self.current();
                for (k, v) in attrs {
                    self.doc.set_attr(body, &k, &v);
                }
                return;
            }
            _ => {}
        }
        let head_only = matches!(tag.as_str(), "title" | "meta" | "link" | "base")
            || (matches!(tag.as_str(), "style" | "script")
                && !self.stack.iter().any(|&n| self.doc.is(n, "body")));
        let parent = if head_only && !self.stack.iter().any(|&n| self.doc.is(n, "body")) {
            self.head()
        } else {
            self.ensure_body();
            if CLOSES_P.contains(&tag.as_str()) {
                self.close("p");
            }
            match tag.as_str() {
                "li" => self.close("li"),
                "option" => self.close("option"),
                "dt" | "dd" => {
                    self.close("dt");
                    self.close("dd");
                }
                "td" | "th" => {
                    self.close("td");
                    self.close("th");
                    if !matches!(self.doc.tag(self.current()), Some("tr")) {
                        self.start(String::from("tr"), Vec::new(), false);
                    }
                }
                "tr" => {
                    self.close("td");
                    self.close("th");
                    self.close("tr");
                }
                "a" => self.close("a"),
                _ => {}
            }
            self.current()
        };
        let node = self.doc.create(Kind::Element {
            tag: tag.clone(),
            attrs,
        });
        self.doc.append(parent, node);
        if !self_closing && !is_void(&tag) {
            if head_only && !RAW_TEXT.contains(&tag.as_str()) {
                return;
            }
            if parent != self.current() {
                self.stack.push(parent);
            }
            self.stack.push(node);
        }
    }

    fn end(&mut self, tag: &str) {
        match tag {
            "html" | "body" => {}
            "head" => {
                self.stack.retain(|&n| !self.doc.is(n, "head"));
            }
            "br" => self.start(String::from("br"), Vec::new(), true),
            "p" if self.in_scope("p").is_none() => {
                self.start(String::from("p"), Vec::new(), false);
                self.close("p");
            }
            _ => self.close(tag),
        }
    }

    fn text(&mut self, text: String) {
        let parent = self.current();
        let in_body = self.stack.iter().any(|&n| self.doc.is(n, "body"));
        let raw = self
            .doc
            .tag(parent)
            .is_some_and(|t| RAW_TEXT.contains(&t));
        if !in_body && !raw {
            if text.trim().is_empty() {
                return;
            }
            self.ensure_body();
        }
        let parent = self.current();
        if let Some(&last) = self.doc.nodes[parent].children.last()
            && let Kind::Text(existing) = &mut self.doc.nodes[last].kind
        {
            existing.push_str(&text);
            return;
        }
        let node = self.doc.text(&text);
        self.doc.append(parent, node);
    }
}

/// Bir HTML belgesini ayrıştırır.
pub fn parse(src: &str) -> Document {
    let mut doc = Document::new();
    parse_into(&mut doc, ROOT, src, true);
    let mut b = Builder {
        doc: &mut doc,
        stack: Vec::new(),
    };
    b.ensure_body();
    doc
}

/// `src`'yi `parent`'ın altına ekler (innerHTML). `document` doğruysa belge kökü kurulur.
pub fn parse_into(doc: &mut Document, parent: NodeId, src: &str, document: bool) {
    let mut tokens = Tokenizer {
        src,
        pos: 0,
        raw: None,
    };
    let mut b = Builder {
        doc,
        stack: if document {
            Vec::new()
        } else {
            alloc::vec![parent]
        },
    };
    let fragment = !document;
    while let Some(token) = tokens.next() {
        match token {
            Token::Start {
                tag,
                attrs,
                self_closing,
            } => {
                if fragment {
                    fragment_start(&mut b, tag, attrs, self_closing);
                } else {
                    b.start(tag, attrs, self_closing);
                }
            }
            Token::End(tag) => {
                if fragment {
                    if let Some(i) = b.stack.iter().rposition(|&n| b.doc.is(n, &tag))
                        && i > 0
                    {
                        b.stack.truncate(i);
                    }
                } else {
                    b.end(&tag);
                }
            }
            Token::Text(text) => {
                if fragment {
                    let p = b.current();
                    let n = b.doc.text(&text);
                    b.doc.append(p, n);
                } else {
                    b.text(text);
                }
            }
            Token::Comment => {}
        }
    }
}

fn fragment_start(b: &mut Builder, tag: String, attrs: Vec<(String, String)>, self_closing: bool) {
    if (tag == "li" || tag == "p" || tag == "option")
        && let Some(i) = b.stack.iter().rposition(|&n| b.doc.is(n, &tag))
            && i > 0
        {
            b.stack.truncate(i);
        }
    let parent = b.current();
    let node = b.doc.create(Kind::Element {
        tag: tag.clone(),
        attrs,
    });
    b.doc.append(parent, node);
    if !self_closing && !is_void(&tag) {
        b.stack.push(node);
    }
}
