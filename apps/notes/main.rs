//! Metin Düzenleyici: tek aralıklı yazıyla dosya düzenleme. Fare/Shift+ok ile seçim,
//! Ctrl+C/X/V pano, Ctrl+Z/Y geri al/yinele, Ctrl+S kaydet, Ctrl+O aç, Ctrl+N yeni.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use banna::gfx::text::UiFonts;
use banna::gfx::{Rect, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
    EVENT_WHEEL, KEY_CTRL, KEY_DELETE, KEY_DOWN, KEY_END, KEY_HOME, KEY_LEFT, KEY_PAGE_DOWN,
    KEY_PAGE_UP, KEY_PASTE, KEY_RIGHT, KEY_SHIFT, KEY_UP, Window, clipboard,
};
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, Ui};
use banna::{desktop, fs, process, time};

#[path = "../../src/drivers/font.rs"]
#[allow(dead_code)]
mod font;

banna::entry!(main);

const UNDO_LIMIT: usize = 100;

type Pos = (usize, usize);

#[derive(Clone, PartialEq)]
enum Control {
    New,
    Open,
    Save,
    SaveAs,
    Field,
    Confirm,
    Cancel,
    Text,
}

#[derive(Clone, Copy, PartialEq)]
enum Prompt {
    Open,
    SaveAs,
}

#[derive(Clone, Copy, PartialEq)]
enum Edit {
    Other,
    Typing,
}

struct Snapshot {
    lines: Vec<Vec<char>>,
    cursor: Pos,
}

struct Notes {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    path: Option<String>,
    lines: Vec<Vec<char>>,
    cursor: Pos,
    anchor: Option<Pos>,
    /// Dikey gezinmede korunan sütun.
    goal: usize,
    top: usize,
    left: usize,
    modified: bool,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: Edit,
    prompt: Option<Prompt>,
    field: ui::TextInput,
    message: Option<(String, bool)>,
    close_warned: bool,
    dragging: bool,
    last_click: (Pos, u64),
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    home: String,
    dirty: bool,
}

fn word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl Notes {
    fn cell(&self) -> (i32, i32) {
        (
            font::WIDTH as i32 * self.u,
            font::HEIGHT as i32 * self.u + 2 * self.u,
        )
    }

    fn text_area(&self) -> Rect {
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        Rect::new(0, 56 * u, w, h - 56 * u - 28 * u)
    }

    fn gutter(&self) -> i32 {
        let digits = (self.lines.len().max(1).ilog10() + 1).max(3) as i32;
        (digits + 2) * self.cell().0
    }

    fn visible(&self) -> (usize, usize) {
        let area = self.text_area();
        let (cw, ch) = self.cell();
        let rows = ((area.h - 8 * self.u) / ch).max(1) as usize;
        let cols = ((area.w - self.gutter() - 8 * self.u) / cw).max(1) as usize;
        (rows, cols)
    }

    fn title(&self) -> String {
        let name = self
            .path
            .as_deref()
            .and_then(|p| p.rsplit('/').next())
            .unwrap_or("Adsız");
        format!("{}{}", if self.modified { "• " } else { "" }, name)
    }

    fn load(&mut self, path: &str) {
        match fs::read(path) {
            Ok(data) => {
                let text = String::from_utf8_lossy(&data)
                    .replace("\r\n", "\n")
                    .replace('\t', "    ");
                self.lines = text.split('\n').map(|l| l.chars().collect()).collect();
                self.message = Some((format!("Açıldı: {}", path), false));
            }
            Err(fs::ENOENT) => {
                self.lines = vec![Vec::new()];
                self.message = Some((format!("Yeni dosya: {}", path), false));
            }
            Err(e) => {
                self.message = Some((format!("{}: {}", path, fs::error_text(e)), true));
                return;
            }
        }
        self.path = Some(String::from(path));
        self.reset_view();
    }

    fn reset_view(&mut self) {
        self.cursor = (0, 0);
        self.anchor = None;
        self.top = 0;
        self.left = 0;
        self.modified = false;
        self.undo.clear();
        self.redo.clear();
        self.close_warned = false;
        self.dirty = true;
    }

    fn save(&mut self, path: &str) -> bool {
        let mut text = String::new();
        for (i, line) in self.lines.iter().enumerate() {
            if i > 0 {
                text.push('\n');
            }
            text.extend(line.iter());
        }
        match fs::write(path, text.as_bytes()) {
            Ok(()) => {
                self.path = Some(String::from(path));
                self.modified = false;
                self.close_warned = false;
                self.message = Some((format!("Kaydedildi: {}", path), false));
                true
            }
            Err(e) => {
                self.message = Some((format!("Kaydedilemedi: {}", fs::error_text(e)), true));
                false
            }
        }
    }

    fn checkpoint(&mut self, kind: Edit) {
        if kind == Edit::Typing && self.last_edit == Edit::Typing {
            return;
        }
        self.last_edit = kind;
        self.undo.push(Snapshot {
            lines: self.lines.clone(),
            cursor: self.cursor,
        });
        if self.undo.len() > UNDO_LIMIT {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn restore(&mut self, from_undo: bool) {
        let (from, to) = if from_undo {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        if let Some(s) = from.pop() {
            to.push(Snapshot {
                lines: core::mem::replace(&mut self.lines, s.lines),
                cursor: self.cursor,
            });
            self.cursor = s.cursor;
            self.anchor = None;
            self.modified = true;
            self.last_edit = Edit::Other;
        }
    }

    fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.cursor {
            return None;
        }
        Some(if a < self.cursor {
            (a, self.cursor)
        } else {
            (self.cursor, a)
        })
    }

    fn selected_text(&self) -> Option<String> {
        let (s, e) = self.selection()?;
        let mut out = String::new();
        for line in s.0..=e.0 {
            let l = &self.lines[line];
            let from = if line == s.0 { s.1 } else { 0 };
            let to = if line == e.0 { e.1 } else { l.len() };
            out.extend(l[from.min(l.len())..to.min(l.len())].iter());
            if line != e.0 {
                out.push('\n');
            }
        }
        Some(out)
    }

    fn delete_selection(&mut self) -> bool {
        let Some((s, e)) = self.selection() else {
            self.anchor = None;
            return false;
        };
        let tail: Vec<char> = self.lines[e.0][e.1..].to_vec();
        self.lines[s.0].truncate(s.1);
        self.lines[s.0].extend(tail);
        self.lines.drain(s.0 + 1..=e.0);
        self.cursor = s;
        self.anchor = None;
        self.modified = true;
        true
    }

    fn insert(&mut self, text: &str) {
        self.delete_selection();
        let (line, col) = self.cursor;
        let tail: Vec<char> = self.lines[line].split_off(col);
        let mut parts = text.split('\n');
        let first: Vec<char> = parts.next().unwrap_or("").chars().collect();
        let mut col = col + first.len();
        self.lines[line].extend(first);
        let mut line = line;
        for part in parts {
            line += 1;
            let chars: Vec<char> = part.chars().collect();
            col = chars.len();
            self.lines.insert(line, chars);
        }
        self.lines[line].extend(tail);
        self.cursor = (line, col);
        self.goal = col;
        self.modified = true;
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (line, col) = self.cursor;
        if col > 0 {
            self.lines[line].remove(col - 1);
            self.cursor.1 -= 1;
        } else if line > 0 {
            let removed = self.lines.remove(line);
            let prev = self.lines[line - 1].len();
            self.lines[line - 1].extend(removed);
            self.cursor = (line - 1, prev);
        } else {
            return;
        }
        self.goal = self.cursor.1;
        self.modified = true;
    }

    fn delete_forward(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (line, col) = self.cursor;
        if col < self.lines[line].len() {
            self.lines[line].remove(col);
        } else if line + 1 < self.lines.len() {
            let next = self.lines.remove(line + 1);
            self.lines[line].extend(next);
        } else {
            return;
        }
        self.modified = true;
    }

    /// Ctrl+←/→: önceki kelimenin başına ya da sonraki kelimenin sonuna.
    fn move_word(&mut self, forward: bool) {
        let (mut line, mut col) = self.cursor;
        let word = |c: char| c.is_alphanumeric();
        if forward {
            if col == self.lines[line].len() && line + 1 < self.lines.len() {
                line += 1;
                col = 0;
            }
            let row = &self.lines[line];
            while col < row.len() && !word(row[col]) {
                col += 1;
            }
            while col < row.len() && word(row[col]) {
                col += 1;
            }
        } else {
            if col == 0 && line > 0 {
                line -= 1;
                col = self.lines[line].len();
            }
            let row = &self.lines[line];
            while col > 0 && !word(row[col - 1]) {
                col -= 1;
            }
            while col > 0 && word(row[col - 1]) {
                col -= 1;
            }
        }
        self.cursor = (line, col);
        self.goal = col;
    }

    fn move_cursor(&mut self, key: u32) {
        let (rows, _) = self.visible();
        let (mut line, mut col) = self.cursor;
        let mut vertical = false;
        match key {
            KEY_LEFT => {
                if col > 0 {
                    col -= 1;
                } else if line > 0 {
                    line -= 1;
                    col = self.lines[line].len();
                }
            }
            KEY_RIGHT => {
                if col < self.lines[line].len() {
                    col += 1;
                } else if line + 1 < self.lines.len() {
                    line += 1;
                    col = 0;
                }
            }
            KEY_UP => {
                line = line.saturating_sub(1);
                vertical = true;
            }
            KEY_DOWN => {
                line = (line + 1).min(self.lines.len() - 1);
                vertical = true;
            }
            KEY_PAGE_UP => {
                line = line.saturating_sub(rows);
                vertical = true;
            }
            KEY_PAGE_DOWN => {
                line = (line + rows).min(self.lines.len() - 1);
                vertical = true;
            }
            KEY_HOME => {
                // Önce girintinin sonuna, sonra satır başına.
                let indent = self.lines[line].iter().take_while(|c| **c == ' ').count();
                col = if col == indent { 0 } else { indent };
            }
            KEY_END => col = self.lines[line].len(),
            _ => {}
        }
        if vertical {
            col = self.goal.min(self.lines[line].len());
        } else {
            self.goal = col;
        }
        self.cursor = (line, col);
    }

    fn scroll_to_cursor(&mut self) {
        let (rows, cols) = self.visible();
        let (line, col) = self.cursor;
        if line < self.top {
            self.top = line;
        } else if line >= self.top + rows {
            self.top = line + 1 - rows;
        }
        if col < self.left {
            self.left = col.saturating_sub(8);
        } else if col >= self.left + cols {
            self.left = col + 8 - cols.min(col + 8);
        }
    }

    fn position_at(&self, x: i32, y: i32) -> Pos {
        let area = self.text_area();
        let (cw, ch) = self.cell();
        let row = ((y - area.y - 4 * self.u).max(0) / ch) as usize;
        let line = (self.top + row).min(self.lines.len() - 1);
        let col = ((x - area.x - self.gutter() + cw / 2).max(0) / cw) as usize + self.left;
        (line, col.min(self.lines[line].len()))
    }

    fn select_word(&mut self, pos: Pos) {
        let l = &self.lines[pos.0];
        let mut s = pos.1;
        let mut e = pos.1;
        while s > 0 && word_char(l[s - 1]) {
            s -= 1;
        }
        while e < l.len() && word_char(l[e]) {
            e += 1;
        }
        self.anchor = Some((pos.0, s));
        self.cursor = (pos.0, e);
    }

    fn start_prompt(&mut self, prompt: Prompt) {
        self.prompt = Some(prompt);
        self.field = self
            .path
            .clone()
            .unwrap_or_else(|| format!("{}/", self.home))
            .into();
        self.dirty = true;
    }

    fn confirm_prompt(&mut self) {
        let path = fs::normalize(&self.home, self.field.trim());
        match self.prompt.take() {
            Some(Prompt::Open) => self.load(&path),
            Some(Prompt::SaveAs) => {
                self.save(&path);
            }
            None => {}
        }
        self.dirty = true;
    }

    fn save_current(&mut self) {
        match self.path.clone() {
            Some(p) => {
                self.save(&p);
            }
            None => self.start_prompt(Prompt::SaveAs),
        }
    }

    fn new_document(&mut self) {
        self.path = None;
        self.lines = vec![Vec::new()];
        self.message = None;
        self.reset_view();
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(control) = ui::hit_at(&self.hits, x, y).map(|h| h.control.clone()) else {
            return;
        };
        match control {
            Control::New => self.new_document(),
            Control::Open => self.start_prompt(Prompt::Open),
            Control::Save => self.save_current(),
            Control::SaveAs => self.start_prompt(Prompt::SaveAs),
            Control::Confirm => self.confirm_prompt(),
            Control::Cancel => self.prompt = None,
            Control::Field => {}
            Control::Text => {
                self.prompt = None;
                let pos = self.position_at(x, y);
                let now = time::uptime_ms();
                if self.last_click.0 == pos && now - self.last_click.1 < 400 {
                    self.select_word(pos);
                } else {
                    self.cursor = pos;
                    self.anchor = Some(pos);
                    self.dragging = true;
                }
                self.goal = self.cursor.1;
                self.last_click = (pos, now);
                self.last_edit = Edit::Other;
            }
        }
        self.dirty = true;
    }

    fn drag(&mut self, x: i32, y: i32) {
        if self.dragging {
            let area = self.text_area();
            if y < area.y && self.top > 0 {
                self.top -= 1;
            } else if y > area.bottom() && self.top + 1 < self.lines.len() {
                self.top += 1;
            }
            self.cursor = self.position_at(x, y.clamp(area.y, area.bottom() - 1));
            self.dirty = true;
        }
    }

    fn key(&mut self, key: u32) {
        if self.prompt.is_some() {
            match key {
                13 => self.confirm_prompt(),
                27 => self.prompt = None,
                KEY_PASTE | 22 => self.field.push_str(clipboard::get().trim()),
                k => {
                    self.field.key(k, 240);
                }
            }
            self.dirty = true;
            return;
        }
        self.close_warned = false;
        let shift = key & KEY_SHIFT != 0;
        let ctrl = key & KEY_CTRL != 0;
        let base = key & !(KEY_SHIFT | KEY_CTRL);
        match base {
            KEY_LEFT | KEY_RIGHT | KEY_UP | KEY_DOWN | KEY_HOME | KEY_END | KEY_PAGE_UP
            | KEY_PAGE_DOWN => {
                if shift {
                    if self.anchor.is_none() {
                        self.anchor = Some(self.cursor);
                    }
                } else if let Some((s, e)) = self.selection()
                    && matches!(base, KEY_LEFT | KEY_RIGHT)
                {
                    self.cursor = if base == KEY_LEFT { s } else { e };
                    self.anchor = None;
                    self.goal = self.cursor.1;
                    self.scroll_to_cursor();
                    self.dirty = true;
                    return;
                } else {
                    self.anchor = None;
                }
                if ctrl && matches!(base, KEY_LEFT | KEY_RIGHT) {
                    self.move_word(base == KEY_RIGHT);
                } else {
                    self.move_cursor(base);
                }
                self.last_edit = Edit::Other;
            }
            1 => {
                let last = self.lines.len() - 1;
                self.anchor = Some((0, 0));
                self.cursor = (last, self.lines[last].len());
            }
            3 | 24 => {
                if let Some(text) = self.selected_text() {
                    clipboard::set(&text);
                    if base == 24 {
                        self.checkpoint(Edit::Other);
                        self.delete_selection();
                    }
                }
            }
            22 | KEY_PASTE => {
                let text = clipboard::get().replace("\r\n", "\n").replace('\t', "    ");
                if !text.is_empty() {
                    self.checkpoint(Edit::Other);
                    self.insert(&text);
                }
            }
            26 => self.restore(true),
            25 => self.restore(false),
            19 => self.save_current(),
            15 => self.start_prompt(Prompt::Open),
            14 => self.new_document(),
            8 => {
                self.checkpoint(Edit::Other);
                self.backspace();
            }
            KEY_DELETE => {
                self.checkpoint(Edit::Other);
                self.delete_forward();
            }
            13 => {
                self.checkpoint(Edit::Other);
                let indent: String = self.lines[self.cursor.0]
                    .iter()
                    .take(self.cursor.1)
                    .take_while(|c| **c == ' ')
                    .collect();
                self.insert(&format!("\n{}", indent));
            }
            9 => {
                self.checkpoint(Edit::Typing);
                let pad = 4 - self.cursor.1 % 4;
                self.insert(&" ".repeat(pad));
            }
            k if (0x20..0xf700).contains(&k) => {
                if let Some(c) = char::from_u32(k).filter(|c| !c.is_control()) {
                    self.checkpoint(if c == ' ' { Edit::Other } else { Edit::Typing });
                    let mut b = [0u8; 4];
                    self.insert(c.encode_utf8(&mut b));
                }
            }
            _ => return,
        }
        self.scroll_to_cursor();
        self.dirty = true;
    }

    fn render(&mut self) {
        self.dirty = false;
        let u = self.u;
        let t = self.theme;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let area = self.text_area();
        let (rows, cols) = self.visible();
        let (cw, ch) = self.cell();
        let gutter = self.gutter();
        let editor_bg = if t.dark { 0xff10_141c } else { 0xffff_ffff };
        let selection = self.selection();
        let title = self.title();
        let mut hits = Vec::new();
        {
            let mut c = self.window.canvas();
            c.fill(Rect::new(0, 0, w, h), t.window_bg);
            c.fill(area, editor_bg);
            c.fill(
                Rect::new(area.x, area.y, gutter - cw / 2, area.h),
                t.sidebar,
            );
            // Seçim ve imleç satırı.
            for row in 0..rows {
                let line = self.top + row;
                if line >= self.lines.len() {
                    break;
                }
                let y = area.y + 4 * u + row as i32 * ch;
                if line == self.cursor.0 && selection.is_none() {
                    c.fill(
                        Rect::new(area.x + gutter - cw / 2, y, area.w, ch),
                        with_alpha(t.accent, 22),
                    );
                }
                if let Some((s, e)) = selection
                    && line >= s.0
                    && line <= e.0
                {
                    let len = self.lines[line].len();
                    let from = if line == s.0 { s.1 } else { 0 };
                    let to = if line == e.0 { e.1 } else { len + 1 };
                    let from = from.saturating_sub(self.left).min(cols) as i32;
                    let to = to.saturating_sub(self.left).min(cols + 1) as i32;
                    if to > from {
                        c.fill(
                            Rect::new(area.x + gutter + from * cw, y, (to - from) * cw, ch),
                            with_alpha(t.accent, 90),
                        );
                    }
                }
            }
            let f = &self.fonts;
            let mut ui = Ui::new(
                &mut c,
                t,
                f,
                u,
                Rect::new(0, 0, w, h),
                0,
                &mut hits,
                self.hover,
            );
            ui.hit(area, Control::Text);
            let by = 11 * u;
            let bh = 34 * u;
            let mut x = 12 * u;
            if let Some(prompt) = self.prompt {
                let label = if prompt == Prompt::Open {
                    "Aç"
                } else {
                    "Kaydet"
                };
                let field = Rect::new(x, by, w - x - 200 * u, bh);
                ui.input_field(
                    field,
                    "Dosya yolu",
                    &self.field,
                    false,
                    true,
                    Control::Field,
                );
                ui.primary_button(
                    Rect::new(field.right() + 8 * u, by, 90 * u, bh),
                    label,
                    Control::Confirm,
                );
                ui.secondary_button(
                    Rect::new(field.right() + 104 * u, by, 84 * u, bh),
                    "Vazgeç",
                    Control::Cancel,
                );
            } else {
                for (label, control) in [
                    ("Yeni", Control::New),
                    ("Aç", Control::Open),
                    ("Kaydet", Control::Save),
                    ("Farklı kaydet", Control::SaveAs),
                ] {
                    let bw = ui.f.body.measure(label) + 26 * u;
                    ui.secondary_button(Rect::new(x, by, bw, bh), label, control);
                    x += bw + 6 * u;
                }
                let tw = ui.f.title.measure(&title);
                let tx = (w - 16 * u - tw).max(x + 12 * u);
                ui.f.title
                    .draw_clipped(ui.c, tx, by + 8 * u, &title, w - tx - 16 * u, t.text);
            }
            // Durum çubuğu.
            let bar = Rect::new(0, area.bottom(), w, h - area.bottom());
            ui.c.fill(bar, t.sidebar);
            let pos = format!(
                "Satır {}, Sütun {}  •  {} satır",
                self.cursor.0 + 1,
                self.cursor.1 + 1,
                self.lines.len()
            );
            let pw = ui.f.small.measure(&pos);
            ui.f.small
                .draw(ui.c, w - 14 * u - pw, bar.y + 6 * u, &pos, t.muted);
            if let Some((m, error)) = &self.message {
                let color = if *error { ui::DANGER } else { t.muted };
                ui.f.small
                    .draw_clipped(ui.c, 14 * u, bar.y + 6 * u, m, w - pw - 44 * u, color);
            }
            drop(ui);
        }
        // Metin: 8×16 bitmap yazı tipi, `u` kat büyütülmüş.
        let text_color = t.text;
        let number_color = t.muted;
        let digits = (gutter / cw - 2) as usize;
        let width = self.window.width;
        let clip_right = (area.right() - 4 * u).max(0) as usize;
        for row in 0..rows {
            let line = self.top + row;
            if line >= self.lines.len() {
                break;
            }
            let y = (area.y + 4 * u + row as i32 * ch + u) as usize;
            let number = format!("{:>width$}", line + 1, width = digits);
            for (i, b) in number.bytes().enumerate() {
                let x = (area.x + cw / 2 + i as i32 * cw) as usize;
                draw_glyph(
                    &mut self.window.pixels,
                    width,
                    x,
                    y,
                    u as usize,
                    b,
                    number_color,
                    clip_right,
                );
            }
            for (i, c) in self.lines[line]
                .iter()
                .skip(self.left)
                .take(cols)
                .enumerate()
            {
                if *c == ' ' {
                    continue;
                }
                let x = (area.x + gutter + i as i32 * cw) as usize;
                draw_glyph(
                    &mut self.window.pixels,
                    width,
                    x,
                    y,
                    u as usize,
                    font::encode(*c),
                    text_color,
                    clip_right,
                );
            }
        }
        // İmleç.
        if self.prompt.is_none()
            && self.cursor.0 >= self.top
            && self.cursor.0 < self.top + rows
            && self.cursor.1 >= self.left
        {
            let x = area.x + gutter + (self.cursor.1 - self.left) as i32 * cw;
            let y = area.y + 4 * u + (self.cursor.0 - self.top) as i32 * ch;
            if x < area.right() {
                let mut c = self.window.canvas();
                c.fill(Rect::new(x, y, 2 * u, ch), t.accent);
            }
        }
        self.window.present();
        self.hits = hits;
    }
}

#[allow(clippy::too_many_arguments)]
fn draw_glyph(
    pixels: &mut [u32],
    width: usize,
    x: usize,
    y: usize,
    scale: usize,
    byte: u8,
    color: u32,
    clip_right: usize,
) {
    for (gy, bits) in font::glyph(byte).iter().enumerate() {
        if *bits == 0 {
            continue;
        }
        for sy in 0..scale {
            let row = (y + gy * scale + sy) * width;
            for gx in 0..8 {
                if bits & (0x80 >> gx) == 0 {
                    continue;
                }
                let px = x + gx * scale;
                if px + scale > clip_right {
                    return;
                }
                if let Some(span) = pixels.get_mut(row + px..row + px + scale) {
                    span.fill(color);
                }
            }
        }
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("Metin Düzenleyici", (820 * u) as usize, (580 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("notes: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let info = desktop::query();
    let theme = info
        .as_ref()
        .map(|i| Theme::from(&Prefs::parse(&i.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    let home = info
        .as_ref()
        .map(|i| String::from(i.get("user.home")))
        .filter(|h| !h.is_empty())
        .unwrap_or_else(|| format!("/home/{}", process::username()));
    let mut app = Notes {
        window,
        fonts: ui::fonts(),
        u,
        theme,
        path: None,
        lines: vec![Vec::new()],
        cursor: (0, 0),
        anchor: None,
        goal: 0,
        top: 0,
        left: 0,
        modified: false,
        undo: Vec::new(),
        redo: Vec::new(),
        last_edit: Edit::Other,
        prompt: None,
        field: ui::TextInput::default(),
        message: None,
        close_warned: false,
        dragging: false,
        last_click: ((usize::MAX, 0), 0),
        hits: Vec::new(),
        hover: None,
        home,
        dirty: true,
    };
    if let Some(path) = args.get(1) {
        let path = fs::normalize(&process::cwd(), path);
        app.load(&path);
    }
    banna::println!("notes: ready");
    loop {
        if app.dirty {
            app.render();
        }
        let Some(event) = app.window.event(10_000) else {
            continue;
        };
        match event.kind {
            EVENT_CLOSE => {
                if !app.modified || app.close_warned {
                    return 0;
                }
                app.close_warned = true;
                app.message = Some((
                    String::from(
                        "Kaydedilmemiş değişiklikler var. Kaydetmeden çıkmak için yeniden kapatın.",
                    ),
                    true,
                ));
                app.dirty = true;
            }
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                app.scroll_to_cursor();
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_MOUSE_UP => app.dragging = false,
            EVENT_MOUSE_MOVE => {
                app.drag(event.x, event.y);
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    app.dirty = true;
                }
            }
            EVENT_WHEEL => {
                let steps = event.key as i32 * 3;
                let max = app.lines.len().saturating_sub(1) as i32;
                app.top = (app.top as i32 + steps).clamp(0, max) as usize;
                app.dirty = true;
            }
            EVENT_KEY => app.key(event.key),
            _ => {}
        }
    }
}
