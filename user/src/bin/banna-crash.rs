//! banna-crash: Banna Hata Raporlama Sistemi'nin kaydettiği raporları (`/boot/boot/crash`)
//! oturumdaki Banna hesabına yükler; Banna Ekosistemi ana onay cihazına bildirim gönderir.
//!
//!   banna-crash upload   gönderilmemiş raporları yükle (oturum açılınca çalışır)
//!   banna-crash list     raporları ve gönderim durumlarını göster

#![no_std]
#![no_main]

use banna::ecosystem::Client;
use banna::json;
use banna::{String, Vec, format, fs, println, process, time};

banna::entry!(main);

const DIR: &str = "/boot/boot/crash";
/// Ekosisteme gönderilecek günlüğün en büyük boyutu (sunucu sınırının altında).
const MAX_LOG: usize = 200 * 1024;

fn sent_path() -> String {
    format!("/home/{}/.config/banna/crash-sent", process::username())
}

fn sent() -> Vec<String> {
    fs::read(&sent_path())
        .map(|d| {
            String::from_utf8_lossy(&d)
                .lines()
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn mark_sent(name: &str) {
    let mut list = sent();
    list.push(String::from(name));
    let _ = fs::write(&sent_path(), (list.join("\n") + "\n").as_bytes());
}

fn reports() -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(DIR)
        .unwrap_or_default()
        .into_iter()
        .map(|(name, _)| name)
        .filter(|n| n.starts_with("crash-") && n.ends_with(".log"))
        .collect();
    names.sort();
    names
}

/// Rapor dosyasından (başlık alanları, günlük) JSON gövdesi.
fn body(name: &str, text: &str) -> String {
    let header = text.split("--- details ---").next().unwrap_or("");
    let field = |key: &str| {
        header
            .lines()
            .find_map(|l| l.strip_prefix(key).and_then(|v| v.strip_prefix(": ")))
            .unwrap_or("")
    };
    // Çok uzunsa baştaki başlık ve sondaki günlük korunur.
    let log = if text.len() > MAX_LOG {
        let mut cut = text.len() - MAX_LOG;
        while !text.is_char_boundary(cut) {
            cut += 1;
        }
        format!("{}\n[... kısaltıldı ...]\n{}", header, &text[cut..])
    } else {
        String::from(text)
    };
    format!(
        "{{\"fileName\":{},\"kind\":{},\"title\":{},\"version\":{},\"occurredAt\":{},\"uptimeMs\":{},\"cpu\":{},\"log\":{}}}",
        json::quote(name),
        json::quote(field("kind")),
        json::quote(field("title")),
        json::quote(field("version")),
        json::quote(field("time")),
        field("uptime_ms").parse::<u64>().unwrap_or(0),
        json::quote(field("cpu")),
        json::quote(&log)
    )
}

fn upload() -> i32 {
    let pending: Vec<String> = {
        let done = sent();
        reports()
            .into_iter()
            .filter(|n| !done.contains(n))
            .collect()
    };
    if pending.is_empty() {
        return 0;
    }
    let mut client = Client::load();
    if !client.has_session() {
        println!(
            "banna-crash: {} rapor var; Banna hesabına giriş yapılmadığı için gönderilmedi",
            pending.len()
        );
        return 0;
    }
    for name in pending {
        let Ok(data) = fs::read(&format!("{}/{}", DIR, name)) else {
            continue;
        };
        let text = String::from_utf8_lossy(&data);
        // Açılışta ağ henüz hazır olmayabilir: birkaç kez denenir.
        let mut attempt = 0;
        loop {
            match client.post("/banna-os/crash-reports", &body(&name, &text)) {
                Ok(_) => {
                    println!("banna-crash: {} gönderildi", name);
                    mark_sent(&name);
                    break;
                }
                Err(e) if e.status == 0 && attempt < 6 => {
                    attempt += 1;
                    time::sleep_ms(10_000);
                }
                Err(e) => {
                    println!("banna-crash: {} gönderilemedi: {}", name, e.message);
                    if e.status == 409 || e.status == 400 {
                        mark_sent(&name);
                    }
                    break;
                }
            }
        }
    }
    0
}

fn list() -> i32 {
    let done = sent();
    let names = reports();
    if names.is_empty() {
        println!("Kayıtlı hata raporu yok.");
    }
    for name in names {
        let state = if done.contains(&name) {
            "gönderildi"
        } else {
            "bekliyor"
        };
        println!("  {}  {}", name, state);
    }
    0
}

fn main(args: &[&str]) -> i32 {
    match args.get(1).copied() {
        None | Some("upload") => upload(),
        Some("list") => list(),
        _ => {
            println!("usage: banna-crash [upload|list]");
            2
        }
    }
}
