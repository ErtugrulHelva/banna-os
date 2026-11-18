//! `mv <kaynak>... <hedef>`: aynı birimde taşır; birimler arasında kopyalayıp siler.

#![no_std]
#![no_main]

use banna::{String, eprintln, fs};

banna::entry!(main);

fn target_for(source: &str, dest: &str) -> String {
    match fs::stat(dest) {
        Ok(s) if s.kind == fs::KIND_DIRECTORY => {
            let name = source
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(source);
            banna::format!("{}/{}", dest.trim_end_matches('/'), name)
        }
        _ => String::from(dest),
    }
}

fn main(args: &[&str]) -> i32 {
    if args.len() < 3 {
        eprintln!("usage: mv <kaynak>... <hedef>");
        return 1;
    }
    let dest = args[args.len() - 1];
    let mut status = 0;
    for source in &args[1..args.len() - 1] {
        let target = target_for(source, dest);
        match fs::rename(source, &target) {
            Ok(()) => {}
            Err(-95) => {
                // Farklı birim: kopyala ve sil (yalnızca dosyalar).
                let copied = fs::read(source).and_then(|d| fs::write(&target, &d));
                match copied.and_then(|_| fs::remove(source)) {
                    Ok(()) => {}
                    Err(e) => {
                        eprintln!("mv: {}: {}", source, fs::error_text(e));
                        status = 1;
                    }
                }
            }
            Err(e) => {
                eprintln!("mv: {} -> {}: {}", source, target, fs::error_text(e));
                status = 1;
            }
        }
    }
    status
}
