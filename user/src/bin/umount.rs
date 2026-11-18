#![no_std]
#![no_main]

use banna::{eprintln, fs};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    let Some(point) = args.get(1) else {
        eprintln!("usage: umount <dizin>");
        return 1;
    };
    match fs::umount(point) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("umount: {}: {}", point, fs::error_text(e));
            1
        }
    }
}
