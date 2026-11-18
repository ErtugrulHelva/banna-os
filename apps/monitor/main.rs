//! Sistem İzleyici: çalışma süresi, bellek, kare hızı, çekirdek yükleri, görevler ve ağ.
//! Bilgiyi saniyede bir masaüstünden (`desktop::query`) okur.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use banna::desktop::{self, Info};
use banna::gfx::text::UiFonts;
use banna::gfx::{Argb, Canvas, Rect, rgb, with_alpha};
use banna::gui::{EVENT_CLOSE, EVENT_KEY, EVENT_RESIZE, Window};
use banna::theme::{Prefs, Theme};
use banna::ui;

banna::entry!(main);

fn bar(c: &mut Canvas, rect: Rect, permille: i64, color: Argb, t: &Theme) {
    c.fill_rounded(
        rect,
        rect.h / 2,
        with_alpha(if t.dark { 0xffffff } else { 0 }, 24),
    );
    let filled = (rect.w as i64 * permille.clamp(0, 1000) / 1000) as i32;
    if filled > 0 {
        c.fill_rounded(
            Rect::new(rect.x, rect.y, filled.max(rect.h), rect.h),
            rect.h / 2,
            color,
        );
    }
}

fn render(window: &mut Window, info: &Info, f: &UiFonts, u: i32) {
    let t = Theme::from(&Prefs::parse(&info.text()));
    let (w, h) = (window.width as i32, window.height as i32);
    let mut c = window.canvas();
    c.fill(Rect::new(0, 0, w, h), t.window_bg);
    let x = 18 * u;
    let mut y = 14 * u;
    let line = f.body.line_height() + 4 * u;
    let label_w = 96 * u;
    let bar_w = w - 2 * x;
    let row = |c: &mut Canvas, y: i32, label: &str, value: &str| {
        f.body.draw(c, x, y, label, t.muted);
        f.body.draw(c, x + label_w, y, value, t.text);
    };

    let uptime = info.num("sys.uptime_ms") / 1000;
    row(
        &mut c,
        y,
        "Çalışma",
        &format!(
            "{}s {:02}dk {:02}sn",
            uptime / 3600,
            (uptime / 60) % 60,
            uptime % 60
        ),
    );
    y += line;
    let total = info.num("sys.mem_total");
    let used = total - info.num("sys.mem_free");
    row(
        &mut c,
        y,
        "Bellek",
        &format!("{} / {} MiB", used >> 20, total >> 20),
    );
    y += line;
    bar(
        &mut c,
        Rect::new(x, y, bar_w, 6 * u),
        used * 1000 / total.max(1),
        t.accent,
        &t,
    );
    y += 14 * u;
    let frame_us = info.num("display.frame_us");
    row(
        &mut c,
        y,
        "Ekran",
        &format!("{} fps, kare {} µs", info.get("display.fps"), frame_us),
    );
    y += line;
    bar(
        &mut c,
        Rect::new(x, y, bar_w, 6 * u),
        frame_us * 1000 / 16_667,
        rgb(0x5f, 0xd7, 0x8f),
        &t,
    );
    y += 14 * u;
    let loads: alloc::vec::Vec<i64> = info
        .get("sched.loads")
        .split(',')
        .filter_map(|l| l.parse().ok())
        .collect();
    row(&mut c, y, "İşlemci", &format!("{} çekirdek", loads.len()));
    y += line;
    let gap = 6 * u;
    let each = ((bar_w - gap * (loads.len() as i32 - 1)) / loads.len().max(1) as i32).max(4);
    for (i, load) in loads.iter().enumerate() {
        bar(
            &mut c,
            Rect::new(x + i as i32 * (each + gap), y, each, 6 * u),
            load * 10,
            rgb(0xff, 0xb4, 0x5f),
            &t,
        );
    }
    y += 14 * u;
    row(
        &mut c,
        y,
        "Görevler",
        &format!(
            "{} thread, {} işlem",
            info.get("sched.threads"),
            info.get("sched.processes")
        ),
    );
    y += line;
    row(&mut c, y, "Ağ", info.get("net.status"));
}

fn main(_: &[&str]) -> i32 {
    let u = ui::unit();
    let mut window = match Window::new("Sistem İzleyici", (400 * u) as usize, (196 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("monitor: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let fonts = ui::fonts();
    loop {
        if let Some(info) = desktop::query() {
            render(&mut window, &info, &fonts, u);
            window.present();
        }
        // Saniyede bir yenile; kapatma ve Esc ile çık.
        while let Some(event) = window.event(1000) {
            match event.kind {
                EVENT_CLOSE => return 0,
                EVENT_RESIZE => {
                    window.resize(event.x as usize, event.y as usize);
                    break;
                }
                EVENT_KEY if event.key == 27 => return 0,
                _ => {}
            }
        }
    }
}
