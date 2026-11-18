#![no_std]
#![no_main]

use banna::{eprintln, fs, io};

banna::entry!(main);

fn copy_stdin() -> i32 {
    let mut buf = [0u8; 4096];
    loop {
        let n = io::read_fd(0, &mut buf);
        if n <= 0 {
            return 0;
        }
        if io::write_all(1, &buf[..n as usize]).is_err() {
            return 1;
        }
    }
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        return copy_stdin();
    }
    let mut status = 0;
    for path in &args[1..] {
        if *path == "-" {
            status |= copy_stdin();
            continue;
        }
        match fs::read(path) {
            Ok(data) => {
                if io::write_all(1, &data).is_err() {
                    return 1;
                }
            }
            Err(e) => {
                eprintln!("cat: {}: {}", path, fs::error_text(e));
                status = 1;
            }
        }
    }
    status
}
