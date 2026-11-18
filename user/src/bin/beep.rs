//! `beep [Hz] [ms]`: /dev/dsp'ye kare dalga çalar.

#![no_std]
#![no_main]

use banna::{Vec, eprintln, fs};

banna::entry!(main);

const RATE: u32 = 48_000;

fn main(args: &[&str]) -> i32 {
    let frequency: u32 = args
        .get(1)
        .and_then(|a| a.parse().ok())
        .unwrap_or(880)
        .clamp(20, 20_000);
    let ms: u32 = args
        .get(2)
        .and_then(|a| a.parse().ok())
        .unwrap_or(200)
        .min(10_000);
    let mut dsp = match fs::File::open("/dev/dsp", fs::WRITE) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("beep: /dev/dsp: {} (ses kartı yok mu?)", fs::error_text(e));
            return 1;
        }
    };
    let total = RATE * ms / 1000;
    let period = RATE / frequency;
    let mut pcm = Vec::with_capacity(total as usize * 4);
    for i in 0..total {
        let edge = (i.min(total - i) as i32 * 16).min(8000) as i16;
        let value = if i % period < period / 2 { edge } else { -edge };
        pcm.extend_from_slice(&value.to_le_bytes());
        pcm.extend_from_slice(&value.to_le_bytes());
    }
    match dsp.write_all(&pcm) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("beep: {}", fs::error_text(e));
            1
        }
    }
}
