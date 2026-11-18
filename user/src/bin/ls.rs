//! `ls [-l] [-a] [yol]`: dizin içeriği; `-l` kip, sahip, boyut ve tarih gösterir.

#![no_std]
#![no_main]

use banna::{String, Vec, fs, println, process};

banna::entry!(main);

fn mode_text(kind: u32, mode: u32) -> String {
    let mut s = String::from(match kind {
        fs::KIND_DIRECTORY => "d",
        fs::KIND_DEVICE => "b",
        _ => "-",
    });
    for shift in [6, 3, 0] {
        let bits = (mode >> shift) & 7;
        s.push(if bits & 4 != 0 { 'r' } else { '-' });
        s.push(if bits & 2 != 0 { 'w' } else { '-' });
        s.push(if bits & 1 != 0 { 'x' } else { '-' });
    }
    if mode & 0o1000 != 0 {
        s.pop();
        s.push('t');
    }
    s
}

fn owner_text(uid: u32) -> String {
    if uid == 0 {
        String::from("root")
    } else if uid == process::uid() {
        process::username()
    } else {
        banna::format!("{}", uid)
    }
}

fn date_text(unix: u32) -> String {
    if unix == 0 {
        return String::from("-");
    }
    let days = unix as i64 / 86400;
    let secs = unix as i64 % 86400;
    // Gregoryen takvim (1970'ten itibaren gün → tarih).
    let z = days + 719468;
    let era = z / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    banna::format!(
        "{:02}.{:02}.{} {:02}:{:02}",
        d,
        m,
        y,
        secs / 3600,
        secs % 3600 / 60
    )
}

fn main(args: &[&str]) -> i32 {
    let long = args.iter().any(|a| a.starts_with('-') && a.contains('l'));
    let all = args.iter().any(|a| a.starts_with('-') && a.contains('a'));
    let paths: Vec<&str> = args[1..]
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    let path = paths.first().copied().unwrap_or(".");
    match fs::stat(path) {
        Ok(s) if s.kind != fs::KIND_DIRECTORY => {
            println!("{}", path);
            return 0;
        }
        Err(e) => {
            println!("ls: {}: {}", path, fs::error_text(e));
            return 1;
        }
        _ => {}
    }
    match fs::read_dir(path) {
        Ok(mut entries) => {
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            for (name, stat) in entries {
                if !all && name.starts_with('.') {
                    continue;
                }
                if long {
                    println!(
                        "{} {:<8} {:>10} {}  {}{}",
                        mode_text(stat.kind, stat.mode),
                        owner_text(stat.uid),
                        stat.size,
                        date_text(stat.mtime),
                        name,
                        if stat.kind == fs::KIND_DIRECTORY {
                            "/"
                        } else {
                            ""
                        }
                    );
                } else if stat.kind == fs::KIND_DIRECTORY {
                    println!("  {:<24} <dir>", name);
                } else {
                    println!("  {:<24} {:>9}", name, stat.size);
                }
            }
            0
        }
        Err(e) => {
            println!("ls: {}: {}", path, fs::error_text(e));
            1
        }
    }
}
