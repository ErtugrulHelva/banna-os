//! Banna Ekosistemi istemcisi (`https://ecosys.trybanna.com/api`): E-Banna oturumu,
//! JSON istekleri ve mağaza katalogları. Banna Panel ve Banna Store bunu kullanır.
//!
//! Oturum, sunucunun verdiği `banna_session` ve `banna_device` çerezleridir; kullanıcının
//! ev dizininde `~/.config/banna/session` dosyasında saklanır.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::json::{self, Value};
use crate::{fs, net, process};

pub const BASE: &str = "https://ecosys.trybanna.com/api";
pub const CLIENT_NAME: &str = "Banna Panel (Banna OS)";
/// Banna OS paket deposu (hatch).
pub const HATCH_REPO: &str = "https://ecosys.trybanna.com/hatch/";
/// Banna Notify (bildirim servisi).
pub const NOTIFY: &str = "https://ecosys.trybanna.com/notify";

#[derive(Debug, Clone)]
pub struct ApiError {
    pub status: u16,
    pub code: String,
    pub message: String,
}

impl ApiError {
    fn network(message: String) -> Self {
        Self {
            status: 0,
            code: String::from("NETWORK_ERROR"),
            message,
        }
    }

    /// Kullanıcıya gösterilecek Türkçe ileti.
    pub fn text(&self) -> String {
        let known = match self.code.as_str() {
            "INVALID_CREDENTIALS" => "E-Banna adresi veya şifre hatalı.",
            "ACCOUNT_LOCKED" => "Çok fazla başarısız deneme. Birkaç dakika sonra tekrar deneyin.",
            "UNKNOWN_DEVICE" => "Bu cihaz daha önce kullanılmamış.",
            "UNAUTHORIZED" | "UNAUTHENTICATED" => "Oturum süresi doldu, yeniden giriş yapın.",
            "NETWORK_ERROR" => "Banna sunucularına ulaşılamıyor. Ağ bağlantısını kontrol edin.",
            _ => "",
        };
        if !known.is_empty() {
            String::from(known)
        } else if !self.message.is_empty() {
            self.message.clone()
        } else {
            format!("Sunucu hatası ({}).", self.status)
        }
    }
}

pub type Result<T> = core::result::Result<T, ApiError>;

fn session_path() -> String {
    format!("/home/{}/.config/banna/session", process::username())
}

pub struct Client {
    cookies: Vec<(String, String)>,
}

impl Default for Client {
    fn default() -> Self {
        Self::load()
    }
}

impl Client {
    /// Kayıtlı oturumla (varsa) istemci.
    pub fn load() -> Self {
        let cookies = fs::read(&session_path())
            .ok()
            .map(|data| {
                String::from_utf8_lossy(&data)
                    .lines()
                    .filter_map(|l| l.split_once('='))
                    .map(|(k, v)| (String::from(k), String::from(v)))
                    .collect()
            })
            .unwrap_or_default();
        Self { cookies }
    }

    fn save(&self) {
        let home = format!("/home/{}", process::username());
        let _ = fs::mkdir(&format!("{}/.config", home));
        let _ = fs::mkdir(&format!("{}/.config/banna", home));
        let text: String = self
            .cookies
            .iter()
            .map(|(k, v)| format!("{}={}\n", k, v))
            .collect();
        let path = session_path();
        if fs::write(&path, text.as_bytes()).is_ok() {
            let _ = fs::chmod(&path, 0o600);
        }
    }

    pub fn has_session(&self) -> bool {
        self.cookies
            .iter()
            .any(|(k, v)| k == "banna_session" && !v.is_empty())
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{}={}", k, v))
            .collect::<Vec<_>>()
            .join("; ")
    }

    /// multipart/form-data ile metin alanları gönderir (ör. Banna Mail `/mail/send`).
    pub fn post_form(&mut self, path: &str, fields: &[(&str, &str)]) -> Result<Value> {
        self.post_multipart(path, fields, &[])
    }

    /// multipart/form-data: metin alanları ve `file` parçaları (dosya adı, içerik).
    pub fn post_multipart(
        &mut self,
        path: &str,
        fields: &[(&str, &str)],
        files: &[(&str, &[u8])],
    ) -> Result<Value> {
        let boundary = "----BannaOSFormBoundary7d1f";
        let mut body = Vec::new();
        for (name, value) in fields {
            body.extend_from_slice(
                format!(
                    "--{}\r\nContent-Disposition: form-data; name=\"{}\"\r\n\r\n{}\r\n",
                    boundary, name, value
                )
                .as_bytes(),
            );
        }
        for (filename, data) in files {
            body.extend_from_slice(
                format!(
                    "--{}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{}\"\r\nContent-Type: {}\r\n\r\n",
                    boundary,
                    filename.replace('"', "'"),
                    mime_type(filename)
                )
                .as_bytes(),
            );
            body.extend_from_slice(data);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{}--\r\n", boundary).as_bytes());
        let content_type = format!("multipart/form-data; boundary={}", boundary);
        self.send("POST", path, Some((&content_type, &body)))
    }

    /// Ham yanıt gövdesi (ör. Banna Mail eki `/mail/attachments/<id>`).
    pub fn download(&mut self, path: &str) -> Result<Vec<u8>> {
        let response = self.exchange("GET", path, None)?;
        if response.status >= 400 {
            return Err(api_error(&response));
        }
        Ok(response.body)
    }

    /// API isteği; gövde JSON metni. Yanıt JSON olarak çözülür.
    pub fn call(&mut self, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
        self.send(
            method,
            path,
            body.map(|b| ("application/json", b.as_bytes())),
        )
    }

    fn exchange(
        &mut self,
        method: &str,
        path: &str,
        body: Option<(&str, &[u8])>,
    ) -> Result<net::Response> {
        let url = format!("{}{}", BASE, path);
        let cookie = self.cookie_header();
        let mut headers: Vec<(&str, &str)> = alloc::vec![("Accept", "application/json")];
        if !cookie.is_empty() {
            headers.push(("Cookie", &cookie));
        }
        if let Some((content_type, _)) = body {
            headers.push(("Content-Type", content_type));
        }
        let response = net::request(&net::Request {
            method,
            url: &url,
            headers: &headers,
            body: body.map(|(_, b)| b).unwrap_or(&[]),
        })
        .map_err(ApiError::network)?;
        let mut changed = false;
        for (name, value) in response.cookies() {
            changed = true;
            self.cookies.retain(|(k, _)| *k != name);
            if !value.is_empty() {
                self.cookies.push((name, value));
            }
        }
        if response.status == 401 {
            changed = true;
            self.cookies.retain(|(k, _)| k != "banna_session");
        }
        if changed {
            self.save();
        }
        Ok(response)
    }

    fn send(&mut self, method: &str, path: &str, body: Option<(&str, &[u8])>) -> Result<Value> {
        let response = self.exchange(method, path, body)?;
        if response.status >= 400 {
            return Err(api_error(&response));
        }
        Ok(json::parse(&response.body).unwrap_or(Value::Null))
    }

    pub fn get(&mut self, path: &str) -> Result<Value> {
        self.call("GET", path, None)
    }

    pub fn post(&mut self, path: &str, body: &str) -> Result<Value> {
        self.call("POST", path, Some(body))
    }

    pub fn patch(&mut self, path: &str, body: &str) -> Result<Value> {
        self.call("PATCH", path, Some(body))
    }

    pub fn delete(&mut self, path: &str) -> Result<Value> {
        self.call("DELETE", path, None)
    }

    /// Giriş: önce bilinen cihaz olarak dener; cihaz tanınmazsa yeni cihaz girişine geçer.
    pub fn login(&mut self, identifier: &str, password: &str) -> Result<Login> {
        let body = json::object(&[
            ("ebannaAddress", identifier),
            ("password", password),
            ("clientName", CLIENT_NAME),
        ]);
        match self.post("/auth/login", &body) {
            Ok(_) => return Ok(Login::Done),
            Err(e) if e.code == "UNKNOWN_DEVICE" => {}
            Err(e) => return Err(e),
        }
        let body = json::object(&[
            ("identifier", identifier),
            ("password", password),
            ("clientName", CLIENT_NAME),
        ]);
        let res = self.post("/auth/login/new-device", &body)?;
        if res.get("status").as_str() == "PENDING_APPROVAL" {
            return Ok(Login::Pending {
                approval_id: String::from(res.get("approvalId").as_str()),
                code: String::from(res.get("fallbackCode").as_str()),
            });
        }
        Ok(Login::Done)
    }

    /// Yeni cihaz onayının durumu: APPROVED, DENIED, EXPIRED ya da PENDING.
    pub fn approval_status(&mut self, approval_id: &str) -> Result<String> {
        let res = self.get(&format!("/auth/login/approval/{}", approval_id))?;
        Ok(String::from(res.get("status").as_str()))
    }

    pub fn logout(&mut self) {
        let _ = self.post("/auth/logout", "{}");
        self.cookies.retain(|(k, _)| k != "banna_session");
        self.save();
    }
}

fn api_error(response: &net::Response) -> ApiError {
    let value = json::parse(&response.body).unwrap_or(Value::Null);
    ApiError {
        status: response.status,
        code: String::from(value.get("error").as_str()),
        message: String::from(value.get("message").as_str()),
    }
}

fn mime_type(filename: &str) -> &'static str {
    let ext = filename
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "txt" | "md" | "conf" | "ini" | "rs" => "text/plain",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "bmp" => "image/bmp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "wav" => "audio/wav",
        _ => "application/octet-stream",
    }
}

/// Banna Notify REST çağrısı (`/v1/...`); belirteç `POST /notify/token` ile alınır.
pub fn notify_request(token: &str, method: &str, path: &str, body: Option<&str>) -> Result<Value> {
    let url = format!("{}{}", NOTIFY, path);
    let auth = format!("Bearer {}", token);
    let mut headers: Vec<(&str, &str)> =
        alloc::vec![("Accept", "application/json"), ("Authorization", &auth)];
    if body.is_some() {
        headers.push(("Content-Type", "application/json"));
    }
    let response = net::request(&net::Request {
        method,
        url: &url,
        headers: &headers,
        body: body.map(str::as_bytes).unwrap_or(&[]),
    })
    .map_err(ApiError::network)?;
    if response.status >= 400 {
        return Err(api_error(&response));
    }
    Ok(json::parse(&response.body).unwrap_or(Value::Null))
}

pub enum Login {
    Done,
    /// Hesabın onay cihazından onay bekleniyor; `code` diğer cihazda gösterilen kod.
    Pending {
        approval_id: String,
        code: String,
    },
}

/// Ekosistemin Banna Store kataloğu (Banna'nın mobil uygulamaları; giriş gerekmez). Öğede
/// `platforms` dizisi varsa uygulamanın çalıştığı platformlar oradadır, yoksa Android'dir.
pub fn mobile_catalog() -> Result<Vec<Value>> {
    let response = net::get(&format!("{}/banna-store/apps", BASE)).map_err(ApiError::network)?;
    let value = json::parse(&response.body).unwrap_or(Value::Null);
    Ok(value.get("apps").items().to_vec())
}

/// Banna Store platform panelleri (`/banna-store/platforms`): her öğede `id` (banna-os, android,
/// ios, windows, macos, linux) ve `apps` (id, name, tagline, version, size, iconUrl, icon, detail,
/// install, downloadUrl). Sunucuda uç nokta yoksa (eski sürüm) `None`.
pub fn store_platforms() -> Result<Option<Vec<Value>>> {
    let response =
        net::get(&format!("{}/banna-store/platforms", BASE)).map_err(ApiError::network)?;
    if response.status != 200 {
        return Ok(None);
    }
    Ok(json::parse(&response.body).map(|v| v.get("platforms").items().to_vec()))
}

/// Banna Panel'in masaüstü sürümü (`target`: darwin, windows, linux; `arch`: aarch64,
/// x86_64). Yanıtta `version`, `notes`, `url` bulunur; o platform için yayın yoksa `None`.
pub fn desktop_release(target: &str, arch: &str) -> Result<Option<Value>> {
    let url = format!("{}/updates/banna-panel/{}/{}/0.0.0", BASE, target, arch);
    let response = net::get(&url).map_err(ApiError::network)?;
    if response.status != 200 {
        return Ok(None);
    }
    Ok(json::parse(&response.body).filter(|v| !v.get("version").as_str().is_empty()))
}

/// Banna deposunun imza anahtarı (RSA-2048, açık kısım). `index`, `index.sig` ile
/// (PKCS#1 v1.5, SHA-256) imzalanır; özel anahtar banna_os/keys/hatch-signing.pem.
const REPOSITORY_KEY: [&str; 8] = [
    "db40654996fd5224e158739526419fd41d1d0f5b07ec1bf21638886fb7a96ba8",
    "4c086072d8a717f45910b7ca5807bb65eb4a4e7c53df979be58a525e2b3ecf60",
    "077a3aaa5f274a46407f001ce3cb08966f34542eb7919773e28693d61f52c2ef",
    "bc46472f397a6491154c72a90bc700925a3ffc87a8c87bd3803c055871f78d0d",
    "c9f5b79bcdc03c52d47f388941c050af033d4e75caa7adc233911fe9fc08bd91",
    "7f8cd620926689cb18100920841fd89f36c9c584ec8bb90242d9ede9c1dc94c2",
    "f82031b0c9bffffd5738c6fcedfb9db6704924c94d555eafd4c2967f8c8a767d",
    "642c90973b4c291f04faed992b4042070e4f19bcad3dff7d206e06b6c0a5a269",
];

/// Paket listesinin imzasını doğrular.
pub fn verify_repository(index: &[u8], signature: &[u8]) -> bool {
    let hex: String = REPOSITORY_KEY.concat();
    let Some(n) = crate::crypto::from_hex(&hex) else {
        return false;
    };
    let digest = crate::crypto::sha256(index);
    crate::tls::bigint::rsa_verify(&n, &[1, 0, 1], crate::tls::Hash::Sha256, &digest, signature)
}
