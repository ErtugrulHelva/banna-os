//! TLS istemcisinin ana makinede denetimi: AES/GCM/P-256 test vektörleri ve Banna
//! ekosistemiyle (ecosys.trybanna.com) gerçek el sıkışma. Kullanım: make tls-check

extern crate alloc;
#[path = "../src/crypto.rs"]
#[allow(dead_code)]
mod crypto;
#[path = "../user/src/tls/mod.rs"]
mod tls;

use std::io::{Read, Write};

struct Tcp(std::net::TcpStream);
impl tls::Transport for Tcp {
    fn recv(&mut self, buf: &mut [u8]) -> Result<usize, i64> { self.0.read(buf).map_err(|_| -5) }
    fn send(&mut self, data: &[u8]) -> Result<(), i64> { self.0.write_all(data).map_err(|_| -5) }
}

fn hex(b: &[u8]) -> String { b.iter().map(|x| format!("{:02x}", x)).collect() }

fn main() {
    // AES-128 FIPS-197
    let key: [u8; 16] = core::array::from_fn(|i| i as u8);
    let mut block: [u8; 16] = core::array::from_fn(|i| (i as u8) * 0x11);
    tls::aes::Aes128::new(&key).encrypt_block(&mut block);
    println!("aes  {} {}", hex(&block), hex(&block) == "69c4e0d86a7b0430d8cdb78070b4c55a");
    // GCM test case 2
    let g = tls::aes::Gcm::new(&[0; 16]);
    let out = g.seal(&[0; 12], &[], &[0; 16]);
    println!("gcm  {}", hex(&out) == "0388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf");
    println!("gcm open {}", g.open(&[0; 12], &[], &out) == Some(vec![0; 16]));
    // P-256: 1·G = G, ve d1·(d2·G) == d2·(d1·G)
    let mut one = [0u8; 32]; one[31] = 1;
    let g1 = tls::p256::public_key(&one);
    println!("p256 G {}", hex(&g1[1..33]) == "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296");
    let a = [7u8; 32]; let mut a2 = a; tls::p256::clamp_secret(&mut a2);
    let b = [9u8; 32]; let mut b2 = b; tls::p256::clamp_secret(&mut b2);
    let s1 = tls::p256::shared_secret(&a2, &tls::p256::public_key(&b2)).unwrap();
    let s2 = tls::p256::shared_secret(&b2, &tls::p256::public_key(&a2)).unwrap();
    println!("ecdh {}", s1 == s2);

    // SHA-384/512 (FIPS 180-4 "abc")
    println!("sha384 {}", hex(&tls::sha512::sha384(b"abc")).starts_with("cb00753f45a35e8b"));
    println!("sha512 {}", hex(&tls::sha512::sha512(b"abc")).starts_with("ddaf35a193617aba"));

    // ECDSA (openssl ile üretilmiş): doğru imza geçmeli, değiştirilmiş ileti/imza geçmemeli.
    let unhex = |h: &str| (0..h.len()).step_by(2).map(|i| u8::from_str_radix(&h[i..i + 2], 16).unwrap()).collect::<Vec<u8>>();
    for (curve, hash, sig, key) in [
        (tls::ecdsa::Curve::P256, tls::Hash::Sha256, "3044022007ee5f240e083132b4af23113a5c3fa55ad224532257968fd9e374535cbcb72302206da62927d799a01e76dfd2038c0e8e06a65fd19bf653905bc90fb3dc1a6a540b", "04243c37b808d081a26c9ebb89d6cef683828f4bbd8f0162e71ad2fd8ff781a33b09943c2b1d2c2e7a3f88cc4ecb28d112a74c3f58c2cdfd7e48155c031ee475e0"),
        (tls::ecdsa::Curve::P384, tls::Hash::Sha384, "3065023100c87e472e447579a06349760cdb843a4dede3af839f7b8de6e3873d9cc769b8cf771e6042072b69fa7bd099ab3da3870e023029730ceef8840d68dbf32bacf990d1f4e897aeb67e9851187c9273c3156e6d24d9fb211d104c8f95a1b3e18e2a4ff8ad", "04e8b4ed34a9893d5d138f4673714dc81013d8e10a31fae66cbe092b71e8c7717c907db843e5a557f9ba8439c90d01e8b971f5917772bd1afbde96f6ca6c0e9ffa9b472400e5244e9e01001672dd31f4fb1a39ab17f9afd0acfceb510de1dbd46f"),
    ] {
        let (sig, key) = (unhex(sig), unhex(key));
        let good = tls::ecdsa::verify(curve, &key, &hash.digest(b"banna"), &sig);
        let bad_msg = tls::ecdsa::verify(curve, &key, &hash.digest(b"bannb"), &sig);
        let mut tampered = sig.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        let bad_sig = tls::ecdsa::verify(curve, &key, &hash.digest(b"banna"), &tampered);
        println!("ecdsa {:?} {}", curve, good && !bad_msg && !bad_sig);
    }

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64;
    let mut seed = now as u64 ^ 0x9e3779b97f4a7c15;
    let mut rng = |buf: &mut [u8]| for b in buf { seed ^= seed << 13; seed ^= seed >> 7; seed ^= seed << 17; *b = seed as u8; };
    let hosts: Vec<String> = std::env::args().skip(1).collect();
    let defaults = ["ecosys.trybanna.com", "www.google.com", "github.com", "www.cloudflare.com", "letsencrypt.org", "www.amazon.com", "crates.io"];
    let list: Vec<&str> = if hosts.is_empty() { defaults.to_vec() } else { hosts.iter().map(|s| s.as_str()).collect() };
    let mut failed = 0;
    for host in list {
        let tcp = match std::net::TcpStream::connect((host, 443)) {
            Ok(t) => t,
            Err(e) => { println!("{:<22} bağlanamadı: {}", host, e); continue; }
        };
        let t0 = std::time::Instant::now();
        let mut s = match tls::connect(Tcp(tcp), host, now, &mut rng) {
            Ok(s) => s,
            Err(e) => { failed += 1; println!("{:<22} TLS hata: {:?} {}", host, e, e.message()); continue; }
        };
        let took = t0.elapsed();
        s.write_all(format!("HEAD / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n", host).as_bytes()).unwrap();
        let mut out = Vec::new();
        let _ = s.read_to_end(&mut out);
        let text = String::from_utf8_lossy(&out);
        println!("{:<22} el sıkışma {:>6.0?}  {}", host, took, text.lines().next().unwrap_or("(yanıt yok)"));
    }
    if failed > 0 {
        std::process::exit(1);
    }
}
