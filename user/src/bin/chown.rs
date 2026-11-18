//! `chown <uid>[:<gid>] <dosya>...` (yalnızca root; `root` adı 0 demektir).

#![no_std]
#![no_main]

use banna::{eprintln, fs};

banna::entry!(main);

fn id(text: &str) -> Option<u32> {
    if text == "root" {
        Some(0)
    } else {
        text.parse().ok()
    }
}

fn main(args: &[&str]) -> i32 {
    let (Some(owner), true) = (args.get(1), args.len() > 2) else {
        eprintln!("usage: chown <uid>[:<gid>] <dosya>...");
        return 1;
    };
    let (uid, gid) = match owner.split_once(':') {
        Some((u, g)) => (id(u), id(g)),
        None => (id(owner), id(owner)),
    };
    let (Some(uid), Some(gid)) = (uid, gid) else {
        eprintln!("chown: sayısal kimlik ya da 'root' beklenir");
        return 1;
    };
    let mut status = 0;
    for path in &args[2..] {
        if let Err(e) = fs::chown(path, uid, gid) {
            eprintln!("chown: {}: {}", path, fs::error_text(e));
            status = 1;
        }
    }
    status
}
