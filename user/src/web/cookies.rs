//! Çerez kavanozu (RFC 6265'in tarayıcıların uyguladığı kısmı): alan adı ve yol eşleşmesi,
//! `Max-Age` / `Expires`, `Secure`, `HttpOnly`. HTTP yanıtları (`Set-Cookie`) ve
//! `document.cookie` aynı kavanozu kullanır; süresi olan çerezler `save` / `load` ile diske
//! yazılır (oturum çerezleri tarayıcı kapanınca gider).

use alloc::string::String;
use alloc::vec::Vec;

use super::url::Url;

#[derive(Clone, Debug, PartialEq)]
pub struct Cookie {
    pub name: String,
    pub value: String,
    /// Küçük harfli alan adı (baştaki nokta olmadan).
    pub domain: String,
    /// `Domain` verilmediyse yalnızca tam olarak bu ana makineye gider.
    pub host_only: bool,
    pub path: String,
    /// Bitiş zamanı (Unix saniyesi); `None` = oturum çerezi.
    pub expires: Option<i64>,
    pub secure: bool,
    pub http_only: bool,
}

/// Kavanozdaki en çok çerez (eskiler atılır).
const MAX_COOKIES: usize = 3000;
const MAX_VALUE: usize = 4096;

static mut JAR: Vec<Cookie> = Vec::new();
static mut DIRTY: bool = false;

fn jar() -> &'static mut Vec<Cookie> {
    unsafe { &mut *core::ptr::addr_of_mut!(JAR) }
}

fn now() -> i64 {
    crate::time::unix_time()
}

/// Kalıcı çerezler son kayıttan beri değişti mi (Banna Search kaydetmek için yoklar).
pub fn take_dirty() -> bool {
    unsafe { core::mem::replace(&mut *core::ptr::addr_of_mut!(DIRTY), false) }
}

fn mark_dirty() {
    unsafe { *core::ptr::addr_of_mut!(DIRTY) = true };
}

fn domain_match(host: &str, domain: &str) -> bool {
    host == domain
        || (host.len() > domain.len()
            && host.ends_with(domain)
            && host.as_bytes()[host.len() - domain.len() - 1] == b'.'
            && !host.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
}

fn path_match(request: &str, cookie: &str) -> bool {
    request == cookie
        || (request.starts_with(cookie)
            && (cookie.ends_with('/') || request.as_bytes().get(cookie.len()) == Some(&b'/')))
}

/// Adresin varsayılan çerez yolu: son `/`'a kadar olan dizin.
fn default_path(path: &str) -> String {
    match path.rfind('/') {
        Some(0) | None => String::from("/"),
        Some(i) => String::from(&path[..i]),
    }
}

/// Bu isteğe gidecek çerezler ("a=1; b=2"); `script` true ise HttpOnly olanlar atlanır.
pub fn header_for(url: &str, script: bool) -> String {
    let Some(u) = Url::parse(url) else {
        return String::new();
    };
    let host = u.host.to_ascii_lowercase();
    let secure = u.scheme == "https";
    let path = if u.path.is_empty() { "/" } else { u.path.as_str() };
    let t = now();
    let mut matched: Vec<&Cookie> = jar()
        .iter()
        .filter(|c| {
            (if c.host_only { host == c.domain } else { domain_match(&host, &c.domain) })
                && path_match(path, &c.path)
                && (!c.secure || secure)
                && !(script && c.http_only)
                && c.expires.is_none_or(|e| e > t)
        })
        .collect();
    // Daha uzun yollu çerezler önce gelir.
    matched.sort_by_key(|c| core::cmp::Reverse(c.path.len()));
    let parts: Vec<String> = matched
        .iter()
        .map(|c| {
            if c.name.is_empty() {
                c.value.clone()
            } else {
                alloc::format!("{}={}", c.name, c.value)
            }
        })
        .collect();
    parts.join("; ")
}

/// `Set-Cookie` değerini (ya da `document.cookie = ...`) kavanoza yazar.
pub fn store(url: &str, header: &str, script: bool) {
    let Some(u) = Url::parse(url) else { return };
    let host = u.host.to_ascii_lowercase();
    let mut parts = header.split(';');
    let pair = parts.next().unwrap_or("").trim();
    let (name, value) = match pair.split_once('=') {
        Some((n, v)) => (n.trim(), v.trim()),
        None => ("", pair),
    };
    if name.is_empty() && value.is_empty() || value.len() > MAX_VALUE {
        return;
    }
    let mut cookie = Cookie {
        name: String::from(name),
        value: String::from(value.trim_matches('"')),
        domain: host.clone(),
        host_only: true,
        path: default_path(&u.path),
        expires: None,
        secure: false,
        http_only: false,
    };
    let mut max_age = None;
    let mut expires = None;
    for attr in parts {
        let (k, v) = match attr.split_once('=') {
            Some((k, v)) => (k.trim(), v.trim()),
            None => (attr.trim(), ""),
        };
        match k.to_ascii_lowercase().as_str() {
            "domain" if !v.is_empty() => {
                let d = v.trim_start_matches('.').to_ascii_lowercase();
                // Başka siteye ya da üst düzey alan adına çerez yazılamaz.
                if !domain_match(&host, &d) || !d.contains('.') {
                    return;
                }
                cookie.domain = d;
                cookie.host_only = false;
            }
            "path" if v.starts_with('/') => cookie.path = String::from(v),
            "max-age" => max_age = v.parse::<i64>().ok(),
            "expires" => expires = parse_http_date(v),
            "secure" => cookie.secure = true,
            "httponly" => cookie.http_only = true,
            _ => {}
        }
    }
    // Max-Age, Expires'tan önceliklidir; sıfır ya da geçmiş = sil.
    cookie.expires = match (max_age, expires) {
        (Some(age), _) => Some(now().saturating_add(age)),
        (None, Some(at)) => Some(at),
        _ => None,
    };
    if cookie.secure && u.scheme != "https" {
        return;
    }
    let jar = jar();
    if let Some(i) = jar
        .iter()
        .position(|c| c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path)
    {
        // Betik HttpOnly çerezin üzerine yazamaz.
        if script && jar[i].http_only {
            return;
        }
        let old = jar.remove(i);
        if old.expires.is_some() {
            mark_dirty();
        }
    }
    if script && cookie.http_only {
        return;
    }
    if cookie.expires.is_some_and(|e| e <= now()) {
        return;
    }
    if cookie.expires.is_some() {
        mark_dirty();
    }
    jar.push(cookie);
    if jar.len() > MAX_COOKIES {
        jar.remove(0);
    }
}

/// Yanıt başlıklarındaki bütün `Set-Cookie` satırları.
pub fn store_headers(url: &str, headers: &str) {
    for line in headers.lines() {
        if let Some((k, v)) = line.split_once(':')
            && k.trim().eq_ignore_ascii_case("set-cookie")
        {
            store(url, v.trim(), false);
        }
    }
}

/// Bir sitenin (alan adının) çerezlerini siler; boş dizgi = hepsi.
pub fn clear(domain: &str) {
    let jar = jar();
    let before = jar.len();
    jar.retain(|c| !domain.is_empty() && !domain_match(&c.domain, domain) && !domain_match(domain, &c.domain));
    if jar.len() != before {
        mark_dirty();
    }
}

pub fn count() -> usize {
    jar().len()
}

/// Kalıcı çerezler, satır başına bir tane (sekmeyle ayrılmış).
pub fn save() -> String {
    let t = now();
    let mut out = String::new();
    for c in jar().iter().filter(|c| c.expires.is_some_and(|e| e > t)) {
        out.push_str(&alloc::format!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\n",
            c.domain,
            c.host_only as u8,
            c.path,
            c.expires.unwrap_or(0),
            c.secure as u8,
            c.http_only as u8,
            c.name,
            c.value
        ));
    }
    out
}

pub fn load(text: &str) {
    let t = now();
    let jar = jar();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(8, '\t').collect();
        if f.len() != 8 {
            continue;
        }
        let Ok(expires) = f[3].parse::<i64>() else { continue };
        if expires <= t {
            continue;
        }
        let cookie = Cookie {
            domain: String::from(f[0]),
            host_only: f[1] == "1",
            path: String::from(f[2]),
            expires: Some(expires),
            secure: f[4] == "1",
            http_only: f[5] == "1",
            name: String::from(f[6]),
            value: String::from(f[7]),
        };
        jar.retain(|c| !(c.name == cookie.name && c.domain == cookie.domain && c.path == cookie.path));
        jar.push(cookie);
    }
}

/// "Wed, 21 Oct 2026 07:28:00 GMT" (ve "21-Oct-26" gibi eski biçimler) -> Unix saniyesi.
pub fn parse_http_date(text: &str) -> Option<i64> {
    const MONTHS: [&str; 12] = ["jan", "feb", "mar", "apr", "may", "jun", "jul", "aug", "sep", "oct", "nov", "dec"];
    let (mut day, mut month, mut year, mut time) = (None, None, None, None);
    for token in text.split([' ', ',', '-']).filter(|t| !t.is_empty()) {
        let lower = token.to_ascii_lowercase();
        if time.is_none() && token.contains(':') {
            let mut it = token.split(':').map(|p| p.parse::<i64>().ok());
            if let (Some(Some(h)), Some(Some(m)), s) = (it.next(), it.next(), it.next()) {
                time = Some(h * 3600 + m * 60 + s.flatten().unwrap_or(0));
            }
        } else if month.is_none() && lower.len() >= 3 && MONTHS.contains(&&lower[..3]) {
            month = MONTHS.iter().position(|m| *m == &lower[..3]).map(|i| i as i64 + 1);
        } else if let Ok(n) = token.parse::<i64>() {
            if day.is_none() && token.len() <= 2 && (1..=31).contains(&n) {
                day = Some(n);
            } else if year.is_none() {
                year = Some(match n {
                    0..=69 => n + 2000,
                    70..=99 => n + 1900,
                    _ => n,
                });
            }
        }
    }
    let (y, m, d) = (year?, month?, day?);
    // Gregoryen takvimden gün sayısı (Howard Hinnant, days_from_civil).
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + time.unwrap_or(0))
}
