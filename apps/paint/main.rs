#![no_std]
#![no_main]

use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
    Window,
};
use banna::println;

banna::entry!(main);

const W: usize = 480;
const H: usize = 320;
const COLORS: [u32; 6] = [0xffffff, 0xff5f57, 0x5fd78f, 0x5fb4ff, 0xffd75f, 0x1b1f2a];

fn main(_: &[&str]) -> i32 {
    let mut window = match Window::new("Paint", W, H) {
        Ok(w) => w,
        Err(e) => {
            println!("paint: cannot open a window ({})", e);
            return 1;
        }
    };
    let mut color = 0;
    let mut drawing = false;
    let mut last = (0i32, 0i32);
    let redraw_palette = |w: &mut Window, selected: usize| {
        for (i, &c) in COLORS.iter().enumerate() {
            w.fill(
                8 + i * 28,
                8,
                22,
                22,
                if i == selected { 0xffffff } else { 0x30343c },
            );
            w.fill(10 + i * 28, 10, 18, 18, c);
        }
    };
    window.fill(0, 0, W, H, 0x1b1f2a);
    redraw_palette(&mut window, color);
    window.present();
    loop {
        let Some(event) = window.event(1000) else {
            continue;
        };
        match event.kind {
            EVENT_CLOSE => return 0,
            EVENT_RESIZE => {
                let (old_w, old_h) = window.resize(event.x as usize, event.y as usize);
                let (w, h) = (window.width, window.height);
                // Yeni açılan alanlar zemin rengiyle doldurulur; çizim korunur.
                window.fill(old_w.min(w), 0, w.saturating_sub(old_w), h, 0x1b1f2a);
                window.fill(0, old_h.min(h), w, h.saturating_sub(old_h), 0x1b1f2a);
                redraw_palette(&mut window, color);
            }
            EVENT_KEY if event.key == b'c' as u32 => {
                let (w, h) = (window.width, window.height);
                window.fill(0, 0, w, h, 0x1b1f2a);
                redraw_palette(&mut window, color);
            }
            EVENT_MOUSE_DOWN => {
                if event.y < 32 && (event.x as usize) < 8 + COLORS.len() * 28 {
                    color = ((event.x as usize).saturating_sub(8) / 28).min(COLORS.len() - 1);
                    redraw_palette(&mut window, color);
                } else {
                    drawing = true;
                    last = (event.x, event.y);
                }
            }
            EVENT_MOUSE_UP => drawing = false,
            EVENT_MOUSE_MOVE if drawing => {
                let (x0, y0) = last;
                let steps = (event.x - x0).abs().max((event.y - y0).abs()).max(1);
                for s in 0..=steps {
                    let x = x0 + (event.x - x0) * s / steps;
                    let y = y0 + (event.y - y0) * s / steps;
                    if y > 34 {
                        window.fill(
                            (x - 2).max(0) as usize,
                            (y - 2).max(0) as usize,
                            4,
                            4,
                            COLORS[color],
                        );
                    }
                }
                last = (event.x, event.y);
            }
            _ => continue,
        }
        window.present();
    }
}
