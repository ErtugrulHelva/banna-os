//! brct'nin terminal tarafı. Banna OS'ta Rust kodu yazılıp burada derlenir: `brct new`
//! proje açar, `brct build` Banna Rust derleyicisiyle (`banna::brs`) `.banna` çalıştırılabilir
//! dosyası üretir, `brct run` derleyip çalıştırır. SDK ile bilgisayarda derlenen tam Rust
//! uygulamaları (`brct new --sdk`) da kurulup yönetilebilir.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::sys::Stat;
use banna::{eprintln, fs, println, process};

const FILES_DIR: &str = "/var/hatch/files";
const INSTALLED: &str = "/var/hatch/installed";

type Result<T> = core::result::Result<T, String>;

fn size_text(bytes: u64) -> String {
    match bytes {
        b if b >= 1024 * 1024 => format!("{}.{} MiB", b >> 20, (b & 0xfffff) * 10 >> 20),
        b if b >= 1024 => format!("{} KiB", b.div_ceil(1024)),
        b => format!("{} bayt", b),
    }
}

fn help() -> i32 {
    println!("brct — Banna Rust Compatibility Tool (Banna OS)");
    println!();
    println!("  brct list [--all]             brct ile yapılmış uygulamalar (--all: tümü)");
    println!("  brct run <ad> [arg...]   uygulamayı ya da programı çalıştırır");
    println!("  brct info <ad>               app.conf, yol, boyut ve paket sürümü");
    println!("  brct install <yol>                paket / .app klasörü / program kurar (sudo)");
    println!("  brct remove <ad>              kurulu uygulamayı kaldırır (sudo)");
    println!("  brct new <ad>                 Banna OS'ta derlenen yeni Rust projesi");
    println!("  brct build [proje|dosya.rs]   .banna çalıştırılabilir dosyası derler");
    println!("  brct check [proje|dosya.rs]   yalnızca derleme hatalarını denetler");
    println!("  brct run [proje|dosya.rs]     derler ve çalıştırır (-- sonrası argümanlar)");
    println!("  brct new <ad> --sdk [--cli]   bilgisayarda (rustc) derlenen SDK projesi");
    println!("  brct doctor                   kurulumu ve depoyu denetler");
    println!("  brct open                       brct penceresini açar");
    0
}

fn read_text(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn conf_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|l| {
        let (k, v) = l.split_once('=')?;
        (k.trim() == key).then(|| String::from(v.trim()))
    })
}

fn is_dir(path: &str) -> bool {
    fs::stat(path).is_ok_and(|s| s.kind == fs::KIND_DIRECTORY)
}

fn is_file(path: &str) -> bool {
    fs::stat(path).is_ok_and(|s| s.kind == fs::KIND_FILE)
}

fn tree_size(path: &str) -> u64 {
    match fs::stat(path) {
        Ok(s) if s.kind == fs::KIND_DIRECTORY => fs::read_dir(path)
            .map(|list| {
                list.iter()
                    .map(|(n, _)| tree_size(&format!("{}/{}", path, n)))
                    .sum()
            })
            .unwrap_or(0),
        Ok(s) => s.size,
        Err(_) => 0,
    }
}

fn installed() -> Vec<(String, String)> {
    read_text(INSTALLED)
        .map(|t| {
            t.lines()
                .filter_map(|l| {
                    let (n, v) = l.trim().split_once(' ')?;
                    Some((String::from(n), String::from(v)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn list(all: bool) -> i32 {
    let Ok(entries) = fs::read_dir("/apps") else {
        eprintln!("brct: /apps okunamadı");
        return 1;
    };
    let packages = installed();
    let mut rows = Vec::new();
    for (id, stat) in entries {
        if stat.kind != fs::KIND_DIRECTORY {
            continue;
        }
        let Some(conf) = read_text(&format!("/apps/{}/app.conf", id)) else {
            continue;
        };
        let brct = conf_value(&conf, "builder").as_deref() == Some("brct");
        if !brct && !all {
            continue;
        }
        let name = conf_value(&conf, "name").unwrap_or_else(|| id.clone());
        let version = packages
            .iter()
            .find(|(n, _)| *n == id)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| String::from("yerleşik"));
        rows.push((id.clone(), name, version, brct));
    }
    rows.sort();
    if rows.is_empty() {
        println!(
            "brct ile yapılmış uygulama yok. Kurmak için: hatch install <ad> ya da sudo brct install <yol>"
        );
        return 0;
    }
    for (id, name, version, brct) in rows {
        println!(
            "  {:<18} {:<22} {:<12} {}",
            id,
            name,
            version,
            if brct { "brct" } else { "" }
        );
    }
    0
}

/// Komut adını yola çevirir: masaüstü uygulaması (`/apps/<ad>/<ad>`) ya da program (`/bin/<ad>`).
fn resolve(name: &str) -> Option<(String, bool)> {
    if name.starts_with('/') || name.starts_with('.') {
        let path = fs::normalize(&process::cwd(), name);
        return is_file(&path).then_some((path, false));
    }
    let app = format!("/apps/{}/{}", name, name);
    if is_file(&app) {
        return Some((app, true));
    }
    let bin = format!("/bin/{}", name);
    is_file(&bin).then_some((bin, false))
}

fn run_program(args: &[&str]) -> i32 {
    let Some(&name) = args.first() else {
        eprintln!("kullanım: brct run <ad> [arg...]");
        return 2;
    };
    let Some((path, gui)) = resolve(name) else {
        eprintln!("brct: '{}' bulunamadı (brct list --all)", name);
        return 1;
    };
    let program = path.rsplit('/').next().unwrap_or(name);
    let mut argv: Vec<&str> = alloc::vec![program];
    argv.extend_from_slice(&args[1..]);
    if gui {
        // Pencereli uygulama terminali bekletmez.
        return match process::spawn(&path, &argv) {
            Ok(pid) => {
                println!("{} açıldı (pid {})", program, pid);
                0
            }
            Err(e) => {
                eprintln!("brct: {} açılamadı: {}", path, fs::error_text(e));
                1
            }
        };
    }
    // Komut satırı programı bu terminalin çıktısını paylaşır.
    match process::fork() {
        Ok(0) => {
            let code = process::exec(&path, &argv);
            eprintln!("brct: {} çalıştırılamadı: {}", path, fs::error_text(code));
            process::exit(127);
        }
        Ok(pid) => {
            let code = process::wait(pid);
            if let Some(text) = process::describe_exit(code) {
                eprintln!("{}", text);
            }
            code as i32
        }
        Err(e) => {
            eprintln!("brct: fork: {}", fs::error_text(e));
            1
        }
    }
}

fn info(args: &[&str]) -> i32 {
    let Some(&name) = args.first() else {
        eprintln!("kullanım: brct info <ad>");
        return 2;
    };
    let Some((path, gui)) = resolve(name) else {
        eprintln!("brct: '{}' bulunamadı", name);
        return 1;
    };
    println!("ad:       {}", name);
    println!("yol:      {}", path);
    println!(
        "tür:      {}",
        if gui {
            "masaüstü uygulaması"
        } else {
            "komut satırı programı"
        }
    );
    if gui {
        let dir = format!("/apps/{}", name);
        let conf = read_text(&format!("{}/app.conf", dir)).unwrap_or_default();
        for key in ["name", "description", "builder", "single", "icon"] {
            if let Some(v) = conf_value(&conf, key) {
                println!("{:<9} {}", format!("{}:", key), v);
            }
        }
        println!("boyut:    {}", size_text(tree_size(&dir)));
    } else {
        println!("boyut:    {}", size_text(tree_size(&path)));
    }
    match installed().into_iter().find(|(n, _)| n == name) {
        Some((_, v)) => println!("paket:    {} (hatch remove {} ile kaldırılır)", v, name),
        None => println!("paket:    yok (sistemle gelen ya da elle kurulan)"),
    }
    0
}

fn mkdir_p(path: &str) {
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        let _ = fs::mkdir(&at);
    }
}

fn copy_file(from: &str, to: &str, executable: bool) -> Result<u64> {
    let data = fs::read(from).map_err(|e| format!("{}: {}", from, fs::error_text(e)))?;
    if let Some(slash) = to.rfind('/') {
        mkdir_p(&to[..slash]);
    }
    fs::write(to, &data).map_err(|e| format!("{}: {}", to, fs::error_text(e)))?;
    let _ = fs::chmod(to, if executable { 0o755 } else { 0o644 });
    Ok(data.len() as u64)
}

/// `from` ağacını `to` altına kopyalar; kopyalanan hedef yollarını `out`'a ekler.
fn copy_tree(from: &str, to: &str, out: &mut Vec<String>) -> Result<()> {
    let entries = fs::read_dir(from).map_err(|e| format!("{}: {}", from, fs::error_text(e)))?;
    for (name, stat) in entries {
        // macOS'un USB belleğe bıraktığı üst veri dosyaları kopyalanmaz.
        if name.starts_with("._") || name == ".DS_Store" || name == ".fseventsd" {
            continue;
        }
        let (source, target) = (format!("{}/{}", from, name), format!("{}/{}", to, name));
        if stat.kind == fs::KIND_DIRECTORY {
            copy_tree(&source, &target, out)?;
        } else {
            copy_file(&source, &target, executable(&target, &stat))?;
            out.push(target);
        }
    }
    Ok(())
}

/// Programlar çalıştırılabilir olmalı: /bin altındakiler ve uzantısız uygulama dosyaları.
fn executable(path: &str, _: &Stat) -> bool {
    path.starts_with("/bin/")
        || (path.starts_with("/apps/") && !path.rsplit('/').next().unwrap_or("").contains('.'))
}

fn record(name: &str, version: &str, files: &[String]) -> Result<()> {
    mkdir_p(FILES_DIR);
    let list = format!("{}\n", files.join("\n"));
    fs::write(&format!("{}/{}", FILES_DIR, name), list.as_bytes())
        .map_err(|e| format!("{}: {}", FILES_DIR, fs::error_text(e)))?;
    let mut db = installed();
    db.retain(|(n, _)| n != name);
    db.push((String::from(name), String::from(version)));
    let text: String = db.iter().map(|(n, v)| format!("{} {}\n", n, v)).collect();
    fs::write(INSTALLED, text.as_bytes())
        .map_err(|e| format!("{}: {}", INSTALLED, fs::error_text(e)))
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && !id.starts_with('-')
        && id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// Kaynağın türünü anlar ve kurar: (ad, sürüm, kurulan dosyalar).
fn install_from(source: &str) -> Result<(String, String, Vec<String>)> {
    let base = source.trim_end_matches('/');
    let leaf = base.rsplit('/').next().unwrap_or(base);
    let mut files = Vec::new();
    if is_file(&format!("{}/package.ini", base)) {
        // `brct package` çıktısı: package.ini + files/ ağacı (yalnızca /apps/<ad>/ ve /bin/<ad>).
        let id = String::from(leaf);
        let ini = read_text(&format!("{}/package.ini", base)).unwrap_or_default();
        let version = conf_value(&ini, "version").unwrap_or_else(|| String::from("0.1.0"));
        let root = format!("{}/files", base);
        for (top, target) in [
            ("apps", format!("/apps/{}", id)),
            ("bin", String::from("/bin")),
        ] {
            let dir = if top == "apps" {
                format!("{}/apps/{}", root, id)
            } else {
                format!("{}/bin", root)
            };
            if is_dir(&dir) {
                copy_tree(&dir, &target, &mut files)?;
            }
        }
        if files.is_empty() {
            return Err(format!("{}: files/apps/{}/ ya da files/bin/ yok", base, id));
        }
        return Ok((id, version, files));
    }
    if is_file(&format!("{}/app.conf", base)) {
        // `brct <proje>` çıktısı: target/banna/<ad>.app/ (app.conf + program + simgeler).
        let id = String::from(leaf.trim_end_matches(".app"));
        if !is_file(&format!("{}/{}", base, id)) {
            return Err(format!("{}: {} programı yok", base, id));
        }
        copy_tree(base, &format!("/apps/{}", id), &mut files)?;
        return Ok((id, String::from("0.0.0-yerel"), files));
    }
    if is_file(base) {
        // `.banna` programı uzantısız ada kurulur (`/bin/hesap`).
        let id = String::from(leaf.trim_end_matches(".banna"));
        let target = format!("/bin/{}", id);
        copy_file(base, &target, true)?;
        return Ok((id, String::from("0.0.0-yerel"), alloc::vec![target]));
    }
    Err(format!(
        "{}: paket, .app klasörü ya da program değil",
        source
    ))
}

fn install(args: &[&str]) -> i32 {
    let Some(&source) = args.first() else {
        eprintln!("kullanım: sudo brct install <paket klasörü | ad.app | program>");
        return 2;
    };
    if process::uid() != 0 {
        eprintln!(
            "brct: kurulum yönetici yetkisi ister: sudo brct install {}",
            source
        );
        return 1;
    }
    let path = fs::normalize(&process::cwd(), source);
    match install_from(&path) {
        Ok((id, _, _)) if !valid_id(&id) => {
            eprintln!("brct: geçersiz ad '{}'", id);
            1
        }
        Ok((id, version, files)) => {
            if let Err(e) = record(&id, &version, &files) {
                eprintln!("brct: uyarı: paket kaydı yazılamadı: {}", e);
            }
            let size: u64 = files.iter().map(|f| tree_size(f)).sum();
            println!(
                "{} {} kuruldu ({} dosya, {})",
                id,
                version,
                files.len(),
                size_text(size)
            );
            if files.iter().any(|f| f.starts_with("/apps/")) {
                println!(
                    "Başlatıcıda ve dock'ta görünür; terminalden: brct run {}",
                    id
                );
            } else {
                println!("Çalıştırmak için: {}", id);
            }
            0
        }
        Err(e) => {
            eprintln!("brct: {}", e);
            1
        }
    }
}

fn remove(args: &[&str]) -> i32 {
    let Some(&name) = args.first() else {
        eprintln!("kullanım: sudo brct remove <ad>");
        return 2;
    };
    if process::uid() != 0 {
        eprintln!(
            "brct: kaldırma yönetici yetkisi ister: sudo brct remove {}",
            name
        );
        return 1;
    }
    if !installed().iter().any(|(n, _)| n == name) {
        eprintln!(
            "brct: {} paket olarak kurulu değil (sistem uygulamaları kaldırılamaz)",
            name
        );
        return 1;
    }
    run_program(&["hatch", "remove", name])
}

fn write_new(path: &str, text: &str) -> Result<()> {
    fs::write(path, text.as_bytes()).map_err(|e| format!("{}: {}", path, fs::error_text(e)))
}

/// Banna OS'ta derlenen proje: Banna.toml + src/main.rs.
fn new_banna_project(id: &str) -> i32 {
    let dir = fs::normalize(&process::cwd(), id);
    if fs::stat(&dir).is_ok() {
        eprintln!("brct: {} zaten var", dir);
        return 1;
    }
    let result = (|| -> Result<()> {
        mkdir_p(&format!("{}/src", dir));
        write_new(
            &format!("{}/Banna.toml", dir),
            &format!("[package]\nname = \"{id}\"\nversion = \"0.1.0\"\ndescription = \"Banna OS'ta yazılmış program\"\n"),
        )?;
        write_new(
            &format!("{}/src/main.rs", dir),
            &format!(
                "// {id}: Banna OS'ta yazılıp derlenen Rust programı.\n\
                 // Derle: brct build    Çalıştır: brct run    (ya da ./target/{id}.banna)\n\n\
                 use std::io;\n\n\
                 fn selamla(ad: &str) -> String {{\n    format!(\"Merhaba, {{}}!\", ad)\n}}\n\n\
                 fn main() {{\n    println!(\"Adın ne?\");\n    let mut ad = String::new();\n    \
                 io::stdin().read_line(&mut ad).unwrap();\n    let ad = ad.trim();\n    \
                 let ad = if ad.is_empty() {{ \"Banna\" }} else {{ ad }};\n    println!(\"{{}}\", selamla(ad));\n\n    \
                 let kareler: Vec<i64> = (1..=5).map(|x| x * x).collect();\n    \
                 println!(\"Kareler: {{:?}} (toplam {{}})\", kareler, kareler.iter().sum::<i64>());\n}}\n"
            ),
        )?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            println!("{} oluşturuldu.", dir);
            println!("  cd {}  &&  edit src/main.rs", id);
            println!("  brct run        derle ve çalıştır");
            println!("  brct build      target/{}.banna üretir", id);
            0
        }
        Err(e) => {
            eprintln!("brct: {}", e);
            1
        }
    }
}

/// Derlenecek kaynak ve çıktı: (kaynak, .banna yolu, ad).
fn locate(arg: Option<&str>) -> Result<(String, String, String)> {
    let cwd = process::cwd();
    let target = fs::normalize(&cwd, arg.unwrap_or("."));
    if target.ends_with(".rs") {
        if !is_file(&target) {
            return Err(format!("{} bulunamadı", target));
        }
        let stem = target.rsplit('/').next().unwrap_or("program").trim_end_matches(".rs");
        let dir = target.rsplit_once('/').map(|(d, _)| d).unwrap_or("/");
        let dir = if dir.is_empty() { "/" } else { dir };
        return Ok((target.clone(), format!("{}/{}.banna", dir.trim_end_matches('/'), stem), String::from(stem)));
    }
    let toml = format!("{}/Banna.toml", target.trim_end_matches('/'));
    let Some(text) = read_text(&toml) else {
        if is_file(&format!("{}/Cargo.toml", target)) {
            return Err(String::from(
                "bu bir SDK projesi (Cargo.toml): bilgisayarda `brct <proje>` ile derlenir. \
                 Banna OS'ta derlemek için `brct new <ad>` ile proje açın ya da `brct build dosya.rs`",
            ));
        }
        return Err(format!("{}: Banna.toml yok (brct new <ad> ya da brct build dosya.rs)", target));
    };
    let name = conf_value(&text, "name")
        .map(|n| String::from(n.trim_matches('"')))
        .unwrap_or_else(|| String::from(target.rsplit('/').next().unwrap_or("program")));
    let root = target.trim_end_matches('/');
    Ok((format!("{}/src/main.rs", root), format!("{}/target/{}.banna", root, name), name))
}

fn compile(arg: Option<&str>, write: bool) -> Result<(String, String)> {
    let (source, output, name) = locate(arg)?;
    let text = read_text(&source).ok_or_else(|| format!("{} okunamadı", source))?;
    let started = banna::time::uptime_ms();
    let bytes = banna::brs::build(&text).map_err(|e| format!("{}: {}", source.rsplit('/').next().unwrap_or(&source), e))?;
    if write {
        if let Some((dir, _)) = output.rsplit_once('/') {
            mkdir_p(dir);
        }
        fs::write(&output, &bytes).map_err(|e| format!("{}: {}", output, fs::error_text(e)))?;
        let _ = fs::chmod(&output, 0o755);
        println!(
            "{} derlendi → {} ({}, {} ms)",
            name,
            output,
            size_text(bytes.len() as u64),
            banna::time::uptime_ms() - started
        );
    } else {
        println!("{}: hata yok ({} ms)", name, banna::time::uptime_ms() - started);
    }
    Ok((output, name))
}

fn build_command(args: &[&str], write: bool) -> i32 {
    match compile(args.first().copied(), write) {
        Ok(_) => 0,
        Err(e) => {
            eprintln!("brct: {}", e);
            1
        }
    }
}

/// `brct run` proje/dosya için: derle ve çalıştır.
fn build_and_run(args: &[&str]) -> i32 {
    let split = args.iter().position(|a| *a == "--").unwrap_or(args.len());
    let (own, extra) = (&args[..split], args.get(split + 1..).unwrap_or(&[]));
    let (output, _) = match compile(own.first().copied(), true) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("brct: {}", e);
            return 1;
        }
    };
    let mut argv: Vec<&str> = alloc::vec!["banna-run", output.as_str()];
    argv.extend_from_slice(extra);
    match process::fork() {
        Ok(0) => {
            let code = process::exec("/bin/banna-run", &argv);
            eprintln!("brct: banna-run çalıştırılamadı: {}", fs::error_text(code));
            process::exit(127);
        }
        Ok(pid) => process::wait(pid) as i32,
        Err(e) => {
            eprintln!("brct: fork: {}", fs::error_text(e));
            1
        }
    }
}

fn new_project(args: &[&str]) -> i32 {
    if !args.contains(&"--sdk") {
        let Some(&id) = args.iter().find(|a| !a.starts_with("--")) else {
            eprintln!("kullanım: brct new <ad>");
            return 2;
        };
        if !valid_id(id) {
            eprintln!("brct: ad küçük harf, rakam ve '-' içermeli (ör. hesap-makinesi)");
            return 1;
        }
        return new_banna_project(id);
    }
    let cli = args.contains(&"--cli");
    let Some(&id) = args.iter().find(|a| !a.starts_with("--")) else {
        eprintln!("kullanım: brct new <ad> [--cli]");
        return 2;
    };
    if !valid_id(id) {
        eprintln!("brct: ad küçük harf, rakam ve '-' içermeli (ör. merhaba-banna)");
        return 1;
    }
    let dir = fs::normalize(&process::cwd(), id);
    if fs::stat(&dir).is_ok() {
        eprintln!("brct: {} zaten var", dir);
        return 1;
    }
    let result = (|| -> Result<()> {
        mkdir_p(&format!("{}/src", dir));
        write_new(
            &format!("{}/Cargo.toml", dir),
            &format!(
                "[package]\nname = \"{id}\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
                 description = \"Banna OS uygulaması\"\n\n\
                 # brct buildrken banna kütüphanesinin yolunu kendisi ayarlar.\n\
                 [dependencies]\nbanna = {{ package = \"banna-user\", path = \"../banna_os/user\" }}\n\n\
                 [profile.dev]\npanic = \"abort\"\n\n[profile.release]\npanic = \"abort\"\n\n[workspace]\n"
            ),
        )?;
        write_new(&format!("{}/.gitignore", dir), "/target\n")?;
        let main = if cli {
            format!(
                "//! {id}: Banna OS komut satırı programı.\n\n#![no_std]\n#![no_main]\n\n\
                 use banna::prelude::*;\n\nbanna::main!(main);\n\n\
                 fn main(args: &[&str]) -> i32 {{\n    let name = args.get(1).copied().unwrap_or(\"Banna\");\n    \
                 println!(\"Merhaba, {{}}!\", name);\n    0\n}}\n"
            )
        } else {
            format!(
                r#"//! {id}: Banna OS masaüstü uygulaması.

#![no_std]
#![no_main]

use banna::prelude::*;
use banna::icons;

banna::main!(main);

fn draw(window: &mut Window, theme: &Theme) {{
    let u = ui::unit();
    let f = ui::fonts();
    let (w, h) = (window.width as i32, window.height as i32);
    let mut c = window.canvas();
    c.fill(Rect::new(0, 0, w, h), theme.window_bg);
    let logo = Rect::new(24 * u, 24 * u, 48 * u, 48 * u);
    icons::logo(&mut c, logo, f.title, theme.accent, rgb(0xff, 0xff, 0xff));
    f.heading.draw(&mut c, logo.right() + 16 * u, 26 * u, "Merhaba Banna!", theme.text);
    f.body.draw(
        &mut c,
        24 * u,
        96 * u,
        "Bu uygulama brct ile Rust'ta yazıldı.",
        theme.muted,
    );
}}

fn main(_args: &[&str]) -> i32 {{
    let u = ui::unit() as usize;
    let mut window = match Window::new("{id}", 420 * u, 160 * u) {{
        Ok(w) => w,
        Err(e) => {{
            eprintln!("{id}: pencere açılamadı ({{}})", e);
            return 1;
        }}
    }};
    // Masaüstünün teması (koyu/açık, vurgu rengi).
    let theme = desktop::query()
        .map(|info| Theme::from(&Prefs::parse(&info.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    draw(&mut window, &theme);
    window.present();
    loop {{
        if let Some(event) = window.event(60_000) {{
            match event.kind {{
                EVENT_CLOSE => return 0,
                EVENT_KEY if event.key == 27 => return 0,
                EVENT_RESIZE => {{
                    window.resize(event.x as usize, event.y as usize);
                    draw(&mut window, &theme);
                    window.present();
                }}
                _ => {{}}
            }}
        }}
    }}
}}
"#
            )
        };
        write_new(&format!("{}/src/main.rs", dir), &main)?;
        if !cli {
            write_new(
                &format!("{}/app.conf", dir),
                &format!("name={id}\ndescription=Banna OS uygulaması\ntitle={id}\norder=50\n"),
            )?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            println!(
                "{} oluşturuldu ({}).",
                dir,
                if cli { "komut satırı" } else { "masaüstü" }
            );
            println!("Düzenle: edit {}/src/main.rs", id);
            println!(
                "Derle:   klasörü bilgisayarına kopyala ve 'brct {}' çalıştır (brct build)",
                id
            );
            0
        }
        Err(e) => {
            eprintln!("brct: {}", e);
            1
        }
    }
}

fn sdk_build() -> i32 {
    println!("SDK projeleri (Cargo.toml, tam Rust + banna kitaplığı) bilgisayarda derlenir:");
    println!();
    println!("  1. banna_os klasöründe:      make brct-install");
    println!("  2. projeyi derle:            brct <proje> --release");
    println!("  3. USB belleğe kopyala:      <proje>/target/banna/<ad>.app  (ya da brct package)");
    println!("  4. Banna OS'ta kur:          sudo brct install /usb0/<ad>.app");
    println!();
    println!("Depoya koymak için: brct package <proje> && make repo, ardından hatch install <ad>.");
    0
}

fn doctor() -> i32 {
    let mut problems = 0;
    let mut check = |ok: bool, what: &str, hint: &str| {
        println!("  [{}] {}", if ok { " OK " } else { "HATA" }, what);
        if !ok {
            println!("         {}", hint);
            problems += 1;
        }
    };
    check(
        is_dir("/apps"),
        "/apps uygulama klasörü",
        "sistem kurulumu eksik",
    );
    check(
        is_file("/bin/hatch"),
        "hatch paket yöneticisi",
        "brct install yine çalışır, depo kurulumu çalışmaz",
    );
    let source = read_text("/etc/hatch/sources").unwrap_or_default();
    let source = source
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty() && !l.starts_with('#'))
        .unwrap_or("https://ecosys.trybanna.com/hatch/ (varsayılan)");
    check(true, &format!("depo: {}", source), "");
    let net = banna::net::netinfo();
    check(
        net.is_some(),
        "ağ bağlantısı",
        "hatch install için ağ gerekir; USB'den kurulum için gerekmez",
    );
    let brct_apps = fs::read_dir("/apps")
        .map(|list| {
            list.iter()
                .filter(|(id, _)| {
                    read_text(&format!("/apps/{}/app.conf", id))
                        .is_some_and(|c| conf_value(&c, "builder").as_deref() == Some("brct"))
                })
                .count()
        })
        .unwrap_or(0);
    check(
        true,
        &format!("brct ile yapılmış uygulama: {}", brct_apps),
        "",
    );
    check(
        true,
        if process::uid() == 0 {
            "yönetici yetkisi var"
        } else {
            "kurma/kaldırma için sudo gerekir"
        },
        "",
    );
    if problems == 0 {
        println!("Sorun yok.");
    }
    (problems > 0) as i32
}

pub fn run(args: &[&str]) -> i32 {
    let rest = args.get(1..).unwrap_or(&[]);
    match args.first().copied() {
        None | Some("help" | "-h" | "--help") => help(),
        Some("list" | "ls") => list(rest.contains(&"--all")),
        Some("run") => {
            let first = rest.first().copied();
            let project = match first {
                None | Some("--") => true,
                Some(a) => a.ends_with(".rs") || is_file(&format!("{}/Banna.toml", fs::normalize(&process::cwd(), a))),
            };
            if project { build_and_run(rest) } else { run_program(rest) }
        }
        Some("check") => build_command(rest, false),
        Some("info") => info(rest),
        Some("install") => install(rest),
        Some("remove") => remove(rest),
        Some("new") => new_project(rest),
        Some("build") if !is_file(&format!("{}/Cargo.toml", process::cwd())) || rest.first().is_some_and(|a| a.ends_with(".rs")) => build_command(rest, true),
        Some("build" | "package") => sdk_build(),
        Some("doctor") => doctor(),
        Some("version" | "--version") => {
            println!("brct (Banna OS) {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(other) => {
            eprintln!("brct: bilinmeyen komut '{}' (brct help)", other);
            2
        }
    }
}
