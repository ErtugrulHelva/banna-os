#![no_std]
#![no_main]

use banna::{Vec, eprintln, fs, io, println};

banna::entry!(main);

fn count(data: &[u8]) -> (usize, usize, usize) {
    let text = banna::String::from_utf8_lossy(data);
    let lines = data.iter().filter(|&&b| b == b'\n').count();
    (lines, text.split_whitespace().count(), data.len())
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        let mut data = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            let n = io::read_fd(0, &mut buf);
            if n <= 0 {
                break;
            }
            data.extend_from_slice(&buf[..n as usize]);
        }
        let (l, w, c) = count(&data);
        println!("{:>7} {:>7} {:>7}", l, w, c);
        return 0;
    }
    let mut status = 0;
    for path in &args[1..] {
        match fs::read(path) {
            Ok(data) => {
                let (l, w, c) = count(&data);
                println!("{:>7} {:>7} {:>7} {}", l, w, c, path);
            }
            Err(e) => {
                eprintln!("wc: {}: {}", path, fs::error_text(e));
                status = 1;
            }
        }
    }
    status
}
