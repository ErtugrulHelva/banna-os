#![no_std]
#![no_main]

use banna::{net, println};

banna::entry!(main);

fn main(args: &[&str]) -> i32 {
    let port: u16 = args.get(1).and_then(|p| p.parse().ok()).unwrap_or(7);
    let count: u32 = args.get(2).and_then(|c| c.parse().ok()).unwrap_or(1);
    let listener = match net::TcpListener::bind(port) {
        Ok(l) => l,
        Err(e) => {
            println!("tcpecho: port {}: {}", port, net::error_text(e));
            return 1;
        }
    };
    println!("tcpecho: listening on port {}", port);
    for _ in 0..count {
        let mut stream = match listener.accept(0) {
            Ok(s) => s,
            Err(e) => {
                println!("tcpecho: accept: {}", net::error_text(e));
                return 1;
            }
        };
        let mut total = 0usize;
        let mut buf = [0u8; 4096];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    total += n;
                    if stream.write_all(&buf[..n]).is_err() {
                        break;
                    }
                }
                Err(e) => {
                    println!("tcpecho: {}", net::error_text(e));
                    break;
                }
            }
        }
        println!("tcpecho: echoed {} bytes", total);
    }
    0
}
