#![no_std]
#![no_main]

use banna::{fs, println, process, time, vec::Vec};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    println!(
        "Hello from an ELF program! pid {}, args {:?}",
        process::pid(),
        args
    );
    let squares: Vec<u64> = (1..=10).map(|n| n * n).collect();
    println!(
        "heap ok: sum of squares 1..10 = {}",
        squares.iter().sum::<u64>()
    );
    match fs::read("/docs/HELLO.TXT") {
        Ok(data) => println!(
            "/docs/HELLO.TXT: {}",
            core::str::from_utf8(&data).unwrap_or("?").trim()
        ),
        Err(e) => println!("/docs/HELLO.TXT: {}", fs::error_text(e)),
    }
    let path = "/tmp/hello-test.txt";
    let ok = fs::write(path, b"written by hello").is_ok()
        && fs::read(path)
            .map(|d| d == b"written by hello")
            .unwrap_or(false)
        && fs::remove(path).is_ok();
    println!(
        "file write/read/remove: {}",
        if ok { "ok" } else { "FAILED" }
    );
    let start = time::uptime_ms();
    time::sleep_ms(10);
    println!("slept {} ms", time::uptime_ms() - start);
    if ok { 0 } else { 1 }
}
