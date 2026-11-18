//! hatch: Banna OS paket yöneticisi.
//!
//! Depo düz HTTP üzerinden sunulur: `<kaynak>/index` paket listesi, `<kaynak>/pkgs/*.hpk`
//! paketler. Her paket indirilince boyutu ve SHA-256 özeti listeyle karşılaştırılır.
//!
//! Durum: `/etc/hatch/sources` (depo adresi), `/var/hatch/index` (son liste),
//! `/var/hatch/installed` (kurulu paketler), `/var/hatch/files/<paket>` (dosya listesi).
//!
//! Sistem ve sürücü güncellemeleri de buradan gelir (`banna::sysupdate`): `update`
//! sistem veritabanını alır, `upgrade` güncellemeyi indirip yeniden başlatmaya hazırlar.

#![no_std]
#![no_main]

use banna::crypto;
use banna::ecosystem;
use banna::sysupdate;
use banna::{String, Vec, format, fs, io, json, net, println, process, system, tty};
use core::cmp::Ordering;

banna::entry!(main);

const SOURCES: &str = "/etc/hatch/sources";
const STATE_DIR: &str = "/var/hatch";
const INDEX: &str = "/var/hatch/index";
const INSTALLED: &str = "/var/hatch/installed";
const FILES_DIR: &str = "/var/hatch/files";
/// Banna ekosistemindeki Banna deposu (banna-repo/hatch). Yerel test deposu için
/// `hatch source http://@gateway:8080/` (`@gateway` DHCP ağ geçidiyle değiştirilir).
const DEFAULT_SOURCE: &str = "https://ecosys.trybanna.com/hatch/";
const MAX_PACKAGE: usize = 32 * 1024 * 1024;
const PCI_IDS: &str = "/usr/share/hwdata/pci.ids";
const USB_IDS: &str = "/usr/share/hwdata/usb.ids";
/// Aydınlatma metninin sürümü; metin değişince sunucudaki değerle birlikte değiştirilir ve
/// kullanıcıdan yeniden onay istenir.
const NOTICE_VERSION: &str = "2026-11-18";
const NOTICE: &str = "\
DONANIM BİLGİLERİ AYDINLATMA METNİ VE AÇIK RIZA

Veri sorumlusu: Banna Software Company (xliofpir@gmail.com)

Gönderilecek bilgiler:
  - Bu ekranda sorulan ya da sistemde kayıtlı bilgisayar adı
  - Banna OS sürümü
  - İşlemci modeli ve toplam bellek miktarı
  - PCI ve USB aygıtlarının satıcı, model ve kimlik bilgileri
  - Disk modelleri, kullanılan sürücüler ve yüklü çekirdek modülleri

Gönderilmeyecek bilgiler: dosyalarınız, parolalarınız, ağ (MAC) adresiniz, aygıt
seri numaraları. E-Banna adresiniz bu kayıtlara yazılmaz; aynı bilgisayarın iki kez
sayılmaması için hesabınızdan ve cihazınızdan türetilen tek yönlü bir anahtar tutulur.

Amaç: Banna OS'un hangi donanımlarda kullanıldığını görmek ve sürücü geliştirmeyi
buna göre önceliklendirmek. Her donanım için kaç bilgisayarda kullanıldığını gösteren
bir sayım listesi de tutulur.

Hukuki sebep: açık rızanız (KVKK m. 5/1). Veriler Banna'nın sunucusundaki ayrı bir
veritabanında saklanır, yalnızca Banna yöneticisi erişebilir ve üçüncü kişilerle
paylaşılmaz. Kayıt, silinmesini isteyene ya da amaç ortadan kalkana kadar tutulur.

Haklarınız: KVKK m. 11 kapsamındaki haklarınızı kullanmak ya da rızanızı geri alıp
kaydınızı sildirmek için bilgisayar adınızla birlikte xliofpir@gmail.com adresine
yazabilirsiniz. Kabul etmemeniz Banna OS'u kullanmanızı etkilemez.";

#[derive(Clone, Debug)]
struct Package {
    name: String,
    version: String,
    size: usize,
    sha256: String,
    depends: Vec<String>,
    file: String,
    description: String,
    /// Tanıtım metni ve bu sürümün notları (Banna Store'un uygulama sayfası).
    about: String,
    notes: Vec<String>,
}

type Result<T> = core::result::Result<T, String>;

fn usage() -> i32 {
    println!("hatch - Banna OS paket yöneticisi");
    println!();
    println!("  hatch update              paket listesini ve sistem güncellemelerini denetle");
    println!("  hatch install <paket>...  paket(ler)i bağımlılıklarıyla kur");
    println!("  hatch upgrade             paketleri yükselt, sistem/sürücü güncellemesini hazırla");
    println!("  hatch status              sistem sürümü ve bekleyen güncelleme");
    println!("  hatch remove <paket>...   paketi kaldır");
    println!("  hatch list                kurulu paketler");
    println!("  hatch search [metin]      depodaki paketlerde ara");
    println!("  hatch info <paket>        paket ayrıntıları");
    println!("  hatch source [adres]      depo adresini göster / ayarla");
    println!("  hatch diagnose            donanımları, modelleri ve sürücüleri listele");
    println!("  hatch diagnose --send     onayınızla donanım listesini Banna'ya gönder");
    println!();
    println!("Kurma, güncelleme ve kaldırma yönetici ister: sudo hatch ...");
    0
}

fn require_root(command: &str) -> Result<()> {
    if process::uid() == 0 {
        Ok(())
    } else {
        Err(format!(
            "bu işlem yönetici yetkisi ister: sudo hatch {}",
            command
        ))
    }
}

fn mkdir_p(path: &str) {
    let mut current = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        current.push('/');
        current.push_str(part);
        let _ = fs::mkdir(&current);
    }
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(i) => &path[..i],
    }
}

fn read_text(path: &str) -> Option<String> {
    fs::read(path)
        .ok()
        .map(|d| String::from_utf8_lossy(&d).into_owned())
}

fn write_text(path: &str, text: &str) -> Result<()> {
    mkdir_p(parent(path));
    fs::write(path, text.as_bytes()).map_err(|e| format!("{}: {}", path, fs::error_text(e)))
}

fn source() -> String {
    read_text(SOURCES)
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty() && !l.starts_with('#'))
                .map(String::from)
        })
        .unwrap_or_else(|| String::from(DEFAULT_SOURCE))
}

/// Kaynak adresini kullanılabilir hale getirir (`@gateway`, sondaki `/`).
fn base_url() -> Result<String> {
    let mut url = source();
    if url.contains("@gateway") {
        let info = net::netinfo().ok_or_else(|| String::from("ağ bağlantısı yok"))?;
        url = url.replace("@gateway", &format!("{}", info.gateway));
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    Ok(url)
}

fn download(url: &str) -> Result<Vec<u8>> {
    let response = net::get(url)?;
    if response.status != 200 {
        return Err(format!("{}: HTTP {}", url, response.status));
    }
    Ok(response.body)
}

fn parse_index(text: &str) -> Result<Vec<Package>> {
    let mut lines = text.lines();
    if lines.next().map(str::trim) != Some("hatch-index 1") {
        return Err(String::from(
            "paket listesi tanınmadı (hatch-index 1 bekleniyordu)",
        ));
    }
    let mut packages = Vec::new();
    let mut current: Option<Package> = None;
    for line in lines {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        let value = value.trim();
        if key == "package" {
            current = Some(Package {
                name: String::from(value),
                version: String::new(),
                size: 0,
                sha256: String::new(),
                depends: Vec::new(),
                file: String::new(),
                description: String::new(),
                about: String::new(),
                notes: Vec::new(),
            });
            continue;
        }
        let Some(p) = current.as_mut() else {
            return Err(format!("paket listesi bozuk: '{}'", line));
        };
        match key {
            "version" => p.version = String::from(value),
            "size" => p.size = value.parse().unwrap_or(0),
            "sha256" => p.sha256 = value.to_ascii_lowercase(),
            "depends" => {
                p.depends = value
                    .split([',', ' '])
                    .filter(|d| !d.is_empty())
                    .map(String::from)
                    .collect()
            }
            "file" => p.file = String::from(value),
            "description" => p.description = String::from(value),
            "about" => p.about = String::from(value),
            "note" => p.notes.push(String::from(value)),
            "end" => {
                let p = current.take().unwrap();
                if p.name.is_empty() || p.version.is_empty() || p.file.is_empty() {
                    return Err(format!("paket listesi bozuk: '{}' eksik alan", p.name));
                }
                packages.push(p);
            }
            _ => {}
        }
    }
    Ok(packages)
}

fn load_index() -> Result<Vec<Package>> {
    let text = read_text(INDEX)
        .ok_or_else(|| String::from("paket listesi yok; önce: sudo hatch update"))?;
    parse_index(&text)
}

/// Noktalı sürümleri sayısal parçalarıyla karşılaştırır (1.10 > 1.9).
fn version_cmp(a: &str, b: &str) -> Ordering {
    let mut x = a.split(['.', '-']);
    let mut y = b.split(['.', '-']);
    loop {
        match (x.next(), y.next()) {
            (None, None) => return Ordering::Equal,
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (Some(p), Some(q)) => {
                let order = match (p.parse::<u64>(), q.parse::<u64>()) {
                    (Ok(m), Ok(n)) => m.cmp(&n),
                    _ => p.cmp(q),
                };
                if order != Ordering::Equal {
                    return order;
                }
            }
        }
    }
}

fn installed() -> Vec<(String, String)> {
    read_text(INSTALLED)
        .map(|text| {
            text.lines()
                .filter_map(|l| {
                    let (name, version) = l.trim().split_once(' ')?;
                    Some((String::from(name), String::from(version)))
                })
                .collect()
        })
        .unwrap_or_default()
}

fn save_installed(list: &[(String, String)]) -> Result<()> {
    let mut text = String::new();
    for (name, version) in list {
        text.push_str(&format!("{} {}\n", name, version));
    }
    write_text(INSTALLED, &text)
}

fn files_of(name: &str) -> Vec<String> {
    read_text(&format!("{}/{}", FILES_DIR, name))
        .map(|t| t.lines().map(String::from).collect())
        .unwrap_or_default()
}

/// `.hpk`: "HPK1\n", ardından her dosya için "F <boyut> <yol>\n" ve ham baytlar.
fn unpack(data: &[u8]) -> Result<Vec<(String, &[u8])>> {
    let mut rest = data
        .strip_prefix(b"HPK1\n")
        .ok_or_else(|| String::from("paket biçimi tanınmadı"))?;
    let mut files = Vec::new();
    while !rest.is_empty() {
        let end = rest
            .iter()
            .position(|&b| b == b'\n')
            .ok_or_else(|| String::from("paket bozuk"))?;
        let header = core::str::from_utf8(&rest[..end]).map_err(|_| String::from("paket bozuk"))?;
        let mut parts = header.splitn(3, ' ');
        let (Some("F"), Some(size), Some(path)) = (parts.next(), parts.next(), parts.next()) else {
            return Err(String::from("paket bozuk"));
        };
        let size: usize = size.parse().map_err(|_| String::from("paket bozuk"))?;
        rest = &rest[end + 1..];
        if rest.len() < size {
            return Err(String::from("paket eksik"));
        }
        files.push((String::from(path), &rest[..size]));
        rest = &rest[size..];
    }
    Ok(files)
}

/// Paketler yalnızca `/bin`, `/usr`, kendi `/etc/<paket>` ve `/apps/<paket>` (masaüstü
/// uygulaması) dizinlerine yazabilir.
fn allowed_path(package: &str, path: &str) -> bool {
    if !path.starts_with('/') || path.split('/').any(|p| p == ".." || p == ".") {
        return false;
    }
    let own_etc = format!("/etc/{}/", package);
    let own_app = format!("/apps/{}/", package);
    path.starts_with("/bin/")
        || path.starts_with("/usr/")
        || path.starts_with(&own_etc)
        || path.starts_with(&own_app)
}

fn find<'a>(index: &'a [Package], name: &str) -> Option<&'a Package> {
    index.iter().find(|p| p.name == name)
}

/// Bağımlılıklar önce gelecek şekilde kurulum sırası.
fn plan<'a>(index: &'a [Package], names: &[&str]) -> Result<Vec<&'a Package>> {
    fn visit<'a>(
        index: &'a [Package],
        name: &str,
        order: &mut Vec<&'a Package>,
        stack: &mut Vec<String>,
    ) -> Result<()> {
        if order.iter().any(|p| p.name == name) {
            return Ok(());
        }
        if stack.iter().any(|s| s == name) {
            return Err(format!("döngüsel bağımlılık: {}", name));
        }
        let package = find(index, name).ok_or_else(|| match stack.last() {
            Some(parent) => format!("{} paketinin bağımlılığı {} depoda yok", parent, name),
            None => format!("{} adlı paket yok (hatch search)", name),
        })?;
        stack.push(String::from(name));
        for dep in &package.depends {
            visit(index, dep, order, stack)?;
        }
        stack.pop();
        order.push(package);
        Ok(())
    }
    let mut order = Vec::new();
    for name in names {
        visit(index, name, &mut order, &mut Vec::new())?;
    }
    Ok(order)
}

fn kib(bytes: usize) -> usize {
    bytes.div_ceil(1024)
}

fn install_one(base: &str, package: &Package) -> Result<()> {
    if package.size > MAX_PACKAGE {
        return Err(format!("{}: paket çok büyük", package.name));
    }
    println!(
        "İndiriliyor {} {} ({} KiB)...",
        package.name,
        package.version,
        kib(package.size)
    );
    let started = banna::time::uptime_ms();
    let data = download(&format!("{}{}", base, package.file))?;
    sysupdate::report_speed(data.len(), started);
    if data.len() != package.size {
        return Err(format!(
            "{}: boyut uyuşmuyor ({} != {})",
            package.name,
            data.len(),
            package.size
        ));
    }
    let digest = crypto::to_hex(&crypto::sha256(&data));
    if digest != package.sha256 {
        return Err(format!("{}: SHA-256 doğrulanamadı", package.name));
    }
    let files = unpack(&data)?;
    if let Some((bad, _)) = files.iter().find(|(p, _)| !allowed_path(&package.name, p)) {
        return Err(format!("{}: izin verilmeyen yol {}", package.name, bad));
    }
    println!(
        "Kuruluyor {} {} ({} dosya)",
        package.name,
        package.version,
        files.len()
    );
    let previous = files_of(&package.name);
    for (path, bytes) in &files {
        mkdir_p(parent(path));
        fs::write(path, bytes).map_err(|e| format!("{}: {}", path, fs::error_text(e)))?;
        // ext2'de programlar çalıştırılabilir, diğer dosyalar herkesçe okunabilir olmalı.
        let app_binary =
            path.starts_with("/apps/") && !path.rsplit('/').next().unwrap_or("").contains('.');
        let mode = if path.starts_with("/bin/") || app_binary {
            0o755
        } else {
            0o644
        };
        let _ = fs::chmod(path, mode);
    }
    for old in previous
        .iter()
        .filter(|o| !files.iter().any(|(p, _)| p == *o))
    {
        let _ = fs::remove(old);
    }
    let list: Vec<&str> = files.iter().map(|(p, _)| p.as_str()).collect();
    write_text(
        &format!("{}/{}", FILES_DIR, package.name),
        &format!("{}\n", list.join("\n")),
    )?;
    let mut db = installed();
    db.retain(|(n, _)| *n != package.name);
    db.push((package.name.clone(), package.version.clone()));
    save_installed(&db)
}

fn cmd_new() -> Result<()> {
    require_root("update")?;
    let base = base_url()?;
    println!("Paket listesi alınıyor: {}index", base);
    let data = download(&format!("{}index", base))?;
    let signature = download(&format!("{}index.sig", base))
        .map_err(|_| String::from("paket listesi imzasız (index.sig yok); depo güvenilmiyor"))?;
    if !banna::ecosystem::verify_repository(&data, &signature) {
        return Err(String::from(
            "paket listesinin imzası geçersiz; depo Banna deposu değil ya da değiştirilmiş",
        ));
    }
    println!("İmza doğrulandı (Banna deposu anahtarı).");
    let text = String::from_utf8_lossy(&data).into_owned();
    let index = parse_index(&text)?;
    mkdir_p(STATE_DIR);
    write_text(INDEX, &text)?;
    let upgradable = upgrades(&index).len();
    println!("Paket listesi güncellendi: {} paket.", index.len());
    if upgradable > 0 {
        println!("{} paket yükseltilebilir.", upgradable);
    } else {
        println!("Tüm paketler güncel.");
    }
    let system = match sysupdate::refresh(&base) {
        Ok(m) => system_summary(&m),
        Err(e) => {
            println!("Sistem güncellemeleri alınamadı: {}", e);
            false
        }
    };
    if upgradable > 0 || system {
        println!("Kurmak için: sudo hatch upgrade");
    }
    Ok(())
}

/// Sistem veritabanını kurulu sistemle karşılaştırır; güncelleme varsa `true`.
fn system_summary(m: &sysupdate::Manifest) -> bool {
    let files = sysupdate::changed(m);
    if files.is_empty() {
        println!("Sistem güncel ({}).", sysupdate::current_version());
        return false;
    }
    let size: usize = files.iter().map(|e| e.size).sum();
    println!(
        "Sistem güncellemesi: {} -> {} ({} dosya, {} KiB)",
        sysupdate::current_version(),
        m.version,
        files.len(),
        kib(size)
    );
    if !m.notes.is_empty() {
        println!("  {}", m.notes);
    }
    for change in &m.changes {
        println!("  • {}", change);
    }
    if files.iter().any(|e| e.path == sysupdate::KERNEL) {
        match sysupdate::driver_updates(m) {
            Some(d) if !d.is_empty() => println!("  Sürücü güncellemeleri: {}", d.join(", ")),
            Some(_) => println!("  Çekirdek güncellemesi"),
            None => println!("  Çekirdek ve sürücü güncellemesi"),
        }
    }
    let firmware = files
        .iter()
        .filter(|e| e.path.starts_with("/lib/firmware/"))
        .count();
    if firmware > 0 {
        println!("  {} sürücü yazılımı (firmware)", firmware);
    }
    if sysupdate::pending().as_deref() == Some(m.version.as_str()) {
        println!("  Hazır: yeniden başlatınca kurulacak.");
    }
    true
}

fn upgrade_system(base: &str) -> Result<()> {
    let m = match sysupdate::cached() {
        Ok(m) => m,
        Err(_) => {
            println!("Sistem veritabanı yok; önce: sudo hatch update");
            return Ok(());
        }
    };
    if sysupdate::changed(&m).is_empty() {
        sysupdate::mark_current(&m);
        println!("Sistem güncel ({}).", m.version);
        return Ok(());
    }
    if sysupdate::pending().as_deref() == Some(m.version.as_str()) {
        println!(
            "Sistem güncellemesi {} zaten hazır; yeniden başlatınca kurulacak.",
            m.version
        );
        return Ok(());
    }
    println!("Sistem güncellemesi {} indiriliyor...", m.version);
    let count = sysupdate::stage(base, &m)?;
    println!(
        "Sistem güncellemesi {} hazır ({} dosya). Yeniden başlatınca kurulacak: reboot",
        m.version, count
    );
    Ok(())
}

fn cmd_status() -> Result<()> {
    println!("Banna OS {}", sysupdate::current_version());
    match sysupdate::pending() {
        Some(v) => println!("Yeniden başlatınca kurulacak: {}", v),
        None => println!("Bekleyen sistem güncellemesi yok."),
    }
    if let Some(e) = sysupdate::last_error() {
        println!("Son güncelleme başarısız oldu: {}", e);
    }
    if let Ok(m) = sysupdate::cached() {
        system_summary(&m);
    }
    Ok(())
}

fn upgrades(index: &[Package]) -> Vec<&Package> {
    installed()
        .iter()
        .filter_map(|(name, version)| {
            find(index, name).filter(|p| version_cmp(&p.version, version) == Ordering::Greater)
        })
        .collect()
}

fn cmd_install(names: &[&str]) -> Result<()> {
    require_root("install")?;
    if names.is_empty() {
        return Err(String::from(
            "kurulacak paket adı gerekli: hatch install <paket>",
        ));
    }
    let index = load_index()?;
    let current = installed();
    let order = plan(&index, names)?;
    let todo: Vec<&Package> = order
        .into_iter()
        .filter(|p| {
            let same = current
                .iter()
                .any(|(n, v)| *n == p.name && version_cmp(v, &p.version) != Ordering::Less);
            if same && names.contains(&p.name.as_str()) {
                println!("{} zaten kurulu ({}).", p.name, p.version);
            }
            !same
        })
        .collect();
    if todo.is_empty() {
        return Ok(());
    }
    let list: Vec<&str> = todo.iter().map(|p| p.name.as_str()).collect();
    let total: usize = todo.iter().map(|p| p.size).sum();
    println!("Kurulacak: {} ({} KiB)", list.join(" "), kib(total));
    let base = base_url()?;
    for package in &todo {
        install_one(&base, package)?;
    }
    println!("{} paket kuruldu.", todo.len());
    Ok(())
}

fn cmd_upgrade() -> Result<()> {
    require_root("upgrade")?;
    let base = base_url()?;
    upgrade_packages(&base)?;
    upgrade_system(&base)
}

fn upgrade_packages(base: &str) -> Result<()> {
    let index = load_index()?;
    let todo = upgrades(&index);
    if todo.is_empty() {
        println!("Tüm paketler güncel.");
        return Ok(());
    }
    let current = installed();
    for p in &todo {
        let old = current
            .iter()
            .find(|(n, _)| *n == p.name)
            .map_or("?", |(_, v)| v.as_str());
        println!("  {} {} -> {}", p.name, old, p.version);
    }
    let names: Vec<&str> = todo.iter().map(|p| p.name.as_str()).collect();
    let order = plan(&index, &names)?;
    let mut count = 0;
    for package in order {
        let up_to_date = current
            .iter()
            .any(|(n, v)| *n == package.name && version_cmp(v, &package.version) != Ordering::Less);
        if !up_to_date {
            install_one(base, package)?;
            count += 1;
        }
    }
    println!("{} paket yükseltildi.", count);
    Ok(())
}

fn cmd_remove(names: &[&str]) -> Result<()> {
    require_root("remove")?;
    if names.is_empty() {
        return Err(String::from("kaldırılacak paket adı gerekli"));
    }
    let mut db = installed();
    for name in names {
        if !db.iter().any(|(n, _)| n == name) {
            return Err(format!("{} kurulu değil", name));
        }
        if let Ok(index) = load_index() {
            let users: Vec<&str> = db
                .iter()
                .filter(|(n, _)| !names.contains(&n.as_str()))
                .filter_map(|(n, _)| find(&index, n))
                .filter(|p| p.depends.iter().any(|d| d == name))
                .map(|p| p.name.as_str())
                .collect();
            if !users.is_empty() {
                return Err(format!(
                    "{} şunlar tarafından kullanılıyor: {}",
                    name,
                    users.join(" ")
                ));
            }
        }
    }
    for name in names {
        let files = files_of(name);
        for path in &files {
            let _ = fs::remove(path);
        }
        let _ = fs::remove(&format!("{}/{}", FILES_DIR, name));
        db.retain(|(n, _)| n != name);
        println!("{} kaldırıldı ({} dosya).", name, files.len());
    }
    save_installed(&db)
}

fn cmd_list() -> Result<()> {
    let db = installed();
    if db.is_empty() {
        println!("Kurulu paket yok.");
        return Ok(());
    }
    let index = load_index().unwrap_or_default();
    for (name, version) in &db {
        let newer = find(&index, name)
            .filter(|p| version_cmp(&p.version, version) == Ordering::Greater)
            .map(|p| format!("  (yükseltilebilir: {})", p.version))
            .unwrap_or_default();
        println!("{:<16} {}{}", name, version, newer);
    }
    Ok(())
}

fn cmd_search(query: Option<&str>) -> Result<()> {
    let index = load_index()?;
    let query = query.map(|q| q.to_lowercase());
    let db = installed();
    let mut shown = 0;
    for p in &index {
        let matches = query.as_ref().is_none_or(|q| {
            p.name.to_lowercase().contains(q.as_str())
                || p.description.to_lowercase().contains(q.as_str())
        });
        if matches {
            let mark = if db.iter().any(|(n, _)| *n == p.name) {
                " [kurulu]"
            } else {
                ""
            };
            println!("{:<16} {:<8} {}{}", p.name, p.version, p.description, mark);
            shown += 1;
        }
    }
    if shown == 0 {
        println!("Eşleşen paket yok.");
    }
    Ok(())
}

fn cmd_info(name: Option<&str>) -> Result<()> {
    let name = name.ok_or_else(|| String::from("paket adı gerekli: hatch info <paket>"))?;
    let index = load_index()?;
    let p = find(&index, name).ok_or_else(|| format!("{} adlı paket yok", name))?;
    println!("Paket:       {}", p.name);
    println!("Sürüm:       {}", p.version);
    println!("Açıklama:    {}", p.description);
    println!("Boyut:       {} KiB", kib(p.size));
    let deps = if p.depends.is_empty() {
        String::from("-")
    } else {
        p.depends.join(", ")
    };
    println!("Bağımlılık:  {}", deps);
    if !p.about.is_empty() {
        println!();
        println!("{}", p.about);
    }
    if !p.notes.is_empty() {
        println!();
        println!("{} sürümünde:", p.version);
        for n in &p.notes {
            println!("  • {}", n);
        }
    }
    println!();
    match installed().iter().find(|(n, _)| *n == p.name) {
        Some((_, v)) => {
            println!("Durum:       kurulu ({})", v);
            for f in files_of(&p.name) {
                println!("             {}", f);
            }
        }
        None => println!("Durum:       kurulu değil"),
    }
    Ok(())
}

fn cmd_source(url: Option<&str>) -> Result<()> {
    match url {
        None => {
            println!("{}", source());
            if let Ok(base) = base_url() {
                println!("(şu an: {})", base);
            }
            Ok(())
        }
        Some(url) => {
            require_root("source")?;
            if !url.starts_with("http://") && !url.starts_with("https://") {
                return Err(String::from("adres http:// ya da https:// ile başlamalı"));
            }
            write_text(SOURCES, &format!("{}\n", url))?;
            println!("Depo: {}  (şimdi: sudo hatch update)", url);
            Ok(())
        }
    }
}

/// `hatch diagnose` satırı. `slot` ve `state` yalnızca ekranda gösterilir, gönderilmez.
struct Hardware {
    kind: &'static str,
    category: String,
    vendor: String,
    model: String,
    id: String,
    driver: String,
    slot: String,
    state: String,
}

impl Hardware {
    fn new(kind: &'static str, category: &str, vendor: &str, model: &str) -> Self {
        Self {
            kind,
            category: String::from(category),
            vendor: String::from(vendor),
            model: String::from(model),
            id: String::new(),
            driver: String::new(),
            slot: String::new(),
            state: String::new(),
        }
    }

    /// "Satıcı Model"; model adı satıcıyı zaten içeriyorsa tekrarlanmaz.
    fn name(&self) -> String {
        if self.vendor.is_empty() || self.model.contains(self.vendor.as_str()) {
            self.model.clone()
        } else {
            format!("{} {}", self.vendor, self.model)
        }
    }

    fn to_json(&self) -> String {
        json::object(&[
            ("kind", self.kind),
            ("category", &self.category),
            ("vendor", &self.vendor),
            ("model", &self.model),
            ("hardwareId", &self.id),
            ("driver", &self.driver),
        ])
    }
}

/// `pci.ids` / `usb.ids` içinde "satıcı:aygıt" kimliğinin satıcı ve model adı.
fn id_names(db: &str, id: &str) -> (Option<String>, Option<String>) {
    let Some((vendor_id, device_id)) = id.split_once(':') else {
        return (None, None);
    };
    let mut vendor = None;
    for line in db.lines() {
        if line.starts_with('#') {
            continue;
        }
        match line.strip_prefix('\t') {
            None if vendor.is_some() => break,
            None => {
                vendor = line
                    .strip_prefix(vendor_id)
                    .and_then(|l| l.strip_prefix("  "))
                    .map(String::from);
            }
            Some(device) if vendor.is_some() => {
                let model = device
                    .strip_prefix(device_id)
                    .and_then(|l| l.strip_prefix("  "));
                if let Some(model) = model {
                    return (vendor, Some(String::from(model)));
                }
            }
            Some(_) => {}
        }
    }
    (vendor, None)
}

fn c_text(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from(String::from_utf8_lossy(&bytes[..end]).trim())
}

fn collect_hardware() -> Vec<Hardware> {
    let mut out = Vec::new();
    let info = process::sysinfo();
    let vendor = match c_text(&info.vendor).as_str() {
        "GenuineIntel" => String::from("Intel"),
        "AuthenticAMD" => String::from("AMD"),
        other => String::from(other),
    };
    let mut cpu = Hardware::new("cpu", "İşlemci", &vendor, &c_text(&info.brand));
    cpu.state = format!("{} çekirdek", info.cpus);
    out.push(cpu);
    // Kullanılabilir bellek takılı olandan azdır: takılı miktara yuvarlanır.
    let mib = info.total_memory >> 20;
    let memory = if mib < 2048 {
        format!("{} MiB", mib.div_ceil(256) * 256)
    } else {
        format!("{} GiB", mib.div_ceil(1024))
    };
    out.push(Hardware::new("memory", "Bellek", "", &memory));

    let pci_ids = read_text(PCI_IDS).unwrap_or_default();
    let usb_ids = read_text(USB_IDS).unwrap_or_default();
    let report = system::drivers_report();
    let mut modules = Vec::new();
    for line in report.lines() {
        let fields: Vec<&str> = line.split('\t').collect();
        match fields.as_slice() {
            ["pci", slot, id, class, driver, state] => {
                let (vendor, model) = id_names(&pci_ids, id);
                let model = model.unwrap_or_else(|| format!("Bilinmeyen model ({})", id));
                let mut item = Hardware::new("pci", class, vendor.as_deref().unwrap_or(""), &model);
                item.id = String::from(*id);
                item.driver = String::from(*driver);
                item.slot = String::from(*slot);
                item.state = String::from(*state);
                out.push(item);
            }
            ["usb", port, id, name, drivers] => {
                let (vendor, model) = id_names(&usb_ids, id);
                let model = model.unwrap_or_else(|| {
                    if name.is_empty() {
                        format!("Bilinmeyen model ({})", id)
                    } else {
                        String::from(*name)
                    }
                });
                let mut item =
                    Hardware::new("usb", "USB aygıtı", vendor.as_deref().unwrap_or(""), &model);
                item.id = String::from(*id);
                item.driver = String::from(*drivers);
                item.slot = format!("port {}", port);
                out.push(item);
            }
            ["mod", name, size] => {
                let mut item = Hardware::new("module", "Çekirdek modülü", "", name);
                item.driver = String::from(*name);
                item.state = format!("{} KiB", size);
                modules.push(item);
            }
            _ => {}
        }
    }
    for disk in system::storage().disks {
        let category = if disk.removable {
            "Çıkarılabilir depolama"
        } else {
            "Depolama"
        };
        let mut item = Hardware::new("disk", category, "", &disk.model);
        item.slot = disk.name;
        out.push(item);
    }
    out.append(&mut modules);
    out
}

fn system_hostname() -> String {
    read_text("/etc/hostname")
        .map(|h| String::from(h.trim()))
        .unwrap_or_default()
}

fn print_hardware(items: &[Hardware]) {
    const SECTIONS: [(&str, &str); 6] = [
        ("cpu", "İşlemci"),
        ("memory", "Bellek"),
        ("pci", "PCI aygıtları"),
        ("usb", "USB aygıtları"),
        ("disk", "Diskler"),
        ("module", "Yüklü çekirdek modülleri"),
    ];
    for (kind, title) in SECTIONS {
        let section: Vec<&Hardware> = items.iter().filter(|h| h.kind == kind).collect();
        if section.is_empty() {
            continue;
        }
        println!();
        println!("{} ({})", title, section.len());
        for h in section {
            println!("  {}", h.name());
            let mut details: Vec<String> = Vec::new();
            for (label, value) in [("", &h.slot), ("kimlik ", &h.id)] {
                if !value.is_empty() {
                    details.push(format!("{}{}", label, value));
                }
            }
            if !matches!(h.kind, "cpu" | "memory" | "module" | "usb") {
                details.push(h.category.clone());
            }
            if h.kind != "module" && !h.driver.is_empty() {
                let driver = if h.driver == "-" { "yok" } else { &h.driver };
                details.push(format!("sürücü {}", driver));
            }
            if !h.state.is_empty() {
                details.push(h.state.clone());
            }
            if !details.is_empty() {
                println!("    {}", details.join(" | "));
            }
        }
    }
}

fn config_path(name: &str) -> String {
    format!("/home/{}/.config/banna/{}", process::username(), name)
}

/// Metnin bu sürümü daha önce kabul edilmediyse gösterir ve açık onay ister.
fn ensure_consent() -> Result<bool> {
    let path = config_path("hardware-consent");
    if read_text(&path).as_deref().map(str::trim) == Some(NOTICE_VERSION) {
        println!(
            "Aydınlatma metnini daha önce kabul ettiniz ({}).",
            NOTICE_VERSION
        );
        return Ok(true);
    }
    if !tty::is_tty(0) {
        return Err(String::from(
            "aydınlatma metni terminalde okunup onaylanmalı; komutu doğrudan çalıştırın",
        ));
    }
    println!("{}", NOTICE);
    println!();
    let answer = io::prompt("Okudum, kabul ediyorum. Onaylamak için 'kabul' yazın: ");
    if !answer.eq_ignore_ascii_case("kabul") {
        return Ok(false);
    }
    write_text(&path, &format!("{}\n", NOTICE_VERSION))?;
    Ok(true)
}

/// Bilgisayar adı: sistemdeki ad; varsayılan "banna" ise bir kez sorulur ve saklanır.
fn computer_name() -> Result<String> {
    let path = config_path("computer-name");
    if let Some(saved) = read_text(&path) {
        let saved = saved.trim();
        if !saved.is_empty() {
            return Ok(String::from(saved));
        }
    }
    let hostname = system_hostname();
    if !hostname.is_empty() && hostname != "banna" {
        return Ok(hostname);
    }
    let answer = io::prompt("Bilgisayar adı (boş bırakılırsa \"banna\"): ");
    let name: String = answer
        .chars()
        .filter(|c| !c.is_control())
        .take(64)
        .collect();
    let name = match name.trim() {
        "" => String::from("banna"),
        name => String::from(name),
    };
    write_text(&path, &format!("{}\n", name))?;
    Ok(name)
}

fn send_diagnose(items: &[Hardware]) -> Result<()> {
    let mut client = ecosystem::Client::load();
    if !client.has_session() {
        return Err(String::from(
            "Banna hesabı oturumu bulunamadı; Banna Panel'den giriş yapıp komutu sudo olmadan çalıştırın",
        ));
    }
    if !ensure_consent()? {
        println!("Kabul edilmedi; hiçbir bilgi gönderilmedi.");
        return Ok(());
    }
    let name = computer_name()?;
    let devices: Vec<String> = items.iter().map(Hardware::to_json).collect();
    let body = format!(
        "{{\"consent\":true,\"noticeVersion\":{},\"computerName\":{},\"osVersion\":{},\"devices\":[{}]}}",
        json::quote(NOTICE_VERSION),
        json::quote(&name),
        json::quote(&sysupdate::current_version()),
        devices.join(",")
    );
    client
        .post("/banna-os/hardware-reports", &body)
        .map_err(|e| e.text())?;
    println!(
        "{} için {} kayıt gönderildi. Teşekkürler.",
        name,
        items.len()
    );
    Ok(())
}

fn cmd_diagnose(args: &[&str]) -> Result<()> {
    let items = collect_hardware();
    match args {
        [] => {
            println!("Bilgisayar: {}", system_hostname());
            println!("Sistem:     Banna OS {}", sysupdate::current_version());
            print_hardware(&items);
            Ok(())
        }
        ["--send"] => send_diagnose(&items),
        _ => Err(String::from("kullanım: hatch diagnose [--send]")),
    }
}

fn main(args: &[&str]) -> i32 {
    let rest = args.get(2..).unwrap_or(&[]);
    let result = match args.get(1).copied() {
        Some("update") | Some("new") => cmd_new(),
        Some("install") | Some("-i") => cmd_install(rest),
        Some("upgrade") | Some("-U") => cmd_upgrade(),
        Some("remove") | Some("-r") => cmd_remove(rest),
        Some("list") => cmd_list(),
        Some("status") => cmd_status(),
        Some("search") => cmd_search(rest.first().copied()),
        Some("info") => cmd_info(rest.first().copied()),
        Some("source") => cmd_source(rest.first().copied()),
        Some("diagnose") => cmd_diagnose(rest),
        None | Some("help") | Some("-h") | Some("--help") => return usage(),
        Some(other) => Err(format!("bilinmeyen komut '{}' (hatch help)", other)),
    };
    match result {
        Ok(()) => 0,
        Err(e) => {
            banna::eprintln!("hatch: {}", e);
            1
        }
    }
}
