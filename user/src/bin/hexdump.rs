//! `hexdump [dosya]`: onaltılık + ASCII döküm (dosya yoksa stdin).

#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, format, fs, io};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    let data = match args.get(1) {
        Some(path) => match fs::read(path) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("hexdump: {}: {}", path, fs::error_text(e));
                return 1;
            }
        },
        None => {
            let mut data = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = io::read_fd(0, &mut buf);
                if n <= 0 {
                    break;
                }
                data.extend_from_slice(&buf[..n as usize]);
            }
            data
        }
    };
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        out.push_str(&format!("{:08x}  ", i * 16));
        for j in 0..16 {
            match chunk.get(j) {
                Some(b) => out.push_str(&format!("{:02x} ", b)),
                None => out.push_str("   "),
            }
            if j == 7 {
                out.push(' ');
            }
        }
        out.push_str(" |");
        for &b in chunk {
            out.push(if (0x20..0x7f).contains(&b) {
                b as char
            } else {
                '.'
            });
        }
        out.push_str("|\n");
        if out.len() > 16 * 1024 {
            if io::write_all(1, out.as_bytes()).is_err() {
                return 1;
            }
            out.clear();
        }
    }
    out.push_str(&format!("{:08x}\n", data.len()));
    let _ = io::write_all(1, out.as_bytes());
    0
}
