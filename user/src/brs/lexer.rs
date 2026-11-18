//! Banna Rust sözcük çözümleyicisi.

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Int(i64),
    Float(f64),
    Str(String),
    Char(char),
    Ident(String),
    /// Yaşam süresi etiketi (`'a`): yok sayılır.
    Lifetime,
    P(&'static str),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    pub line: u32,
}

const PUNCT: &[&str] = &[
    "..=", "...", "<<=", ">>=", "::", "->", "=>", "==", "!=", "<=", ">=", "&&", "||", "+=", "-=",
    "*=", "/=", "%=", "^=", "&=", "|=", "<<", ">>", "..", "{", "}", "(", ")", "[", "]", ";", ",",
    ".", ":", "+", "-", "*", "/", "%", "=", "<", ">", "!", "&", "|", "^", "?", "#", "@", "_", "$",
];

pub fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let c: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut line = 1u32;
    let mut out = Vec::new();
    while i < c.len() {
        let ch = c[i];
        if ch == '\n' {
            line += 1;
            i += 1;
            continue;
        }
        if ch.is_whitespace() {
            i += 1;
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'/') {
            while i < c.len() && c[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if ch == '/' && c.get(i + 1) == Some(&'*') {
            let mut depth = 0;
            while i < c.len() {
                if c[i] == '/' && c.get(i + 1) == Some(&'*') {
                    depth += 1;
                    i += 2;
                } else if c[i] == '*' && c.get(i + 1) == Some(&'/') {
                    depth -= 1;
                    i += 2;
                    if depth == 0 {
                        break;
                    }
                } else {
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
            }
            continue;
        }
        let tok = if ch.is_ascii_digit() {
            let start = i;
            let mut float = false;
            if ch == '0' && matches!(c.get(i + 1), Some('x' | 'b' | 'o')) {
                let radix = match c[i + 1] {
                    'x' => 16,
                    'b' => 2,
                    _ => 8,
                };
                i += 2;
                let s = i;
                while i < c.len() && (c[i].is_digit(radix) || c[i] == '_') {
                    i += 1;
                }
                let digits: String = c[s..i].iter().filter(|&&d| d != '_').collect();
                skip_suffix(&c, &mut i);
                let v = i64::from_str_radix(&digits, radix).map_err(|_| alloc::format!("{}. satır: geçersiz sayı", line))?;
                out.push(Token { tok: Tok::Int(v), line });
                continue;
            }
            while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                i += 1;
            }
            if i < c.len() && c[i] == '.' && c.get(i + 1).is_some_and(|d| d.is_ascii_digit()) {
                float = true;
                i += 1;
                while i < c.len() && (c[i].is_ascii_digit() || c[i] == '_') {
                    i += 1;
                }
            }
            if i < c.len() && (c[i] == 'e' || c[i] == 'E') && c.get(i + 1).is_some_and(|d| d.is_ascii_digit() || *d == '-' || *d == '+') {
                float = true;
                i += 2;
                while i < c.len() && c[i].is_ascii_digit() {
                    i += 1;
                }
            }
            let text: String = c[start..i].iter().filter(|&&d| d != '_').collect();
            let suffix_start = i;
            skip_suffix(&c, &mut i);
            let suffix: String = c[suffix_start..i].iter().collect();
            if float || suffix.starts_with('f') {
                Tok::Float(text.parse().map_err(|_| alloc::format!("{}. satır: geçersiz sayı", line))?)
            } else {
                Tok::Int(text.parse().map_err(|_| alloc::format!("{}. satır: sayı çok büyük", line))?)
            }
        } else if ch == '"' || (ch == 'r' && matches!(c.get(i + 1), Some('"' | '#'))) {
            if ch == 'r' {
                i += 1;
                let mut hashes = 0;
                while c.get(i) == Some(&'#') {
                    hashes += 1;
                    i += 1;
                }
                i += 1;
                let start = i;
                loop {
                    if i >= c.len() {
                        return Err(alloc::format!("{}. satır: dizge kapanmadı", line));
                    }
                    if c[i] == '"' && (0..hashes).all(|k| c.get(i + 1 + k) == Some(&'#')) {
                        break;
                    }
                    if c[i] == '\n' {
                        line += 1;
                    }
                    i += 1;
                }
                let s: String = c[start..i].iter().collect();
                i += 1 + hashes;
                Tok::Str(s)
            } else {
                i += 1;
                let mut s = String::new();
                while i < c.len() && c[i] != '"' {
                    if c[i] == '\\' {
                        i += 1;
                        if c.get(i) == Some(&'\n') {
                            line += 1;
                            i += 1;
                            while i < c.len() && c[i].is_whitespace() {
                                i += 1;
                            }
                            continue;
                        }
                        s.push(escape(&c, &mut i));
                    } else {
                        if c[i] == '\n' {
                            line += 1;
                        }
                        s.push(c[i]);
                        i += 1;
                    }
                }
                i += 1;
                Tok::Str(s)
            }
        } else if ch == '\'' {
            // Karakter mi, yaşam süresi mi?
            if c.get(i + 2) == Some(&'\'') && c.get(i + 1) != Some(&'\\') {
                let v = c[i + 1];
                i += 3;
                Tok::Char(v)
            } else if c.get(i + 1) == Some(&'\\') {
                i += 2;
                let v = escape(&c, &mut i);
                if c.get(i) != Some(&'\'') {
                    return Err(alloc::format!("{}. satır: karakter kapanmadı", line));
                }
                i += 1;
                Tok::Char(v)
            } else {
                i += 1;
                while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                    i += 1;
                }
                Tok::Lifetime
            }
        } else if ch.is_alphabetic() || ch == '_' {
            let start = i;
            while i < c.len() && (c[i].is_alphanumeric() || c[i] == '_') {
                i += 1;
            }
            let word: String = c[start..i].iter().collect();
            if word == "_" { Tok::P("_") } else { Tok::Ident(word) }
        } else {
            let rest: String = c[i..(i + 3).min(c.len())].iter().collect();
            let p = PUNCT
                .iter()
                .find(|p| rest.starts_with(**p))
                .ok_or_else(|| alloc::format!("{}. satır: beklenmeyen karakter '{}'", line, ch))?;
            i += p.len();
            Tok::P(p)
        };
        out.push(Token { tok, line });
    }
    out.push(Token { tok: Tok::Eof, line });
    Ok(out)
}

fn skip_suffix(c: &[char], i: &mut usize) {
    let suffixes = ["i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize", "f32", "f64"];
    for s in suffixes {
        let n = s.len();
        if c.len() >= *i + n && c[*i..*i + n].iter().collect::<String>() == s {
            *i += n;
            return;
        }
    }
}

fn escape(c: &[char], i: &mut usize) -> char {
    let e = c.get(*i).copied().unwrap_or('\\');
    *i += 1;
    match e {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        '0' => '\0',
        'x' => {
            let hex: String = c[*i..(*i + 2).min(c.len())].iter().collect();
            *i += 2;
            char::from_u32(u32::from_str_radix(&hex, 16).unwrap_or(63)).unwrap_or('?')
        }
        'u' => {
            let end = c[*i..].iter().position(|&x| x == '}').map_or(c.len(), |p| *i + p);
            let hex: String = c[*i + 1..end].iter().collect();
            *i = end + 1;
            char::from_u32(u32::from_str_radix(&hex, 16).unwrap_or(63)).unwrap_or('?')
        }
        other => other,
    }
}
