//! `sudo <komut>`: yönetici parolası doğrulanınca komutu root olarak çalıştırır.

#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, fs, io, print, println, process, tty};

banna::entry!(main);

const TRIES: usize = 3;

/// Parolayı yankısız okur (terminal değilse düz satır).
fn read_password(prompt: &str) -> Option<String> {
    print!("{}", prompt);
    let old = tty::mode(0).ok();
    if let Some(mode) = old {
        let _ = tty::set_mode(0, (mode & !tty::ECHO) | tty::ICANON);
    }
    let mut line = Vec::new();
    let mut buf = [0u8; 64];
    let ok = loop {
        let n = io::read_fd(0, &mut buf);
        if n <= 0 {
            break false;
        }
        let chunk = &buf[..n as usize];
        match chunk.iter().position(|&b| b == b'\n') {
            Some(end) => {
                line.extend_from_slice(&chunk[..end]);
                break true;
            }
            None => line.extend_from_slice(chunk),
        }
    };
    if let Some(mode) = old {
        let _ = tty::set_mode(0, mode);
    }
    println!();
    ok.then(|| String::from_utf8_lossy(&line).into_owned())
}

fn resolve(name: &str) -> Option<String> {
    let path = if name.contains('/') {
        fs::normalize(&process::cwd(), name)
    } else {
        banna::format!("/bin/{}", name)
    };
    fs::stat(&path)
        .ok()
        .filter(|s| s.kind == fs::KIND_FILE)
        .map(|_| path)
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 2 {
        eprintln!("usage: sudo <command> [args]...");
        return 1;
    }
    if process::uid() != 0 {
        let user = process::username();
        let mut granted = false;
        for _ in 0..TRIES {
            let Some(password) = read_password(&banna::format!("[sudo] {} parolası: ", user))
            else {
                return 1;
            };
            match process::elevate(&password) {
                Ok(()) => {
                    granted = true;
                    break;
                }
                Err(process::EPERM) => {
                    eprintln!("sudo: {} yönetici değil", user);
                    return 1;
                }
                Err(_) => eprintln!("Parola yanlış, tekrar dene."),
            }
        }
        if !granted {
            eprintln!("sudo: {} hatalı deneme", TRIES);
            return 1;
        }
    }
    let command = &args[1..];
    match resolve(command[0]) {
        Some(path) => {
            let code = process::exec(&path, command);
            eprintln!("sudo: {}: {}", command[0], fs::error_text(code));
            126
        }
        None if process::kcmd(&command.join(" ")).is_ok() => 0,
        None => {
            eprintln!("sudo: {}: command not found", command[0]);
            127
        }
    }
}
