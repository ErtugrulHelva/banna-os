//! URL ayrıştırma ve göreli adres çözme (RFC 3986'nın tarayıcıda gereken kısmı).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug)]
pub struct Url {
    pub scheme: String,
    pub host: String,
    pub port: Option<u16>,
    pub path: String,
    pub query: Option<String>,
    pub fragment: Option<String>,
}

impl Url {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (scheme, rest) = text.split_once(':')?;
        if scheme.is_empty() || !scheme.chars().all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c)) {
            return None;
        }
        let scheme = scheme.to_ascii_lowercase();
        let (rest, fragment) = match rest.split_once('#') {
            Some((r, f)) => (r, Some(String::from(f))),
            None => (rest, None),
        };
        let (rest, query) = match rest.split_once('?') {
            Some((r, q)) => (r, Some(String::from(q))),
            None => (rest, None),
        };
        let (host, port, path) = if let Some(after) = rest.strip_prefix("//") {
            let (authority, path) = match after.find('/') {
                Some(i) => (&after[..i], &after[i..]),
                None => (after, "/"),
            };
            let authority = authority.rsplit('@').next().unwrap_or(authority);
            let (host, port) = match authority.rsplit_once(':') {
                Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => (h, p.parse().ok()),
                _ => (authority, None),
            };
            (host.to_ascii_lowercase(), port, String::from(path))
        } else {
            (String::new(), None, String::from(rest))
        };
        Some(Self {
            scheme,
            host,
            port,
            path,
            query,
            fragment,
        })
    }

    pub fn host_port(&self) -> String {
        match self.port {
            Some(p) => alloc::format!("{}:{}", self.host, p),
            None => self.host.clone(),
        }
    }

    pub fn origin(&self) -> String {
        alloc::format!("{}://{}", self.scheme, self.host_port())
    }

    pub fn to_string(&self) -> String {
        let mut s = if self.host.is_empty() && self.scheme != "file" {
            alloc::format!("{}:{}", self.scheme, self.path)
        } else {
            alloc::format!("{}://{}{}", self.scheme, self.host_port(), self.path)
        };
        if let Some(q) = &self.query {
            s.push('?');
            s.push_str(q);
        }
        if let Some(f) = &self.fragment {
            s.push('#');
            s.push_str(f);
        }
        s
    }
}

fn normalize_path(path: &str) -> String {
    let mut out: Vec<&str> = Vec::new();
    let segments: Vec<&str> = path.split('/').collect();
    for (i, seg) in segments.iter().enumerate() {
        match *seg {
            "." => {
                if i == segments.len() - 1 {
                    out.push("");
                }
            }
            ".." => {
                if out.len() > 1 {
                    out.pop();
                }
                if i == segments.len() - 1 {
                    out.push("");
                }
            }
            s => out.push(s),
        }
    }
    let joined = out.join("/");
    if joined.starts_with('/') { joined } else { alloc::format!("/{}", joined) }
}

/// `href`'i `base`'e göre mutlak adrese çevirir.
pub fn resolve(base: &str, href: &str) -> String {
    let href = href.trim();
    if href.is_empty() {
        return String::from(base.split('#').next().unwrap_or(base));
    }
    if let Some(u) = Url::parse(href)
        && (u.scheme.len() > 1 || !href.contains('\\'))
        && !href.starts_with("//")
        && matches!(u.scheme.as_str(), "http" | "https" | "file" | "data" | "javascript" | "mailto" | "banna" | "about" | "blob" | "tel") {
            return u.to_string();
        }
    let Some(b) = Url::parse(base) else {
        return String::from(href);
    };
    if let Some(rest) = href.strip_prefix("//") {
        return alloc::format!("{}://{}", b.scheme, rest);
    }
    let mut u = b.clone();
    u.fragment = None;
    if let Some(f) = href.strip_prefix('#') {
        u.fragment = Some(String::from(f));
        return u.to_string();
    }
    if let Some(q) = href.strip_prefix('?') {
        let (q, f) = match q.split_once('#') {
            Some((q, f)) => (q, Some(String::from(f))),
            None => (q, None),
        };
        u.query = Some(String::from(q));
        u.fragment = f;
        return u.to_string();
    }
    let (path_part, fragment) = match href.split_once('#') {
        Some((p, f)) => (p, Some(String::from(f))),
        None => (href, None),
    };
    let (path_part, query) = match path_part.split_once('?') {
        Some((p, q)) => (p, Some(String::from(q))),
        None => (path_part, None),
    };
    let path = if path_part.starts_with('/') {
        String::from(path_part)
    } else {
        let dir = match b.path.rfind('/') {
            Some(i) => &b.path[..=i],
            None => "/",
        };
        alloc::format!("{}{}", dir, path_part)
    };
    u.path = normalize_path(&path);
    u.query = query;
    u.fragment = fragment;
    u.to_string()
}
