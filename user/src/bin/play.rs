//! `play <dosya.wav>`: PCM WAV çalar (8/16 bit, mono/stereo, her örnekleme hızı → 48 kHz).

#![no_std]
#![no_main]

use banna::{Vec, eprintln, fs, println};

banna::entry!(main);

const RATE: u32 = 48_000;

struct Wave<'a> {
    channels: u16,
    rate: u32,
    bits: u16,
    data: &'a [u8],
}

fn parse(bytes: &[u8]) -> Result<Wave<'_>, &'static str> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err("WAV (RIFF/WAVE) değil");
    }
    let mut at = 12;
    let mut format = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let size = u32::from_le_bytes(bytes[at + 4..at + 8].try_into().unwrap()) as usize;
        let body = &bytes[at + 8..(at + 8 + size).min(bytes.len())];
        if id == b"fmt " && body.len() >= 16 {
            let kind = u16::from_le_bytes([body[0], body[1]]);
            if kind != 1 {
                return Err("yalnızca sıkıştırılmamış PCM desteklenir");
            }
            format = Some((
                u16::from_le_bytes([body[2], body[3]]),
                u32::from_le_bytes(body[4..8].try_into().unwrap()),
                u16::from_le_bytes([body[14], body[15]]),
            ));
        } else if id == b"data" {
            let (channels, rate, bits) = format.ok_or("fmt bölümü yok")?;
            if !(1..=2).contains(&channels) || !(bits == 8 || bits == 16) || rate == 0 {
                return Err("desteklenmeyen biçim (1-2 kanal, 8/16 bit)");
            }
            return Ok(Wave {
                channels,
                rate,
                bits,
                data: body,
            });
        }
        at += 8 + size + (size & 1);
    }
    Err("data bölümü yok")
}

fn sample(w: &Wave, frame: usize, channel: u16) -> i16 {
    let channel = channel.min(w.channels - 1) as usize;
    let bytes = (w.bits / 8) as usize;
    let at = (frame * w.channels as usize + channel) * bytes;
    match w.bits {
        8 => ((w.data[at] as i16) - 128) << 8,
        _ => i16::from_le_bytes([w.data[at], w.data[at + 1]]),
    }
}

fn main(args: &[&str]) -> i32 {
    let Some(path) = args.get(1) else {
        eprintln!("usage: play <dosya.wav>");
        return 1;
    };
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("play: {}: {}", path, fs::error_text(e));
            return 1;
        }
    };
    let wave = match parse(&bytes) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("play: {}: {}", path, e);
            return 1;
        }
    };
    let frames = wave.data.len() / (wave.channels as usize * (wave.bits / 8) as usize);
    let out_frames = frames as u64 * RATE as u64 / wave.rate as u64;
    println!(
        "{}: {} Hz, {} bit, {} kanal, {}.{} sn",
        path,
        wave.rate,
        wave.bits,
        wave.channels,
        frames as u32 / wave.rate,
        frames as u32 % wave.rate * 10 / wave.rate
    );
    let mut dsp = match fs::File::open("/dev/dsp", fs::WRITE) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("play: /dev/dsp: {}", fs::error_text(e));
            return 1;
        }
    };
    let mut chunk = Vec::with_capacity(16 * 1024);
    for i in 0..out_frames {
        let src = (i * wave.rate as u64 / RATE as u64) as usize;
        chunk.extend_from_slice(&sample(&wave, src, 0).to_le_bytes());
        chunk.extend_from_slice(&sample(&wave, src, 1).to_le_bytes());
        if chunk.len() >= 16 * 1024 {
            if dsp.write_all(&chunk).is_err() {
                return 1;
            }
            chunk.clear();
        }
    }
    if dsp.write_all(&chunk).is_err() {
        return 1;
    }
    0
}
