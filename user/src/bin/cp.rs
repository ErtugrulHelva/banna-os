//! `cp [-r] <kaynak>... <hedef>`: dosya (ve -r ile dizin) kopyalar.

#![no_std]
#![no_main]

use banna::{String, Vec, eprintln, fs};

banna::entry!(main);

fn copy(source: &str, target: &str, recursive: bool) -> Result<(), String> {
    let stat = fs::stat(source).map_err(|e| banna::format!("{}: {}", source, fs::error_text(e)))?;
    if stat.kind == fs::KIND_DIRECTORY {
        if !recursive {
            return Err(banna::format!("{}: dizin (-r gerekir)", source));
        }
        if fs::stat(target).is_err() {
            fs::mkdir(target).map_err(|e| banna::format!("{}: {}", target, fs::error_text(e)))?;
        }
        let entries = fs::read_dir(source)
            .map_err(|e| banna::format!("{}: {}", source, fs::error_text(e)))?;
        for (name, _) in entries {
            copy(
                &banna::format!("{}/{}", source, name),
                &banna::format!("{}/{}", target, name),
                true,
            )?;
        }
        return Ok(());
    }
    let data = fs::read(source).map_err(|e| banna::format!("{}: {}", source, fs::error_text(e)))?;
    fs::write(target, &data).map_err(|e| banna::format!("{}: {}", target, fs::error_text(e)))?;
    let _ = fs::chmod(target, stat.mode & 0o777);
    Ok(())
}

fn main(args: &[&str]) -> i32 {
    let recursive = args.contains(&"-r") || args.contains(&"-R");
    let paths: Vec<&str> = args[1..]
        .iter()
        .copied()
        .filter(|a| !a.starts_with('-'))
        .collect();
    if paths.len() < 2 {
        eprintln!("usage: cp [-r] <kaynak>... <hedef>");
        return 1;
    }
    let dest = paths[paths.len() - 1];
    let into_dir = fs::stat(dest).is_ok_and(|s| s.kind == fs::KIND_DIRECTORY);
    let mut status = 0;
    for source in &paths[..paths.len() - 1] {
        let target = if into_dir {
            let name = source
                .trim_end_matches('/')
                .rsplit('/')
                .next()
                .unwrap_or(source);
            banna::format!("{}/{}", dest.trim_end_matches('/'), name)
        } else {
            String::from(dest)
        };
        if let Err(e) = copy(source, &target, recursive) {
            eprintln!("cp: {}", e);
            status = 1;
        }
    }
    status
}
