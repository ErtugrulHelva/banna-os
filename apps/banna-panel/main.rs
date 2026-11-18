//! Banna Panel: Banna ekosistemi hesabının (E-Banna) Banna OS istemcisi. Tauri ile yazılmış
//! masaüstü Banna Panel'in (4.8.0) ring 3'te çalışan Rust sürümü; Banna Store'dan kurulur.
//!
//! Sayfalar: Panel (genel bakış), Bildirimler (Banna Notify), Banna Mail (klasörler, ekler,
//! engellenenler), Cihazlar (giriş ve şifre sıfırlama onayları), Banna Agent (bu bilgisayarı
//! eşleştirme, uzaktan kurulum), Güvenlik, Ayarlar ve İndirilenler. Sunucu:
//! `https://ecosys.trybanna.com/api`, bildirimler `https://ecosys.trybanna.com/notify`.
//! Banna Notify'ın WebSocket'i yerine bildirimler arka planda yoklanır.

#![no_std]
#![no_main]

extern crate alloc;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use banna::ecosystem::{self, ApiError, Client, Login};
use banna::gfx::text::{Font, UiFonts};
use banna::gfx::{Argb, Rect, rgb, with_alpha};
use banna::gui::{
    EVENT_CLOSE, EVENT_KEY, EVENT_MOUSE_DOWN, EVENT_MOUSE_MOVE, EVENT_RESIZE, EVENT_WHEEL,
    KEY_DOWN, KEY_PAGE_DOWN, KEY_PAGE_UP, KEY_PASTE, KEY_UP, Window, clipboard,
};
use banna::icons::{self, Icon};
use banna::json::{self, Value};
use banna::task::Task;
use banna::theme::{Prefs, Theme};
use banna::ui::{self, DANGER, Hit, Ui, WHITE};
use banna::{desktop, fs, net, process, time};

banna::entry!(main);

const VERSION: &str = "4.9.0";
const AGENT_CONFIG: &str = "/etc/banna-agent/config";
const SUPPORT_ADDRESS: &str = "destek@banna.com";
const POLL_MS: u64 = 45_000;
const MAIL_COOLDOWN_MS: u64 = 3_000;
const CONTACT_COOLDOWN_MS: u64 = 60_000;
const SUCCESS: Argb = rgb(0x3f, 0xb9, 0x6b);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Page {
    Dashboard,
    Notifications,
    Mail,
    Devices,
    Agent,
    Security,
    Settings,
    Downloads,
}

const NAV: [(Page, &str, &str, Icon); 7] = [
    (Page::Dashboard, "Panel", "Genel Bakış", Icon::Panel),
    (
        Page::Notifications,
        "Bildirimler",
        "Banna Notify",
        Icon::Info,
    ),
    (Page::Mail, "Banna Mail", "Gelen Kutusu", Icon::Editor),
    (Page::Devices, "Cihazlar", "Oturumlar", Icon::Display),
    (
        Page::Agent,
        "Banna Agent",
        "Banna OS & Store",
        Icon::Network,
    ),
    (Page::Security, "Güvenlik", "Hesap", Icon::Users),
    (
        Page::Settings,
        "Ayarlar",
        "Bildirim & İletişim",
        Icon::Settings,
    ),
];

const FOLDERS: [(&str, &str); 4] = [
    ("INBOX", "Gelen Kutusu"),
    ("SPAM", "Spam"),
    ("TRASH", "Çöp Kutusu"),
    ("SENT", "Gönderilenler"),
];
const SENT: usize = 3;

// Metin alanları.
const F_ADDRESS: usize = 0;
const F_PASSWORD: usize = 1;
const F_SEARCH: usize = 2;
const F_TO: usize = 3;
const F_SUBJECT: usize = 4;
const F_BODY: usize = 5;
const F_ATTACH: usize = 6;
const F_RENAME: usize = 7;
const F_CURRENT: usize = 8;
const F_NEW: usize = 9;
const F_DELETE: usize = 10;
const F_ADMIN: usize = 11;
const F_CONTACT_SUBJECT: usize = 12;
const F_CONTACT_BODY: usize = 13;
const FIELDS: usize = 14;

const GREETINGS: [&str; 8] = [
    "Umarım her şey yolundadır. Bugün güzel bir gün olsun!",
    "Tekrar hoş geldin! Umarım günün gülümsemelerle dolu geçer.",
    "Merhaba! Bugün kendine iyi bakmayı unutma.",
    "Hoş geldin. Umarım içindeki güneş hiç kaybolmaz.",
    "Selam! Küçük bir hatırlatma: bugün de harikasın.",
    "Umarım her şey gönlünce gider. Keyifli çalışmalar!",
    "Bir nefes al, bugün senin günün.",
    "Hoş geldin! Ne yaparsan yap, iyi gitsin.",
];

const CHANGELOG: [(&str, &[&str]); 3] = [
    (
        "4.9.0",
        &[
            "Banna OS hata raporları: Banna Hata Raporlama Sistemi'nin gönderdiği takılma ve çökme raporları, ana onay cihazındaysanız Cihazlar sayfasında görünür; ayrıntılı günlüğü açıp kopyalayabilir ve silebilirsiniz.",
        ],
    ),
    (
        "4.8.0 (Banna OS)",
        &[
            "Banna Panel artık Banna OS'te: Rust ile yeniden yazıldı, Banna Store'dan kurulur ve ring 3'te çalışır.",
            "Bildirimler (Banna Notify): okundu, sil, yalnızca güvenlik, seçip toplu silme. Yeni bildirimler arka planda yoklanır ve pencerenin köşesinde gösterilir.",
            "Banna Mail: Gelen, Spam, Çöp ve Gönderilenler klasörleri, arama, favoriler, önemli, taşıma, klasör boşaltma, engellenen gönderenler, dosya eki gönderme ve ekleri İndirilenler klasörüne kaydetme.",
            "Cihazlar: yeni cihaz girişi ve şifre sıfırlama onayları, yeniden adlandırma, onay cihazı seçimi.",
            "Banna Agent: bu bilgisayarı tek tıkla eşleştirme, diğer cihazlar için jeton, eşleşmiş cihazlara Banna Store'dan uzaktan kurulum ve son etkinlik.",
        ],
    ),
    (
        "Masaüstü sürümünden farklar",
        &[
            "Tema masaüstünü izler (Ayarlar uygulaması). Üst durum şeridi, profil resmi yükleme ve otomatik güncelleme yok; güncellemeleri Banna Store yapar.",
        ],
    ),
];

#[derive(Clone, PartialEq)]
enum Action {
    DeleteMail,
    EmptyFolder,
    RemoveDevice(String),
    ApproveReset(String),
    DeleteNotifications,
}

#[derive(Clone, PartialEq)]
enum Control {
    Nothing,
    Field(usize),
    Login,
    Remember,
    Page(Page),
    Logout,
    Ask(Action),
    Do(Action),
    CloseModal,
    Toast,
    // Bildirimler
    OpenNotification(String),
    NotifRead(String),
    NotifDelete(String),
    SecurityOnly,
    NotifReload,
    ReadAll,
    Selecting(bool),
    ToggleSelect(String),
    SelectAll,
    MoreNotifications,
    // Banna Mail
    Folder(usize),
    Favorites,
    OpenMail(String),
    Flag(&'static str),
    Move(&'static str),
    Download(String, String),
    Compose,
    Send,
    AddAttachment,
    RemoveAttachment(usize),
    Blocked,
    Unblock(String),
    // Cihazlar
    ReloadDevices,
    OpenCrash(String),
    CopyCrash,
    DeleteCrash(String),
    StartRename(String),
    SaveRename,
    CancelRename,
    ApprovalDevice(String, bool),
    Respond(String, PendingKind, bool),
    // Banna Agent
    ReloadAgent,
    IssueToken(String),
    CopyToken(String),
    Target(String),
    RemoteInstall(String),
    PairAgent,
    UnpairAgent,
    // Güvenlik
    ChangePassword,
    DeleteAccount(bool),
    ConfirmDelete,
    // Ayarlar
    Toasts,
    Legal(&'static str),
    Changelog,
    SendContact,
    Wizard,
    DesktopSettings,
    Reload,
    CopyPath(String),
}

#[derive(Clone)]
struct Notification {
    id: String,
    app: String,
    category: String,
    title: String,
    body: String,
    resource_type: String,
    resource_id: String,
    created: String,
    read: bool,
}

impl Notification {
    fn parse(v: &Value) -> Self {
        let s = |k: &str| String::from(v.get(k).as_str());
        Self {
            id: s("id"),
            app: s("application_id"),
            category: s("category"),
            title: s("title"),
            body: s("body"),
            resource_type: s("resource_type"),
            resource_id: s("resource_id"),
            created: s("created_at"),
            read: !v.get("read_at").is_null(),
        }
    }

    fn security(&self) -> bool {
        self.category == "security"
    }
}

struct Doc {
    title: String,
    updated: String,
    sections: Vec<(String, Vec<String>)>,
}

enum Modal {
    Confirm(String, Action),
    Compose,
    Blocked,
    Document(Doc),
}

struct Download {
    name: String,
    path: String,
    size: usize,
}

/// Onay cihazının yanıtladığı istek türü.
#[derive(Clone, Copy, PartialEq)]
enum PendingKind {
    Login,
    Reset,
}

impl PendingKind {
    fn base(self) -> &'static str {
        match self {
            PendingKind::Login => "/auth/login/approval",
            PendingKind::Reset => "/auth/password-reset",
        }
    }
}

struct Pending {
    id: String,
    kind: PendingKind,
    device: String,
    ip: String,
    created: String,
}

struct State {
    fonts: UiFonts,
    u: i32,
    theme: Theme,
    tz_minutes: i64,
    client: Client,
    // Oturum
    booting: bool,
    signed_in: bool,
    remember: bool,
    toasts: bool,
    pending_login: Option<(String, String)>,
    last_approval_poll: u64,
    me: Value,
    page: Page,
    fields: [ui::TextInput; FIELDS],
    focus: Option<usize>,
    message: Option<(String, bool)>,
    status: Option<String>,
    modal: Option<Modal>,
    toast: Option<(String, String, u64)>,
    scroll: i32,
    max_scroll: i32,
    modal_scroll: i32,
    mouse: (i32, i32),
    last_poll: u64,
    // Banna Notify
    notify_enabled: Option<bool>,
    notify_token: String,
    notify_expires: i64,
    notify_registered: bool,
    notifications: Vec<Notification>,
    notif_unread: i64,
    notif_cursor: Option<String>,
    security_only: bool,
    selecting: bool,
    selected: Vec<String>,
    // Banna Mail
    mail_unread: i64,
    folder: usize,
    favorites_only: bool,
    mail_list: Option<Vec<Value>>,
    mail_detail: Option<Value>,
    blocked: Option<Vec<Value>>,
    attachments: Vec<String>,
    last_mail_send: u64,
    list_scroll: i32,
    list_max: i32,
    detail_scroll: i32,
    detail_max: i32,
    // Cihazlar
    devices: Option<Vec<Value>>,
    /// Ana onay cihazındaysa Banna OS hata raporları ve açık olanın ayrıntısı.
    crashes: Vec<Value>,
    crash_open: Option<Value>,
    pending: Vec<Pending>,
    known_pending: Vec<String>,
    renaming: Option<String>,
    // Banna Agent
    os_status: Value,
    store_apps: Option<Vec<Value>>,
    tokens: Vec<(String, String)>,
    target: String,
    agent_paired: bool,
    task: Option<Task>,
    // Güvenlik ve ayarlar
    deleting_account: bool,
    last_contact: u64,
    downloads: Vec<Download>,
}

fn home() -> String {
    format!("/home/{}", process::username())
}

fn config_path() -> String {
    format!("{}/.config/banna/panel.conf", home())
}

fn random_index(n: usize) -> usize {
    let mut b = [0u8; 4];
    net::random(&mut b);
    u32::from_le_bytes(b) as usize % n.max(1)
}

/// URL sorgu parametresi kodlaması.
fn encode(text: &str) -> String {
    let mut out = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + if m <= 2 { 1 } else { 0 }, m, d)
}

/// ISO 8601 (UTC) → yerel saatle "24.10.2026 19:50".
fn date(iso: &str, tz_minutes: i64) -> String {
    let num = |a: usize, b: usize| iso.get(a..b).and_then(|s| s.parse::<i64>().ok());
    let (Some(y), Some(mo), Some(d), Some(h), Some(mi)) =
        (num(0, 4), num(5, 7), num(8, 10), num(11, 13), num(14, 16))
    else {
        return if iso.is_empty() {
            String::from("-")
        } else {
            String::from(iso)
        };
    };
    let utc = iso.ends_with('Z') || iso.len() <= 19;
    let mut minutes = days_from_civil(y, mo, d) * 1440 + h * 60 + mi;
    if utc {
        minutes += tz_minutes;
    }
    let (y, mo, d) = civil_from_days(minutes.div_euclid(1440));
    let rest = minutes.rem_euclid(1440);
    format!("{:02}.{:02}.{} {:02}:{:02}", d, mo, y, rest / 60, rest % 60)
}

fn bytes_text(n: usize) -> String {
    if n < 1024 {
        format!("{} B", n)
    } else if n < 1024 * 1024 {
        format!("{}.{} KB", n / 1024, n % 1024 * 10 / 1024)
    } else {
        format!(
            "{}.{} MB",
            n / (1024 * 1024),
            n % (1024 * 1024) * 10 / (1024 * 1024)
        )
    }
}

fn account_type(kind: &str) -> &str {
    match kind {
        "BIREYSEL" => "Bireysel Hesap",
        "OGRENCI" => "Öğrenci Hesabı",
        other => other,
    }
}

/// Metni genişliğe göre satırlara böler.
fn wrap(font: &Font, text: &str, width: i32) -> Vec<String> {
    let mut lines = Vec::new();
    for para in text.split('\n') {
        let mut line = String::new();
        for word in para.split(' ') {
            let candidate = if line.is_empty() {
                String::from(word)
            } else {
                format!("{} {}", line, word)
            };
            if font.measure(&candidate) > width && !line.is_empty() {
                lines.push(core::mem::replace(&mut line, String::from(word)));
            } else {
                line = candidate;
            }
        }
        lines.push(line);
    }
    lines
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// `~/İndirilenler` içinde var olmayan bir yol.
fn download_path(name: &str) -> String {
    let dir = format!("{}/İndirilenler", home());
    let _ = fs::mkdir(&dir);
    let clean: String = name
        .chars()
        .map(|c| if c == '/' || c.is_control() { '_' } else { c })
        .collect();
    let clean = if clean.is_empty() {
        String::from("ek")
    } else {
        clean
    };
    let (stem, ext) = match clean.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (String::from(s), format!(".{}", e)),
        _ => (clean.clone(), String::new()),
    };
    let mut path = format!("{}/{}", dir, clean);
    let mut n = 1;
    while fs::stat(&path).is_ok() {
        path = format!("{}/{} ({}){}", dir, stem, n, ext);
        n += 1;
    }
    path
}

impl State {
    fn ok(&mut self, text: &str) {
        self.message = Some((String::from(text), false));
    }

    fn fail(&mut self, e: &ApiError) {
        self.message = Some((e.text(), true));
    }

    fn save_config(&self) {
        let _ = fs::mkdir(&format!("{}/.config", home()));
        let _ = fs::mkdir(&format!("{}/.config/banna", home()));
        let text = format!(
            "remember={}\ntoasts={}\n",
            self.remember as u8, self.toasts as u8
        );
        let _ = fs::write(&config_path(), text.as_bytes());
    }

    fn load_config(&mut self) {
        let Ok(data) = fs::read(&config_path()) else {
            return;
        };
        for line in String::from_utf8_lossy(&data).lines() {
            match line.split_once('=') {
                Some(("remember", v)) => self.remember = v.trim() != "0",
                Some(("toasts", v)) => self.toasts = v.trim() != "0",
                _ => {}
            }
        }
    }

    fn approval_device(&self) -> bool {
        self.me.get("device").get("isApprovalDevice").as_bool()
    }

    fn address(&self) -> &str {
        self.me.get("current").get("ebannaAddress").as_str()
    }

    // ---- Oturum ----

    fn load_account(&mut self) {
        match self.client.get("/auth/me") {
            Ok(me) => {
                self.me = me;
                if !self.signed_in {
                    self.signed_in = true;
                    let g = GREETINGS[random_index(GREETINGS.len())];
                    self.show_toast("Hoş geldin", g);
                    banna::println!("banna-panel: signed in");
                }
            }
            Err(e) => {
                self.signed_in = false;
                if e.status != 401 {
                    self.fail(&e);
                }
                return;
            }
        }
        self.refresh_counts();
        self.load_page();
        self.last_poll = time::uptime_ms();
    }

    fn login(&mut self) {
        if self.fields[F_ADDRESS].trim().is_empty() || self.fields[F_PASSWORD].is_empty() {
            self.message = Some((String::from("E-Banna adresini ve şifreyi girin."), true));
            return;
        }
        let address = self.fields[F_ADDRESS].trim().to_ascii_lowercase();
        let password = core::mem::take(&mut self.fields[F_PASSWORD]);
        self.save_config();
        match self.client.login(&address, &password) {
            Ok(Login::Done) => {
                self.message = None;
                self.focus = None;
                self.load_account();
            }
            Ok(Login::Pending { approval_id, code }) => {
                self.message = None;
                self.pending_login = Some((approval_id, code));
                self.last_approval_poll = time::uptime_ms();
            }
            Err(e) => self.fail(&e),
        }
    }

    fn poll_login_approval(&mut self) -> bool {
        let Some((id, _)) = self.pending_login.clone() else {
            return false;
        };
        if time::uptime_ms() - self.last_approval_poll < 2500 {
            return false;
        }
        self.last_approval_poll = time::uptime_ms();
        match self.client.approval_status(&id).as_deref() {
            Ok("APPROVED") => {
                self.pending_login = None;
                self.load_account();
            }
            Ok("DENIED") => {
                self.pending_login = None;
                self.message = Some((String::from("Giriş isteği reddedildi."), true));
            }
            Ok("EXPIRED") | Err(_) => {
                self.pending_login = None;
                self.message = Some((String::from("Onay süresi doldu, tekrar deneyin."), true));
            }
            _ => return false,
        }
        true
    }

    fn logout(&mut self) {
        self.client.logout();
        self.signed_in = false;
        self.me = Value::Null;
        self.notify_token.clear();
        self.notify_registered = false;
        self.notifications.clear();
        self.mail_list = None;
        self.mail_detail = None;
        self.devices = None;
        self.store_apps = None;
        self.modal = None;
        self.page = Page::Dashboard;
        self.ok("Oturum kapatıldı.");
    }

    fn show_toast(&mut self, title: &str, body: &str) {
        if self.toasts {
            self.toast = Some((
                String::from(title),
                String::from(body),
                time::uptime_ms() + 6000,
            ));
        }
    }

    // ---- Banna Notify ----

    fn notify_token(&mut self, force: bool) -> Option<String> {
        if !force && !self.notify_token.is_empty() && self.notify_expires - time::unix_time() > 60 {
            return Some(self.notify_token.clone());
        }
        match self.client.post("/notify/token", "{}") {
            Ok(v) => {
                self.notify_token = String::from(v.get("accessToken").as_str());
                self.notify_expires = v.get("expiresAt").as_i64();
                self.notify_enabled = Some(!self.notify_token.is_empty());
                if !self.notify_registered && !self.notify_token.is_empty() {
                    let device = v.get("device");
                    let body = json::object(&[
                        ("installation_id", device.get("installationId").as_str()),
                        ("platform", "banna_os"),
                        ("name", device.get("name").as_str()),
                    ]);
                    self.notify_registered = ecosystem::notify_request(
                        &self.notify_token,
                        "POST",
                        "/v1/devices/register",
                        Some(&body),
                    )
                    .is_ok();
                }
                Some(self.notify_token.clone()).filter(|t| !t.is_empty())
            }
            Err(e) => {
                if e.status == 503 {
                    self.notify_enabled = Some(false);
                }
                None
            }
        }
    }

    fn notify_call(
        &mut self,
        method: &str,
        path: &str,
        body: Option<&str>,
    ) -> Result<Value, ApiError> {
        let unavailable = ApiError {
            status: 503,
            code: String::new(),
            message: String::from("Bildirim servisine ulaşılamadı."),
        };
        let Some(token) = self.notify_token(false) else {
            return Err(unavailable);
        };
        match ecosystem::notify_request(&token, method, path, body) {
            Err(e) if e.status == 401 => {
                let token = self.notify_token(true).ok_or(unavailable)?;
                ecosystem::notify_request(&token, method, path, body)
            }
            other => other,
        }
    }

    fn merge_notifications(&mut self, page: &Value) {
        for n in page.get("notifications").items() {
            if !n.get("archived_at").is_null() {
                continue;
            }
            let n = Notification::parse(n);
            match self.notifications.iter_mut().find(|x| x.id == n.id) {
                Some(x) => *x = n,
                None => self.notifications.push(n),
            }
        }
        // UUIDv7: sözlük sırası = zaman sırası.
        self.notifications.sort_by(|a, b| b.id.cmp(&a.id));
        self.notif_cursor =
            Some(String::from(page.get("next_cursor").as_str())).filter(|c| !c.is_empty());
    }

    fn load_notifications(&mut self) {
        match self.notify_call("GET", "/v1/notifications?limit=30", None) {
            Ok(page) => {
                self.notifications.clear();
                self.merge_notifications(&page);
                self.refresh_notif_unread();
            }
            Err(e) => {
                if self.notify_enabled != Some(false) {
                    self.fail(&e);
                }
            }
        }
    }

    fn refresh_notif_unread(&mut self) {
        if let Ok(v) = self.notify_call("GET", "/v1/notifications/unread-count", None) {
            self.notif_unread = v.get("unread_count").as_i64();
        }
    }

    fn more_notifications(&mut self) {
        let Some(cursor) = self.notif_cursor.clone() else {
            return;
        };
        let path = format!("/v1/notifications?limit=30&cursor={}", encode(&cursor));
        match self.notify_call("GET", &path, None) {
            Ok(page) => self.merge_notifications(&page),
            Err(e) => self.fail(&e),
        }
    }

    fn mark_read(&mut self, id: &str, open: bool) {
        let path = format!(
            "/v1/notifications/{}/{}",
            id,
            if open { "open" } else { "read" }
        );
        match self.notify_call("POST", &path, Some("{}")) {
            Ok(_) => {
                if let Some(n) = self.notifications.iter_mut().find(|n| n.id == id) {
                    n.read = true;
                }
                self.refresh_notif_unread();
            }
            Err(_) if open => {}
            Err(_) => {
                self.message = Some((
                    String::from("İşlem tamamlanamadı. Bağlantınızı kontrol edip tekrar deneyin."),
                    true,
                ))
            }
        }
    }

    /// Bildirimleri kalıcı siler (istek başına en fazla 100); silinemeyenler seçili kalır.
    fn delete_notifications(&mut self, ids: Vec<String>) {
        let mut failed = Vec::new();
        for chunk in ids.chunks(100) {
            let list: Vec<String> = chunk.iter().map(|id| json::quote(id)).collect();
            let body = format!("{{\"ids\":[{}]}}", list.join(","));
            match self.notify_call("POST", "/v1/notifications/delete", Some(&body)) {
                Ok(v) => {
                    self.notif_unread = v.get("unread_count").as_i64();
                    self.notifications.retain(|n| !chunk.contains(&n.id));
                }
                Err(_) => failed.extend(chunk.iter().cloned()),
            }
        }
        if failed.is_empty() {
            self.selecting = false;
            self.selected.clear();
        } else {
            self.message = Some((
                format!("{} bildirim silinemedi. Tekrar deneyin.", failed.len()),
                true,
            ));
            self.selected = failed;
        }
    }

    fn open_notification(&mut self, id: &str) {
        let Some(n) = self.notifications.iter().find(|n| n.id == id).cloned() else {
            return;
        };
        self.mark_read(&n.id, true);
        if n.app == "banna_mail" && n.resource_type == "message" && !n.resource_id.is_empty() {
            self.set_page(Page::Mail);
            if self.folder != 0 {
                self.folder = 0;
                self.load_mail_list();
            }
            self.open_mail(&n.resource_id);
        } else if n.security() {
            self.set_page(Page::Devices);
        }
    }

    // ---- Sayaçlar ve arka plan yoklaması ----

    fn refresh_mail_unread(&mut self) {
        if let Ok(v) = self.client.get("/mail/inbox/unread-count") {
            self.mail_unread = v.get("count").as_i64();
        }
    }

    fn load_pending(&mut self) {
        self.pending.clear();
        if !self.approval_device() {
            return;
        }
        for kind in [PendingKind::Reset, PendingKind::Login] {
            if let Ok(list) = self.client.get(&format!("{}/pending", kind.base())) {
                for p in list.items() {
                    self.pending.push(Pending {
                        id: String::from(p.get("id").as_str()),
                        kind,
                        device: String::from(p.get("requestingDeviceName").as_str()),
                        ip: String::from(p.get("requestingIp").as_str()),
                        created: String::from(p.get("createdAt").as_str()),
                    });
                }
            }
        }
    }

    fn refresh_counts(&mut self) {
        self.refresh_mail_unread();
        if self.notify_enabled != Some(false) {
            self.refresh_notif_unread();
        }
        self.load_pending();
        self.known_pending = self.pending.iter().map(|p| p.id.clone()).collect();
    }

    /// Yeni bildirim, mail ve onay isteklerini yoklar; yenilerini köşede gösterir.
    fn poll(&mut self) {
        self.last_poll = time::uptime_ms();
        if self.notify_enabled != Some(false) {
            let before = self.notif_unread;
            self.refresh_notif_unread();
            if self.notif_unread > before {
                let known: Vec<String> = self.notifications.iter().map(|n| n.id.clone()).collect();
                if let Ok(page) = self.notify_call("GET", "/v1/notifications?limit=30", None) {
                    self.merge_notifications(&page);
                }
                if let Some(n) = self
                    .notifications
                    .iter()
                    .find(|n| !n.read && !known.contains(&n.id))
                    .cloned()
                {
                    self.show_toast(&n.title, &n.body);
                    if n.app == "banna_mail"
                        && self.page == Page::Mail
                        && self.mail_detail.is_none()
                    {
                        self.load_mail_list();
                    }
                }
            }
        }
        let before = self.mail_unread;
        self.refresh_mail_unread();
        if self.notify_enabled == Some(false) && self.mail_unread > before {
            let delta = self.mail_unread - before;
            let text = if delta == 1 {
                String::from("1 yeni mesajınız var.")
            } else {
                format!("{} yeni mesajınız var.", delta)
            };
            self.show_toast("Yeni Banna Mail", &text);
        }
        if self.approval_device() {
            self.load_pending();
            if self.notify_enabled == Some(false)
                && let Some(p) = self
                    .pending
                    .iter()
                    .find(|p| !self.known_pending.contains(&p.id))
            {
                let body = format!("{} bu hesaba giriş yapmak istiyor.", p.device);
                self.show_toast("Yeni cihazdan giriş isteği", &body);
            }
            self.known_pending = self.pending.iter().map(|p| p.id.clone()).collect();
        }
    }

    // ---- Sayfalar ----

    fn set_page(&mut self, page: Page) {
        self.page = page;
        self.scroll = 0;
        self.message = None;
        self.focus = None;
        self.selecting = false;
        self.selected.clear();
        self.renaming = None;
        self.deleting_account = false;
        self.load_page();
    }

    fn load_page(&mut self) {
        if !self.signed_in {
            return;
        }
        match self.page {
            Page::Dashboard => {}
            Page::Notifications => self.load_notifications(),
            Page::Mail => {
                self.refresh_mail_unread();
                if self.mail_list.is_none() {
                    self.load_mail_list();
                }
            }
            Page::Devices => self.load_devices(),
            Page::Agent => self.load_agent(),
            _ => {}
        }
    }

    // ---- Banna Mail ----

    fn load_mail_list(&mut self) {
        self.mail_detail = None;
        self.list_scroll = 0;
        let search = String::from(self.fields[F_SEARCH].trim());
        let path = if self.folder == SENT {
            if search.is_empty() {
                String::from("/mail/sent")
            } else {
                format!("/mail/sent?q={}", encode(&search))
            }
        } else {
            let mut p = format!("/mail/inbox?folder={}", FOLDERS[self.folder].0);
            if !search.is_empty() {
                p.push_str(&format!("&q={}", encode(&search)));
            }
            if self.favorites_only {
                p.push_str("&favorites=true");
            }
            p
        };
        match self.client.get(&path) {
            Ok(v) => self.mail_list = Some(v.items().to_vec()),
            Err(e) => {
                self.mail_list = Some(Vec::new());
                self.fail(&e);
            }
        }
    }

    fn open_mail(&mut self, id: &str) {
        let path = if self.folder == SENT {
            format!("/mail/sent/{}", id)
        } else {
            format!("/mail/inbox/{}", id)
        };
        match self.client.get(&path) {
            Ok(m) => {
                self.detail_scroll = 0;
                if self.folder != SENT {
                    if let Some(list) = self.mail_list.as_mut()
                        && let Some(Value::Object(fields)) =
                            list.iter_mut().find(|x| x.get("id").as_str() == id)
                    {
                        fields.retain(|(k, _)| k != "readAt");
                        fields.push((String::from("readAt"), m.get("readAt").clone()));
                    }
                    self.refresh_mail_unread();
                }
                self.mail_detail = Some(m);
            }
            Err(e) => self.fail(&e),
        }
    }

    fn detail_id(&self) -> Option<String> {
        self.mail_detail
            .as_ref()
            .map(|d| String::from(d.get("id").as_str()))
    }

    fn toggle_flag(&mut self, field: &str) {
        let Some(detail) = self.mail_detail.as_ref() else {
            return;
        };
        let id = String::from(detail.get("id").as_str());
        let next = !detail.get(field).as_bool();
        let body = format!("{{{}:{}}}", json::quote(field), next);
        match self.client.patch(&format!("/mail/inbox/{}", id), &body) {
            Ok(_) => {
                if let Some(Value::Object(fields)) = self.mail_detail.as_mut() {
                    fields.retain(|(k, _)| k != field);
                    fields.push((String::from(field), Value::Bool(next)));
                }
                if let Some(Value::Object(fields)) = self
                    .mail_list
                    .as_mut()
                    .and_then(|l| l.iter_mut().find(|m| m.get("id").as_str() == id))
                {
                    fields.retain(|(k, _)| k != field);
                    fields.push((String::from(field), Value::Bool(next)));
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    fn move_mail(&mut self, folder: &str) {
        let Some(id) = self.detail_id() else {
            return;
        };
        let body = json::object(&[("folder", folder)]);
        match self.client.patch(&format!("/mail/inbox/{}", id), &body) {
            Ok(_) => {
                self.load_mail_list();
                self.refresh_mail_unread();
                self.ok(match folder {
                    "TRASH" => "Çöp kutusuna taşındı.",
                    "SPAM" => "Spam olarak işaretlendi.",
                    _ => "Gelen kutusuna taşındı.",
                });
            }
            Err(e) => self.fail(&e),
        }
    }

    fn delete_mail(&mut self) {
        let Some(id) = self.detail_id() else {
            return;
        };
        let path = if self.folder == SENT {
            format!("/mail/sent/{}", id)
        } else {
            format!("/mail/inbox/{}", id)
        };
        match self.client.delete(&path) {
            Ok(_) => {
                self.load_mail_list();
                self.refresh_mail_unread();
                self.ok("Mesaj silindi.");
            }
            Err(e) => self.fail(&e),
        }
    }

    fn empty_folder(&mut self) {
        if self.folder == SENT {
            return;
        }
        let path = format!("/mail/inbox/empty/{}", FOLDERS[self.folder].0);
        match self.client.post(&path, "{}") {
            Ok(_) => {
                self.load_mail_list();
                self.refresh_mail_unread();
                self.ok("Klasör boşaltıldı.");
            }
            Err(e) => self.fail(&e),
        }
    }

    fn download(&mut self, id: &str, name: &str) {
        match self.client.download(&format!("/mail/attachments/{}", id)) {
            Ok(data) => {
                let path = download_path(name);
                match fs::write(&path, &data) {
                    Ok(()) => {
                        self.message = Some((format!("{} kaydedildi.", path), false));
                        self.downloads.insert(
                            0,
                            Download {
                                name: String::from(file_name(&path)),
                                path,
                                size: data.len(),
                            },
                        );
                    }
                    Err(e) => {
                        self.message = Some((
                            format!("{} yazılamadı ({}).", path, fs::error_text(e)),
                            true,
                        ))
                    }
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    fn add_attachment(&mut self) {
        let raw = String::from(self.fields[F_ATTACH].trim());
        if raw.is_empty() {
            return;
        }
        let path = if raw.starts_with('/') {
            raw
        } else {
            fs::normalize(&home(), &raw)
        };
        match fs::stat(&path) {
            Ok(st) if st.kind != fs::KIND_DIRECTORY => {
                if !self.attachments.contains(&path) {
                    self.attachments.push(path);
                }
                self.fields[F_ATTACH].clear();
                self.message = None;
            }
            Ok(_) => self.message = Some((String::from("Klasör eklenemez."), true)),
            Err(e) => self.message = Some((format!("{}: {}", path, fs::error_text(e)), true)),
        }
    }

    fn send_mail(&mut self) {
        if time::uptime_ms().saturating_sub(self.last_mail_send) < MAIL_COOLDOWN_MS
            && self.last_mail_send != 0
        {
            self.message = Some((String::from("Biraz bekleyip tekrar deneyin."), true));
            return;
        }
        let to: Vec<String> = self.fields[F_TO]
            .split([',', ' ', ';', '\n'])
            .map(|a| a.trim().to_ascii_lowercase())
            .filter(|a| !a.is_empty())
            .collect();
        let subject = String::from(self.fields[F_SUBJECT].trim());
        let body = String::from(self.fields[F_BODY].trim());
        if to.is_empty() || subject.is_empty() || body.is_empty() {
            self.message = Some((String::from("Alıcı, konu ve mesaj gerekli."), true));
            return;
        }
        let mut files = Vec::new();
        for path in &self.attachments {
            match fs::read(path) {
                Ok(data) => files.push((String::from(file_name(path)), data)),
                Err(e) => {
                    self.message = Some((format!("{}: {}", path, fs::error_text(e)), true));
                    return;
                }
            }
        }
        let recipients = format!(
            "[{}]",
            to.iter()
                .map(|a| json::quote(a))
                .collect::<Vec<_>>()
                .join(",")
        );
        let parts: Vec<(&str, &[u8])> = files
            .iter()
            .map(|(n, d)| (n.as_str(), d.as_slice()))
            .collect();
        self.last_mail_send = time::uptime_ms();
        match self.client.post_multipart(
            "/mail/send",
            &[
                ("toEbannaAddresses", &recipients),
                ("subject", &subject),
                ("body", &body),
            ],
            &parts,
        ) {
            Ok(res) => {
                let missing: Vec<&str> = res
                    .get("notFound")
                    .items()
                    .iter()
                    .map(|v| v.as_str())
                    .collect();
                let sent = res.get("messageIds").items().len();
                if !missing.is_empty() {
                    self.message = Some((
                        format!("Bulunamayan alıcı(lar): {}", missing.join(", ")),
                        true,
                    ));
                    if sent == 0 {
                        return;
                    }
                } else {
                    self.ok("Mail gönderildi.");
                }
                for f in [F_TO, F_SUBJECT, F_BODY, F_ATTACH] {
                    self.fields[f].clear();
                }
                self.attachments.clear();
                self.modal = None;
                self.focus = None;
                if self.folder == SENT {
                    self.load_mail_list();
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    fn load_blocked(&mut self) {
        self.modal = Some(Modal::Blocked);
        self.modal_scroll = 0;
        match self.client.get("/mail/blocked") {
            Ok(v) => self.blocked = Some(v.items().to_vec()),
            Err(e) => {
                self.blocked = Some(Vec::new());
                self.fail(&e);
            }
        }
    }

    fn unblock(&mut self, id: &str) {
        match self.client.delete(&format!("/mail/blocked/{}", id)) {
            Ok(_) => {
                if let Some(list) = self.blocked.as_mut() {
                    list.retain(|b| b.get("id").as_str() != id);
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    // ---- Cihazlar ----

    fn load_devices(&mut self) {
        match self.client.get("/devices") {
            Ok(v) => self.devices = Some(v.items().to_vec()),
            Err(e) => {
                self.devices = Some(Vec::new());
                self.fail(&e);
            }
        }
        self.load_pending();
        self.crashes = if self.approval_device() {
            self.client
                .get("/banna-os/crash-reports")
                .map(|v| v.items().to_vec())
                .unwrap_or_default()
        } else {
            Vec::new()
        };
    }

    fn open_crash(&mut self, id: &str) {
        if self
            .crash_open
            .as_ref()
            .is_some_and(|c| c.get("id").as_str() == id)
        {
            self.crash_open = None;
            return;
        }
        match self.client.get(&format!("/banna-os/crash-reports/{}", id)) {
            Ok(v) => self.crash_open = Some(v),
            Err(e) => self.fail(&e),
        }
    }

    fn delete_crash(&mut self, id: &str) {
        match self
            .client
            .delete(&format!("/banna-os/crash-reports/{}", id))
        {
            Ok(_) => {
                if self
                    .crash_open
                    .as_ref()
                    .is_some_and(|c| c.get("id").as_str() == id)
                {
                    self.crash_open = None;
                }
                self.crashes.retain(|c| c.get("id").as_str() != id);
                self.ok("Hata raporu silindi.");
            }
            Err(e) => self.fail(&e),
        }
    }

    fn save_rename(&mut self) {
        let Some(id) = self.renaming.clone() else {
            return;
        };
        let name = String::from(self.fields[F_RENAME].trim());
        if name.is_empty() {
            return;
        }
        let body = json::object(&[("deviceName", &name)]);
        match self.client.patch(&format!("/devices/{}", id), &body) {
            Ok(_) => {
                self.renaming = None;
                self.focus = None;
                self.ok("Cihaz adı güncellendi.");
                self.load_devices();
                if self.me.get("device").get("id").as_str() == id {
                    self.load_account();
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    fn set_approval_device(&mut self, id: &str, enabled: bool) {
        let body = format!("{{\"enabled\":{}}}", enabled);
        match self
            .client
            .patch(&format!("/devices/{}/approval-device", id), &body)
        {
            Ok(_) => {
                self.load_devices();
                if let Ok(me) = self.client.get("/auth/me") {
                    self.me = me;
                }
            }
            Err(e) => self.fail(&e),
        }
    }

    fn remove_device(&mut self, id: &str) {
        match self.client.delete(&format!("/devices/{}", id)) {
            Ok(_) => {
                self.ok("Cihaz kaldırıldı.");
                self.load_devices();
            }
            Err(e) => self.fail(&e),
        }
    }

    fn respond(&mut self, id: &str, kind: PendingKind, approve: bool) {
        let path = format!(
            "{}/{}/{}",
            kind.base(),
            id,
            if approve { "approve" } else { "deny" }
        );
        match self.client.post(&path, "{}") {
            Ok(_) => self.ok(if approve {
                "İstek onaylandı."
            } else {
                "İstek reddedildi."
            }),
            Err(e) => self.fail(&e),
        }
        self.load_pending();
    }

    // ---- Banna Agent ----

    fn load_agent(&mut self) {
        self.agent_paired = fs::stat(AGENT_CONFIG).is_ok();
        match self.client.get("/banna-os/status") {
            Ok(v) => self.os_status = v,
            Err(e) => self.fail(&e),
        }
        self.store_apps = Some(
            self.client
                .get("/store/apps")
                .map(|v| v.items().to_vec())
                .unwrap_or_default(),
        );
        let devices = self.os_status.get("devices").items();
        if !devices
            .iter()
            .any(|d| d.get("id").as_str() == self.target && d.get("agentPaired").as_bool())
        {
            self.target = devices
                .iter()
                .find(|d| d.get("agentPaired").as_bool())
                .map(|d| String::from(d.get("id").as_str()))
                .unwrap_or_default();
        }
    }

    fn issue_token(&mut self, device: &str) {
        let body = json::object(&[("deviceId", device)]);
        match self.client.post("/banna-os/agent-token", &body) {
            Ok(v) => {
                let token = String::from(v.get("token").as_str());
                self.tokens.retain(|(d, _)| d != device);
                self.tokens.push((String::from(device), token));
                self.load_agent();
            }
            Err(e) => self.fail(&e),
        }
    }

    fn remote_install(&mut self, package: &str) {
        if self.target.is_empty() {
            return;
        }
        let body = json::object(&[("deviceId", &self.target), ("packageName", package)]);
        match self.client.post("/store/install", &body) {
            Ok(_) => {
                self.message = Some((format!("{} kurulum kuyruğuna eklendi.", package), false));
                self.load_agent();
            }
            Err(e) => self.fail(&e),
        }
    }

    fn need_admin(&mut self) -> bool {
        if process::uid() != 0 && self.fields[F_ADMIN].is_empty() {
            self.focus = Some(F_ADMIN);
            self.message = Some((
                String::from("Bu bilgisayarın yönetici parolasını girin."),
                true,
            ));
            return true;
        }
        false
    }

    /// Bu bilgisayar için ajan jetonu alır ve `banna-agent pair`'ı yönetici olarak çalıştırır.
    fn pair_agent(&mut self) {
        if self.task.is_some() || self.need_admin() {
            return;
        }
        let device = String::from(self.me.get("device").get("id").as_str());
        let body = json::object(&[("deviceId", &device)]);
        let token = match self.client.post("/banna-os/agent-token", &body) {
            Ok(v) => String::from(v.get("token").as_str()),
            Err(e) => {
                self.fail(&e);
                return;
            }
        };
        let password = core::mem::take(&mut self.fields[F_ADMIN]);
        let args = ["banna-agent", "pair", token.as_str()];
        let started = if process::uid() == 0 {
            Task::start("/bin/banna-agent", &args)
        } else {
            Task::start_as_root("/bin/banna-agent", &args, &password)
        };
        match started {
            Ok(t) => {
                self.task = Some(t);
                self.ok("Ajan bağlanıyor…");
            }
            Err(e) => self.message = Some((format!("banna-agent başlatılamadı ({})", e), true)),
        }
        self.focus = None;
    }

    fn unpair_agent(&mut self) {
        if self.task.is_some() || self.need_admin() {
            return;
        }
        let password = core::mem::take(&mut self.fields[F_ADMIN]);
        let args = ["banna-agent", "unpair"];
        let started = if process::uid() == 0 {
            Task::start("/bin/banna-agent", &args)
        } else {
            Task::start_as_root("/bin/banna-agent", &args, &password)
        };
        match started {
            Ok(t) => self.task = Some(t),
            Err(e) => self.message = Some((format!("banna-agent başlatılamadı ({})", e), true)),
        }
        self.focus = None;
    }

    fn poll_task(&mut self) -> bool {
        let Some(task) = self.task.as_mut() else {
            return false;
        };
        if !task.pump() {
            return false;
        }
        if let Some(code) = task.exit {
            let line = String::from(task.last_line());
            self.message = Some((line, code != 0));
            self.task = None;
            self.load_agent();
        }
        true
    }

    // ---- Güvenlik ----

    fn change_password(&mut self) {
        if self.fields[F_CURRENT].is_empty() || self.fields[F_NEW].is_empty() {
            self.message = Some((String::from("Mevcut ve yeni şifreyi girin."), true));
            return;
        }
        let body = json::object(&[
            ("currentPassword", &self.fields[F_CURRENT]),
            ("newPassword", &self.fields[F_NEW]),
        ]);
        match self.client.patch("/account/password", &body) {
            Ok(_) => {
                self.fields[F_CURRENT].clear();
                self.fields[F_NEW].clear();
                self.focus = None;
                self.ok("Şifre güncellendi.");
            }
            Err(e) => self.fail(&e),
        }
    }

    fn delete_account(&mut self) {
        if self.fields[F_DELETE].is_empty() {
            self.focus = Some(F_DELETE);
            return;
        }
        let body = json::object(&[("password", &self.fields[F_DELETE])]);
        self.fields[F_DELETE].clear();
        match self.client.call("DELETE", "/account", Some(&body)) {
            Ok(_) => {
                self.deleting_account = false;
                self.logout();
                self.ok("Hesabınız silindi.");
            }
            Err(e) => self.fail(&e),
        }
    }

    // ---- Ayarlar ----

    fn open_legal(&mut self, path: &str) {
        match self.client.get(path) {
            Ok(v) => {
                let sections = v
                    .get("sections")
                    .items()
                    .iter()
                    .map(|s| {
                        (
                            String::from(s.get("heading").as_str()),
                            s.get("body")
                                .items()
                                .iter()
                                .map(|p| String::from(p.as_str()))
                                .collect(),
                        )
                    })
                    .collect();
                self.modal_scroll = 0;
                self.modal = Some(Modal::Document(Doc {
                    title: String::from(v.get("title").as_str()),
                    updated: String::from(v.get("updatedAt").as_str()),
                    sections,
                }));
            }
            Err(e) => self.fail(&e),
        }
    }

    fn send_contact(&mut self) {
        let subject = String::from(self.fields[F_CONTACT_SUBJECT].trim());
        let body = String::from(self.fields[F_CONTACT_BODY].trim());
        if subject.is_empty() || body.is_empty() {
            self.message = Some((String::from("Konu ve mesaj gerekli."), true));
            return;
        }
        if self.last_contact != 0
            && time::uptime_ms().saturating_sub(self.last_contact) < CONTACT_COOLDOWN_MS
        {
            self.message = Some((
                String::from("Destek mesajları arasında bir dakika bekleyin."),
                true,
            ));
            return;
        }
        let to = format!("[{}]", json::quote(SUPPORT_ADDRESS));
        let subject = format!("[Banna Panel] {}", subject);
        self.last_contact = time::uptime_ms();
        match self.client.post_form(
            "/mail/send",
            &[
                ("toEbannaAddresses", &to),
                ("subject", &subject),
                ("body", &body),
            ],
        ) {
            Ok(_) => {
                self.fields[F_CONTACT_SUBJECT].clear();
                self.fields[F_CONTACT_BODY].clear();
                self.focus = None;
                self.ok("Mesajınız Banna ekibine iletildi.");
            }
            Err(e) => self.fail(&e),
        }
    }

    // ---- Girdi ----

    fn handle(&mut self, control: Control) {
        match control {
            Control::Nothing => {}
            Control::Field(f) => self.focus = Some(f),
            Control::Login => self.login(),
            Control::Remember => {
                self.remember = !self.remember;
                self.save_config();
            }
            Control::Page(p) => self.set_page(p),
            Control::Logout => self.logout(),
            Control::Ask(action) => {
                let text = match &action {
                    Action::DeleteMail => {
                        String::from("Bu mesajı kalıcı olarak silmek istiyor musun?")
                    }
                    Action::EmptyFolder => format!(
                        "{} klasörünü boşaltmak istiyor musun?",
                        FOLDERS[self.folder].1
                    ),
                    Action::RemoveDevice(id) => {
                        let name = self
                            .devices
                            .iter()
                            .flatten()
                            .find(|d| d.get("id").as_str() == id)
                            .map(|d| String::from(d.get("deviceName").as_str()))
                            .unwrap_or_default();
                        format!(
                            "\"{}\" cihazını kaldırmak istiyor musun? Bu cihazdaki oturum kapatılacak.",
                            name
                        )
                    }
                    Action::ApproveReset(id) => {
                        let name = self
                            .pending
                            .iter()
                            .find(|p| p.id == *id)
                            .map(|p| p.device.clone())
                            .unwrap_or_default();
                        format!(
                            "{} cihazına hesabınızın şifresini değiştirme izni verilsin mi?",
                            name
                        )
                    }
                    Action::DeleteNotifications => format!(
                        "{} bildirim kalıcı olarak silinsin mi?",
                        self.selected.len()
                    ),
                };
                self.modal = Some(Modal::Confirm(text, action));
            }
            Control::Do(action) => {
                self.modal = None;
                match action {
                    Action::DeleteMail => self.delete_mail(),
                    Action::EmptyFolder => self.empty_folder(),
                    Action::RemoveDevice(id) => self.remove_device(&id),
                    Action::ApproveReset(id) => self.respond(&id, PendingKind::Reset, true),
                    Action::DeleteNotifications => {
                        let ids = self.selected.clone();
                        self.delete_notifications(ids);
                    }
                }
            }
            Control::CloseModal => {
                self.modal = None;
                if matches!(self.focus, Some(F_TO | F_SUBJECT | F_BODY | F_ATTACH)) {
                    self.focus = None;
                }
            }
            Control::Toast => {
                self.toast = None;
                if self.signed_in && self.notify_enabled == Some(true) {
                    self.set_page(Page::Notifications);
                }
            }
            Control::OpenNotification(id) => {
                if self.selecting {
                    self.handle(Control::ToggleSelect(id));
                } else {
                    self.open_notification(&id);
                }
            }
            Control::NotifRead(id) => self.mark_read(&id, false),
            Control::NotifDelete(id) => self.delete_notifications(alloc::vec![id]),
            Control::SecurityOnly => {
                self.security_only = !self.security_only;
                self.scroll = 0;
            }
            Control::NotifReload => self.load_notifications(),
            Control::ReadAll => {
                match self.notify_call("POST", "/v1/notifications/read-all", Some("{}")) {
                    Ok(_) => {
                        for n in self.notifications.iter_mut() {
                            n.read = true;
                        }
                        self.notif_unread = 0;
                    }
                    Err(e) => self.fail(&e),
                }
            }
            Control::Selecting(on) => {
                self.selecting = on;
                self.selected.clear();
            }
            Control::ToggleSelect(id) => {
                if let Some(i) = self.selected.iter().position(|s| *s == id) {
                    self.selected.remove(i);
                } else {
                    self.selected.push(id);
                }
            }
            Control::SelectAll => {
                let visible: Vec<String> = self
                    .notifications
                    .iter()
                    .filter(|n| !self.security_only || n.security())
                    .map(|n| n.id.clone())
                    .collect();
                if visible.iter().all(|id| self.selected.contains(id)) {
                    self.selected.clear();
                } else {
                    self.selected = visible;
                }
            }
            Control::MoreNotifications => self.more_notifications(),
            Control::Folder(f) => {
                self.folder = f;
                self.load_mail_list();
            }
            Control::Favorites => {
                self.favorites_only = !self.favorites_only;
                self.load_mail_list();
            }
            Control::OpenMail(id) => self.open_mail(&id),
            Control::Flag(field) => self.toggle_flag(field),
            Control::Move(folder) => self.move_mail(folder),
            Control::Download(id, name) => self.download(&id, &name),
            Control::Compose => {
                self.modal = Some(Modal::Compose);
                self.focus = Some(F_TO);
                self.message = None;
            }
            Control::Send => self.send_mail(),
            Control::AddAttachment => self.add_attachment(),
            Control::RemoveAttachment(i) => {
                if i < self.attachments.len() {
                    self.attachments.remove(i);
                }
            }
            Control::Blocked => self.load_blocked(),
            Control::Unblock(id) => self.unblock(&id),
            Control::ReloadDevices => self.load_devices(),
            Control::OpenCrash(id) => self.open_crash(&id),
            Control::CopyCrash => {
                if let Some(c) = &self.crash_open {
                    clipboard::set(c.get("log").as_str());
                    self.ok("Günlük panoya kopyalandı.");
                }
            }
            Control::DeleteCrash(id) => self.delete_crash(&id),
            Control::StartRename(id) => {
                self.fields[F_RENAME] = self
                    .devices
                    .iter()
                    .flatten()
                    .find(|d| d.get("id").as_str() == id)
                    .map(|d| String::from(d.get("deviceName").as_str()))
                    .unwrap_or_default()
                    .into();
                self.renaming = Some(id);
                self.focus = Some(F_RENAME);
            }
            Control::SaveRename => self.save_rename(),
            Control::CancelRename => {
                self.renaming = None;
                self.focus = None;
            }
            Control::ApprovalDevice(id, enabled) => self.set_approval_device(&id, enabled),
            Control::Respond(id, kind, approve) => {
                if kind == PendingKind::Reset && approve {
                    self.handle(Control::Ask(Action::ApproveReset(id)));
                } else {
                    self.respond(&id, kind, approve);
                }
            }
            Control::ReloadAgent => self.load_agent(),
            Control::IssueToken(id) => self.issue_token(&id),
            Control::CopyToken(token) => {
                clipboard::set(&token);
                self.ok("Jeton panoya kopyalandı.");
            }
            Control::Target(id) => self.target = id,
            Control::RemoteInstall(name) => self.remote_install(&name),
            Control::PairAgent => self.pair_agent(),
            Control::UnpairAgent => self.unpair_agent(),
            Control::ChangePassword => self.change_password(),
            Control::DeleteAccount(on) => {
                self.deleting_account = on;
                self.fields[F_DELETE].clear();
                self.focus = if on { Some(F_DELETE) } else { None };
            }
            Control::ConfirmDelete => self.delete_account(),
            Control::Toasts => {
                self.toasts = !self.toasts;
                self.save_config();
            }
            Control::Legal(path) => self.open_legal(path),
            Control::Changelog => {
                self.modal_scroll = 0;
                self.modal = Some(Modal::Document(Doc {
                    title: String::from("Sürüm Notları"),
                    updated: String::new(),
                    sections: CHANGELOG
                        .iter()
                        .map(|(h, b)| {
                            (
                                String::from(*h),
                                b.iter().map(|p| String::from(*p)).collect(),
                            )
                        })
                        .collect(),
                }));
            }
            Control::SendContact => self.send_contact(),
            Control::Wizard => {
                let _ = process::spawn("/apps/setup/setup", &["setup"]);
            }
            Control::DesktopSettings => {
                let _ = process::spawn("/apps/settings/settings", &["settings"]);
            }
            Control::Reload => {
                self.load_account();
                self.ok("Veriler yenilendi.");
            }
            Control::CopyPath(path) => {
                clipboard::set(&path);
                self.ok("Yol panoya kopyalandı.");
            }
        }
    }

    /// Ağ isteği yapan denetimler için kısa durum metni (istek sürerken gösterilir).
    fn busy_text(&self, control: &Control) -> Option<&'static str> {
        Some(match control {
            Control::Login => "Giriş yapılıyor…",
            Control::Send => "Gönderiliyor…",
            Control::SendContact => "Gönderiliyor…",
            Control::Download(..) => "İndiriliyor…",
            Control::Page(Page::Dashboard | Page::Security | Page::Settings | Page::Downloads) => {
                return None;
            }
            Control::Page(_)
            | Control::Folder(_)
            | Control::Favorites
            | Control::OpenMail(_)
            | Control::NotifReload
            | Control::ReloadDevices
            | Control::ReloadAgent
            | Control::Reload
            | Control::OpenNotification(_)
            | Control::MoreNotifications
            | Control::Blocked
            | Control::Legal(_) => "Yükleniyor…",
            Control::Do(_)
            | Control::NotifRead(_)
            | Control::NotifDelete(_)
            | Control::ReadAll
            | Control::Flag(_)
            | Control::Move(_)
            | Control::Unblock(_)
            | Control::SaveRename
            | Control::ApprovalDevice(..)
            | Control::Respond(..)
            | Control::IssueToken(_)
            | Control::RemoteInstall(_)
            | Control::PairAgent
            | Control::ChangePassword
            | Control::ConfirmDelete
            | Control::Logout => "Bekleyin…",
            _ => return None,
        })
    }

    fn multiline(field: usize) -> bool {
        matches!(field, F_BODY | F_CONTACT_BODY)
    }

    fn secret(field: usize) -> bool {
        matches!(field, F_PASSWORD | F_CURRENT | F_NEW | F_DELETE | F_ADMIN)
    }

    /// Klavye: odaktaki alana yazar; Enter alanın işini yapar. Gerekirse bir denetim döndürür.
    fn key(&mut self, key: u32) -> Option<Control> {
        if key == 27 {
            if self.modal.is_some() {
                return Some(Control::CloseModal);
            }
            self.focus = None;
            self.renaming = None;
            self.selecting = false;
            self.selected.clear();
            self.deleting_account = false;
            return None;
        }
        match key {
            KEY_UP => self.scroll_by(-48 * self.u),
            KEY_DOWN => self.scroll_by(48 * self.u),
            KEY_PAGE_UP => self.scroll_by(-400 * self.u),
            KEY_PAGE_DOWN => self.scroll_by(400 * self.u),
            _ => {}
        }
        if key == 1 && self.selecting {
            return Some(Control::SelectAll);
        }
        let f = self.focus?;
        let multi = Self::multiline(f);
        if key == KEY_PASTE {
            let text = clipboard::get();
            let text = if multi {
                text
            } else {
                text.replace(['\n', '\r'], " ")
            };
            let max = if multi { 8000 } else { 256 };
            for c in text.chars() {
                if self.fields[f].chars().count() >= max {
                    break;
                }
                if c == '\n' || !c.is_control() {
                    self.fields[f].push(c);
                }
            }
            return None;
        }
        if key == 9 {
            self.focus = Some(match f {
                F_ADDRESS => F_PASSWORD,
                F_PASSWORD => F_ADDRESS,
                F_TO => F_SUBJECT,
                F_SUBJECT => F_BODY,
                F_BODY => F_ATTACH,
                F_ATTACH => F_TO,
                F_CURRENT => F_NEW,
                F_NEW => F_CURRENT,
                F_CONTACT_SUBJECT => F_CONTACT_BODY,
                F_CONTACT_BODY => F_CONTACT_SUBJECT,
                other => other,
            });
            return None;
        }
        if key == 13 {
            if multi {
                self.fields[f].push('\n');
                return None;
            }
            return match f {
                F_ADDRESS => {
                    self.focus = Some(F_PASSWORD);
                    None
                }
                F_PASSWORD => Some(Control::Login),
                F_SEARCH => Some(Control::Folder(self.folder)),
                F_TO => {
                    self.focus = Some(F_SUBJECT);
                    None
                }
                F_SUBJECT => {
                    self.focus = Some(F_BODY);
                    None
                }
                F_ATTACH => Some(Control::AddAttachment),
                F_RENAME => Some(Control::SaveRename),
                F_CURRENT => {
                    self.focus = Some(F_NEW);
                    None
                }
                F_NEW => Some(Control::ChangePassword),
                F_DELETE => Some(Control::ConfirmDelete),
                F_ADMIN => Some(if self.agent_paired {
                    Control::UnpairAgent
                } else {
                    Control::PairAgent
                }),
                F_CONTACT_SUBJECT => {
                    self.focus = Some(F_CONTACT_BODY);
                    None
                }
                _ => None,
            };
        }
        let max = if multi {
            8000
        } else if f == F_ATTACH {
            512
        } else {
            128
        };
        self.fields[f].key(key, max);
        None
    }

    fn scroll_by(&mut self, delta: i32) {
        if self.modal.is_some() {
            self.modal_scroll = (self.modal_scroll + delta).max(0);
        } else if self.page == Page::Mail && self.signed_in {
            let (_, list_right) = mail_columns(self);
            if self.mouse.0 < list_right {
                self.list_scroll = (self.list_scroll + delta).clamp(0, self.list_max);
            } else {
                self.detail_scroll = (self.detail_scroll + delta).clamp(0, self.detail_max);
            }
        } else {
            self.scroll = (self.scroll + delta).clamp(0, self.max_scroll);
        }
    }
}

// ---- Çizim yardımcıları ----

#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Primary,
    Secondary,
    Danger,
    Ghost,
}

fn button_width(ui: &Ui<Control>, text: &str) -> i32 {
    ui.f.small.measure(text) + 28 * ui.u
}

/// Küçük düğme; dikdörtgenini döndürür.
fn button(ui: &mut Ui<Control>, x: i32, y: i32, text: &str, kind: Kind, control: Control) -> Rect {
    let u = ui.u;
    let r = Rect::new(x, y, button_width(ui, text), 32 * u);
    let hovered = ui.hovered(r);
    let (bg, fg) = match (kind, hovered) {
        (Kind::Primary, false) => (ui.t.accent, WHITE),
        (Kind::Primary, true) => (
            banna::gfx::blend(ui.t.accent, with_alpha(0xffffff, 40)) | 0xff00_0000,
            WHITE,
        ),
        (Kind::Secondary, false) => (ui.t.control, ui.t.text),
        (Kind::Secondary, true) => (ui.t.hover, ui.t.text),
        (Kind::Danger, false) => (with_alpha(DANGER, 40), rgb(0xff, 0x7b, 0x72)),
        (Kind::Danger, true) => (DANGER, WHITE),
        (Kind::Ghost, false) => (0, ui.t.muted),
        (Kind::Ghost, true) => (ui.t.hover, ui.t.text),
    };
    if bg != 0 {
        icons::rounded_rect(ui.c, r, 9 * u, bg);
    }
    let f = ui.f.small;
    let tw = f.measure(text);
    f.draw(
        ui.c,
        r.x + r.w / 2 - tw / 2,
        r.y + (r.h - f.ascent - f.descent) / 2,
        text,
        fg,
    );
    ui.hit(r, control);
    r
}

/// Sağa yaslı düğme dizisi; en soldaki düğmenin x'ini döndürür.
fn buttons_right(ui: &mut Ui<Control>, right: i32, y: i32, items: &[(&str, Kind, Control)]) -> i32 {
    let mut x = right;
    for (text, kind, control) in items.iter().rev() {
        x -= button_width(ui, text);
        button(ui, x, y, text, *kind, control.clone());
        x -= 8 * ui.u;
    }
    x
}

/// Soldan sağa dizilen, sığmayınca alt satıra geçen düğmeler; sonraki y'yi döndürür.
fn button_flow(
    ui: &mut Ui<Control>,
    x0: i32,
    y: i32,
    width: i32,
    items: &[(&str, Kind, Control)],
) -> i32 {
    let u = ui.u;
    let (mut x, mut y) = (x0, y);
    for (text, kind, control) in items {
        let w = button_width(ui, text);
        if x > x0 && x + w > x0 + width {
            x = x0;
            y += 40 * u;
        }
        button(ui, x, y, text, *kind, control.clone());
        x += w + 8 * u;
    }
    y + 40 * u
}

/// Hap biçimli etiket; genişliğini döndürür.
fn pill(ui: &mut Ui<Control>, x: i32, y: i32, text: &str, color: Argb) -> i32 {
    let u = ui.u;
    let f = ui.f.small;
    let w = f.measure(text) + 18 * u;
    let h = f.ascent + f.descent + 8 * u;
    icons::rounded_rect(ui.c, Rect::new(x, y, w, h), h / 2, with_alpha(color, 46));
    f.draw(ui.c, x + 9 * u, y + 4 * u, text, color);
    w
}

fn avatar(ui: &mut Ui<Control>, cx: i32, cy: i32, r: i32, address: &str, font: &Font) {
    icons::disc(ui.c, cx, cy, r, ui.t.accent);
    let initial: String = address
        .chars()
        .take(1)
        .flat_map(|c| c.to_uppercase())
        .collect();
    let w = font.measure(&initial);
    font.draw(
        ui.c,
        cx - w / 2,
        cy - (font.ascent + font.descent) / 2,
        &initial,
        WHITE,
    );
}

fn tile(ui: &mut Ui<Control>, r: Rect, label: &str, value: &str) {
    let u = ui.u;
    icons::rounded_rect(ui.c, r, 12 * u, ui.t.card);
    ui.f.small
        .draw(ui.c, r.x + 16 * u, r.y + 14 * u, label, ui.t.muted);
    ui.f.title.draw_clipped(
        ui.c,
        r.x + 16 * u,
        r.y + 38 * u,
        value,
        r.w - 32 * u,
        ui.t.text,
    );
}

/// Tile ızgarası: `per_row` sütun; ui.y'yi ilerletir.
fn tiles(ui: &mut Ui<Control>, items: &[(&str, String)], per_row: usize, height: i32) {
    let u = ui.u;
    let gap = 12 * u;
    let per_row = per_row.max(1) as i32;
    let w = (ui.w - (per_row - 1) * gap) / per_row;
    for (i, (label, value)) in items.iter().enumerate() {
        let col = i as i32 % per_row;
        let row = i as i32 / per_row;
        let r = Rect::new(
            ui.x + col * (w + gap),
            ui.y + row * (height + gap),
            w,
            height,
        );
        tile(ui, r, label, value);
    }
    let rows = (items.len() as i32 + per_row - 1) / per_row;
    ui.y += rows * (height + gap) + 8 * u;
}

fn field(ui: &mut Ui<Control>, s: &State, rect: Rect, placeholder: &str, f: usize) {
    ui.input_field(
        rect,
        placeholder,
        &s.fields[f],
        State::secret(f),
        s.focus == Some(f),
        Control::Field(f),
    );
}

/// Çok satırlı metin kutusu.
fn text_area(ui: &mut Ui<Control>, s: &State, rect: Rect, placeholder: &str, f: usize) {
    let u = ui.u;
    let focused = s.focus == Some(f);
    icons::rounded_rect(
        ui.c,
        rect,
        10 * u,
        if focused { ui.t.accent } else { ui.t.control },
    );
    icons::rounded_rect(
        ui.c,
        rect.inset(if focused { 2 * u } else { u }),
        9 * u,
        ui.t.card,
    );
    let font = ui.f.body;
    let lh = font.line_height();
    let lines = wrap(font, &s.fields[f], rect.w - 28 * u);
    let visible = ((rect.h - 20 * u) / lh).max(1) as usize;
    let skip = lines.len().saturating_sub(visible);
    let mut y = rect.y + 10 * u;
    let mut end = (rect.x + 14 * u, y);
    for line in lines.iter().skip(skip) {
        let x = font.draw_clipped(ui.c, rect.x + 14 * u, y, line, rect.w - 28 * u, ui.t.text);
        end = (x, y);
        y += lh;
    }
    if s.fields[f].is_empty() && !focused {
        font.draw(
            ui.c,
            rect.x + 14 * u,
            rect.y + 10 * u,
            placeholder,
            ui.t.muted,
        );
    }
    if focused {
        ui.c.fill(
            Rect::new(end.0 + u, end.1, 2 * u, font.ascent + font.descent),
            ui.t.accent,
        );
    }
    ui.hit(rect, Control::Field(f));
}

fn label(ui: &mut Ui<Control>, x: i32, y: i32, text: &str) {
    ui.f.small.draw(ui.c, x, y, text, ui.t.muted);
}

/// Banna OS hata raporları (Banna Hata Raporlama Sistemi) ve açık raporun günlüğü.
fn draw_crashes(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    ui.section("Hata raporları");
    ui.card(s.crashes.len());
    for c in &s.crashes {
        let id = String::from(c.get("id").as_str());
        let kind = match c.get("kind").as_str() {
            "watchdog" => "Takılma",
            "panic" => "Çekirdek paniği",
            "exception" => "Çekirdek istisnası",
            other => other,
        };
        let desc = format!(
            "{} · {} · Banna OS {}",
            c.get("deviceName").as_str(),
            kind,
            c.get("version").as_str()
        );
        let open = s
            .crash_open
            .as_ref()
            .is_some_and(|o| o.get("id").as_str() == id);
        ui.buttons(
            c.get("title").as_str(),
            Some(&desc),
            &[
                (
                    if open { "Gizle" } else { "Günlük" },
                    Control::OpenCrash(id.clone()),
                    false,
                ),
                ("Sil", Control::DeleteCrash(id), false),
            ],
        );
    }
    let Some(open) = &s.crash_open else {
        return;
    };
    // Günlüğün son satırları; tamamı "Kopyala" ile alınır.
    let log = open.get("log").as_str();
    let lines: Vec<&str> = log.lines().collect();
    let shown = &lines[lines.len().saturating_sub(60)..];
    let lh = ui.f.small.line_height();
    let rect = Rect::new(ui.x, ui.y + 8 * u, ui.w, shown.len() as i32 * lh + 60 * u);
    icons::rounded_rect(ui.c, rect, 12 * u, ui.t.card);
    ui.f.title.draw(
        ui.c,
        rect.x + 16 * u,
        rect.y + 12 * u,
        "Ayrıntılı günlük",
        ui.t.text,
    );
    buttons_right(
        ui,
        rect.right() - 12 * u,
        rect.y + 8 * u,
        &[("Kopyala", Kind::Ghost, Control::CopyCrash)],
    );
    let mut y = rect.y + 44 * u;
    for line in shown {
        ui.f.small
            .draw_clipped(ui.c, rect.x + 16 * u, y, line, rect.w - 32 * u, ui.t.muted);
        y += lh;
    }
    ui.y = rect.bottom() + 8 * u;
}

fn empty(ui: &mut Ui<Control>, text: &str) {
    ui.note(text);
}

/// Sayfa başlığı ve sağda düğmeler.
fn toolbar(ui: &mut Ui<Control>, title: &str, items: &[(&str, Kind, Control)]) {
    let y = ui.y;
    let right = ui.x + ui.w;
    let heading = ui.f.heading;
    heading.draw(ui.c, ui.x, y, title, ui.t.text);
    buttons_right(ui, right, y + 2 * ui.u, items);
    ui.y += heading.line_height() + 14 * ui.u;
}

// ---- Sayfalar ----

fn draw_login(s: &State, ui: &mut Ui<Control>, w: i32, h: i32) {
    let u = ui.u;
    let card = Rect::new(w / 2 - 220 * u, h / 2 - 260 * u, 440 * u, 520 * u);
    ui.c.shadow(card, 18 * u, 18 * u, 70);
    icons::rounded_rect(ui.c, card, 18 * u, ui.t.card);
    let logo = Rect::new(w / 2 - 32 * u, card.y + 28 * u, 64 * u, 64 * u);
    icons::logo(ui.c, logo, ui.f.heading, ui.t.accent, WHITE);
    let centered = |ui: &mut Ui<Control>, font: &Font, y: i32, text: &str, color: Argb| {
        let tw = font.measure(text);
        font.draw(ui.c, w / 2 - tw / 2, y, text, color);
    };
    let (heading, small, body) = (ui.f.heading, ui.f.small, ui.f.body);
    centered(ui, heading, card.y + 104 * u, "Banna Panel", ui.t.text);
    if s.booting {
        centered(
            ui,
            small,
            card.y + 150 * u,
            "Banna Panel yükleniyor…",
            ui.t.muted,
        );
        return;
    }
    centered(
        ui,
        small,
        card.y + 144 * u,
        "Banna hesabınızla giriş yapın",
        ui.t.muted,
    );
    let (x, y, fw) = (card.x + 32 * u, card.y + 184 * u, card.w - 64 * u);
    if let Some((_, code)) = &s.pending_login {
        ui.f.title.draw(ui.c, x, y, "Onay bekleniyor", ui.t.text);
        for (i, line) in wrap(
            body,
            "Bu cihaz hesabınızda yeni. Onay cihazınızda (Banna uygulaması ya da Banna Panel) bu girişi onaylayın.",
            fw,
        )
        .iter()
        .enumerate()
        {
            body.draw(ui.c, x, y + 34 * u + i as i32 * body.line_height(), line, ui.t.muted);
        }
        let box_ = Rect::new(x, y + 130 * u, fw, 64 * u);
        icons::rounded_rect(ui.c, box_, 12 * u, ui.t.control);
        small.draw(
            ui.c,
            box_.x + 16 * u,
            box_.y + 10 * u,
            "Yedek kod",
            ui.t.muted,
        );
        heading.draw(ui.c, box_.x + 16 * u, box_.y + 26 * u, code, ui.t.accent);
    } else {
        label(ui, x, y, "E-Banna adresi");
        field(
            ui,
            s,
            Rect::new(x, y + 20 * u, fw, 40 * u),
            "kullanici@banna.com",
            F_ADDRESS,
        );
        label(ui, x, y + 72 * u, "Şifre");
        field(
            ui,
            s,
            Rect::new(x, y + 92 * u, fw, 40 * u),
            "Şifre",
            F_PASSWORD,
        );
        // Beni hatırla
        let cb = Rect::new(x, y + 146 * u, 20 * u, 20 * u);
        icons::rounded_rect(
            ui.c,
            cb,
            5 * u,
            if s.remember {
                ui.t.accent
            } else {
                ui.t.control
            },
        );
        if s.remember {
            icons::check(ui.c, cb.inset(3 * u), WHITE);
        }
        body.draw(ui.c, cb.right() + 10 * u, cb.y, "Beni hatırla", ui.t.text);
        ui.hit(
            Rect::new(x, cb.y - 4 * u, 160 * u, 28 * u),
            Control::Remember,
        );
        ui.primary_button(
            Rect::new(x, y + 182 * u, fw, 42 * u),
            "Giriş Yap",
            Control::Login,
        );
    }
    if let Some(status) = &s.status {
        centered(ui, small, card.bottom() - 70 * u, status, ui.t.muted);
    } else if let Some((text, error)) = &s.message {
        let color = if *error { DANGER } else { ui.t.muted };
        for (i, line) in wrap(small, text, fw).iter().take(2).enumerate() {
            centered(
                ui,
                small,
                card.bottom() - 74 * u + i as i32 * small.line_height(),
                line,
                color,
            );
        }
    }
    centered(
        ui,
        small,
        card.bottom() - 30 * u,
        "Hesabın yok mu? ecosys.trybanna.com",
        ui.t.muted,
    );
}

fn draw_sidebar(s: &State, ui: &mut Ui<Control>, sidebar_w: i32, h: i32) {
    let u = ui.u;
    let t = ui.t;
    ui.c.fill(Rect::new(0, 0, sidebar_w, h), t.sidebar);
    ui.c.fill(
        Rect::new(sidebar_w - 1, 0, 1, h),
        with_alpha(if t.dark { 0xffffff } else { 0 }, 14),
    );
    let logo = Rect::new(18 * u, 22 * u, 38 * u, 38 * u);
    icons::logo(ui.c, logo, ui.f.title, t.accent, WHITE);
    ui.f.title
        .draw(ui.c, logo.right() + 12 * u, 22 * u, "Banna Panel", t.text);
    ui.f.small
        .draw(ui.c, logo.right() + 12 * u, 44 * u, "Banna OS", t.muted);
    let mut y = 84 * u;
    for (page, label, hint, icon) in NAV.iter() {
        let item = Rect::new(10 * u, y, sidebar_w - 20 * u, 46 * u);
        if *page == s.page {
            icons::rounded_rect(
                ui.c,
                item,
                10 * u,
                with_alpha(t.accent, if t.dark { 70 } else { 60 }),
            );
        } else if ui.hovered(item) {
            icons::rounded_rect(ui.c, item, 10 * u, t.hover);
        }
        icons::draw(
            ui.c,
            Rect::new(item.x + 8 * u, item.y + 9 * u, 28 * u, 28 * u),
            *icon,
        );
        ui.f.body
            .draw(ui.c, item.x + 46 * u, item.y + 5 * u, label, t.text);
        ui.f.small
            .draw(ui.c, item.x + 46 * u, item.y + 25 * u, hint, t.muted);
        let count = match page {
            Page::Notifications if s.notify_enabled == Some(true) => s.notif_unread,
            Page::Mail => s.mail_unread,
            Page::Devices => s.pending.len() as i64,
            _ => 0,
        };
        if count > 0 {
            let text = if count > 99 {
                String::from("99+")
            } else {
                format!("{}", count)
            };
            let f = ui.f.small;
            let bw = (f.measure(&text) + 14 * u).max(22 * u);
            let b = Rect::new(item.right() - bw - 8 * u, item.y + 13 * u, bw, 20 * u);
            let color = if *page == Page::Notifications || *page == Page::Devices {
                DANGER
            } else {
                t.accent
            };
            icons::rounded_rect(ui.c, b, 10 * u, color);
            f.draw(
                ui.c,
                b.x + (bw - f.measure(&text)) / 2,
                b.y + (20 * u - f.ascent - f.descent) / 2,
                &text,
                WHITE,
            );
        }
        ui.hit(item, Control::Page(*page));
        y += 50 * u;
    }

    // Alt kısım: indirilenler, kullanıcı, çıkış.
    let dl = Rect::new(10 * u, h - 156 * u, sidebar_w - 20 * u, 36 * u);
    if s.page == Page::Downloads {
        icons::rounded_rect(
            ui.c,
            dl,
            10 * u,
            with_alpha(t.accent, if t.dark { 70 } else { 60 }),
        );
    } else if ui.hovered(dl) {
        icons::rounded_rect(ui.c, dl, 10 * u, t.hover);
    }
    let text = if s.downloads.is_empty() {
        String::from("İndirilenler")
    } else {
        format!("İndirilenler ({})", s.downloads.len())
    };
    ui.f.body.draw(
        ui.c,
        dl.x + 12 * u,
        dl.y + (dl.h - ui.f.body.ascent - ui.f.body.descent) / 2,
        &text,
        t.text,
    );
    ui.hit(dl, Control::Page(Page::Downloads));
    let address = String::from(s.address());
    let title = ui.f.title;
    avatar(ui, 34 * u, h - 92 * u, 18 * u, &address, title);
    ui.f.small.draw_clipped(
        ui.c,
        60 * u,
        h - 106 * u,
        &address,
        sidebar_w - 72 * u,
        t.text,
    );
    ui.f.small.draw_clipped(
        ui.c,
        60 * u,
        h - 88 * u,
        s.me.get("device").get("deviceName").as_str(),
        sidebar_w - 72 * u,
        t.muted,
    );
    let out = Rect::new(10 * u, h - 56 * u, sidebar_w - 20 * u, 36 * u);
    if ui.hovered(out) {
        icons::rounded_rect(ui.c, out, 10 * u, t.hover);
    }
    ui.f.body.draw(
        ui.c,
        out.x + 12 * u,
        out.y + (out.h - ui.f.body.ascent - ui.f.body.descent) / 2,
        "Çıkış Yap",
        t.muted,
    );
    ui.hit(out, Control::Logout);
}

fn draw_dashboard(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    let current = s.me.get("current");
    let address = current.get("ebannaAddress").as_str();
    let heading = ui.f.heading;
    avatar(ui, ui.x + 32 * u, ui.y + 32 * u, 32 * u, address, heading);
    ui.f.small
        .draw(ui.c, ui.x + 80 * u, ui.y + 8 * u, "Hoş geldin", ui.t.muted);
    ui.f.heading.draw_clipped(
        ui.c,
        ui.x + 80 * u,
        ui.y + 26 * u,
        address,
        ui.w - 80 * u,
        ui.t.text,
    );
    ui.y += 84 * u;

    let mut stats = alloc::vec![("Yeni mail", format!("{}", s.mail_unread))];
    if s.notify_enabled == Some(true) {
        stats.push(("Okunmamış bildirim", format!("{}", s.notif_unread)));
    }
    if s.approval_device() {
        stats.push(("Bekleyen onay", format!("{}", s.pending.len())));
    }
    let n = stats.len();
    tiles(ui, &stats, n, 84 * u);

    ui.section("Hesap Bilgileri");
    let mut info = alloc::vec![
        ("E-Banna Adresi", String::from(address)),
        ("E-Kimlik", String::from(current.get("eKimlik").as_str())),
        (
            "Hesap Türü",
            String::from(account_type(current.get("accountType").as_str())),
        ),
        ("Dil", current.get("locale").as_str().to_uppercase()),
        (
            "Kayıt Tarihi",
            date(current.get("createdAt").as_str(), s.tz_minutes)
        ),
    ];
    let student = s.me.get("studentProfile");
    if !student.is_null() {
        info.push(("Okul", String::from(student.get("schoolName").as_str())));
        info.push(("Sınıf", String::from(student.get("gradeLevel").as_str())));
    }
    let per_row = if ui.w > 700 * u { 3 } else { 2 };
    tiles(ui, &info, per_row, 72 * u);

    ui.section("Bu Cihaz");
    ui.card(2);
    let device = s.me.get("device");
    ui.info("Cihaz adı", device.get("deviceName").as_str());
    ui.info(
        "Onay cihazı",
        if device.get("isApprovalDevice").as_bool() {
            "Evet"
        } else {
            "Hayır"
        },
    );

    ui.section("Bağlı Kimlikler");
    let linked = s.me.get("linkedIdentities").items();
    if linked.is_empty() {
        empty(ui, "Başka bağlı bir E-Kimlik yok.");
    } else {
        ui.card(linked.len());
        for id in linked {
            ui.info(
                id.get("ebannaAddress").as_str(),
                account_type(id.get("accountType").as_str()),
            );
        }
    }
}

fn draw_notifications(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    if s.notify_enabled == Some(false) {
        ui.heading("Bildirimler");
        ui.card(1);
        ui.info("Banna Notify", "Bildirim servisi şu anda sunucuda kapalı.");
        return;
    }
    let visible: Vec<&Notification> = s
        .notifications
        .iter()
        .filter(|n| !s.security_only || n.security())
        .collect();
    ui.f.small
        .draw(ui.c, ui.x, ui.y, "Banna Notify", ui.t.muted);
    ui.y += ui.f.small.line_height();
    if s.selecting {
        let all = !visible.is_empty() && visible.iter().all(|n| s.selected.contains(&n.id));
        let label = if s.selected.is_empty() {
            String::from("Tümünü seç")
        } else {
            format!("{} seçildi", s.selected.len())
        };
        toolbar(
            ui,
            "Bildirimler",
            &[
                (
                    if all { "Seçimi kaldır" } else { &label },
                    Kind::Secondary,
                    Control::SelectAll,
                ),
                ("Bitti", Kind::Secondary, Control::Selecting(false)),
            ],
        );
    } else {
        let mut items = alloc::vec![
            (
                "Yalnızca güvenlik",
                if s.security_only {
                    Kind::Primary
                } else {
                    Kind::Secondary
                },
                Control::SecurityOnly,
            ),
            ("Yenile", Kind::Secondary, Control::NotifReload),
        ];
        if s.notif_unread > 0 {
            items.push(("Tümünü okundu yap", Kind::Secondary, Control::ReadAll));
        }
        if !visible.is_empty() {
            items.push(("Seç", Kind::Secondary, Control::Selecting(true)));
        }
        toolbar(ui, "Bildirimler", &items);
    }
    let x = ui.x;
    let w = pill(
        ui,
        x,
        ui.y,
        &format!("{} okunmamış", s.notif_unread),
        ui.t.accent,
    );
    let muted = ui.t.muted;
    pill(ui, x + w + 8 * u, ui.y, "Otomatik yenileme: 45 sn", muted);
    ui.y += 40 * u;

    if visible.is_empty() {
        empty(
            ui,
            if s.notify_enabled.is_none() {
                "Yükleniyor…"
            } else {
                "Henüz bildiriminiz yok."
            },
        );
    }
    let row_h = 78 * u;
    for n in visible {
        let r = Rect::new(ui.x, ui.y, ui.w, row_h - 8 * u);
        ui.y += row_h;
        if r.bottom() < ui.clip.y || r.y > ui.clip.bottom() {
            continue;
        }
        let selected = s.selected.contains(&n.id);
        let bg = if selected {
            with_alpha(ui.t.accent, 60)
        } else if ui.hovered(r) {
            ui.t.hover
        } else {
            ui.t.card
        };
        icons::rounded_rect(ui.c, r, 12 * u, bg);
        ui.hit(r, Control::OpenNotification(n.id.clone()));
        let mut x = r.x + 16 * u;
        if s.selecting {
            let cb = Rect::new(x, r.y + r.h / 2 - 10 * u, 20 * u, 20 * u);
            icons::rounded_rect(
                ui.c,
                cb,
                5 * u,
                if selected { ui.t.accent } else { ui.t.control },
            );
            if selected {
                icons::check(ui.c, cb.inset(3 * u), WHITE);
            }
            x += 32 * u;
        }
        let color = if n.security() {
            DANGER
        } else if n.app == "banna_mail" {
            ui.t.accent
        } else {
            SUCCESS
        };
        icons::disc(
            ui.c,
            x + 18 * u,
            r.y + r.h / 2,
            18 * u,
            with_alpha(color, 60),
        );
        let glyph = if n.app == "banna_mail" {
            "@"
        } else if matches!(
            n.resource_type.as_str(),
            "login_approval" | "password_reset"
        ) {
            "!"
        } else if n.security() {
            "#"
        } else {
            "B"
        };
        let gw = ui.f.title.measure(glyph);
        ui.f.title.draw(
            ui.c,
            x + 18 * u - gw / 2,
            r.y + r.h / 2 - (ui.f.title.ascent + ui.f.title.descent) / 2,
            glyph,
            color,
        );
        if !n.read {
            icons::disc(ui.c, x + 34 * u, r.y + r.h / 2 - 14 * u, 5 * u, ui.t.accent);
        }
        x += 50 * u;
        let actions_w = if s.selecting { 0 } else { 150 * u };
        let tw = r.right() - x - actions_w - 12 * u;
        let font = if n.read { ui.f.body } else { ui.f.title };
        font.draw_clipped(ui.c, x, r.y + 8 * u, &n.title, tw, ui.t.text);
        ui.f.small
            .draw_clipped(ui.c, x, r.y + 30 * u, &n.body, tw, ui.t.muted);
        let meta = format!(
            "{}{}",
            date(&n.created, s.tz_minutes),
            if n.security() { " · Güvenlik" } else { "" }
        );
        ui.f.small.draw_clipped(
            ui.c,
            x,
            r.y + 48 * u,
            &meta,
            tw,
            with_alpha(ui.t.muted, 200),
        );
        if !s.selecting {
            let mut items = Vec::new();
            if !n.read {
                items.push(("Okundu", Kind::Ghost, Control::NotifRead(n.id.clone())));
            }
            items.push(("Sil", Kind::Danger, Control::NotifDelete(n.id.clone())));
            buttons_right(ui, r.right() - 12 * u, r.y + r.h / 2 - 16 * u, &items);
        }
    }
    if s.notif_cursor.is_some() && !s.selecting {
        let x = ui.x;
        let y = ui.y;
        button(
            ui,
            x,
            y,
            "Daha fazla",
            Kind::Secondary,
            Control::MoreNotifications,
        );
        ui.y += 48 * u;
    }
    if s.selecting {
        ui.y += 60 * u;
    }
}

/// Seçim kipinde alttaki sabit çubuk.
fn draw_selection_bar(s: &State, ui: &mut Ui<Control>, area: Rect) {
    let u = ui.u;
    let bar = Rect::new(area.x, area.bottom() - 64 * u, area.w, 52 * u);
    ui.c.shadow(bar, 14 * u, 12 * u, 60);
    icons::rounded_rect(ui.c, bar, 14 * u, ui.t.card);
    let text = if s.selected.is_empty() {
        String::from("Silmek istediğiniz bildirimleri seçin")
    } else {
        format!("{} bildirim seçildi", s.selected.len())
    };
    ui.f.body.draw(
        ui.c,
        bar.x + 18 * u,
        bar.y + (bar.h - ui.f.body.ascent - ui.f.body.descent) / 2,
        &text,
        ui.t.text,
    );
    if !s.selected.is_empty() {
        buttons_right(
            ui,
            bar.right() - 12 * u,
            bar.y + 10 * u,
            &[(
                "Seçilenleri sil",
                Kind::Danger,
                Control::Ask(Action::DeleteNotifications),
            )],
        );
    }
}

/// Banna Mail sütunları: (liste x, liste sağ kenarı).
fn mail_columns(s: &State) -> (i32, i32) {
    let u = s.u;
    let left = 240 * u + 28 * u;
    (left, left + 320 * u)
}

fn draw_mail(s: &State, ui: &mut Ui<Control>, h: i32) -> (i32, i32) {
    let u = ui.u;
    let t = ui.t;
    let title = if s.mail_unread > 0 {
        format!("Banna Mail · {} okunmamış", s.mail_unread)
    } else {
        String::from("Banna Mail")
    };
    ui.y = 24 * u;
    toolbar(
        ui,
        &title,
        &[
            ("Engellenenler", Kind::Secondary, Control::Blocked),
            ("Yeni Mail", Kind::Primary, Control::Compose),
        ],
    );
    let top = ui.y;
    let (lx, lright) = mail_columns(s);
    let lw = lright - lx;
    let bottom = h - 56 * u;

    // Sol sütun: arama, klasörler, liste.
    let col = Rect::new(lx, top, lw, bottom - top);
    icons::rounded_rect(ui.c, col, 14 * u, t.card);
    let ix = lx + 12 * u;
    let iw = lw - 24 * u;
    let mut y = top + 12 * u;
    field(
        ui,
        s,
        Rect::new(ix, y, iw, 36 * u),
        "Ara… (Enter)",
        F_SEARCH,
    );
    y += 46 * u;
    let tab_w = (iw - 6 * u) / 2;
    for (i, (_, name)) in FOLDERS.iter().enumerate() {
        let r = Rect::new(
            ix + (i as i32 % 2) * (tab_w + 6 * u),
            y + (i as i32 / 2) * 36 * u,
            tab_w,
            32 * u,
        );
        let selected = i == s.folder;
        let bg = if selected {
            with_alpha(t.accent, if t.dark { 80 } else { 60 })
        } else if ui.hovered(r) {
            t.hover
        } else {
            t.control
        };
        icons::rounded_rect(ui.c, r, 9 * u, bg);
        let text = if i == 0 && s.mail_unread > 0 {
            format!("{} ({})", name, s.mail_unread)
        } else {
            String::from(*name)
        };
        let f = ui.f.small;
        let tw = f.measure(&text);
        f.draw(
            ui.c,
            r.x + (r.w - tw) / 2,
            r.y + (r.h - f.ascent - f.descent) / 2,
            &text,
            t.text,
        );
        ui.hit(r, Control::Folder(i));
    }
    y += 78 * u;
    if s.folder != SENT {
        let cb = Rect::new(ix, y, 18 * u, 18 * u);
        icons::rounded_rect(
            ui.c,
            cb,
            5 * u,
            if s.favorites_only {
                t.accent
            } else {
                t.control
            },
        );
        if s.favorites_only {
            icons::check(ui.c, cb.inset(3 * u), WHITE);
        }
        ui.f.small
            .draw(ui.c, cb.right() + 8 * u, y + u, "Sadece favoriler", t.muted);
        ui.hit(Rect::new(ix, y - 4 * u, iw, 26 * u), Control::Favorites);
        y += 30 * u;
    }
    let has_items = s.mail_list.as_ref().is_some_and(|l| !l.is_empty());
    let list_bottom = if s.folder != SENT && has_items {
        col.bottom() - 52 * u
    } else {
        col.bottom() - 8 * u
    };
    let list_rect = Rect::new(lx, y, lw, list_bottom - y);
    let saved = ui.c.clip;
    ui.clip = list_rect;
    ui.c.clip = list_rect;
    let item_h = 62 * u;
    let mut iy = y - s.list_scroll;
    match &s.mail_list {
        None => {
            ui.f.small
                .draw(ui.c, ix, iy + 8 * u, "Yükleniyor…", t.muted);
        }
        Some(list) if list.is_empty() => {
            ui.f.small.draw(ui.c, ix, iy + 8 * u, "Mesaj yok.", t.muted);
        }
        Some(list) => {
            let current = s
                .mail_detail
                .as_ref()
                .map(|d| d.get("id").as_str())
                .unwrap_or("");
            for m in list {
                let r = Rect::new(ix - 4 * u, iy, iw + 8 * u, item_h - 4 * u);
                iy += item_h;
                if r.bottom() < list_rect.y || r.y > list_rect.bottom() {
                    continue;
                }
                let id = m.get("id").as_str();
                let unread = s.folder != SENT && m.get("readAt").is_null();
                let bg = if id == current {
                    with_alpha(t.accent, if t.dark { 70 } else { 50 })
                } else if ui.hovered(r) {
                    t.hover
                } else {
                    0
                };
                if bg != 0 {
                    icons::rounded_rect(ui.c, r, 10 * u, bg);
                }
                if unread {
                    icons::disc(ui.c, r.x + 10 * u, r.y + 17 * u, 4 * u, t.accent);
                }
                let who = if s.folder == SENT {
                    m.get("to").as_str()
                } else {
                    m.get("from").as_str()
                };
                let d = date(m.get("sentAt").as_str(), s.tz_minutes);
                let dw = ui.f.small.measure(&d);
                let font = if unread { ui.f.title } else { ui.f.body };
                font.draw_clipped(
                    ui.c,
                    r.x + 20 * u,
                    r.y + 6 * u,
                    who,
                    r.w - dw - 36 * u,
                    t.text,
                );
                ui.f.small
                    .draw(ui.c, r.right() - dw - 8 * u, r.y + 9 * u, &d, t.muted);
                let subject = if m.get("isFavorite").as_bool() {
                    format!("[Favori] {}", m.get("subject").as_str())
                } else {
                    String::from(m.get("subject").as_str())
                };
                ui.f.small.draw_clipped(
                    ui.c,
                    r.x + 20 * u,
                    r.y + 32 * u,
                    &subject,
                    r.w - 28 * u,
                    t.muted,
                );
                ui.hit(r, Control::OpenMail(String::from(id)));
            }
        }
    }
    let list_max = (iy + s.list_scroll - list_rect.bottom()).max(0);
    ui.clip = saved;
    ui.c.clip = saved;
    if s.folder != SENT && has_items {
        let label = "Klasörü Boşalt";
        let bw = button_width(ui, label);
        button(
            ui,
            lx + (lw - bw) / 2,
            col.bottom() - 44 * u,
            label,
            Kind::Ghost,
            Control::Ask(Action::EmptyFolder),
        );
    }

    // Sağ sütun: ileti.
    let dx = lright + 16 * u;
    let dw = ui.x + ui.w - dx;
    let panel = Rect::new(dx, top, dw, bottom - top);
    icons::rounded_rect(ui.c, panel, 14 * u, t.card);
    let Some(d) = &s.mail_detail else {
        let text = "Okumak için bir mesaj seç.";
        let tw = ui.f.body.measure(text);
        ui.f.body.draw(
            ui.c,
            panel.x + (panel.w - tw) / 2,
            panel.y + panel.h / 2 - 10 * u,
            text,
            t.muted,
        );
        return (list_max, 0);
    };
    let px = panel.x + 20 * u;
    let pw = panel.w - 40 * u;
    let inner = panel.inset(2 * u);
    ui.clip = inner;
    ui.c.clip = inner;
    let mut y = panel.y + 18 * u - s.detail_scroll;
    for line in wrap(ui.f.heading, d.get("subject").as_str(), pw)
        .iter()
        .take(3)
    {
        ui.f.heading.draw(ui.c, px, y, line, t.text);
        y += ui.f.heading.line_height();
    }
    let who = if s.folder == SENT {
        format!("Alıcı: {}", d.get("to").as_str())
    } else {
        format!("Gönderen: {}", d.get("from").as_str())
    };
    let meta = format!("{} · {}", who, date(d.get("sentAt").as_str(), s.tz_minutes));
    ui.f.small
        .draw_clipped(ui.c, px, y + 4 * u, &meta, pw, t.muted);
    y += 30 * u;
    let mut actions: Vec<(&str, Kind, Control)> = Vec::new();
    if s.folder != SENT {
        actions.push((
            if d.get("isFavorite").as_bool() {
                "Favoriden çıkar"
            } else {
                "Favori"
            },
            Kind::Secondary,
            Control::Flag("isFavorite"),
        ));
        actions.push((
            if d.get("isImportant").as_bool() {
                "Önemli (kaldır)"
            } else {
                "Önemli"
            },
            Kind::Secondary,
            Control::Flag("isImportant"),
        ));
        if s.folder != 2 {
            actions.push(("Çöpe Taşı", Kind::Secondary, Control::Move("TRASH")));
        }
        if s.folder != 1 {
            actions.push(("Spam İşaretle", Kind::Secondary, Control::Move("SPAM")));
        }
        if s.folder != 0 {
            actions.push((
                "Gelen Kutusuna Taşı",
                Kind::Secondary,
                Control::Move("INBOX"),
            ));
        }
    }
    actions.push(("Sil", Kind::Danger, Control::Ask(Action::DeleteMail)));
    y = button_flow(ui, px, y, pw, &actions) + 6 * u;
    ui.c.fill(
        Rect::new(px, y, pw, 1),
        with_alpha(if t.dark { 0xffffff } else { 0 }, 18),
    );
    y += 16 * u;
    let body = ui.f.body;
    for line in wrap(body, d.get("body").as_str(), pw) {
        if y + body.line_height() >= inner.y && y <= inner.bottom() {
            body.draw(ui.c, px, y, &line, t.text);
        }
        y += body.line_height();
    }
    let attachments = d.get("attachments").items();
    if !attachments.is_empty() {
        y += 16 * u;
        ui.f.small.draw(ui.c, px, y, "Ekler", t.muted);
        y += 24 * u;
        for a in attachments {
            let r = Rect::new(px, y, pw, 44 * u);
            icons::rounded_rect(ui.c, r, 10 * u, t.control);
            let name = a.get("filename").as_str();
            let text = format!(
                "{} · {}",
                name,
                bytes_text(a.get("sizeBytes").as_i64().max(0) as usize)
            );
            ui.f.small.draw_clipped(
                ui.c,
                r.x + 14 * u,
                r.y + 14 * u,
                &text,
                r.w - 110 * u,
                t.text,
            );
            buttons_right(
                ui,
                r.right() - 8 * u,
                r.y + 6 * u,
                &[(
                    "İndir",
                    Kind::Primary,
                    Control::Download(String::from(a.get("id").as_str()), String::from(name)),
                )],
            );
            y += 52 * u;
        }
    }
    let detail_max = (y + s.detail_scroll + 20 * u - panel.bottom()).max(0);
    ui.clip = saved;
    ui.c.clip = saved;
    (list_max, detail_max)
}

fn draw_devices(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    toolbar(
        ui,
        "Cihazlar",
        &[("Yenile", Kind::Secondary, Control::ReloadDevices)],
    );
    if s.approval_device() && !s.pending.is_empty() {
        ui.section("Bekleyen Onaylar");
        ui.card(s.pending.len());
        for p in &s.pending {
            let label = format!(
                "{}{}",
                match p.kind {
                    PendingKind::Reset => "Şifre sıfırlama · ",
                    PendingKind::Login => "Yeni cihazdan giriş · ",
                },
                p.device
            );
            let desc = format!(
                "{} · {}",
                if p.ip.is_empty() {
                    "Bilinmeyen IP"
                } else {
                    &p.ip
                },
                date(&p.created, s.tz_minutes)
            );
            ui.buttons(
                &label,
                Some(&desc),
                &[
                    (
                        "Onayla",
                        Control::Respond(p.id.clone(), p.kind, true),
                        false,
                    ),
                    (
                        "Reddet",
                        Control::Respond(p.id.clone(), p.kind, false),
                        true,
                    ),
                ],
            );
        }
        ui.section("Cihazlar");
    }
    if s.renaming.is_some() {
        let r = Rect::new(ui.x, ui.y, ui.w - 240 * u, 40 * u);
        field(ui, s, r, "Yeni cihaz adı", F_RENAME);
        let y = ui.y + 4 * u;
        buttons_right(
            ui,
            ui.x + ui.w,
            y,
            &[
                ("Kaydet", Kind::Primary, Control::SaveRename),
                ("Vazgeç", Kind::Ghost, Control::CancelRename),
            ],
        );
        ui.y += 56 * u;
    }
    if s.approval_device() && !s.crashes.is_empty() {
        draw_crashes(s, ui);
        ui.section("Cihazlar");
    }
    let Some(list) = &s.devices else {
        empty(ui, "Yükleniyor…");
        return;
    };
    if list.is_empty() {
        empty(ui, "Hiç cihaz bulunamadı.");
        return;
    }
    ui.card(list.len());
    for d in list {
        let mut name = String::from(d.get("deviceName").as_str());
        if d.get("isCurrentDevice").as_bool() {
            name.push_str(" (bu cihaz)");
        }
        let approval = d.get("isApprovalDevice").as_bool();
        let mut desc = format!(
            "Son görülme: {}",
            date(d.get("lastSeenAt").as_str(), s.tz_minutes)
        );
        let ip = d.get("lastSeenIp").as_str();
        if !ip.is_empty() {
            desc.push_str(&format!(" · {}", ip));
        }
        if approval {
            desc.push_str(" · Onay cihazı");
        }
        let id = String::from(d.get("id").as_str());
        ui.buttons(
            &name,
            Some(&desc),
            &[
                ("Adlandır", Control::StartRename(id.clone()), false),
                (
                    if approval {
                        "Onay cihazını kaldır"
                    } else {
                        "Onay cihazı yap"
                    },
                    Control::ApprovalDevice(id.clone(), !approval),
                    false,
                ),
                ("Kaldır", Control::Ask(Action::RemoveDevice(id)), true),
            ],
        );
    }
}

fn draw_agent(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    toolbar(
        ui,
        "Banna Agent",
        &[("Yenile", Kind::Secondary, Control::ReloadAgent)],
    );
    ui.note("Banna Agent bu bilgisayarı panelde gösterir, kurulum geçmişini bildirir ve uzaktan kurulumları uygular.");

    ui.section("Bu bilgisayar");
    ui.card(2);
    ui.info(
        "Ajan durumu",
        if s.agent_paired {
            "Eşleştirildi"
        } else {
            "Eşleşmemiş"
        },
    );
    let row = ui.row();
    if s.task.is_some() {
        ui.f.body.draw(
            ui.c,
            row.x + 18 * u,
            row.y + 16 * u,
            "Ajan çalışıyor…",
            ui.t.muted,
        );
    } else {
        let fw = (row.w - 260 * u).min(320 * u);
        if process::uid() != 0 {
            field(
                ui,
                s,
                Rect::new(row.x + 18 * u, row.y + 8 * u, fw, 40 * u),
                "Yönetici parolası",
                F_ADMIN,
            );
        }
        let items = if s.agent_paired {
            [("Bağlantıyı kaldır", Kind::Danger, Control::UnpairAgent)]
        } else {
            [("Bu bilgisayarı eşleştir", Kind::Primary, Control::PairAgent)]
        };
        buttons_right(ui, row.right() - 18 * u, row.y + 12 * u, &items);
    }

    ui.section("Cihazlarım");
    let devices = s.os_status.get("devices").items();
    if devices.is_empty() {
        empty(ui, "Kayıtlı cihaz yok.");
    } else {
        let rows = devices.len()
            + devices
                .iter()
                .filter(|d| s.tokens.iter().any(|(id, _)| id == d.get("id").as_str()))
                .count();
        ui.card(rows);
        for d in devices {
            let id = String::from(d.get("id").as_str());
            let paired = d.get("agentPaired").as_bool();
            let os = d.get("osName").as_str();
            let label = format!(
                "{}{} · {}",
                d.get("deviceName").as_str(),
                if os.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", os)
                },
                if paired {
                    "Eşleştirildi"
                } else {
                    "Eşleşmemiş"
                }
            );
            let seen = d.get("lastAgentSeenAt").as_str();
            let desc = format!(
                "Son agent görülme: {}",
                if seen.is_empty() {
                    String::from("-")
                } else {
                    date(seen, s.tz_minutes)
                }
            );
            ui.buttons(
                &label,
                Some(&desc),
                &[(
                    if paired {
                        "Yeniden Eşleştir"
                    } else {
                        "Jeton Oluştur"
                    },
                    Control::IssueToken(id.clone()),
                    false,
                )],
            );
            if let Some((_, token)) = s.tokens.iter().find(|(d, _)| *d == id) {
                ui.buttons(
                    token,
                    Some("Diğer cihazda: sudo banna-agent pair <jeton>"),
                    &[("Kopyala", Control::CopyToken(token.clone()), false)],
                );
            }
        }
    }

    ui.section("Banna Store");
    let paired: Vec<&Value> = devices
        .iter()
        .filter(|d| d.get("agentPaired").as_bool())
        .collect();
    if paired.is_empty() {
        empty(ui, "Uzaktan kurulum için eşleştirilmiş bir cihaz gerekir.");
    } else {
        let options: Vec<(String, Control)> = paired
            .iter()
            .map(|d| {
                (
                    String::from(d.get("deviceName").as_str()),
                    Control::Target(String::from(d.get("id").as_str())),
                )
            })
            .collect();
        ui.chips(&options, &Control::Target(s.target.clone()));
    }
    match &s.store_apps {
        None => empty(ui, "Yükleniyor…"),
        Some(apps) if apps.is_empty() => empty(ui, "Katalogda uygulama yok."),
        Some(apps) => {
            ui.card(apps.len());
            for app in apps {
                let name = app.get("packageName").as_str();
                let size = app.get("sizeBytes").as_i64();
                let label = format!(
                    "{} · v{} · {}{}",
                    name,
                    app.get("version").as_str(),
                    app.get("architecture").as_str(),
                    if size > 0 {
                        format!(" · {}", bytes_text(size as usize))
                    } else {
                        String::new()
                    }
                );
                if paired.is_empty() {
                    ui.info(&label, app.get("description").as_str());
                } else {
                    ui.buttons(
                        &label,
                        Some(app.get("description").as_str()),
                        &[(
                            "Cihaza Kur",
                            Control::RemoteInstall(String::from(name)),
                            false,
                        )],
                    );
                }
            }
        }
    }

    ui.section("Son Etkinlik");
    let activity = s.os_status.get("recentActivity").items();
    if activity.is_empty() {
        empty(ui, "Henüz bir etkinlik yok.");
    } else {
        ui.card(activity.len());
        for a in activity {
            let version = a.get("version").as_str();
            let label = format!(
                "{}{}",
                a.get("packageName").as_str(),
                if version.is_empty() {
                    String::new()
                } else {
                    format!(" ({})", version)
                }
            );
            let when = if a.get("completedAt").is_null() {
                a.get("requestedAt").as_str()
            } else {
                a.get("completedAt").as_str()
            };
            let status = match a.get("status").as_str() {
                "SUCCESS" => "Başarılı",
                "FAILED" => "Başarısız",
                "QUEUED" => "Kuyrukta",
                other => other,
            };
            let mut desc = format!(
                "{} · {} · {} · {}",
                a.get("deviceName").as_str(),
                if a.get("origin").as_str() == "AGENT_REPORT" {
                    "Agent raporu"
                } else {
                    "Uzaktan kurulum"
                },
                status,
                date(when, s.tz_minutes)
            );
            let err = a.get("errorMessage").as_str();
            if !err.is_empty() {
                desc.push_str(&format!(" · {}", err));
            }
            let row = ui.row();
            ui.label(row, &label, Some(&desc), row.w - 36 * u);
        }
    }
}

fn draw_security(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    ui.heading("Güvenlik");
    ui.section("Şifre Değiştir");
    let fw = ((ui.w - 12 * u) / 2).min(360 * u);
    label(ui, ui.x, ui.y, "Mevcut şifre");
    let y = ui.y + 20 * u;
    field(
        ui,
        s,
        Rect::new(ui.x, y, fw, 40 * u),
        "Mevcut şifre",
        F_CURRENT,
    );
    label(ui, ui.x + fw + 12 * u, ui.y, "Yeni şifre");
    field(
        ui,
        s,
        Rect::new(ui.x + fw + 12 * u, y, fw, 40 * u),
        "Yeni şifre",
        F_NEW,
    );
    ui.y = y + 54 * u;
    let x = ui.x;
    let yy = ui.y;
    button(
        ui,
        x,
        yy,
        "Şifreyi Güncelle",
        Kind::Primary,
        Control::ChangePassword,
    );
    ui.y += 56 * u;

    ui.section("Hesabı Sil");
    let warn = Rect::new(
        ui.x,
        ui.y,
        ui.w,
        if s.deleting_account { 170 * u } else { 110 * u },
    );
    icons::rounded_rect(ui.c, warn, 12 * u, with_alpha(DANGER, 30));
    ui.f.body.draw_clipped(
        ui.c,
        warn.x + 18 * u,
        warn.y + 16 * u,
        "Bu işlem geri alınamaz. Tüm mailleriniz, cihazlarınız ve verileriniz kalıcı olarak silinir.",
        warn.w - 36 * u,
        ui.t.text,
    );
    let by = warn.y + 56 * u;
    if !s.deleting_account {
        button(
            ui,
            warn.x + 18 * u,
            by,
            "Hesabımı Sil",
            Kind::Danger,
            Control::DeleteAccount(true),
        );
    } else {
        label(ui, warn.x + 18 * u, by, "Şifrenizi girerek onaylayın");
        field(
            ui,
            s,
            Rect::new(warn.x + 18 * u, by + 20 * u, fw, 40 * u),
            "Şifre",
            F_DELETE,
        );
        let x = warn.x + 18 * u;
        let r = button(
            ui,
            x,
            by + 72 * u,
            "Kalıcı Olarak Sil",
            Kind::Danger,
            Control::ConfirmDelete,
        );
        button(
            ui,
            r.right() + 8 * u,
            by + 72 * u,
            "Vazgeç",
            Kind::Ghost,
            Control::DeleteAccount(false),
        );
    }
    ui.y = warn.bottom() + 20 * u;
}

fn draw_settings(s: &State, ui: &mut Ui<Control>) {
    let u = ui.u;
    ui.heading("Ayarlar");
    ui.section("Görünüm");
    ui.card(1);
    ui.buttons(
        "Tema",
        Some("Banna Panel masaüstünün koyu/açık temasını ve vurgu rengini kullanır"),
        &[("Masaüstü ayarları", Control::DesktopSettings, false)],
    );
    ui.section("Bildirimler");
    ui.card(1);
    ui.toggle(
        "Yeni bildirimleri göster",
        Some("Yeni bildirim, mail ve onay istekleri pencerenin köşesinde görünür"),
        s.toasts,
        Control::Toasts,
    );
    ui.section("Yasal");
    ui.card(1);
    ui.buttons(
        "KVKK ve Aydınlatma Metni",
        None,
        &[
            ("KVKK", Control::Legal("/legal/kvkk"), false),
            (
                "Aydınlatma Metni",
                Control::Legal("/legal/aydinlatma"),
                false,
            ),
        ],
    );
    ui.section("İletişim");
    ui.note(&format!(
        "Mesajınız Banna ekibine ({}) Banna Mail ile gider.",
        SUPPORT_ADDRESS
    ));
    label(ui, ui.x, ui.y, "Konu");
    let y = ui.y + 20 * u;
    field(
        ui,
        s,
        Rect::new(ui.x, y, ui.w, 40 * u),
        "Konu",
        F_CONTACT_SUBJECT,
    );
    label(ui, ui.x, y + 52 * u, "Mesaj");
    text_area(
        ui,
        s,
        Rect::new(ui.x, y + 72 * u, ui.w, 120 * u),
        "Mesajınız",
        F_CONTACT_BODY,
    );
    ui.y = y + 204 * u;
    let x = ui.x;
    let yy = ui.y;
    button(ui, x, yy, "Gönder", Kind::Primary, Control::SendContact);
    ui.y += 52 * u;
    ui.section("Uygulama");
    ui.card(3);
    ui.buttons(
        &format!("Banna Panel {}", VERSION),
        Some("Banna OS sürümü · güncellemeler Banna Store'dan"),
        &[("Sürüm notları", Control::Changelog, false)],
    );
    ui.buttons(
        "Kurulum sihirbazı",
        Some("Görünüm, duvar kağıdı ve Banna hesabı adımları"),
        &[("Aç", Control::Wizard, false)],
    );
    ui.buttons(
        "Verileri yenile",
        None,
        &[("Yenile", Control::Reload, false)],
    );
}

fn draw_downloads(s: &State, ui: &mut Ui<Control>) {
    ui.heading("İndirilenler");
    ui.note(&format!(
        "Banna Mail ekleri {}/İndirilenler klasörüne kaydedilir.",
        home()
    ));
    if s.downloads.is_empty() {
        empty(ui, "Bu oturumda henüz bir şey indirilmedi.");
        return;
    }
    ui.card(s.downloads.len());
    for d in &s.downloads {
        let desc = format!("{} · {}", d.path, bytes_text(d.size));
        ui.buttons(
            &d.name,
            Some(&desc),
            &[("Yolu kopyala", Control::CopyPath(d.path.clone()), false)],
        );
    }
}

// ---- Pencereler (modal) ----

fn modal_card(ui: &mut Ui<Control>, w: i32, h: i32, cw: i32, ch: i32) -> Rect {
    let u = ui.u;
    let full = Rect::new(0, 0, w, h);
    ui.clip = full;
    ui.c.clip = full;
    ui.c.fill(full, with_alpha(0, 120));
    ui.hit(full, Control::CloseModal);
    let card = Rect::new((w - cw) / 2, (h - ch) / 2, cw, ch);
    ui.c.shadow(card, 18 * u, 20 * u, 90);
    icons::rounded_rect(ui.c, card, 18 * u, ui.t.window_bg);
    ui.hit(card, Control::Nothing);
    card
}

fn draw_modal(s: &State, ui: &mut Ui<Control>, w: i32, h: i32) -> i32 {
    let u = ui.u;
    let t = ui.t;
    match s.modal.as_ref() {
        None => 0,
        Some(Modal::Confirm(text, action)) => {
            let card = modal_card(ui, w, h, 460 * u, 190 * u);
            let mut y = card.y + 26 * u;
            for line in wrap(ui.f.body, text, card.w - 48 * u).iter().take(3) {
                ui.f.body.draw(ui.c, card.x + 24 * u, y, line, t.text);
                y += ui.f.body.line_height();
            }
            buttons_right(
                ui,
                card.right() - 20 * u,
                card.bottom() - 52 * u,
                &[
                    ("Vazgeç", Kind::Secondary, Control::CloseModal),
                    ("Onayla", Kind::Danger, Control::Do(action.clone())),
                ],
            );
            0
        }
        Some(Modal::Compose) => {
            let cw = (w - 80 * u).min(640 * u);
            let ch = (h - 60 * u).min(600 * u);
            let card = modal_card(ui, w, h, cw, ch);
            let x = card.x + 24 * u;
            let fw = card.w - 48 * u;
            let mut y = card.y + 20 * u;
            ui.f.heading.draw(ui.c, x, y, "Yeni Mail", t.text);
            y += ui.f.heading.line_height() + 8 * u;
            label(ui, x, y, "Kime (E-Banna adresleri, virgülle ayır)");
            field(
                ui,
                s,
                Rect::new(x, y + 20 * u, fw, 38 * u),
                "ornek@banna.com",
                F_TO,
            );
            y += 66 * u;
            label(ui, x, y, "Konu");
            field(
                ui,
                s,
                Rect::new(x, y + 20 * u, fw, 38 * u),
                "Konu",
                F_SUBJECT,
            );
            y += 66 * u;
            label(ui, x, y, "Mesaj");
            let body_h = card.bottom() - (y + 20 * u) - 190 * u;
            text_area(
                ui,
                s,
                Rect::new(x, y + 20 * u, fw, body_h.max(60 * u)),
                "Mesajınız",
                F_BODY,
            );
            y += 20 * u + body_h.max(60 * u) + 12 * u;
            label(ui, x, y, "Ekler (dosya yolu, ör. Belgeler/rapor.pdf)");
            let aw = fw - button_width(ui, "Dosya Ekle") - 8 * u;
            field(
                ui,
                s,
                Rect::new(x, y + 20 * u, aw, 36 * u),
                "/home/…",
                F_ATTACH,
            );
            button(
                ui,
                x + aw + 8 * u,
                y + 22 * u,
                "Dosya Ekle",
                Kind::Secondary,
                Control::AddAttachment,
            );
            y += 64 * u;
            let mut cx = x;
            for (i, path) in s.attachments.iter().enumerate() {
                let name = format!("{}  x", file_name(path));
                let cw = button_width(ui, &name);
                if cx + cw > x + fw {
                    break;
                }
                button(
                    ui,
                    cx,
                    y,
                    &name,
                    Kind::Secondary,
                    Control::RemoveAttachment(i),
                );
                cx += cw + 6 * u;
            }
            let by = card.bottom() - 52 * u;
            if let Some((text, error)) = &s.message {
                let color = if *error { DANGER } else { t.muted };
                ui.f.small
                    .draw_clipped(ui.c, x, by + 8 * u, text, fw - 220 * u, color);
            }
            buttons_right(
                ui,
                card.right() - 20 * u,
                by,
                &[
                    ("Vazgeç", Kind::Ghost, Control::CloseModal),
                    ("Gönder", Kind::Primary, Control::Send),
                ],
            );
            0
        }
        Some(Modal::Blocked) => {
            let card = modal_card(ui, w, h, 520 * u, 440 * u);
            let x = card.x + 24 * u;
            ui.f.heading
                .draw(ui.c, x, card.y + 20 * u, "Engellenen Gönderenler", t.text);
            let list = Rect::new(card.x, card.y + 70 * u, card.w, card.h - 140 * u);
            ui.clip = list;
            ui.c.clip = list;
            let mut y = list.y - s.modal_scroll;
            match &s.blocked {
                None => {
                    ui.f.small.draw(ui.c, x, y, "Yükleniyor…", t.muted);
                }
                Some(b) if b.is_empty() => {
                    ui.f.small
                        .draw(ui.c, x, y, "Engellenen kimse yok.", t.muted);
                }
                Some(b) => {
                    for item in b {
                        let r = Rect::new(x, y, card.w - 48 * u, 48 * u);
                        icons::rounded_rect(ui.c, r, 10 * u, t.card);
                        ui.f.body.draw_clipped(
                            ui.c,
                            r.x + 14 * u,
                            r.y + 14 * u,
                            item.get("ebannaAddress").as_str(),
                            r.w - 160 * u,
                            t.text,
                        );
                        buttons_right(
                            ui,
                            r.right() - 8 * u,
                            r.y + 8 * u,
                            &[(
                                "Engeli Kaldır",
                                Kind::Ghost,
                                Control::Unblock(String::from(item.get("id").as_str())),
                            )],
                        );
                        y += 56 * u;
                    }
                }
            }
            let full = Rect::new(0, 0, w, h);
            ui.clip = full;
            ui.c.clip = full;
            buttons_right(
                ui,
                card.right() - 20 * u,
                card.bottom() - 52 * u,
                &[("Kapat", Kind::Secondary, Control::CloseModal)],
            );
            (y + s.modal_scroll - list.bottom()).max(0)
        }
        Some(Modal::Document(doc)) => {
            let cw = (w - 80 * u).min(720 * u);
            let ch = h - 80 * u;
            let card = modal_card(ui, w, h, cw, ch);
            let x = card.x + 28 * u;
            let tw = card.w - 56 * u;
            let area = Rect::new(card.x, card.y + 8 * u, card.w, card.h - 76 * u);
            ui.clip = area;
            ui.c.clip = area;
            let mut y = card.y + 24 * u - s.modal_scroll;
            for line in wrap(ui.f.heading, &doc.title, tw) {
                ui.f.heading.draw(ui.c, x, y, &line, t.text);
                y += ui.f.heading.line_height();
            }
            if !doc.updated.is_empty() {
                ui.f.small.draw(
                    ui.c,
                    x,
                    y + 4 * u,
                    &format!("Son güncelleme: {}", doc.updated),
                    t.muted,
                );
                y += 26 * u;
            }
            y += 12 * u;
            for (heading, paragraphs) in &doc.sections {
                if !heading.is_empty() {
                    for line in wrap(ui.f.title, heading, tw) {
                        ui.f.title.draw(ui.c, x, y, &line, t.text);
                        y += ui.f.title.line_height();
                    }
                    y += 6 * u;
                }
                for p in paragraphs {
                    for line in wrap(ui.f.body, p, tw) {
                        if y + 40 * u > area.y && y < area.bottom() {
                            ui.f.body.draw(ui.c, x, y, &line, t.muted);
                        }
                        y += ui.f.body.line_height();
                    }
                    y += 10 * u;
                }
                y += 8 * u;
            }
            let full = Rect::new(0, 0, w, h);
            ui.clip = full;
            ui.c.clip = full;
            buttons_right(
                ui,
                card.right() - 20 * u,
                card.bottom() - 52 * u,
                &[("Kapat", Kind::Secondary, Control::CloseModal)],
            );
            (y + s.modal_scroll - area.bottom()).max(0)
        }
    }
}

fn draw_toast(s: &State, ui: &mut Ui<Control>, w: i32) {
    let Some((title, body, _)) = &s.toast else {
        return;
    };
    let u = ui.u;
    let tw = 360 * u;
    let r = Rect::new(w - tw - 20 * u, 16 * u, tw, 74 * u);
    let full = ui.c.clip;
    ui.c.shadow(r, 14 * u, 14 * u, 80);
    icons::rounded_rect(ui.c, r, 14 * u, ui.t.card);
    icons::logo(
        ui.c,
        Rect::new(r.x + 14 * u, r.y + 17 * u, 40 * u, 40 * u),
        ui.f.title,
        ui.t.accent,
        WHITE,
    );
    ui.f.title.draw_clipped(
        ui.c,
        r.x + 66 * u,
        r.y + 14 * u,
        title,
        r.w - 80 * u,
        ui.t.text,
    );
    ui.f.small.draw_clipped(
        ui.c,
        r.x + 66 * u,
        r.y + 40 * u,
        body,
        r.w - 80 * u,
        ui.t.muted,
    );
    ui.hit(r, Control::Toast);
    ui.c.clip = full;
}

struct App {
    window: Window,
    s: State,
    hits: Vec<Hit<Control>>,
    hover: Option<Rect>,
    dirty: bool,
}

impl App {
    fn refresh_theme(&mut self) {
        if let Some(info) = desktop::query() {
            let prefs = Prefs::parse(&info.text());
            self.s.theme = Theme::from(&prefs);
            self.s.tz_minutes = prefs.tz_minutes.unwrap_or(180) as i64;
        }
    }

    fn render(&mut self) {
        self.dirty = false;
        let s = &mut self.s;
        let u = s.u;
        let (w, h) = (self.window.width as i32, self.window.height as i32);
        let t = s.theme;
        let mut hits = Vec::new();
        let sidebar_w = 240 * u;
        let pad = 28 * u;
        let mut bottom = 0;
        let mut mail_max = (0, 0);
        let mut modal_max = 0;
        {
            let mut c = self.window.canvas();
            c.fill(Rect::new(0, 0, w, h), t.window_bg);
            let full = Rect::new(0, 0, w, h);
            let mut ui = Ui::new(&mut c, t, &s.fonts, u, full, 0, &mut hits, self.hover);
            if !s.signed_in {
                draw_login(s, &mut ui, w, h);
            } else {
                draw_sidebar(s, &mut ui, sidebar_w, h);
                let area = Rect::new(sidebar_w + pad, 0, w - sidebar_w - 2 * pad, h);
                let content = Rect::new(sidebar_w, 0, w - sidebar_w, h);
                ui.x = area.x;
                ui.w = area.w;
                ui.clip = content;
                ui.c.clip = content;
                if s.page == Page::Mail {
                    mail_max = draw_mail(s, &mut ui, h);
                } else {
                    ui.y = 24 * u - s.scroll;
                    match s.page {
                        Page::Dashboard => draw_dashboard(s, &mut ui),
                        Page::Notifications => draw_notifications(s, &mut ui),
                        Page::Devices => draw_devices(s, &mut ui),
                        Page::Agent => draw_agent(s, &mut ui),
                        Page::Security => draw_security(s, &mut ui),
                        Page::Settings => draw_settings(s, &mut ui),
                        Page::Downloads => draw_downloads(s, &mut ui),
                        Page::Mail => {}
                    }
                    bottom = ui.y + s.scroll + 56 * u;
                    let max_scroll = (bottom - h).max(0);
                    if max_scroll > 0 {
                        let track_h = h - 16 * u;
                        let bar_h = (track_h * h / bottom.max(1)).max(30 * u);
                        let bar_y = 8 * u
                            + (track_h - bar_h) * s.scroll.min(max_scroll) / max_scroll.max(1);
                        icons::rounded_rect(
                            ui.c,
                            Rect::new(w - 8 * u, bar_y, 4 * u, bar_h),
                            2 * u,
                            with_alpha(t.muted, 120),
                        );
                    }
                }
                if s.page == Page::Notifications && s.selecting {
                    draw_selection_bar(s, &mut ui, area);
                }
                // Alt durum çubuğu.
                let text = s.status.as_ref().map(|st| (st.clone(), false)).or_else(|| {
                    s.message
                        .clone()
                        .filter(|_| !matches!(s.modal, Some(Modal::Compose)))
                });
                if let Some((text, error)) = text {
                    let f = ui.f.small;
                    let bw = (f.measure(&text) + 32 * u).min(area.w);
                    let bar = Rect::new(area.x + (area.w - bw) / 2, h - 44 * u, bw, 32 * u);
                    let lift = if s.page == Page::Notifications && s.selecting {
                        70 * u
                    } else {
                        0
                    };
                    let bar = Rect::new(bar.x, bar.y - lift, bar.w, bar.h);
                    ui.c.shadow(bar, 12 * u, 10 * u, 50);
                    icons::rounded_rect(ui.c, bar, 12 * u, if error { DANGER } else { t.card });
                    f.draw_clipped(
                        ui.c,
                        bar.x + 16 * u,
                        bar.y + (bar.h - f.ascent - f.descent) / 2,
                        &text,
                        bar.w - 32 * u,
                        if error { WHITE } else { t.text },
                    );
                }
            }
            if s.modal.is_some() {
                ui.hits.clear();
                modal_max = draw_modal(s, &mut ui, w, h);
            }
            ui.clip = full;
            ui.c.clip = full;
            draw_toast(s, &mut ui, w);
        }
        self.window.present();
        self.hits = hits;
        if s.signed_in {
            if s.page == Page::Mail {
                s.list_max = mail_max.0;
                s.detail_max = mail_max.1;
                if s.list_scroll > s.list_max || s.detail_scroll > s.detail_max {
                    s.list_scroll = s.list_scroll.min(s.list_max);
                    s.detail_scroll = s.detail_scroll.min(s.detail_max);
                    self.dirty = true;
                }
            } else {
                s.max_scroll = (bottom - h).max(0);
                if s.scroll > s.max_scroll {
                    s.scroll = s.max_scroll;
                    self.dirty = true;
                }
            }
        }
        if s.modal_scroll > modal_max {
            s.modal_scroll = modal_max;
            self.dirty = true;
        }
    }

    fn run(&mut self, control: Control) {
        if let Some(text) = self.s.busy_text(&control) {
            self.s.status = Some(String::from(text));
            self.render();
        }
        self.s.handle(control);
        self.s.status = None;
        self.dirty = true;
    }

    fn press(&mut self, x: i32, y: i32) {
        let Some(hit) = ui::hit_at(&self.hits, x, y).cloned() else {
            if self.s.focus.is_some() {
                self.s.focus = None;
                self.dirty = true;
            }
            return;
        };
        if !matches!(hit.control, Control::Field(_)) && self.s.modal.is_none() {
            self.s.focus = match hit.control {
                Control::Nothing => self.s.focus,
                _ => None,
            };
        }
        self.run(hit.control);
    }
}

fn main(_args: &[&str]) -> i32 {
    let u = ui::unit();
    let info = process::sysinfo();
    // Compositor pencereleri en fazla 1920x1200.
    let width = (1080 * u)
        .min(info.screen_width as i32 - 40 * u)
        .clamp(640, 1920);
    let height = (720 * u)
        .min(info.screen_height as i32 - 110 * u)
        .clamp(480, 1200);
    let window = match Window::new("Banna Panel", width as usize, height as usize) {
        Ok(w) => w,
        Err(e) => {
            banna::eprintln!("banna-panel: pencere açılamadı ({})", e);
            return 1;
        }
    };
    let prefs = desktop::query()
        .map(|i| Prefs::parse(&i.text()))
        .unwrap_or_default();
    let client = Client::load();
    let has_session = client.has_session();
    let mut app = App {
        window,
        s: State {
            fonts: ui::fonts(),
            u,
            theme: Theme::from(&prefs),
            tz_minutes: prefs.tz_minutes.unwrap_or(180) as i64,
            client,
            booting: has_session,
            signed_in: false,
            remember: true,
            toasts: true,
            pending_login: None,
            last_approval_poll: 0,
            me: Value::Null,
            page: Page::Dashboard,
            fields: core::array::from_fn(|_| ui::TextInput::default()),
            focus: Some(F_ADDRESS),
            message: None,
            status: None,
            modal: None,
            toast: None,
            scroll: 0,
            max_scroll: 0,
            modal_scroll: 0,
            mouse: (0, 0),
            last_poll: 0,
            notify_enabled: None,
            notify_token: String::new(),
            notify_expires: 0,
            notify_registered: false,
            notifications: Vec::new(),
            notif_unread: 0,
            notif_cursor: None,
            security_only: false,
            selecting: false,
            selected: Vec::new(),
            mail_unread: 0,
            folder: 0,
            favorites_only: false,
            mail_list: None,
            mail_detail: None,
            blocked: None,
            attachments: Vec::new(),
            last_mail_send: 0,
            list_scroll: 0,
            list_max: 0,
            detail_scroll: 0,
            detail_max: 0,
            devices: None,
            crashes: Vec::new(),
            crash_open: None,
            pending: Vec::new(),
            known_pending: Vec::new(),
            renaming: None,
            os_status: Value::Null,
            store_apps: None,
            tokens: Vec::new(),
            target: String::new(),
            agent_paired: false,
            task: None,
            deleting_account: false,
            last_contact: 0,
            downloads: Vec::new(),
        },
        hits: Vec::new(),
        hover: None,
        dirty: true,
    };
    app.s.load_config();
    banna::println!("banna-panel: ready");
    if has_session {
        app.render();
        app.s.load_account();
        app.s.booting = false;
        if app.s.signed_in {
            app.s.focus = None;
        }
    }
    loop {
        if app.dirty {
            app.render();
        }
        let now = time::uptime_ms();
        let timeout = if app.s.task.is_some() {
            200
        } else if app.s.pending_login.is_some() {
            500
        } else if app.s.toast.is_some() {
            1000
        } else {
            5_000
        };
        let event = app.window.event(timeout);
        if let Some((_, _, until)) = app.s.toast
            && now >= until
        {
            app.s.toast = None;
            app.dirty = true;
        }
        if app.s.poll_login_approval() || app.s.poll_task() {
            app.dirty = true;
        }
        if app.s.signed_in
            && app.s.modal.is_none()
            && time::uptime_ms().saturating_sub(app.s.last_poll) >= POLL_MS
        {
            let before = (app.s.notif_unread, app.s.mail_unread, app.s.pending.len());
            app.s.poll();
            if before != (app.s.notif_unread, app.s.mail_unread, app.s.pending.len())
                || app.s.toast.is_some()
            {
                app.dirty = true;
            }
        }
        let Some(event) = event else {
            continue;
        };
        match event.kind {
            EVENT_CLOSE => {
                if app.s.signed_in && !app.s.remember {
                    app.s.client.logout();
                }
                return 0;
            }
            EVENT_RESIZE => {
                app.window.resize(event.x as usize, event.y as usize);
                app.dirty = true;
            }
            EVENT_MOUSE_DOWN => app.press(event.x, event.y),
            EVENT_MOUSE_MOVE => {
                app.s.mouse = (event.x, event.y);
                let hovered = ui::hit_at(&app.hits, event.x, event.y).map(|h| h.rect);
                if hovered != app.hover {
                    app.hover = hovered;
                    app.dirty = true;
                }
            }
            EVENT_WHEEL => {
                app.s.scroll_by(event.key as i32 * 60 * u);
                app.dirty = true;
            }
            EVENT_KEY => {
                if let Some(control) = app.s.key(event.key) {
                    app.run(control);
                }
                app.dirty = true;
            }
            _ => {}
        }
        if app.s.page == Page::Settings || app.s.page == Page::Dashboard {
            app.refresh_theme();
        }
    }
}
