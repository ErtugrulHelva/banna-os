//! JavaScript sözcük çözümleyici: sayılar (onaltılık, ikili, sekizli, ayraçlı),
//! dizgeler (kaçış dizileri), şablon dizgeler, düzenli ifade değişmezleri (bağlama göre),
//! noktalama ve satır sonu bilgisi (otomatik noktalı virgül için).

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Num(f64),
    Str(Rc<str>),
    /// Şablon: ham parçalar (kaçışları çözülmüş) ve `${...}` içindeki kaynaklar.
    Template(Vec<String>, Vec<String>),
    Ident(Rc<str>),
    Punct(&'static str),
    Regex(String, String),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Öncesinde satır sonu var mı (ASI).
    pub newline: bool,
    pub line: u32,
}

const PUNCTS: &[&str] = &[
    ">>>=", "...", "===", "!==", "**=", "<<=", ">>=", ">>>", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%",
    "&", "|", "^", "!", "~", "?", ":", "=", ".", "@", "#",
];

pub fn tokenize(src: &str) -> Result<Vec<Token>, String> {
    let chars: Vec<char> = src.chars().collect();
    let mut i = 0;
    let mut out: Vec<Token> = Vec::new();
    let mut newline = false;
    let mut line = 1u32;
    while i < chars.len() {
        let c = chars[i];
        if c == '\n' || c == '\u{2028}' || c == '\u{2029}' {
            newline = true;
            line += 1;
            i += 1;
            continue;
        }
        if c.is_whitespace() || c == '\u{feff}' {
            i += 1;
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'/') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if c == '/' && chars.get(i + 1) == Some(&'*') {
            i += 2;
            while i + 1 < chars.len() && !(chars[i] == '*' && chars[i + 1] == '/') {
                if chars[i] == '\n' {
                    newline = true;
                    line += 1;
                }
                i += 1;
            }
            i += 2;
            continue;
        }
        // HTML yorum ayıracı (eski betikler).
        if c == '<' && chars[i..].starts_with(&['<', '!', '-', '-']) {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        let tok = if c.is_ascii_digit() || (c == '.' && chars.get(i + 1).is_some_and(|d| d.is_ascii_digit())) {
            Tok::Num(number(&chars, &mut i)?)
        } else if c == '"' || c == '\'' {
            Tok::Str(Rc::from(string(&chars, &mut i, c)?.as_str()))
        } else if c == '`' {
            template(&chars, &mut i, &mut line)?
        } else if c.is_alphabetic() || c == '_' || c == '$' || c == '\\' {
            let start = i;
            while i < chars.len()
                && (chars[i].is_alphanumeric() || chars[i] == '_' || chars[i] == '$' || chars[i] == '\u{200c}' || chars[i] == '\u{200d}')
            {
                i += 1;
            }
            if i == start {
                i += 1;
            }
            Tok::Ident(Rc::from(chars[start..i].iter().collect::<String>().as_str()))
        } else if c == '/' && regex_allowed(out.last()) {
            regex(&chars, &mut i)?
        } else {
            let rest: String = chars[i..(i + 4).min(chars.len())].iter().collect();
            let p = PUNCTS
                .iter()
                .find(|p| rest.starts_with(**p))
                .ok_or_else(|| alloc::format!("beklenmeyen karakter '{}' (satır {})", c, line))?;
            // `?.5` sayısal üçlü koşul, isteğe bağlı zincir değil.
            if *p == "?." && chars.get(i + 2).is_some_and(|d| d.is_ascii_digit()) {
                i += 1;
                Tok::Punct("?")
            } else {
                i += p.chars().count();
                Tok::Punct(p)
            }
        };
        out.push(Token {
            tok,
            newline: core::mem::take(&mut newline),
            line,
        });
    }
    out.push(Token {
        tok: Tok::Eof,
        newline: true,
        line,
    });
    Ok(out)
}

fn regex_allowed(prev: Option<&Token>) -> bool {
    match prev.map(|t| &t.tok) {
        None => true,
        Some(Tok::Num(_) | Tok::Str(_) | Tok::Template(..) | Tok::Regex(..)) => false,
        Some(Tok::Ident(name)) => matches!(
            &**name,
            "return" | "typeof" | "instanceof" | "in" | "of" | "new" | "delete" | "void" | "throw"
                | "case" | "do" | "else" | "yield" | "await"
        ),
        Some(Tok::Punct(p)) => !matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Some(Tok::Eof) => true,
    }
}

fn number(c: &[char], i: &mut usize) -> Result<f64, String> {
    let start = *i;
    if c[*i] == '0' && c.get(*i + 1).is_some_and(|x| matches!(x, 'x' | 'X' | 'b' | 'B' | 'o' | 'O')) {
        let radix = match c[*i + 1] {
            'x' | 'X' => 16,
            'b' | 'B' => 2,
            _ => 8,
        };
        *i += 2;
        let mut v = 0f64;
        while *i < c.len() && (c[*i].is_digit(radix) || c[*i] == '_') {
            if c[*i] != '_' {
                v = v * radix as f64 + c[*i].to_digit(radix).unwrap() as f64;
            }
            *i += 1;
        }
        if c.get(*i) == Some(&'n') {
            *i += 1;
        }
        return Ok(v);
    }
    while *i < c.len() && (c[*i].is_ascii_digit() || c[*i] == '_') {
        *i += 1;
    }
    if *i < c.len() && c[*i] == '.' {
        *i += 1;
        while *i < c.len() && (c[*i].is_ascii_digit() || c[*i] == '_') {
            *i += 1;
        }
    }
    if *i < c.len() && (c[*i] == 'e' || c[*i] == 'E') {
        let save = *i;
        *i += 1;
        if *i < c.len() && (c[*i] == '+' || c[*i] == '-') {
            *i += 1;
        }
        if *i < c.len() && c[*i].is_ascii_digit() {
            while *i < c.len() && c[*i].is_ascii_digit() {
                *i += 1;
            }
        } else {
            *i = save;
        }
    }
    let text: String = c[start..*i].iter().filter(|&&ch| ch != '_').collect();
    if c.get(*i) == Some(&'n') {
        *i += 1;
    }
    text.parse::<f64>()
        .map_err(|_| alloc::format!("geçersiz sayı {}", text))
}

fn escape(c: &[char], i: &mut usize, out: &mut String) {
    let e = c.get(*i).copied().unwrap_or('\\');
    *i += 1;
    match e {
        'n' => out.push('\n'),
        't' => out.push('\t'),
        'r' => out.push('\r'),
        'b' => out.push('\u{8}'),
        'f' => out.push('\u{c}'),
        'v' => out.push('\u{b}'),
        '0' if !c.get(*i).is_some_and(|d| d.is_ascii_digit()) => out.push('\0'),
        'x' => {
            let hex: String = c[*i..(*i + 2).min(c.len())].iter().collect();
            *i += 2;
            if let Some(ch) = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32) {
                out.push(ch);
            }
        }
        'u' => {
            let code = if c.get(*i) == Some(&'{') {
                let end = c[*i..].iter().position(|&x| x == '}').map_or(c.len(), |p| *i + p);
                let hex: String = c[*i + 1..end].iter().collect();
                *i = end + 1;
                u32::from_str_radix(&hex, 16).unwrap_or(0xfffd)
            } else {
                let hex: String = c[*i..(*i + 4).min(c.len())].iter().collect();
                *i += 4;
                u32::from_str_radix(&hex, 16).unwrap_or(0xfffd)
            };
            // Vekil çiftler: ardından gelen düşük vekille birleştir.
            if (0xd800..0xdc00).contains(&code) && c.get(*i) == Some(&'\\') && c.get(*i + 1) == Some(&'u') {
                let hex: String = c[*i + 2..(*i + 6).min(c.len())].iter().collect();
                if let Ok(low) = u32::from_str_radix(&hex, 16)
                    && (0xdc00..0xe000).contains(&low)
                {
                    *i += 6;
                    let full = 0x10000 + ((code - 0xd800) << 10) + (low - 0xdc00);
                    out.push(char::from_u32(full).unwrap_or('\u{fffd}'));
                    return;
                }
            }
            out.push(char::from_u32(code).unwrap_or('\u{fffd}'));
        }
        '\r' => {
            if c.get(*i) == Some(&'\n') {
                *i += 1;
            }
        }
        '\n' | '\u{2028}' | '\u{2029}' => {}
        other => out.push(other),
    }
}

fn string(c: &[char], i: &mut usize, quote: char) -> Result<String, String> {
    *i += 1;
    let mut out = String::new();
    while *i < c.len() && c[*i] != quote {
        if c[*i] == '\\' {
            *i += 1;
            escape(c, i, &mut out);
        } else {
            if c[*i] == '\n' {
                return Err(String::from("dizge kapanmadan satır bitti"));
            }
            out.push(c[*i]);
            *i += 1;
        }
    }
    *i += 1;
    Ok(out)
}

fn template(c: &[char], i: &mut usize, line: &mut u32) -> Result<Tok, String> {
    *i += 1;
    let mut parts = Vec::new();
    let mut exprs = Vec::new();
    let mut current = String::new();
    while *i < c.len() && c[*i] != '`' {
        match c[*i] {
            '\\' => {
                *i += 1;
                escape(c, i, &mut current);
            }
            '$' if c.get(*i + 1) == Some(&'{') => {
                *i += 2;
                let start = *i;
                let mut depth = 1;
                while *i < c.len() {
                    match c[*i] {
                        '{' => depth += 1,
                        '}' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                        }
                        '`' => {
                            // İç içe şablon: kapanışına atla.
                            *i += 1;
                            while *i < c.len() && c[*i] != '`' {
                                if c[*i] == '\\' {
                                    *i += 1;
                                }
                                *i += 1;
                            }
                        }
                        '"' | '\'' => {
                            let q = c[*i];
                            *i += 1;
                            while *i < c.len() && c[*i] != q {
                                if c[*i] == '\\' {
                                    *i += 1;
                                }
                                *i += 1;
                            }
                        }
                        '\n' => *line += 1,
                        _ => {}
                    }
                    *i += 1;
                }
                exprs.push(c[start..(*i).min(c.len())].iter().collect());
                parts.push(core::mem::take(&mut current));
                *i += 1;
            }
            ch => {
                if ch == '\n' {
                    *line += 1;
                }
                current.push(ch);
                *i += 1;
            }
        }
    }
    *i += 1;
    parts.push(current);
    Ok(Tok::Template(parts, exprs))
}

fn regex(c: &[char], i: &mut usize) -> Result<Tok, String> {
    *i += 1;
    let mut pattern = String::new();
    let mut class = false;
    while *i < c.len() {
        let ch = c[*i];
        if ch == '\n' {
            return Err(String::from("düzenli ifade kapanmadı"));
        }
        if ch == '\\' {
            pattern.push(ch);
            *i += 1;
            if *i < c.len() {
                pattern.push(c[*i]);
            }
            *i += 1;
            continue;
        }
        if ch == '[' {
            class = true;
        } else if ch == ']' {
            class = false;
        } else if ch == '/' && !class {
            break;
        }
        pattern.push(ch);
        *i += 1;
    }
    *i += 1;
    let mut flags = String::new();
    while *i < c.len() && c[*i].is_ascii_alphabetic() {
        flags.push(c[*i]);
        *i += 1;
    }
    Ok(Tok::Regex(pattern, flags))
}
