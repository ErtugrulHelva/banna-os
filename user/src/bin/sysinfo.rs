#![no_std]
#![no_main]

use banna::{println, process};

banna::entry!(main);

fn text(bytes: &[u8]) -> &str {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    core::str::from_utf8(&bytes[..end]).unwrap_or("?").trim()
}

fn main(_: &[&str]) -> i32 {
    let info = process::sysinfo();
    let s = info.uptime_ms / 1000;
    println!(
        "CPU      {} ({}), {} core(s), TSC {} MHz",
        text(&info.brand),
        text(&info.vendor),
        info.cpus,
        info.tsc_mhz
    );
    println!(
        "Memory   {} MiB used / {} MiB",
        (info.total_memory - info.free_memory) >> 20,
        info.total_memory >> 20
    );
    println!(
        "Uptime   {}h {:02}m {:02}s",
        s / 3600,
        (s / 60) % 60,
        s % 60
    );
    println!("Threads  {}, processes {}", info.threads, info.processes);
    println!("Display  {} fps", info.fps);
    0
}
