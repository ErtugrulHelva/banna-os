//! Masaüstü: olay güdümlü compositor, pencereler, panel, başlatıcı, dock ve Ayarlar.
//!
//! - Her pencerenin içeriği kendi yüzeyinde önbelleklenir; sahne yalnızca
//!   hasar gören dikdörtgenlerde ve yüzeylerden kopyalanarak birleştirilir.
//! - Opak bir pencerenin tamamen örttüğü bölgede altındakiler hiç çizilmez.
//! - İmleç ekran katmanında bindirilir; hareketi sahneyi yeniden çizdirmez.
//! - İş yokken compositor uyur; girdi, konsol çıktısı, uygulamalar ve süren
//!   animasyonlar uyandırır. Kare hızı 60 ile sınırlıdır.

pub mod app;
pub mod apps;
pub mod control;
mod desk;
mod draw;
/// Uygulamalarla ortak (`banna::icons`); bazı şekilleri yalnızca uygulamalar kullanır.
#[allow(dead_code)]
pub mod icons;
pub mod launcher;
pub mod login;
mod menu;
pub mod prefs;
pub mod theme;
pub mod wallpaper;
#[allow(dead_code)]
pub mod wallpaper_art;

use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::arch::{pit, tsc};
use crate::drivers::{audio, console, keyboard, mouse};
use crate::gfx::{Damage, Rect, Surface, display};
use crate::kernel::users;
use crate::sched::{self, Completion, Mutex, WaitQueue};
use crate::sync::SpinLock;
use control::{Action, Change, Pref};
use prefs::{Prefs, Theme};

/// Ekran yüksekliğine göre seçilen arayüz ölçüleri.
#[derive(Clone, Copy)]
pub struct Metrics {
    pub panel_h: i32,
    pub title_h: i32,
    pub radius: i32,
    pub shadow: i32,
    pub close: i32,
    pub unit: i32,
}

const SMALL: Metrics = Metrics {
    panel_h: 30,
    title_h: 32,
    radius: 12,
    shadow: 18,
    close: 12,
    unit: 1,
};

const LARGE: Metrics = Metrics {
    panel_h: 40,
    title_h: 42,
    radius: 20,
    shadow: 28,
    close: 16,
    unit: 2,
};

static LARGE_UI: AtomicBool = AtomicBool::new(false);

pub fn m() -> Metrics {
    if LARGE_UI.load(Ordering::Relaxed) {
        LARGE
    } else {
        SMALL
    }
}
const TERMINAL_COLUMNS: usize = 96;
const TERMINAL_ROWS: usize = 28;

static RUNNING: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
static PENDING: AtomicBool = AtomicBool::new(false);
static WAKE: WaitQueue = WaitQueue::new();
static FPS: AtomicU64 = AtomicU64::new(0);
const BACKDROP_BLUR: i32 = 56;

static FRAME_TIME_US: AtomicU64 = AtomicU64::new(0);
static FRAMES: AtomicU64 = AtomicU64::new(0);
static SCENE_PIXELS: AtomicU64 = AtomicU64::new(0);
static CURSOR_ONLY: AtomicU64 = AtomicU64::new(0);
static COMPLETION: SpinLock<Option<Arc<Completion>>> = SpinLock::new(None);
pub(crate) static DESKTOP: Mutex<Option<Desktop>> = Mutex::new(None);

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    /// Ring 3 uygulamasının penceresi (pid).
    App(u64),
}

pub struct Window {
    pub id: u64,
    pub kind: Kind,
    pub title: String,
    pub frame: Rect,
    /// Büyütülmüşse önceki konum ve istemci boyutu.
    pub restore: Option<(i32, i32, i32, i32)>,
    pub visible: bool,
    pub surface: Surface,
    pub channel: Option<Arc<app::Channel>>,
    /// Ait olduğu sanal masaüstü (0..DESKTOPS).
    pub desktop: u8,
    /// Başka masaüstüne geçildiği için gizlendi (simge durumuna küçültülmüş değil).
    pub parked: bool,
    /// Ekranın tamamına ya da bir yarısına yaslandıysa (eski boyut `restore`'da).
    pub snapped: Option<Snap>,
}

/// Pencere yaslama: başlıktan sürükleyip ekranın üst kenarına (tam ekran), sol ya da sağ
/// kenarına (yarım ekran) bırakınca.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Snap {
    Maximize,
    Left,
    Right,
}

/// Sanal masaüstü sayısı.
pub const DESKTOPS: u8 = 2;

impl Window {
    fn new(id: u64, kind: Kind, title: &str, x: i32, y: i32, client_w: i32, client_h: i32) -> Self {
        Self {
            id,
            kind,
            title: String::from(title),
            frame: Rect::new(x, y, client_w, client_h + m().title_h),
            visible: true,
            surface: Surface::new(client_w, client_h, draw::WINDOW_BG & 0x00ff_ffff),
            channel: None,
            restore: None,
            desktop: 0,
            parked: false,
            snapped: None,
        }
    }

    pub fn client(&self) -> Rect {
        Rect::new(
            self.frame.x,
            self.frame.y + m().title_h,
            self.surface.width,
            self.surface.height,
        )
    }

    pub fn title_bar(&self) -> Rect {
        Rect::new(self.frame.x, self.frame.y, self.frame.w, m().title_h)
    }

    pub fn close_button(&self) -> Rect {
        let c = m().close;
        Rect::new(self.frame.x + c, self.frame.y + (m().title_h - c) / 2, c, c)
    }

    pub fn minimize_button(&self) -> Rect {
        let c = m().close;
        self.close_button().offset(c * 7 / 4, 0)
    }

    pub fn maximize_button(&self) -> Rect {
        let c = m().close;
        self.minimize_button().offset(c * 7 / 4, 0)
    }

    /// Sağ alt köşedeki boyutlandırma tutamağı.
    pub fn resize_handle(&self) -> Rect {
        let s = 18 * m().unit;
        Rect::new(self.frame.right() - s, self.frame.bottom() - s, s, s)
    }

    pub fn footprint(&self) -> Rect {
        let shadow = m().shadow;
        self.frame
            .inset(-shadow)
            .union(&self.frame.offset(0, shadow / 3).inset(-shadow))
    }

    /// Kesin opak olan iç bölge (köşe yuvarlamaları hariç).
    pub fn opaque(&self) -> Rect {
        self.frame.inset(m().radius)
    }
}

/// İmlece yakınlığa göre öğenin hedef büyütmesi (0..=256).
fn dock_zoom_target(d: &DockItem, cursor_x: i32, base: i32, in_zone: bool) -> i32 {
    if !in_zone {
        return 0;
    }
    let center = d.rect.x + d.rect.w / 2;
    (256 - (cursor_x - center).abs() * 256 / (base * 2)).clamp(0, 256)
}

/// Dock öğesi: sabitlenmiş ya da açık bir uygulama (bilinmeyen uygulamalar pencere başına).
pub struct DockItem {
    pub app: Option<String>,
    pub window: Option<u64>,
    pub label: String,
    pub rect: Rect,
    /// Fare yakınlığı (büyütme): 0..=256, hedefe doğru yumuşakça ilerler.
    pub zoom: i32,
    pub running: bool,
    pub focused: bool,
    /// Sabitlenmişlerle açıklar arasındaki ayıracın hemen sağındaki öğe.
    pub after_separator: bool,
}

pub struct Desktop {
    pub width: i32,
    pub height: i32,
    pub wallpaper: Surface,
    /// Cam yüzeylerin örneklediği bulanık duvar kağıdı.
    pub backdrop: Surface,
    /// Duvar kağıdı her değiştiğinde artar (önbelleğe alınmış cam katmanları için).
    pub backdrop_gen: u64,
    pub windows: Vec<Window>,
    pub dock: Vec<DockItem>,
    pub cursor: (i32, i32),
    buttons: u8,
    drag: Option<(u64, i32, i32)>,
    /// Sürüklemenin başladığı imleç konumu (yaslanmış pencereyi çekince eski boyutuna
    /// döndürmek için).
    drag_start: (i32, i32),
    /// Sürüklenen pencere bırakılınca yaslanacağı yer ve önizleme dikdörtgeni.
    pub snap_preview: Option<(Snap, Rect)>,
    /// Gösterilen sanal masaüstü.
    pub current_desktop: u8,
    /// Boyutlandırma: (pencere, başlangıç imleci x/y, başlangıç istemci boyutu).
    resize_drag: Option<(u64, i32, i32, i32, i32)>,
    /// Alt+Tab: pencere sırası ve seçili olan.
    pub switcher: Option<(Vec<u64>, usize)>,
    capture: Option<u64>,
    pub hovered_dock: Option<usize>,
    /// Dock büyütme animasyonunun son adım zamanı (süreye dayalı yumuşatma için).
    dock_tick_ms: u64,
    /// Boşta önbelleğe çizilecek sıradaki dock öğesi (tüm büyütme boyutları).
    dock_warm: usize,
    pub hovered_logo: bool,
    pub damage: Damage,
    last_second: u64,
    next_window_id: u64,
    pub clock_text: String,
    pub status_text: String,
    pub login: Option<login::Login>,
    last_minute: u64,
    pub prefs: Prefs,
    pub theme: Theme,
    pub launcher: Option<launcher::Launcher>,
    pub now_ms: u64,
    last_input_ms: u64,
    pub blanked: bool,
    pending_mode: Option<(u32, u32)>,
    dock_zone: bool,
    /// Açık sağ tık menüsü.
    pub menu: Option<menu::Menu>,
    /// Masaüstü simgeleri, seçili olan ve son tıklama (çift tıklama için).
    pub desk: Vec<desk::Entry>,
    pub desk_selected: Option<usize>,
    desk_click: (usize, u64),
    desk_scan_ms: u64,
}

/// Klavye olayının uygulamaya giden kodu (0 = gönderilmez).
fn key_code(key: &keyboard::KeyEvent) -> u32 {
    use keyboard::Key;
    let code = match key.key {
        Key::Char(c)
            if key.modifiers.ctrl && key.modifiers.shift && c.eq_ignore_ascii_case(&'v') =>
        {
            app::KEY_PASTE
        }
        Key::Char(c) if key.modifiers.ctrl && c.is_ascii_alphabetic() => {
            c.to_ascii_lowercase() as u32 - 'a' as u32 + 1
        }
        Key::Char('\\') if key.modifiers.ctrl => 0x1c,
        // Ctrl ile yakınlaştırma (Ctrl + / - / 0) ve sekme geçişi (Ctrl+Tab): KEY_CTRL bitiyle.
        Key::Char(c @ ('+' | '-' | '=' | '0')) if key.modifiers.ctrl => c as u32 | app::KEY_CTRL,
        Key::Tab if key.modifiers.ctrl => {
            9 | app::KEY_CTRL
                | if key.modifiers.shift {
                    app::KEY_SHIFT
                } else {
                    0
                }
        }
        Key::Char(c) => c as u32,
        Key::Enter => 13,
        Key::Backspace => 8,
        Key::Escape => 27,
        Key::Tab => 9,
        Key::Up => app::KEY_UP,
        Key::Down => app::KEY_DOWN,
        Key::Left => app::KEY_LEFT,
        Key::Right => app::KEY_RIGHT,
        Key::Home => app::KEY_HOME,
        Key::End => app::KEY_END,
        Key::Delete => app::KEY_DELETE,
        Key::PageUp => app::KEY_PAGE_UP,
        Key::PageDown => app::KEY_PAGE_DOWN,
        Key::Insert => app::KEY_INSERT,
        Key::F(n @ 1..=12) => app::KEY_F1 + n as u32 - 1,
        _ => 0,
    };
    let mut code = code;
    if (app::KEY_UP..=app::KEY_PAGE_DOWN).contains(&code) {
        if key.modifiers.shift {
            code |= app::KEY_SHIFT;
        }
        if key.modifiers.ctrl {
            code |= app::KEY_CTRL;
        }
    }
    code
}

fn contains(outer: &Rect, inner: &Rect) -> bool {
    inner.x >= outer.x
        && inner.y >= outer.y
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn sound(frequency: u32, ms: u32) {
    sched::spawn_detached("ui-sound", move || {
        audio::beep(frequency, ms);
        0
    });
}

impl Desktop {
    fn new(width: i32, height: i32, prefs: Prefs) -> Self {
        LARGE_UI.store(height >= 1300, Ordering::Relaxed);
        let theme = Theme::from(&prefs);
        let wallpaper = wallpaper::render(&prefs.wallpaper, width, height, prefs.wallpaper_dim)
            .unwrap_or_else(|| {
                wallpaper::render(&prefs::Wallpaper::Preset(0), width, height, 0).unwrap()
            });
        if let prefs::Wallpaper::Image(path) = &prefs.wallpaper {
            wallpaper::request_image(path, width, height, prefs.wallpaper_dim);
        }
        let backdrop = crate::gfx::glass::backdrop(&wallpaper, BACKDROP_BLUR);
        let mut desktop = Self {
            width,
            height,
            wallpaper,
            backdrop,
            backdrop_gen: 0,
            windows: Vec::new(),
            dock: Vec::new(),
            cursor: (width / 2, height / 2),
            buttons: 0,
            drag: None,
            drag_start: (0, 0),
            snap_preview: None,
            current_desktop: 0,
            resize_drag: None,
            switcher: None,
            capture: None,
            hovered_dock: None,
            dock_tick_ms: 0,
            dock_warm: 0,
            hovered_logo: false,
            damage: Damage::default(),
            last_second: u64::MAX,
            next_window_id: 100,
            clock_text: String::new(),
            status_text: String::new(),
            login: None,
            last_minute: u64::MAX,
            prefs,
            theme,
            launcher: None,
            now_ms: pit::uptime_ms(),
            last_input_ms: pit::uptime_ms(),
            blanked: false,
            pending_mode: None,
            dock_zone: false,
            menu: None,
            desk: Vec::new(),
            desk_selected: None,
            desk_click: (usize::MAX, 0),
            desk_scan_ms: 0,
        };
        apps::reload();
        desktop.relayout_dock();
        desktop.full_damage();
        desktop
    }

    // ---- Masaüstü simgeleri ve sağ tık menüleri ----

    fn update_desk(&mut self, now_ms: u64) {
        let wanted = self.prefs.desktop_icons && self.login.is_none() && users::session().is_some();
        if !wanted {
            if !self.desk.is_empty() {
                self.damage.add(desk::bounds(&self.desk));
                self.desk.clear();
                self.desk_selected = None;
            }
            return;
        }
        if now_ms >= self.desk_scan_ms + 3000 {
            self.desk_scan_ms = now_ms;
            desk::scan_async();
        }
        if let Some(list) = desk::take_scanned() {
            let same = list.len() == self.desk.len()
                && list
                    .iter()
                    .zip(&self.desk)
                    .all(|((n, d), e)| *n == e.name && *d == e.dir);
            if !same {
                let folder = desk::folder().unwrap_or_default();
                self.damage.add(desk::bounds(&self.desk));
                self.desk = desk::layout(&list, &folder, self.height);
                self.damage.add(desk::bounds(&self.desk));
                self.desk_selected = None;
            }
        }
    }

    fn select_desk(&mut self, index: Option<usize>) {
        if self.desk_selected != index {
            for i in [self.desk_selected, index].into_iter().flatten() {
                if let Some(e) = self.desk.get(i) {
                    self.damage.add(e.rect);
                }
            }
            self.desk_selected = index;
        }
    }

    /// Pencerenin olmadığı yere sol tık: simge seçimi, çift tıklamayla açma.
    fn desk_press(&mut self, x: i32, y: i32) {
        let index = desk::at(&self.desk, x, y);
        self.select_desk(index);
        let Some(i) = index else {
            return;
        };
        let now = pit::uptime_ms();
        if self.desk_click.0 == i && now - self.desk_click.1 < 450 {
            self.desk_click = (usize::MAX, 0);
            let path = self.desk[i].path.clone();
            self.menu_action(menu::Action::Open(path));
        } else {
            self.desk_click = (i, now);
        }
    }

    fn open_menu(&mut self, x: i32, y: i32, items: Vec<menu::Item>) {
        if let Some(old) = self.menu.take() {
            self.damage.add(old.footprint());
        }
        let menu = menu::Menu::new(
            x,
            y,
            items,
            &draw::fonts(self.height),
            (self.width, self.height),
        );
        self.damage.add(menu.footprint());
        self.menu = Some(menu);
    }

    /// Sağ tık: dock öğesi, pencere (uygulamaya iletilir), masaüstü simgesi ya da boş masaüstü.
    fn context_press(&mut self, x: i32, y: i32) {
        use menu::{Action, Item};
        if let Some(old) = self.menu.take() {
            self.damage.add(old.footprint());
        }
        if self.launcher.as_ref().is_some_and(|l| !l.closing()) {
            return;
        }
        if let Some(index) = self.dock.iter().position(|d| d.rect.contains(x, y)) {
            let items = self.dock_menu(index);
            let rect = self.dock[index].rect;
            self.open_menu(rect.x + rect.w / 2, y, items);
            // Dock menüsü simgenin üstünde, ortalı açılır.
            let top = self.dock_area().y;
            if let Some(menu) = self.menu.as_mut() {
                self.damage.add(menu.footprint());
                let w = menu.rect.w;
                menu.rect.x = (rect.x + rect.w / 2 - w / 2).clamp(4, self.width - w - 4);
                menu.rect.y = (top - menu.rect.h - 10 * m().unit).max(4);
            }
            // Üzerindeki ipucu balonu da silinmeli.
            self.full_damage();
            return;
        }
        if y < m().panel_h || self.dock_area().contains(x, y) {
            return;
        }
        if let Some(window) = self
            .windows
            .iter()
            .rev()
            .find(|w| w.visible && w.frame.contains(x, y))
        {
            let client = window.client();
            let id = window.id;
            if client.contains(x, y) {
                self.send_to_app(
                    id,
                    app::Event::mouse(app::EVENT_CONTEXT, x - client.x, y - client.y, 2),
                );
            } else if window.title_bar().contains(x, y) {
                let (snapped, restore, desktop) = (window.snapped, window.restore, window.desktop);
                let mut items = Vec::new();
                if snapped.is_some() || restore.is_some() {
                    items.push(Item::new("Eski boyutuna döndür", Action::RestoreWindow(id)));
                }
                if snapped != Some(Snap::Maximize) {
                    items.push(Item::new("Büyüt", Action::SnapWindow(id, Snap::Maximize)));
                }
                if snapped != Some(Snap::Left) {
                    items.push(Item::new(
                        "Sol yarıya yasla",
                        Action::SnapWindow(id, Snap::Left),
                    ));
                }
                if snapped != Some(Snap::Right) {
                    items.push(Item::new(
                        "Sağ yarıya yasla",
                        Action::SnapWindow(id, Snap::Right),
                    ));
                }
                items.push(Item::separator());
                for d in (0..DESKTOPS).filter(|d| *d != desktop) {
                    items.push(Item::new(
                        &alloc::format!("Masaüstü {}'ye taşı", d + 1),
                        Action::MoveToDesktop(id, d),
                    ));
                }
                items.push(Item::separator());
                items.push(Item::new("Kapat", Action::CloseWindow(id)));
                self.open_menu(x, y, items);
            }
            return;
        }
        let folder = desk::folder();
        if let Some(i) = desk::at(&self.desk, x, y) {
            self.select_desk(Some(i));
            let path = self.desk[i].path.clone();
            let mut items = alloc::vec![Item::new("Aç", Action::Open(path.clone()))];
            if self.desk[i].dir {
                items.push(Item::new(
                    "Burada terminal aç",
                    Action::TerminalAt(path.clone()),
                ));
            }
            items.push(Item::separator());
            items.push(Item::new("Çöp kutusuna taşı", Action::Delete(path)));
            self.open_menu(x, y, items);
            return;
        }
        self.select_desk(None);
        let mut items = Vec::new();
        if let Some(folder) = folder {
            items.push(Item::new("Yeni klasör", Action::NewFolder));
            items.push(Item::new("Burada terminal aç", Action::TerminalAt(folder)));
            items.push(Item::separator());
        }
        items.push(Item::new("Duvar kağıdını değiştir…", Action::Wallpaper));
        items.push(Item::new("Ekran ayarları…", Action::DisplaySettings));
        items.push(Item::new(
            if self.prefs.desktop_icons {
                "Masaüstü simgelerini gizle"
            } else {
                "Masaüstü simgelerini göster"
            },
            Action::ToggleIcons,
        ));
        self.open_menu(x, y, items);
    }

    fn dock_menu(&self, index: usize) -> Vec<menu::Item> {
        use menu::{Action, Item};
        let item = &self.dock[index];
        let mut items = Vec::new();
        match (&item.app, item.window) {
            (Some(id), _) => {
                let app = apps::get(id);
                let actions = app.as_ref().map(|a| a.actions.clone()).unwrap_or_default();
                let single = app.as_ref().is_some_and(|a| a.single);
                for (label, args) in actions {
                    items.push(Item::new(&label, Action::Launch(id.clone(), args)));
                }
                if items.is_empty() && !single {
                    items.push(Item::new(
                        "Yeni pencere",
                        Action::Launch(id.clone(), Vec::new()),
                    ));
                }
                if !items.is_empty() {
                    items.push(Item::separator());
                }
                if item.running {
                    items.push(Item::new("Göster", Action::Show(id.clone())));
                    items.push(Item::new("Kapat", Action::Quit(id.clone())));
                } else {
                    items.push(Item::new("Aç", Action::Show(id.clone())));
                }
                let pinned = apps::pinned(&self.prefs.dock_add, &self.prefs.dock_remove)
                    .iter()
                    .any(|a| a.id == *id);
                items.push(if pinned {
                    Item::new("Dock'tan kaldır", Action::Unpin(id.clone()))
                } else {
                    Item::new("Dock'ta tut", Action::Pin(id.clone()))
                });
            }
            (None, Some(window)) => items.push(Item::new("Kapat", Action::CloseWindow(window))),
            _ => {}
        }
        items.push(Item::separator());
        items.push(Item::new("Dock ayarları…", Action::DockSettings));
        items
    }

    fn close_app_windows(&mut self, matches: impl Fn(&Window) -> bool) {
        let targets: Vec<(usize, u64)> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| matches(w))
            .map(|(i, w)| (i, w.id))
            .collect();
        for (i, id) in targets.into_iter().rev() {
            self.send_to_app(id, app::Event::close());
            self.set_visible(i, false);
        }
    }

    fn set_dock_pin(&mut self, id: &str, pin: bool) {
        let without = |list: &str| -> String {
            list.split(',')
                .map(str::trim)
                .filter(|x| !x.is_empty() && *x != id)
                .collect::<Vec<_>>()
                .join(",")
        };
        let default = apps::get(id).is_some_and(|a| a.dock.is_some());
        self.prefs.dock_add = without(&self.prefs.dock_add);
        self.prefs.dock_remove = without(&self.prefs.dock_remove);
        let (list, add) = match (pin, default) {
            (true, false) => (&mut self.prefs.dock_add, true),
            (false, true) => (&mut self.prefs.dock_remove, true),
            _ => (&mut self.prefs.dock_add, false),
        };
        if add {
            if !list.is_empty() {
                list.push(',');
            }
            list.push_str(id);
        }
        self.save_prefs();
        self.relayout_dock();
    }

    fn menu_action(&mut self, action: menu::Action) {
        use menu::Action;
        let session = users::session();
        match action {
            Action::NewFolder => {
                let (Some(folder), Some(session)) = (desk::folder(), session) else {
                    return;
                };
                sched::spawn_detached("desk-mkdir", move || {
                    let mut path = alloc::format!("{}/Yeni klasör", folder);
                    let mut n = 2;
                    while crate::fs::stat(&path).is_ok() {
                        path = alloc::format!("{}/Yeni klasör {}", folder, n);
                        n += 1;
                    }
                    let owner = crate::fs::Owner {
                        uid: session.uid,
                        gid: session.uid,
                    };
                    let _ = crate::fs::mkdir_as(&path, owner, 0o755);
                    desk::scan_async();
                    0
                });
                self.desk_scan_ms = 0;
            }
            Action::TerminalAt(dir) => {
                apps::spawn_with("terminal", alloc::vec![String::from("-d"), dir])
            }
            Action::Wallpaper => apps::spawn_with("settings", alloc::vec!["Duvar kağıdı".into()]),
            Action::DisplaySettings => apps::spawn_with("settings", alloc::vec!["Ekran".into()]),
            Action::DockSettings => {
                apps::spawn_with("settings", alloc::vec!["Dock ve panel".into()])
            }
            Action::Launch(id, args) if args.is_empty() => self.launch(&id, true),
            Action::Launch(id, args) => apps::spawn_with(&id, args),
            Action::Show(id) => self.launch(&id, false),
            Action::Quit(id) => {
                self.close_app_windows(|w| apps::app_of(w).as_deref() == Some(id.as_str()))
            }
            Action::CloseWindow(window) => self.close_app_windows(|w| w.id == window),
            Action::SnapWindow(window, snap) => self.snap_window(window, snap),
            Action::RestoreWindow(window) => self.unsnap_window(window, None),
            Action::MoveToDesktop(window, desktop) => self.move_to_desktop(window, desktop),
            Action::Pin(id) => self.set_dock_pin(&id, true),
            Action::Unpin(id) => self.set_dock_pin(&id, false),
            Action::Open(path) => {
                let dir =
                    crate::fs::stat(&path).is_ok_and(|m| m.kind == crate::fs::Kind::Directory);
                let lower = path.to_lowercase();
                let image = [".png", ".jpg", ".jpeg", ".bmp", ".ppm"]
                    .iter()
                    .any(|e| lower.ends_with(e));
                let app = if dir {
                    "files"
                } else if image {
                    "viewer"
                } else {
                    "notes"
                };
                apps::spawn_with(app, alloc::vec![path]);
            }
            Action::Delete(path) => {
                let Some(session) = session else {
                    return;
                };
                sched::spawn_detached("desk-trash", move || {
                    // Çöp kutusu: ~/.Trash (aynı birimde taşıma); izin yoksa dokunulmaz.
                    if !crate::fs::may_modify_entry(session.uid, &path, true) {
                        crate::log!("gui: no permission to move {} to trash", path);
                        return -1;
                    }
                    let trash = alloc::format!("{}/.Trash", session.home);
                    if crate::fs::stat(&trash).is_err() {
                        let owner = crate::fs::Owner {
                            uid: session.uid,
                            gid: session.uid,
                        };
                        let _ = crate::fs::mkdir_as(&trash, owner, 0o700);
                    }
                    let name = path.rsplit('/').next().unwrap_or("öğe");
                    let mut target = alloc::format!("{}/{}", trash, name);
                    let mut n = 2;
                    while crate::fs::stat(&target).is_ok() {
                        target = alloc::format!("{}/{} {}", trash, name, n);
                        n += 1;
                    }
                    let _ = crate::fs::rename(&path, &target);
                    desk::scan_async();
                    0
                });
                self.desk_scan_ms = 0;
            }
            Action::ToggleIcons => {
                self.prefs.desktop_icons = !self.prefs.desktop_icons;
                self.save_prefs();
                self.desk_scan_ms = 0;
            }
        }
    }

    fn base_dock_size(&self) -> i32 {
        let px = [42, 52, 64][self.prefs.dock_size.min(2) as usize];
        if m().unit == 2 { px * 4 / 3 } else { px }
    }

    /// Dock öğelerini sabitlenmiş ve açık uygulamalardan yeniden kurar.
    fn relayout_dock(&mut self) {
        self.dock_warm = 0;
        // Dock daralınca eski alanın kenarları da yeniden çizilmeli.
        if !self.dock.is_empty() {
            self.damage.add(self.dock_area());
        }
        let old: Vec<(Option<String>, Option<u64>, i32)> = self
            .dock
            .iter()
            .map(|d| (d.app.clone(), d.window, d.zoom))
            .collect();
        let focused = self.focused().map(|w| w.id);
        let mut items: Vec<DockItem> = Vec::new();
        let item = |app: Option<String>, window: Option<u64>, label: String| DockItem {
            app,
            window,
            label,
            rect: Rect::default(),
            zoom: 0,
            running: false,
            focused: false,
            after_separator: false,
        };
        for app in apps::pinned(&self.prefs.dock_add, &self.prefs.dock_remove) {
            items.push(item(Some(app.id), None, app.name));
        }
        let pinned = items.len();
        for w in &self.windows {
            let app = apps::app_of(w);
            let index = match &app {
                Some(a) => items.iter().position(|d| d.app.as_ref() == Some(a)),
                None => None,
            };
            let index = match index {
                Some(i) => i,
                None if self.prefs.dock_running => {
                    let label = app
                        .as_deref()
                        .and_then(apps::get)
                        .map(|a| a.name)
                        .unwrap_or_else(|| w.title.clone());
                    let window = if app.is_none() { Some(w.id) } else { None };
                    items.push(item(app, window, label));
                    items.len() - 1
                }
                None => continue,
            };
            items[index].running = true;
            items[index].focused |= focused == Some(w.id) && w.visible;
        }
        if items.len() > pinned {
            items[pinned].after_separator = true;
        }
        for d in items.iter_mut() {
            if let Some((_, _, zoom)) = old.iter().find(|(a, w, _)| *a == d.app && *w == d.window) {
                d.zoom = *zoom;
            }
        }
        self.dock = items;
        self.layout_dock_rects();
        self.damage.add(self.dock_area());
    }

    fn item_size(&self, zoom: i32) -> i32 {
        let base = self.base_dock_size();
        if self.prefs.dock_magnify {
            base + base * 45 / 100 * zoom / 256
        } else {
            base
        }
    }

    /// Öğe dikdörtgenleri (büyütme dahil), alt kenarları hizalı ve ortalanmış.
    fn layout_dock_rects(&mut self) {
        let base = self.base_dock_size();
        let gap = base / 4;
        let sep = gap * 2;
        let widths: Vec<i32> = self.dock.iter().map(|d| self.item_size(d.zoom)).collect();
        let total: i32 = widths.iter().sum::<i32>()
            + gap * (self.dock.len() as i32 - 1).max(0)
            + self.dock.iter().filter(|d| d.after_separator).count() as i32 * sep;
        let bottom = self.height - 12 * m().unit - 10 * m().unit;
        let mut x = self.width / 2 - total / 2;
        for (d, w) in self.dock.iter_mut().zip(widths) {
            if d.after_separator {
                x += sep;
            }
            d.rect = Rect::new(x, bottom - w, w, w);
            x += w + gap;
        }
    }

    /// Dock arka planı: simgelerin yatay kapsamı, yükseklik büyütmesiz.
    pub fn dock_background(&self) -> Rect {
        let base = self.base_dock_size();
        let pad = 10 * m().unit;
        let bottom = self.height - 12 * m().unit;
        let (left, right) = match (self.dock.first(), self.dock.last()) {
            (Some(a), Some(b)) => (a.rect.x, b.rect.right()),
            _ => (self.width / 2, self.width / 2),
        };
        Rect::new(
            left - pad,
            bottom - base - 2 * pad,
            right - left + 2 * pad,
            base + 2 * pad,
        )
    }

    /// Dock'un çizebileceği tüm alan: büyüyen simgeler ve ipucu balonu dahil.
    pub fn dock_area(&self) -> Rect {
        let base = self.base_dock_size();
        let bg = self.dock_background();
        let mut area = bg.inset(-base / 2);
        area.y -= base / 2 + 48 * m().unit;
        area.h += base / 2 + 48 * m().unit;
        for d in &self.dock {
            area = area.union(&d.rect);
        }
        area
    }

    pub fn logo_rect(&self) -> Rect {
        let h = m().panel_h;
        let size = h - 10 * m().unit;
        Rect::new(10 * m().unit, (h - size) / 2, size, size)
    }

    pub fn focused(&self) -> Option<&Window> {
        self.windows.iter().rev().find(|w| w.visible)
    }

    fn index_of(&self, id: u64) -> Option<usize> {
        self.windows.iter().position(|w| w.id == id)
    }

    fn raise(&mut self, index: usize) {
        let previous_focus = self.focused().map(|w| w.id);
        let window = self.windows.remove(index);
        let id = window.id;
        self.windows.push(window);
        if previous_focus != Some(id) {
            let footprint = self.windows.last().unwrap().footprint();
            self.damage.add(footprint);
            if let Some(prev) = previous_focus.and_then(|p| self.index_of(p)) {
                let bar = self.windows[prev].title_bar();
                self.damage.add(bar);
            }
            self.relayout_dock();
            self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        }
        self.update_keyboard_route();
    }

    /// Masaüstü açıkken tüm tuşlar compositor'a gelir (çekirdek kabuğu beklemededir).
    fn update_keyboard_route(&self) {
        keyboard::route_to_gui(true);
    }

    fn full_damage(&mut self) {
        self.damage.add(Rect::new(0, 0, self.width, self.height));
    }

    /// Oturum yoksa giriş ekranını açar; giriş tamamlanınca masaüstüne döner.
    fn update_session(&mut self) {
        if users::session().is_none() && self.login.is_none() {
            self.login = Some(login::Login::new());
            self.drag = None;
            self.capture = None;
            self.launcher = None;
            self.full_damage();
            self.update_keyboard_route();
        }
        let Some(login) = self.login.as_mut() else {
            return;
        };
        let before = login.dirty_area(self.width, self.height);
        let (changed, user) = login.poll();
        if changed {
            let after = login.dirty_area(self.width, self.height);
            self.damage.add(before);
            self.damage.add(after);
        }
        if let Some(user) = user {
            self.login = None;
            users::begin_session(&user);
            self.full_damage();
            self.update_keyboard_route();
            apps::start_session(user.home.clone());
        }
    }

    fn handle_login_input(&mut self, pressed: bool) {
        let (x, y) = self.cursor;
        let (width, height) = (self.width, self.height);
        let Some(login) = self.login.as_mut() else {
            return;
        };
        let before = login.card_area(width, height);
        let mut changed = pressed && login.click(x, y, width, height);
        while let Some(key) = keyboard::take_gui_key() {
            changed |= login.key(key);
        }
        if changed {
            let after = login.card_area(width, height);
            self.damage.add(before);
            self.damage.add(after);
        }
    }

    fn set_visible(&mut self, index: usize, visible: bool) {
        let mut index = index;
        if visible && self.windows[index].desktop != self.current_desktop {
            let id = self.windows[index].id;
            self.switch_desktop(self.windows[index].desktop);
            let Some(i) = self.index_of(id) else {
                return;
            };
            index = i;
        }
        if !visible {
            self.windows[index].parked = false;
        }
        if self.windows[index].visible == visible {
            return;
        }
        self.windows[index].visible = visible;
        let footprint = self.windows[index].footprint();
        self.damage.add(footprint);
        if visible {
            self.raise(index);
        } else if let Some(bar) = self.focused().map(|w| w.title_bar()) {
            self.damage.add(bar);
        }
        self.relayout_dock();
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        self.update_keyboard_route();
    }

    pub fn add_window(&mut self, mut window: Window) -> u64 {
        let id = self.next_window_id;
        self.next_window_id += 1;
        window.id = id;
        window.desktop = self.current_desktop;
        let offset = (self.windows.len() as i32 % 8) * 28;
        window.frame.x = (self.width / 2 - window.frame.w / 2 + offset - 80)
            .clamp(0, (self.width - window.frame.w).max(0));
        // Sığıyorsa dock'un üstünde kalsın.
        let floor = self.dock_background().y - 8 * m().unit;
        let lowest = if window.frame.h + m().panel_h <= floor {
            floor - window.frame.h
        } else {
            self.height - window.frame.h
        };
        window.frame.y = (self.height / 2 - window.frame.h / 2 + offset - 60)
            .clamp(m().panel_h, lowest.max(m().panel_h));
        self.damage.add(window.footprint());
        // Önceki odaklı pencerenin başlığı artık pasif renkte çizilmeli.
        if let Some(previous) = self.focused().map(|w| w.title_bar()) {
            self.damage.add(previous);
        }
        self.windows.push(window);
        self.relayout_dock();
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        self.update_keyboard_route();
        id
    }

    pub fn remove_window(&mut self, id: u64) {
        if let Some(index) = self.index_of(id) {
            let window = self.windows.remove(index);
            self.damage.add(window.footprint());
            if let Some(channel) = window.channel {
                channel.close();
            }
            if self.drag.is_some_and(|(d, _, _)| d == id) {
                self.drag = None;
            }
            if self.capture == Some(id) {
                self.capture = None;
            }
            if let Some(bar) = self.focused().map(|w| w.title_bar()) {
                self.damage.add(bar);
            }
            self.relayout_dock();
            self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
            self.update_keyboard_route();
        }
    }

    fn send_to_app(&self, id: u64, event: app::Event) {
        if let Some(channel) = self
            .windows
            .iter()
            .find(|w| w.id == id)
            .and_then(|w| w.channel.as_ref())
        {
            channel.push(event);
        }
    }

    fn handle_input(&mut self) {
        let state = mouse::state();
        let previous = self.cursor;
        let moved = (state.x, state.y) != previous;
        self.cursor = (state.x, state.y);
        let (x, y) = self.cursor;
        let wheel = mouse::take_wheel();
        let pressed = state.buttons & mouse::LEFT != 0 && self.buttons & mouse::LEFT == 0;
        let right = state.buttons & mouse::RIGHT != 0 && self.buttons & mouse::RIGHT == 0;
        let released = state.buttons & mouse::LEFT == 0 && self.buttons & mouse::LEFT != 0;
        let buttons_changed = state.buttons != self.buttons;
        self.buttons = state.buttons;
        let mut activity = moved || buttons_changed || wheel != 0;

        if self.login.is_some() {
            self.handle_login_input(pressed);
            self.note_activity(true);
            return;
        }

        if moved {
            self.hover(x, y);
        }
        if pressed {
            self.press(x, y);
        }
        if right {
            self.context_press(x, y);
        }
        if released {
            if let Some((id, ..)) = self.drag.take()
                && let Some((snap, rect)) = self.snap_preview.take()
            {
                self.damage.add(rect.inset(-4));
                self.snap_window(id, snap);
            }
            if let Some((id, ..)) = self.resize_drag.take() {
                self.notify_resize(id);
            }
            if let Some(id) = self.capture.take()
                && let Some(window) = self.windows.iter().find(|w| w.id == id)
            {
                let client = window.client();
                self.send_to_app(
                    id,
                    app::Event::mouse(app::EVENT_MOUSE_UP, x - client.x, y - client.y, 1),
                );
            }
        }
        // Tekerlek: imlecin altındaki uygulama penceresine.
        if wheel != 0
            && let Some(window) = self
                .windows
                .iter()
                .rev()
                .find(|w| w.visible && w.frame.contains(x, y))
                .filter(|w| w.client().contains(x, y))
        {
            let client = window.client();
            let mut event = app::Event::mouse(app::EVENT_WHEEL, x - client.x, y - client.y, 0);
            event.key = wheel as u32;
            self.send_to_app(window.id, event);
        }
        if moved {
            if let Some((id, x0, y0, w0, h0)) = self.resize_drag {
                self.resize_window(id, w0 + x - x0, h0 + y - y0);
            } else if let Some((id, dx, dy)) = self.drag {
                let (mut dx, mut dy) = (dx, dy);
                // Yaslanmış pencere çekilince eski boyutuna döner; imleç başlıkta aynı oranda kalır.
                let moved_far = (x - self.drag_start.0).abs() + (y - self.drag_start.1).abs() > 8;
                if moved_far
                    && let Some(index) = self.index_of(id)
                    && let Some((_, _, rw, _)) = self.windows[index].restore
                {
                    let old_w = self.windows[index].frame.w.max(1);
                    let nx = dx * rw / old_w;
                    self.unsnap_window(id, Some((x - nx, y - dy)));
                    dx = nx;
                    dy = dy.min(m().title_h - 1);
                    self.drag = Some((id, dx, dy));
                }
                self.drag_window(id, x - dx, y - dy);
                self.update_snap_preview(x, y);
            } else if let Some(target) = self.capture.or_else(|| {
                self.windows
                    .iter()
                    .rev()
                    .find(|w| w.visible && w.frame.contains(x, y))
                    .filter(|w| matches!(w.kind, Kind::App(_)) && w.client().contains(x, y))
                    .map(|w| w.id)
            }) && let Some(window) = self.windows.iter().find(|w| w.id == target)
            {
                let client = window.client();
                self.send_to_app(
                    target,
                    app::Event::mouse(app::EVENT_MOUSE_MOVE, x - client.x, y - client.y, 0),
                );
            }
        }

        while let Some(key) = keyboard::take_gui_key() {
            activity = true;
            self.key(key);
        }
        self.note_activity(activity);
    }

    fn note_activity(&mut self, activity: bool) {
        if !activity {
            return;
        }
        self.last_input_ms = pit::uptime_ms();
        if self.blanked {
            self.blanked = false;
            self.full_damage();
        }
    }

    fn hover(&mut self, x: i32, y: i32) {
        if let Some(menu) = self.menu.as_mut() {
            let item = menu.item_at(x, y, &draw::fonts(self.height));
            if item != menu.hover {
                menu.hover = item;
                let area = menu.footprint();
                self.damage.add(area);
            }
        }
        let logo = self.logo_rect().inset(-4 * m().unit).contains(x, y);
        if logo != self.hovered_logo {
            self.hovered_logo = logo;
            self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        }
        let in_zone = self.dock_background().inset(-8 * m().unit).contains(x, y)
            || self.dock.iter().any(|d| d.rect.contains(x, y));
        let hovered = if in_zone {
            self.dock.iter().position(|d| d.rect.contains(x, y))
        } else {
            None
        };
        if hovered != self.hovered_dock || in_zone || self.dock_zone {
            self.hovered_dock = hovered;
            self.damage.add(self.dock_area());
        }
        self.dock_zone = in_zone;
        if let Some(l) = self.launcher.as_mut()
            && l.hover(x, y)
        {
            let area = l.footprint();
            self.damage.add(area);
        }
    }

    fn key(&mut self, key: keyboard::KeyEvent) {
        if let Some(menu) = self.menu.take() {
            self.damage.add(menu.footprint());
            if matches!(key.key, keyboard::Key::Escape) {
                return;
            }
        }
        use keyboard::Key;
        if matches!(key.key, Key::Super) && self.launcher.as_ref().is_none_or(|l| l.closing()) {
            self.open_launcher();
            return;
        }
        if let Some(l) = self.launcher.as_mut()
            && !l.closing()
        {
            match l.key(key) {
                launcher::Outcome::Close => self.close_launcher(),
                launcher::Outcome::Run(t) => self.run_launcher_target(t),
                launcher::Outcome::Redraw => {
                    let area = l.footprint();
                    self.damage.add(area);
                }
                launcher::Outcome::None => {}
            }
            return;
        }
        // Alt+1 / Alt+2 masaüstü değiştirir; Shift ile odaklı pencereyi de götürür. (Ctrl+Alt
        // kullanılamaz: UTM imleç yakalamayı onunla bırakır, klavye sürücüsü sıfırlar.)
        // Rakam tuşu tarama kodundan tanınır: Shift'le düzene göre "!" gibi bir simgeye döner.
        let digit = match key.scancode {
            0x02..=0x0a => Some(key.scancode - 0x02),
            _ => None,
        };
        if key.modifiers.alt
            && !key.modifiers.ctrl
            && let Some(target) = digit.filter(|d| *d < DESKTOPS)
        {
            if key.modifiers.shift
                && let Some(id) = self.focused().map(|w| w.id)
            {
                self.move_to_desktop(id, target);
                self.switch_desktop(target);
                if let Some(i) = self.index_of(id) {
                    self.set_visible(i, true);
                    self.raise(i);
                }
            } else {
                self.switch_desktop(target);
            }
            return;
        }
        // Alt+Tab: pencereler arasında geçiş (Alt bırakılınca seçilen öne gelir).
        if matches!(key.key, Key::Tab) && key.modifiers.alt {
            self.switch_next();
            return;
        }
        // Alt+F4 pencereyi kapatır, F11 tam ekran (büyüt) yapar.
        if let Some(id) = self.focused().map(|w| w.id) {
            if matches!(key.key, Key::F(4)) && key.modifiers.alt {
                if let Some(index) = self.windows.iter().position(|w| w.id == id) {
                    self.send_to_app(id, app::Event::close());
                    self.set_visible(index, false);
                }
                return;
            }
            if matches!(key.key, Key::F(11)) {
                self.toggle_maximize(id);
                return;
            }
        }
        if self.switcher.is_some() && matches!(key.key, Key::Escape) {
            self.switcher = None;
            self.full_damage();
            return;
        }
        let Some(window) = self.focused() else {
            return;
        };
        let id = window.id;
        let code = key_code(&key);
        if code != 0 {
            self.send_to_app(id, app::Event::key(code));
        }
    }

    fn open_launcher(&mut self) {
        apps::reload_async();
        let mut l = launcher::Launcher::new(pit::uptime_ms(), self.prefs.animations);
        l.render_if_needed(
            &self.theme,
            &draw::fonts(self.height),
            &self.backdrop,
            self.backdrop_gen,
        );
        self.damage.add(l.footprint());
        self.launcher = Some(l);
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        crate::log!("gui: launcher opened");
    }

    fn close_launcher(&mut self) {
        let now = pit::uptime_ms();
        if let Some(l) = self.launcher.as_mut() {
            l.close(now);
            let area = l.footprint();
            self.damage.add(area);
        }
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
    }

    fn run_launcher_target(&mut self, target: launcher::Target) {
        self.close_launcher();
        match target {
            launcher::Target::App(id) => self.launch(&id, true),
            launcher::Target::Logout => self.action(Action::Logout),
            launcher::Target::Reboot => self.action(Action::Reboot),
            launcher::Target::Shutdown => self.action(Action::Shutdown),
        }
    }

    /// Uygulamayı açar. `new_instance` değilse (dock) ya da uygulama tek pencereliyse
    /// açık penceresi öne getirilir.
    fn launch(&mut self, id: &str, new_instance: bool) {
        let single = apps::get(id).is_some_and(|a| a.single);
        if (!new_instance || single)
            && let Some(index) = self
                .windows
                .iter()
                .rposition(|w| apps::app_of(w).as_deref() == Some(id))
        {
            self.set_visible(index, true);
            self.raise(index);
            return;
        }
        apps::spawn(id);
    }

    fn press(&mut self, x: i32, y: i32) {
        if let Some(menu) = self.menu.take() {
            self.damage.add(menu.footprint());
            if let Some(i) = menu.item_at(x, y, &draw::fonts(self.height))
                && let Some(action) = menu.items[i].action.clone()
            {
                self.menu_action(action);
            }
            return;
        }
        // Başlatıcı açıksa tıklamalar ona gider; dışına tıklamak kapatır.
        let on_logo = self.logo_rect().inset(-4 * m().unit).contains(x, y);
        if let Some(l) = self.launcher.as_mut()
            && !l.closing()
        {
            if on_logo {
                self.close_launcher();
                return;
            }
            match l.click(x, y) {
                launcher::Outcome::Run(t) => self.run_launcher_target(t),
                launcher::Outcome::Close => self.close_launcher(),
                _ => {}
            }
            return;
        }
        if on_logo {
            self.open_launcher();
            return;
        }
        if y < m().panel_h
            && let Some(i) = self
                .desktop_buttons()
                .iter()
                .position(|r| r.inset(-3).contains(x, y))
        {
            self.switch_desktop(i as u8);
            return;
        }
        if let Some(index) = self.dock.iter().position(|d| d.rect.contains(x, y)) {
            self.dock_click(index);
            return;
        }
        let Some(index) = self
            .windows
            .iter()
            .rposition(|w| w.visible && w.frame.contains(x, y))
        else {
            self.desk_press(x, y);
            return;
        };
        self.raise(index);
        let window = self.windows.last().unwrap();
        let id = window.id;
        let last = self.windows.len() - 1;
        if window.close_button().inset(-3).contains(x, y) {
            self.send_to_app(id, app::Event::close());
            self.set_visible(last, false);
        } else if window.minimize_button().inset(-3).contains(x, y) {
            self.set_visible(last, false);
        } else if window.maximize_button().inset(-3).contains(x, y) {
            self.toggle_maximize(id);
        } else if window.resize_handle().contains(x, y) {
            self.resize_drag = Some((id, x, y, window.surface.width, window.surface.height));
        } else if window.title_bar().contains(x, y) {
            self.drag = Some((id, x - window.frame.x, y - window.frame.y));
            self.drag_start = (x, y);
        } else if window.client().contains(x, y) {
            let client = window.client();
            self.capture = Some(id);
            self.send_to_app(
                id,
                app::Event::mouse(app::EVENT_MOUSE_DOWN, x - client.x, y - client.y, 1),
            );
        }
    }

    /// Dock tıklaması: açık değilse başlatır; odaklıysa küçültür; değilse öne getirir.
    fn dock_click(&mut self, index: usize) {
        let (app, window) = (self.dock[index].app.clone(), self.dock[index].window);
        let belongs = |w: &Window| match (&app, window) {
            (_, Some(id)) => w.id == id,
            (Some(a), None) => apps::app_of(w).as_ref() == Some(a),
            _ => false,
        };
        let focused = self.focused().map(|w| w.id);
        let own: Vec<(usize, bool, u64)> = self
            .windows
            .iter()
            .enumerate()
            .filter(|(_, w)| belongs(w))
            .map(|(i, w)| (i, w.visible, w.id))
            .collect();
        if own.is_empty() {
            if let Some(a) = app {
                self.launch(&a, false);
            }
            return;
        }
        if let Some(&(i, _, _)) = own
            .iter()
            .find(|(_, visible, id)| *visible && Some(*id) == focused)
        {
            self.set_visible(i, false);
            return;
        }
        // Gizlileri geri getir, en üsttekini öne al. Bu masaüstünde penceresi yoksa
        // en son penceresinin masaüstüne geçilir (pencereler masaüstleri arasında dağılmaz).
        let here: Vec<u64> = own
            .iter()
            .filter(|(i, ..)| self.windows[*i].desktop == self.current_desktop)
            .map(|(_, _, id)| *id)
            .collect();
        let ids: Vec<u64> = if here.is_empty() {
            let (last, ..) = own[own.len() - 1];
            let desktop = self.windows[last].desktop;
            own.iter()
                .filter(|(i, ..)| self.windows[*i].desktop == desktop)
                .map(|(_, _, id)| *id)
                .collect()
        } else {
            here
        };
        for id in &ids {
            if let Some(i) = self.index_of(*id) {
                self.set_visible(i, true);
            }
        }
        if let Some(i) = ids.last().and_then(|id| self.index_of(*id)) {
            self.raise(i);
        }
    }

    /// İstemci alanını yeni boyuta getirir; eski içerik sol üstte kalır, uygulamaya
    /// bırakınca `EVENT_RESIZE` gider.
    fn resize_window(&mut self, id: u64, width: i32, height: i32) {
        let u = m().unit;
        let max_w = self.width;
        let max_h = self.height - m().panel_h - m().title_h;
        let (bg, panel_h, (sw, sh)) =
            (self.theme.window_bg, m().panel_h, (self.width, self.height));
        let Some(index) = self.index_of(id) else {
            return;
        };
        let w = &mut self.windows[index];
        let (nw, nh) = (width.clamp(240 * u, max_w), height.clamp(120 * u, max_h));
        if (nw, nh) == (w.surface.width, w.surface.height) {
            return;
        }
        let old = w.footprint();
        let mut surface = Surface::new(nw, nh, bg & 0x00ff_ffff);
        for y in 0..nh.min(w.surface.height) {
            let n = nw.min(w.surface.width) as usize;
            let src = &w.surface.pixels[(y * w.surface.width) as usize..][..n];
            surface.pixels[(y * nw) as usize..][..n].copy_from_slice(src);
        }
        w.surface = surface;
        w.frame.w = nw;
        w.frame.h = nh + m().title_h;
        w.frame.x = w.frame.x.clamp(-w.frame.w + 80, sw - 80);
        w.frame.y = w.frame.y.clamp(panel_h, sh - m().title_h);
        let new = w.footprint();
        self.damage.add(old);
        self.damage.add(new);
    }

    fn notify_resize(&self, id: u64) {
        if let Some(w) = self.windows.iter().find(|w| w.id == id) {
            let mut event =
                app::Event::mouse(app::EVENT_RESIZE, w.surface.width, w.surface.height, 0);
            event.key = 0;
            self.send_to_app(id, event);
        }
    }

    /// Pencerelerin kullanabileceği alan: panelin altı, dock'un üstü.
    fn work_area(&self) -> Rect {
        let top = m().panel_h;
        let bottom = self.dock_background().y - 6 * m().unit;
        Rect::new(0, top, self.width, bottom - top)
    }

    /// Yaslanan pencerenin çerçevesi (başlık dahil).
    pub fn snap_frame(&self, snap: Snap) -> Rect {
        let a = self.work_area();
        let half = a.w / 2;
        match snap {
            Snap::Maximize => a,
            Snap::Left => Rect::new(a.x, a.y, half, a.h),
            Snap::Right => Rect::new(a.x + half, a.y, a.w - half, a.h),
        }
    }

    /// Pencereyi ekranın tamamına ya da bir yarısına yaslar; eski konum ve boyut saklanır.
    fn snap_window(&mut self, id: u64, snap: Snap) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let target = self.snap_frame(snap);
        let win = &mut self.windows[index];
        if win.restore.is_none() {
            win.restore = Some((
                win.frame.x,
                win.frame.y,
                win.surface.width,
                win.surface.height,
            ));
        }
        win.snapped = Some(snap);
        self.resize_window(id, target.w, target.h - m().title_h);
        let win = &mut self.windows[index];
        let old = win.footprint();
        win.frame.x = target.x;
        win.frame.y = target.y;
        let new = win.footprint();
        self.damage.add(old);
        self.damage.add(new);
        self.notify_resize(id);
    }

    /// Yaslanmış pencereyi eski boyutuna döndürür; `at` verilirse sol üst köşe oraya gelir.
    fn unsnap_window(&mut self, id: u64, at: Option<(i32, i32)>) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        let Some((x, y, w, h)) = self.windows[index].restore.take() else {
            return;
        };
        self.windows[index].snapped = None;
        self.resize_window(id, w, h);
        let (x, y) = at.unwrap_or((x, y));
        let win = &mut self.windows[index];
        let old = win.footprint();
        win.frame.x = x;
        win.frame.y = y.max(m().panel_h);
        let new = win.footprint();
        self.damage.add(old);
        self.damage.add(new);
        self.notify_resize(id);
    }

    /// Büyüt / eski boyutuna döndür (dock görünür kalır). Yarıya yaslanmış pencere büyür.
    fn toggle_maximize(&mut self, id: u64) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        if self.windows[index].snapped == Some(Snap::Maximize)
            || (self.windows[index].restore.is_some() && self.windows[index].snapped.is_none())
        {
            self.unsnap_window(id, None);
        } else {
            self.snap_window(id, Snap::Maximize);
        }
    }

    /// Sürükleme sırasında imlecin değdiği kenara göre yaslama önizlemesi.
    fn update_snap_preview(&mut self, x: i32, y: i32) {
        let edge = 4;
        let snap = if y <= edge {
            Some(Snap::Maximize)
        } else if x <= edge {
            Some(Snap::Left)
        } else if x >= self.width - 1 - edge {
            Some(Snap::Right)
        } else {
            None
        };
        let next = snap.map(|s| (s, self.snap_frame(s)));
        if next.map(|n| n.0) != self.snap_preview.map(|p| p.0) {
            if let Some((_, r)) = self.snap_preview {
                self.damage.add(r.inset(-4));
            }
            if let Some((_, r)) = next {
                self.damage.add(r.inset(-4));
            }
            self.snap_preview = next;
        }
    }

    // ---- sanal masaüstleri ----

    /// Başka masaüstüne geçer: buradaki açık pencereler park edilir, oradakiler görünür.
    pub fn switch_desktop(&mut self, target: u8) {
        if target >= DESKTOPS || target == self.current_desktop {
            return;
        }
        let current = self.current_desktop;
        self.drag = None;
        self.resize_drag = None;
        self.capture = None;
        self.switcher = None;
        self.snap_preview = None;
        for w in self.windows.iter_mut() {
            if w.desktop == current && w.visible {
                w.visible = false;
                w.parked = true;
            } else if w.desktop == target && w.parked {
                w.visible = true;
                w.parked = false;
            }
        }
        self.current_desktop = target;
        crate::log!("gui: desktop {}", target + 1);
        self.relayout_dock();
        self.full_damage();
        self.update_keyboard_route();
    }

    /// Pencereyi başka masaüstüne taşır (o masaüstüne geçmez).
    fn move_to_desktop(&mut self, id: u64, target: u8) {
        let Some(index) = self.index_of(id) else {
            return;
        };
        if target >= DESKTOPS || self.windows[index].desktop == target {
            return;
        }
        let current = self.current_desktop;
        let win = &mut self.windows[index];
        win.desktop = target;
        if target != current && win.visible {
            win.visible = false;
            win.parked = true;
            let footprint = win.footprint();
            self.damage.add(footprint);
        }
        if let Some(bar) = self.focused().map(|w| w.title_bar()) {
            self.damage.add(bar);
        }
        self.relayout_dock();
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
        self.update_keyboard_route();
    }

    /// Paneldeki masaüstü düğmeleri (durum metninin solunda).
    pub fn desktop_buttons(&self) -> Vec<Rect> {
        let u = m().unit;
        let f = draw::fonts(self.height);
        let size = 22 * u;
        let gap = 6 * u;
        let right = self.width - 16 * u - f.body.measure(&self.status_text) - 18 * u;
        let y = (m().panel_h - size) / 2;
        (0..DESKTOPS as i32)
            .map(|i| {
                let x = right - (DESKTOPS as i32 - i) * (size + gap) + gap;
                Rect::new(x, y, size, size)
            })
            .collect()
    }

    /// Alt+Tab: ilk basışta en son kullanılan ikinci pencereyi seçer, sonra sırayla ilerler.
    fn switch_next(&mut self) {
        match self.switcher.as_mut() {
            Some((order, selected)) => *selected = (*selected + 1) % order.len().max(1),
            None => {
                let order: Vec<u64> = self.windows.iter().rev().map(|w| w.id).collect();
                if order.len() < 2 {
                    return;
                }
                self.switcher = Some((order, 1));
            }
        }
        self.full_damage();
    }

    fn finish_switch(&mut self) {
        if let Some((order, selected)) = self.switcher.take()
            && let Some(index) = order.get(selected).and_then(|id| self.index_of(*id))
        {
            self.set_visible(index, true);
            if let Some(i) = order.get(selected).and_then(|id| self.index_of(*id)) {
                self.raise(i);
            }
        }
        self.full_damage();
    }

    fn drag_window(&mut self, id: u64, x: i32, y: i32) {
        let (width, height) = (self.width, self.height);
        let Some(index) = self.index_of(id) else {
            return;
        };
        let window = &mut self.windows[index];
        let new_x = x.clamp(-window.frame.w + 80, width - 80);
        let new_y = y.clamp(m().panel_h, height - m().title_h);
        if (new_x, new_y) == (window.frame.x, window.frame.y) {
            return;
        }
        let old = window.footprint();
        window.frame.x = new_x;
        window.frame.y = new_y;
        let new = window.footprint();
        self.damage.add(old);
        self.damage.add(new);
    }

    // ---- ayarların uygulanması ----

    fn save_prefs(&self) {
        prefs::save(&self.prefs);
    }

    fn retheme(&mut self) {
        self.theme = Theme::from(&self.prefs);
        self.full_damage();
    }

    fn set_wallpaper(&mut self, surface: Surface) {
        self.backdrop = crate::gfx::glass::backdrop(&surface, BACKDROP_BLUR);
        self.backdrop_gen += 1;
        self.wallpaper = surface;
    }

    fn reload_wallpaper(&mut self) {
        let (w, h, dim) = (self.width, self.height, self.prefs.wallpaper_dim);
        match &self.prefs.wallpaper {
            prefs::Wallpaper::Image(path) => wallpaper::request_image(path, w, h, dim),
            other => {
                if let Some(surface) = wallpaper::render(other, w, h, dim) {
                    self.set_wallpaper(surface);
                    self.full_damage();
                }
            }
        }
    }

    fn update_clock(&mut self) {
        self.clock_text = draw::clock_text(&self.prefs);
        self.status_text = draw::status_text(&self.prefs);
        self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
    }

    fn apply(&mut self, change: Change) {
        match change {
            Change::Set(pref, value) => self.set_pref(pref, value),
            Change::Wallpaper(choice) => {
                if self.prefs.wallpaper != choice {
                    self.prefs.wallpaper = choice;
                    self.reload_wallpaper();
                    self.save_prefs();
                    crate::log!("prefs: wallpaper {:?}", self.prefs.wallpaper);
                }
            }
            Change::Action(action) => self.action(action),
            Change::Release(pref) => self.slider_released(pref),
        }
    }

    fn set_pref(&mut self, pref: Pref, v: i32) {
        let on = v != 0;
        let p = &mut self.prefs;
        match pref {
            Pref::Dark => {
                p.dark = on;
                self.retheme();
            }
            Pref::Accent => {
                p.accent = v as u8;
                self.retheme();
            }
            Pref::PanelOpacity => {
                p.panel_opacity = v.clamp(30, 100) as u8;
                self.retheme();
            }
            Pref::Animations => p.animations = on,
            Pref::Shadows => {
                p.shadows = on;
                self.retheme();
            }
            Pref::WallpaperDim => p.wallpaper_dim = v.clamp(0, 80) as u8,
            Pref::NightLight => {
                p.night_light = on;
                self.full_damage();
            }
            Pref::NightStrength => {
                p.night_strength = v.clamp(10, 80) as u8;
                if p.night_light {
                    self.full_damage();
                }
            }
            Pref::ShowFps => {
                p.show_fps = on;
                self.update_clock();
            }
            Pref::Vsync => {
                p.vsync = on;
                crate::drivers::gpu::vsync::set_enabled(on);
            }
            Pref::StatusItems => {
                p.status_items = on;
                self.update_clock();
            }
            Pref::Parallel => {
                p.parallel = on;
                PARALLEL.store(on, Ordering::Relaxed);
            }
            Pref::ScreenOff => p.screen_off_min = v.clamp(0, 240) as u8,
            Pref::Brightness => {
                p.brightness = Some(v.clamp(5, 100) as u8);
            }
            Pref::Resolution => {
                let mode = ((v as u32) >> 16, (v as u32) & 0xffff);
                if mode != (self.width as u32, self.height as u32) {
                    self.pending_mode = Some(mode);
                }
            }
            Pref::Volume => {
                p.volume = Some(v.clamp(0, 100) as u8);
                p.muted = false;
                audio::set_volume(v.clamp(0, 100) as u32);
            }
            Pref::Muted => {
                if on && p.volume.is_none() {
                    p.volume = Some(audio::volume() as u8);
                }
                p.muted = on;
                audio::set_volume(if on { 0 } else { p.volume.unwrap_or(80) as u32 });
            }
            Pref::UiSounds => p.ui_sounds = on,
            Pref::Profile => {
                p.profile = Some(v.clamp(0, 2) as u8);
                let profile = match v {
                    0 => crate::drivers::cpupower::Profile::Performance,
                    2 => crate::drivers::cpupower::Profile::PowerSave,
                    _ => crate::drivers::cpupower::Profile::Balanced,
                };
                sched::spawn_detached("cpupower", move || {
                    crate::drivers::cpupower::set_profile(profile);
                    0
                });
            }
            Pref::LidAction => {
                p.lid_action = v.clamp(0, 2) as u8;
                crate::drivers::power::set_lid_action(p.lid_action);
            }
            Pref::PowerButton => {
                p.power_button = v.clamp(0, 1) as u8;
                crate::drivers::power::set_power_button_action(p.power_button);
            }
            Pref::Layout => {
                p.layout_us = Some(on);
                keyboard::set_layout(if on {
                    keyboard::Layout::Us
                } else {
                    keyboard::Layout::TurkishQ
                });
            }
            Pref::MouseSpeed => {
                p.mouse_speed = Some(v.clamp(20, 400) as u16);
                mouse::set_speed(v);
            }
            Pref::LeftHanded => {
                p.left_handed = Some(on);
                mouse::set_left_handed(on);
            }
            Pref::Clock24 => {
                p.clock_24h = on;
                self.update_clock();
            }
            Pref::ClockSeconds => {
                p.clock_seconds = on;
                self.update_clock();
            }
            Pref::ShowDate => {
                p.show_date = on;
                self.update_clock();
            }
            Pref::DockSize => {
                p.dock_size = v.clamp(0, 2) as u8;
                self.damage.add(self.dock_area());
                self.relayout_dock();
            }
            Pref::DockMagnify => p.dock_magnify = on,
            Pref::DockRunning => {
                p.dock_running = on;
                self.damage.add(self.dock_area());
                self.relayout_dock();
            }
        }
        crate::log!("prefs: {:?} = {}", pref, v);
        self.save_prefs();
    }

    /// Kaydırıcı bırakılınca uygulanan pahalı işler.
    fn slider_released(&mut self, pref: Pref) {
        match pref {
            Pref::WallpaperDim => self.reload_wallpaper(),
            Pref::Brightness => {
                if let Some(level) = self.prefs.brightness {
                    sched::spawn_detached("brightness", move || {
                        crate::drivers::power::set_brightness(level as u32);
                        0
                    });
                }
            }
            Pref::Volume if self.prefs.ui_sounds => sound(880, 60),
            _ => {}
        }
    }

    fn action(&mut self, action: Action) {
        match action {
            Action::TestSound => {
                sched::spawn_detached("ui-sound", || {
                    audio::beep(660, 140);
                    sched::sleep_ms(40);
                    audio::beep(880, 180);
                    0
                });
            }
            Action::Benchmark => {
                control::set_notice(String::from("Test: ölçülüyor..."));
                sched::spawn_detached("benchmark", || {
                    let text = match benchmark(20) {
                        Some(Benchmark {
                            single, parallel, ..
                        }) => alloc::format!(
                            "Test: tek çekirdek {}.{} ms, tüm çekirdekler {}.{} ms ({}.{}x)",
                            single / 1000,
                            single % 1000 / 100,
                            parallel / 1000,
                            parallel % 1000 / 100,
                            single / parallel.max(1),
                            single * 10 / parallel.max(1) % 10
                        ),
                        None => String::from("Test: masaüstü yok"),
                    };
                    crate::log!("gui: {}", text);
                    control::set_notice(text);
                    0
                });
            }
            Action::DhcpRenew => {
                sched::spawn_detached("dhcp-renew", || {
                    let ok = crate::net::dhcp_renew();
                    control::set_notice(String::from(if ok {
                        "DHCP: yenileme isteği gönderildi"
                    } else {
                        "DHCP: yenilenemedi"
                    }));
                    0
                });
            }
            Action::Logout => {
                let ids: Vec<u64> = self
                    .windows
                    .iter()
                    .filter(|w| matches!(w.kind, Kind::App(_)))
                    .map(|w| w.id)
                    .collect();
                for id in ids {
                    self.send_to_app(id, app::Event::close());
                    if let Some(i) = self.index_of(id) {
                        self.set_visible(i, false);
                    }
                }
                users::end_session();
            }
            Action::Reboot => {
                sched::spawn_detached("reboot", || crate::drivers::power::reboot());
            }
            Action::Shutdown => {
                sched::spawn_detached("shutdown", || crate::drivers::power::shutdown());
            }
            Action::TzMinus | Action::TzPlus => {
                let step = if action == Action::TzPlus { 30 } else { -30 };
                let next = (crate::drivers::rtc::local_offset_minutes() + step).clamp(-720, 840);
                crate::drivers::rtc::set_local_offset_minutes(next);
                self.prefs.tz_minutes = Some(next as i16);
                self.update_clock();
                self.save_prefs();
            }
            Action::Reset => {
                let old = self.prefs.clone();
                self.prefs = Prefs {
                    volume: old.volume,
                    brightness: old.brightness,
                    profile: old.profile,
                    layout_us: old.layout_us,
                    mouse_speed: old.mouse_speed,
                    left_handed: old.left_handed,
                    tz_minutes: old.tz_minutes,
                    resolution: old.resolution,
                    muted: old.muted,
                    ..Prefs::default()
                };
                PARALLEL.store(true, Ordering::Relaxed);
                self.retheme();
                self.reload_wallpaper();
                self.relayout_dock();
                self.update_clock();
                self.save_prefs();
            }
        }
    }

    /// Ekran kipi değişti: düzen, duvar kağıdı ve pencere konumları yeniden kurulur.
    fn resize(&mut self, width: i32, height: i32) {
        self.width = width;
        self.height = height;
        LARGE_UI.store(height >= 1300, Ordering::Relaxed);
        self.launcher = None;
        self.drag = None;
        self.capture = None;
        self.reload_wallpaper();
        if let Some(surface) = wallpaper::render(
            &self.prefs.wallpaper,
            width,
            height,
            self.prefs.wallpaper_dim,
        ) {
            self.set_wallpaper(surface);
        } else if self.wallpaper.width != width || self.wallpaper.height != height {
            self.set_wallpaper(
                wallpaper::render(&prefs::Wallpaper::Preset(0), width, height, 0).unwrap(),
            );
        }
        for w in self.windows.iter_mut() {
            w.frame.h = w.surface.height + m().title_h;
            w.frame.x = w.frame.x.clamp(0, (width - w.frame.w).max(0));
            w.frame.y = w
                .frame
                .y
                .clamp(m().panel_h, (height - w.frame.h).max(m().panel_h));
        }
        self.relayout_dock();
        self.update_clock();
        self.full_damage();
    }

    /// Fare yakınlığına göre dock büyütmesini ilerletir; sürüyorsa `true`.
    fn animate_dock(&mut self) -> bool {
        if !self.prefs.dock_magnify {
            return false;
        }
        // Süreye dayalı yumuşatma: kare hızından bağımsız aynı hızda (τ ≈ 45 ms) ilerler;
        // yavaş sistemde ağır çekim, hızlıda ani sıçrama olmaz.
        const TAU_MS: i32 = 45;
        let dt = (self.now_ms.saturating_sub(self.dock_tick_ms)).clamp(1, 60) as i32;
        self.dock_tick_ms = self.now_ms;
        let cx = self.cursor.0;
        let base = self.base_dock_size();
        let in_zone = self.dock_zone && self.login.is_none();
        let mut moving = false;
        let animate = self.prefs.animations;
        for d in self.dock.iter_mut() {
            let target = dock_zoom_target(d, cx, base, in_zone);
            if d.zoom != target {
                let diff = target - d.zoom;
                let step = if animate {
                    let s = diff * dt / (TAU_MS + dt);
                    if s == 0 { diff.signum() } else { s }
                } else {
                    diff
                };
                d.zoom += step;
                moving = true;
            }
        }
        if moving {
            let before = self.dock_area();
            self.layout_dock_rects();
            self.damage.add(before);
            self.damage.add(self.dock_area());
        }
        moving
    }

    /// Boşta: sıradaki dock simgesinin tüm büyütme boyutlarını önbelleğe çizer (ilk
    /// üzerine gelişte takılma olmasın). Yapılacak iş kaldıysa `true`.
    fn prewarm_dock(&mut self) -> bool {
        let Some(item) = self.dock.get(self.dock_warm) else {
            return false;
        };
        self.dock_warm += 1;
        if let Some(app) = item.app.as_ref().and_then(|id| apps::get(id)) {
            let font = draw::fonts(self.height).title;
            let sizes: Vec<i32> = if self.prefs.dock_magnify {
                (0..=256).map(|z| self.item_size(z)).collect()
            } else {
                alloc::vec![self.base_dock_size()]
            };
            let mut last = 0;
            for size in sizes {
                if size != last {
                    apps::icon_layer(&app, size, font);
                    last = size;
                }
            }
        }
        self.dock_warm < self.dock.len()
    }

    /// Süren animasyon varsa compositor uyumadan sonraki kareyi çizer.
    fn animating(&self) -> bool {
        // Alt+Tab açıkken Alt'ın bırakılmasını yakalamak için kare kare yokla.
        self.switcher.is_some()
            || self
                .launcher
                .as_ref()
                .is_some_and(|l| l.animating(self.now_ms))
            || (self.prefs.dock_magnify
                && self.dock.iter().any(|d| {
                    let in_zone = self.dock_zone && self.login.is_none();
                    d.zoom != dock_zoom_target(d, self.cursor.0, self.base_dock_size(), in_zone)
                }))
    }

    fn update(&mut self, now_ms: u64) {
        self.now_ms = now_ms;
        self.update_desk(now_ms);
        // Arka plandaki katalog taraması yeni/silinmiş uygulama buldu.
        if apps::take_changed() {
            self.relayout_dock();
            if let Some(l) = &self.launcher
                && !l.closing()
            {
                let area = l.footprint();
                self.damage.add(area);
                let mut l = launcher::Launcher::new(now_ms, false);
                l.render_if_needed(
                    &self.theme,
                    &draw::fonts(self.height),
                    &self.backdrop,
                    self.backdrop_gen,
                );
                self.damage.add(l.footprint());
                self.launcher = Some(l);
            }
        }
        if self.switcher.is_some() && !keyboard::alt_down() {
            self.finish_switch();
        }
        self.update_session();
        let minute = now_ms / 60_000;
        if self.login.is_some() && minute != self.last_minute {
            self.last_minute = minute;
            self.full_damage();
        }

        // Başlatıcı animasyonu ve kapanışı.
        if let Some(l) = self.launcher.as_mut() {
            l.render_if_needed(
                &self.theme,
                &draw::fonts(self.height),
                &self.backdrop,
                self.backdrop_gen,
            );
            if l.animating(now_ms) {
                let area = l.footprint();
                self.damage.add(area);
            }
            if l.progress(now_ms).is_none() {
                let area = l.footprint();
                self.damage.add(area);
                self.launcher = None;
            }
        }
        self.animate_dock();

        if let Some((path, w, h, surface)) = wallpaper::take_image()
            && self.prefs.wallpaper == prefs::Wallpaper::Image(path)
            && (w, h) == (self.width, self.height)
            && let Some(surface) = surface
        {
            self.set_wallpaper(surface);
            self.full_damage();
        }

        // Ekran koruma: hareketsizlikte ekranı karart.
        let idle_limit = self.prefs.screen_off_min as u64 * 60_000;
        if idle_limit > 0
            && !self.blanked
            && now_ms.saturating_sub(self.last_input_ms) >= idle_limit
        {
            self.blanked = true;
            self.full_damage();
            crate::log!(
                "gui: screen blanked after {} min idle",
                self.prefs.screen_off_min
            );
        }

        let second = now_ms / 1000;
        if second != self.last_second {
            self.last_second = second;
            let clock = draw::clock_text(&self.prefs);
            let status = draw::status_text(&self.prefs);
            if clock != self.clock_text || status != self.status_text {
                self.clock_text = clock;
                self.status_text = status;
                self.damage.add(Rect::new(0, 0, self.width, m().panel_h));
            }
        }

        let dead: Vec<(u64, u64)> = self
            .windows
            .iter()
            .filter_map(|w| match w.kind {
                Kind::App(pid) if !sched::process::is_running(pid) => Some((w.id, pid)),
                _ => None,
            })
            .collect();
        for (id, pid) in dead {
            self.remove_window(id);
            app::forget_process(pid);
            apps::forget(pid);
        }
    }

    /// Hasar gören bölgeleri arka tampona birleştirir ve ekrana kopyalar. Bölgeler ayrık
    /// şeritlere bölünüp tüm çekirdeklerde paralel çizilir.
    fn render(&mut self, display: &mut display::Display) -> bool {
        if !self.compose_frame(display) {
            return false;
        }
        display.present();
        true
    }

    /// Hasarlı bölgeleri arka tampona çizer (ekrana kopyalamadan); kopyalanacak iş varsa `true`.
    fn compose_frame(&mut self, display: &mut display::Display) -> bool {
        display.set_cursor((!self.blanked).then_some(self.cursor));
        let bounds = display.back.bounds();
        let rects: Vec<Rect> = self
            .damage
            .take()
            .iter()
            .map(|r| r.intersect(&bounds))
            .filter(|r| !r.is_empty())
            .collect();
        let bands = crate::gfx::split_bands(&rects, RENDER_BAND);
        let mut pixels = 0u64;
        for band in &bands {
            display.mark(*band);
            pixels += band.area() as u64;
        }
        compose(self, &mut display.back, &bands);
        if !display.has_work() {
            return false;
        }
        if pixels == 0 {
            CURSOR_ONLY.fetch_add(1, Ordering::Relaxed);
        }
        SCENE_PIXELS.fetch_add(pixels, Ordering::Relaxed);
        true
    }

    /// Klibi tamamen örten en üstteki opak pencere: (duvar kağıdı gerekli mi, ilk çizilecek pencere).
    pub fn visible_from(&self, clip: &Rect) -> (bool, usize) {
        let visible: Vec<usize> = (0..self.windows.len())
            .filter(|&i| self.windows[i].visible)
            .collect();
        match visible
            .iter()
            .rposition(|&i| contains(&self.windows[i].opaque(), clip))
        {
            Some(pos) => (false, visible[pos]),
            None => (true, 0),
        }
    }
}

/// Kaydedilmiş sistem ayarlarını sürücülere uygular (yalnızca dosyada bulunanlar).
fn apply_system_prefs(p: &Prefs) {
    crate::drivers::gpu::vsync::set_enabled(p.vsync);
    if let Some(v) = p.volume {
        audio::set_volume(if p.muted { 0 } else { v as u32 });
    } else if p.muted {
        audio::set_volume(0);
    }
    if let Some(us) = p.layout_us {
        keyboard::set_layout(if us {
            keyboard::Layout::Us
        } else {
            keyboard::Layout::TurkishQ
        });
    }
    if let Some(v) = p.mouse_speed {
        mouse::set_speed(v as i32);
    }
    if let Some(v) = p.left_handed {
        mouse::set_left_handed(v);
    }
    if let Some(v) = p.tz_minutes {
        crate::drivers::rtc::set_local_offset_minutes(v as i64);
    }
    crate::drivers::power::set_lid_action(p.lid_action);
    crate::drivers::power::set_power_button_action(p.power_button);
    PARALLEL.store(p.parallel, Ordering::Relaxed);
    if let Some(level) = p.brightness {
        sched::spawn_detached("brightness", move || {
            crate::drivers::power::set_brightness(level as u32);
            0
        });
    }
    if let Some(v) = p.profile {
        let profile = match v {
            0 => crate::drivers::cpupower::Profile::Performance,
            2 => crate::drivers::cpupower::Profile::PowerSave,
            _ => crate::drivers::cpupower::Profile::Balanced,
        };
        sched::spawn_detached("cpupower", move || {
            crate::drivers::cpupower::set_profile(profile);
            0
        });
    }
}

/// Çalışan masaüstünün çözünürlüğünü değiştirir ve düzeni yeniden kurar.
fn change_mode(width: u32, height: u32) {
    match crate::drivers::gpu::set_mode_live(width, height) {
        Ok(()) => {
            console::enter_window_mode(TERMINAL_COLUMNS, TERMINAL_ROWS);
            let (w, h) = display::with(|d| d.size()).unwrap_or((width as i32, height as i32));
            mouse::set_bounds(w, h);
            let mut guard = DESKTOP.lock();
            if let Some(desktop) = guard.as_mut() {
                desktop.resize(w, h);
                desktop.prefs.resolution = Some((w as u32, h as u32));
                desktop.save_prefs();
                desktop.cursor = (w / 2, h / 2);
            }
            crate::log!("gui: resolution changed to {}x{}", w, h);
        }
        Err(crate::drivers::gpu::RESTART_REQUIRED) => {
            control::set_notice(alloc::format!(
                "Çözünürlük {}×{} yeniden başlatınca uygulanacak",
                width,
                height
            ));
        }
        Err(e) => {
            crate::log!("gui: resolution {}x{} failed: {}", width, height, e);
            control::set_notice(alloc::format!("Çözünürlük değiştirilemedi: {}", e));
        }
    }
}

/// Uygulamalar için ayarlar ve canlı bilgi (`SYS_INFO` konu 2); masaüstü yoksa `None`.
pub fn query() -> Option<String> {
    let prefs = DESKTOP.lock().as_ref()?.prefs.clone();
    Some(control::query(&prefs))
}

/// Uygulamadan gelen ayar değişikliği ya da eylem (`SYS_DESKTOP_SET`).
pub fn set(key: &str, value: &str) -> Result<(), &'static str> {
    let change = control::parse(key, value).ok_or("unknown setting")?;
    {
        let mut guard = DESKTOP.lock();
        let desktop = guard.as_mut().ok_or("desktop is not running")?;
        desktop.apply(change);
    }
    wake();
    Ok(())
}

/// Masaüstü kapanana kadar bekler.
pub fn wait_stopped() {
    let done = COMPLETION.lock().clone();
    if let Some(done) = done {
        done.wait();
    }
    while RUNNING.load(Ordering::Relaxed) {
        sched::sleep_ms(20);
    }
}

/// Paralel çizimde bir işçinin aldığı şerit yüksekliği (satır).
const RENDER_BAND: i32 = 64;

/// Çok çekirdekli çizimi kapatmak için (karşılaştırma ölçümleri).
static PARALLEL: AtomicBool = AtomicBool::new(true);

/// Sahneyi verilen ayrık şeritlere çizer (paralelse tüm çekirdeklerde).
fn compose(desktop: &Desktop, back: &mut crate::gfx::Surface, bands: &[Rect]) {
    let (pixels, width, height) = (back.pixels.as_mut_ptr() as usize, back.width, back.height);
    let desktop = desktop as *const Desktop as usize;
    let draw_band = |i: usize| {
        // Şeritler ayrıktır ve sahne çizilirken masaüstü değişmez (DESKTOP kilidi tutuluyor).
        let mut canvas =
            unsafe { crate::gfx::Canvas::from_raw(pixels as *mut u32, width, height, bands[i]) };
        draw::scene(unsafe { &*(desktop as *const Desktop) }, &mut canvas);
    };
    if PARALLEL.load(Ordering::Relaxed) {
        crate::gfx::parallel::for_each(bands.len(), &draw_band);
    } else {
        (0..bands.len()).for_each(draw_band);
    }
}

/// Tam ekran sahne süreleri (µs, kare başına).
pub struct Benchmark {
    pub single: u64,
    pub parallel: u64,
    /// Paralel ölçümde yalnızca sahne çizimi ve yalnızca ekrana kopyalama.
    pub compose: u64,
    pub present: u64,
}

/// Tam ekran sahne çizimi + kopyalama süresi: tek çekirdek ve tüm çekirdekler.
pub fn benchmark(frames: u32) -> Option<Benchmark> {
    let mut guard = DESKTOP.lock();
    let desktop = guard.as_mut()?;
    let frames = frames.max(1) as u64;
    let measure = |parallel: bool| {
        PARALLEL.store(parallel, Ordering::Relaxed);
        let (mut compose_ns, mut present_ns) = (0, 0);
        for _ in 0..frames {
            display::with(|d| {
                let all = d.back.bounds();
                let bands = crate::gfx::split_bands(&[all], RENDER_BAND);
                let start = tsc::now_ns();
                compose(desktop, &mut d.back, &bands);
                let mid = tsc::now_ns();
                d.mark_all();
                d.present();
                compose_ns += mid - start;
                present_ns += tsc::now_ns() - mid;
            });
        }
        (compose_ns / frames / 1000, present_ns / frames / 1000)
    };
    let single = measure(false);
    let parallel = measure(true);
    PARALLEL.store(true, Ordering::Relaxed);
    Some(Benchmark {
        single: single.0 + single.1,
        parallel: parallel.0 + parallel.1,
        compose: parallel.0,
        present: parallel.1,
    })
}

/// En üstteki pencereyi kare kare sürükler (çapraz gidiş-dönüş); kare başına ortalama
/// çizim + kopyalama süresi (µs). Pencere sonunda eski yerine konur.
pub fn drag_benchmark(frames: u32) -> Option<u64> {
    let mut guard = DESKTOP.lock();
    let desktop = guard.as_mut()?;
    let window = desktop.windows.iter().rev().find(|w| w.visible)?;
    let (id, x0, y0) = (window.id, window.frame.x, window.frame.y);
    let frames = frames.max(2);
    let mut total = 0;
    for i in 0..frames {
        let step = (i % 40) as i32;
        let offset = if step < 20 { step * 8 } else { (40 - step) * 8 };
        desktop.drag_window(id, x0 + offset, y0 + offset / 2);
        let start = tsc::now_ns();
        display::with(|d| desktop.render(d));
        total += tsc::now_ns() - start;
    }
    desktop.drag_window(id, x0, y0);
    display::with(|d| desktop.render(d));
    Some(total / frames as u64 / 1000)
}

/// İmleci dock üzerinde soldan sağa gezdirir (16 ms'lik adımlarla): kare başına ortalama
/// ve en kötü süre (µs). `cold` ise simge önbelleği önce boşaltılır (ilk üzerine geliş).
pub fn dock_benchmark(frames: u32, cold: bool) -> Option<(u64, u64)> {
    let mut guard = DESKTOP.lock();
    let desktop = guard.as_mut()?;
    if cold {
        apps::clear_icon_cache();
    }
    let bg = desktop.dock_background();
    let (saved_cursor, saved_zone, saved_now) = (desktop.cursor, desktop.dock_zone, desktop.now_ms);
    let frames = frames.max(2);
    let (mut total, mut worst) = (0, 0);
    let mut now = desktop.now_ms;
    for i in 0..frames {
        now += 16;
        desktop.now_ms = now;
        desktop.dock_zone = true;
        desktop.cursor = (bg.x + bg.w * i as i32 / frames as i32, bg.y + bg.h / 2);
        let start = tsc::now_ns();
        desktop.animate_dock();
        display::with(|d| desktop.render(d));
        let took = tsc::now_ns() - start;
        total += took;
        worst = worst.max(took);
    }
    desktop.cursor = saved_cursor;
    desktop.dock_zone = saved_zone;
    desktop.now_ms = saved_now.max(now);
    desktop.animate_dock();
    Some((total / frames as u64 / 1000, worst / 1000))
}

/// Compositor'ı uyandırır (fare/klavye kesmeleri, konsol, uygulamalar).
pub fn wake() {
    if RUNNING.load(Ordering::Relaxed) {
        PENDING.store(true, Ordering::Release);
        WAKE.wake_all();
    }
}

fn compositor() -> i64 {
    let Some((width, height)) = display::with(|d| d.size()) else {
        return -1;
    };
    let prefs = prefs::load();
    apply_system_prefs(&prefs);
    // Kaydedilmiş çözünürlük (kart destekliyorsa).
    let (width, height) = match prefs.resolution {
        // UEFI kipini yükleyici zaten display.conf'tan kurdu.
        Some((w, h))
            if (w as i32, h as i32) != (width, height)
                && crate::drivers::gpu::adapter() != crate::drivers::gpu::Adapter::Firmware =>
        {
            match crate::drivers::gpu::set_mode_live(w, h) {
                Ok(()) => {
                    console::enter_window_mode(TERMINAL_COLUMNS, TERMINAL_ROWS);
                    display::with(|d| d.size()).unwrap_or((width, height))
                }
                Err(e) => {
                    crate::log!("gui: saved resolution {}x{} not applied: {}", w, h, e);
                    (width, height)
                }
            }
        }
        _ => (width, height),
    };
    mouse::set_bounds(width, height);
    let mut desktop = Desktop::new(width, height, prefs);
    desktop.cursor = (mouse::state().x, mouse::state().y);
    desktop.update_keyboard_route();
    *DESKTOP.lock() = Some(desktop);
    if users::session().is_some() {
        apps::spawn("terminal");
    }
    crate::log!("gui: desktop started ({}x{})", width, height);

    let mut window_start = tsc::now_ns();
    let mut frames_in_window = 0u64;
    let mut busy_ns = 0u64;
    let mut prewarmed_gen = u64::MAX;
    while !STOP.load(Ordering::Relaxed) {
        crate::kernel::watchdog::gui_beat();
        PENDING.store(false, Ordering::Release);
        let start = tsc::now_ns();
        let (presented, animating, mode) = {
            let mut guard = DESKTOP.lock();
            let desktop = guard.as_mut().unwrap();
            desktop.handle_input();
            desktop.update(pit::uptime_ms());
            let presented = display::with(|d| desktop.compose_frame(d)).unwrap_or(false);
            let animating = desktop.animating();
            // Başlatıcının camı boşta önceden hesaplanır (ilk açılış beklemesin).
            if !presented && !animating && desktop.prewarm_dock() {
                PENDING.store(true, Ordering::Release);
            }
            if !presented
                && !animating
                && desktop.launcher.is_none()
                && desktop.backdrop_gen != prewarmed_gen
            {
                prewarmed_gen = desktop.backdrop_gen;
                launcher::Launcher::prewarm(
                    &desktop.theme,
                    &draw::fonts(desktop.height),
                    &desktop.backdrop,
                    desktop.backdrop_gen,
                );
            }
            (presented, animating, desktop.pending_mode.take())
        };
        // Kopyalama masaüstü kilidi dışında ve (varsa) dikey boşlukta başlar.
        let mut waited = 0;
        if presented {
            let before = tsc::now_ns();
            crate::drivers::gpu::vsync::wait();
            waited = tsc::now_ns() - before;
            display::with(|d| d.present());
        }
        if let Some((w, h)) = mode {
            change_mode(w, h);
        }
        if animating {
            PENDING.store(true, Ordering::Release);
        }
        let end = tsc::now_ns();
        if presented {
            busy_ns += end - start - waited;
            frames_in_window += 1;
            FRAMES.fetch_add(1, Ordering::Relaxed);
        }
        if end - window_start >= 1_000_000_000 {
            let elapsed = end - window_start;
            FPS.store(
                (frames_in_window * 1_000_000_000 + elapsed / 2) / elapsed,
                Ordering::Relaxed,
            );
            FRAME_TIME_US.store(busy_ns / frames_in_window.max(1) / 1000, Ordering::Relaxed);
            window_start = end;
            frames_in_window = 0;
            busy_ns = 0;
        }

        // Kare sınırı yok: yeni iş varsa hemen sonraki kare çizilir (uygulamalara sıra verilerek).
        if presented && PENDING.load(Ordering::Acquire) {
            sched::yield_now();
            continue;
        }
        let until_second = 1000 - pit::uptime_ms() % 1000;
        WAKE.wait_timeout(
            || PENDING.load(Ordering::Acquire) || STOP.load(Ordering::Relaxed),
            until_second.max(1),
        );
    }

    crate::kernel::watchdog::gui_stopped();
    if let Some(desktop) = DESKTOP.lock().take() {
        for window in desktop.windows {
            if let Some(channel) = window.channel {
                channel.close();
            }
        }
    }
    keyboard::route_to_gui(false);
    display::with(|d| d.set_cursor(None));
    console::leave_window_mode();
    RUNNING.store(false, Ordering::Relaxed);
    crate::log!(
        "gui: desktop stopped after {} frames",
        FRAMES.load(Ordering::Relaxed)
    );
    0
}

pub fn start() -> Result<(), &'static str> {
    if !display::is_ready() {
        return Err("display is not initialised");
    }
    if RUNNING.swap(true, Ordering::Relaxed) {
        return Err("desktop is already running");
    }
    if !mouse::present() {
        crate::log!("gui: no mouse, the desktop is keyboard-only");
    }
    STOP.store(false, Ordering::Relaxed);
    FRAMES.store(0, Ordering::Relaxed);
    console::enter_window_mode(TERMINAL_COLUMNS, TERMINAL_ROWS);
    console::set_muted(false);
    match sched::spawn("compositor", compositor) {
        Some((_, done)) => {
            *COMPLETION.lock() = Some(done);
            Ok(())
        }
        None => {
            RUNNING.store(false, Ordering::Relaxed);
            console::leave_window_mode();
            Err("could not start the compositor thread")
        }
    }
}

pub fn stop() {
    STOP.store(true, Ordering::Relaxed);
    WAKE.wake_all();
    let done = COMPLETION.lock().take();
    if let Some(done) = done {
        done.wait();
    }
}

pub fn is_running() -> bool {
    RUNNING.load(Ordering::Relaxed)
}

pub struct Stats {
    pub fps: u64,
    pub frame_time_us: u64,
    pub frames: u64,
    pub scene_pixels: u64,
    pub cursor_only_frames: u64,
}

pub fn stats() -> Stats {
    Stats {
        fps: FPS.load(Ordering::Relaxed),
        frame_time_us: FRAME_TIME_US.load(Ordering::Relaxed),
        frames: FRAMES.load(Ordering::Relaxed),
        scene_pixels: SCENE_PIXELS.load(Ordering::Relaxed),
        cursor_only_frames: CURSOR_ONLY.load(Ordering::Relaxed),
    }
}
