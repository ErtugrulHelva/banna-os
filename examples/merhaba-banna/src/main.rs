//! merhaba-banna: Banna OS masaüstü uygulaması.

#![no_std]
#![no_main]

use banna::prelude::*;
use banna::icons;

banna::main!(main);

fn draw(window: &mut Window, theme: &Theme) {
    let u = ui::unit();
    let f = ui::fonts();
    let (w, h) = (window.width as i32, window.height as i32);
    let mut c = window.canvas();
    c.fill(Rect::new(0, 0, w, h), theme.window_bg);
    let logo = Rect::new(24 * u, 24 * u, 48 * u, 48 * u);
    icons::logo(&mut c, logo, f.title, theme.accent, rgb(0xff, 0xff, 0xff));
    f.heading.draw(&mut c, logo.right() + 16 * u, 26 * u, "Merhaba Banna!", theme.text);
    f.body.draw(
        &mut c,
        24 * u,
        96 * u,
        "Bu uygulama brct ile Rust'ta yazıldı.",
        theme.muted,
    );
}

fn main(_args: &[&str]) -> i32 {
    let u = ui::unit() as usize;
    let mut window = match Window::new("merhaba-banna", 420 * u, 160 * u) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("merhaba-banna: pencere açılamadı ({})", e);
            return 1;
        }
    };
    // Masaüstünün teması (koyu/açık, vurgu rengi).
    let theme = desktop::query()
        .map(|info| Theme::from(&Prefs::parse(&info.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    draw(&mut window, &theme);
    window.present();
    loop {
        if let Some(event) = window.event(60_000) {
            match event.kind {
                EVENT_CLOSE => return 0,
                EVENT_KEY if event.key == 27 => return 0,
                EVENT_RESIZE => {
                    window.resize(event.x as usize, event.y as usize);
                    draw(&mut window, &theme);
                    window.present();
                }
                _ => {}
            }
        }
    }
}
