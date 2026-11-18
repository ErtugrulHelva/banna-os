//! Hücre tabanlı metin konsolu.
//!
//! Metin bir hücre ızgarasında tutulur. Çizim üç kipte yapılır:
//! `Early` (açılışın başında doğrudan framebuffer'a), `Display` (arka tampona
//! çizilip hasarlı bölge ekrana kopyalanır) ve `Window` (masaüstündeki terminal
//! penceresini compositor çizer).

use core::fmt;

use super::font;
pub use super::framebuffer::Color;
use super::framebuffer::Framebuffer;
use crate::gfx::{self, Canvas, Rect};
use crate::sync::SpinLock;

pub const BLACK: Color = Color(0x0b, 0x0e, 0x14);
pub const WHITE: Color = Color(0xe6, 0xe6, 0xe6);
pub const GRAY: Color = Color(0x80, 0x86, 0x90);
pub const GREEN: Color = Color(0x5f, 0xd7, 0x5f);
pub const RED: Color = Color(0xff, 0x5f, 0x5f);
pub const CYAN: Color = Color(0x5f, 0xd7, 0xff);

const MARGIN: usize = 8;
const TAB_WIDTH: usize = 4;
const MAX_COLUMNS: usize = 256;
const MAX_ROWS: usize = 128;

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Cell {
    pub ch: u8,
    pub fg: Color,
    pub bg: Color,
}

const BLANK: Cell = Cell {
    ch: b' ',
    fg: WHITE,
    bg: BLACK,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Early,
    Display,
    Window,
}

pub struct Console {
    fb: Option<Framebuffer>,
    cells: [Cell; MAX_COLUMNS * MAX_ROWS],
    columns: usize,
    rows: usize,
    column: usize,
    row: usize,
    pub fg: Color,
    pub bg: Color,
    cursor: bool,
    mode: Mode,
    dirty: u128,
    scrolled: usize,
    generation: u64,
    muted: bool,
    scale: usize,
}

impl Console {
    const fn new() -> Self {
        Self {
            fb: None,
            cells: [BLANK; MAX_COLUMNS * MAX_ROWS],
            columns: 0,
            rows: 0,
            column: 0,
            row: 0,
            fg: WHITE,
            bg: BLACK,
            cursor: false,
            mode: Mode::Early,
            dirty: 0,
            scrolled: 0,
            generation: 0,
            muted: false,
            scale: 1,
        }
    }

    pub fn cell_size(&self) -> (usize, usize) {
        (font::WIDTH * self.scale, font::HEIGHT * self.scale)
    }

    fn full_screen_grid(&self, fb: &Framebuffer) -> (usize, usize) {
        let (cw, ch) = self.cell_size();
        (
            ((fb.width - 2 * MARGIN) / cw).min(MAX_COLUMNS),
            ((fb.height - 2 * MARGIN) / ch).min(MAX_ROWS),
        )
    }

    /// Hücre ölçeği: yüksek çözünürlükte bitmap font 2x büyütülür.
    pub fn scale_for(height: usize) -> usize {
        if height >= 1300 { 2 } else { 1 }
    }

    fn direct(&self) -> bool {
        self.mode == Mode::Early && !self.muted
    }

    fn attach(&mut self, fb: Framebuffer) {
        self.scale = Self::scale_for(fb.height);
        let (columns, rows) = self.full_screen_grid(&fb);
        self.columns = columns;
        self.rows = rows;
        self.fb = Some(fb);
        self.clear();
    }

    pub fn size(&self) -> Option<(usize, usize)> {
        self.fb.as_ref().map(|fb| (fb.width, fb.height))
    }

    fn cell(&self, column: usize, row: usize) -> Cell {
        self.cells[row * MAX_COLUMNS + column]
    }

    fn mark_row(&mut self, row: usize) {
        if row < 128 {
            self.dirty |= 1 << row;
        }
        self.generation += 1;
    }

    fn mark_all(&mut self) {
        self.dirty = u128::MAX;
        self.generation += 1;
    }

    pub fn clear(&mut self) {
        for row in 0..self.rows {
            for column in 0..self.columns {
                self.cells[row * MAX_COLUMNS + column] = Cell {
                    ch: b' ',
                    fg: self.fg,
                    bg: self.bg,
                };
            }
        }
        self.column = 0;
        self.row = 0;
        self.scrolled = 0;
        self.mark_all();
        if self.direct() {
            let bg = self.bg;
            if let Some(fb) = self.fb.as_mut() {
                fb.fill_rect(0, 0, fb.width, fb.height, bg);
            }
        }
    }

    fn set_cell(&mut self, column: usize, row: usize, ch: u8, fg: Color) {
        let bg = self.bg;
        self.cells[row * MAX_COLUMNS + column] = Cell { ch, fg, bg };
        self.mark_row(row);
        let (direct, scale) = (self.direct(), self.scale);
        if direct && let Some(fb) = self.fb.as_mut() {
            fb.draw_glyph(
                MARGIN + column * font::WIDTH * scale,
                MARGIN + row * font::HEIGHT * scale,
                ch,
                fg,
                bg,
                scale,
            );
        }
    }

    fn newline(&mut self) {
        self.column = 0;
        if self.row + 1 < self.rows {
            self.row += 1;
            return;
        }
        self.cells
            .copy_within(MAX_COLUMNS..self.rows * MAX_COLUMNS, 0);
        let last = self.rows - 1;
        for column in 0..self.columns {
            self.cells[last * MAX_COLUMNS + column] = Cell {
                ch: b' ',
                fg: self.fg,
                bg: self.bg,
            };
        }
        self.scrolled += 1;
        self.dirty >>= 1;
        self.mark_row(last);
        if self.direct() {
            self.redraw_direct();
        }
    }

    fn redraw_direct(&mut self) {
        let Some(mut fb) = self.fb.clone() else {
            return;
        };
        for row in 0..self.rows {
            for column in 0..self.columns {
                let cell = self.cell(column, row);
                fb.draw_glyph(
                    MARGIN + column * font::WIDTH * self.scale,
                    MARGIN + row * font::HEIGHT * self.scale,
                    cell.ch,
                    cell.fg,
                    cell.bg,
                    self.scale,
                );
            }
        }
        self.scrolled = 0;
        self.dirty = 0;
    }

    fn put_byte(&mut self, byte: u8) {
        if self.columns == 0 {
            return;
        }
        match byte {
            b'\n' => self.newline(),
            b'\r' => self.column = 0,
            b'\t' => {
                let next = (self.column / TAB_WIDTH + 1) * TAB_WIDTH;
                while self.column < next.min(self.columns) {
                    self.put_byte(b' ');
                }
            }
            0x08 => {
                if self.column > 0 {
                    self.column -= 1;
                    let fg = self.fg;
                    self.set_cell(self.column, self.row, b' ', fg);
                }
            }
            byte => {
                if self.column >= self.columns {
                    self.newline();
                }
                let fg = self.fg;
                self.set_cell(self.column, self.row, byte, fg);
                self.column += 1;
            }
        }
    }

    fn draw_cursor_direct(&mut self, visible: bool) {
        if !self.direct() || self.column >= self.columns {
            return;
        }
        let scale = self.scale;
        let cell = self.cell(self.column, self.row);
        if let Some(fb) = self.fb.as_mut() {
            let (ch, fg) = if visible {
                (b'_', GRAY)
            } else {
                (cell.ch, cell.fg)
            };
            fb.draw_glyph(
                MARGIN + self.column * font::WIDTH * scale,
                MARGIN + self.row * font::HEIGHT * scale,
                ch,
                fg,
                cell.bg,
                scale,
            );
        }
    }

    /// İmleci satır içinde silmeden kaydırır (satır düzenleme).
    pub fn move_cursor(&mut self, delta: isize) {
        let visible = self.cursor;
        self.set_cursor(false);
        self.column = (self.column as isize + delta).clamp(0, self.columns as isize - 1) as usize;
        self.set_cursor(visible);
    }

    pub fn set_cursor(&mut self, visible: bool) {
        self.draw_cursor_direct(false);
        self.cursor = visible;
        self.draw_cursor_direct(visible);
        self.mark_row(self.row);
    }

    /// Hücreleri bir kanvasa çizer; yalnızca `rows_mask` içindeki satırlar.
    pub fn render(&self, canvas: &mut Canvas, origin_x: i32, origin_y: i32, rows_mask: u128) {
        let scale = self.scale as i32;
        let cw = font::WIDTH as i32 * scale;
        let ch = font::HEIGHT as i32 * scale;
        for row in 0..self.rows {
            if row < 128 && rows_mask & (1 << row) == 0 {
                continue;
            }
            let y = origin_y + row as i32 * ch;
            for column in 0..self.columns {
                let cell = self.cell(column, row);
                let x = origin_x + column as i32 * cw;
                canvas.fill(Rect::new(x, y, cw, ch), cell.bg.argb());
                let cursor_here = self.cursor && row == self.row && column == self.column;
                if cursor_here {
                    canvas.glyph_scaled(x, y, b'_', GRAY.argb(), scale);
                } else if cell.ch != b' ' {
                    canvas.glyph_scaled(x, y, cell.ch, cell.fg.argb(), scale);
                }
            }
        }
    }

    /// İçeriği koruyarak ızgarayı yeniden boyutlandırır (son satırlar kalır).
    fn resize(&mut self, columns: usize, rows: usize) {
        let columns = columns.clamp(1, MAX_COLUMNS);
        let rows = rows.clamp(1, MAX_ROWS);
        let used = self.row + 1;
        let first = used.saturating_sub(rows);
        if first > 0 {
            self.cells
                .copy_within(first * MAX_COLUMNS..used * MAX_COLUMNS, 0);
        }
        let kept = used - first;
        let old_columns = self.columns;
        let blank = Cell {
            ch: b' ',
            fg: self.fg,
            bg: self.bg,
        };
        for row in 0..rows {
            for column in 0..columns {
                if row >= kept || column >= old_columns {
                    self.cells[row * MAX_COLUMNS + column] = blank;
                }
            }
        }
        self.row = kept - 1;
        self.column = self.column.min(columns);
        self.columns = columns;
        self.rows = rows;
        self.scrolled = 0;
        self.mark_all();
    }
}

impl fmt::Write for Console {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.cursor {
            self.draw_cursor_direct(false);
            self.mark_row(self.row);
        }
        for ch in s.chars() {
            self.put_byte(font::encode(ch));
        }
        if self.cursor {
            self.draw_cursor_direct(true);
            self.mark_row(self.row);
        }
        Ok(())
    }
}

pub static CONSOLE: SpinLock<Console> = SpinLock::new(Console::new());

pub fn init(fb: Framebuffer) {
    CONSOLE.lock().attach(fb);
}

/// Ekran çözünürlüğü değişti: konsol ızgarası yeni boyuta uyarlanır.
pub fn set_framebuffer(fb: Framebuffer) {
    let mut console = CONSOLE.lock();
    console.scale = Console::scale_for(fb.height);
    let (columns, rows) = console.full_screen_grid(&fb);
    console.fb = Some(fb);
    if console.mode != Mode::Window {
        console.resize(columns, rows);
    }
}

/// Açılış ekranı görünürken konsol ekrana çizmez (hücreler yine tutulur).
pub fn set_muted(muted: bool) {
    let mut console = CONSOLE.lock();
    console.muted = muted;
    console.mark_all();
}

pub fn framebuffer() -> Option<Framebuffer> {
    CONSOLE.lock().fb.clone()
}

pub fn mode() -> Mode {
    CONSOLE.lock().mode
}

/// Arka tampon hazır: bundan sonra çizim `flush` ile yapılır.
pub fn attach_display() {
    {
        let mut console = CONSOLE.lock();
        console.mode = Mode::Display;
        console.mark_all();
    }
    flush();
}

/// Masaüstü: konsol bir pencerenin içine sığacak şekilde küçülür.
pub fn enter_window_mode(columns: usize, rows: usize) {
    let mut console = CONSOLE.lock();
    console.mode = Mode::Window;
    console.resize(columns, rows);
}

pub fn leave_window_mode() {
    {
        let mut console = CONSOLE.lock();
        let Some(fb) = console.fb.clone() else { return };
        let (columns, rows) = console.full_screen_grid(&fb);
        console.mode = Mode::Display;
        console.resize(columns, rows);
    }
    gfx::display::with(|display| {
        let bg = BLACK.argb();
        let bounds = display.back.bounds();
        display.back.canvas().fill(bounds, bg);
        display.mark_all();
    });
    flush();
}

/// Değişen satırları (ve kaydırmayı) verilen yüzeye çizer; hasar dikdörtgenini döndürür.
pub fn paint(surface: &mut gfx::Surface, origin_x: i32, origin_y: i32) -> Option<Rect> {
    let mut console = CONSOLE.lock();
    let (cw, ch) = console.cell_size();
    let line = ch as i32;
    let area_w = console.columns as i32 * cw as i32;
    let area_h = console.rows as i32 * line;
    let mut damage = Rect::default();
    let scrolled = core::mem::take(&mut console.scrolled) as i32;
    if scrolled > 0 && scrolled < console.rows as i32 {
        let width = surface.width as usize;
        let shift = (scrolled * line) as usize;
        for y in origin_y..origin_y + area_h - shift as i32 {
            let dst = y as usize * width + origin_x as usize;
            let src = (y as usize + shift) * width + origin_x as usize;
            surface.pixels.copy_within(src..src + area_w as usize, dst);
        }
        let first_new = console.rows - scrolled as usize;
        for row in first_new..console.rows.min(128) {
            console.dirty |= 1 << row;
        }
        damage = Rect::new(origin_x, origin_y, area_w, area_h);
    } else if scrolled > 0 {
        console.dirty = u128::MAX;
    }
    let mask = core::mem::take(&mut console.dirty);
    if mask != 0 {
        let mut canvas = surface.canvas();
        console.render(&mut canvas, origin_x, origin_y, mask);
        for row in 0..console.rows.min(128) {
            if mask & (1 << row) != 0 {
                damage = damage.union(&Rect::new(
                    origin_x,
                    origin_y + row as i32 * line,
                    area_w,
                    line,
                ));
            }
        }
    }
    (!damage.is_empty()).then_some(damage)
}

/// `Display` kipinde değişen satırları arka tampona çizip ekrana kopyalar.
pub fn flush() {
    if CONSOLE.lock().muted {
        return;
    }
    match mode() {
        Mode::Display => {}
        Mode::Window => {
            crate::gui::wake();
            return;
        }
        Mode::Early => return,
    }
    gfx::display::with(|display| {
        if let Some(damage) = paint(&mut display.back, MARGIN as i32, MARGIN as i32) {
            display.mark(damage);
        }
        display.present();
    });
}

/// Panik yolunda: kilitleri tutanlar bir daha çalışmayacağı için doğrudan framebuffer'a döner.
pub fn emergency_takeover() {
    unsafe { CONSOLE.force_unlock() };
    let mut console = CONSOLE.lock();
    console.muted = false;
    if console.mode != Mode::Early
        && let Some(fb) = console.fb.clone()
    {
        let (columns, rows) = console.full_screen_grid(&fb);
        console.mode = Mode::Early;
        console.resize(columns, rows);
        console.redraw_direct();
    }
}

pub fn set_cursor(visible: bool) {
    CONSOLE.lock().set_cursor(visible);
    flush();
}

pub fn move_cursor(delta: isize) {
    CONSOLE.lock().move_cursor(delta);
    flush();
}

pub fn clear() {
    CONSOLE.lock().clear();
    flush();
}

pub fn set_color(fg: Color) {
    CONSOLE.lock().fg = fg;
}
