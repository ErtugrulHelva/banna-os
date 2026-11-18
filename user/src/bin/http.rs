#![no_std]
#![no_main]

use banna::{fs, io, net, println, time};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    // `http -n <sayı> <url>`: aynı adres art arda istenir; ilki yeni bağlantı, sonrakiler
    // keep-alive havuzundan (bağlantı kurma süresinin etkisini görmek için).
    if args.get(1) == Some(&"-n") {
        let (Some(count), Some(url)) = (args.get(2).and_then(|n| n.parse::<u32>().ok()), args.get(3)) else {
            println!("usage: http -n <sayı> <url>");
            return 1;
        };
        let mut total = 0;
        for i in 1..=count {
            let started = time::uptime_ms();
            match net::get(url) {
                Ok(r) => {
                    let ms = time::uptime_ms() - started;
                    total += ms;
                    println!("http: #{} {} -> {} bytes in {} ms", i, r.status, r.body.len(), ms);
                }
                Err(e) => {
                    println!("http: #{} {}", i, e);
                    return 1;
                }
            }
        }
        println!("http: {} istek, toplam {} ms", count, total);
        return 0;
    }
    let Some(url) = args.get(1) else {
        println!(
            "usage: http <url> [file] | http -n <sayı> <url>   e.g. http example.com/, http https://ecosys.trybanna.com/ or http 10.0.2.2:8000/a.txt out.txt"
        );
        return 1;
    };
    let started = time::uptime_ms();
    let response = match net::get(url) {
        Ok(r) => r,
        Err(e) => {
            println!("http: {}", e);
            return 1;
        }
    };
    let ms = (time::uptime_ms() - started).max(1);
    let status_line = response.headers.lines().next().unwrap_or("");
    match args.get(2) {
        Some(path) => {
            if let Err(e) = fs::write(path, &response.body) {
                println!("http: {}: {}", path, fs::error_text(e));
                return 1;
            }
            println!(
                "{} -> {} ({} bytes)",
                status_line,
                path,
                response.body.len()
            );
        }
        None => {
            println!("{}", status_line);
            io::write_out(&banna::String::from_utf8_lossy(&response.body));
            println!();
        }
    }
    println!(
        "http: {} bytes in {} ms ({} KiB/s)",
        response.body.len(),
        ms,
        response.body.len() as u64 * 1000 / 1024 / ms
    );
    if response.status / 100 == 2 { 0 } else { 2 }
}
