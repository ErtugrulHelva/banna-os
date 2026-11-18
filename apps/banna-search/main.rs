//! Banna Search: Banna OS'un web tarayıcısı. Kendi motoruyla (HTML5, CSS, JavaScript —
//! `banna::web`) çalışır; arayüz sayfanın üstünde yüzen sıvı cam çubuklardan oluşur.
//!
//! Sekmeler, adres/arama çubuğu, Firefox tarzı başlangıç sayfası, yer imleri, geçmiş,
//! geliştirici konsolu (F12), ayarlar ve Banna hesabıyla (E-Banna adresi ya da E-Kimlik)
//! oturum açma. Durum: `~/.config/banna-search/` (ayarlar, yer imleri, geçmiş, depolama).

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::ecosystem::{Client, Login};
use banna::gfx::text::Font;
use banna::gfx::{Argb, Canvas, Glass, Rect, Surface, rgb, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, EVENT_WHEEL,
    KEY_CTRL, KEY_DOWN, KEY_F1, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_PASTE, KEY_SHIFT, KEY_UP, Window,
    clipboard,
};
use banna::theme::{Prefs, Theme};
use banna::ui::{self, Hit, TextInput, Ui};
use banna::web::cookies;
use banna::web::layout::{Fonts, Item};
use banna::web::page::{Action, Page, Stage};
use banna::{desktop, fs, process, time};

banna::entry!(main);

const VERSION: &str = "1.1.0";

macro_rules! font {
    ($file:literal) => {
        Font::parse(include_bytes!(concat!("fonts/", $file)))
    };
}

static REGULAR: [Font; 18] = [
    font!("r11.bin"), font!("r12.bin"), font!("r13.bin"), font!("r14.bin"), font!("r15.bin"),
    font!("r16.bin"), font!("r18.bin"), font!("r20.bin"), font!("r22.bin"), font!("r24.bin"),
    font!("r26.bin"), font!("r28.bin"), font!("r32.bin"), font!("r36.bin"), font!("r40.bin"),
    font!("r48.bin"), font!("r56.bin"), font!("r64.bin"),
];
static BOLD: [Font; 18] = [
    font!("b11.bin"), font!("b12.bin"), font!("b13.bin"), font!("b14.bin"), font!("b15.bin"),
    font!("b16.bin"), font!("b18.bin"), font!("b20.bin"), font!("b22.bin"), font!("b24.bin"),
    font!("b26.bin"), font!("b28.bin"), font!("b32.bin"), font!("b36.bin"), font!("b40.bin"),
    font!("b48.bin"), font!("b56.bin"), font!("b64.bin"),
];

struct WebFonts;

impl Fonts for WebFonts {
    fn pick(&self, px: f32, bold: bool) -> &'static Font {
        let set = if bold { &BOLD } else { &REGULAR };
        set.iter()
            .min_by_key(|f| ((f.size as f32 - px).abs() * 10.0) as i32)
            .unwrap()
    }
}

fn font(px: i32, bold: bool) -> &'static Font {
    WebFonts.pick(px as f32, bold)
}

const ENGINES: [(&str, &str); 4] = [
    ("DuckDuckGo", "https://html.duckduckgo.com/html/?q="),
    ("Google", "https://www.google.com/search?q="),
    ("Bing", "https://www.bing.com/search?q="),
    ("Vikipedi", "https://tr.wikipedia.org/w/index.php?search="),
];

const START: &str = "banna:start";

/// Ctrl + / Ctrl - adımları (%).
const ZOOM_STEPS: [u32; 10] = [50, 67, 80, 90, 100, 110, 125, 150, 175, 200];
/// Adres çubuğu öneri sayısı.
const SUGGESTIONS: usize = 6;

struct Settings {
    home: String,
    engine: usize,
    javascript: bool,
    images: bool,
    zoom: u32,
    dark: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            home: String::from(START),
            engine: 0,
            javascript: true,
            images: true,
            zoom: 100,
            dark: false,
        }
    }
}

struct Tab {
    page: Option<Page>,
    url: String,
    title: String,
    history: Vec<String>,
    pos: usize,
    error: Option<String>,
    /// Yükleme sürüyor (sayfa yükleme adımları kalmış).
    loading: bool,
    /// Son yerleşimin zamanı (ms; görseller gelirken seyreltmek için).
    laid_out_at: u64,
}

impl Tab {
    fn new() -> Self {
        Self {
            page: None,
            url: String::new(),
            title: String::from("Yeni sekme"),
            history: Vec::new(),
            pos: 0,
            error: None,
            loading: false,
            laid_out_at: 0,
        }
    }
}

#[derive(Clone, PartialEq)]
enum Control {
    Tab(usize),
    CloseTab(usize),
    NewTab,
    Back,
    Forward,
    Reload,
    Home,
    Address,
    Star,
    Account,
    Menu,
    MenuItem(u8),
    Setting(u8),
    Engine(usize),
    Zoom(u32),
    LoginField(usize),
    LoginSubmit,
    Logout,
    CloseOverlay,
    ConsoleClear,
    Suggest(usize),
    FindPrev,
    FindNext,
    FindClose,
}

#[derive(Clone, Copy, PartialEq)]
enum Overlay {
    None,
    Menu,
    Settings,
    Account,
    Console,
}

struct App {
    window: Window,
    u: i32,
    tabs: Vec<Tab>,
    current: usize,
    address: TextInput,
    address_focus: bool,
    overlay: Overlay,
    settings: Settings,
    bookmarks: Vec<(String, String)>,
    history: Vec<(String, String)>,
    client: Client,
    account: Option<String>,
    login_fields: [TextInput; 2],
    login_focus: usize,
    login_message: Option<String>,
    pending_login: Option<(String, String)>,
    last_poll: u64,
    console: Vec<(u8, String)>,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    hover_link: Option<String>,
    toast: Option<(String, u64)>,
    caret_on: bool,
    dirty: bool,
    /// Sayfada bul (Ctrl+F).
    find: Option<Find>,
    /// Adres çubuğu önerileri (adres, başlık) ve seçili olan.
    suggestions: Vec<(String, String)>,
    suggest_sel: Option<usize>,
    /// Kapatılan sekmelerin adresleri (yeniden açmak için).
    closed: Vec<String>,
    /// Adres alanının son çizildiği yer (öneri listesi altına açılır).
    address_rect: Rect,
}

/// Sayfada bul: aranan metin ve eşleşmelerin sayfa koordinatlarındaki kutuları.
struct Find {
    input: TextInput,
    /// Her eşleşmenin kutuları (birden çok sözcüğe yayılabilir).
    matches: Vec<Vec<(f32, f32, f32, f32)>>,
    current: usize,
}

fn home() -> String {
    format!("/home/{}", process::username())
}

fn config_dir() -> String {
    format!("{}/.config/banna-search", home())
}

fn mkdir_p(path: &str) {
    let mut at = String::new();
    for part in path.split('/').filter(|p| !p.is_empty()) {
        at.push('/');
        at.push_str(part);
        let _ = fs::mkdir(&at);
    }
}

fn read_lines(name: &str) -> Vec<String> {
    fs::read(&format!("{}/{}", config_dir(), name))
        .map(|d| String::from_utf8_lossy(&d).lines().map(String::from).collect())
        .unwrap_or_default()
}

fn write_lines(name: &str, lines: &[String]) {
    mkdir_p(&config_dir());
    let mut text = lines.join("\n");
    text.push('\n');
    let _ = fs::write(&format!("{}/{}", config_dir(), name), text.as_bytes());
}

fn escape_html(s: &str) -> String {
    banna::web::dom::escape(s)
}

fn url_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        let c = b as char;
        if c.is_ascii_alphanumeric() || "-_.~".contains(c) {
            out.push(c);
        } else if c == ' ' {
            out.push('+');
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn url_decode(s: &str) -> String {
    banna::web::js::builtins::uri_decode(&s.replace('+', " "))
}

/// Adres çubuğu metnini adrese çevirir (arama ya da site).
fn to_url(text: &str, engine: usize) -> String {
    let t = text.trim();
    if t.contains("://") || t.starts_with("banna:") || t.starts_with("data:") || t.starts_with("about:") {
        return String::from(t);
    }
    let looks_like_host = !t.contains(' ')
        && (t.contains('.') || t.starts_with("localhost"))
        && !t.ends_with('.');
    if looks_like_host {
        return format!("https://{}", t);
    }
    format!("{}{}", ENGINES[engine].1, url_encode(t))
}

fn host_of(url: &str) -> String {
    banna::web::url::Url::parse(url).map(|u| u.host).unwrap_or_default()
}

fn downloads_dir() -> String {
    format!("{}/İndirilenler", home())
}

/// Yanıt sayfa değil de indirilecek dosya mı (ek olarak gönderilmiş ya da gösterilemeyen tür)?
fn is_download(content_type: &str, disposition: &str) -> bool {
    if disposition.to_ascii_lowercase().trim_start().starts_with("attachment") {
        return true;
    }
    let ct = content_type.to_ascii_lowercase();
    let ct = ct.split(';').next().unwrap_or("").trim();
    !(ct.is_empty()
        || ct.contains("html")
        || ct.contains("xml")
        || ct.starts_with("text/")
        || ct.starts_with("image/")
        || ct.contains("json")
        || ct.contains("javascript"))
}

/// İndirilen dosyanın adı: Content-Disposition'daki `filename`, yoksa adresin son parçası.
fn download_name(url: &str, disposition: &str) -> String {
    let from_header = disposition.split(';').find_map(|p| {
        let (k, v) = p.split_once('=')?;
        let k = k.trim().to_ascii_lowercase();
        if k == "filename*" {
            // filename*=UTF-8''ad%C4%B1.pdf
            Some(url_decode(v.trim().rsplit("''").next().unwrap_or(v)))
        } else if k == "filename" {
            Some(String::from(v.trim().trim_matches('"')))
        } else {
            None
        }
    });
    let from_url = || {
        let path = url.split(['?', '#']).next().unwrap_or("");
        url_decode(path.rsplit('/').next().unwrap_or(""))
    };
    let name: String = from_header
        .filter(|n| !n.is_empty())
        .unwrap_or_else(from_url)
        .chars()
        .map(|c| if c == '/' || c == '\\' || c.is_control() { '_' } else { c })
        .collect();
    let name = String::from(name.trim().trim_start_matches('.'));
    if name.is_empty() { String::from("indirilen-dosya") } else { name }
}

/// Aynı adlı dosya varsa "ad (2).uzantı".
fn unique_path(dir: &str, name: &str) -> String {
    let path = format!("{}/{}", dir, name);
    if fs::stat(&path).is_err() {
        return path;
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    (2..1000)
        .map(|n| format!("{}/{} ({}){}", dir, stem, n, ext))
        .find(|p| fs::stat(p).is_err())
        .unwrap_or(path)
}

fn size_text(bytes: usize) -> String {
    if bytes >= 1 << 20 {
        format!("{}.{} MB", bytes >> 20, ((bytes % (1 << 20)) * 10) >> 20)
    } else if bytes >= 1 << 10 {
        format!("{} KB", bytes >> 10)
    } else {
        format!("{} bayt", bytes)
    }
}

impl App {
    fn tab(&mut self) -> &mut Tab {
        &mut self.tabs[self.current]
    }

    fn scale(&self) -> f32 {
        self.u as f32 * self.settings.zoom as f32 / 100.0
    }

    fn chrome_h(&self) -> i32 {
        96 * self.u
    }

    fn page_area(&self) -> Rect {
        Rect::new(0, 0, self.window.width as i32, self.window.height as i32)
    }

    fn viewport(&self) -> (f32, f32) {
        let area = self.page_area();
        let s = self.scale();
        (area.w as f32 / s, (area.h - self.chrome_h()) as f32 / s)
    }

    // ---- kalıcı durum ----

    fn load_state(&mut self) {
        for line in read_lines("settings") {
            let Some((k, v)) = line.split_once('=') else { continue };
            match k {
                "home" => self.settings.home = String::from(v),
                "engine" => self.settings.engine = v.parse().unwrap_or(0).min(ENGINES.len() - 1),
                "javascript" => self.settings.javascript = v == "1",
                "images" => self.settings.images = v == "1",
                "zoom" => self.settings.zoom = v.parse().unwrap_or(100).clamp(50, 200),
                "dark" => self.settings.dark = v == "1",
                _ => {}
            }
        }
        self.bookmarks = read_lines("bookmarks")
            .into_iter()
            .filter_map(|l| l.split_once('\t').map(|(u, t)| (String::from(u), String::from(t))))
            .collect();
        self.history = read_lines("history")
            .into_iter()
            .filter_map(|l| l.split_once('\t').map(|(u, t)| (String::from(u), String::from(t))))
            .collect();
        if let Ok(data) = fs::read(&format!("{}/cookies", config_dir())) {
            cookies::load(&String::from_utf8_lossy(&data));
        }
        let _ = cookies::take_dirty();
    }

    /// Kalıcı çerezler değiştiyse diske yazar (0600: oturum çerezleri parola gibidir).
    fn save_cookies(force: bool) {
        if !cookies::take_dirty() && !force {
            return;
        }
        mkdir_p(&config_dir());
        let path = format!("{}/cookies", config_dir());
        let _ = fs::write(&path, cookies::save().as_bytes());
        let _ = fs::chmod(&path, 0o600);
    }

    fn save_settings(&self) {
        let s = &self.settings;
        write_lines(
            "settings",
            &[
                format!("home={}", s.home),
                format!("engine={}", s.engine),
                format!("javascript={}", s.javascript as u8),
                format!("images={}", s.images as u8),
                format!("zoom={}", s.zoom),
                format!("dark={}", s.dark as u8),
            ],
        );
    }

    fn save_list(name: &str, list: &[(String, String)]) {
        let lines: Vec<String> = list
            .iter()
            .map(|(u, t)| format!("{}\t{}", u, t.replace(['\t', '\n'], " ")))
            .collect();
        write_lines(name, &lines);
    }

    fn storage_file(url: &str) -> String {
        let host: String = host_of(url)
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' })
            .collect();
        format!("{}/storage/{}", config_dir(), if host.is_empty() { String::from("local") } else { host })
    }

    fn save_storage(page: &Page) {
        if !page.interp.storage_dirty {
            return;
        }
        mkdir_p(&format!("{}/storage", config_dir()));
        let mut text = String::new();
        for (k, v) in page.interp.storage.iter().filter(|(k, _)| k.starts_with("l:")) {
            text.push_str(&format!("{}\t{}\n", k, v.replace('\\', "\\\\").replace('\n', "\\n")));
        }
        let _ = fs::write(&Self::storage_file(page.url()), text.as_bytes());
    }

    fn load_storage(page: &mut Page) {
        if let Ok(data) = fs::read(&Self::storage_file(page.url())) {
            for line in String::from_utf8_lossy(&data).lines() {
                if let Some((k, v)) = line.split_once('\t') {
                    page.interp.storage.push((String::from(k), v.replace("\\n", "\n").replace("\\\\", "\\")));
                }
            }
        }
    }

    // ---- iç sayfalar ----

    fn start_page(&self) -> String {
        let tiles: Vec<(String, String)> = if self.bookmarks.is_empty() {
            [
                ("https://tr.wikipedia.org", "Vikipedi"),
                ("https://ecosys.trybanna.com", "Banna"),
                ("https://html.duckduckgo.com/html/", "DuckDuckGo"),
                ("https://news.ycombinator.com", "Hacker News"),
                ("https://github.com", "GitHub"),
                ("https://www.mozilla.org/tr/firefox/", "Firefox"),
            ]
            .iter()
            .map(|(u, t)| (String::from(*u), String::from(*t)))
            .collect()
        } else {
            self.bookmarks.iter().take(8).cloned().collect()
        };
        let mut tiles_html = String::new();
        for (url, title) in &tiles {
            let letter = title.chars().next().unwrap_or('?').to_uppercase().collect::<String>();
            tiles_html.push_str(&format!(
                "<a class=tile href=\"{}\"><div class=icon>{}</div><div class=name>{}</div></a>",
                escape_html(url),
                escape_html(&letter),
                escape_html(title)
            ));
        }
        let mut recent = String::new();
        for (url, title) in self.history.iter().rev().take(6) {
            recent.push_str(&format!(
                "<li><a href=\"{}\">{}</a> <span class=host>{}</span></li>",
                escape_html(url),
                escape_html(if title.is_empty() { url } else { title }),
                escape_html(&host_of(url))
            ));
        }
        let greeting = match &self.account {
            Some(name) => format!("Merhaba, {}", escape_html(name)),
            None => String::from("Web'de ara ya da bir adres yaz"),
        };
        let (bg, fg, card, muted) = if self.settings.dark {
            ("#1c1b22", "#fbfbfe", "#2b2a33", "#a8a8b3")
        } else {
            ("#f9f9fb", "#15141a", "#ffffff", "#5b5b66")
        };
        format!(
            r#"<!doctype html><html><head><title>Yeni sekme</title><style>
body {{ margin: 0; background: {bg}; color: {fg}; text-align: center; }}
.wrap {{ max-width: 720px; margin: 0 auto; padding: 70px 24px 40px; }}
.logo {{ font-size: 44px; font-weight: bold; letter-spacing: 1px; }}
.logo span {{ color: #7542e5; }}
.sub {{ color: {muted}; margin: 6px 0 28px; }}
form {{ display: flex; gap: 8px; }}
input[type=search] {{ flex: 1; font-size: 17px; padding: 12px 18px; border-radius: 24px; border: 1px solid #cfcfd8; background: {card}; color: {fg}; }}
button {{ font-size: 16px; padding: 12px 20px; border-radius: 24px; border: none; background: #7542e5; color: white; }}
.tiles {{ display: flex; flex-wrap: wrap; justify-content: center; gap: 18px; margin-top: 40px; }}
.tile {{ display: block; width: 96px; text-decoration: none; color: {fg}; }}
.icon {{ width: 64px; height: 64px; margin: 0 auto 8px; border-radius: 16px; background: {card}; border: 1px solid #e0e0e6; font-size: 26px; font-weight: bold; line-height: 64px; color: #7542e5; }}
.name {{ font-size: 13px; }}
h3 {{ text-align: left; margin-top: 44px; font-size: 15px; color: {muted}; }}
ul {{ text-align: left; list-style: none; padding: 0; }}
li {{ padding: 6px 0; border-bottom: 1px solid #e8e8ee; }}
li a {{ color: {fg}; text-decoration: none; }}
.host {{ color: {muted}; font-size: 13px; }}
.foot {{ margin-top: 40px; font-size: 12px; color: {muted}; }}
</style></head><body><div class=wrap>
<div class=logo>Banna <span>Search</span></div>
<div class=sub>{greeting}</div>
<form action="banna:search" method=get><input type=search name=q placeholder="{engine} ile ara ya da adres yaz" autofocus><button type=submit>Ara</button></form>
<div class=tiles>{tiles_html}</div>
{recent_html}
<div class=foot>Banna Search {VERSION} · kendi web motoru (HTML5 · CSS · JavaScript)</div>
</div></body></html>"#,
            bg = bg,
            fg = fg,
            card = card,
            muted = muted,
            greeting = greeting,
            engine = ENGINES[self.settings.engine].0,
            tiles_html = tiles_html,
            recent_html = if recent.is_empty() {
                String::new()
            } else {
                format!("<h3>Son ziyaret edilenler</h3><ul>{}</ul>", recent)
            },
            VERSION = VERSION,
        )
    }

    fn list_page(&self, title: &str, items: &[(String, String)]) -> String {
        let mut rows = String::new();
        for (url, t) in items.iter().rev() {
            rows.push_str(&format!(
                "<li><a href=\"{}\">{}</a><br><small>{}</small></li>",
                escape_html(url),
                escape_html(if t.is_empty() { url } else { t }),
                escape_html(url)
            ));
        }
        if rows.is_empty() {
            rows = String::from("<li>Henüz bir şey yok.</li>");
        }
        format!(
            "<html><head><title>{t}</title><style>body{{font-size:16px;margin:0}} .w{{max-width:760px;margin:0 auto;padding:30px 20px}} li{{padding:8px 0;border-bottom:1px solid #e6e6ec;list-style:none}} ul{{padding:0}} small{{color:#6b6b76}} a{{color:#5b2fd0;text-decoration:none}}</style></head><body><div class=w><h1>{t}</h1><ul>{rows}</ul></div></body></html>",
            t = title,
            rows = rows
        )
    }

    fn error_page(url: &str, message: &str) -> String {
        format!(
            "<html><head><title>Sayfa açılamadı</title><style>body{{margin:0;background:#f9f9fb}} .w{{max-width:640px;margin:80px auto;padding:20px}} h1{{font-size:28px}} code{{background:#ececf2;padding:2px 6px;border-radius:4px}} a{{display:inline-block;margin-top:20px;padding:10px 18px;background:#7542e5;color:white;border-radius:20px;text-decoration:none}}</style></head><body><div class=w><h1>Bu sayfa açılamadı</h1><p><code>{}</code></p><p>{}</p><p>Adresi kontrol edin, ağ bağlantınızı doğrulayın ya da daha sonra tekrar deneyin.</p><a href=\"{}\">Tekrar dene</a></div></body></html>",
            escape_html(url),
            escape_html(message),
            escape_html(url)
        )
    }

    // ---- gezinme ----

    fn navigate(&mut self, url: &str, push: bool) {
        self.navigate_with(url, "GET", "", push);
    }

    fn navigate_with(&mut self, url: &str, method: &str, body: &str, push: bool) {
        let mut url = String::from(url);
        // Başlangıç sayfasındaki arama formu.
        if let Some(q) = url.strip_prefix("banna:search?") {
            let query = q.split('&').find_map(|p| p.strip_prefix("q=")).map(url_decode).unwrap_or_default();
            url = to_url(&query, self.settings.engine);
        }
        self.overlay = Overlay::None;
        self.address_focus = false;
        self.suggestions.clear();
        self.suggest_sel = None;
        self.hover_link = None;
        self.find = None;
        // İndirilen dosyada sayfa değişmez.
        let Some((source, final_url)) = self.fetch_document(&url, method, body) else {
            self.sync_address();
            self.dirty = true;
            return;
        };
        if let Some(old) = self.tab().page.take() {
            Self::save_storage(&old);
        }
        self.toast = None;
        let javascript = self.settings.javascript && !url.starts_with("banna:");
        let mut page = Page::new(&source, &final_url, javascript);
        page.load_images = self.settings.images;
        page.scale = self.scale();
        page.viewport = self.viewport();
        Self::load_storage(&mut page);
        let tab = &mut self.tabs[self.current];
        tab.url = final_url.clone();
        tab.title = page.title();
        tab.loading = true;
        if push {
            tab.history.truncate(tab.pos + if tab.history.is_empty() { 0 } else { 1 });
            tab.history.push(final_url.clone());
            tab.pos = tab.history.len() - 1;
        }
        tab.page = Some(page);
        self.address.set(if final_url == START { "" } else { &final_url });
        if !final_url.starts_with("banna:") {
            let title = tab.title.clone();
            self.history.retain(|(u, _)| *u != final_url);
            self.history.push((final_url.clone(), title));
            if self.history.len() > 300 {
                self.history.remove(0);
            }
        }
        self.relayout();
        self.dirty = true;
    }

    /// Belgeyi getirir: (HTML, son adres); dosya indirildiyse `None`.
    fn fetch_document(&mut self, url: &str, method: &str, body: &str) -> Option<(String, String)> {
        match url {
            START | "about:home" | "about:newtab" | "banna:newtab" => return Some((self.start_page(), String::from(START))),
            "banna:history" => return Some((self.list_page("Geçmiş", &self.history), String::from(url))),
            "banna:bookmarks" => return Some((self.list_page("Yer imleri", &self.bookmarks), String::from(url))),
            "about:blank" => return Some((String::new(), String::from(url))),
            _ => {}
        }
        self.tab().error = None;
        self.status_paint("Bağlanılıyor…");
        // Gelinen sayfa (Referer) ve form gönderiminde köken (Origin): siteler CSRF denetiminde ister.
        let mut extra: Vec<(String, String)> = Vec::new();
        let from = self.tab().url.clone();
        if from.starts_with("http") {
            extra.push((String::from("Referer"), from.clone()));
        }
        if method == "POST" {
            let origin = banna::web::url::Url::parse(if from.starts_with("http") { &from } else { url })
                .map(|u| match u.port {
                    Some(p) => format!("{}://{}:{}", u.scheme, u.host, p),
                    None => format!("{}://{}", u.scheme, u.host),
                })
                .unwrap_or_default();
            if !origin.is_empty() {
                extra.push((String::from("Origin"), origin));
            }
        }
        match banna::web::fetch(url, method, &extra, body) {
            Ok(r) => {
                let disposition = r
                    .headers
                    .iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case("content-disposition"))
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default();
                if r.status < 400 && is_download(&r.content_type, &disposition) {
                    self.save_download(&r.url, &disposition, &r.body);
                    return None;
                }
                let ct = r.content_type.to_ascii_lowercase();
                let text = String::from_utf8_lossy(&r.body).into_owned();
                let html = if ct.starts_with("image/") || banna::web::decode_image(&r.body).is_some() {
                    format!("<html><head><title>{}</title></head><body style='margin:0;background:#202024;text-align:center'><img src=\"{}\" style='max-width:100%'></body></html>", escape_html(url), escape_html(&r.url))
                } else if ct.contains("html") || ct.is_empty() && text.trim_start().starts_with('<') {
                    text
                } else {
                    format!("<html><head><title>{}</title></head><body><pre>{}</pre></body></html>", escape_html(url), escape_html(&text))
                };
                if r.status >= 400 && html.trim().is_empty() {
                    return Some((Self::error_page(url, &format!("HTTP {}", r.status)), String::from(url)));
                }
                Some((html, r.url))
            }
            Err(e) => {
                self.tab().error = Some(e.clone());
                Some((Self::error_page(url, &e), String::from(url)))
            }
        }
    }

    /// Dosyayı ~/İndirilenler'e kaydeder.
    fn save_download(&mut self, url: &str, disposition: &str, body: &[u8]) {
        let dir = downloads_dir();
        mkdir_p(&dir);
        let path = unique_path(&dir, &download_name(url, disposition));
        let name = String::from(path.rsplit('/').next().unwrap_or(""));
        self.toast = Some((
            match fs::write(&path, body) {
                Ok(_) => format!("İndirildi: İndirilenler/{} ({})", name, size_text(body.len())),
                Err(e) => format!("İndirilemedi: {} ({})", name, e),
            },
            time::uptime_ms() + 6000,
        ));
    }

    fn go_back(&mut self) {
        let tab = self.tab();
        if tab.pos > 0 {
            tab.pos -= 1;
            let url = tab.history[tab.pos].clone();
            self.navigate(&url, false);
        }
    }

    fn go_forward(&mut self) {
        let tab = self.tab();
        if tab.pos + 1 < tab.history.len() {
            tab.pos += 1;
            let url = tab.history[tab.pos].clone();
            self.navigate(&url, false);
        }
    }

    fn reload(&mut self) {
        let url = self.tab().url.clone();
        if !url.is_empty() {
            self.navigate(&url, false);
        }
    }

    fn new_tab(&mut self) {
        self.tabs.push(Tab::new());
        self.current = self.tabs.len() - 1;
        let home = self.settings.home.clone();
        self.navigate(&home, true);
        self.address_focus = true;
        self.address.set("");
    }

    fn close_tab(&mut self, i: usize) {
        if let Some(p) = self.tabs[i].page.as_ref() {
            Self::save_storage(p);
        }
        let url = self.tabs[i].url.clone();
        if !url.is_empty() && url != START {
            self.closed.push(url);
            if self.closed.len() > 20 {
                self.closed.remove(0);
            }
        }
        self.find = None;
        self.tabs.remove(i);
        if self.tabs.is_empty() {
            self.new_tab();
            return;
        }
        if self.current >= self.tabs.len() {
            self.current = self.tabs.len() - 1;
        }
        self.sync_address();
        self.relayout();
    }

    fn reopen_closed_tab(&mut self) {
        if let Some(url) = self.closed.pop() {
            self.tabs.push(Tab::new());
            self.current = self.tabs.len() - 1;
            self.navigate(&url, true);
        } else {
            self.toast = Some((String::from("Kapatılan sekme yok"), time::uptime_ms() + 2500));
        }
    }

    fn switch_tab(&mut self, i: usize) {
        if i < self.tabs.len() && i != self.current {
            self.current = i;
            self.find = None;
            self.sync_address();
            self.relayout();
        }
    }

    fn set_zoom(&mut self, zoom: u32) {
        self.settings.zoom = zoom.clamp(ZOOM_STEPS[0], ZOOM_STEPS[ZOOM_STEPS.len() - 1]);
        self.save_settings();
        self.relayout();
        self.toast = Some((format!("Yakınlaştırma %{}", self.settings.zoom), time::uptime_ms() + 1500));
    }

    fn zoom_step(&mut self, up: bool) {
        let z = self.settings.zoom;
        let next = if up {
            ZOOM_STEPS.iter().copied().find(|&s| s > z).unwrap_or(z)
        } else {
            ZOOM_STEPS.iter().rev().copied().find(|&s| s < z).unwrap_or(z)
        };
        self.set_zoom(next);
    }

    fn toggle_bookmark(&mut self) {
        let (url, title) = (self.tab().url.clone(), self.tab().title.clone());
        if url.starts_with("banna:") || url.is_empty() {
            return;
        }
        if let Some(i) = self.bookmarks.iter().position(|(u, _)| *u == url) {
            self.bookmarks.remove(i);
            self.toast = Some((String::from("Yer imi kaldırıldı"), time::uptime_ms() + 2500));
        } else {
            self.bookmarks.push((url, title));
            self.toast = Some((String::from("Yer imlerine eklendi"), time::uptime_ms() + 2500));
        }
        Self::save_list("bookmarks", &self.bookmarks);
    }

    /// Adres çubuğundaki metne uyan yer imleri ve geçmiş (en yeniler önce).
    fn update_suggestions(&mut self) {
        self.suggest_sel = None;
        let query = self.address.trim().to_lowercase();
        self.suggestions.clear();
        if query.is_empty() {
            return;
        }
        let matches = |(u, t): &(String, String)| u.to_lowercase().contains(&query) || t.to_lowercase().contains(&query);
        for item in self.bookmarks.iter().rev().chain(self.history.iter().rev()) {
            if self.suggestions.len() >= SUGGESTIONS {
                break;
            }
            if matches(item) && !self.suggestions.iter().any(|(u, _)| *u == item.0) {
                self.suggestions.push(item.clone());
            }
        }
    }

    // ---- sayfada bul ----

    fn open_find(&mut self) {
        if self.find.is_none() {
            self.find = Some(Find {
                input: TextInput::default(),
                matches: Vec::new(),
                current: 0,
            });
        }
        if let Some(f) = self.find.as_mut() {
            f.input.select_all();
        }
        self.address_focus = false;
    }

    /// Yerleşimdeki metin parçalarında arar (büyük/küçük harf duyarsız; sözcükler arası boşluk
    /// dahil). Eşleşme kutuları sayfa (CSS piksel) koordinatlarındadır.
    fn run_find(&mut self) {
        let scale = self.scale();
        let Some(find) = self.find.as_mut() else { return };
        find.matches.clear();
        find.current = 0;
        let query: Vec<char> = find.input.to_lowercase().chars().collect();
        let Some(page) = self.tabs[self.current].page.as_ref() else { return };
        if query.is_empty() {
            return;
        }
        // Metin parçaları okuma sırasıyla birleştirilir: (parça, başlangıç karakteri).
        let mut joined: Vec<char> = Vec::new();
        let mut spans: Vec<(usize, usize, f32, f32, &str, f32, bool)> = Vec::new();
        for item in &page.layout.items {
            if let Item::Text { x, y, text, size, bold, .. } = item {
                // Parçalar sözcüktür; arada boşluk (altı çizili bağlantıda boşluk parçadadır).
                if !joined.is_empty() && !text.starts_with(' ') {
                    joined.push(' ');
                }
                let start = joined.len();
                joined.extend(text.to_lowercase().chars());
                spans.push((start, joined.len(), *x, *y, text.as_str(), *size, *bold));
            }
        }
        let n = query.len();
        let mut i = 0;
        while i + n <= joined.len() && find.matches.len() < 1000 {
            if joined[i..i + n] == query[..] {
                let mut boxes = Vec::new();
                for &(start, end, x, y, text, size, bold) in &spans {
                    let (a, b) = (i.max(start), (i + n).min(end));
                    if a >= b {
                        continue;
                    }
                    let font = WebFonts.pick(size * scale, bold);
                    let before: String = text.chars().take(a - start).collect();
                    let part: String = text.chars().skip(a - start).take(b - a).collect();
                    boxes.push((
                        x + font.measure(&before) as f32 / scale,
                        y,
                        (font.measure(&part) as f32 / scale).max(2.0),
                        font.line_height() as f32 / scale,
                    ));
                }
                if !boxes.is_empty() {
                    find.matches.push(boxes);
                }
                i += n;
            } else {
                i += 1;
            }
        }
        self.find_scroll();
    }

    fn find_step(&mut self, forward: bool) {
        if let Some(f) = self.find.as_mut()
            && !f.matches.is_empty()
        {
            let n = f.matches.len();
            f.current = if forward { (f.current + 1) % n } else { (f.current + n - 1) % n };
        }
        self.find_scroll();
    }

    /// Geçerli eşleşmeyi görünür yapar.
    fn find_scroll(&mut self) {
        let Some((_, y, _, h)) = self.find.as_ref().and_then(|f| f.matches.get(f.current)).and_then(|m| m.first().copied()) else {
            return;
        };
        let Some(page) = self.tabs[self.current].page.as_mut() else { return };
        let view = page.viewport.1;
        if y < page.scroll + 20.0 || y + h > page.scroll + view - 20.0 {
            let target = (y - view / 3.0).max(0.0);
            page.scroll_by(target - page.scroll);
        }
        self.dirty = true;
    }

    fn sync_address(&mut self) {
        let url = self.tab().url.clone();
        self.address.set(if url == START { "" } else { &url });
    }

    fn relayout(&mut self) {
        let scale = self.scale();
        let viewport = self.viewport();
        if let Some(page) = self.tabs[self.current].page.as_mut() {
            page.scale = scale;
            page.viewport = viewport;
            page.relayout(&WebFonts);
            page.scroll_to_fragment();
        }
    }

    fn handle_action(&mut self, action: Action) {
        match action {
            Action::Navigate(url) => self.navigate(&url, true),
            Action::Post(url, body) => self.navigate_with(&url, "POST", &body, true),
            Action::Back => self.go_back(),
            Action::Redraw => {
                if let Some(page) = self.tab().page.as_mut()
                    && (page.needs_layout || page.interp.doc.dirty)
                {
                    page.relayout(&WebFonts);
                }
                self.dirty = true;
            }
            Action::None => {}
        }
        self.collect_page_messages();
    }

    fn collect_page_messages(&mut self) {
        let Some(page) = self.tabs[self.current].page.as_mut() else { return };
        let msgs = page.take_console();
        let alerts = page.take_alerts();
        let title = page.title();
        if page.interp.host.title_changed {
            page.interp.host.title_changed = false;
        }
        for e in core::mem::take(&mut page.errors) {
            self.console.push((1, e));
        }
        self.console.extend(msgs.into_iter().filter(|(l, _)| *l != 9));
        if self.console.len() > 400 {
            let extra = self.console.len() - 400;
            self.console.drain(..extra);
        }
        if let Some(a) = alerts.into_iter().last() {
            self.toast = Some((a, time::uptime_ms() + 6000));
        }
        let tab = &mut self.tabs[self.current];
        if !title.is_empty() && title != tab.title {
            tab.title = title.clone();
            if let Some(entry) = self.history.iter_mut().rev().find(|(u, _)| *u == tab.url) {
                entry.1 = title;
            }
            self.dirty = true;
        }
    }

    // ---- hesap ----

    fn load_account(&mut self) {
        if !self.client.has_session() {
            self.account = None;
            return;
        }
        match self.client.get("/auth/me") {
            Ok(me) => {
                let current = me.get("current");
                let name = [current.get("displayName"), current.get("name"), current.get("ebannaAddress")]
                    .iter()
                    .map(|v| v.as_str())
                    .find(|s| !s.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| String::from("Banna hesabı"));
                self.account = Some(name);
            }
            Err(_) => self.account = None,
        }
    }

    fn login(&mut self) {
        let id = self.login_fields[0].trim().to_ascii_lowercase();
        let password = String::from(self.login_fields[1].as_str());
        if id.is_empty() || password.is_empty() {
            self.login_message = Some(String::from("E-Banna adresinizi ya da E-Kimlik numaranızı ve şifrenizi girin."));
            return;
        }
        self.login_message = Some(String::from("Giriş yapılıyor…"));
        self.paint();
        match self.client.login(&id, &password) {
            Ok(Login::Done) => {
                self.login_fields[1].clear_text();
                self.login_message = None;
                self.load_account();
                self.toast = Some((String::from("Banna hesabınızla giriş yaptınız"), time::uptime_ms() + 4000));
            }
            Ok(Login::Pending { approval_id, code }) => {
                self.login_fields[1].clear_text();
                self.login_message = Some(format!("Onay cihazınızda bu girişi onaylayın. Kod: {}", code));
                self.pending_login = Some((approval_id, code));
                self.last_poll = time::uptime_ms();
            }
            Err(e) => {
                self.login_message = Some(if e.status == 401 {
                    String::from("E-Banna adresi / E-Kimlik ya da şifre hatalı.")
                } else {
                    e.message.clone()
                });
            }
        }
    }

    fn poll_login(&mut self) -> bool {
        let Some((id, _)) = self.pending_login.clone() else { return false };
        if time::uptime_ms() - self.last_poll < 2500 {
            return false;
        }
        self.last_poll = time::uptime_ms();
        match self.client.approval_status(&id).as_deref() {
            Ok("APPROVED") => {
                self.pending_login = None;
                self.login_message = None;
                self.load_account();
            }
            Ok("DENIED") => {
                self.pending_login = None;
                self.login_message = Some(String::from("Giriş isteği reddedildi."));
            }
            Ok("EXPIRED") | Err(_) => {
                self.pending_login = None;
                self.login_message = Some(String::from("Onay süresi doldu, tekrar deneyin."));
            }
            _ => return false,
        }
        true
    }

    // ---- çizim ----

    fn theme(&self) -> Theme {
        let prefs = desktop::query().map(|i| Prefs::parse(&i.text())).unwrap_or_default();
        Theme::from(&prefs)
    }

    fn glass(&self) -> Glass {
        let u = self.u;
        Glass {
            tint: if self.settings.dark {
                with_alpha(rgb(0x20, 0x1f, 0x26), 150)
            } else {
                with_alpha(rgb(0xff, 0xff, 0xff), 120)
            },
            rim: if self.settings.dark { 120 } else { 210 },
            refraction: 8 * u,
            edge: 8 * u,
            sheen: if self.settings.dark { 18 } else { 45 },
        }
    }

    fn text_color(&self) -> Argb {
        if self.settings.dark { rgb(0xfb, 0xfb, 0xfe) } else { rgb(0x15, 0x14, 0x1a) }
    }

    fn muted(&self) -> Argb {
        if self.settings.dark { rgb(0xa8, 0xa8, 0xb3) } else { rgb(0x5b, 0x5b, 0x66) }
    }

    /// Pencere tuvalindeki bir bölgenin bulanık kopyası (cam arka planı).
    fn backdrop(canvas: &mut Canvas, r: Rect, blur: i32) -> Surface {
        let r = r.intersect(&Rect::new(0, 0, canvas.width, canvas.height));
        let mut s = Surface::new(r.w.max(1), r.h.max(1), 0xffffffff);
        for y in 0..r.h {
            let row = canvas.span(r.y + y, r.x, r.right()).to_vec();
            let start = (y * s.width) as usize;
            s.pixels[start..start + row.len()].copy_from_slice(&row);
        }
        banna::gfx::glass::backdrop(&s, blur)
    }

    fn glass_panel(&self, c: &mut Canvas, r: Rect, radius: i32) {
        let u = self.u;
        let bd = Self::backdrop(c, r.inset(-12 * u), 14 * u);
        let origin = (-(r.x - 12 * u), -(r.y - 12 * u));
        c.shadow(r, radius, 10 * u, 40);
        c.glass(r, radius, &bd, origin, &self.glass());
    }

    fn status_paint(&mut self, text: &str) {
        self.toast = Some((String::from(text), time::uptime_ms() + 30_000));
        self.paint();
    }

    fn paint(&mut self) {
        let u = self.u;
        let area = self.page_area();
        let chrome = self.chrome_h();
        let caret = self.caret_on;
        self.hits.clear();
        let dark = self.settings.dark;
        // Çizim boyunca `self.window`'a başka erişim yok: tuval pencereden ayrı ödünç alınır.
        let window: *mut Window = &mut self.window;
        let mut canvas = unsafe { (*window).canvas() };
        canvas.fill(area, if dark { rgb(0x1c, 0x1b, 0x22) } else { rgb(0xff, 0xff, 0xff) });
        let content = Rect::new(area.x, area.y, area.w, area.h);
        if let Some(page) = self.tabs[self.current].page.as_mut() {
            page.paint(canvas.reborrow(), Rect::new(content.x, content.y + chrome, content.w, content.h - chrome), &WebFonts, caret);
            // Kaydırılmış içerik çubukların altında görünsün (cam arka planı için).
            if page.scroll > 0.0 {
                let saved = page.scroll;
                let lift = (chrome as f32 / page.scale).min(saved);
                page.scroll = saved - lift;
                let top = Rect::new(content.x, content.y, content.w, chrome);
                let mut view = canvas.reborrow();
                view.clip = top;
                page.paint(view, Rect::new(content.x, content.y + chrome - (lift * page.scale) as i32, content.w, chrome + (lift * page.scale) as i32), &WebFonts, false);
                page.scroll = saved;
            }
        }
        // Sayfada bul eşleşmeleri.
        if let Some(f) = &self.find
            && let Some(page) = self.tabs[self.current].page.as_ref()
        {
            let s = page.scale;
            let top = content.y + chrome;
            let mut c2 = canvas.reborrow();
            c2.clip = Rect::new(content.x, top, content.w, content.h - chrome);
            for (i, &(x, y, w, h)) in f.matches.iter().enumerate().flat_map(|(i, m)| m.iter().map(move |b| (i, b))) {
                let r = Rect::new(
                    content.x + (x * s) as i32,
                    top + ((y - page.scroll) * s) as i32,
                    ((w * s) as i32).max(2),
                    (h * s) as i32,
                );
                if r.bottom() < top || r.y > content.bottom() {
                    continue;
                }
                let color = if i == f.current { with_alpha(0xff9632, 150) } else { with_alpha(0xffe14d, 110) };
                c2.fill(r, color);
            }
        }
        canvas.clip = area;
        self.paint_chrome(&mut canvas);
        self.paint_suggestions(&mut canvas);
        self.paint_find(&mut canvas);
        self.paint_overlay(&mut canvas);
        // Durum hapı: bağlantı adresi ya da bildirim.
        let status = self
            .toast
            .as_ref()
            .filter(|(_, until)| time::uptime_ms() < *until)
            .map(|(t, _)| t.clone())
            .or_else(|| self.hover_link.clone())
            .or_else(|| {
                let tab = &self.tabs[self.current];
                (tab.loading && tab.page.as_ref().is_some_and(|p| p.stage != Stage::Done)).then(|| String::from("Yükleniyor…"))
            });
        if let Some(text) = status {
            let f = font(13 * u, false);
            let w = (f.measure(&text) + 28 * u).min(area.w - 24 * u);
            let r = Rect::new(12 * u, area.bottom() - 40 * u, w, 30 * u);
            self.glass_panel(&mut canvas, r, 15 * u);
            f.draw_clipped(&mut canvas, r.x + 14 * u, r.y + (r.h - f.line_height()) / 2, &text, r.w - 28 * u, self.text_color());
        }
        drop(canvas);
        self.window.present();
        self.dirty = false;
    }

    fn icon_button(&mut self, c: &mut Canvas, r: Rect, label: &str, control: Control, enabled: bool) {
        let u = self.u;
        if enabled && self.hover == Some(r) {
            c.fill_rounded(r, r.h / 2, with_alpha(if self.settings.dark { 0xffffff } else { 0x000000 }, 28));
        }
        let f = font(18 * u, false);
        let color = if enabled { self.text_color() } else { with_alpha(self.muted() & 0xffffff, 110) };
        if !vector_icon(c, r, label, color, u) {
            f.draw(c, r.x + (r.w - f.measure(label)) / 2, r.y + (r.h - f.line_height()) / 2, label, color);
        }
        if enabled {
            self.hits.push(Hit { rect: r, control, slider: None });
        }
    }

    fn paint_chrome(&mut self, c: &mut Canvas) {
        let u = self.u;
        let w = self.window.width as i32;
        // Sekmeler.
        let tab_bar = Rect::new(8 * u, 6 * u, w - 16 * u, 32 * u);
        let n = self.tabs.len() as i32;
        let new_w = 32 * u;
        let tab_w = ((tab_bar.w - new_w - 6 * u) / n.max(1)).clamp(60 * u, 230 * u);
        for i in 0..self.tabs.len() {
            let r = Rect::new(tab_bar.x + i as i32 * (tab_w + 4 * u), tab_bar.y, tab_w, tab_bar.h);
            if r.right() > tab_bar.right() - new_w {
                break;
            }
            let active = i == self.current;
            if active {
                self.glass_panel(c, r, 12 * u);
            } else if self.hover == Some(r) {
                c.fill_rounded(r, 12 * u, with_alpha(if self.settings.dark { 0xffffff } else { 0x000000 }, 18));
            }
            let f = font(13 * u, active);
            let title = if self.tabs[i].title.is_empty() { self.tabs[i].url.clone() } else { self.tabs[i].title.clone() };
            let loading = self.tabs[i].loading && self.tabs[i].page.as_ref().is_some_and(|p| p.stage != Stage::Done);
            let prefix = if loading { "◌ " } else { "" };
            f.draw_clipped(c, r.x + 12 * u, r.y + (r.h - f.line_height()) / 2, &format!("{}{}", prefix, title), r.w - 40 * u, self.text_color());
            self.hits.push(Hit { rect: r, control: Control::Tab(i), slider: None });
            let x_rect = Rect::new(r.right() - 26 * u, r.y + 6 * u, 20 * u, 20 * u);
            self.icon_button(c, x_rect, "×", Control::CloseTab(i), true);
        }
        let plus = Rect::new((tab_bar.x + n * (tab_w + 4 * u)).min(tab_bar.right() - new_w), tab_bar.y, new_w, tab_bar.h);
        self.icon_button(c, plus, "+", Control::NewTab, true);

        // Araç çubuğu (yüzen cam hap).
        let bar = Rect::new(8 * u, 44 * u, w - 16 * u, 44 * u);
        self.glass_panel(c, bar, 22 * u);
        let mut x = bar.x + 6 * u;
        let by = bar.y + 6 * u;
        let bs = 32 * u;
        let (can_back, can_fwd) = {
            let t = &self.tabs[self.current];
            (t.pos > 0, t.pos + 1 < t.history.len())
        };
        for (label, control, enabled) in [
            ("←", Control::Back, can_back),
            ("→", Control::Forward, can_fwd),
            ("reload", Control::Reload, true),
            ("home", Control::Home, true),
        ] {
            self.icon_button(c, Rect::new(x, by, bs, bs), label, control, enabled);
            x += bs + 2 * u;
        }
        let right_buttons = 3;
        let field = Rect::new(x + 6 * u, by, bar.right() - x - 12 * u - right_buttons * (bs + 2 * u), bs);
        c.fill_rounded(field, field.h / 2, if self.settings.dark { with_alpha(0x000000, 90) } else { with_alpha(0xffffff, 170) });
        if self.address_focus {
            c.fill_rounded(Rect::new(field.x, field.y, field.w, field.h), field.h / 2, with_alpha(0x7542e5, 40));
            c.fill_rounded(field.inset(2 * u), field.h / 2 - 2 * u, if self.settings.dark { rgb(0x2b, 0x2a, 0x33) } else { rgb(0xff, 0xff, 0xff) });
        }
        let f = font(14 * u, false);
        let secure = self.tabs[self.current].url.starts_with("https://");
        let lock = if self.tabs[self.current].url.starts_with("banna:") { "banna" } else if secure { "lock" } else { "info" };
        let icon_rect = Rect::new(field.x + 8 * u, field.y + 4 * u, 22 * u, field.h - 8 * u);
        vector_icon(c, icon_rect, lock, if secure { rgb(0x2e, 0x9e, 0x5b) } else if lock == "banna" { rgb(0x75, 0x42, 0xe5) } else { self.muted() }, u);
        let tx = field.x + 34 * u;
        let ty = field.y + (field.h - f.line_height()) / 2;
        let text = self.address.as_str();
        if text.is_empty() && !self.address_focus {
            f.draw(c, tx, ty, &format!("{} ile ara ya da adres yaz", ENGINES[self.settings.engine].0), self.muted());
        } else {
            let saved = c.clip;
            c.clip = c.clip.intersect(&Rect::new(tx, field.y, field.w - 46 * u, field.h));
            let shown: String = text.chars().collect();
            let cursor = if self.address_focus { self.address.cursor() } else { 0 };
            let before: String = shown.chars().take(cursor).collect();
            let shift = (f.measure(&before) - (field.w - 60 * u)).max(0);
            if self.address_focus && let Some((a, b)) = self.address.selection() {
                let pa: String = shown.chars().take(a).collect();
                let pb: String = shown.chars().take(b).collect();
                c.fill(Rect::new(tx - shift + f.measure(&pa), ty, f.measure(&pb) - f.measure(&pa), f.line_height()), with_alpha(0x7542e5, 90));
            }
            f.draw(c, tx - shift, ty, &shown, self.text_color());
            if self.address_focus && self.caret_on {
                c.fill(Rect::new(tx - shift + f.measure(&before), ty, 2 * u, f.line_height()), rgb(0x75, 0x42, 0xe5));
            }
            c.clip = saved;
        }
        self.hits.push(Hit { rect: field, control: Control::Address, slider: None });
        self.address_rect = field;
        let mut rx = field.right() + 6 * u;
        let starred = self.bookmarks.iter().any(|(b, _)| *b == self.tabs[self.current].url);
        let account_label = match &self.account {
            Some(name) => name.chars().next().unwrap_or('B').to_uppercase().collect::<String>(),
            None => String::from("user"),
        };
        for (label, control) in [
            (String::from(if starred { "★" } else { "☆" }), Control::Star),
            (account_label, Control::Account),
            (String::from("menu"), Control::Menu),
        ] {
            let r = Rect::new(rx, by, bs, bs);
            if control == Control::Account && self.account.is_some() {
                c.fill_rounded(r.inset(3 * u), r.h / 2, rgb(0x75, 0x42, 0xe5));
                let f = font(14 * u, true);
                f.draw(c, r.x + (r.w - f.measure(&label)) / 2, r.y + (r.h - f.line_height()) / 2, &label, 0xffffffff);
                self.hits.push(Hit { rect: r, control, slider: None });
            } else {
                self.icon_button(c, r, &label, control, true);
            }
            rx += bs + 2 * u;
        }
    }

    fn paint_suggestions(&mut self, c: &mut Canvas) {
        if !self.address_focus || self.suggestions.is_empty() {
            return;
        }
        let u = self.u;
        let field = self.address_rect;
        let row_h = 44 * u;
        let r = Rect::new(field.x, field.bottom() + 8 * u, field.w, self.suggestions.len() as i32 * row_h + 12 * u);
        self.glass_panel(c, r, 16 * u);
        let title_font = font(14 * u, false);
        let url_font = font(12 * u, false);
        let suggestions = self.suggestions.clone();
        for (i, (url, title)) in suggestions.iter().enumerate() {
            let row = Rect::new(r.x + 6 * u, r.y + 6 * u + i as i32 * row_h, r.w - 12 * u, row_h);
            if self.suggest_sel == Some(i) || self.hover == Some(row) {
                c.fill_rounded(row, 10 * u, with_alpha(0x7542e5, 60));
            }
            let starred = self.bookmarks.iter().any(|(b, _)| b == url);
            let icon = Rect::new(row.x + 8 * u, row.y + (row_h - 22 * u) / 2, 22 * u, 22 * u);
            if starred {
                title_font.draw(c, icon.x + 3 * u, icon.y + 1 * u, "★", self.muted());
            } else {
                vector_icon(c, icon, "reload", self.muted(), u);
            }
            if title.is_empty() {
                title_font.draw_clipped(c, row.x + 40 * u, row.y + (row_h - title_font.line_height()) / 2, url, row.w - 52 * u, self.text_color());
            } else {
                title_font.draw_clipped(c, row.x + 40 * u, row.y + 4 * u, title, row.w - 52 * u, self.text_color());
                url_font.draw_clipped(c, row.x + 40 * u, row.y + 4 * u + title_font.line_height(), url, row.w - 52 * u, self.muted());
            }
            self.hits.push(Hit { rect: row, control: Control::Suggest(i), slider: None });
        }
    }

    fn paint_find(&mut self, c: &mut Canvas) {
        let Some(find) = &self.find else { return };
        let u = self.u;
        let w = self.window.width as i32;
        let r = Rect::new(w - 400 * u, 96 * u, 388 * u, 44 * u);
        self.glass_panel(c, r, 22 * u);
        let f = font(14 * u, false);
        let field = Rect::new(r.x + 10 * u, r.y + 6 * u, r.w - 170 * u, r.h - 12 * u);
        c.fill_rounded(field, field.h / 2, if self.settings.dark { with_alpha(0x000000, 90) } else { with_alpha(0xffffff, 190) });
        let ty = field.y + (field.h - f.line_height()) / 2;
        let text = find.input.text.clone();
        let saved = c.clip;
        c.clip = c.clip.intersect(&field.inset(4 * u));
        if text.is_empty() {
            f.draw(c, field.x + 12 * u, ty, "Sayfada bul", self.muted());
        }
        let end = f.draw(c, field.x + 12 * u, ty, &text, self.text_color());
        if self.caret_on {
            c.fill(Rect::new(end, ty, 2 * u, f.line_height()), rgb(0x75, 0x42, 0xe5));
        }
        c.clip = saved;
        let count = if text.is_empty() {
            String::new()
        } else if find.matches.is_empty() {
            String::from("Yok")
        } else {
            format!("{}/{}", find.current + 1, find.matches.len())
        };
        let small = font(13 * u, false);
        small.draw(c, field.right() + 10 * u, r.y + (r.h - small.line_height()) / 2, &count, self.muted());
        let bs = 30 * u;
        let mut x = r.right() - 3 * (bs + 2 * u) - 6 * u;
        for (label, control) in [("↑", Control::FindPrev), ("↓", Control::FindNext), ("×", Control::FindClose)] {
            self.icon_button(c, Rect::new(x, r.y + 7 * u, bs, bs), label, control, true);
            x += bs + 2 * u;
        }
    }

    fn paint_overlay(&mut self, c: &mut Canvas) {
        let u = self.u;
        let w = self.window.width as i32;
        let h = self.window.height as i32;
        match self.overlay {
            Overlay::None => {}
            Overlay::Menu => {
                let items: [(&str, u8); 11] = [
                    ("Yeni sekme        Ctrl+T", 0),
                    ("Kapatılan sekmeyi aç", 8),
                    ("Sayfada bul       Ctrl+F", 9),
                    ("Geçmiş", 1),
                    ("Yer imleri        Ctrl+D", 2),
                    ("Yakınlaştır  Ctrl + / - / 0", 10),
                    ("Sayfayı yenile    F5", 3),
                    ("Geliştirici konsolu  F12", 4),
                    ("Ayarlar", 5),
                    ("Banna hesabı", 6),
                    ("Banna Search hakkında", 7),
                ];
                let r = Rect::new(w - 268 * u, 92 * u, 256 * u, (items.len() as i32 * 36 + 12) * u);
                self.glass_panel(c, r, 16 * u);
                let f = font(14 * u, false);
                for (i, (label, id)) in items.iter().enumerate() {
                    let row = Rect::new(r.x + 6 * u, r.y + 6 * u + i as i32 * 36 * u, r.w - 12 * u, 34 * u);
                    if self.hover == Some(row) {
                        c.fill_rounded(row, 10 * u, with_alpha(0x7542e5, 60));
                    }
                    f.draw(c, row.x + 12 * u, row.y + (row.h - f.line_height()) / 2, label, self.text_color());
                    self.hits.push(Hit { rect: row, control: Control::MenuItem(*id), slider: None });
                }
            }
            Overlay::Settings | Overlay::Account => {
                let panel = Rect::new(w - 440 * u, 92 * u, 428 * u, h - 104 * u);
                self.glass_panel(c, panel, 20 * u);
                let theme = self.theme();
                let fonts = ui::fonts();
                let mut hits = core::mem::take(&mut self.hits);
                let hover = self.hover;
                {
                    let inner = panel.inset(18 * u);
                    let mut g = Ui::new(c, theme, &fonts, u, inner, inner.y, &mut hits, hover);
                    let close = Rect::new(inner.right() - 30 * u, inner.y, 30 * u, 30 * u);
                    fonts.title.draw(g.c, close.x + 8 * u, close.y + 4 * u, "×", g.t.text);
                    g.hit(close, Control::CloseOverlay);
                    if self.overlay == Overlay::Settings {
                        settings_ui(&mut g, &self.settings);
                    } else {
                        account_ui(&mut g, self.account.as_deref(), &self.login_fields, self.login_focus, self.login_message.as_deref(), self.caret_on);
                    }
                }
                self.hits = hits;
                c.clip = Rect::new(0, 0, w, h);
            }
            Overlay::Console => {
                let r = Rect::new(8 * u, h - 260 * u, w - 16 * u, 250 * u);
                self.glass_panel(c, r, 16 * u);
                let f = font(13 * u, false);
                let title = font(14 * u, true);
                title.draw(c, r.x + 14 * u, r.y + 10 * u, "Konsol", self.text_color());
                let clear = Rect::new(r.right() - 90 * u, r.y + 6 * u, 80 * u, 26 * u);
                f.draw(c, clear.x + 12 * u, clear.y + 4 * u, "Temizle", rgb(0x75, 0x42, 0xe5));
                self.hits.push(Hit { rect: clear, control: Control::ConsoleClear, slider: None });
                let lines = (r.h - 44 * u) / f.line_height();
                let start = self.console.len().saturating_sub(lines as usize);
                let saved = c.clip;
                c.clip = r.inset(8 * u);
                for (i, (level, text)) in self.console[start..].iter().enumerate() {
                    let color = match level {
                        2 => rgb(0xd7, 0x26, 0x4f),
                        1 => rgb(0xb3, 0x7a, 0x00),
                        _ => self.text_color(),
                    };
                    f.draw_clipped(c, r.x + 14 * u, r.y + 38 * u + i as i32 * f.line_height(), text, r.w - 28 * u, color);
                }
                c.clip = saved;
            }
        }
    }

    // ---- girdi ----

    fn page_point(&self, x: i32, y: i32) -> Option<(f32, f32)> {
        let chrome = self.chrome_h();
        if y < chrome {
            return None;
        }
        let s = self.scale();
        Some((x as f32 / s, (y - chrome) as f32 / s))
    }

    fn press(&mut self, x: i32, y: i32) {
        if let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() {
            self.run(hit.control, x);
            return;
        }
        if self.overlay != Overlay::None && self.overlay != Overlay::Console {
            self.overlay = Overlay::None;
            self.dirty = true;
            return;
        }
        self.address_focus = false;
        self.suggestions.clear();
        self.sync_address();
        if let Some((px, py)) = self.page_point(x, y)
            && let Some(page) = self.tab().page.as_mut()
        {
            let action = page.click(px, py);
            self.handle_action(action);
        }
        self.dirty = true;
    }

    fn run(&mut self, control: Control, _x: i32) {
        self.dirty = true;
        match control {
            Control::Tab(i) => self.switch_tab(i),
            Control::CloseTab(i) => self.close_tab(i),
            Control::NewTab => self.new_tab(),
            Control::Back => self.go_back(),
            Control::Forward => self.go_forward(),
            Control::Reload => self.reload(),
            Control::Home => {
                let home = self.settings.home.clone();
                self.navigate(&home, true);
            }
            Control::Address => {
                if !self.address_focus {
                    self.address_focus = true;
                    self.address.select_all();
                }
                self.overlay = Overlay::None;
            }
            Control::Star => self.toggle_bookmark(),
            Control::Account => {
                self.overlay = if self.overlay == Overlay::Account { Overlay::None } else { Overlay::Account };
                self.login_focus = 0;
            }
            Control::Menu => {
                self.overlay = if self.overlay == Overlay::Menu { Overlay::None } else { Overlay::Menu };
            }
            Control::MenuItem(id) => {
                self.overlay = Overlay::None;
                match id {
                    0 => self.new_tab(),
                    1 => self.navigate("banna:history", true),
                    2 => self.navigate("banna:bookmarks", true),
                    3 => self.reload(),
                    4 => self.overlay = Overlay::Console,
                    5 => self.overlay = Overlay::Settings,
                    6 => self.overlay = Overlay::Account,
                    8 => self.reopen_closed_tab(),
                    9 => self.open_find(),
                    10 => self.set_zoom(100),
                    _ => {
                        self.toast = Some((format!("Banna Search {} · Banna OS için kendi web motoru", VERSION), time::uptime_ms() + 5000));
                    }
                }
            }
            Control::Setting(id) => {
                let s = &mut self.settings;
                match id {
                    0 => s.javascript = !s.javascript,
                    1 => s.images = !s.images,
                    2 => s.dark = !s.dark,
                    3 => {
                        s.home = if s.home == START {
                            self.tabs[self.current].url.clone()
                        } else {
                            String::from(START)
                        }
                    }
                    4 => {
                        self.history.clear();
                        Self::save_list("history", &self.history);
                        self.toast = Some((String::from("Geçmiş temizlendi"), time::uptime_ms() + 2500));
                    }
                    _ => {
                        let dir = format!("{}/storage", config_dir());
                        if let Ok(list) = fs::read_dir(&dir) {
                            for (name, _) in list {
                                if name != "." && name != ".." {
                                    let _ = fs::remove(&format!("{}/{}", dir, name));
                                }
                            }
                        }
                        cookies::clear("");
                        Self::save_cookies(true);
                        self.toast = Some((String::from("Site verileri ve çerezler silindi"), time::uptime_ms() + 2500));
                    }
                }
                self.save_settings();
                if id == 2 && self.tab().url == START {
                    self.navigate(START, false);
                }
            }
            Control::Engine(i) => {
                self.settings.engine = i;
                self.save_settings();
            }
            Control::Zoom(z) => {
                self.settings.zoom = z;
                self.save_settings();
                self.relayout();
            }
            Control::LoginField(i) => self.login_focus = i,
            Control::LoginSubmit => self.login(),
            Control::Logout => {
                self.client.logout();
                self.account = None;
                self.toast = Some((String::from("Oturum kapatıldı"), time::uptime_ms() + 2500));
            }
            Control::CloseOverlay => self.overlay = Overlay::None,
            Control::ConsoleClear => self.console.clear(),
            Control::Suggest(i) => {
                if let Some((url, _)) = self.suggestions.get(i).cloned() {
                    self.navigate(&url, true);
                }
            }
            Control::FindPrev => self.find_step(false),
            Control::FindNext => self.find_step(true),
            Control::FindClose => self.find = None,
        }
    }

    fn key(&mut self, key: u32) {
        self.dirty = true;
        let ctrl = key & KEY_CTRL != 0;
        let base = key & !(KEY_CTRL | KEY_SHIFT);
        // Genel kısayollar.
        match key {
            20 => return self.new_tab(),
            23 => {
                let c = self.current;
                return self.close_tab(c);
            }
            12 => {
                self.address_focus = true;
                self.address.select_all();
                return;
            }
            18 => return self.reload(),
            // Ctrl+F sayfada bul, Ctrl+D yer imi.
            6 => return self.open_find(),
            4 => return self.toggle_bookmark(),
            k if k == '+' as u32 | KEY_CTRL || k == '=' as u32 | KEY_CTRL => return self.zoom_step(true),
            k if k == '-' as u32 | KEY_CTRL => return self.zoom_step(false),
            k if k == '0' as u32 | KEY_CTRL => return self.set_zoom(100),
            // Ctrl+Tab / Ctrl+Shift+Tab sekmeler arasında.
            k if k == 9 | KEY_CTRL => {
                let next = (self.current + 1) % self.tabs.len();
                return self.switch_tab(next);
            }
            k if k == 9 | KEY_CTRL | KEY_SHIFT => {
                let prev = (self.current + self.tabs.len() - 1) % self.tabs.len();
                return self.switch_tab(prev);
            }
            _ => {}
        }
        if self.find.is_some() && !self.address_focus && self.overlay == Overlay::None {
            match base {
                27 => self.find = None,
                13 | KEY_DOWN => self.find_step(true),
                KEY_UP => self.find_step(false),
                _ => {
                    let f = self.find.as_mut().unwrap();
                    let before = f.input.text.clone();
                    f.input.key(key, 200);
                    if f.input.text != before {
                        self.run_find();
                    }
                }
            }
            return;
        }
        if base == KEY_F1 + 4 {
            return self.reload();
        }
        if base == KEY_F1 + 11 {
            self.overlay = if self.overlay == Overlay::Console { Overlay::None } else { Overlay::Console };
            return;
        }
        if key == 27 {
            if self.overlay != Overlay::None {
                self.overlay = Overlay::None;
            } else if self.address_focus {
                self.address_focus = false;
                self.suggestions.clear();
                self.sync_address();
            } else if self.find.is_some() {
                self.find = None;
            }
            return;
        }
        if ctrl && base == 0xf702 && !self.address_focus {
            return self.go_back();
        }
        if ctrl && base == 0xf703 && !self.address_focus {
            return self.go_forward();
        }
        if self.overlay == Overlay::Account && self.account.is_none() {
            match key {
                9 => self.login_focus = (self.login_focus + 1) % 2,
                13 => self.login(),
                _ => {
                    self.login_fields[self.login_focus].key(key, 120);
                }
            }
            return;
        }
        if self.address_focus {
            match key {
                13 => {
                    let target = match self.suggest_sel.and_then(|i| self.suggestions.get(i)) {
                        Some((url, _)) => url.clone(),
                        None => to_url(self.address.as_str(), self.settings.engine),
                    };
                    self.address_focus = false;
                    self.navigate(&target, true);
                }
                KEY_DOWN if !self.suggestions.is_empty() => {
                    self.suggest_sel = Some(self.suggest_sel.map_or(0, |i| (i + 1) % self.suggestions.len()));
                }
                KEY_UP if !self.suggestions.is_empty() => {
                    self.suggest_sel = match self.suggest_sel {
                        Some(0) | None => None,
                        Some(i) => Some(i - 1),
                    };
                }
                3 => {
                    clipboard::set(self.address.as_str());
                }
                _ => {
                    let before = self.address.text.clone();
                    self.address.key(key, 4096);
                    if self.address.text != before {
                        self.update_suggestions();
                    }
                }
            }
            return;
        }
        // Sayfa: odaktaki form denetimi ya da kaydırma.
        let editing = self
            .tab()
            .page
            .as_ref()
            .and_then(|p| p.interp.doc.focus.map(|f| p.is_editable(f)))
            .unwrap_or(false);
        let u = self.u as f32;
        let Some(page) = self.tabs[self.current].page.as_mut() else { return };
        if editing {
            let action = match key {
                8 => page.type_key('\u{8}', "Backspace"),
                13 => page.type_key('\n', "Enter"),
                KEY_PASTE => {
                    let mut last = Action::None;
                    for ch in clipboard::get().chars().filter(|c| *c != '\r') {
                        last = page.type_key(ch, "v");
                    }
                    last
                }
                k if (32..0xf700).contains(&k) => match char::from_u32(k) {
                    Some(ch) => page.type_key(ch, &format!("{}", ch)),
                    None => Action::None,
                },
                _ => Action::None,
            };
            self.handle_action(action);
            return;
        }
        let view = page.viewport.1;
        let moved = match base {
            KEY_UP => page.scroll_by(-40.0),
            KEY_DOWN => page.scroll_by(40.0),
            KEY_PAGE_UP => page.scroll_by(-view * 0.9),
            KEY_PAGE_DOWN | 32 => page.scroll_by(view * 0.9),
            0xf704 => page.scroll_by(-1e9),
            0xf705 => page.scroll_by(1e9),
            8 => {
                self.go_back();
                return;
            }
            _ => false,
        };
        let _ = (moved, u);
    }
}

fn settings_ui(g: &mut Ui<Control>, s: &Settings) {
    g.heading("Ayarlar");
    g.section("Genel");
    g.card(3);
    g.toggle("JavaScript", Some("Sayfalardaki betikleri çalıştır"), s.javascript, Control::Setting(0));
    g.toggle("Görselleri yükle", Some("Kapalıyken sayfalar daha hızlı açılır"), s.images, Control::Setting(1));
    g.toggle("Koyu görünüm", Some("Tarayıcı çubukları ve başlangıç sayfası"), s.dark, Control::Setting(2));
    g.section("Arama motoru");
    let options: Vec<(&str, Control)> = ENGINES.iter().enumerate().map(|(i, (n, _))| (*n, Control::Engine(i))).collect();
    g.card(1);
    g.choice("Varsayılan", None, &options, &Control::Engine(s.engine));
    g.section("Yakınlaştırma");
    g.card(1);
    g.choice(
        "Sayfa boyutu",
        None,
        &[("%80", Control::Zoom(80)), ("%100", Control::Zoom(100)), ("%125", Control::Zoom(125)), ("%150", Control::Zoom(150))],
        &Control::Zoom(s.zoom),
    );
    g.section("Gizlilik");
    g.card(1);
    g.buttons(
        "Tarama verileri",
        Some(if s.home == START { "Ana sayfa: başlangıç sayfası" } else { "Ana sayfa: özel adres" }),
        &[("Geçmişi sil", Control::Setting(4), false), ("Çerezler ve site verileri", Control::Setting(5), false), ("Ana sayfa", Control::Setting(3), false)],
    );
    g.note("Banna Search kendi web motorunu kullanır; sayfaların bir kısmı tam görünmeyebilir.");
}

fn account_ui(
    g: &mut Ui<Control>,
    account: Option<&str>,
    fields: &[TextInput; 2],
    focus: usize,
    message: Option<&str>,
    _caret: bool,
) {
    g.heading("Banna hesabı");
    if let Some(name) = account {
        g.card(1);
        g.info("Oturum açık", name);
        g.note("Banna Search, Banna Panel ile aynı oturumu kullanır. Yer imleri ve ayarlar bu bilgisayarda tutulur.");
        let r = Rect::new(g.x, g.y, g.w, 44 * g.u);
        g.secondary_button(r, "Oturumu kapat", Control::Logout);
        g.y += 54 * g.u;
        return;
    }
    g.note("E-Banna adresiniz ya da E-Kimlik numaranızla giriş yapın.");
    let u = g.u;
    let r = Rect::new(g.x, g.y, g.w, 44 * u);
    g.input_field(r, "E-Banna adresi ya da E-Kimlik", &fields[0], false, focus == 0, Control::LoginField(0));
    g.y += 54 * u;
    let r = Rect::new(g.x, g.y, g.w, 44 * u);
    g.input_field(r, "Şifre", &fields[1], true, focus == 1, Control::LoginField(1));
    g.y += 58 * u;
    let r = Rect::new(g.x, g.y, g.w, 44 * u);
    g.primary_button(r, "Giriş yap", Control::LoginSubmit);
    g.y += 56 * u;
    if let Some(m) = message {
        g.note(m);
    }
    g.note("Hesabınız yoksa Banna Panel'den oluşturabilirsiniz.");
}

fn main(args: &[&str]) -> i32 {
    let u = ui::unit();
    let info = process::sysinfo();
    let width = (1200 * u).min(info.screen_width as i32 - 40 * u).clamp(640, 1920);
    let height = (800 * u).min(info.screen_height as i32 - 110 * u).clamp(480, 1200);
    let window = match Window::new("Banna Search", width as usize, height as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("banna-search: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let mut app = App {
        window,
        u,
        tabs: alloc::vec![Tab::new()],
        current: 0,
        address: TextInput::default(),
        address_focus: false,
        overlay: Overlay::None,
        settings: Settings::default(),
        bookmarks: Vec::new(),
        history: Vec::new(),
        client: Client::load(),
        account: None,
        login_fields: core::array::from_fn(|_| TextInput::default()),
        login_focus: 0,
        login_message: None,
        pending_login: None,
        last_poll: 0,
        console: Vec::new(),
        hits: Vec::new(),
        hover: None,
        hover_link: None,
        toast: None,
        caret_on: true,
        dirty: true,
        find: None,
        suggestions: Vec::new(),
        suggest_sel: None,
        closed: Vec::new(),
        address_rect: Rect::new(0, 0, 0, 0),
    };
    app.load_state();
    app.load_account();
    let first = args.get(1).map(|a| to_url(a, app.settings.engine)).unwrap_or_else(|| app.settings.home.clone());
    app.navigate(&first, true);
    banna::println!("banna-search: ready");
    let mut last_blink = time::uptime_ms();
    let mut last_save = time::uptime_ms();
    loop {
        // Yükleme adımları (stil, betik, görsel) olaylar arasında ilerler.
        let loading = {
            let tab = &mut app.tabs[app.current];
            match tab.page.as_mut() {
                Some(p) if p.stage != Stage::Done => {
                    if p.load_step() {
                        // Görseller gelirken her biri için tüm sayfa yeniden yerleştirilmez:
                        // büyük sayfalarda (binlerce öğe, yüzlerce görsel) yükleme dakikalar
                        // sürüyordu. Son yerleşimin süresine göre seyreltilir; sonuncusu
                        // yükleme bitince yapılır.
                        let now = time::uptime_ms();
                        let gap = (p.layout_ms * 4).max(400);
                        let due = p.stage != Stage::Images || now.saturating_sub(tab.laid_out_at) >= gap;
                        if p.needs_layout && due {
                            p.relayout(&WebFonts);
                            tab.laid_out_at = time::uptime_ms();
                        }
                        app.dirty = true;
                    }
                    if p.stage == Stage::Done && p.needs_layout {
                        p.relayout(&WebFonts);
                        tab.laid_out_at = time::uptime_ms();
                        app.dirty = true;
                    }
                    true
                }
                _ => {
                    if tab.loading {
                        tab.loading = false;
                        app.dirty = true;
                    }
                    false
                }
            }
        };
        if loading {
            app.collect_page_messages();
            if let Some(p) = app.tab().page.as_mut() {
                let action = p.take_action();
                if !matches!(action, Action::None | Action::Redraw) {
                    app.handle_action(action);
                }
            }
        }
        if app.dirty {
            app.paint();
        }
        let now = time::uptime_ms();
        let timer_wait = app.tab().page.as_ref().and_then(|p| p.next_timer_ms()).unwrap_or(500);
        let timeout = if loading { 1 } else { timer_wait.clamp(1, 500) };
        let event = app.window.event(timeout);
        if now - last_blink >= 530 {
            last_blink = now;
            let typing = app.address_focus
                || app.overlay == Overlay::Account
                || app.tab().page.as_ref().is_some_and(|p| p.interp.doc.focus.is_some());
            if typing {
                app.caret_on = !app.caret_on;
                app.dirty = true;
            }
        }
        if let Some((_, until)) = app.toast
            && now >= until
        {
            app.toast = None;
            app.dirty = true;
        }
        if app.poll_login() {
            app.dirty = true;
        }
        if let Some(p) = app.tab().page.as_mut() {
            let action = p.tick();
            if !matches!(action, Action::None) {
                app.handle_action(action);
            }
        }
        if now - last_save > 15_000 {
            last_save = now;
            App::save_list("history", &app.history);
            App::save_cookies(false);
        }
        let Some(event) = event else { continue };
        match event.kind {
            EVENT_CLOSE => {
                App::save_list("history", &app.history);
                App::save_cookies(false);
                for tab in &app.tabs {
                    if let Some(p) = &tab.page {
                        App::save_storage(p);
                    }
                }
                return 0;
            }
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                app.relayout();
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    app.dirty = true;
                }
                if hovered.is_none()
                    && let Some((px, py)) = app.page_point(event.x, event.y)
                    && let Some(page) = app.tabs[app.current].page.as_mut()
                {
                    let link = page.link_at(px, py);
                    if page.set_hover(px, py) {
                        page.relayout(&WebFonts);
                        app.dirty = true;
                    }
                    if link != app.hover_link {
                        app.hover_link = link;
                        app.dirty = true;
                    }
                }
            }
            EVENT_WHEEL => {
                let step = event.key as i32 as f32 * 60.0;
                if let Some(page) = app.tabs[app.current].page.as_mut()
                    && page.scroll_by(step)
                {
                    app.dirty = true;
                }
            }
            EVENT_KEY => app.key(event.key),
            _ => {}
        }
    }
}

fn disc(c: &mut Canvas, cx: i32, cy: i32, r: i32, color: Argb) {
    c.fill_rounded(Rect::new(cx - r, cy - r, 2 * r, 2 * r), r, color);
}

/// Halka (iç yarıçap `r - w`); `gap` doğruysa sağ üst çeyrek boş bırakılır.
fn ring(c: &mut Canvas, cx: i32, cy: i32, r: i32, w: i32, color: Argb, gap: bool) {
    for dy in -r..=r {
        for dx in -r..=r {
            let d = dx * dx + dy * dy;
            if d <= r * r && d >= (r - w) * (r - w) && !(gap && dx > 0 && dy < 0 && dx > -dy / 2) {
                c.fill(Rect::new(cx + dx, cy + dy, 1, 1), color);
            }
        }
    }
}

/// Yazı tipinde olmayan araç çubuğu simgeleri; çizildiyse `true`.
fn vector_icon(c: &mut Canvas, r: Rect, name: &str, color: Argb, u: i32) -> bool {
    let cx = r.x + r.w / 2;
    let cy = r.y + r.h / 2;
    let s = (r.w.min(r.h) * 3 / 10).max(4);
    let w = (2 * u).max(2) * 3 / 4 + 1;
    match name {
        "reload" => {
            ring(c, cx, cy, s, w, color, true);
            // Yayın üst ucunda sağa bakan ok başı.
            let t = s / 2 + 1;
            let ty = cy - s + w / 2;
            for i in 0..t {
                c.fill(Rect::new(cx + i, ty - (t - i), 1, 2 * (t - i) + 1), color);
            }
        }
        "home" => {
            let roof = s;
            for i in 0..roof {
                c.fill(Rect::new(cx - i - 1, cy - roof + i, 2 * i + 2, 1), color);
            }
            c.fill(Rect::new(cx - s * 2 / 3, cy, s * 4 / 3, s * 2 / 3 + 1), color);
        }
        "menu" => {
            for k in -1..=1 {
                c.fill_rounded(Rect::new(cx - s, cy + k * s * 2 / 3 - w / 2, 2 * s, w), w / 2, color);
            }
        }
        "user" => {
            ring(c, cx, cy - s / 3, s / 2 + 1, w, color, false);
            // Omuzlar: alttaki halkanın yalnızca üst yarısı.
            let (ox, oy, rr) = (cx, cy + s + s / 4, s);
            for dy in -rr..=0 {
                for dx in -rr..=rr {
                    let d = dx * dx + dy * dy;
                    if d <= rr * rr && d >= (rr - w) * (rr - w) {
                        c.fill(Rect::new(ox + dx, oy + dy, 1, 1), color);
                    }
                }
            }
        }
        "lock" => {
            ring(c, cx, cy - s / 4, s / 2 + 1, w, color, false);
            c.fill_rounded(Rect::new(cx - s * 3 / 4, cy - s / 6, s * 3 / 2, s), 2 * u, color);
        }
        "info" => {
            ring(c, cx, cy, s, w, color, false);
            c.fill(Rect::new(cx - w / 2, cy - s / 8, w, s / 2 + 1), color);
            disc(c, cx, cy - s / 2 + 1, w / 2 + 1, color);
        }
        "banna" => disc(c, cx, cy, s / 2 + 2, color),
        _ => return false,
    }
    true
}
