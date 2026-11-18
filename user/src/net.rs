//! TCP istemci/sunucu soketleri ve DNS çözümleme.

use crate::sys;
use crate::{String, Vec};

pub const EAGAIN: i64 = -11;

pub fn error_text(code: i64) -> &'static str {
    match -code {
        2 => "host not found",
        4 => "interrupted",
        9 => "bad socket",
        11 => "timed out (no data)",
        98 => "address in use",
        101 => "network unreachable",
        104 => "connection reset by peer",
        107 => "not connected",
        110 => "connection timed out",
        111 => "connection refused",
        _ => "network error",
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Ipv4(pub [u8; 4]);

impl core::fmt::Display for Ipv4 {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        let [a, b, c, d] = self.0;
        write!(f, "{}.{}.{}.{}", a, b, c, d)
    }
}

/// Alan adını ya da noktalı IPv4 metnini adrese çevirir.
pub fn resolve(host: &str) -> Result<Ipv4, i64> {
    let r = sys::call(sys::RESOLVE, &[host.as_ptr() as u64, host.len() as u64]);
    if r < 0 {
        Err(r)
    } else {
        Ok(Ipv4((r as u32).to_be_bytes()))
    }
}

#[derive(Clone, Copy, Debug)]
pub struct NetInfo {
    pub address: Ipv4,
    pub netmask: Ipv4,
    pub gateway: Ipv4,
    pub dns: Ipv4,
}

/// DHCP ile alınan adres bilgisi; ağ yoksa `None`.
pub fn netinfo() -> Option<NetInfo> {
    let mut raw = [0u32; 4];
    if sys::call(sys::NETINFO, &[raw.as_mut_ptr() as u64]) < 0 {
        return None;
    }
    let ip = |v: u32| Ipv4(v.to_be_bytes());
    Some(NetInfo {
        address: ip(raw[0]),
        netmask: ip(raw[1]),
        gateway: ip(raw[2]),
        dns: ip(raw[3]),
    })
}

pub struct TcpStream {
    fd: u64,
}

impl TcpStream {
    pub fn connect(address: Ipv4, port: u16, timeout_ms: u64) -> Result<Self, i64> {
        let ip = u32::from_be_bytes(address.0) as u64;
        let fd = sys::call(sys::CONNECT, &[ip, port as u64, timeout_ms]);
        if fd < 0 {
            Err(fd)
        } else {
            Ok(Self { fd: fd as u64 })
        }
    }

    pub fn connect_host(host: &str, port: u16) -> Result<Self, i64> {
        Self::connect(resolve(host)?, port, 10_000)
    }

    /// En fazla `buf.len()` bayt okur; 0 = karşı taraf kapattı.
    /// `timeout_ms` 0 ise veri gelene kadar bekler.
    pub fn read_timeout(&mut self, buf: &mut [u8], timeout_ms: u64) -> Result<usize, i64> {
        let n = sys::call(
            sys::RECV,
            &[
                self.fd,
                buf.as_mut_ptr() as u64,
                buf.len() as u64,
                timeout_ms,
            ],
        );
        if n < 0 { Err(n) } else { Ok(n as usize) }
    }

    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize, i64> {
        self.read_timeout(buf, 0)
    }

    pub fn write_all(&mut self, mut data: &[u8]) -> Result<(), i64> {
        while !data.is_empty() {
            let n = crate::io::write_fd(self.fd, data);
            if n <= 0 {
                return Err(if n == 0 { -107 } else { n });
            }
            data = &data[n as usize..];
        }
        Ok(())
    }

    /// Bağlantı kapanana kadar her şeyi okur.
    pub fn read_to_end(&mut self, out: &mut Vec<u8>, timeout_ms: u64) -> Result<(), i64> {
        let mut buf = [0u8; 8192];
        loop {
            match self.read_timeout(&mut buf, timeout_ms)? {
                0 => return Ok(()),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    pub fn fd(&self) -> u64 {
        self.fd
    }
}

impl Drop for TcpStream {
    fn drop(&mut self) {
        sys::call(sys::CLOSE, &[self.fd]);
    }
}

pub struct TcpListener {
    fd: u64,
}

impl TcpListener {
    pub fn bind(port: u16) -> Result<Self, i64> {
        let fd = sys::call(sys::LISTEN, &[port as u64]);
        if fd < 0 {
            Err(fd)
        } else {
            Ok(Self { fd: fd as u64 })
        }
    }

    /// `timeout_ms` 0 ise bağlantı gelene kadar bekler.
    pub fn accept(&self, timeout_ms: u64) -> Result<TcpStream, i64> {
        let fd = sys::call(sys::ACCEPT, &[self.fd, timeout_ms]);
        if fd < 0 {
            Err(fd)
        } else {
            Ok(TcpStream { fd: fd as u64 })
        }
    }
}

impl Drop for TcpListener {
    fn drop(&mut self) {
        sys::call(sys::CLOSE, &[self.fd]);
    }
}

pub struct Url<'a> {
    pub https: bool,
    pub host: &'a str,
    pub port: u16,
    pub path: &'a str,
}

/// `http://` ya da `https://host[:port][/path]` çözümler (şema yoksa http).
pub fn parse_url(url: &str) -> Option<Url<'_>> {
    let (https, rest) = match url.strip_prefix("https://") {
        Some(r) => (true, r),
        None => (false, url.strip_prefix("http://").unwrap_or(url)),
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h, p.parse().ok()?),
        None => (authority, if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some(Url {
        https,
        host,
        port,
        path,
    })
}

pub struct Response {
    pub status: u16,
    pub headers: String,
    pub body: Vec<u8>,
    /// Yönlendirmelerden sonraki son adres.
    pub url: String,
}

impl Response {
    /// İlk eşleşen başlığın değeri (büyük/küçük harf duyarsız).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            k.trim().eq_ignore_ascii_case(name).then(|| v.trim())
        })
    }

    /// `Set-Cookie` başlıklarının `ad=değer` kısımları.
    pub fn cookies(&self) -> Vec<(String, String)> {
        self.headers
            .lines()
            .filter_map(|l| {
                let (k, v) = l.split_once(':')?;
                if !k.trim().eq_ignore_ascii_case("set-cookie") {
                    return None;
                }
                let pair = v.trim().split(';').next()?;
                let (name, value) = pair.split_once('=')?;
                Some((String::from(name.trim()), String::from(value.trim())))
            })
            .collect()
    }
}

impl crate::tls::Transport for TcpStream {
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, i64> {
        self.read_timeout(buf, 30_000)
    }

    fn send(&mut self, data: &[u8]) -> Result<(), i64> {
        self.write_all(data)
    }
}

/// Çekirdekten rastgele baytlar.
pub fn random(buf: &mut [u8]) {
    for chunk in buf.chunks_mut(4096) {
        sys::call(
            sys::RANDOM,
            &[chunk.as_mut_ptr() as u64, chunk.len() as u64],
        );
    }
}

pub struct Request<'a> {
    pub method: &'a str,
    pub url: &'a str,
    pub headers: &'a [(&'a str, &'a str)],
    pub body: &'a [u8],
}

/// Açık bağlantı: düz TCP ya da TLS.
enum Conn {
    Plain(TcpStream),
    Tls(alloc::boxed::Box<crate::tls::TlsStream<TcpStream>>),
}

impl Conn {
    fn write_all(&mut self, data: &[u8]) -> Result<(), String> {
        match self {
            Conn::Plain(s) => s.write_all(data).map_err(|e| String::from(error_text(e))),
            Conn::Tls(s) => s.write_all(data).map_err(|e| String::from(e.message())),
        }
    }

    /// 0 = bağlantı kapandı.
    fn read(&mut self, buf: &mut [u8]) -> Result<usize, String> {
        match self {
            Conn::Plain(s) => s.read_timeout(buf, 30_000).map_err(|e| String::from(error_text(e))),
            Conn::Tls(s) => s.read(buf).map_err(|e| String::from(e.message())),
        }
    }
}

/// Keep-alive bağlantı havuzu: aynı sunucuya giden istekler TCP ve TLS el sıkışmasını
/// yeniden yapmaz (bir el sıkışma uzak sunucularda ~3 gidiş-dönüş, yüzlerce ms sürer).
struct Pooled {
    host: String,
    port: u16,
    https: bool,
    conn: Conn,
    idle_since: u64,
}

/// Bu süreden uzun boşta kalan bağlantı kullanılmaz (sunucular genelde 5-60 sn'de kapatır).
const POOL_IDLE_MS: u64 = 10_000;
const POOL_MAX: usize = 6;

static mut POOL: Vec<Pooled> = Vec::new();

fn pool() -> &'static mut Vec<Pooled> {
    unsafe { &mut *core::ptr::addr_of_mut!(POOL) }
}

fn take_pooled(u: &Url) -> Option<Conn> {
    let now = crate::time::uptime_ms();
    pool().retain(|p| now.saturating_sub(p.idle_since) < POOL_IDLE_MS);
    let at = pool()
        .iter()
        .position(|p| p.host == u.host && p.port == u.port && p.https == u.https)?;
    Some(pool().swap_remove(at).conn)
}

fn put_pooled(u: &Url, conn: Conn) {
    let list = pool();
    if list.len() >= POOL_MAX {
        list.remove(0);
    }
    list.push(Pooled {
        host: String::from(u.host),
        port: u.port,
        https: u.https,
        conn,
        idle_since: crate::time::uptime_ms(),
    });
}

/// Açık bağlantıları kapatır (ör. ağ değiştiğinde).
pub fn close_idle_connections() {
    pool().clear();
}

fn open(u: &Url) -> Result<Conn, String> {
    let address = resolve(u.host).map_err(|e| crate::format!("{}: {}", u.host, error_text(e)))?;
    let stream = TcpStream::connect(address, u.port, 10_000)
        .map_err(|e| crate::format!("{}: {}", u.host, error_text(e)))?;
    if !u.https {
        return Ok(Conn::Plain(stream));
    }
    let tls = crate::tls::connect(stream, u.host, crate::time::unix_time(), &mut random)
        .map_err(|e| crate::format!("{}: TLS: {}", u.host, e.message()))?;
    Ok(Conn::Tls(alloc::boxed::Box::new(tls)))
}

/// Chunked gövdenin tamamı geldiyse (son 0 boyutlu parça + boş satır) uzunluğu.
fn chunked_len(data: &[u8]) -> Option<usize> {
    let mut at = 0;
    loop {
        let line_end = data[at..].windows(2).position(|w| w == b"\r\n")? + at;
        let size_text = core::str::from_utf8(&data[at..line_end]).ok()?;
        let size = usize::from_str_radix(size_text.split(';').next()?.trim(), 16).ok()?;
        at = line_end + 2;
        if size == 0 {
            // Sondaki başlıklar (trailer) ve boş satır.
            loop {
                let end = data[at..].windows(2).position(|w| w == b"\r\n")? + at;
                let empty = end == at;
                at = end + 2;
                if empty {
                    return Some(at);
                }
            }
        }
        at += size + 2;
        if at > data.len() {
            return None;
        }
    }
}

/// Tek istek-yanıt; `Ok((yanıt, bağlantı yeniden kullanılabilir mi))`. Hiç bayt
/// gelmeden bağlantı kapanırsa `Err(None)` (havuzdaki bağlantı bayatlamış: yeniden dene).
fn transact(conn: &mut Conn, data: &[u8], head_only: bool) -> Result<(Response, bool), Option<String>> {
    conn.write_all(data).map_err(|_| None)?;
    let mut raw = Vec::new();
    let mut buf = alloc::vec![0u8; 16 * 1024];
    let split = loop {
        if let Some(i) = raw.windows(4).position(|w| w == b"\r\n\r\n") {
            break i;
        }
        match conn.read(&mut buf) {
            Ok(0) | Err(_) if raw.is_empty() => return Err(None),
            Ok(0) => return Err(Some(String::from("geçersiz HTTP yanıtı"))),
            Ok(n) => raw.extend_from_slice(&buf[..n]),
            Err(e) => return Err(Some(e)),
        }
    };
    let headers = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut body = raw.split_off(split + 4);
    let status: u16 = headers.split(' ').nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let lower = headers.to_ascii_lowercase();
    let header = |name: &str| {
        lower.lines().skip(1).find_map(|l| {
            let (k, v) = l.split_once(':')?;
            (k.trim() == name).then(|| String::from(v.trim()))
        })
    };
    let chunked = header("transfer-encoding").is_some_and(|v| v.contains("chunked"));
    let length: Option<usize> = header("content-length").and_then(|v| v.parse().ok());
    let http11 = headers.starts_with("HTTP/1.1");
    let connection = header("connection").unwrap_or_default();
    let mut reusable = if http11 { !connection.contains("close") } else { connection.contains("keep-alive") };
    let no_body = head_only || status == 204 || status == 304 || (100..200).contains(&status);
    if no_body {
        body.clear();
    } else if chunked {
        loop {
            if let Some(n) = chunked_len(&body) {
                body.truncate(n);
                break;
            }
            match conn.read(&mut buf).map_err(Some)? {
                0 => {
                    reusable = false;
                    break;
                }
                n => body.extend_from_slice(&buf[..n]),
            }
        }
        body = dechunk(&body);
    } else if let Some(n) = length {
        while body.len() < n {
            match conn.read(&mut buf).map_err(Some)? {
                0 => {
                    reusable = false;
                    break;
                }
                k => body.extend_from_slice(&buf[..k]),
            }
        }
        body.truncate(n);
    } else {
        // Uzunluk yok: gövde bağlantı kapanana kadar sürer.
        reusable = false;
        loop {
            match conn.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => body.extend_from_slice(&buf[..n]),
            }
        }
    }
    Ok((
        Response {
            status,
            headers,
            body,
            url: String::new(),
        },
        reusable,
    ))
}

fn exchange(req: &Request, u: &Url) -> Result<Response, String> {
    let mut head = crate::format!("{} {} HTTP/1.1\r\nHost: {}\r\n", req.method, u.path, u.host);
    // Varsayılan başlıklar, çağıran kendisi vermediyse (tarayıcı Accept'i kendisi gönderir).
    let given = |name: &str| req.headers.iter().any(|(k, _)| k.eq_ignore_ascii_case(name));
    if !given("user-agent") {
        head.push_str(&crate::format!("User-Agent: BannaOS/{}\r\n", env!("CARGO_PKG_VERSION")));
    }
    if !given("accept") {
        head.push_str("Accept: */*\r\n");
    }
    head.push_str("Connection: keep-alive\r\n");
    for (k, v) in req.headers {
        head.push_str(&crate::format!("{}: {}\r\n", k, v));
    }
    if !req.body.is_empty() || req.method != "GET" {
        head.push_str(&crate::format!("Content-Length: {}\r\n", req.body.len()));
    }
    head.push_str("\r\n");
    let mut data = head.into_bytes();
    data.extend_from_slice(req.body);
    let head_only = req.method == "HEAD";

    // Önce havuzdaki bağlantı; sunucu onu kapatmışsa yeni bağlantıyla bir kez daha.
    if let Some(mut conn) = take_pooled(u) {
        match transact(&mut conn, &data, head_only) {
            Ok((response, reusable)) => {
                if reusable {
                    put_pooled(u, conn);
                }
                return Ok(response);
            }
            Err(Some(e)) => return Err(e),
            Err(None) => {}
        }
    }
    let mut conn = open(u)?;
    match transact(&mut conn, &data, head_only) {
        Ok((response, reusable)) => {
            if reusable {
                put_pooled(u, conn);
            }
            Ok(response)
        }
        Err(e) => Err(e.unwrap_or_else(|| crate::format!("{}: bağlantı kapandı", u.host))),
    }
}

/// Tek HTTP/1.1 isteği: yönlendirme izlenmez (tarayıcı her adımda çerezleri kendisi yönetir).
pub fn request_once(req: &Request) -> Result<Response, String> {
    let u = parse_url(req.url).ok_or_else(|| crate::format!("geçersiz adres: {}", req.url))?;
    let mut response = exchange(req, &u)?;
    response.url = String::from(req.url);
    Ok(response)
}

/// HTTP/1.1 isteği (http ve https); GET yönlendirmeleri en fazla 5 kez izlenir.
pub fn request(req: &Request) -> Result<Response, String> {
    let mut url = String::from(req.url);
    for _ in 0..6 {
        let u = parse_url(&url).ok_or_else(|| crate::format!("geçersiz adres: {}", url))?;
        let mut response = exchange(req, &u)?;
        response.url = url.clone();
        let redirect =
            matches!(response.status, 301 | 302 | 303 | 307 | 308) && req.method == "GET";
        match response.header("location").filter(|_| redirect) {
            Some(location) if location.starts_with("http") => url = String::from(location),
            Some(location) => {
                url = crate::format!(
                    "{}://{}:{}{}",
                    if u.https { "https" } else { "http" },
                    u.host,
                    u.port,
                    location
                )
            }
            None => return Ok(response),
        }
    }
    Err(String::from("çok fazla yönlendirme"))
}

pub fn get(url: &str) -> Result<Response, String> {
    request(&Request {
        method: "GET",
        url,
        headers: &[],
        body: &[],
    })
}

/// Basit HTTP GET (eski arayüz; https de olur).
pub fn http_get(url: &str) -> Result<Response, i64> {
    get(url).map_err(|_| -5)
}

fn dechunk(mut data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(line_end) = data.windows(2).position(|w| w == b"\r\n") {
        let size_text = core::str::from_utf8(&data[..line_end]).unwrap_or("0");
        let size_text = size_text.split(';').next().unwrap_or("0").trim();
        let Ok(size) = usize::from_str_radix(size_text, 16) else {
            break;
        };
        data = &data[line_end + 2..];
        if size == 0 || data.len() < size {
            out.extend_from_slice(&data[..size.min(data.len())]);
            break;
        }
        out.extend_from_slice(&data[..size]);
        data = data.get(size + 2..).unwrap_or(&[]);
    }
    out
}
