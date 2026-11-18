//! Uygulama kataloğu: `/apps/<kimlik>/app.conf` tanım dosyalarından okunur.
//!
//! Her uygulama ring 3 programıdır ve kendi klasöründe durur (`/apps/<kimlik>/<kimlik>`);
//! çekirdek yalnızca listeyi okur, başlatır ve pencereleri uygulamalarla eşler.
//!
//! ```text
//! name=Ayarlar
//! description=Sistemi özelleştir
//! icon=settings            # ya da icon=image:icon.png (klasördeki PNG)
//! exec=settings            # göreli yol /apps/<kimlik>/ altındadır
//! title=Ayarlar            # pencere başlığı (açık pencereyi tanımak için)
//! dock=2                   # dock'a sabitlenme sırası (yoksa sabitlenmez)
//! order=2                  # başlatıcıdaki sıra
//! terminal=1               # komut satırı programı: terminal penceresinde çalışır
//! single=1                 # tek pencere: yeniden açınca var olan öne gelir
//! requires=/bin/calc       # bu dosya yoksa uygulama gösterilmez
//! hidden=1                 # başlatıcıda ve dock'ta gösterilmez (ör. Nova, çekirdek başlatır)
//! ```

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use alloc::sync::Arc;

use super::icons::Icon;
use super::{Kind, Window};
use crate::sync::SpinLock;

pub const DIR: &str = "/apps";

#[derive(Clone, Debug)]
pub struct App {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: Option<Icon>,
    pub image: Option<Arc<ImageIcon>>,
    pub exec: String,
    pub title: String,
    pub dock: Option<u32>,
    pub order: u32,
    pub terminal: bool,
    /// Tek pencereli: yeniden açmak var olan pencereyi öne getirir.
    pub single: bool,
    pub requires: Option<String>,
    pub hidden: bool,
    /// Dock sağ tık menüsündeki eylemler: `action=Etiket|argümanlar` (argümansız = yeni pencere).
    pub actions: Vec<(String, Vec<String>)>,
    /// `requires` dosyası var mı (taramada bakılır; çizim sırasında diske gidilmez).
    present: bool,
    /// app.conf metni: değişmediyse yeniden taramada simge tekrar okunmaz.
    source: String,
}

impl App {
    fn parse(id: &str, text: &str) -> Self {
        let mut app = App {
            id: String::from(id),
            name: String::from(id),
            description: String::new(),
            icon: None,
            image: None,
            exec: format!("{}/{}/{}", DIR, id, id),
            title: String::new(),
            dock: None,
            order: 100,
            terminal: false,
            single: false,
            requires: None,
            hidden: false,
            actions: Vec::new(),
            present: true,
            source: String::from(text),
        };
        let mut exec: Option<String> = None;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "name" => app.name = value.into(),
                "description" => app.description = value.into(),
                "icon" => match value.strip_prefix("image:") {
                    Some(file) if !file.contains("..") => {
                        app.image = crate::fs::read_all(&format!("{}/{}/{}", DIR, id, file))
                            .ok()
                            .and_then(|data| ImageIcon::decode(&data))
                            .map(Arc::new);
                    }
                    _ => app.icon = Icon::from_name(value),
                },
                "exec" => exec = Some(value.into()),
                "title" => app.title = value.into(),
                "dock" => app.dock = value.parse().ok(),
                "order" => app.order = value.parse().unwrap_or(100),
                "terminal" => app.terminal = value == "1",
                "single" => app.single = value == "1",
                "requires" => app.requires = Some(value.into()),
                "hidden" => app.hidden = value == "1",
                "action" => {
                    let (label, args) = value.split_once('|').unwrap_or((value, ""));
                    let args = args.split_whitespace().map(String::from).collect();
                    app.actions.push((String::from(label.trim()), args));
                }
                _ => {}
            }
        }
        // Terminal programları PATH'te aranır; diğerlerinin göreli yolu kendi klasörüdür.
        match exec {
            Some(e) if app.terminal || e.starts_with('/') => app.exec = e,
            Some(e) => app.exec = format!("{}/{}/{}", DIR, id, e),
            None => {}
        }
        if app.title.is_empty() {
            app.title = app.name.clone();
        }
        app
    }

    fn available(&self) -> bool {
        self.present
    }
}

/// PNG'den gelen uygulama simgesi; son çizilen boyuttaki küçültülmüş hâli saklanır.
pub struct ImageIcon {
    image: crate::gfx::png::Image,
    scaled: SpinLock<Option<(u32, u32, Vec<u32>)>>,
}

impl core::fmt::Debug for ImageIcon {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "ImageIcon({}x{})", self.image.width, self.image.height)
    }
}

impl ImageIcon {
    fn decode(data: &[u8]) -> Option<Self> {
        let image = crate::gfx::png::decode(data)?;
        (image.width > 0 && image.height > 0).then(|| Self {
            image,
            scaled: SpinLock::new(None),
        })
    }

    fn draw(&self, canvas: &mut crate::gfx::Canvas, rect: crate::gfx::Rect) {
        let (w, h) = super::icons::image_fit(rect, self.image.width, self.image.height);
        let mut cache = self.scaled.lock();
        if !matches!(&*cache, Some((cw, ch, _)) if (*cw, *ch) == (w, h)) {
            *cache = Some((w, h, self.image.scaled(w, h)));
        }
        if let Some((_, _, pixels)) = &*cache {
            super::icons::image(canvas, rect, pixels, w, h);
        }
    }
}

/// Uygulamanın simgesi: PNG, yerleşik çizim ya da adın baş harfi.
pub fn draw_icon(
    canvas: &mut crate::gfx::Canvas,
    rect: crate::gfx::Rect,
    app: Option<&App>,
    label: &str,
    font: &crate::gfx::text::Font,
) {
    match app {
        Some(App {
            image: Some(image), ..
        }) => image.draw(canvas, rect),
        Some(App { icon: Some(i), .. }) => super::icons::draw(canvas, rect, *i),
        _ => super::icons::letter(canvas, rect, label, font),
    }
}

/// Çizilmiş simge katmanları: (uygulama, boyut) → 0xAARRGGBB pikseller.
type IconLayer = (String, i32, Arc<Vec<u32>>);
static ICON_CACHE: SpinLock<Vec<IconLayer>> = SpinLock::new(Vec::new());

/// Simgenin `size` boyutlu katmanı (önbellekten ya da bir kez çizilerek). Saydamlık,
/// simgenin siyah ve beyaz zemine çizilmiş hâllerinin farkından çıkarılır.
pub fn icon_layer(app: &App, size: i32, font: &crate::gfx::text::Font) -> Arc<Vec<u32>> {
    let cached = ICON_CACHE
        .lock()
        .iter()
        .find(|(id, s, _)| *id == app.id && *s == size)
        .map(|(_, _, layer)| layer.clone());
    if let Some(layer) = cached {
        return layer;
    }
    let square = crate::gfx::Rect::new(0, 0, size, size);
    let mut black = crate::gfx::Surface::new(size, size, 0);
    let mut white = crate::gfx::Surface::new(size, size, 0xff_ffff);
    draw_icon(&mut black.canvas(), square, Some(app), &app.name, font);
    draw_icon(&mut white.canvas(), square, Some(app), &app.name, font);
    let layer: Arc<Vec<u32>> = Arc::new(
        black
            .pixels
            .iter()
            .zip(&white.pixels)
            .map(|(&b, &w)| {
                let a = 255 - ((w & 0xff) - (b & 0xff)).min(255);
                if a == 0 {
                    return 0;
                }
                let un = |c: u32| ((c * 255) / a).min(255);
                a << 24 | un(b >> 16 & 0xff) << 16 | un(b >> 8 & 0xff) << 8 | un(b & 0xff)
            })
            .collect(),
    );
    let mut cache = ICON_CACHE.lock();
    if !cache.iter().any(|(id, s, _)| *id == app.id && *s == size) {
        // Dock büyütmesi her simge için ~25 boyut kullanır.
        if cache.len() >= 384 {
            cache.remove(0);
        }
        cache.push((app.id.clone(), size, layer.clone()));
    }
    layer
}

pub fn clear_icon_cache() {
    ICON_CACHE.lock().clear();
}

/// Simgeyi önbellekteki katmanından bindirir (kilit çizim sırasında tutulmaz).
pub fn draw_icon_cached(
    canvas: &mut crate::gfx::Canvas,
    rect: crate::gfx::Rect,
    app: &App,
    font: &crate::gfx::text::Font,
) {
    let size = rect.w;
    let layer = icon_layer(app, size, font);
    canvas.blend_image(&layer, size, size, rect.x, rect.y);
}

static CATALOG: SpinLock<Vec<App>> = SpinLock::new(Vec::new());
static CHANGED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static RELOADING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Kataloğu arka planda yeniden tarar (disk okuması compositor'ı bekletmesin); liste
/// değiştiyse `take_changed` true döner.
pub fn reload_async() {
    use core::sync::atomic::Ordering;
    if RELOADING.swap(true, Ordering::AcqRel) {
        return;
    }
    crate::sched::spawn_detached("apps-reload", || {
        reload();
        RELOADING.store(false, Ordering::Release);
        super::wake();
        0
    });
}

pub fn take_changed() -> bool {
    CHANGED.swap(false, core::sync::atomic::Ordering::AcqRel)
}

/// `/apps` klasörünü yeniden tarar (masaüstü açılışında ve başlatıcı açılırken).
pub fn reload() {
    let previous = CATALOG.lock().clone();
    let mut apps = Vec::new();
    if let Ok(entries) = crate::fs::read_dir(DIR) {
        for e in entries {
            if e.kind != crate::fs::Kind::Directory || e.name.starts_with('.') {
                continue;
            }
            if let Ok(data) = crate::fs::read_all(&format!("{}/{}/app.conf", DIR, e.name)) {
                let text = String::from_utf8_lossy(&data);
                let mut app = previous
                    .iter()
                    .find(|a| a.id == e.name && a.source == text)
                    .cloned()
                    .unwrap_or_else(|| App::parse(&e.name, &text));
                app.present = app
                    .requires
                    .as_ref()
                    .is_none_or(|path| crate::fs::stat(path).is_ok());
                // Terminal uygulamalarının komutu, diğerlerinin programı bulunmalı.
                if app.terminal || crate::fs::stat(&app.exec).is_ok() {
                    apps.push(app);
                }
            }
        }
    }
    apps.sort_by(|a, b| a.order.cmp(&b.order).then(a.name.cmp(&b.name)));
    let count = apps.len();
    let mut catalog = CATALOG.lock();
    let same = catalog.len() == apps.len()
        && catalog.iter().zip(&apps).all(|(a, b)| {
            a.id == b.id && a.name == b.name && a.dock == b.dock && a.present == b.present
        });
    *catalog = apps;
    drop(catalog);
    if !same {
        ICON_CACHE.lock().clear();
        CHANGED.store(true, core::sync::atomic::Ordering::Release);
    }
    crate::log!("gui: {} app(s) in {}", count, DIR);
}

pub fn get(id: &str) -> Option<App> {
    CATALOG.lock().iter().find(|a| a.id == id).cloned()
}

/// Başlatıcıda gösterilecek uygulamalar.
pub fn installed() -> Vec<App> {
    CATALOG
        .lock()
        .iter()
        .filter(|a| a.available() && !a.hidden)
        .cloned()
        .collect()
}

/// Dock'a sabitlenmiş uygulamalar: app.conf `dock=` sırası, kullanıcının kaldırdıkları
/// çıkarılıp eklediklerinin (`add`, virgülle) sonuna konmasıyla.
pub fn pinned(add: &str, remove: &str) -> Vec<App> {
    let listed = |list: &str, id: &str| list.split(',').any(|x| x.trim() == id);
    let catalog = CATALOG.lock();
    let mut list: Vec<App> = catalog
        .iter()
        .filter(|a| a.dock.is_some() && !listed(remove, &a.id))
        .cloned()
        .collect();
    list.sort_by_key(|a| a.dock);
    for id in add.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        if !list.iter().any(|a| a.id == id)
            && let Some(app) = catalog.iter().find(|a| a.id == id)
        {
            list.push(app.clone());
        }
    }
    list
}

/// Uygulamayı ek argümanlarla yeni pencerede başlatır.
pub fn spawn_with(id: &str, extra: Vec<String>) {
    let id = String::from(id);
    crate::sched::spawn_detached("launcher", move || {
        let extra: Vec<&str> = extra.iter().map(String::as_str).collect();
        run(&id, &extra)
    });
}

/// Başlatıcıdan açılan işlemler: pid → uygulama kimliği.
static LAUNCHED: SpinLock<Vec<(u64, String)>> = SpinLock::new(Vec::new());

pub fn app_of(window: &Window) -> Option<String> {
    let Kind::App(pid) = window.kind;
    if let Some((_, id)) = LAUNCHED.lock().iter().find(|(p, _)| *p == pid) {
        return Some(id.clone());
    }
    CATALOG
        .lock()
        .iter()
        .find(|a| a.title == window.title || a.name == window.title)
        .map(|a| a.id.clone())
}

pub fn forget(pid: u64) {
    LAUNCHED.lock().retain(|(p, _)| *p != pid);
}

/// Uygulamayı oturum kullanıcısı adına başlatır (disk okuması ayrı thread'de).
pub fn spawn(id: &str) {
    let id = String::from(id);
    crate::sched::spawn_detached("launcher", move || run(&id, &[]));
}

/// Uygulamayı bu thread'de başlatır: işlem kuruldu mu.
fn run(id: &str, extra: &[&str]) -> i64 {
    let Some(app) = get(id) else {
        crate::log!("gui: no app '{}' in {}", id, DIR);
        return -1;
    };
    let (path, mut args) = if app.terminal {
        let terminal = get("terminal")
            .map(|t| t.exec)
            .unwrap_or_else(|| format!("{}/terminal/terminal", DIR));
        (
            terminal,
            alloc::vec![
                String::from("terminal"),
                String::from("-t"),
                app.title.clone(),
                app.exec.clone(),
            ],
        )
    } else {
        let name = String::from(app.exec.rsplit('/').next().unwrap_or(&app.exec));
        (app.exec.clone(), alloc::vec![name])
    };
    args.extend(extra.iter().map(|a| String::from(*a)));
    match crate::kernel::elf::spawn(&path, &args) {
        Ok((process, _)) => {
            LAUNCHED.lock().push((process.pid, app.id.clone()));
            crate::log!("gui: launched {}", app.name);
            0
        }
        Err(e) => {
            crate::log!("gui: could not start {}: {}", path, e.message());
            -1
        }
    }
}

/// Oturum açıldı: terminal başlar; kullanıcının ilk oturumuysa ardından kurulum
/// sihirbazı açılır (en üstte kalsın diye biraz sonra); bekleyen hata raporları yüklenir.
pub fn start_session(home: String) {
    crate::sched::spawn_detached("session", move || {
        run("terminal", &[]);
        let marker = format!("{}/.config/banna/welcome", home);
        if crate::fs::stat(&marker).is_err() && get("setup").is_some() {
            crate::sched::sleep_ms(400);
            run("setup", &[]);
        }
        // Hata raporlama: önceki çökme raporları Banna hesabına gönderilir.
        if crate::fs::stat("/bin/banna-crash").is_ok() {
            let args = [String::from("banna-crash"), String::from("upload")];
            if let Err(e) = crate::kernel::elf::spawn("/bin/banna-crash", &args) {
                crate::log!("gui: banna-crash failed: {}", e.message());
            }
        }
        0
    });
}
