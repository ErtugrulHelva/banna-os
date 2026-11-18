//! Banna Store: Banna ekosisteminin uygulamaları, platform panelleriyle (Banna OS, Android,
//! iOS, Windows, macOS, Linux). Store açılınca bu cihazın işletim sisteminin paneli seçilir
//! ve yalnızca onunla uyumlu uygulamalar kurulabilir; diğer paneller o platformların
//! uygulamalarını indirme bağlantılarıyla gösterir.
//!
//! Banna OS paneli: Banna OS deposundan (hatch) kurma, kaldırma ve güncelleme. Katalog
//! doğrudan deponun `index` dosyasından okunur (giriş gerekmez). Kurma ve kaldırma yönetici
//! ister: parola bir kez sorulur, işlemi `hatch` yapar.
//!
//! Diğer paneller: ekosistemin Banna Store kataloğu (`/banna-store/apps`, mobil uygulamalar)
//! ve Banna Panel'in masaüstü sürümleri (`/updates/banna-panel/<os>/<mimari>`).
//!
//! Karta tıklanınca uygulama sayfası açılır (`detail.rs`): tanıtım, bu sürümdeki yenilikler
//! (paketin CHANGELOG.md'si, depoda `about` / `note` / `date`) ve sürüm bilgisi.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::ecosystem;
use banna::gfx::text::UiFonts;
use banna::gfx::{Rect, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_MOUSE_UP, EVENT_RESIZE,
    EVENT_WHEEL, Window,
};
use banna::icons::{self, Icon};
use banna::io::{self, PollEntry};
use banna::task::Task;
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, Ui, WHITE};
use banna::{desktop, fs, net, process, time};

banna::entry!(main);

mod detail;
use detail::Detail;

const SOURCES: &str = "/etc/hatch/sources";
const INSTALLED: &str = "/var/hatch/installed";

#[derive(Clone)]
struct Package {
    name: String,
    version: String,
    description: String,
    title: String,
    icon: Option<Icon>,
    size: usize,
    /// brct ile paketlenmiş (katalogda `builder brct`).
    brct: bool,
    /// Uygulama sayfası: tanıtım, bu sürümün notları, yayın tarihi, bağımlılıklar.
    about: String,
    notes: Vec<String>,
    date: String,
    depends: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Discover,
    Installed,
    Updates,
}

/// Store'un platform panelleri.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Platform {
    BannaOs,
    Android,
    Ios,
    Windows,
    Macos,
    Linux,
}

impl Platform {
    const ALL: [Platform; 6] = [
        Platform::BannaOs,
        Platform::Android,
        Platform::Ios,
        Platform::Windows,
        Platform::Macos,
        Platform::Linux,
    ];

    /// Store'u çalıştıran cihazın işletim sistemi: açılışta seçilen ve kurulum yapılabilen
    /// panel. Bu program yalnızca Banna OS'ta çalışır; diğer sistemlerin Banna Store'ları
    /// aynı panelleri kendi sistemleriyle açar.
    const fn current() -> Self {
        Platform::BannaOs
    }

    fn label(self) -> &'static str {
        match self {
            Platform::BannaOs => "Banna OS",
            Platform::Android => "Android",
            Platform::Ios => "iOS",
            Platform::Windows => "Windows",
            Platform::Macos => "macOS",
            Platform::Linux => "Linux",
        }
    }

    /// Katalogdaki `platforms` adı.
    fn from_id(id: &str) -> Option<Self> {
        Some(match id.to_ascii_lowercase().as_str() {
            "banna" | "bannaos" | "banna-os" => Platform::BannaOs,
            "android" => Platform::Android,
            "ios" | "ipados" => Platform::Ios,
            "windows" => Platform::Windows,
            "macos" | "darwin" | "mac" => Platform::Macos,
            "linux" => Platform::Linux,
            _ => return None,
        })
    }

    /// Bu cihazda kurulamayan uygulamalar için açıklama.
    fn hint(self) -> &'static str {
        match self {
            Platform::Android => "Android telefon ve tabletlerde Banna Store ile kurulur.",
            Platform::Ios => "iPhone ve iPad içindir.",
            Platform::Windows => "Windows bilgisayarlar içindir.",
            Platform::Macos => "Mac bilgisayarlar içindir.",
            Platform::Linux => "Linux bilgisayarlar içindir.",
            Platform::BannaOs => "",
        }
    }
}

/// Başka bir platformun uygulaması (bu cihaza kurulamaz; bağlantısı kopyalanabilir).
#[derive(Clone)]
struct RemoteApp {
    id: String,
    name: String,
    tagline: String,
    version: String,
    size: u64,
    platforms: Vec<Platform>,
    /// Ek bilgi (ör. desteklenen işlemci mimarileri).
    detail: String,
    link: String,
    icon: Option<Icon>,
    description: String,
    notes: Vec<String>,
    date: String,
}

/// Banna Panel'in masaüstü yayınları: (hedef, mimari, platform, mimari adı).
const DESKTOP_TARGETS: [(&str, &str, Platform, &str); 4] = [
    ("darwin", "aarch64", Platform::Macos, "Apple Silicon"),
    ("darwin", "x86_64", Platform::Macos, "Intel"),
    ("windows", "x86_64", Platform::Windows, "x64"),
    ("linux", "x86_64", Platform::Linux, "x64"),
];

#[derive(Clone, PartialEq)]
enum Control {
    Platform(Platform),
    Tab(Tab),
    CopyLink(String),
    Open(Detail),
    Back,
    Launch(String),
    DismissNews,
    Search,
    Refresh,
    Install(String),
    Remove(String),
    Upgrade(String),
    UpgradeAll,
    Password,
    Unlock,
    CancelUnlock,
}

fn source() -> String {
    let configured = fs::read(SOURCES).ok().and_then(|d| {
        String::from_utf8_lossy(&d)
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .map(String::from)
    });
    let mut url = configured.unwrap_or_else(|| String::from(ecosystem::HATCH_REPO));
    if url.contains("@gateway")
        && let Some(info) = net::netinfo()
    {
        url = url.replace("@gateway", &format!("{}", info.gateway));
    }
    if !url.ends_with('/') {
        url.push('/');
    }
    url
}

fn parse_index(text: &str) -> Vec<Package> {
    let mut out = Vec::new();
    let mut cur: Option<Package> = None;
    for line in text.lines().skip(1) {
        let (key, value) = line.trim().split_once(' ').unwrap_or((line.trim(), ""));
        match key {
            "package" => {
                cur = Some(Package {
                    name: String::from(value),
                    version: String::new(),
                    description: String::new(),
                    title: String::new(),
                    icon: None,
                    size: 0,
                    brct: false,
                    about: String::new(),
                    notes: Vec::new(),
                    date: String::new(),
                    depends: Vec::new(),
                })
            }
            "end" => {
                if let Some(mut p) = cur.take() {
                    if p.title.is_empty() {
                        p.title = p.name.clone();
                    }
                    out.push(p);
                }
            }
            _ => {
                if let Some(p) = cur.as_mut() {
                    match key {
                        "version" => p.version = String::from(value),
                        "description" => p.description = String::from(value),
                        "title" => p.title = String::from(value),
                        "icon" => p.icon = Icon::from_name(value),
                        "size" => p.size = value.parse().unwrap_or(0),
                        "builder" => p.brct = value == "brct",
                        "about" => p.about = String::from(value),
                        "note" => p.notes.push(String::from(value)),
                        "date" => p.date = String::from(value),
                        "depends" => {
                            p.depends = value
                                .split([',', ' '])
                                .filter(|d| !d.is_empty())
                                .map(String::from)
                                .collect()
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

fn installed() -> Vec<(String, String)> {
    fs::read(INSTALLED)
        .ok()
        .map(|d| {
            String::from_utf8_lossy(&d)
                .lines()
                .filter_map(|l| l.trim().split_once(' '))
                .map(|(n, v)| (String::from(n), String::from(v)))
                .collect()
        })
        .unwrap_or_default()
}

fn newer(a: &str, b: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split(['.', '-'])
            .map(|p| p.parse().unwrap_or(0))
            .collect()
    };
    parse(a) > parse(b)
}

fn size_text(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{}.{} MB", bytes >> 20, ((bytes % (1 << 20)) * 10) >> 20)
    } else {
        format!("{} KB", bytes.div_ceil(1024))
    }
}

struct Store {
    window: Window,
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    platform: Platform,
    tab: Tab,
    packages: Vec<Package>,
    installed: Vec<(String, String)>,
    /// Diğer platformların uygulamaları (ilk kez başka bir panel açılınca alınır).
    remote: Vec<RemoteApp>,
    remote_loaded: bool,
    /// Uygulama simgeleri (ekosistemden PNG, 48u boyutunda): (kimlik, pikseller).
    remote_icons: Vec<(String, Vec<u32>)>,
    status: String,
    error: bool,
    search: ui::TextInput,
    search_focused: bool,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    /// Basılı tutulan denetim: işlem bırakınca yapılır, işaretçi dışarı çekilirse vazgeçilir.
    pressed: Option<Rect>,
    scroll: i32,
    max_scroll: i32,
    task: Option<(Task, String)>,
    /// Parola sorulurken bekleyen işlem.
    unlock: Option<(String, Vec<String>)>,
    password: ui::TextInput,
    /// Doğrulanmış yönetici parolası (yalnızca çocuk işlemleri yükseltmek için).
    granted: Option<String>,
    /// "brct ile yapıldı" rozetindeki logo (küçültülmüş): (genişlik, yükseklik, pikseller).
    brct_mark: Option<(i32, i32, Vec<u32>)>,
    /// Banna OS sistem güncellemesi (Store ve uygulamalar dahil sistem dosyaları).
    system: Option<SystemUpdate>,
    /// Açık uygulama sayfası ve kaydırması.
    detail: Option<Detail>,
    detail_scroll: i32,
    detail_max_scroll: i32,
    /// "Yenilikler" penceresi: (başlık, maddeler).
    whats_new: Option<Vec<(String, Vec<String>)>>,
    dirty: bool,
}

/// Depodaki imzalı sistem bildiriminin kurulu sistemden farkı.
struct SystemUpdate {
    version: String,
    notes: String,
    files: usize,
    size: usize,
    /// İndirilip hazırlandı: yeniden başlatınca kurulacak.
    ready: bool,
    date: String,
    /// Sürüm notları ve güncellenen yerleşik uygulamaların adları.
    changes: Vec<String>,
    apps: Vec<String>,
}

/// `/apps/<kimlik>/...` yolundan uygulamanın adı (kurulu app.conf'tan; yoksa kimlik).
fn app_name(id: &str) -> String {
    fs::read(&format!("/apps/{}/app.conf", id))
        .ok()
        .and_then(|d| {
            String::from_utf8_lossy(&d)
                .lines()
                .find_map(|l| l.strip_prefix("name=").map(|n| String::from(n.trim())))
        })
        .unwrap_or_else(|| String::from(id))
}

fn check_system() -> Option<SystemUpdate> {
    let m = banna::sysupdate::refresh(&source()).ok()?;
    let files = banna::sysupdate::changed(&m);
    if files.is_empty() {
        return None;
    }
    let mut apps: Vec<String> = Vec::new();
    for e in &files {
        if let Some(id) = e
            .path
            .strip_prefix("/apps/")
            .and_then(|r| r.split('/').next())
        {
            let name = app_name(id);
            if !apps.contains(&name) {
                apps.push(name);
            }
        }
    }
    Some(SystemUpdate {
        size: files.iter().map(|e| e.size).sum(),
        date: m.date.clone(),
        changes: m.changes.clone(),
        apps,
        files: files.len(),
        ready: banna::sysupdate::pending().as_deref() == Some(m.version.as_str()),
        version: m.version,
        notes: m.notes,
    })
}

const BRCT_MARK: &str = "/usr/share/brct/brct-mark.png";

fn load_brct_mark(height: u32) -> Option<(i32, i32, Vec<u32>)> {
    let image = banna::gfx::png::decode(&fs::read(BRCT_MARK).ok()?)?;
    let width = image.width * height / image.height.max(1);
    Some((width as i32, height as i32, image.scaled(width, height)))
}

impl Store {
    fn load_catalog(&mut self) {
        let url = format!("{}index", source());
        self.status = format!("Katalog alınıyor: {}", url);
        self.render();
        let signature = net::get(&format!("{}index.sig", source()))
            .ok()
            .filter(|r| r.status == 200)
            .map(|r| r.body);
        match net::get(&url) {
            Ok(r)
                if r.status == 200
                    && !signature
                        .as_ref()
                        .is_some_and(|sig| ecosystem::verify_repository(&r.body, sig)) =>
            {
                self.packages.clear();
                self.status =
                    String::from("Paket listesinin imzası doğrulanamadı; depo güvenilmiyor.");
                self.error = true;
            }
            Ok(r) if r.status == 200 => {
                self.packages = parse_index(&String::from_utf8_lossy(&r.body));
                self.status = format!("{} paket • {}", self.packages.len(), source());
                self.error = false;
            }
            Ok(r) => {
                self.status = format!("Depo yanıt vermedi (HTTP {}): {}", r.status, url);
                self.error = true;
            }
            Err(e) => {
                self.status = format!("Depoya ulaşılamadı: {}", e);
                self.error = true;
            }
        }
        self.installed = installed();
        self.system = check_system();
        self.dirty = true;
    }

    fn fetch_icon(&mut self, id: &str, url: &str) {
        if url.is_empty() || self.remote_icons.iter().any(|(i, _)| i == id) {
            return;
        }
        let Ok(r) = net::get(url) else { return };
        if r.status != 200 {
            return;
        }
        if let Some(img) = banna::gfx::png::decode(&r.body) {
            let size = (48 * self.u) as u32;
            let pixels =
                banna::wallpaper_art::scale_argb(&img.pixels, img.width, img.height, size, size);
            self.remote_icons.push((String::from(id), pixels));
        }
    }

    fn load_remote(&mut self) {
        self.status = String::from("Diğer platformların uygulamaları alınıyor…");
        self.error = false;
        self.render();
        self.remote.clear();
        match ecosystem::store_platforms() {
            Ok(Some(platforms)) => {
                for entry in &platforms {
                    let Some(platform) = Platform::from_id(entry.get("id").as_str()) else {
                        continue;
                    };
                    if platform == Platform::current() {
                        continue;
                    }
                    for app in entry.get("apps").items() {
                        let id = String::from(app.get("id").as_str());
                        self.remote.push(RemoteApp {
                            id: id.clone(),
                            name: String::from(app.get("name").as_str()),
                            tagline: String::from(app.get("tagline").as_str()),
                            version: String::from(app.get("version").as_str()),
                            size: app.get("size").as_i64().max(0) as u64,
                            platforms: alloc::vec![platform],
                            detail: String::from(app.get("detail").as_str()),
                            link: String::from(app.get("downloadUrl").as_str()),
                            icon: Icon::from_name(app.get("icon").as_str()),
                            description: String::from(app.get("description").as_str()),
                            notes: detail::split_notes(app.get("notes").as_str()),
                            date: String::from(app.get("pubDate").as_str()),
                        });
                        self.fetch_icon(&id, app.get("iconUrl").as_str());
                    }
                }
                self.finish_remote();
            }
            Ok(None) => self.load_remote_legacy(),
            Err(e) => {
                self.status = e.text();
                self.error = true;
                self.remote_loaded = true;
                self.dirty = true;
            }
        }
    }

    fn finish_remote(&mut self) {
        self.remote_loaded = true;
        if !self.error {
            self.status = format!("{} uygulama • diğer platformlar", self.remote.len());
        }
        self.dirty = true;
    }

    /// `/banna-store/platforms` olmayan sunucular: mobil katalog ve masaüstü güncelleme uçları.
    fn load_remote_legacy(&mut self) {
        match ecosystem::mobile_catalog() {
            Ok(apps) => {
                for app in &apps {
                    let id = String::from(app.get("id").as_str());
                    let mut platforms: Vec<Platform> = app
                        .get("platforms")
                        .items()
                        .iter()
                        .filter_map(|p| Platform::from_id(p.as_str()))
                        .collect();
                    if platforms.is_empty() {
                        platforms.push(Platform::Android);
                    }
                    let link = [app.get("downloadUrl").as_str(), app.get("apkUrl").as_str()]
                        .into_iter()
                        .find(|l| !l.is_empty())
                        .unwrap_or("");
                    self.remote.push(RemoteApp {
                        id: id.clone(),
                        name: String::from(app.get("name").as_str()),
                        tagline: String::from(app.get("tagline").as_str()),
                        version: String::from(app.get("version").as_str()),
                        size: app.get("size").as_i64().max(0) as u64,
                        platforms,
                        detail: String::new(),
                        link: String::from(link),
                        icon: None,
                        description: String::from(app.get("description").as_str()),
                        notes: detail::split_notes(app.get("notes").as_str()),
                        date: String::from(app.get("pubDate").as_str()),
                    });
                    self.fetch_icon(&id, app.get("iconUrl").as_str());
                }
            }
            Err(e) => {
                self.status = e.text();
                self.error = true;
            }
        }
        // Banna Panel'in masaüstü sürümleri: her platform için bir kayıt, mimariler ek bilgide.
        for (target, arch, platform, arch_label) in DESKTOP_TARGETS {
            let Ok(Some(release)) = ecosystem::desktop_release(target, arch) else {
                continue;
            };
            let id = format!("banna-panel-{}", platform.label().to_ascii_lowercase());
            if let Some(existing) = self.remote.iter_mut().find(|a| a.id == id) {
                existing.detail = format!("{}, {}", existing.detail, arch_label);
                continue;
            }
            self.remote.push(RemoteApp {
                id,
                name: String::from("Banna Panel"),
                tagline: String::from(
                    "Banna hesabın masaüstünde: bildirimler, Banna Mail, cihazlar",
                ),
                version: String::from(release.get("version").as_str()),
                size: 0,
                platforms: alloc::vec![platform],
                detail: String::from(arch_label),
                link: String::from(release.get("url").as_str()),
                icon: Some(Icon::Panel),
                description: String::new(),
                notes: detail::split_notes(release.get("notes").as_str()),
                date: String::from(release.get("pub_date").as_str()),
            });
        }
        self.finish_remote();
    }

    /// Seçili paneldeki (Banna OS dışı) uygulamalar, aramaya göre süzülmüş.
    fn remote_visible(&self) -> Vec<RemoteApp> {
        let q = self.search.to_lowercase();
        self.remote
            .iter()
            .filter(|a| a.platforms.contains(&self.platform))
            .filter(|a| {
                q.is_empty()
                    || a.name.to_lowercase().contains(&q)
                    || a.tagline.to_lowercase().contains(&q)
            })
            .cloned()
            .collect()
    }

    fn installed_version(&self, name: &str) -> Option<&str> {
        self.installed
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// hatch'i yönetici olarak çalıştırır (gerekirse önce parola sorar).
    /// hatch'i yönetici olarak çalıştırır. Store'un kendisi yetkisiz kalır: parolayla
    /// yalnızca çalıştırılan komut yükseltilir (parola Store açıkken bellekte tutulur).
    fn run_hatch(&mut self, label: &str, commands: Vec<String>) {
        if self.task.is_some() {
            return;
        }
        if process::uid() != 0 && self.granted.is_none() {
            self.unlock = Some((String::from(label), commands));
            self.password.clear();
            self.dirty = true;
            return;
        }
        // Depo adresi Store'unkiyle aynı olsun; ardından liste ve istenen işlem.
        let mut script = format!("hatch source {} && hatch update", source());
        for c in &commands {
            script.push_str(" && hatch ");
            script.push_str(c);
        }
        let args = ["sh", "-c", script.as_str()];
        let started = match &self.granted {
            Some(password) => Task::start_as_root("/bin/sh", &args, password),
            None => Task::start("/bin/sh", &args),
        };
        match started {
            Ok(task) => {
                self.status = format!("{}…", label);
                self.error = false;
                self.task = Some((task, String::from(label)));
            }
            Err(e) => {
                self.status = format!("hatch başlatılamadı ({})", e);
                self.error = true;
            }
        }
        self.dirty = true;
    }

    fn unlock(&mut self) {
        let Some((label, commands)) = self.unlock.take() else {
            return;
        };
        self.granted = Some(core::mem::take(&mut self.password));
        self.run_hatch(&label, commands);
        self.dirty = true;
    }

    /// Metin alanları basınca odaklanır; diğer denetimler basılı görünür ve bırakınca çalışır.
    fn mouse_down(&mut self, x: i32, y: i32) {
        match ui::hit_at(&self.hits, x, y) {
            Some(hit) if !matches!(hit.control, Control::Search | Control::Password) => {
                self.pressed = Some(hit.rect);
                self.hover = Some(hit.rect);
                self.dirty = true;
            }
            _ => self.press(x, y),
        }
    }

    fn mouse_up(&mut self, x: i32, y: i32) {
        let Some(pressed) = self.pressed.take() else {
            return;
        };
        self.dirty = true;
        if ui::hit_at(&self.hits, x, y).is_some_and(|h| h.rect == pressed) {
            self.press(x, y);
        }
    }

    /// Basılı denetim, işaretçi üzerindeyken küçülmüş çizilir.
    fn draw_pressed(&self, c: &mut banna::gfx::Canvas) {
        if let Some(r) = self.pressed.filter(|r| self.hover == Some(*r)) {
            ui::press_effect(c, r);
        }
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() else {
            self.search_focused = false;
            return;
        };
        self.search_focused = false;
        match hit.control {
            Control::Platform(p) => {
                self.platform = p;
                self.scroll = 0;
                if p != Platform::BannaOs && !self.remote_loaded {
                    self.load_remote();
                }
            }
            Control::Tab(t) => {
                self.tab = t;
                self.scroll = 0;
            }
            Control::CopyLink(link) => {
                banna::gui::clipboard::set(&link);
                self.status = format!("Bağlantı panoya kopyalandı: {}", link);
                self.error = false;
            }
            Control::Open(d) => {
                self.detail = Some(d);
                self.detail_scroll = 0;
            }
            Control::Back => self.detail = None,
            Control::Launch(name) => {
                if !detail::launch(&name) {
                    self.status = format!("{} açılamadı", name);
                    self.error = true;
                }
            }
            Control::DismissNews => {
                self.whats_new = None;
                detail::remember(&self.installed);
            }
            Control::Search => self.search_focused = true,
            Control::Refresh => {
                self.load_catalog();
                if self.platform != Platform::BannaOs {
                    self.load_remote();
                }
            }
            Control::Install(n) => self.run_hatch(
                &format!("{} kuruluyor", n),
                alloc::vec![format!("install {}", n)],
            ),
            Control::Remove(n) => self.run_hatch(
                &format!("{} kaldırılıyor", n),
                alloc::vec![format!("remove {}", n)],
            ),
            Control::Upgrade(n) => self.run_hatch(
                &format!("{} güncelleniyor", n),
                alloc::vec![format!("install {}", n)],
            ),
            Control::UpgradeAll => self.run_hatch(
                "Tüm paketler güncelleniyor",
                alloc::vec![String::from("-U")],
            ),
            Control::Password => {}
            Control::Unlock => self.unlock(),
            Control::CancelUnlock => {
                self.unlock = None;
                self.password.clear();
            }
        }
        self.dirty = true;
    }

    fn key(&mut self, key: u32) -> bool {
        if self.unlock.is_some() {
            match key {
                13 => self.unlock(),
                27 => self.unlock = None,
                k => {
                    self.password.key(k, 64);
                }
            }
            self.dirty = true;
            return true;
        }
        if key == 27 && self.whats_new.is_some() {
            self.whats_new = None;
            detail::remember(&self.installed);
            self.dirty = true;
            return true;
        }
        if key == 27 && self.detail.is_some() {
            self.detail = None;
            self.dirty = true;
            return true;
        }
        if key == 27 && !self.search_focused {
            return false;
        }
        if self.search_focused || (0x20..0xf700).contains(&key) {
            if key == 27 || key == 13 {
                self.search_focused = false;
            } else {
                self.search_focused = true;
                self.search.key(key, 40);
                self.scroll = 0;
            }
            self.dirty = true;
        }
        true
    }

    fn visible(&self) -> Vec<Package> {
        let q = self.search.to_lowercase();
        self.packages
            .iter()
            .filter(|p| {
                q.is_empty()
                    || p.name.to_lowercase().contains(&q)
                    || p.title.to_lowercase().contains(&q)
                    || p.description.to_lowercase().contains(&q)
            })
            .filter(|p| match self.tab {
                Tab::Discover => true,
                Tab::Installed => self.installed_version(&p.name).is_some(),
                Tab::Updates => self
                    .installed_version(&p.name)
                    .is_some_and(|v| newer(&p.version, v)),
            })
            .cloned()
            .collect()
    }

    fn render(&mut self) {
        self.dirty = false;
        if self.render_detail() {
            return;
        }
        let u = self.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let t = self.theme;
        let mut hits = Vec::new();
        let banna_panel = self.platform == Platform::BannaOs;
        let list = if banna_panel {
            self.visible()
        } else {
            Vec::new()
        };
        let remote = if banna_panel {
            Vec::new()
        } else {
            self.remote_visible()
        };
        let installed_list = self.installed.clone();
        let version_of = |name: &str| {
            installed_list
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, v)| v.clone())
        };
        let bottom;
        {
            // Çizim boyunca pencereye başka erişim yok: tuval pencereden ayrı ödünç alınır.
            let window: *mut Window = &mut self.window;
            let mut c = unsafe { (*window).canvas() };
            let f = &self.fonts;
            c.fill(Rect::new(0, 0, w, h), t.window_bg);

            // Başlık şeridi: logo, ad, arama, yenile.
            let header_h = 72 * u;
            c.fill(Rect::new(0, 0, w, header_h), t.sidebar);
            let logo = Rect::new(20 * u, 16 * u, 40 * u, 40 * u);
            icons::logo(&mut c, logo, f.title, t.accent, WHITE);
            f.heading
                .draw(&mut c, logo.right() + 14 * u, 18 * u, "Banna Store", t.text);
            let area = Rect::new(0, 0, w, h);
            let mut ui = Ui::new(&mut c, t, f, u, area, 0, &mut hits, self.hover);
            let search = Rect::new(
                w - 20 * u - 96 * u - 12 * u - 300 * u,
                18 * u,
                300 * u,
                36 * u,
            );
            ui.input_field(
                search,
                "Uygulama ara",
                &self.search,
                false,
                self.search_focused,
                Control::Search,
            );
            ui.secondary_button(
                Rect::new(w - 20 * u - 96 * u, 18 * u, 96 * u, 36 * u),
                "Yenile",
                Control::Refresh,
            );

            // Platform panelleri: bu cihazın sistemi başta ve işaretli.
            let mut x = 20 * u;
            let platforms_y = header_h + 14 * u;
            for p in Platform::ALL {
                let current = p == Platform::current();
                let label = if current {
                    format!("{} • Bu cihaz", p.label())
                } else {
                    String::from(p.label())
                };
                let tw = ui.f.body.measure(&label) + 28 * u;
                let r = Rect::new(x, platforms_y, tw, 34 * u);
                if self.platform == p {
                    icons::rounded_rect(ui.c, r, 17 * u, t.accent);
                } else if ui.hovered(r) {
                    icons::rounded_rect(ui.c, r, 17 * u, t.hover);
                } else {
                    icons::rounded_rect(ui.c, r, 17 * u, t.card);
                }
                ui.f.body.draw(
                    ui.c,
                    r.x + 14 * u,
                    r.y + (r.h - ui.f.body.ascent - ui.f.body.descent) / 2,
                    &label,
                    if self.platform == p { WHITE } else { t.text },
                );
                ui.hit(r, Control::Platform(p));
                x += tw + 8 * u;
            }

            // Banna OS panelinin sekmeleri.
            let tabs_y = platforms_y + 46 * u;
            let updates = self
                .packages
                .iter()
                .filter(|p| version_of(&p.name).is_some_and(|v| newer(&p.version, &v)))
                .count()
                + self.system.is_some() as usize;
            let list_top = if banna_panel {
                let mut x = 20 * u;
                let installed_count = self.installed.len();
                for (label, tab) in [
                    (String::from("Keşfet"), Tab::Discover),
                    (format!("Yüklü ({})", installed_count), Tab::Installed),
                    (format!("Güncellemeler ({})", updates), Tab::Updates),
                ] {
                    let tw = ui.f.small.measure(&label) + 24 * u;
                    let r = Rect::new(x, tabs_y, tw, 30 * u);
                    let selected = self.tab == tab;
                    if selected {
                        icons::rounded_rect(ui.c, r, 15 * u, with_alpha(t.accent, 60));
                    } else if ui.hovered(r) {
                        icons::rounded_rect(ui.c, r, 15 * u, t.hover);
                    }
                    ui.f.small.draw(
                        ui.c,
                        r.x + 12 * u,
                        r.y + (r.h - ui.f.small.ascent - ui.f.small.descent) / 2,
                        &label,
                        if selected { t.accent } else { t.text },
                    );
                    ui.hit(r, Control::Tab(tab));
                    x += tw + 6 * u;
                }
                if self.tab == Tab::Updates && updates > 0 {
                    ui.primary_button(
                        Rect::new(w - 20 * u - 180 * u, tabs_y - 2 * u, 180 * u, 34 * u),
                        "Hepsini güncelle",
                        Control::UpgradeAll,
                    );
                }
                tabs_y + 44 * u
            } else {
                tabs_y
            };

            // Liste.
            let status_h = 36 * u;
            let list_area = Rect::new(20 * u, list_top, w - 40 * u, h - list_top - status_h);
            ui.clip = list_area;
            ui.c.clip = list_area;
            ui.x = list_area.x;
            ui.w = list_area.w;
            ui.y = list_top - self.scroll;
            let row_h = 84 * u;
            if !banna_panel {
                let name = self.platform.label();
                ui.note(&format!(
                    "{} Bu cihaz Banna OS çalıştırıyor; bağlantıyı kopyalayıp o cihazda açın.",
                    self.platform.hint()
                ));
                if !self.remote_loaded {
                    ui.note("Yükleniyor…");
                } else if remote.is_empty() {
                    ui.y += 20 * u;
                    ui.note(&if self.search.is_empty() {
                        format!("Henüz {} için yayımlanmış bir Banna uygulaması yok.", name)
                    } else {
                        String::from("Eşleşen uygulama yok.")
                    });
                }
                for app in &remote {
                    let card = Rect::new(ui.x, ui.y, ui.w, row_h - 10 * u);
                    icons::rounded_rect(ui.c, card, 14 * u, t.card);
                    ui.hit(
                        card,
                        Control::Open(detail::remote_detail(&app.id, self.platform)),
                    );
                    let icon = Rect::new(card.x + 16 * u, card.y + 14 * u, 48 * u, 48 * u);
                    match (
                        app.icon,
                        self.remote_icons.iter().find(|(id, _)| *id == app.id),
                    ) {
                        (Some(i), _) => icons::draw(ui.c, icon, i),
                        (None, Some((_, pixels))) => {
                            ui.c.blend_image(pixels, icon.w, icon.h, icon.x, icon.y)
                        }
                        (None, None) => icons::letter(ui.c, icon, &app.name, ui.f.title),
                    }
                    let tx = icon.right() + 16 * u;
                    ui.f.title
                        .draw(ui.c, tx, card.y + 14 * u, &app.name, t.text);
                    let mut meta = String::from(app.version.as_str());
                    if app.size > 0 {
                        meta = format!("{} • {}", meta, size_text(app.size as usize));
                    }
                    if !app.detail.is_empty() {
                        meta = format!("{} • {}", meta, app.detail);
                    }
                    meta = format!("{} • {}", meta, name);
                    ui.f.small.draw(ui.c, tx, card.y + 38 * u, &meta, t.muted);
                    ui.f.small.draw_clipped(
                        ui.c,
                        tx,
                        card.y + 56 * u,
                        &app.tagline,
                        card.w - (tx - card.x) - 260 * u,
                        t.muted,
                    );
                    let bw = 170 * u;
                    let b = Rect::new(
                        card.right() - 16 * u - bw,
                        card.y + (card.h - 34 * u) / 2,
                        bw,
                        34 * u,
                    );
                    if !app.link.is_empty() {
                        ui.secondary_button(
                            b,
                            "Bağlantıyı kopyala",
                            Control::CopyLink(app.link.clone()),
                        );
                    }
                    let mark = "Bu cihazla uyumlu değil";
                    let mw = ui.f.small.measure(mark);
                    ui.f.small
                        .draw(ui.c, b.x - 14 * u - mw, b.y + 10 * u, mark, t.muted);
                    ui.y += row_h;
                }
            } else if self.tab == Tab::Updates
                && let Some(sys) = &self.system
            {
                // Sistem güncellemesi: Store, B-Player ve diğer yerleşik uygulamalar bununla gelir.
                let card = Rect::new(ui.x, ui.y, ui.w, row_h - 10 * u);
                icons::rounded_rect(ui.c, card, 14 * u, t.card);
                ui.hit(card, Control::Open(Detail::System));
                let icon = Rect::new(card.x + 16 * u, card.y + 14 * u, 48 * u, 48 * u);
                icons::logo(ui.c, icon, ui.f.title, t.accent, WHITE);
                let tx = icon.right() + 16 * u;
                ui.f.title.draw(
                    ui.c,
                    tx,
                    card.y + 14 * u,
                    "Banna OS sistem güncellemesi",
                    t.text,
                );
                let meta = format!(
                    "{} • {} dosya • {}",
                    sys.version,
                    sys.files,
                    size_text(sys.size)
                );
                ui.f.small.draw(ui.c, tx, card.y + 38 * u, &meta, t.muted);
                ui.f.small.draw_clipped(
                    ui.c,
                    tx,
                    card.y + 56 * u,
                    if sys.notes.is_empty() {
                        "Çekirdek, Banna Store ve yerleşik uygulamalar"
                    } else {
                        &sys.notes
                    },
                    card.w - (tx - card.x) - 260 * u,
                    t.muted,
                );
                let bw = 110 * u;
                let b = Rect::new(
                    card.right() - 16 * u - bw,
                    card.y + (card.h - 34 * u) / 2,
                    bw,
                    34 * u,
                );
                if sys.ready {
                    let mark = "Yeniden başlatınca kurulacak";
                    let mw = ui.f.small.measure(mark);
                    ui.f.small
                        .draw(ui.c, b.right() - mw, b.y + 10 * u, mark, t.accent);
                } else {
                    ui.primary_button(b, "Güncelle", Control::UpgradeAll);
                }
                ui.y += row_h;
            } else if list.is_empty() {
                ui.y += 30 * u;
                ui.note(match self.tab {
                    Tab::Installed => "Henüz Banna Store'dan kurulmuş paket yok.",
                    Tab::Updates => "Tüm paketler güncel.",
                    _ if self.error => {
                        "Katalog alınamadı. Ağ bağlantısını kontrol edip Yenile'ye basın."
                    }
                    _ => "Eşleşen paket yok.",
                });
            }
            for p in &list {
                let card = Rect::new(ui.x, ui.y, ui.w, row_h - 10 * u);
                icons::rounded_rect(ui.c, card, 14 * u, t.card);
                ui.hit(card, Control::Open(Detail::Package(p.name.clone())));
                let icon = Rect::new(card.x + 16 * u, card.y + 14 * u, 48 * u, 48 * u);
                match p.icon {
                    Some(i) => icons::draw(ui.c, icon, i),
                    None => icons::letter(ui.c, icon, &p.title, ui.f.title),
                }
                let tx = icon.right() + 16 * u;
                let title_w = ui.f.title.draw(ui.c, tx, card.y + 14 * u, &p.title, t.text) - tx;
                if p.brct
                    && let Some((mw, mh, pixels)) = &self.brct_mark
                {
                    let label = "ile yapıldı";
                    let lw = ui.f.small.measure(label);
                    let pill = Rect::new(
                        tx + title_w + 12 * u,
                        card.y + 12 * u,
                        mw + lw + 26 * u,
                        mh + 8 * u,
                    );
                    icons::rounded_rect(ui.c, pill, pill.h / 2, WHITE);
                    ui.c.blend_image(pixels, *mw, *mh, pill.x + 10 * u, pill.y + 4 * u);
                    ui.f.small.draw(
                        ui.c,
                        pill.x + 16 * u + mw,
                        pill.y + (pill.h - ui.f.small.ascent - ui.f.small.descent) / 2,
                        label,
                        banna::gfx::rgb(0x1d, 0x3f, 0x66),
                    );
                }
                let mut meta = format!("{} • {}", p.version, size_text(p.size));
                if p.title != p.name {
                    meta = format!("{} • {}", p.name, meta);
                }
                ui.f.small.draw(ui.c, tx, card.y + 38 * u, &meta, t.muted);
                ui.f.small.draw_clipped(
                    ui.c,
                    tx,
                    card.y + 56 * u,
                    &p.description,
                    card.w - (tx - card.x) - 260 * u,
                    t.muted,
                );
                let bw = 110 * u;
                let b = Rect::new(
                    card.right() - 16 * u - bw,
                    card.y + (card.h - 34 * u) / 2,
                    bw,
                    34 * u,
                );
                match version_of(&p.name) {
                    None => ui.primary_button(b, "Yükle", Control::Install(p.name.clone())),
                    Some(v) if newer(&p.version, &v) => {
                        ui.primary_button(b, "Güncelle", Control::Upgrade(p.name.clone()));
                        ui.secondary_button(
                            b.offset(-(bw + 10 * u), 0),
                            "Kaldır",
                            Control::Remove(p.name.clone()),
                        );
                    }
                    Some(_) => {
                        ui.secondary_button(b, "Kaldır", Control::Remove(p.name.clone()));
                        let mark = "Yüklü";
                        let mw = ui.f.small.measure(mark);
                        ui.f.small
                            .draw(ui.c, b.x - 14 * u - mw, b.y + 10 * u, mark, t.accent);
                    }
                }
                ui.y += row_h;
            }
            bottom = ui.y + self.scroll - list_top;
            let list_h = list_area.h;

            c.clip = Rect::new(0, 0, w, h);
            self.draw_status(&mut c, w, h);
            self.max_scroll = (bottom - list_h).max(0);
            self.render_whats_new(&mut c, &mut hits);
            self.draw_unlock(&mut c, &mut hits);
            self.draw_pressed(&mut c);
        }
        self.window.present();
        self.hits = hits;
    }

    fn draw_status(&self, c: &mut banna::gfx::Canvas, w: i32, h: i32) {
        let u = self.u;
        let t = self.theme;
        let f = &self.fonts;
        let status_h = 36 * u;
        let bar = Rect::new(0, h - status_h, w, status_h);
        c.fill(bar, t.sidebar);
        let color = if self.error {
            ui::DANGER
        } else if self.task.is_some() {
            t.accent
        } else {
            t.muted
        };
        f.small.draw_clipped(
            c,
            20 * u,
            bar.y + (status_h - f.small.ascent - f.small.descent) / 2,
            &self.status,
            w - 40 * u,
            color,
        );
    }

    fn draw_unlock(&self, c: &mut banna::gfx::Canvas, hits: &mut Vec<Hit<Control>>) {
        let u = self.u;
        let t = self.theme;
        let f = &self.fonts;
        let (w, h) = (c.width, c.height);
        // Parola penceresi.
        if let Some((label, _)) = &self.unlock {
            c.fill(Rect::new(0, 0, w, h), with_alpha(0, 120));
            let card = Rect::new(w / 2 - 200 * u, h / 2 - 110 * u, 400 * u, 220 * u);
            icons::rounded_rect(c, card, 16 * u, t.window_bg);
            let mut ui = Ui::new(c, t, f, u, card, card.y, hits, self.hover);
            ui.f.title.draw(
                ui.c,
                card.x + 24 * u,
                card.y + 22 * u,
                "Yönetici izni gerekiyor",
                t.text,
            );
            ui.f.small
                .draw(ui.c, card.x + 24 * u, card.y + 50 * u, label, t.muted);
            let field = Rect::new(card.x + 24 * u, card.y + 82 * u, card.w - 48 * u, 40 * u);
            ui.input_field(
                field,
                "Parola",
                &self.password,
                true,
                true,
                Control::Password,
            );
            let bw = 120 * u;
            ui.secondary_button(
                Rect::new(
                    card.right() - 24 * u - 2 * bw - 10 * u,
                    card.bottom() - 60 * u,
                    bw,
                    38 * u,
                ),
                "Vazgeç",
                Control::CancelUnlock,
            );
            ui.primary_button(
                Rect::new(
                    card.right() - 24 * u - bw,
                    card.bottom() - 60 * u,
                    bw,
                    38 * u,
                ),
                "Onayla",
                Control::Unlock,
            );
        }
    }

    fn poll_task(&mut self) {
        let Some((task, label)) = self.task.as_mut() else {
            return;
        };
        if !task.pump() {
            return;
        }
        let line = String::from(task.last_line());
        if !line.is_empty() {
            self.status = line;
        }
        if let Some(code) = task.exit {
            if code == 126 {
                // Parola yanlış ya da yönetici değil: bir dahaki işlemde yeniden sor.
                self.granted = None;
            }
            if code == 0 {
                self.status = format!(
                    "{}: tamamlandı",
                    label
                        .trim_end_matches(" kuruluyor")
                        .trim_end_matches(" kaldırılıyor")
                );
                self.error = false;
            } else {
                self.error = true;
            }
            banna::println!("store: task finished ({})", code);
            self.task = None;
            self.installed = installed();
            self.system = check_system();
        }
        self.dirty = true;
    }
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let window = match Window::new("Banna Store", (900 * u) as usize, (600 * u) as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("store: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let theme = desktop::query()
        .map(|i| Theme::from(&Prefs::parse(&i.text())))
        .unwrap_or_else(|| Theme::from(&Prefs::default()));
    let mut store = Store {
        window,
        fonts: ui::fonts(),
        u,
        theme,
        platform: Platform::current(),
        tab: Tab::Discover,
        packages: Vec::new(),
        installed: Vec::new(),
        remote: Vec::new(),
        remote_loaded: false,
        remote_icons: Vec::new(),
        system: None,
        detail: None,
        detail_scroll: 0,
        detail_max_scroll: 0,
        whats_new: None,
        status: String::new(),
        error: false,
        search: ui::TextInput::default(),
        search_focused: false,
        hits: Vec::new(),
        hover: None,
        pressed: None,
        scroll: 0,
        max_scroll: 0,
        task: None,
        unlock: None,
        password: ui::TextInput::default(),
        granted: None,
        brct_mark: load_brct_mark((18 * u) as u32),
        dirty: true,
    };
    banna::println!("store: ready");
    store.load_catalog();
    store.collect_whats_new();
    if let Some(name) = args.iter().skip_while(|a| **a != "--install").nth(1) {
        store.press_install(name);
    }
    loop {
        if store.dirty {
            store.render();
        }
        let mut entries = alloc::vec![PollEntry::window(store.window.id())];
        if let Some((task, _)) = &store.task {
            entries.push(PollEntry::fd(task.fd()));
        }
        io::poll(
            &mut entries,
            Some(if store.task.is_some() { 200 } else { 10_000 }),
        );
        store.poll_task();
        while let Some(event) = store.window.event(0) {
            match event.kind {
                EVENT_CLOSE => return 0,
                EVENT_RESIZE => {
                    store.window.resize(event.x as usize, event.y as usize);
                    store.dirty = true;
                }
                EVENT_MOUSE_DOWN => store.mouse_down(event.x, event.y),
                EVENT_MOUSE_UP => store.mouse_up(event.x, event.y),
                EVENT_MOUSE_MOVE => {
                    let hovered = ui::hit_at(&store.hits, event.x, event.y).map(|h| h.rect);
                    if hovered != store.hover {
                        store.hover = hovered;
                        store.dirty = true;
                    }
                }
                EVENT_WHEEL if store.detail.is_some() => {
                    store.detail_scroll = (store.detail_scroll + event.key as i32 * 60 * u)
                        .clamp(0, store.detail_max_scroll);
                    store.dirty = true;
                }
                EVENT_WHEEL => {
                    store.scroll =
                        (store.scroll + event.key as i32 * 60 * u).clamp(0, store.max_scroll);
                    store.dirty = true;
                }
                EVENT_KEY => {
                    if !store.key(event.key) {
                        return 0;
                    }
                }
                _ => {}
            }
        }
        let _ = time::uptime_ms();
    }
}

impl Store {
    fn press_install(&mut self, name: &str) {
        self.run_hatch(
            &format!("{} kuruluyor", name),
            alloc::vec![format!("install {}", name)],
        );
    }
}
