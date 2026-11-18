//! brct — Banna Rust Compatibility Tool.
//!
//! Ana makinede (macOS/Linux) yazılan Rust uygulamalarını Banna OS için derler:
//!
//!   brct new <ad> [--cli]            yeni Banna OS uygulaması oluşturur
//!   brct <yol> [--release]           derler (cargo build yerine)
//!   brct --path <yol> [--release]    aynısı
//!   brct package <yol> [--out <dizin>]  Banna Store / hatch paketi hazırlar
//!   brct doctor                      araç zincirini denetler
//!
//! Derleme Banna OS hedefiyle (x86_64-banna_user, no_std + alloc, `banna` kütüphanesi)
//! yapılır. Çıktı: `<yol>/target/banna/<profil>/<ad>` ve masaüstü uygulamaları için
//! `<yol>/target/banna/<ad>.app/` (app.conf + program, /apps/<ad>/ altına kopyalanmaya hazır).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

fn die(msg: impl AsRef<str>) -> ! {
    eprintln!("brct: {}", msg.as_ref());
    exit(1)
}

/// Banna OS kaynak ağacı (SDK): BANNA_SDK ortam değişkeni ya da derlenirkenki konum.
fn sdk() -> PathBuf {
    let path = env::var("BANNA_SDK")
        .ok()
        .or_else(|| option_env!("BANNA_SDK").map(String::from))
        .unwrap_or_else(|| die("BANNA_SDK ayarlı değil (banna_os kaynak dizini)"));
    let path = PathBuf::from(path);
    if !path.join("user/x86_64-banna_user.json").is_file() {
        die(format!(
            "{} bir Banna OS kaynak ağacı değil",
            path.display()
        ));
    }
    path
}

fn usage() -> ! {
    println!("brct — Banna Rust Compatibility Tool (Banna OS için Rust derleyicisi)");
    println!();
    println!(
        "  brct new <ad> [--cli]              yeni uygulama (varsayılan: masaüstü uygulaması)"
    );
    println!("  brct <yol> [--release]             Banna OS için derle (cargo build yerine)");
    println!("  brct --path <yol> [--release]      aynısı");
    println!("  brct package <yol> [--out <dizin>] Banna Store / hatch paketi hazırla");
    println!("  brct doctor                        araç zincirini denetle");
    println!();
    println!("Uygulamalar no_std'dir; giriş noktası banna::main!(main), API'ler banna::prelude.");
    exit(0)
}

/// Cargo.toml'dan [package] alanı (basit okuma).
fn manifest_field(project: &Path, key: &str) -> Option<String> {
    let text = fs::read_to_string(project.join("Cargo.toml")).ok()?;
    let mut in_package = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_package = line == "[package]";
            continue;
        }
        if in_package
            && let Some((k, v)) = line.split_once('=')
            && k.trim() == key
        {
            return Some(v.trim().trim_matches('"').to_string());
        }
    }
    None
}

fn toolchain(sdk: &Path) -> String {
    fs::read_to_string(sdk.join("rust-toolchain.toml"))
        .ok()
        .and_then(|t| {
            t.lines().find_map(|l| {
                l.trim().strip_prefix("channel").map(|v| {
                    v.trim_start_matches([' ', '='])
                        .trim()
                        .trim_matches('"')
                        .to_string()
                })
            })
        })
        .unwrap_or_else(|| String::from("nightly"))
}

/// Banna OS'ta `brct new` ile açılan projeler banna kütüphanesini `../banna_os/user`
/// olarak gösterir; bu yol bu bilgisayarda yoksa SDK'daki kütüphaneye çevrilir.
fn fix_banna_path(project: &Path, sdk: &Path) {
    let manifest = project.join("Cargo.toml");
    let Ok(text) = fs::read_to_string(&manifest) else {
        return;
    };
    let user = sdk.join("user");
    let mut changed = false;
    let lines: Vec<String> = text
        .lines()
        .map(|line| {
            let Some(at) = line.find("path = \"") else {
                return line.to_string();
            };
            if !line.contains("banna-user") {
                return line.to_string();
            }
            let rest = &line[at + 8..];
            let Some(end) = rest.find('"') else {
                return line.to_string();
            };
            let current = &rest[..end];
            if project.join(current).join("Cargo.toml").is_file() {
                return line.to_string();
            }
            changed = true;
            format!("{}{}{}", &line[..at + 8], user.display(), &rest[end..])
        })
        .collect();
    if changed {
        fs::write(&manifest, lines.join("\n") + "\n").unwrap();
        println!(
            "brct: banna kütüphanesi yolu {} olarak ayarlandı",
            user.display()
        );
    }
}

fn build(project: &Path, release: bool) -> PathBuf {
    let sdk = sdk();
    let project = project
        .canonicalize()
        .unwrap_or_else(|_| die(format!("{} bulunamadı", project.display())));
    let name = manifest_field(&project, "name").unwrap_or_else(|| {
        die(format!(
            "{}/Cargo.toml bulunamadı ya da adı yok",
            project.display()
        ))
    });
    fix_banna_path(&project, &sdk);
    let profile = if release { "release" } else { "debug" };
    let target_dir = project.join("target/brct");
    let user = sdk.join("user");
    let rustflags = format!(
        "-C link-arg=-T{} -C relocation-model=static",
        user.join("user.ld").display()
    );
    println!("brct: {} derleniyor (Banna OS, {})", name, profile);
    let mut cmd = Command::new("rustup");
    cmd.current_dir(&project)
        .args(["run", &toolchain(&sdk), "cargo", "build"])
        .arg("--manifest-path")
        .arg(project.join("Cargo.toml"))
        .arg("--target")
        .arg(user.join("x86_64-banna_user.json"))
        .arg("--target-dir")
        .arg(&target_dir)
        .args([
            "-Z",
            "build-std=core,compiler_builtins,alloc",
            "-Z",
            "build-std-features=compiler-builtins-mem",
            "-Z",
            "json-target-spec",
            "--config",
            "profile.dev.panic=\"abort\"",
            "--config",
            "profile.dev.opt-level=2",
            "--config",
            "profile.release.panic=\"abort\"",
            "--config",
            "profile.release.opt-level=3",
            "--config",
            "profile.release.lto=true",
            "--config",
            "profile.release.codegen-units=1",
        ])
        .env("RUSTFLAGS", rustflags)
        .env_remove("CARGO_TARGET_DIR");
    if release {
        cmd.arg("--release");
    }
    let status = cmd
        .status()
        .unwrap_or_else(|e| die(format!("rustup çalıştırılamadı ({}); Rust kurulu mu?", e)));
    if !status.success() {
        die("derleme başarısız");
    }
    let built = target_dir
        .join("x86_64-banna_user")
        .join(profile)
        .join(&name);
    let bytes = fs::read(&built).unwrap_or_else(|_| die(format!("{} oluşmadı", built.display())));
    if bytes.get(..4) != Some(b"\x7fELF") || bytes.get(18) != Some(&0x3e) {
        die("çıktı x86_64 ELF değil");
    }
    let out_dir = project.join("target/banna").join(profile);
    fs::create_dir_all(&out_dir).unwrap();
    let out = out_dir.join(&name);
    fs::copy(&built, &out).unwrap();

    // Masaüstü uygulaması: app.conf varsa /apps/<ad>/ için paket klasörü.
    let conf = project.join("app.conf");
    if conf.is_file() {
        let bundle = project.join("target/banna").join(format!("{}.app", name));
        fs::create_dir_all(&bundle).unwrap();
        install_app_files(&project, &bundle);
        fs::copy(&built, bundle.join(&name)).unwrap();
        println!(
            "brct: uygulama paketi {} (→ /apps/{}/)",
            bundle.display(),
            name
        );
    }
    println!("brct: {} ({} KiB)", out.display(), bytes.len() / 1024);
    out
}

/// app.conf'u `builder=brct` işaretiyle ve proje kökündeki PNG'leri (simgeler) kopyalar.
fn install_app_files(project: &Path, dest: &Path) {
    let conf = fs::read_to_string(project.join("app.conf")).unwrap();
    let mut out: String = conf
        .lines()
        .filter(|l| l.split('=').next().map(str::trim) != Some("builder"))
        .map(|l| format!("{}\n", l))
        .collect();
    out.push_str("builder=brct\n");
    fs::write(dest.join("app.conf"), out).unwrap();
    for entry in fs::read_dir(project).unwrap().flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "png") {
            fs::copy(&path, dest.join(entry.file_name())).unwrap();
        }
    }
}

fn package(project: &Path, out: Option<PathBuf>) {
    let binary = build(project, true);
    let project = project.canonicalize().unwrap();
    let name = manifest_field(&project, "name").unwrap();
    let version = manifest_field(&project, "version").unwrap_or_else(|| String::from("0.1.0"));
    let description = manifest_field(&project, "description").unwrap_or_default();
    let out = out.unwrap_or_else(|| sdk().join("packages"));
    let dir = out.join(&name);
    if dir.exists() {
        fs::remove_dir_all(&dir).unwrap();
    }
    let conf = project.join("app.conf");
    let files = if conf.is_file() {
        let app = dir.join("files/apps").join(&name);
        fs::create_dir_all(&app).unwrap();
        install_app_files(&project, &app);
        fs::copy(&binary, app.join(&name)).unwrap();
        format!("/apps/{}/", name)
    } else {
        let bin = dir.join("files/bin");
        fs::create_dir_all(&bin).unwrap();
        fs::copy(&binary, bin.join(&name)).unwrap();
        format!("/bin/{}", name)
    };
    fs::write(
        dir.join("package.ini"),
        format!(
            "# brct package ile üretildi\nversion = {}\ndescription = {}\nbuilder = brct\n",
            version, description
        ),
    )
    .unwrap();
    println!("brct: paket {} ({} kurulur)", dir.display(), files);
    println!(
        "brct: depoyu yenilemek için: make repo  (ekosistem: banna-repo/scripts/build-hatch.sh)"
    );
}

fn new_project(name: &str, cli: bool) {
    let valid = !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.starts_with('-');
    let dir = PathBuf::from(name);
    let id = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(name)
        .to_string();
    if !valid
        && !id
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        die("ad küçük harf, rakam ve '-' içermeli (ör. merhaba-banna)");
    }
    if dir.exists() {
        die(format!("{} zaten var", dir.display()));
    }
    let sdk = sdk();
    fs::create_dir_all(dir.join("src")).unwrap();
    fs::write(
        dir.join("Cargo.toml"),
        format!(
            r#"[package]
name = "{id}"
version = "0.1.0"
edition = "2024"
description = "Banna OS uygulaması"

# Banna OS kütüphanesi: pencere, çizim, arayüz öğeleri, dosya, ağ (https dahil), JSON.
[dependencies]
banna = {{ package = "banna-user", path = "{sdk}/user" }}

[profile.dev]
panic = "abort"

[profile.release]
panic = "abort"

# Bağımsız proje (üst dizindeki bir çalışma alanına katılmaz).
[workspace]
"#,
            id = id,
            sdk = sdk.join("").display().to_string().trim_end_matches('/'),
        ),
    )
    .unwrap();
    fs::write(dir.join(".gitignore"), "/target\n").unwrap();
    let main = if cli {
        format!(
            r#"//! {id}: Banna OS komut satırı programı.

#![no_std]
#![no_main]

use banna::prelude::*;

banna::main!(main);

fn main(args: &[&str]) -> i32 {{
    let name = args.get(1).copied().unwrap_or("Banna");
    println!("Merhaba, {{}}!", name);
    0
}}
"#
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
    fs::write(dir.join("src/main.rs"), main).unwrap();
    if !cli {
        fs::write(
            dir.join("app.conf"),
            format!(
                "# Banna OS uygulama tanımı (bkz. banna_os/apps/README.md)\nname={id}\ndescription=Banna OS uygulaması\ntitle={id}\norder=50\n"
            ),
        )
        .unwrap();
    }
    println!("brct: {} oluşturuldu", dir.display());
    println!("  cd {} && brct . --release", dir.display());
}

fn doctor() {
    let sdk = sdk();
    println!("SDK: {}", sdk.display());
    let tc = toolchain(&sdk);
    let ok = |label: &str, cmd: &mut Command| {
        let good = cmd.output().map(|o| o.status.success()).unwrap_or(false);
        println!("  [{}] {}", if good { "tamam" } else { "YOK" }, label);
        good
    };
    let mut all = ok("rustup", Command::new("rustup").arg("--version"));
    all &= ok(
        &format!("{} araç zinciri", tc),
        Command::new("rustup").args(["run", &tc, "rustc", "--version"]),
    );
    let sysroot = Command::new("rustup")
        .args(["run", &tc, "rustc", "--print", "sysroot"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    let src = Path::new(&sysroot).join("lib/rustlib/src/rust/library/core");
    println!(
        "  [{}] rust-src bileşeni",
        if src.is_dir() { "tamam" } else { "YOK" }
    );
    all &= src.is_dir();
    if !all {
        println!(
            "kurulum: rustup toolchain install {} --component rust-src",
            tc
        );
        exit(1);
    }
}

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    let release = args.iter().any(|a| a == "--release" || a == "-r");
    let positional: Vec<&String> = args.iter().filter(|a| !a.starts_with('-')).collect();
    let flag_value = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(PathBuf::from)
    };
    match positional.first().map(|s| s.as_str()) {
        None if flag_value("--path").is_some() => {
            build(&flag_value("--path").unwrap(), release);
        }
        None => usage(),
        Some("help") => usage(),
        Some("doctor") => doctor(),
        Some("new") => {
            let name = positional
                .get(1)
                .unwrap_or_else(|| die("kullanım: brct new <ad> [--cli]"));
            new_project(name, args.iter().any(|a| a == "--cli"));
        }
        Some("package") => {
            let path = positional
                .get(1)
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("."));
            package(&path, flag_value("--out"));
        }
        Some("build") => {
            let path = positional
                .get(1)
                .map(PathBuf::from)
                .or_else(|| flag_value("--path"));
            build(&path.unwrap_or_else(|| PathBuf::from(".")), release);
        }
        Some(path) => {
            build(Path::new(path), release);
        }
    }
}
