//! Banna OS sistem ve sürücü güncellemeleri (çekirdek, UEFI yükleyicisi, programlar,
//! uygulamalar, sürücü yazılımları). `hatch update` sistem veritabanını (imzalı bildirim)
//! indirir, `hatch upgrade` değişen dosyaları hazırlar; çekirdek bir sonraki açılışta
//! oturum başlamadan `banna-update apply` ile uygular.
//!
//! Depo: hatch kaynağının `system/` klasörü. `manifest` (imzalı, `manifest.sig`):
//!   banna-system 1
//!   version <sürüm>
//!   notes <açıklama>          (yayının adı; tek satır)
//!   date <YYYY-AA-GG>
//!   note <madde>              (sürüm notları, CHANGELOG.md'nin en üstteki bölümü)
//!   driver <ad> <özet>
//!   file <sha256> <boyut> <yol>
//! Dosyalar `system/files/<yol>` adresindedir. Eski çekirdek `/boot/boot/banna_os.old`
//! olarak kalır (önyükleme menüsündeki "Önceki sürüm" ve otomatik geri dönüş için).

use crate::crypto::{sha256, to_hex};
use crate::{String, Vec, format, fs, net, println};

pub const VERSION_FILE: &str = "/etc/banna/version";
pub const DRIVERS_FILE: &str = "/etc/banna/drivers";
pub const KERNEL: &str = "/boot/boot/banna_os";
pub const PREVIOUS_KERNEL: &str = "/boot/boot/banna_os.old";
const SOURCES: &str = "/etc/hatch/sources";
/// `hatch update` ile alınan son bildirim (sistem veritabanı).
const CACHE: &str = "/var/hatch/system/manifest";
/// Kurulu sistemin bildirimi: değişmeyen dosyalar yeniden özetlenmez.
const INSTALLED: &str = "/var/hatch/system/installed";
const STAGE: &str = "/var/hatch/system/stage";
/// Varsa çekirdek açılışta uygular; uygulama başlarken `APPLYING` olur (döngü olmaz).
pub const PENDING: &str = "/var/hatch/system/pending";
const APPLYING: &str = "/var/hatch/system/applying";
const LAST_ERROR: &str = "/var/hatch/system/last-error";

/// `banna-update apply` çıkış kodu: önyükleme dosyaları değişti, yeniden başlatılmalı.
pub const EXIT_REBOOT: i32 = 3;

pub type Result<T> = core::result::Result<T, String>;

pub struct Entry {
    pub sha256: String,
    pub size: usize,
    pub path: String,
}

pub struct Manifest {
    pub version: String,
    pub notes: String,
    pub date: String,
    /// Sürüm notları (madde madde).
    pub changes: Vec<String>,
    pub drivers: Vec<(String, String)>,
    pub files: Vec<Entry>,
}

fn read_text(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|d| String::from(String::from_utf8_lossy(&d).trim()))
}

fn mkdir_p(path: &str) {
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        let _ = fs::mkdir(&at);
    }
}

fn write(path: &str, data: &[u8]) -> Result<()> {
    if let Some((dir, _)) = path.rsplit_once('/') {
        mkdir_p(dir);
    }
    fs::write(path, data).map_err(|e| format!("{}: {}", path, fs::error_text(e)))
}

fn remove_tree(path: &str) {
    if let Ok(entries) = fs::read_dir(path) {
        for (name, stat) in entries {
            if name == "." || name == ".." {
                continue;
            }
            let child = format!("{}/{}", path, name);
            if stat.kind == fs::KIND_DIRECTORY {
                remove_tree(&child);
            } else {
                let _ = fs::remove(&child);
            }
        }
    }
    let _ = fs::remove(path);
}

pub fn current_version() -> String {
    read_text(VERSION_FILE).unwrap_or_else(|| String::from("bilinmiyor"))
}

/// Hatch deposunun kök adresi (`@gateway` DHCP ağ geçidiyle değiştirilir).
pub fn repo_base() -> Result<String> {
    let mut url = read_text(SOURCES)
        .and_then(|t| {
            t.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
        })
        .unwrap_or_else(|| String::from(crate::ecosystem::HATCH_REPO));
    if url.contains("@gateway") {
        let info = net::netinfo().ok_or_else(|| String::from("ağ bağlantısı yok"))?;
        url = url.replace("@gateway", &format!("{}", info.gateway));
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    Ok(url)
}

/// İndirme süresi ve hızı (yavaş bağlantıları teşhis için).
pub fn report_speed(bytes: usize, started_ms: u64) {
    let ms = crate::time::uptime_ms().saturating_sub(started_ms).max(1);
    println!(
        "  {} KiB, {}.{:02} s, {} KiB/s",
        bytes.div_ceil(1024),
        ms / 1000,
        ms % 1000 / 10,
        bytes as u64 * 1000 / 1024 / ms
    );
}

fn download(url: &str) -> Result<Vec<u8>> {
    let response = net::get(url)?;
    if response.status != 200 {
        return Err(format!("{}: HTTP {}", url, response.status));
    }
    Ok(response.body)
}

/// Güncellenebilir yollar: çekirdek ve yükleyici (EFI bölümü), programlar, uygulamalar,
/// paylaşılan veriler ve sürücü yazılımları.
fn allowed(path: &str) -> bool {
    !path.contains("..")
        && (path == KERNEL
            || path == "/boot/EFI/BOOT/BOOTX64.EFI"
            || path.starts_with("/bin/")
            || path.starts_with("/apps/")
            || path.starts_with("/usr/share/")
            || path.starts_with("/lib/firmware/")
            || path.starts_with("/lib/modules/"))
}

fn parse(text: &str) -> Result<Manifest> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("banna-system 1") {
        return Err(String::from("güncelleme bildirimi tanınmıyor"));
    }
    let mut m = Manifest {
        version: String::new(),
        notes: String::new(),
        date: String::new(),
        changes: Vec::new(),
        drivers: Vec::new(),
        files: Vec::new(),
    };
    for line in lines {
        let (key, rest) = line.split_once(' ').unwrap_or((line, ""));
        match key {
            "version" => m.version = String::from(rest.trim()),
            "notes" => m.notes = String::from(rest.trim()),
            "date" => m.date = String::from(rest.trim()),
            "note" => m.changes.push(String::from(rest.trim())),
            "driver" => {
                if let Some((name, hash)) = rest.trim().split_once(' ') {
                    m.drivers
                        .push((String::from(name), String::from(hash.trim())));
                }
            }
            "file" => {
                let mut f = rest.splitn(3, ' ');
                let (Some(sha), Some(size), Some(path)) = (f.next(), f.next(), f.next()) else {
                    return Err(format!("bozuk satır: {}", line));
                };
                let path = String::from(path.trim());
                if !allowed(&path) {
                    return Err(format!("izin verilmeyen yol: {}", path));
                }
                m.files.push(Entry {
                    sha256: String::from(sha),
                    size: size.parse().map_err(|_| format!("bozuk boyut: {}", line))?,
                    path,
                });
            }
            _ => {}
        }
    }
    if m.version.is_empty() {
        return Err(String::from("bildirimde sürüm yok"));
    }
    Ok(m)
}

fn verified(data: &[u8], signature: &[u8]) -> Result<Manifest> {
    if !crate::ecosystem::verify_repository(data, signature) {
        return Err(String::from(
            "güncelleme imzası geçersiz; kaynak Banna deposu değil ya da değiştirilmiş",
        ));
    }
    parse(&String::from_utf8_lossy(data))
}

fn load_signed(path: &str) -> Result<Manifest> {
    let data = fs::read(path).map_err(|_| format!("{} yok", path))?;
    let signature = fs::read(&format!("{}.sig", path)).map_err(|_| format!("{}.sig yok", path))?;
    verified(&data, &signature)
}

/// Depodaki bildirimi indirir, imzasını doğrular ve sistem veritabanına kaydeder.
pub fn refresh(repo: &str) -> Result<Manifest> {
    let base = format!("{}system/", repo);
    let data = download(&format!("{}manifest", base))?;
    let signature = download(&format!("{}manifest.sig", base))
        .map_err(|_| String::from("güncelleme imzasız (manifest.sig yok); güvenilmiyor"))?;
    let m = verified(&data, &signature)?;
    // Yönetici değilse (banna-update check) yalnızca okunur, kaydedilmez.
    let _ = write(CACHE, &data).and_then(|_| write(&format!("{}.sig", CACHE), &signature));
    Ok(m)
}

/// `hatch update` ile kaydedilmiş bildirim.
pub fn cached() -> Result<Manifest> {
    load_signed(CACHE)
}

/// Yerel dosyası farklı olan girdiler.
pub fn changed(m: &Manifest) -> Vec<&Entry> {
    let installed = read_text(INSTALLED).unwrap_or_default();
    m.files
        .iter()
        .filter(|e| {
            let line = format!("file {} {} {}", e.sha256, e.size, e.path);
            if installed.lines().any(|l| l.trim() == line)
                && fs::stat(&e.path).is_ok_and(|s| s.size as usize == e.size)
            {
                return false;
            }
            match fs::read(&e.path) {
                Ok(data) => data.len() != e.size || to_hex(&sha256(&data)) != e.sha256,
                Err(_) => true,
            }
        })
        .collect()
}

/// Değişen sürücüler (çekirdekteki sürücülerin kaynak özeti). Yerel liste yoksa `None`.
pub fn driver_updates(m: &Manifest) -> Option<Vec<String>> {
    let local = read_text(DRIVERS_FILE)?;
    Some(
        m.drivers
            .iter()
            .filter(|(name, hash)| {
                !local.lines().any(|l| {
                    l.trim()
                        .split_once(' ')
                        .is_some_and(|(n, h)| n == name && h.trim() == hash)
                })
            })
            .map(|(name, _)| name.clone())
            .collect(),
    )
}

fn drivers_text(m: &Manifest) -> String {
    let mut text = String::new();
    for (name, hash) in &m.drivers {
        text.push_str(&format!("{} {}\n", name, hash));
    }
    text
}

fn touches_boot(list: &[&Entry]) -> bool {
    list.iter().any(|e| e.path.starts_with("/boot/"))
}

/// Hazırlanmış (yeniden başlatınca kurulacak) güncellemenin sürümü.
pub fn pending() -> Option<String> {
    read_text(PENDING)
}

/// Son açılıştaki uygulama hatası (varsa).
pub fn last_error() -> Option<String> {
    read_text(LAST_ERROR)
}

/// Değişen dosyaları indirip doğrular ve `STAGE` altına koyar; dosya sayısını döndürür.
pub fn stage(repo: &str, m: &Manifest) -> Result<usize> {
    let list = changed(m);
    if list.is_empty() {
        return Ok(0);
    }
    if touches_boot(&list) && fs::stat("/boot/boot").is_err() {
        return Err(String::from(
            "EFI bölümü /boot'a bağlı değil (Live USB'de sistem güncellemesi yapılamaz)",
        ));
    }
    let base = format!("{}system/", repo);
    let _ = fs::remove(PENDING);
    remove_tree(STAGE);
    for (i, e) in list.iter().enumerate() {
        println!(
            "[{}/{}] indiriliyor: {} ({} KiB)",
            i + 1,
            list.len(),
            e.path,
            e.size.div_ceil(1024)
        );
        let started = crate::time::uptime_ms();
        let data = download(&format!("{}files{}", base, e.path))?;
        report_speed(data.len(), started);
        if data.len() != e.size || to_hex(&sha256(&data)) != e.sha256 {
            remove_tree(STAGE);
            return Err(format!(
                "{}: sağlama toplamı tutmuyor; güncelleme iptal edildi",
                e.path
            ));
        }
        write(&format!("{}/files{}", STAGE, e.path), &data)?;
    }
    let manifest = fs::read(CACHE).map_err(|_| String::from("sistem veritabanı yok"))?;
    let signature = fs::read(&format!("{}.sig", CACHE)).unwrap_or_default();
    write(&format!("{}/manifest", STAGE), &manifest)?;
    write(&format!("{}/manifest.sig", STAGE), &signature)?;
    let _ = fs::remove(LAST_ERROR);
    write(PENDING, format!("{}\n", m.version).as_bytes())?;
    Ok(list.len())
}

/// Dosyayı yan dosyaya yazıp yerine taşır (yarım yazılmış dosya kalmaz).
fn replace(path: &str, data: &[u8]) -> Result<()> {
    let temp = format!("{}.new", path);
    write(&temp, data)?;
    let _ = fs::remove(path);
    fs::rename(&temp, path).map_err(|e| format!("{}: {}", path, fs::error_text(e)))?;
    let app_binary =
        path.starts_with("/apps/") && !path.rsplit('/').next().unwrap_or("").contains('.');
    if path.starts_with("/bin/") || app_binary {
        let _ = fs::chmod(path, 0o755);
    }
    Ok(())
}

fn install_kernel(data: &[u8]) -> Result<()> {
    if fs::stat(KERNEL).is_ok() {
        let _ = fs::remove(PREVIOUS_KERNEL);
        fs::rename(KERNEL, PREVIOUS_KERNEL)
            .map_err(|err| format!("eski çekirdek saklanamadı: {}", fs::error_text(err)))?;
    }
    fs::write(KERNEL, data).map_err(|err| {
        let _ = fs::rename(PREVIOUS_KERNEL, KERNEL);
        format!("çekirdek yazılamadı: {}", fs::error_text(err))
    })
}

fn apply_staged() -> Result<bool> {
    let m = load_signed(&format!("{}/manifest", STAGE))?;
    let mut staged: Vec<(&Entry, Vec<u8>)> = Vec::new();
    for e in &m.files {
        let Ok(data) = fs::read(&format!("{}/files{}", STAGE, e.path)) else {
            continue;
        };
        if data.len() != e.size || to_hex(&sha256(&data)) != e.sha256 {
            return Err(format!("{}: hazırlanan dosya bozuk", e.path));
        }
        staged.push((e, data));
    }
    let boot = staged.iter().any(|(e, _)| e.path.starts_with("/boot/"));
    if boot && fs::stat("/boot/boot").is_err() {
        return Err(String::from("EFI bölümü /boot'a bağlı değil"));
    }
    // Önce programlar, en son çekirdek (yarıda kalırsa eski çekirdek açılmaya devam eder).
    staged.sort_by_key(|(e, _)| e.path == KERNEL);
    for (e, data) in &staged {
        if e.path == KERNEL {
            install_kernel(data)?;
        } else {
            replace(&e.path, data)?;
        }
        println!("  güncellendi: {}", e.path);
    }
    let _ = fs::write(VERSION_FILE, format!("{}\n", m.version).as_bytes());
    let _ = fs::write(DRIVERS_FILE, drivers_text(&m).as_bytes());
    if let Ok(data) = fs::read(&format!("{}/manifest", STAGE)) {
        let _ = write(INSTALLED, &data);
    }
    println!("Banna OS {} kuruldu ({} dosya).", m.version, staged.len());
    Ok(boot)
}

/// Hazırlanan güncellemeyi kurar; önyükleme dosyaları değiştiyse `true`.
pub fn apply() -> Result<bool> {
    if fs::stat(PENDING).is_err() {
        return Err(String::from("bekleyen sistem güncellemesi yok"));
    }
    let _ = fs::remove(APPLYING);
    fs::rename(PENDING, APPLYING).map_err(|e| format!("{}: {}", PENDING, fs::error_text(e)))?;
    match apply_staged() {
        Ok(boot) => {
            remove_tree(STAGE);
            let _ = fs::remove(APPLYING);
            Ok(boot)
        }
        Err(e) => {
            let _ = write(LAST_ERROR, format!("{}\n", e).as_bytes());
            Err(e)
        }
    }
}

/// Bildirim zaten kurulu sistemle aynıysa sürüm/sürücü kayıtlarını eşitler.
pub fn mark_current(m: &Manifest) {
    let _ = fs::write(VERSION_FILE, format!("{}\n", m.version).as_bytes());
    let _ = fs::write(DRIVERS_FILE, drivers_text(m).as_bytes());
    if let Ok(data) = fs::read(CACHE) {
        let _ = write(INSTALLED, &data);
    }
}
