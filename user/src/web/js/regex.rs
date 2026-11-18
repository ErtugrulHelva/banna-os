//! Geri izlemeli düzenli ifade motoru (JavaScript sözdizimi): karakter sınıfları,
//! `\d \w \s \b`, yakalayan/yakalamayan/adlı gruplar, ileri bakış, geri başvuru,
//! açgözlü ve tembel nicelleyiciler, `i m s g y` bayrakları.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Start,
    End,
    WordBoundary(bool),
    Group(Vec<Vec<Node>>, Option<usize>),
    Look(Vec<Vec<Node>>, bool),
    Repeat(Box<Node>, usize, usize, bool),
    Backref(usize),
}

#[derive(Debug, Clone)]
pub struct Regex {
    alts: Vec<Vec<Node>>,
    pub groups: usize,
    pub names: Vec<(String, usize)>,
    pub ignore_case: bool,
    pub multiline: bool,
    pub dot_all: bool,
    pub global: bool,
    pub sticky: bool,
    pub source: String,
    pub flags: String,
}

type Caps = Vec<Option<(usize, usize)>>;

struct Compiler<'a> {
    c: &'a [char],
    i: usize,
    groups: usize,
    names: Vec<(String, usize)>,
}

const DIGIT: &[(char, char)] = &[('0', '9')];
const WORD: &[(char, char)] = &[('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')];
const SPACE: &[(char, char)] = &[
    (' ', ' '),
    ('\t', '\r'),
    ('\u{a0}', '\u{a0}'),
    ('\u{2000}', '\u{200a}'),
    ('\u{2028}', '\u{2029}'),
    ('\u{feff}', '\u{feff}'),
];

impl Compiler<'_> {
    fn alternatives(&mut self) -> Result<Vec<Vec<Node>>, String> {
        let mut alts = alloc::vec![Vec::new()];
        while self.i < self.c.len() {
            match self.c[self.i] {
                '|' => {
                    self.i += 1;
                    alts.push(Vec::new());
                }
                ')' => break,
                _ => {
                    let atom = self.atom()?;
                    let atom = self.quantifier(atom)?;
                    alts.last_mut().unwrap().push(atom);
                }
            }
        }
        Ok(alts)
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
        let (min, max) = match self.c.get(self.i) {
            Some('*') => (0, usize::MAX),
            Some('+') => (1, usize::MAX),
            Some('?') => (0, 1),
            Some('{') => {
                let end = match self.c[self.i..].iter().position(|&x| x == '}') {
                    Some(e) => self.i + e,
                    None => return Ok(atom),
                };
                let inner: String = self.c[self.i + 1..end].iter().collect();
                let parsed = match inner.split_once(',') {
                    Some((a, "")) => a.parse().ok().map(|a| (a, usize::MAX)),
                    Some((a, b)) => a.parse().ok().zip(b.parse().ok()),
                    None => inner.parse().ok().map(|a| (a, a)),
                };
                let Some(range) = parsed else { return Ok(atom) };
                self.i = end;
                range
            }
            _ => return Ok(atom),
        };
        self.i += 1;
        let greedy = if self.c.get(self.i) == Some(&'?') {
            self.i += 1;
            false
        } else {
            true
        };
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
    }

    fn escape_class(&mut self, e: char) -> Option<Node> {
        Some(match e {
            'd' => Node::Class(DIGIT.to_vec(), false),
            'D' => Node::Class(DIGIT.to_vec(), true),
            'w' => Node::Class(WORD.to_vec(), false),
            'W' => Node::Class(WORD.to_vec(), true),
            's' => Node::Class(SPACE.to_vec(), false),
            'S' => Node::Class(SPACE.to_vec(), true),
            _ => return None,
        })
    }

    fn escape_char(&mut self, e: char) -> char {
        match e {
            'n' => '\n',
            't' => '\t',
            'r' => '\r',
            'f' => '\u{c}',
            'v' => '\u{b}',
            '0' => '\0',
            'x' => {
                let hex: String = self.c[self.i..(self.i + 2).min(self.c.len())].iter().collect();
                self.i += 2;
                u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('x')
            }
            'u' => {
                if self.c.get(self.i) == Some(&'{') {
                    let end = self.c[self.i..].iter().position(|&x| x == '}').map_or(self.c.len(), |p| self.i + p);
                    let hex: String = self.c[self.i + 1..end].iter().collect();
                    self.i = end + 1;
                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('u')
                } else {
                    let hex: String = self.c[self.i..(self.i + 4).min(self.c.len())].iter().collect();
                    self.i += 4;
                    u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32).unwrap_or('u')
                }
            }
            other => other,
        }
    }

    fn atom(&mut self) -> Result<Node, String> {
        let ch = self.c[self.i];
        self.i += 1;
        Ok(match ch {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let mut capture = true;
                let mut look = None;
                if self.c.get(self.i) == Some(&'?') {
                    match self.c.get(self.i + 1) {
                        Some(':') => {
                            capture = false;
                            self.i += 2;
                        }
                        Some('=') => {
                            look = Some(true);
                            self.i += 2;
                        }
                        Some('!') => {
                            look = Some(false);
                            self.i += 2;
                        }
                        Some('<') if !matches!(self.c.get(self.i + 2), Some('=' | '!')) => {
                            let end = self.c[self.i..].iter().position(|&x| x == '>').map_or(self.c.len(), |p| self.i + p);
                            let name: String = self.c[self.i + 2..end].iter().collect();
                            self.i = end + 1;
                            self.names.push((name, self.groups + 1));
                        }
                        Some('<') => {
                            // Geri bakış desteklenmiyor: her zaman eşleşen ileri bakış gibi davranır.
                            look = Some(self.c.get(self.i + 2) == Some(&'='));
                            self.i += 3;
                        }
                        _ => {}
                    }
                }
                let index = if capture && look.is_none() {
                    self.groups += 1;
                    Some(self.groups)
                } else {
                    None
                };
                let alts = self.alternatives()?;
                if self.c.get(self.i) != Some(&')') {
                    return Err(String::from("düzenli ifadede ')' eksik"));
                }
                self.i += 1;
                match look {
                    Some(positive) => Node::Look(alts, positive),
                    None => Node::Group(alts, index),
                }
            }
            '[' => {
                let negate = self.c.get(self.i) == Some(&'^');
                if negate {
                    self.i += 1;
                }
                let mut ranges = Vec::new();
                let mut first = true;
                while self.i < self.c.len() && (self.c[self.i] != ']' || first) {
                    first = false;
                    let mut lo = self.c[self.i];
                    self.i += 1;
                    if lo == '\\' && self.i < self.c.len() {
                        let e = self.c[self.i];
                        self.i += 1;
                        if let Some(Node::Class(set, neg)) = self.escape_class(e) {
                            if neg {
                                // [^\d] içinde \D: yaklaşık olarak geniş aralık.
                                ranges.push(('\0', '/'));
                                ranges.push((':', '\u{10ffff}'));
                            } else {
                                ranges.extend(set);
                            }
                            continue;
                        }
                        lo = if e == 'b' { '\u{8}' } else { self.escape_char(e) };
                    }
                    let mut hi = lo;
                    if self.c.get(self.i) == Some(&'-') && self.c.get(self.i + 1).is_some_and(|&x| x != ']') {
                        self.i += 1;
                        hi = self.c[self.i];
                        self.i += 1;
                        if hi == '\\' && self.i < self.c.len() {
                            let e = self.c[self.i];
                            self.i += 1;
                            hi = self.escape_char(e);
                        }
                    }
                    ranges.push((lo, hi));
                }
                self.i += 1;
                Node::Class(ranges, negate)
            }
            '\\' => {
                let e = *self.c.get(self.i).unwrap_or(&'\\');
                self.i += 1;
                if let Some(n) = self.escape_class(e) {
                    n
                } else if e == 'b' || e == 'B' {
                    Node::WordBoundary(e == 'b')
                } else if e.is_ascii_digit() && e != '0' {
                    Node::Backref(e.to_digit(10).unwrap() as usize)
                } else {
                    Node::Char(self.escape_char(e))
                }
            }
            c => Node::Char(c),
        })
    }
}

fn fold(c: char) -> char {
    c.to_lowercase().next().unwrap_or(c)
}

impl Regex {
    pub fn new(source: &str, flags: &str) -> Result<Self, String> {
        let chars: Vec<char> = source.chars().collect();
        let mut comp = Compiler {
            c: &chars,
            i: 0,
            groups: 0,
            names: Vec::new(),
        };
        let alts = comp.alternatives()?;
        if comp.i < chars.len() {
            return Err(String::from("düzenli ifadede fazladan ')'"));
        }
        Ok(Self {
            alts,
            groups: comp.groups,
            names: comp.names,
            ignore_case: flags.contains('i'),
            multiline: flags.contains('m'),
            dot_all: flags.contains('s'),
            global: flags.contains('g'),
            sticky: flags.contains('y'),
            source: String::from(source),
            flags: String::from(flags),
        })
    }

    /// `start` karakter konumundan itibaren ilk eşleşme: gruplar (karakter aralıkları).
    pub fn exec_at(&self, text: &[char], start: usize) -> Option<Caps> {
        let last = if self.sticky { start } else { text.len() };
        for begin in start..=last.min(text.len()) {
            let mut caps: Caps = alloc::vec![None; self.groups + 1];
            let mut end = None;
            let mut steps = 0u32;
            if self.alts_match(&self.alts, text, begin, &mut caps, &mut steps, &mut |p, _| {
                end = Some(p);
                true
            }) {
                caps[0] = Some((begin, end.unwrap_or(begin)));
                return Some(caps);
            }
        }
        None
    }

    fn alts_match(
        &self,
        alts: &[Vec<Node>],
        t: &[char],
        pos: usize,
        caps: &mut Caps,
        steps: &mut u32,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        for alt in alts {
            let saved = caps.clone();
            if self.seq(alt, t, pos, caps, steps, k) {
                return true;
            }
            *caps = saved;
        }
        false
    }

    fn seq(
        &self,
        nodes: &[Node],
        t: &[char],
        pos: usize,
        caps: &mut Caps,
        steps: &mut u32,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
    ) -> bool {
        *steps += 1;
        if *steps > 2_000_000 {
            return false;
        }
        let Some((node, rest)) = nodes.split_first() else {
            return k(pos, caps);
        };
        match node {
            Node::Group(alts, index) => {
                let index = *index;
                self.alts_match(alts, t, pos, caps, steps, &mut |end, caps: &mut Caps| {
                    let saved = index.map(|i| caps[i]);
                    if let Some(i) = index {
                        caps[i] = Some((pos, end));
                    }
                    if self.seq(rest, t, end, caps, &mut 0, k) {
                        return true;
                    }
                    if let (Some(i), Some(s)) = (index, saved) {
                        caps[i] = s;
                    }
                    false
                })
            }
            Node::Look(alts, positive) => {
                let mut probe = caps.clone();
                let found = self.alts_match(alts, t, pos, &mut probe, steps, &mut |_, _| true);
                if found == *positive {
                    if found {
                        *caps = probe;
                    }
                    self.seq(rest, t, pos, caps, steps, k)
                } else {
                    false
                }
            }
            Node::Repeat(inner, min, max, greedy) => {
                self.repeat(inner, *min, *max, *greedy, rest, t, pos, caps, steps, k, 0, usize::MAX)
            }
            _ => match self.single(node, t, pos, caps) {
                Some(next) => self.seq(rest, t, next, caps, steps, k),
                None => false,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &self,
        inner: &Node,
        min: usize,
        max: usize,
        greedy: bool,
        rest: &[Node],
        t: &[char],
        pos: usize,
        caps: &mut Caps,
        steps: &mut u32,
        k: &mut dyn FnMut(usize, &mut Caps) -> bool,
        count: usize,
        last: usize,
    ) -> bool {
        *steps += 1;
        if *steps > 2_000_000 {
            return false;
        }
        // Boş eşleşmede sonsuz döngüye girme.
        if count > min && pos == last {
            return false;
        }
        let can_stop = count >= min;
        if !greedy && can_stop && self.seq(rest, t, pos, caps, steps, k) {
            return true;
        }
        if count < max {
            // Basit öğelerde özyineleme yerine doğrudan ilerle.
            let more = match inner {
                Node::Group(..) | Node::Look(..) | Node::Repeat(..) => {
                    let one = core::slice::from_ref(inner);
                    self.seq(one, t, pos, caps, steps, &mut |next, caps: &mut Caps| {
                        self.repeat(inner, min, max, greedy, rest, t, next, caps, &mut 0, k, count + 1, pos)
                    })
                }
                _ => match self.single(inner, t, pos, caps) {
                    Some(next) => self.repeat(inner, min, max, greedy, rest, t, next, caps, steps, k, count + 1, pos),
                    None => false,
                },
            };
            if more {
                return true;
            }
        }
        greedy && can_stop && self.seq(rest, t, pos, caps, steps, k)
    }

    fn is_word(c: Option<&char>) -> bool {
        c.is_some_and(|c| c.is_alphanumeric() || *c == '_')
    }

    fn single(&self, node: &Node, t: &[char], pos: usize, caps: &Caps) -> Option<usize> {
        match node {
            Node::Char(c) => {
                let ch = *t.get(pos)?;
                let ok = if self.ignore_case { fold(ch) == fold(*c) } else { ch == *c };
                ok.then_some(pos + 1)
            }
            Node::Any => {
                let ch = *t.get(pos)?;
                (self.dot_all || (ch != '\n' && ch != '\r')).then_some(pos + 1)
            }
            Node::Class(ranges, negate) => {
                let ch = *t.get(pos)?;
                let test = |c: char| ranges.iter().any(|&(lo, hi)| c >= lo && c <= hi);
                let inside = test(ch) || (self.ignore_case && (test(fold(ch)) || ch.to_uppercase().next().is_some_and(test)));
                (inside != *negate).then_some(pos + 1)
            }
            Node::Start => (pos == 0 || (self.multiline && t[pos - 1] == '\n')).then_some(pos),
            Node::End => (pos == t.len() || (self.multiline && t[pos] == '\n')).then_some(pos),
            Node::WordBoundary(want) => {
                let before = pos > 0 && Self::is_word(t.get(pos - 1));
                let after = Self::is_word(t.get(pos));
                ((before != after) == *want).then_some(pos)
            }
            Node::Backref(i) => match caps.get(*i).copied().flatten() {
                Some((s, e)) => {
                    let len = e - s;
                    (t.len() >= pos + len && t[s..e] == t[pos..pos + len]).then_some(pos + len)
                }
                None => Some(pos),
            },
            _ => None,
        }
    }
}
