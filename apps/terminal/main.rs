//! Masaüstü terminali: pencere + pty, içinde `/bin/sh` çalışır.
//!
//! ANSI/VT100 alt kümesini yorumlar (imleç hareketi, silme, SGR renkleri) ve
//! 1000 satırlık geri kaydırma tutar (PageUp/PageDown).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::collections::VecDeque;
use banna::gui::{
    EVENT_CLOSE, EVENT_CONTEXT, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP,
    EVENT_RESIZE, KEY_PASTE, Window, clipboard,
};
use banna::theme::{Prefs, Theme};
use banna::ui::ContextMenu;

/// Sağ tık menüsü eylemleri.
#[derive(Clone, Copy)]
enum MenuAction {
    Copy,
    Paste,
    NewWindow,
    Clear,
}
use banna::io::{self, PollEntry};
use banna::{String, Vec, process, time, tty, vec};

#[path = "../../src/drivers/font.rs"]
mod font;

banna::entry!(main);

const COLUMNS: usize = 96;
const ROWS: usize = 28;
const SCROLLBACK: usize = 1000;
const FRAME_MS: u64 = 16;
const SIGHUP: u32 = 1;

const BG: u32 = 0x0b0e14;
const FG: u32 = 0xe6e6e6;
const PALETTE: [u32; 16] = [
    0x3a3f4b, 0xff5f5f, 0x5fd75f, 0xe5c07b, 0x61afef, 0xc678dd, 0x5fd7ff, 0xe6e6e6, 0x808690,
    0xff8787, 0x87ff87, 0xffd787, 0x87afff, 0xd787ff, 0x87d7ff, 0xffffff,
];

const KEY_UP: u32 = 0xf700;
const KEY_DOWN: u32 = 0xf701;
const KEY_LEFT: u32 = 0xf702;
const KEY_RIGHT: u32 = 0xf703;
const KEY_HOME: u32 = 0xf704;
const KEY_END: u32 = 0xf705;
const KEY_DELETE: u32 = 0xf706;
const KEY_PAGE_UP: u32 = 0xf707;
const KEY_PAGE_DOWN: u32 = 0xf708;
const KEY_INSERT: u32 = 0xf709;
const KEY_F1: u32 = 0xf721;
const KEY_CTRL: u32 = 0x2_0000;

fn arrow_letter(code: u32) -> Option<u8> {
    match code {
        KEY_UP => Some(b'A'),
        KEY_DOWN => Some(b'B'),
        KEY_RIGHT => Some(b'C'),
        KEY_LEFT => Some(b'D'),
        KEY_HOME => Some(b'H'),
        KEY_END => Some(b'F'),
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Cell {
    glyph: u8,
    fg: u32,
    bg: u32,
}

const BLANK: Cell = Cell {
    glyph: b' ',
    fg: FG,
    bg: BG,
};

enum Parse {
    Ground,
    Escape,
    Csi {
        params: Vec<u16>,
        current: Option<u16>,
        private: bool,
    },
}

struct Screen {
    cols: usize,
    rows: usize,
    lines: VecDeque<Vec<Cell>>,
    row: usize,
    col: usize,
    saved: (usize, usize),
    fg: u32,
    bg: u32,
    bright: bool,
    pending_wrap: bool,
    cursor_visible: bool,
    /// Görünüm, en alttan kaç satır yukarıda.
    scroll: usize,
    /// Tam ekran programlar için alternatif ekran: asıl ekranın satırları ve imleci.
    primary: Option<(VecDeque<Vec<Cell>>, usize, usize)>,
    dirty: bool,
    parse: Parse,
    utf8: Vec<u8>,
}

impl Screen {
    fn new(cols: usize, rows: usize) -> Self {
        Self {
            cols,
            rows,
            lines: (0..rows).map(|_| vec![BLANK; cols]).collect(),
            row: 0,
            col: 0,
            saved: (0, 0),
            fg: FG,
            bg: BG,
            bright: false,
            pending_wrap: false,
            cursor_visible: true,
            scroll: 0,
            primary: None,
            dirty: true,
            parse: Parse::Ground,
            utf8: Vec::new(),
        }
    }

    fn top(&self) -> usize {
        self.lines.len() - self.rows
    }

    fn line(&mut self, row: usize) -> &mut Vec<Cell> {
        let top = self.top();
        &mut self.lines[top + row]
    }

    fn blank(&self) -> Cell {
        Cell {
            glyph: b' ',
            fg: self.fg,
            bg: self.bg,
        }
    }

    fn newline(&mut self) {
        self.pending_wrap = false;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        self.lines.push_back(vec![BLANK; self.cols]);
        if self.primary.is_some() || self.lines.len() > SCROLLBACK + self.rows {
            self.lines.pop_front();
        } else if self.scroll > 0 {
            self.scroll += 1;
        }
    }

    fn put(&mut self, ch: char) {
        if self.pending_wrap {
            self.newline();
            self.col = 0;
        }
        let cell = Cell {
            glyph: font::encode(ch),
            fg: self.fg,
            bg: self.bg,
        };
        let (row, col) = (self.row, self.col);
        self.line(row)[col] = cell;
        if self.col + 1 >= self.cols {
            self.pending_wrap = true;
        } else {
            self.col += 1;
        }
    }

    fn feed(&mut self, bytes: &[u8]) {
        self.dirty = true;
        for &byte in bytes {
            self.byte(byte);
        }
    }

    fn byte(&mut self, byte: u8) {
        match core::mem::replace(&mut self.parse, Parse::Ground) {
            Parse::Ground => self.ground(byte),
            Parse::Escape => match byte {
                b'[' => {
                    self.parse = Parse::Csi {
                        params: Vec::new(),
                        current: None,
                        private: false,
                    }
                }
                b'7' => self.saved = (self.row, self.col),
                b'8' => (self.row, self.col) = self.saved,
                b'c' => *self = Screen::new(self.cols, self.rows),
                _ => {}
            },
            Parse::Csi {
                mut params,
                mut current,
                mut private,
            } => match byte {
                b'0'..=b'9' => {
                    let digit = (byte - b'0') as u16;
                    current = Some(
                        current
                            .unwrap_or(0)
                            .saturating_mul(10)
                            .saturating_add(digit),
                    );
                    self.parse = Parse::Csi {
                        params,
                        current,
                        private,
                    };
                }
                b';' => {
                    params.push(current.unwrap_or(0));
                    self.parse = Parse::Csi {
                        params,
                        current: None,
                        private,
                    };
                }
                b'?' => {
                    private = true;
                    self.parse = Parse::Csi {
                        params,
                        current,
                        private,
                    };
                }
                0x40..=0x7e => {
                    if let Some(c) = current {
                        params.push(c);
                    }
                    self.csi(byte, &params, private);
                }
                _ => {}
            },
        }
    }

    fn ground(&mut self, byte: u8) {
        if !self.utf8.is_empty() || byte >= 0x80 {
            self.utf8.push(byte);
            match core::str::from_utf8(&self.utf8) {
                Ok(text) => {
                    let ch = text.chars().next().unwrap_or('?');
                    self.utf8.clear();
                    self.put(ch);
                }
                Err(e) if e.error_len().is_some() || self.utf8.len() >= 4 => {
                    self.utf8.clear();
                    self.put('?');
                }
                Err(_) => {}
            }
            return;
        }
        match byte {
            0x1b => self.parse = Parse::Escape,
            b'\n' => {
                self.newline();
                self.col = 0;
            }
            b'\r' => {
                self.col = 0;
                self.pending_wrap = false;
            }
            0x08 => {
                self.col = self.col.saturating_sub(1);
                self.pending_wrap = false;
            }
            b'\t' => {
                let next = (self.col / 8 + 1) * 8;
                while self.col < next.min(self.cols - 1) {
                    self.put(' ');
                }
            }
            0x20..=0x7e => self.put(byte as char),
            _ => {}
        }
    }

    fn csi(&mut self, command: u8, params: &[u16], private: bool) {
        let arg = |i: usize, default: u16| match params.get(i) {
            Some(&0) | None => default as usize,
            Some(&v) => v as usize,
        };
        self.pending_wrap = false;
        match command {
            b'A' => self.row = self.row.saturating_sub(arg(0, 1)),
            b'B' => self.row = (self.row + arg(0, 1)).min(self.rows - 1),
            b'C' => self.col = (self.col + arg(0, 1)).min(self.cols - 1),
            b'D' => self.col = self.col.saturating_sub(arg(0, 1)),
            b'G' => self.col = (arg(0, 1) - 1).min(self.cols - 1),
            b'H' | b'f' => {
                self.row = (arg(0, 1) - 1).min(self.rows - 1);
                self.col = (arg(1, 1) - 1).min(self.cols - 1);
            }
            b'J' => self.erase_display(params.first().copied().unwrap_or(0)),
            b'K' => {
                let blank = self.blank();
                let (row, col, cols) = (self.row, self.col, self.cols);
                let line = self.line(row);
                let range = match params.first().copied().unwrap_or(0) {
                    0 => col..cols,
                    1 => 0..col + 1,
                    _ => 0..cols,
                };
                line[range].fill(blank);
            }
            b'P' => {
                let blank = self.blank();
                let (row, col, n, cols) = (self.row, self.col, arg(0, 1), self.cols);
                let line = self.line(row);
                let n = n.min(cols - col);
                line.copy_within(col + n.., col);
                line[cols - n..].fill(blank);
            }
            b'@' => {
                let blank = self.blank();
                let (row, col, n, cols) = (self.row, self.col, arg(0, 1), self.cols);
                let line = self.line(row);
                let n = n.min(cols - col);
                line.copy_within(col..cols - n, col + n);
                line[col..col + n].fill(blank);
            }
            b'm' => self.sgr(params),
            b'h' | b'l' if private && params.first() == Some(&25) => {
                self.cursor_visible = command == b'h';
            }
            b'h' if private && params.first() == Some(&1049) => self.enter_alternate(),
            b'l' if private && params.first() == Some(&1049) => self.leave_alternate(),
            _ => {}
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let blank = self.blank();
        let (row, col) = (self.row, self.col);
        match mode {
            0 => {
                self.line(row)[col..].fill(blank);
                for r in row + 1..self.rows {
                    self.line(r).fill(blank);
                }
            }
            1 => {
                for r in 0..row {
                    self.line(r).fill(blank);
                }
                self.line(row)[..=col].fill(blank);
            }
            _ => {
                for r in 0..self.rows {
                    self.line(r).fill(blank);
                }
                if mode == 3 {
                    let keep = self.lines.len() - self.rows;
                    self.lines.drain(..keep);
                    self.scroll = 0;
                }
            }
        }
    }

    fn sgr(&mut self, params: &[u16]) {
        if params.is_empty() {
            self.reset_style();
            return;
        }
        for &p in params {
            match p {
                0 => self.reset_style(),
                1 => self.bright = true,
                22 => self.bright = false,
                7 => core::mem::swap(&mut self.fg, &mut self.bg),
                30..=37 => self.fg = PALETTE[(p - 30) as usize + if self.bright { 8 } else { 0 }],
                90..=97 => self.fg = PALETTE[(p - 90) as usize + 8],
                39 => self.fg = FG,
                40..=47 => self.bg = PALETTE[(p - 40) as usize],
                100..=107 => self.bg = PALETTE[(p - 100) as usize + 8],
                49 => self.bg = BG,
                _ => {}
            }
        }
    }

    fn reset_style(&mut self) {
        self.fg = FG;
        self.bg = BG;
        self.bright = false;
    }

    fn enter_alternate(&mut self) {
        if self.primary.is_some() {
            return;
        }
        let lines = core::mem::replace(
            &mut self.lines,
            (0..self.rows).map(|_| vec![BLANK; self.cols]).collect(),
        );
        self.primary = Some((lines, self.row, self.col));
        self.scroll = 0;
    }

    fn leave_alternate(&mut self) {
        if let Some((lines, row, col)) = self.primary.take() {
            self.lines = lines;
            (self.row, self.col) = (row, col);
            self.cursor_visible = true;
        }
    }

    fn scroll_view(&mut self, up: bool) {
        let max = self.lines.len() - self.rows;
        self.scroll = if up {
            (self.scroll + self.rows / 2).min(max)
        } else {
            self.scroll.saturating_sub(self.rows / 2)
        };
        self.dirty = true;
    }

    /// Pencere boyutu değişti: satırlar kısaltılır/uzatılır, imleç içeride tutulur.
    fn resize(&mut self, cols: usize, rows: usize) {
        let (cols, rows) = (cols.max(10), rows.max(3));
        for line in self.lines.iter_mut() {
            line.resize(cols, BLANK);
        }
        if let Some((lines, _, _)) = self.primary.as_mut() {
            for line in lines.iter_mut() {
                line.resize(cols, BLANK);
            }
        }
        if rows < self.rows {
            // Önce imlecin altındaki satırlar atılır; yetmezse görünüm yukarı kayar
            // (üstteki satırlar geri kaydırmada kalır).
            let mut excess = self.rows - rows;
            let below = self.rows - 1 - self.row;
            let drop = excess.min(below);
            for _ in 0..drop {
                self.lines.pop_back();
            }
            excess -= drop;
            self.row -= excess;
        } else if rows > self.rows {
            // Geri kaydırmadaki satırlar görünüme döner, kalan yer altta boş satırla dolar.
            let extra = rows - self.rows;
            let scrollback = self.lines.len() - self.rows;
            let pull = extra.min(scrollback);
            self.row += pull;
            for _ in pull..extra {
                self.lines.push_back(vec![BLANK; cols]);
            }
        }
        self.cols = cols;
        self.rows = rows;
        self.row = self.row.min(rows - 1);
        self.col = self.col.min(cols - 1);
        self.scroll = 0;
        self.dirty = true;
    }

    /// Görünür ekrandaki seçili metin (satır sonu boşlukları atılır).
    fn selected_text(&self, from: (usize, usize), to: (usize, usize)) -> String {
        let (start, end) = if from <= to { (from, to) } else { (to, from) };
        let top = self.top() - self.scroll;
        let mut out = String::new();
        for row in start.0..=end.0.min(self.rows - 1) {
            let line = &self.lines[top + row];
            let c0 = if row == start.0 { start.1 } else { 0 };
            let c1 = if row == end.0 { end.1 + 1 } else { self.cols };
            let text: String = line[c0.min(self.cols)..c1.min(self.cols)]
                .iter()
                .map(|c| font::decode(c.glyph))
                .collect();
            out.push_str(text.trim_end());
            if row != end.0 {
                out.push('\n');
            }
        }
        out
    }
}

struct View {
    window: Window,
    scale: usize,
    padding: usize,
}

impl View {
    fn cell(&self) -> (usize, usize) {
        (font::WIDTH * self.scale, font::HEIGHT * self.scale)
    }

    /// Piksel konumu → hücre (satır, sütun).
    fn cell_at(&self, screen: &Screen, x: i32, y: i32) -> (usize, usize) {
        let (cw, ch) = self.cell();
        let col = ((x.max(0) as usize).saturating_sub(self.padding) / cw).min(screen.cols - 1);
        let row = ((y.max(0) as usize).saturating_sub(self.padding) / ch).min(screen.rows - 1);
        (row, col)
    }

    fn render(
        &mut self,
        screen: &Screen,
        selection: Option<((usize, usize), (usize, usize))>,
        menu: Option<&ContextMenu<MenuAction>>,
    ) {
        let (cw, ch) = self.cell();
        let top = screen.top() - screen.scroll;
        let width = self.window.width;
        let selected = |row: usize, col: usize| {
            selection.is_some_and(|(a, b)| {
                let (s, e) = if a <= b { (a, b) } else { (b, a) };
                (row, col) >= s && (row, col) <= e
            })
        };
        for row in 0..screen.rows {
            let line = &screen.lines[top + row];
            for (col, cell) in line.iter().enumerate() {
                let cursor = screen.cursor_visible
                    && screen.scroll == 0
                    && row == screen.row
                    && col == screen.col;
                let (fg, bg) = if cursor ^ selected(row, col) {
                    (cell.bg, cell.fg)
                } else {
                    (cell.fg, cell.bg)
                };
                let glyph = font::glyph(cell.glyph);
                let x0 = self.padding + col * cw;
                let y0 = self.padding + row * ch;
                for (gy, bits) in glyph.iter().enumerate() {
                    for sy in 0..self.scale {
                        let start = (y0 + gy * self.scale + sy) * width + x0;
                        let pixels = &mut self.window.pixels[start..start + cw];
                        for (gx, pair) in pixels.chunks_mut(self.scale).enumerate() {
                            let on = bits & (0x80 >> gx) != 0;
                            pair.fill(if on { fg } else { bg });
                        }
                    }
                }
            }
        }
        if let Some(menu) = menu {
            let theme = Theme::from(&Prefs::default());
            menu.draw(&mut self.window.canvas(), &theme);
        }
        self.window.present();
    }
}

fn key_bytes(code: u32, out: &mut Vec<u8>) {
    let ctrl = code & KEY_CTRL != 0;
    let code = if code >= 0x1_0000 {
        code & 0xffff
    } else {
        code
    };
    if ctrl && let Some(letter) = arrow_letter(code) {
        out.extend_from_slice(b"\x1b[1;5");
        out.push(letter);
        return;
    }
    if (KEY_F1..KEY_F1 + 12).contains(&code) {
        // xterm: F1-F4 SS3, F5-F12 CSI n ~
        const TILDE: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
        let n = (code - KEY_F1) as usize;
        if n < 4 {
            out.extend_from_slice(&[0x1b, b'O', b'P' + n as u8]);
        } else {
            out.extend_from_slice(alloc::format!("\x1b[{}~", TILDE[n - 4]).as_bytes());
        }
        return;
    }
    match code {
        KEY_INSERT => out.extend_from_slice(b"\x1b[2~"),
        KEY_PAGE_UP => out.extend_from_slice(b"\x1b[5~"),
        KEY_PAGE_DOWN => out.extend_from_slice(b"\x1b[6~"),
        13 => out.push(b'\r'),
        8 => out.push(0x7f),
        KEY_UP => out.extend_from_slice(b"\x1b[A"),
        KEY_DOWN => out.extend_from_slice(b"\x1b[B"),
        KEY_RIGHT => out.extend_from_slice(b"\x1b[C"),
        KEY_LEFT => out.extend_from_slice(b"\x1b[D"),
        KEY_HOME => out.extend_from_slice(b"\x1b[H"),
        KEY_END => out.extend_from_slice(b"\x1b[F"),
        KEY_DELETE => out.extend_from_slice(b"\x1b[3~"),
        _ => {
            if let Some(c) = char::from_u32(code) {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
}

/// Çocuk işlem: yeni süreç grubu kurar, pty'yi stdin/stdout/stderr yapar ve kabuğu çalıştırır.
fn start_shell(master: u64, slave: u64, command: Option<&str>, cwd: Option<&str>) -> ! {
    if let Some(dir) = cwd {
        let _ = process::chdir(dir);
    }
    process::setpgid(0, 0);
    tty::set_foreground(slave, process::pid());
    for fd in 0..3 {
        let _ = io::dup2(slave, fd);
    }
    io::close(master);
    if slave > 2 {
        io::close(slave);
    }
    process::ignore_signals(0);
    match command {
        Some(command) => process::exec("/bin/sh", &["sh", "-c", command]),
        None => process::exec("/bin/sh", &["sh"]),
    };
    banna::eprintln!("terminal: /bin/sh could not be started");
    process::exit(127)
}

/// `terminal [-t başlık] [-d klasör] [komut...]`: komut verilirse kabuk yerine o çalışır,
/// bitince pencere kapanır; `-d` kabuğun başlangıç klasörüdür.
fn main(args: &[&str]) -> i32 {
    let mut title = "Terminal";
    let mut cwd: Option<&str> = None;
    let mut rest = args.get(1..).unwrap_or(&[]);
    loop {
        match rest {
            ["-t", value, tail @ ..] => {
                title = value;
                rest = tail;
            }
            ["-d", value, tail @ ..] => {
                cwd = Some(value);
                rest = tail;
            }
            _ => break,
        }
    }
    let command = (!rest.is_empty()).then(|| rest.join(" "));
    let info = process::sysinfo();
    let scale = if info.screen_height >= 1300 { 2 } else { 1 };
    let padding = 10 * scale;
    let width = COLUMNS * font::WIDTH * scale + 2 * padding;
    let height = ROWS * font::HEIGHT * scale + 2 * padding;
    let window = match Window::new(title, width, height) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("terminal: no window ({})", e);
            return 1;
        }
    };
    let mut view = View {
        window,
        scale,
        padding,
    };
    view.window.pixels.fill(BG);

    let (master, slave) = match tty::openpty() {
        Ok(pair) => pair,
        Err(e) => {
            banna::eprintln!("terminal: openpty failed ({})", e);
            return 1;
        }
    };
    tty::set_window_size(master, COLUMNS, ROWS);
    let shell = match process::fork() {
        Ok(0) => start_shell(master, slave, command.as_deref(), cwd),
        Ok(pid) => pid,
        Err(e) => {
            banna::eprintln!("terminal: fork failed ({})", e);
            return 1;
        }
    };
    io::close(slave);

    let mut screen = Screen::new(COLUMNS, ROWS);
    let mut selection: Option<((usize, usize), (usize, usize))> = None;
    let mut selecting = false;
    let mut menu: Option<ContextMenu<MenuAction>> = None;
    let start_dir = cwd.map(String::from);
    let mut buf = vec![0u8; 16 * 1024];
    let mut input = Vec::new();
    let mut last_frame = 0u64;
    let mut open = true;
    while open {
        let since = time::uptime_ms().saturating_sub(last_frame);
        let timeout = screen.dirty.then(|| FRAME_MS.saturating_sub(since));
        let mut entries = [PollEntry::fd(master), PollEntry::window(view.window.id())];
        io::poll(&mut entries, timeout);

        while open && let Some(event) = view.window.event(0) {
            match event.kind {
                EVENT_CLOSE => {
                    let _ = process::signal(-(shell as i64), SIGHUP);
                    let fg = tty::foreground(master);
                    if fg > 0 {
                        let _ = process::signal(-fg, SIGHUP);
                    }
                    open = false;
                }
                EVENT_RESIZE => {
                    view.window
                        .resize(event.x.max(1) as usize, event.y.max(1) as usize);
                    view.window.pixels.fill(BG);
                    let (cw, ch) = view.cell();
                    let cols = (view.window.width.saturating_sub(2 * view.padding) / cw).max(10);
                    let rows = (view.window.height.saturating_sub(2 * view.padding) / ch).max(3);
                    screen.resize(cols, rows);
                    tty::set_window_size(master, cols, rows);
                    selection = None;
                }
                EVENT_CONTEXT => {
                    let has_selection = selection.is_some_and(|(a, b)| a != b);
                    let items = alloc::vec![
                        ("Kopyala", has_selection.then_some(MenuAction::Copy)),
                        ("Yapıştır", Some(MenuAction::Paste)),
                        ("", None),
                        ("Yeni pencere", Some(MenuAction::NewWindow)),
                        ("Ekranı temizle", Some(MenuAction::Clear)),
                    ]
                    .into_iter()
                    .filter(|(l, a)| a.is_some() || l.is_empty())
                    .collect();
                    let bounds = (view.window.width as i32, view.window.height as i32);
                    menu = Some(ContextMenu::new(event.x, event.y, items, bounds));
                    screen.dirty = true;
                }
                EVENT_MOUSE_MOVE if menu.is_some() => {
                    if menu.as_mut().is_some_and(|m| m.hover_at(event.x, event.y)) {
                        screen.dirty = true;
                    }
                }
                EVENT_MOUSE_DOWN if menu.is_some() => {
                    let picked = menu.take().and_then(|m| m.pick(event.x, event.y));
                    screen.dirty = true;
                    match picked {
                        Some(MenuAction::Copy) => {
                            if let Some((a, b)) = selection {
                                clipboard::set(&screen.selected_text(a, b));
                            }
                        }
                        Some(MenuAction::Paste) => {
                            input.extend_from_slice(clipboard::get().replace('\n', "\r").as_bytes())
                        }
                        Some(MenuAction::NewWindow) => {
                            let mut args = alloc::vec!["terminal"];
                            if let Some(dir) = start_dir.as_deref() {
                                args.extend(["-d", dir]);
                            }
                            let _ = process::spawn("/apps/terminal/terminal", &args);
                        }
                        // Ctrl+L: kabuk ekranı temizler.
                        Some(MenuAction::Clear) => input.push(0x0c),
                        None => {}
                    }
                }
                EVENT_MOUSE_DOWN => {
                    let cell = view.cell_at(&screen, event.x, event.y);
                    selection = Some((cell, cell));
                    selecting = true;
                    screen.dirty = true;
                }
                EVENT_MOUSE_MOVE if selecting => {
                    if let Some((start, _)) = selection {
                        selection = Some((start, view.cell_at(&screen, event.x, event.y)));
                        screen.dirty = true;
                    }
                }
                EVENT_MOUSE_UP => {
                    selecting = false;
                    match selection {
                        Some((a, b)) if a != b => clipboard::set(&screen.selected_text(a, b)),
                        _ => selection = None,
                    }
                    screen.dirty = true;
                }
                EVENT_KEY if event.key == KEY_PASTE => {
                    input.extend_from_slice(clipboard::get().replace('\n', "\r").as_bytes());
                }
                EVENT_KEY => match event.key {
                    KEY_PAGE_UP if screen.primary.is_some() => input.extend_from_slice(b"\x1b[5~"),
                    KEY_PAGE_DOWN if screen.primary.is_some() => {
                        input.extend_from_slice(b"\x1b[6~")
                    }
                    KEY_PAGE_UP => screen.scroll_view(true),
                    KEY_PAGE_DOWN => screen.scroll_view(false),
                    code => {
                        key_bytes(code, &mut input);
                        if screen.scroll != 0 {
                            screen.scroll = 0;
                            screen.dirty = true;
                        }
                    }
                },
                _ => {}
            }
        }
        if !input.is_empty() {
            let _ = io::write_all(master, &input);
            input.clear();
        }

        if entries[0].ready != 0 {
            let n = io::read_fd(master, &mut buf);
            if n <= 0 {
                break;
            }
            screen.feed(&buf[..n as usize]);
            selection = None;
        }

        if screen.dirty && time::uptime_ms().saturating_sub(last_frame) >= FRAME_MS {
            view.render(&screen, selection, menu.as_ref());
            screen.dirty = false;
            last_frame = time::uptime_ms();
        }
    }
    0
}
