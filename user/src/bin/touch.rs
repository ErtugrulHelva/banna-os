#![no_std]
#![no_main]

use banna::{eprintln, fs};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        eprintln!("usage: touch <file>...");
        return 1;
    }
    let mut status = 0;
    for path in &args[1..] {
        if let Err(e) = fs::File::open(path, fs::WRITE | fs::CREATE) {
            eprintln!("touch: {}: {}", path, fs::error_text(e));
            status = 1;
        }
    }
    status
}
