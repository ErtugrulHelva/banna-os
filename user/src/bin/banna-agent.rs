//! banna-agent: Banna OS cihazını Banna Ekosistemi paneline bağlayan servis.
//!
//! Linux'taki banna-agent ile aynı API: `/etc/banna-agent/config` içindeki cihaz belirteciyle
//! (Bearer) sistem bilgisini ve hatch kurulum geçmişini `/banna-os/report`'a bildirir,
//! panelden istenen uzaktan kurulumları (`/banna-os/queue`) hatch ile uygular.
//!
//!   banna-agent pair <belirteç>   eşleştir (yönetici; Banna Panel çağırır)
//!   banna-agent unpair            eşleştirmeyi kaldır
//!   banna-agent status            durum
//!   banna-agent run               servis döngüsü (açılışta root olarak başlar)
//!   banna-agent once              tek bildirim + kuyruk turu

#![no_std]
#![no_main]

use banna::json::{self, Value};
use banna::{String, Vec, format, fs, net, println, process, time};

banna::entry!(main);

const CONFIG_DIR: &str = "/etc/banna-agent";
const CONFIG: &str = "/etc/banna-agent/config";
const STATE_DIR: &str = "/var/lib/banna-agent";
const REPORTED: &str = "/var/lib/banna-agent/reported";
const INSTALLED: &str = "/var/hatch/installed";
const DEFAULT_BASE: &str = "https://ecosys.trybanna.com/api";
const PERIOD_MS: u64 = 5 * 60 * 1000;

struct Config {
    token: String,
    base: String,
}

fn read_text(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn load_config() -> Option<Config> {
    let text = read_text(CONFIG)?;
    let mut token = String::new();
    let mut base = String::from(DEFAULT_BASE);
    for line in text.lines() {
        if let Some((k, v)) = line.trim().split_once('=') {
            match k.trim() {
                "BANNA_AGENT_TOKEN" => token = String::from(v.trim()),
                "BANNA_API_BASE" => base = String::from(v.trim().trim_end_matches('/')),
                _ => {}
            }
        }
    }
    (!token.is_empty()).then_some(Config { token, base })
}

fn call(cfg: &Config, method: &str, path: &str, body: Option<&str>) -> Result<Value, String> {
    let auth = format!("Bearer {}", cfg.token);
    let mut headers: Vec<(&str, &str)> = banna::vec![
        ("Authorization", auth.as_str()),
        ("Accept", "application/json")
    ];
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    let r = net::request(&net::Request {
        method,
        url: &format!("{}{}", cfg.base, path),
        headers: &headers,
        body: body.unwrap_or("").as_bytes(),
    })?;
    if r.status >= 400 {
        return Err(format!(
            "HTTP {} {}",
            r.status,
            String::from_utf8_lossy(&r.body)
        ));
    }
    Ok(json::parse(&r.body).unwrap_or(Value::Null))
}

fn installed() -> Vec<(String, String)> {
    read_text(INSTALLED)
        .map(|t| {
            t.lines()
                .filter_map(|l| l.trim().split_once(' '))
                .map(|(n, v)| (String::from(n), String::from(v)))
                .collect()
        })
        .unwrap_or_default()
}

fn report(cfg: &Config) -> Result<(), String> {
    let info = process::sysinfo();
    let ip = net::netinfo()
        .map(|n| format!("{}", n.address))
        .unwrap_or_default();
    let disk = fs::statfs("/")
        .map(|(total, free)| format!("{} / {} MB", (total - free) >> 20, total >> 20))
        .unwrap_or_default();
    let mem = format!(
        "{} / {} MB",
        (info.total_memory - info.free_memory) >> 20,
        info.total_memory >> 20
    );
    let hostname = read_text("/etc/hostname")
        .map(|h| String::from(h.trim()))
        .unwrap_or_else(|| String::from("banna"));
    // Son bildirimden beri kurulan ya da güncellenen paketler.
    let reported = read_text(REPORTED).unwrap_or_default();
    let now = installed();
    let fresh: Vec<&(String, String)> = now
        .iter()
        .filter(|(n, v)| !reported.lines().any(|l| l.trim() == format!("{} {}", n, v)))
        .collect();
    let packages: Vec<String> = fresh
        .iter()
        .map(|(n, v)| {
            format!(
                "{{\"packageName\":{},\"version\":{},\"status\":\"SUCCESS\"}}",
                json::quote(n),
                json::quote(v)
            )
        })
        .collect();
    let body = format!(
        "{{\"osName\":\"BannaOS\",\"osVersion\":{},\"kernelVersion\":{},\"architecture\":\"x86_64\",\"hostname\":{},\"localIp\":{},\"diskUsage\":{},\"memUsage\":{},\"packages\":[{}]}}",
        json::quote(env!("CARGO_PKG_VERSION")),
        json::quote(&format!("banna {}", env!("CARGO_PKG_VERSION"))),
        json::quote(&hostname),
        json::quote(&ip),
        json::quote(&disk),
        json::quote(&mem),
        packages.join(",")
    );
    call(cfg, "POST", "/banna-os/report", Some(&body))?;
    let _ = fs::mkdir("/var/lib");
    let _ = fs::mkdir(STATE_DIR);
    let text: String = now.iter().map(|(n, v)| format!("{} {}\n", n, v)).collect();
    let _ = fs::write(REPORTED, text.as_bytes());
    Ok(())
}

/// `/bin/hatch` alt komutunu çalıştırır; çıkış kodu.
fn hatch(args: &[&str]) -> i64 {
    let mut full = banna::vec!["hatch"];
    full.extend_from_slice(args);
    match process::spawn("/bin/hatch", &full) {
        Ok(pid) => process::wait(pid),
        Err(e) => e,
    }
}

fn run_queue(cfg: &Config) -> Result<usize, String> {
    let queue = call(cfg, "GET", "/banna-os/queue", None)?;
    let items = queue.items();
    if items.is_empty() {
        return Ok(0);
    }
    hatch(&["new"]);
    for item in items {
        let id = item.get("id").as_str();
        let name = item.get("packageName").as_str();
        // hatch paket adları: küçük harf, rakam, - . +
        let valid = !name.is_empty()
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || "-.+".contains(c));
        let code = if valid { hatch(&["install", name]) } else { 2 };
        let body = if code == 0 {
            String::from("{\"status\":\"SUCCESS\"}")
        } else {
            format!(
                "{{\"status\":\"FAILED\",\"errorMessage\":{}}}",
                json::quote(&format!(
                    "hatch install {} başarısız (kod {}); paket Banna OS deposunda olmayabilir",
                    name, code
                ))
            )
        };
        println!(
            "banna-agent: {} kurulumu {}",
            name,
            if code == 0 { "tamam" } else { "başarısız" }
        );
        call(
            cfg,
            "POST",
            &format!("/banna-os/queue/{}/result", id),
            Some(&body),
        )?;
    }
    Ok(items.len())
}

fn once(cfg: &Config) -> bool {
    match report(cfg).and_then(|_| run_queue(cfg)) {
        Ok(n) => {
            println!(
                "banna-agent: bildirildi{}",
                if n > 0 {
                    format!(", {} uzaktan kurulum", n)
                } else {
                    String::new()
                }
            );
            true
        }
        Err(e) => {
            println!("banna-agent: {}", e);
            false
        }
    }
}

fn main(args: &[&str]) -> i32 {
    match args.get(1).copied() {
        Some("pair") => {
            let Some(token) = args.get(2) else {
                println!("kullanım: banna-agent pair <belirteç>");
                return 1;
            };
            if process::uid() != 0 {
                println!("banna-agent: yönetici yetkisi gerekir (sudo banna-agent pair ...)");
                return 1;
            }
            let _ = fs::mkdir(CONFIG_DIR);
            let text = format!(
                "BANNA_AGENT_TOKEN={}\nBANNA_API_BASE={}\n",
                token, DEFAULT_BASE
            );
            if fs::write(CONFIG, text.as_bytes()).is_err() {
                println!("banna-agent: {} yazılamadı", CONFIG);
                return 1;
            }
            let _ = fs::chmod(CONFIG, 0o600);
            println!("banna-agent: cihaz eşleştirildi");
            // İlk bildirim hemen; servis bir sonraki açılışta kendiliğinden başlar.
            let cfg = load_config().unwrap();
            if once(&cfg) { 0 } else { 2 }
        }
        Some("unpair") => {
            if process::uid() != 0 {
                println!("banna-agent: yönetici yetkisi gerekir");
                return 1;
            }
            let _ = fs::remove(CONFIG);
            println!("banna-agent: eşleştirme kaldırıldı");
            0
        }
        Some("status") => {
            match load_config() {
                Some(cfg) => println!("banna-agent: eşleştirilmiş ({})", cfg.base),
                None => println!("banna-agent: eşleştirilmemiş (Banna Panel → Banna OS)"),
            }
            0
        }
        Some("once") => match load_config() {
            Some(cfg) => {
                if once(&cfg) {
                    0
                } else {
                    2
                }
            }
            None => 1,
        },
        Some("run") => {
            let Some(cfg) = load_config() else {
                println!("banna-agent: yapılandırma yok, servis durdu");
                return 1;
            };
            // Ağ gelene kadar kısa aralıklarla dene, sonra 5 dakikada bir.
            time::sleep_ms(5000);
            loop {
                let ok = once(&cfg);
                time::sleep_ms(if ok { PERIOD_MS } else { 60_000 });
            }
        }
        _ => {
            println!("banna-agent pair <belirteç> | unpair | status | once | run");
            1
        }
    }
}
