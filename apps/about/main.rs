//! Banna OS Hakkında: sürüm, işlemci, bellek ve ekran bilgisi.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;

use banna::desktop;
use banna::gfx::{Rect, rgb};
use banna::gui::{EVENT_CLOSE, EVENT_KEY, EVENT_RESIZE, Window};
use banna::icons;
use banna::theme::{Prefs, Theme};
use banna::ui;

banna::entry!(main);

fn main(_: &[&str]) -> i32 {
    let Some(info) = desktop::query() else {
        banna::eprintln!("about: masaüstü çalışmıyor");
        return 1;
    };
    let u = ui::unit();
    let mut window = match Window::new("Banna OS Hakkında", (460 * u) as usize, (230 * u) as usize)
    {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("about: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let f = ui::fonts();
    let t = Theme::from(&Prefs::parse(&info.text()));
    loop {
        let (w, h) = (window.width as i32, window.height as i32);
        {
            let mut c = window.canvas();
            c.fill(Rect::new(0, 0, w, h), t.window_bg);
            let x = 24 * u;
            let y = 20 * u;
            let logo = Rect::new(x, y, 56 * u, 56 * u);
            icons::logo(&mut c, logo, f.title, rgb(0xff, 0xff, 0xff), rgb(0, 0, 0));
            f.title.draw(
                &mut c,
                x + 76 * u,
                y + 6 * u,
                &format!("Banna OS {}", info.get("sys.version")),
                t.text,
            );
            f.small.draw(
                &mut c,
                x + 76 * u,
                y + 32 * u,
                "Rust ile yazılmış x86_64 işletim sistemi",
                t.muted,
            );
            let mut ty = y + 84 * u;
            for line in [
                format!(
                    "{} • {} çekirdek",
                    info.get("sys.cpu"),
                    info.get("sys.cores")
                ),
                format!(
                    "{} MiB bellek • {}",
                    info.num("sys.mem_total") >> 20,
                    info.get("display.size").replace('x', "×")
                ),
            ] {
                f.body
                    .draw_clipped(&mut c, x, ty, &line, w - 2 * x, t.muted);
                ty += f.body.line_height();
            }
            f.small.draw(
                &mut c,
                x,
                ty + 10 * u,
                "Ayrıntılar: Ayarlar → Hakkında",
                t.text,
            );
        }
        window.present();
        // Kapatılana ya da yeniden boyutlandırılana kadar bekle.
        loop {
            let Some(event) = window.event(60_000) else {
                continue;
            };
            match event.kind {
                EVENT_CLOSE => return 0,
                EVENT_KEY if event.key == 27 => return 0,
                EVENT_RESIZE => {
                    window.resize(event.x as usize, event.y as usize);
                    break;
                }
                _ => {}
            }
        }
    }
}
