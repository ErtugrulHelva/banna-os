//! Masaüstü simgeleri: oturum kullanıcısının `~/Desktop` klasörü, sol üstten aşağı
//! sütunlar hâlinde. Klasör arka planda taranır (disk okuması compositor'ı bekletmez).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, Ordering};

use super::m;
use super::prefs::Theme;
use crate::fs;
use crate::gfx::text::UiFonts;
use crate::gfx::{Canvas, Rect, rgb, with_alpha};
use crate::sync::SpinLock;

pub struct Entry {
    pub name: String,
    pub path: String,
    pub dir: bool,
    pub rect: Rect,
}

static SCANNED: SpinLock<Option<Vec<(String, bool)>>> = SpinLock::new(None);
static SCANNING: AtomicBool = AtomicBool::new(false);

/// Oturum kullanıcısının masaüstü klasörü.
pub fn folder() -> Option<String> {
    crate::kernel::users::session().map(|s| format!("{}/Desktop", s.home))
}

/// Klasörü arka planda okur; bitince compositor uyandırılır (`take_scanned`).
pub fn scan_async() {
    let Some(session) = crate::kernel::users::session() else {
        return;
    };
    if SCANNING.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::sched::spawn_detached("desk-scan", move || {
        let dir = format!("{}/Desktop", session.home);
        if fs::stat(&dir).is_err() {
            let owner = fs::Owner {
                uid: session.uid,
                gid: session.uid,
            };
            let _ = fs::mkdir_as(&dir, owner, 0o755);
        }
        let mut list: Vec<(String, bool)> = fs::read_dir(&dir)
            .unwrap_or_default()
            .into_iter()
            .filter(|e| !e.name.starts_with('.'))
            .map(|e| (e.name, e.kind == fs::Kind::Directory))
            .collect();
        list.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then(a.0.to_lowercase().cmp(&b.0.to_lowercase()))
        });
        *SCANNED.lock() = Some(list);
        SCANNING.store(false, Ordering::Release);
        super::wake();
        0
    });
}

pub fn take_scanned() -> Option<Vec<(String, bool)>> {
    SCANNED.lock().take()
}

fn cell() -> (i32, i32) {
    let u = m().unit;
    (96 * u, 100 * u)
}

/// Öğeleri ızgaraya yerleştirir (sol üstten aşağı, sonra sağa).
pub fn layout(list: &[(String, bool)], folder: &str, height: i32) -> Vec<Entry> {
    let u = m().unit;
    let (cw, ch) = cell();
    let top = m().panel_h + 14 * u;
    let rows = ((height - top - 130 * u) / ch).max(1);
    list.iter()
        .enumerate()
        .map(|(i, (name, dir))| {
            let (col, row) = (i as i32 / rows, i as i32 % rows);
            Entry {
                name: name.clone(),
                path: format!("{}/{}", folder, name),
                dir: *dir,
                rect: Rect::new(14 * u + col * cw, top + row * ch, cw, ch),
            }
        })
        .collect()
}

/// Simgelerin kapladığı toplam alan (hasar bölgesi).
pub fn bounds(entries: &[Entry]) -> Rect {
    entries
        .iter()
        .fold(Rect::default(), |acc, e| acc.union(&e.rect))
}

pub fn at(entries: &[Entry], x: i32, y: i32) -> Option<usize> {
    entries.iter().position(|e| e.rect.contains(x, y))
}

fn draw_folder(canvas: &mut Canvas, r: Rect, accent: u32) {
    let u = m().unit;
    let tab = Rect::new(r.x, r.y, r.w * 2 / 5, r.h / 4);
    canvas.fill_rounded(tab, 4 * u, with_alpha(accent & 0x00ff_ffff, 200));
    let body = Rect::new(r.x, r.y + r.h / 7, r.w, r.h * 6 / 7);
    canvas.fill_rounded(body, 6 * u, with_alpha(accent & 0x00ff_ffff, 235));
    let front = Rect::new(r.x, r.y + r.h / 3, r.w, r.h * 2 / 3);
    canvas.fill_rounded(front, 6 * u, accent);
}

fn draw_file(canvas: &mut Canvas, r: Rect) {
    let u = m().unit;
    let page = Rect::new(r.x + r.w / 8, r.y, r.w * 3 / 4, r.h);
    canvas.fill_rounded(page, 5 * u, rgb(0xf4, 0xf6, 0xfa));
    for i in 0..4 {
        canvas.fill(
            Rect::new(
                page.x + 8 * u,
                page.y + 14 * u + i * 8 * u,
                page.w - 16 * u - (i % 2) * 10 * u,
                2 * u,
            ),
            rgb(0xb8, 0xc0, 0xcc),
        );
    }
}

pub fn draw(
    canvas: &mut Canvas,
    entries: &[Entry],
    selected: Option<usize>,
    t: &Theme,
    f: &UiFonts,
) {
    let u = m().unit;
    for (i, e) in entries.iter().enumerate() {
        if e.rect.intersect(&canvas.clip).is_empty() {
            continue;
        }
        if selected == Some(i) {
            canvas.fill_rounded(e.rect.inset(3 * u), 10 * u, with_alpha(0xffffff, 40));
        }
        let size = 52 * u;
        let icon = Rect::new(
            e.rect.x + (e.rect.w - size) / 2,
            e.rect.y + 8 * u,
            size,
            size,
        );
        if e.dir {
            draw_folder(canvas, icon, t.accent);
        } else {
            draw_file(canvas, icon);
        }
        // Ad: en çok iki satır, ortalı, okunur olsun diye gölgeli.
        let max = e.rect.w - 8 * u;
        let mut lines: Vec<String> = Vec::new();
        let mut current = String::new();
        for c in e.name.chars() {
            current.push(c);
            if f.small.measure(&current) > max {
                current.pop();
                lines.push(core::mem::take(&mut current));
                current.push(c);
                if lines.len() == 2 {
                    break;
                }
            }
        }
        if lines.len() < 2 && !current.is_empty() {
            lines.push(current);
        }
        let mut y = icon.bottom() + 6 * u;
        for line in lines.iter().take(2) {
            let w = f.small.measure(line);
            let x = e.rect.x + (e.rect.w - w) / 2;
            f.small.draw(canvas, x + u, y + u, line, with_alpha(0, 160));
            f.small.draw(canvas, x, y, line, 0xffff_ffff);
            y += f.small.line_height();
        }
    }
}
