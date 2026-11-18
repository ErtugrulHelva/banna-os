#![no_std]
#![no_main]

use banna::gui::{EVENT_CLOSE, EVENT_KEY, EVENT_RESIZE, Window};
use banna::{println, time};

banna::entry!(main);

const W: usize = 320;
const H: usize = 200;

fn sine_table() -> [i32; 256] {
    let mut table = [0i32; 256];
    for (i, v) in table.iter_mut().enumerate() {
        let mut x = i as f32 * core::f32::consts::TAU / 256.0;
        if x > core::f32::consts::PI {
            x -= core::f32::consts::TAU;
        }
        let mut term = x;
        let mut sum = x;
        for k in 1..8 {
            term *= -x * x / ((2 * k) as f32 * (2 * k + 1) as f32);
            sum += term;
        }
        *v = (sum * 127.0) as i32;
    }
    table
}

fn palette(sin: &[i32; 256], v: i32) -> u32 {
    let v = (v & 255) as usize;
    let r = (128 + sin[v]).clamp(0, 255) as u32;
    let g = (128 + sin[(v + 85) & 255]).clamp(0, 255) as u32;
    let b = (128 + sin[(v + 170) & 255]).clamp(0, 255) as u32;
    r << 16 | g << 8 | b
}

fn main(args: &[&str]) -> i32 {
    let table = sine_table();
    let max_frames: u64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(u64::MAX);
    let mut window = match Window::new("Plasma", W, H) {
        Ok(w) => w,
        Err(e) => {
            println!(
                "plasma: cannot open a window ({}); start the desktop first",
                e
            );
            return 1;
        }
    };
    let frame_ns = 1_000_000_000 / 60;
    let start = time::now_ns();
    let mut frame = 0u64;
    let sin = |i: i32| table[(i & 255) as usize];
    while frame < max_frames {
        let t = frame as i32;
        let (w, h) = (window.width, window.height);
        for y in 0..h {
            let row = &mut window.pixels[y * w..(y + 1) * w];
            let a = sin(y as i32 * 2 + t * 3);
            for (x, px) in row.iter_mut().enumerate() {
                let v = sin(x as i32 * 2 + t * 2) + a + sin((x as i32 + y as i32 + t * 4) / 2);
                *px = palette(&table, v / 2 + t);
            }
        }
        window.present();
        frame += 1;
        while let Some(event) = window.event(0) {
            if event.kind == EVENT_CLOSE || (event.kind == EVENT_KEY && event.key == 27) {
                return 0;
            }
            if event.kind == EVENT_RESIZE {
                window.resize(event.x as usize, event.y as usize);
            }
        }
        let target = start + frame * frame_ns;
        let now = time::now_ns();
        if target > now {
            time::sleep_ms((target - now) / 1_000_000);
        }
    }
    let elapsed = time::now_ns() - start;
    println!(
        "plasma: {} frames in {} ms ({} fps)",
        frame,
        elapsed / 1_000_000,
        frame * 1_000_000_000 / elapsed.max(1)
    );
    0
}
