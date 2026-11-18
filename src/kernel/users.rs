//! Kullanıcı hesapları, oturum ve yol tabanlı erişim denetimi.
//!
//! Hesaplar `/etc/users` dosyasında tutulur (yalnızca root okuyabilir); parolalar
//! PBKDF2-HMAC-SHA256 ile tuzlanıp özetlenir. FAT32 izin bitleri taşımadığı için
//! erişim yola göre denetlenir: herkes okur (başkasının ev dizini ve hesap dosyası
//! hariç), normal kullanıcılar yalnızca kendi ev dizinine, `/tmp`'ye ve çıkarılabilir
//! birimlere yazar; root her şeye yazar.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use crate::crypto;
use crate::fs;
use crate::sched::WaitQueue;
use crate::sync::SpinLock;

pub const ROOT_UID: u32 = 0;
const FIRST_UID: u32 = 1000;
pub const USERS_FILE: &str = "/etc/users";
const ITERATIONS: u32 = 10_000;
pub const MIN_PASSWORD: usize = 4;

#[derive(Clone, Debug)]
pub struct User {
    pub name: String,
    pub uid: u32,
    pub admin: bool,
    pub home: String,
    hash: String,
    /// Live USB hesabı: diske kaydedilmez.
    ephemeral: bool,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub uid: u32,
    pub name: String,
    pub admin: bool,
    pub home: String,
}

static USERS: SpinLock<Vec<User>> = SpinLock::new(Vec::new());
static SESSION: SpinLock<Option<Session>> = SpinLock::new(None);
static SESSION_CHANGED: WaitQueue = WaitQueue::new();
static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Kabukta `sudo` ile çalışan komut süresince root yetkisi.
static ELEVATED: AtomicBool = AtomicBool::new(false);

pub(crate) fn hash_password(password: &str, salt: &[u8]) -> String {
    let mut out = [0u8; 32];
    crypto::pbkdf2_sha256(password.as_bytes(), salt, ITERATIONS, &mut out);
    format!(
        "pbkdf2-sha256${}${}${}",
        ITERATIONS,
        crypto::to_hex(salt),
        crypto::to_hex(&out)
    )
}

pub(crate) fn verify(hash: &str, password: &str) -> bool {
    let mut parts = hash.split('$');
    let (Some("pbkdf2-sha256"), Some(iterations), Some(salt), Some(expected)) =
        (parts.next(), parts.next(), parts.next(), parts.next())
    else {
        return false;
    };
    let (Ok(iterations), Some(salt), Some(expected)) = (
        iterations.parse::<u32>(),
        crypto::from_hex(salt),
        crypto::from_hex(expected),
    ) else {
        return false;
    };
    let mut out = alloc::vec![0u8; expected.len()];
    crypto::pbkdf2_sha256(
        password.as_bytes(),
        &salt,
        iterations.clamp(1, 1_000_000),
        &mut out,
    );
    crypto::constant_time_eq(&out, &expected)
}

pub(crate) fn new_salt() -> [u8; 16] {
    let mut salt = [0u8; 16];
    for chunk in salt.chunks_mut(4) {
        chunk.copy_from_slice(&crate::net::random_u32().to_le_bytes());
    }
    salt
}

/// `ad:uid:yönetici:ev:özet` satırları.
fn parse(text: &str) -> Vec<User> {
    text.lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .filter_map(|line| {
            let f: Vec<&str> = line.splitn(5, ':').collect();
            if f.len() != 5 || !valid_name(f[0]) {
                return None;
            }
            Some(User {
                name: String::from(f[0]),
                uid: f[1].parse().ok()?,
                admin: f[2] == "1",
                home: String::from(f[3]),
                hash: String::from(f[4]),
                ephemeral: false,
            })
        })
        .collect()
}

fn serialize(users: &[User]) -> String {
    let mut out = String::from("# Banna OS hesapları: ad:uid:yönetici:ev:parola özeti\n");
    for u in users.iter().filter(|u| !u.ephemeral) {
        out.push_str(&format!(
            "{}:{}:{}:{}:{}\n",
            u.name, u.uid, u.admin as u8, u.home, u.hash
        ));
    }
    out
}

fn save(users: &[User]) -> Result<(), &'static str> {
    if fs::stat("/etc").is_err() {
        fs::mkdir("/etc").map_err(|e| e.message())?;
    }
    fs::write_all(USERS_FILE, serialize(users).as_bytes()).map_err(|e| e.message())?;
    let _ = fs::chmod(USERS_FILE, 0o600);
    Ok(())
}

pub fn load() {
    let users = fs::read_all(USERS_FILE)
        .ok()
        .map(|data| parse(&String::from_utf8_lossy(&data)))
        .unwrap_or_default();
    crate::log!("users: {} account(s)", users.len());
    *USERS.lock() = users;
}

pub fn valid_name(name: &str) -> bool {
    (1..=32).contains(&name.len())
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
        && name != "root"
}

/// Henüz hiç hesap yoksa ilk açılış kurulumu gerekir.
pub fn needs_setup() -> bool {
    USERS.lock().is_empty()
}

pub fn create(name: &str, password: &str, admin: bool) -> Result<User, &'static str> {
    if !valid_name(name) {
        return Err("geçersiz kullanıcı adı (a-z, 0-9, _ -; harfle başlamalı)");
    }
    if password.chars().count() < MIN_PASSWORD {
        return Err("parola en az 4 karakter olmalı");
    }
    let hash = hash_password(password, &new_salt());
    let user = {
        let mut users = USERS.lock();
        if users.iter().any(|u| u.name == name) {
            return Err("bu kullanıcı zaten var");
        }
        let uid = users
            .iter()
            .map(|u| u.uid + 1)
            .max()
            .unwrap_or(FIRST_UID)
            .max(FIRST_UID);
        let user = User {
            name: String::from(name),
            uid,
            admin,
            home: format!("/home/{}", name),
            hash,
            ephemeral: false,
        };
        users.push(user.clone());
        user
    };
    let snapshot = USERS.lock().clone();
    if let Err(e) = save(&snapshot) {
        USERS.lock().retain(|u| u.name != name);
        return Err(e);
    }
    if fs::stat("/home").is_err() {
        let _ = fs::mkdir("/home");
    }
    if fs::mkdir(&user.home).is_ok() {
        // ext2'de ev dizini kullanıcıya ait ve yalnızca ona açık olur (FAT32'de etkisiz).
        let _ = fs::chown(&user.home, user.uid, user.uid);
        let _ = fs::chmod(&user.home, 0o700);
    }
    crate::log!(
        "users: created '{}' uid {}{}",
        user.name,
        user.uid,
        if admin { " (admin)" } else { "" }
    );
    Ok(user)
}

pub fn authenticate(name: &str, password: &str) -> Option<User> {
    let user = USERS.lock().iter().find(|u| u.name == name).cloned()?;
    verify(&user.hash, password).then_some(user)
}

pub enum AdminCheck {
    NotAdmin,
    WrongPassword,
}

/// `sudo` için: `uid` yönetici mi ve parola doğru mu; doğruysa kullanıcı adı.
pub fn verify_admin(uid: u32, password: &str) -> Result<String, AdminCheck> {
    let user = USERS
        .lock()
        .iter()
        .find(|u| u.uid == uid)
        .cloned()
        .ok_or(AdminCheck::NotAdmin)?;
    if !user.admin {
        return Err(AdminCheck::NotAdmin);
    }
    if verify(&user.hash, password) {
        Ok(user.name)
    } else {
        Err(AdminCheck::WrongPassword)
    }
}

pub fn set_password(name: &str, password: &str) -> Result<(), &'static str> {
    if password.chars().count() < MIN_PASSWORD {
        return Err("parola en az 4 karakter olmalı");
    }
    let hash = hash_password(password, &new_salt());
    let snapshot = {
        let mut users = USERS.lock();
        let user = users
            .iter_mut()
            .find(|u| u.name == name)
            .ok_or("böyle bir kullanıcı yok")?;
        user.hash = hash;
        users.clone()
    };
    save(&snapshot)
}

pub fn remove(name: &str) -> Result<(), &'static str> {
    let snapshot = {
        let mut users = USERS.lock();
        let before = users.len();
        users.retain(|u| u.name != name);
        if users.len() == before {
            return Err("böyle bir kullanıcı yok");
        }
        if !users.iter().any(|u| u.admin) {
            return Err("son yönetici silinemez");
        }
        users.clone()
    };
    save(&snapshot)
}

/// (ad, uid, yönetici)
pub fn list() -> Vec<(String, u32, bool)> {
    USERS
        .lock()
        .iter()
        .map(|u| (u.name.clone(), u.uid, u.admin))
        .collect()
}

pub fn name_of(uid: u32) -> String {
    if uid == ROOT_UID {
        return String::from("root");
    }
    USERS
        .lock()
        .iter()
        .find(|u| u.uid == uid)
        .map(|u| u.name.clone())
        .unwrap_or_else(|| format!("{}", uid))
}

pub fn begin_session(user: &User) {
    *SESSION.lock() = Some(Session {
        uid: user.uid,
        name: user.name.clone(),
        admin: user.admin,
        home: user.home.clone(),
    });
    GENERATION.fetch_add(1, Ordering::Release);
    crate::log!("login: {} signed in", user.name);
    SESSION_CHANGED.wake_all();
    crate::gui::wake();
}

pub fn end_session() {
    if let Some(s) = SESSION.lock().take() {
        crate::log!("login: {} signed out", s.name);
    }
    ELEVATED.store(false, Ordering::Relaxed);
    GENERATION.fetch_add(1, Ordering::Release);
    SESSION_CHANGED.wake_all();
    crate::gui::wake();
}

pub fn session() -> Option<Session> {
    SESSION.lock().clone()
}

pub fn generation() -> u64 {
    GENERATION.load(Ordering::Acquire)
}

pub fn set_elevated(on: bool) {
    ELEVATED.store(on, Ordering::Relaxed);
}

/// Çekirdek kabuğunun (ve ondan başlatılan işlemlerin) etkin kullanıcısı.
pub fn effective_uid() -> u32 {
    if ELEVATED.load(Ordering::Relaxed) {
        return ROOT_UID;
    }
    session().map_or(ROOT_UID, |s| s.uid)
}

fn home_owner(path: &str) -> Option<&str> {
    let rest = path.strip_prefix("/home/")?;
    Some(rest.split('/').next().unwrap_or(rest))
}

fn under(path: &str, dir: &str) -> bool {
    path == dir || path.strip_prefix(dir).is_some_and(|r| r.starts_with('/'))
}

fn removable_mount(path: &str) -> bool {
    fs::mounts()
        .iter()
        .any(|m| m.point != "/" && m.device.starts_with("usb") && under(path, &m.point))
}

pub fn can_read(uid: u32, path: &str) -> bool {
    if uid == ROOT_UID {
        return true;
    }
    // FAT32 büyük/küçük harf ayırmaz: `/ETC/USERS` de aynı dosyadır.
    let path = path.to_ascii_lowercase();
    let path = path.as_str();
    if under(path, USERS_FILE) {
        return false;
    }
    match home_owner(path) {
        Some(owner) if !owner.is_empty() => name_of(uid) == owner,
        _ => true,
    }
}

pub fn can_write(uid: u32, path: &str) -> bool {
    if uid == ROOT_UID {
        return true;
    }
    let path = path.to_ascii_lowercase();
    let path = path.as_str();
    if under(path, "/tmp") || removable_mount(path) {
        return true;
    }
    match home_owner(path) {
        Some(owner) => {
            !owner.is_empty() && name_of(uid) == owner && path != format!("/home/{}", owner)
        }
        None => false,
    }
}

pub fn self_test() -> bool {
    let salt = [7u8; 16];
    let hash = hash_password("gizli", &salt);
    let users = parse("# c\nali:1000:1:/home/ali:x\nbad name:1:0:/:x\nveli:1001:0:/home/veli:y\n");
    verify(&hash, "gizli")
        && !verify(&hash, "gizlı")
        && !verify("pbkdf2-sha256$1$zz$00", "x")
        && users.len() == 2
        && users[0].admin
        && !users[1].admin
        && parse(&serialize(&users)).len() == 2
        && valid_name("berke")
        && !valid_name("Berke")
        && !valid_name("root")
        && !valid_name("1abc")
        && under("/home/ali/x", "/home/ali")
        && !under("/home/alice", "/home/ali")
        && !can_read(1000, "/ETC/Users")
        && !can_write(1000, "/BIN/hello")
        && can_write(ROOT_UID, "/etc/users")
        && can_write(1000, "/tmp/x")
}

fn init(_: &mut crate::kernel::initcall::Detail) -> Result<(), &'static str> {
    crate::kernel::initcall::self_test("crypto", crypto::self_test())?;
    crate::kernel::initcall::self_test("users", self_test())?;
    load();
    if fs::stat("/tmp").is_err() {
        let _ = fs::mkdir("/tmp");
    }
    if crate::boot::has_flag("live") {
        start_live_session();
    }
    Ok(())
}

pub const LIVE_USER: &str = "live";
pub const LIVE_PASSWORD: &str = "live";
const LIVE_UID: u32 = 999;

/// Live USB: kaydedilmeyen `live` yönetici hesabıyla (parola `live`) oturum açılır.
fn start_live_session() {
    let user = User {
        name: String::from(LIVE_USER),
        uid: LIVE_UID,
        admin: true,
        home: format!("/home/{}", LIVE_USER),
        hash: hash_password(LIVE_PASSWORD, &new_salt()),
        ephemeral: true,
    };
    {
        let mut users = USERS.lock();
        users.retain(|u| u.name != LIVE_USER);
        users.push(user.clone());
    }
    if fs::stat("/home").is_err() {
        let _ = fs::mkdir("/home");
    }
    if fs::stat(&user.home).is_err() && fs::mkdir(&user.home).is_ok() {
        let _ = fs::chown(&user.home, user.uid, user.uid);
        let _ = fs::chmod(&user.home, 0o700);
    }
    crate::log!("users: live mode, session for '{}'", user.name);
    begin_session(&user);
}

crate::initcall!(late, 5, "Users (PBKDF2-SHA256)", init);
