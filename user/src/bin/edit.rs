//! `edit [dosya]`: terminal metin editörü (nano benzeri).
//!
//! ^S kaydet, ^Q çık, ^F bul, ^G satıra git, ^K satırı kes, ^U yapıştır,
//! oklar / Home / End / PageUp / PageDown ile gezinme.

#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, format, fs, io, process, time, tty};

banna::entry!(main);

const TAB_WIDTH: usize = 4;
const MESSAGE_MS: u64 = 4000;
const HELP: &str = "^S Kaydet  ^Q Çık  ^F Bul  ^G Satıra git  ^K Satır kes  ^U Yapıştır";

#[derive(Clone, Copy, PartialEq)]
enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Ctrl(u8),
}

/// Ham kipte stdin'den tuş okur; kaçış dizilerini ve UTF-8'i çözer.
struct Input {
    pending: Vec<u8>,
}

impl Input {
    fn fill(&mut self) -> bool {
        let mut buf = [0u8; 256];
        let n = io::read_fd(0, &mut buf);
        if n <= 0 {
            return false;
        }
        self.pending.extend_from_slice(&buf[..n as usize]);
        true
    }

    fn next(&mut self) -> Option<Key> {
        loop {
            if let Some((key, used)) = self.parse() {
                self.pending.drain(..used);
                return Some(key);
            }
            if !self.fill() {
                return None;
            }
        }
    }

    /// Tamamlanmış bir tuş varsa (tuş, tüketilen bayt).
    fn parse(&self) -> Option<(Key, usize)> {
        let p = &self.pending;
        let first = *p.first()?;
        let key = match first {
            0x1b => {
                if p.len() == 1 {
                    return None;
                }
                if p[1] == b'O' {
                    // SS3: F1-F4 ve bazı terminallerde Home/End.
                    let key = match *p.get(2)? {
                        b'H' => Key::Home,
                        b'F' => Key::End,
                        _ => Key::Escape,
                    };
                    return Some((key, 3));
                }
                if p[1] != b'[' {
                    return Some((Key::Escape, 1));
                }
                let end = p[2..].iter().position(|b| (0x40..=0x7e).contains(b))? + 2;
                // Ctrl/Shift değiştiricili oklar (ESC [ 1 ; 5 C) düz ok sayılır.
                let body = &p[2..=end];
                let body = if body.starts_with(b"1;") && body.len() == 4 {
                    &body[3..]
                } else {
                    body
                };
                let key = match body {
                    b"A" => Key::Up,
                    b"B" => Key::Down,
                    b"C" => Key::Right,
                    b"D" => Key::Left,
                    b"H" | b"1~" | b"7~" => Key::Home,
                    b"F" | b"4~" | b"8~" => Key::End,
                    b"3~" => Key::Delete,
                    b"5~" => Key::PageUp,
                    b"6~" => Key::PageDown,
                    _ => Key::Escape,
                };
                return Some((key, end + 1));
            }
            b'\r' | b'\n' => Key::Enter,
            0x7f | 0x08 => Key::Backspace,
            b'\t' => Key::Tab,
            0x00..=0x1f => Key::Ctrl(first + b'a' - 1),
            _ => {
                let len = match first {
                    0xc0..=0xdf => 2,
                    0xe0..=0xef => 3,
                    0xf0..=0xf7 => 4,
                    _ => 1,
                };
                if p.len() < len {
                    return None;
                }
                let ch = core::str::from_utf8(&p[..len])
                    .ok()
                    .and_then(|s| s.chars().next())
                    .unwrap_or('?');
                return Some((Key::Char(ch), len));
            }
        };
        Some((key, 1))
    }
}

struct Editor {
    lines: Vec<Vec<char>>,
    cx: usize,
    cy: usize,
    row_offset: usize,
    col_offset: usize,
    columns: usize,
    rows: usize,
    path: Option<String>,
    dirty: bool,
    message: String,
    message_at: u64,
    clipboard: Vec<Vec<char>>,
    last_was_cut: bool,
    quit_armed: bool,
    search: String,
    input: Input,
}

/// Sekmeleri genişleterek imlecin ekran sütunu.
fn render_x(line: &[char], cx: usize) -> usize {
    line[..cx.min(line.len())].iter().fold(0, |x, &c| {
        if c == '\t' {
            x + TAB_WIDTH - x % TAB_WIDTH
        } else {
            x + 1
        }
    })
}

impl Editor {
    fn text_rows(&self) -> usize {
        self.rows.saturating_sub(2).max(1)
    }

    fn set_message(&mut self, text: String) {
        self.message = text;
        self.message_at = time::uptime_ms();
    }

    fn open(&mut self, path: &str) {
        self.path = Some(fs::normalize(&process::cwd(), path));
        match fs::read(path) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data);
                self.lines = text
                    .split('\n')
                    .map(|l| l.trim_end_matches('\r').chars().collect())
                    .collect();
                if self.lines.len() > 1 && self.lines.last().is_some_and(|l| l.is_empty()) {
                    self.lines.pop();
                }
                self.set_message(format!("{} satır okundu", self.lines.len()));
            }
            Err(-2) => self.set_message(String::from("Yeni dosya")),
            Err(e) => self.set_message(format!("Okunamadı: {}", fs::error_text(e))),
        }
        if self.lines.is_empty() {
            self.lines.push(Vec::new());
        }
    }

    fn save(&mut self) {
        let path = match self.path.clone() {
            Some(p) => p,
            None => match self.prompt("Dosya adı: ", "") {
                Some(name) if !name.is_empty() => {
                    let p = fs::normalize(&process::cwd(), &name);
                    self.path = Some(p.clone());
                    p
                }
                _ => {
                    self.set_message(String::from("Kaydetme iptal edildi"));
                    return;
                }
            },
        };
        let mut text = String::new();
        for line in &self.lines {
            text.extend(line.iter());
            text.push('\n');
        }
        match fs::write(&path, text.as_bytes()) {
            Ok(()) => {
                self.dirty = false;
                self.set_message(format!("{} bayt yazıldı: {}", text.len(), path));
            }
            Err(e) => self.set_message(format!("Kaydedilemedi: {}", fs::error_text(e))),
        }
    }

    fn line_len(&self) -> usize {
        self.lines[self.cy].len()
    }

    fn insert(&mut self, c: char) {
        let cy = self.cy;
        self.lines[cy].insert(self.cx, c);
        self.cx += 1;
        self.dirty = true;
    }

    fn newline(&mut self) {
        let indent: Vec<char> = self.lines[self.cy]
            .iter()
            .take_while(|c| **c == ' ' || **c == '\t')
            .copied()
            .collect();
        let indent: Vec<char> = indent.into_iter().take(self.cx).collect();
        let cy = self.cy;
        let rest = self.lines[cy].split_off(self.cx);
        let mut next = indent.clone();
        next.extend(rest);
        self.lines.insert(cy + 1, next);
        self.cy += 1;
        self.cx = indent.len();
        self.dirty = true;
    }

    fn backspace(&mut self) {
        if self.cx > 0 {
            let cy = self.cy;
            self.cx -= 1;
            self.lines[cy].remove(self.cx);
            self.dirty = true;
        } else if self.cy > 0 {
            let line = self.lines.remove(self.cy);
            self.cy -= 1;
            self.cx = self.line_len();
            let cy = self.cy;
            self.lines[cy].extend(line);
            self.dirty = true;
        }
    }

    fn delete(&mut self) {
        if self.cx < self.line_len() {
            let cy = self.cy;
            self.lines[cy].remove(self.cx);
            self.dirty = true;
        } else if self.cy + 1 < self.lines.len() {
            let next = self.lines.remove(self.cy + 1);
            let cy = self.cy;
            self.lines[cy].extend(next);
            self.dirty = true;
        }
    }

    fn cut_line(&mut self) {
        if !self.last_was_cut {
            self.clipboard.clear();
        }
        let line = if self.lines.len() == 1 {
            core::mem::take(&mut self.lines[0])
        } else {
            let line = self.lines.remove(self.cy);
            if self.cy >= self.lines.len() {
                self.cy = self.lines.len() - 1;
            }
            line
        };
        self.clipboard.push(line);
        self.cx = 0;
        self.dirty = true;
    }

    fn paste(&mut self) {
        if self.clipboard.is_empty() {
            return;
        }
        for (i, line) in self.clipboard.clone().into_iter().enumerate() {
            self.lines.insert(self.cy + i, line);
        }
        self.cy += self.clipboard.len();
        self.cx = 0;
        if self.cy >= self.lines.len() {
            self.lines.push(Vec::new());
        }
        self.dirty = true;
    }

    fn find(&mut self) {
        let previous = self.search.clone();
        let Some(query) = self.prompt("Bul: ", &previous) else {
            return;
        };
        if query.is_empty() {
            return;
        }
        self.search = query.clone();
        let needle: Vec<char> = query.chars().collect();
        let total = self.lines.len();
        for step in 0..=total {
            let y = (self.cy + step) % total;
            let line = &self.lines[y];
            let from = if step == 0 { self.cx + 1 } else { 0 };
            let found = (from..=line.len().saturating_sub(needle.len()))
                .find(|&x| line.len() >= needle.len() && line[x..x + needle.len()] == needle[..]);
            if let Some(x) = found {
                self.cy = y;
                self.cx = x;
                self.row_offset = y.saturating_sub(self.text_rows() / 2);
                return;
            }
        }
        self.set_message(format!("Bulunamadı: {}", query));
    }

    fn goto_line(&mut self) {
        let Some(text) = self.prompt("Satır: ", "") else {
            return;
        };
        match text.trim().parse::<usize>() {
            Ok(n) if n >= 1 => {
                self.cy = (n - 1).min(self.lines.len() - 1);
                self.cx = 0;
                self.row_offset = self.cy.saturating_sub(self.text_rows() / 2);
            }
            _ => self.set_message(String::from("Geçersiz satır numarası")),
        }
    }

    /// Mesaj satırında metin ister; Esc ile `None`.
    fn prompt(&mut self, label: &str, initial: &str) -> Option<String> {
        let mut text: Vec<char> = initial.chars().collect();
        loop {
            self.set_message(format!("{}{}", label, text.iter().collect::<String>()));
            self.draw(Some(label.chars().count() + text.len()));
            match self.input.next()? {
                Key::Enter => {
                    self.message.clear();
                    return Some(text.into_iter().collect());
                }
                Key::Escape | Key::Ctrl(b'q') | Key::Ctrl(b'c') => {
                    self.set_message(String::from("İptal edildi"));
                    return None;
                }
                Key::Backspace => {
                    text.pop();
                }
                Key::Char(c) => text.push(c),
                _ => {}
            }
        }
    }

    fn move_cursor(&mut self, key: Key) {
        let page = self.text_rows();
        match key {
            Key::Up => self.cy = self.cy.saturating_sub(1),
            Key::Down => self.cy = (self.cy + 1).min(self.lines.len() - 1),
            Key::Left => {
                if self.cx > 0 {
                    self.cx -= 1;
                } else if self.cy > 0 {
                    self.cy -= 1;
                    self.cx = self.line_len();
                }
            }
            Key::Right => {
                if self.cx < self.line_len() {
                    self.cx += 1;
                } else if self.cy + 1 < self.lines.len() {
                    self.cy += 1;
                    self.cx = 0;
                }
            }
            Key::Home => self.cx = 0,
            Key::End => self.cx = self.line_len(),
            Key::PageUp => self.cy = self.cy.saturating_sub(page),
            Key::PageDown => self.cy = (self.cy + page).min(self.lines.len() - 1),
            _ => {}
        }
        self.cx = self.cx.min(self.line_len());
    }

    fn scroll(&mut self) {
        let rows = self.text_rows();
        if self.cy < self.row_offset {
            self.row_offset = self.cy;
        }
        if self.cy >= self.row_offset + rows {
            self.row_offset = self.cy + 1 - rows;
        }
        let rx = render_x(&self.lines[self.cy], self.cx);
        if rx < self.col_offset {
            self.col_offset = rx;
        }
        if rx >= self.col_offset + self.columns {
            self.col_offset = rx + 1 - self.columns;
        }
    }

    /// Tüm ekranı tek yazmada çizer; `prompt_cursor` verilirse imleç mesaj satırına gider.
    fn draw(&mut self, prompt_cursor: Option<usize>) {
        // Terminal penceresi boyutlandırıldıysa yeni boyuta uy.
        let (columns, rows) = tty::window_size(0);
        let resized = (columns.max(20), rows.max(5)) != (self.columns, self.rows);
        if resized {
            self.columns = columns.max(20);
            self.rows = rows.max(5);
        }
        self.scroll();
        let mut out = String::from(if resized {
            "\x1b[2J\x1b[?25l"
        } else {
            "\x1b[?25l"
        });
        let rows = self.text_rows();
        for screen_row in 0..rows {
            out.push_str(&format!("\x1b[{};1H\x1b[2K", screen_row + 1));
            let y = self.row_offset + screen_row;
            let Some(line) = self.lines.get(y) else {
                out.push_str("\x1b[90m~\x1b[0m");
                continue;
            };
            let mut x = 0;
            for &c in line {
                let cells = if c == '\t' {
                    TAB_WIDTH - x % TAB_WIDTH
                } else {
                    1
                };
                for _ in 0..cells {
                    if x >= self.col_offset && x < self.col_offset + self.columns {
                        out.push(if c == '\t' { ' ' } else { c });
                    }
                    x += 1;
                }
                if x >= self.col_offset + self.columns {
                    break;
                }
            }
        }

        let name = self.path.as_deref().unwrap_or("[adsız]");
        let left = format!(
            " {}{} - {} satır",
            name,
            if self.dirty { " [değişti]" } else { "" },
            self.lines.len()
        );
        let right = format!("satır {}, sütun {} ", self.cy + 1, self.cx + 1);
        let width = self.columns;
        let mut bar: String = left.chars().take(width).collect();
        let used = bar.chars().count();
        let right_len = right.chars().count();
        if used + right_len < width {
            for _ in used..width - right_len {
                bar.push(' ');
            }
            bar.push_str(&right);
        } else {
            for _ in used..width {
                bar.push(' ');
            }
        }
        out.push_str(&format!("\x1b[{};1H\x1b[2K\x1b[7m{}\x1b[0m", rows + 1, bar));

        let fresh = time::uptime_ms().saturating_sub(self.message_at) < MESSAGE_MS;
        let footer = if !self.message.is_empty() && (fresh || prompt_cursor.is_some()) {
            self.message.as_str()
        } else {
            HELP
        };
        let footer: String = footer.chars().take(width.saturating_sub(1)).collect();
        out.push_str(&format!("\x1b[{};1H\x1b[2K{}", rows + 2, footer));

        match prompt_cursor {
            Some(col) => out.push_str(&format!("\x1b[{};{}H", rows + 2, col + 1)),
            None => {
                let rx = render_x(&self.lines[self.cy], self.cx);
                out.push_str(&format!(
                    "\x1b[{};{}H",
                    self.cy - self.row_offset + 1,
                    rx - self.col_offset + 1
                ));
            }
        }
        out.push_str("\x1b[?25h");
        let _ = io::write_all(1, out.as_bytes());
    }

    /// `false` = çık.
    fn handle(&mut self, key: Key) -> bool {
        let quit_armed = core::mem::replace(&mut self.quit_armed, false);
        let last_was_cut = core::mem::replace(&mut self.last_was_cut, false);
        match key {
            Key::Ctrl(b'q') => {
                if self.dirty && !quit_armed {
                    self.quit_armed = true;
                    self.set_message(String::from(
                        "Kaydedilmemiş değişiklikler var! Kaydetmeden çıkmak için tekrar ^Q",
                    ));
                    return true;
                }
                return false;
            }
            Key::Ctrl(b's') => self.save(),
            Key::Ctrl(b'f') => self.find(),
            Key::Ctrl(b'g') => self.goto_line(),
            Key::Ctrl(b'k') => {
                self.last_was_cut = last_was_cut;
                self.cut_line();
                self.last_was_cut = true;
            }
            Key::Ctrl(b'u') => self.paste(),
            Key::Ctrl(b'a') => self.cx = 0,
            Key::Ctrl(b'e') => self.cx = self.line_len(),
            Key::Enter => self.newline(),
            Key::Backspace => self.backspace(),
            Key::Delete => self.delete(),
            Key::Tab => {
                for _ in 0..TAB_WIDTH - render_x(&self.lines[self.cy], self.cx) % TAB_WIDTH {
                    self.insert(' ');
                }
            }
            Key::Char(c) => self.insert(c),
            Key::Up
            | Key::Down
            | Key::Left
            | Key::Right
            | Key::Home
            | Key::End
            | Key::PageUp
            | Key::PageDown => self.move_cursor(key),
            Key::Escape | Key::Ctrl(_) => {}
        }
        true
    }
}

fn main(args: &[&str]) -> i32 {
    if !tty::is_tty(0) {
        eprintln!("edit: bir terminalde çalıştırılmalı");
        return 1;
    }
    let (columns, rows) = tty::window_size(0);
    let mut editor = Editor {
        lines: Vec::new(),
        cx: 0,
        cy: 0,
        row_offset: 0,
        col_offset: 0,
        columns: columns.max(20),
        rows: rows.max(5),
        path: None,
        dirty: false,
        message: String::new(),
        message_at: 0,
        clipboard: Vec::new(),
        last_was_cut: false,
        quit_armed: false,
        search: String::new(),
        input: Input {
            pending: Vec::new(),
        },
    };
    match args.get(1) {
        Some(path) => editor.open(path),
        None => editor.lines.push(Vec::new()),
    }

    let old_mode = tty::set_mode(0, 0).unwrap_or(tty::DEFAULT_MODE);
    let _ = io::write_all(1, b"\x1b[?1049h");
    loop {
        editor.draw(None);
        match editor.input.next() {
            Some(key) => {
                if !editor.handle(key) {
                    break;
                }
            }
            None => break,
        }
    }
    let _ = io::write_all(1, b"\x1b[2J\x1b[?1049l");
    let _ = tty::set_mode(0, old_mode);
    0
}
