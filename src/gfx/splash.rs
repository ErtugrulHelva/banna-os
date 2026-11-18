//! Açılış ekranı: siyah zemin, ortada büyük "B", altında yükleme çubuğu ve o anki adım
//! (gerçek donanımda takılırsa hangi adımda kaldığı görünür).
//!
//! Heap gerektirmez: ekran katmanı hazır değilken doğrudan framebuffer'a,
//! hazır olduğunda arka tampona çizer. Yalnızca değişen çubuk bölgesi yenilenir.

use core::sync::atomic::{AtomicBool, AtomicPtr, AtomicU32, AtomicUsize, Ordering};

use super::{Rect, display, text};
use crate::drivers::{console, font};

static ACTIVE: AtomicBool = AtomicBool::new(false);
static PERMILLE: AtomicU32 = AtomicU32::new(0);
static STARTED_MS: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);

static STEP_PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
static STEP_LEN: AtomicUsize = AtomicUsize::new(0);

const LABEL_CHARS: i32 = 64;
const LABEL_COLOR: u32 = 0x00_70_70_70;
const TRACK: u32 = 0x00_26_26_26;
const FILL: u32 = 0x00_ff_ff_ff;
const MAX_WIDTH: usize = 4096;

struct Layout {
    width: i32,
    height: i32,
    logo: Rect,
    bar: Rect,
    label: Rect,
    scale: i32,
}

fn layout(width: i32, height: i32) -> Layout {
    let (lw, lh) = text::splash_logo()
        .map(|(w, h, _)| (w, h))
        .unwrap_or((0, 0));
    let scale = if height >= 1300 { 2 } else { 1 };
    let logo = Rect::new(width / 2 - lw / 2, height / 2 - lh / 2 - 40 * scale, lw, lh);
    let bar_w = (width / 6).clamp(200, 480);
    let bar = Rect::new(
        width / 2 - bar_w / 2,
        logo.bottom() + 56 * scale,
        bar_w,
        4 * scale,
    );
    let (cw, ch) = (font::WIDTH as i32 * scale, font::HEIGHT as i32 * scale);
    let label = Rect::new(
        width / 2 - LABEL_CHARS * cw / 2,
        bar.bottom() + 20 * scale,
        LABEL_CHARS * cw,
        ch,
    );
    Layout {
        width,
        height,
        logo,
        bar,
        label,
        scale,
    }
}

fn step_name() -> &'static str {
    let ptr = STEP_PTR.load(Ordering::Relaxed);
    if ptr.is_null() {
        return "";
    }
    let bytes = unsafe { core::slice::from_raw_parts(ptr, STEP_LEN.load(Ordering::Relaxed)) };
    core::str::from_utf8(bytes).unwrap_or("")
}

/// Bir satırın `[x, x+len)` aralığının renklerini üretir.
fn shade_row(l: &Layout, y: i32, x: i32, row: &mut [u32], permille: u32) {
    row.fill(0);
    if let Some((w, h, coverage)) = text::splash_logo()
        && y >= l.logo.y
        && y < l.logo.y + h
    {
        let gy = (y - l.logo.y) as usize;
        for (i, px) in row.iter_mut().enumerate() {
            let gx = x + i as i32 - l.logo.x;
            if gx >= 0 && gx < w {
                let a = coverage[gy * w as usize + gx as usize] as u32;
                *px = a << 16 | a << 8 | a;
            }
        }
    }
    if y >= l.label.y && y < l.label.bottom() {
        let name = step_name();
        let count = name.chars().count().min(LABEL_CHARS as usize) as i32;
        let cw = font::WIDTH as i32 * l.scale;
        let start = l.width / 2 - count * cw / 2;
        let gy = ((y - l.label.y) / l.scale) as usize;
        for (i, px) in row.iter_mut().enumerate() {
            let px_x = x + i as i32;
            if px_x < start || px_x >= start + count * cw {
                continue;
            }
            let column = (px_x - start) / cw;
            let Some(c) = name.chars().nth(column as usize) else {
                continue;
            };
            let bits = font::glyph(font::encode(c))[gy];
            let gx = ((px_x - start) % cw) / l.scale;
            if bits & (0x80 >> gx) != 0 {
                *px = LABEL_COLOR;
            }
        }
    }
    if y >= l.bar.y && y < l.bar.bottom() {
        let filled = l.bar.x + (l.bar.w as u32 * permille / 1000) as i32;
        for (i, px) in row.iter_mut().enumerate() {
            let px_x = x + i as i32;
            if px_x >= l.bar.x && px_x < l.bar.right() {
                *px = if px_x < filled { FILL } else { TRACK };
            }
        }
    }
}

fn paint(region: Option<Rect>) {
    let permille = PERMILLE.load(Ordering::Relaxed);
    let drawn = display::with(|d| {
        let (w, h) = d.size();
        let l = layout(w, h);
        let r = region
            .unwrap_or(Rect::new(0, 0, w, h))
            .intersect(&d.back.bounds());
        let mut row = [0u32; MAX_WIDTH];
        for y in r.y..r.bottom() {
            let len = (r.w as usize).min(MAX_WIDTH);
            shade_row(&l, y, r.x, &mut row[..len], permille);
            let start = (y * w + r.x) as usize;
            d.back.pixels[start..start + len].copy_from_slice(&row[..len]);
        }
        d.mark(r);
        d.present();
    });
    if drawn.is_some() {
        return;
    }
    let Some(mut fb) = console::framebuffer() else {
        return;
    };
    let l = layout(fb.width as i32, fb.height as i32);
    let r = region
        .unwrap_or(Rect::new(0, 0, l.width, l.height))
        .intersect(&Rect::new(0, 0, l.width, l.height));
    let mut row = [0u32; MAX_WIDTH];
    for y in r.y..r.bottom() {
        let len = (r.w as usize).min(MAX_WIDTH);
        shade_row(&l, y, r.x, &mut row[..len], permille);
        fb.write_row(r.x as usize, y as usize, &row[..len]);
    }
}

fn bar_region() -> Option<Rect> {
    let (w, h) = display::size()
        .or_else(|| console::framebuffer().map(|fb| (fb.width as i32, fb.height as i32)))?;
    Some(layout(w, h).bar)
}

pub fn is_active() -> bool {
    ACTIVE.load(Ordering::Relaxed)
}

pub fn begin() {
    console::set_muted(true);
    ACTIVE.store(true, Ordering::Relaxed);
    PERMILLE.store(0, Ordering::Relaxed);
    STARTED_MS.store(crate::arch::pit::uptime_ms(), Ordering::Relaxed);
    paint(None);
}

/// O anki açılış adımının adını gösterir.
pub fn set_step(name: &'static str) {
    STEP_PTR.store(name.as_ptr() as *mut u8, Ordering::Relaxed);
    STEP_LEN.store(name.len(), Ordering::Relaxed);
    if is_active()
        && let Some(r) = label_region()
    {
        paint(Some(r));
    }
}

fn label_region() -> Option<Rect> {
    let (w, h) = display::size()
        .or_else(|| console::framebuffer().map(|fb| (fb.width as i32, fb.height as i32)))?;
    Some(layout(w, h).label)
}

pub fn progress(done: usize, total: usize) {
    if !is_active() || total == 0 {
        return;
    }
    let permille = (done * 1000 / total).min(1000) as u32;
    if PERMILLE.swap(permille, Ordering::Relaxed) != permille {
        paint(bar_region());
    }
}

/// Ekran çıkışı değiştiğinde (çözünürlük, GPU) tamamen yeniden çizer.
pub fn redraw() {
    if is_active() {
        paint(None);
    }
}

const MIN_VISIBLE_MS: u64 = 1500;
const FINISH_MS: u64 = 400;

/// Çubuğu akıcı biçimde doldurur; ekran en az `MIN_VISIBLE_MS` görünür kalır.
pub fn finish() {
    if !is_active() {
        return;
    }
    set_step("");
    let now = crate::arch::pit::uptime_ms();
    let started = STARTED_MS.load(Ordering::Relaxed);
    let duration = FINISH_MS.max((started + MIN_VISIBLE_MS).saturating_sub(now));
    let from = PERMILLE.load(Ordering::Relaxed);
    let steps = (duration / 16).max(1);
    for step in 1..=steps {
        let permille = from + (1000 - from) * step as u32 / steps as u32;
        PERMILLE.store(permille, Ordering::Relaxed);
        paint(bar_region());
        crate::sched::sleep_ms(duration / steps);
    }
    ACTIVE.store(false, Ordering::Relaxed);
}
