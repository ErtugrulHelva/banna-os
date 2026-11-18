//! Masaüstüyle konuşma: ayarları ve canlı sistem bilgisini okuma, ayar değiştirme.
//!
//! `query()` çekirdeğin verdiği `anahtar=değer` satırlarını bir sözlüğe çevirir; `set`
//! tek bir ayarı (ör. `dark`, `1`) ya da eylemi (`action`, `test_sound`) gönderir.

use alloc::collections::BTreeMap;

use crate::sys;
use crate::{String, Vec};

pub struct Info {
    values: BTreeMap<String, String>,
}

impl Info {
    pub fn get(&self, key: &str) -> &str {
        self.values.get(key).map(|s| s.as_str()).unwrap_or("")
    }

    pub fn num(&self, key: &str) -> i64 {
        self.get(key).parse().unwrap_or(0)
    }

    /// Boş ise `None` (ör. okunamayan sıcaklık).
    pub fn opt(&self, key: &str) -> Option<i64> {
        self.get(key).parse().ok()
    }

    pub fn flag(&self, key: &str) -> bool {
        self.get(key) == "1"
    }

    /// Ham `anahtar=değer` metni (tercih çözümleyicisi için).
    pub fn text(&self) -> String {
        let mut out = String::new();
        for (k, v) in &self.values {
            out.push_str(k);
            out.push('=');
            out.push_str(v);
            out.push('\n');
        }
        out
    }
}

/// Masaüstü çalışmıyorsa `None`.
pub fn query() -> Option<Info> {
    let mut buf: Vec<u8> = crate::vec![0u8; 8192];
    loop {
        let n = sys::call(sys::INFO, &[2, buf.as_mut_ptr() as u64, buf.len() as u64]);
        if n < 0 {
            return None;
        }
        if (n as usize) <= buf.len() {
            buf.truncate(n as usize);
            break;
        }
        buf.resize(n as usize, 0);
    }
    let text = String::from_utf8_lossy(&buf);
    let values = text
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_once('='))
        .map(|(k, v)| (String::from(k), String::from(v)))
        .collect();
    Some(Info { values })
}

/// Ayar ya da eylem; başarıda 0.
pub fn set(key: &str, value: &str) -> i64 {
    sys::call(
        sys::DESKTOP_SET,
        &[
            key.as_ptr() as u64,
            key.len() as u64,
            value.as_ptr() as u64,
            value.len() as u64,
        ],
    )
}
