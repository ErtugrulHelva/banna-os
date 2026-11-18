//! `chmod <sekizlik-kip> <dosya>...` (ör. chmod 755 betik).

#![no_std]
#![no_main]

use banna::{eprintln, fs};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    let (Some(mode), true) = (args.get(1), args.len() > 2) else {
        eprintln!("usage: chmod <kip> <dosya>...   (ör. chmod 644 notlar.txt)");
        return 1;
    };
    let Ok(mode) = u32::from_str_radix(mode, 8) else {
        eprintln!("chmod: kip sekizlik olmalı (ör. 755)");
        return 1;
    };
    let mut status = 0;
    for path in &args[2..] {
        if let Err(e) = fs::chmod(path, mode) {
            eprintln!("chmod: {}: {}", path, fs::error_text(e));
            status = 1;
        }
    }
    status
}
