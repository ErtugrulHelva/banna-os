#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, fs, io, println};

banna::entry!(main);

fn read_stdin() -> Vec<u8> {
    let mut data = Vec::new();
    let mut buf = [0u8; 4096];
    loop {
        let n = io::read_fd(0, &mut buf);
        if n <= 0 {
            return data;
        }
        data.extend_from_slice(&buf[..n as usize]);
    }
}

/// Alt dize arar; `-i` büyük/küçük harf ayırmaz, `-v` eşleşmeyenleri yazar.
fn main(args: &[&str]) -> i32 {
    let mut ignore_case = false;
    let mut invert = false;
    let mut rest = Vec::new();
    for arg in &args[1..] {
        match *arg {
            "-i" => ignore_case = true,
            "-v" => invert = true,
            a => rest.push(a),
        }
    }
    let Some((pattern, files)) = rest.split_first() else {
        eprintln!("usage: grep [-i] [-v] <text> [file]...");
        return 2;
    };
    let pattern = if ignore_case {
        pattern.to_lowercase()
    } else {
        String::from(*pattern)
    };
    let mut sources: Vec<(Option<&str>, Vec<u8>)> = Vec::new();
    if files.is_empty() {
        sources.push((None, read_stdin()));
    }
    for path in files {
        match fs::read(path) {
            Ok(data) => sources.push((Some(path), data)),
            Err(e) => eprintln!("grep: {}: {}", path, fs::error_text(e)),
        }
    }
    let mut found = false;
    let label = files.len() > 1;
    for (name, data) in &sources {
        for line in String::from_utf8_lossy(data).lines() {
            let hay = if ignore_case {
                line.to_lowercase()
            } else {
                String::from(line)
            };
            if hay.contains(pattern.as_str()) != invert {
                found = true;
                match name {
                    Some(n) if label => println!("{}:{}", n, line),
                    _ => println!("{}", line),
                }
            }
        }
    }
    if found { 0 } else { 1 }
}
