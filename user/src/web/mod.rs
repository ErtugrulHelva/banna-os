//! Banna web motoru: HTML5 ayrıştırıcı, CSS, yerleşim, çizim ve JavaScript
//! yorumlayıcısı. Banna Search tarayıcısı ve yardım/belge görüntüleyicileri kullanır.

pub mod cookies;
pub mod css;
pub mod dom;
pub mod fmath;
pub mod html;
pub mod jpeg;
pub mod js;
pub mod layout;
pub mod page;
pub mod paint;
pub mod style;
pub mod url;

use alloc::string::String;
use alloc::vec::Vec;

pub const USER_AGENT: &str = "Mozilla/5.0 (BannaOS; x86_64; rv:128.0) Gecko/20100101 BannaSearch/1.0 Firefox/128.0";

pub struct Fetched {
    pub status: u16,
    pub url: String,
    pub content_type: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
}

/// En çok izlenen yönlendirme.
const MAX_REDIRECTS: usize = 10;

/// HTTP(S) isteği ya da `data:` adresi. Yönlendirmeler burada izlenir: her adımda çerezler
/// gönderilir ve `Set-Cookie`'ler saklanır (giriş formları çoğu zaman POST → 302 → GET yapar).
pub fn fetch(url: &str, method: &str, extra: &[(String, String)], body: &str) -> Result<Fetched, String> {
    if let Some(rest) = url.strip_prefix("data:") {
        let (meta, data) = rest.split_once(',').ok_or("geçersiz data: adresi")?;
        let body = if meta.ends_with(";base64") {
            base64(data)
        } else {
            js::builtins::uri_decode(data).into_bytes()
        };
        return Ok(Fetched {
            status: 200,
            url: String::from(url),
            content_type: String::from(meta.trim_end_matches(";base64")),
            body,
            headers: Vec::new(),
        });
    }
    let mut url = String::from(url);
    let mut method = String::from(method);
    let mut body = String::from(body);
    for _ in 0..=MAX_REDIRECTS {
        let cookie = cookies::header_for(&url, false);
        let mut headers: Vec<(&str, &str)> = alloc::vec![
            ("Accept-Language", "tr-TR,tr;q=0.9,en;q=0.8"),
            ("Accept", "text/html,application/xhtml+xml,application/json,*/*;q=0.8"),
        ];
        if !cookie.is_empty() {
            headers.push(("Cookie", &cookie));
        }
        for (k, v) in extra {
            if !body.is_empty() || !k.eq_ignore_ascii_case("content-type") {
                headers.push((k, v));
            }
        }
        if !body.is_empty() && !extra.iter().any(|(k, _)| k.eq_ignore_ascii_case("content-type")) {
            headers.push(("Content-Type", "application/x-www-form-urlencoded"));
        }
        let response = crate::net::request_once(&crate::net::Request {
            method: &method,
            url: &url,
            headers: &headers,
            body: body.as_bytes(),
        })?;
        cookies::store_headers(&url, &response.headers);
        if matches!(response.status, 301 | 302 | 303 | 307 | 308)
            && let Some(location) = response.header("location").filter(|l| !l.trim().is_empty())
        {
            let next = url::resolve(&url, location.trim());
            // 301/302/303: form gönderimi GET olarak sürer; 307/308 yöntemi ve gövdeyi korur.
            if response.status != 307 && response.status != 308 && method != "HEAD" {
                method = String::from("GET");
                body.clear();
            }
            url = next;
            continue;
        }
        let pairs = response
            .headers
            .lines()
            .skip(1)
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                Some((String::from(k.trim()), String::from(v.trim())))
            })
            .collect();
        return Ok(Fetched {
            status: response.status,
            content_type: String::from(response.header("content-type").unwrap_or("")),
            url,
            body: response.body,
            headers: pairs,
        });
    }
    Err(String::from("çok fazla yönlendirme"))
}

fn base64(s: &str) -> Vec<u8> {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut buf = 0u32;
    let mut bits = 0;
    for c in s.bytes() {
        let v = match T.iter().position(|&b| b == c) {
            Some(v) => v,
            None if c == b'-' => 62,
            None if c == b'_' => 63,
            None => continue,
        };
        buf = buf << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((buf >> bits) as u8);
        }
    }
    out
}

/// JavaScript `fetch`/`XMLHttpRequest` için metin gövdeli istek.
pub fn js_fetch(
    url: &str,
    method: &str,
    headers: &[(String, String)],
    body: &str,
) -> Result<(u16, String, Vec<(String, String)>), String> {
    let r = fetch(url, method, headers, body)?;
    Ok((r.status, String::from_utf8_lossy(&r.body).into_owned(), r.headers))
}

/// Görsel çözme: PNG ve JPEG (temel + aşamalı olmayan).
pub fn decode_image(data: &[u8]) -> Option<crate::gfx::png::Image> {
    if data.starts_with(b"\x89PNG") {
        crate::gfx::png::decode(data)
    } else if data.starts_with(&[0xff, 0xd8]) {
        jpeg::decode(data)
    } else {
        None
    }
}
