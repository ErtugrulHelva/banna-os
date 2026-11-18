#![no_std]
#![no_main]

use banna::{eprintln, fs};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        eprintln!("usage: mkdir <dir>...");
        return 1;
    }
    let mut status = 0;
    for path in &args[1..] {
        if let Err(e) = fs::mkdir(path) {
            eprintln!("mkdir: {}: {}", path, fs::error_text(e));
            status = 1;
        }
    }
    status
}
