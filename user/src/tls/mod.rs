//! TLS 1.2 istemcisi: TLS_ECDHE_{ECDSA,RSA}_WITH_AES_128_GCM_SHA256, P-256 anahtar değişimi,
//! RSA PKCS#1 ve ECDSA (P-256/P-384) imzaları; sertifika zinciri gömülü köklere kadar
//! doğrulanır, ana makine adı SAN ile eşleştirilir.
//!
//! Banna ekosistemi (`ecosys.trybanna.com`) bu takımı konuşur. Kod çekirdekten bağımsızdır:
//! taşıma katmanı (`Transport`), rastgelelik ve saat çağırandan gelir.

pub mod aes;
pub mod bigint;
pub mod ecdsa;
pub mod p256;
pub mod sha512;
pub mod x509;

use alloc::vec;
use alloc::vec::Vec;

use crate::crypto::{hmac_sha256, sha256};
use aes::Gcm;

const CHANGE_CIPHER_SPEC: u8 = 20;
const ALERT: u8 = 21;
const HANDSHAKE: u8 = 22;
const APPLICATION_DATA: u8 = 23;
const VERSION: [u8; 2] = [3, 3];
const SUITE_ECDSA: u16 = 0xc02b;
const SUITE_RSA: u16 = 0xc02f;
const MAX_RECORD: usize = 16384 + 2048;

/// İmzalarda kullanılan özet algoritmaları.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hash {
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    pub fn digest(self, data: &[u8]) -> Vec<u8> {
        match self {
            Hash::Sha256 => sha256(data).to_vec(),
            Hash::Sha384 => sha512::sha384(data).to_vec(),
            Hash::Sha512 => sha512::sha512(data).to_vec(),
        }
    }

    pub fn len(self) -> usize {
        match self {
            Hash::Sha256 => 32,
            Hash::Sha384 => 48,
            Hash::Sha512 => 64,
        }
    }

    /// PKCS#1 v1.5 DigestInfo öneki.
    pub fn digest_info(self) -> &'static [u8] {
        match self {
            Hash::Sha256 => &[
                0x30, 0x31, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x01, 0x05, 0x00, 0x04, 0x20,
            ],
            Hash::Sha384 => &[
                0x30, 0x41, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x02, 0x05, 0x00, 0x04, 0x30,
            ],
            Hash::Sha512 => &[
                0x30, 0x51, 0x30, 0x0d, 0x06, 0x09, 0x60, 0x86, 0x48, 0x01, 0x65, 0x03, 0x04, 0x02,
                0x03, 0x05, 0x00, 0x04, 0x40,
            ],
        }
    }
}

pub trait Transport {
    /// En fazla `buf.len()` bayt okur; 0 = bağlantı kapandı.
    fn recv(&mut self, buf: &mut [u8]) -> core::result::Result<usize, i64>;
    fn send(&mut self, data: &[u8]) -> core::result::Result<(), i64>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    Io(i64),
    Protocol(&'static str),
    Alert(u8),
    Certificate(x509::CertError),
    BadSignature,
    BadMac,
}

impl Error {
    pub fn message(&self) -> &'static str {
        match self {
            Error::Io(_) => "bağlantı hatası",
            Error::Protocol(m) => m,
            Error::Alert(_) => "sunucu TLS uyarısı gönderdi",
            Error::Certificate(x509::CertError::Expired) => {
                "sertifika süresi dolmuş (saat doğru mu?)"
            }
            Error::Certificate(x509::CertError::WrongHost) => "sertifika bu ada ait değil",
            Error::Certificate(x509::CertError::UnknownIssuer) => {
                "sertifika güvenilen bir köke bağlanmıyor"
            }
            Error::Certificate(_) => "sertifika doğrulanamadı",
            Error::BadSignature => "sunucu anahtar imzası geçersiz",
            Error::BadMac => "kayıt bütünlüğü bozuk",
        }
    }
}

type Result<T> = core::result::Result<T, Error>;

/// TLS 1.2 PRF (P_SHA256).
fn prf(secret: &[u8], label: &[u8], seed: &[u8], len: usize) -> Vec<u8> {
    let mut label_seed = label.to_vec();
    label_seed.extend_from_slice(seed);
    let mut out = Vec::with_capacity(len + 32);
    let mut a = hmac_sha256(secret, &label_seed).to_vec();
    while out.len() < len {
        let mut input = a.clone();
        input.extend_from_slice(&label_seed);
        out.extend_from_slice(&hmac_sha256(secret, &input));
        a = hmac_sha256(secret, &a).to_vec();
    }
    out.truncate(len);
    out
}

struct Direction {
    gcm: Gcm,
    iv: [u8; 4],
    seq: u64,
}

impl Direction {
    fn nonce(&self) -> [u8; 12] {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(&self.iv);
        n[4..].copy_from_slice(&self.seq.to_be_bytes());
        n
    }

    fn aad(&self, kind: u8, len: usize) -> [u8; 13] {
        let mut a = [0u8; 13];
        a[..8].copy_from_slice(&self.seq.to_be_bytes());
        a[8] = kind;
        a[9..11].copy_from_slice(&VERSION);
        a[11..].copy_from_slice(&(len as u16).to_be_bytes());
        a
    }
}

pub struct TlsStream<T: Transport> {
    io: T,
    inbuf: Vec<u8>,
    plain: Vec<u8>,
    read: Option<Direction>,
    write: Option<Direction>,
    closed: bool,
}

impl<T: Transport> TlsStream<T> {
    fn send_record(&mut self, kind: u8, payload: &[u8]) -> Result<()> {
        for chunk in payload.chunks(16384) {
            let body = match self.write.as_mut() {
                Some(d) => {
                    let nonce = d.nonce();
                    let aad = d.aad(kind, chunk.len());
                    let mut body = nonce[4..].to_vec();
                    body.extend_from_slice(&d.gcm.seal(&nonce, &aad, chunk));
                    d.seq += 1;
                    body
                }
                None => chunk.to_vec(),
            };
            let mut record = vec![kind, VERSION[0], VERSION[1]];
            record.extend_from_slice(&(body.len() as u16).to_be_bytes());
            record.extend_from_slice(&body);
            self.io.send(&record).map_err(Error::Io)?;
        }
        Ok(())
    }

    /// Bir kayıt okur: (tür, açık metin). Bağlantı kapandıysa `None`.
    fn recv_record(&mut self) -> Result<Option<(u8, Vec<u8>)>> {
        loop {
            if self.inbuf.len() >= 5 {
                let len = u16::from_be_bytes([self.inbuf[3], self.inbuf[4]]) as usize;
                if len > MAX_RECORD {
                    return Err(Error::Protocol("kayıt çok büyük"));
                }
                if self.inbuf.len() >= 5 + len {
                    let kind = self.inbuf[0];
                    let body: Vec<u8> = self.inbuf[5..5 + len].to_vec();
                    self.inbuf.drain(..5 + len);
                    let plain = match self.read.as_mut() {
                        Some(d) if kind != CHANGE_CIPHER_SPEC => {
                            if body.len() < 8 + 16 {
                                return Err(Error::BadMac);
                            }
                            let mut nonce = [0u8; 12];
                            nonce[..4].copy_from_slice(&d.iv);
                            nonce[4..].copy_from_slice(&body[..8]);
                            let aad = d.aad(kind, body.len() - 8 - 16);
                            let plain =
                                d.gcm.open(&nonce, &aad, &body[8..]).ok_or(Error::BadMac)?;
                            d.seq += 1;
                            plain
                        }
                        _ => body,
                    };
                    if kind == ALERT {
                        // close_notify ya da ölümcül uyarı: bağlantı bitti.
                        if plain.get(1) == Some(&0) {
                            self.closed = true;
                            return Ok(None);
                        }
                        return Err(Error::Alert(plain.get(1).copied().unwrap_or(0)));
                    }
                    return Ok(Some((kind, plain)));
                }
            }
            let mut buf = [0u8; 8192];
            let n = self.io.recv(&mut buf).map_err(Error::Io)?;
            if n == 0 {
                self.closed = true;
                return Ok(None);
            }
            self.inbuf.extend_from_slice(&buf[..n]);
        }
    }

    pub fn write_all(&mut self, data: &[u8]) -> Result<()> {
        self.send_record(APPLICATION_DATA, data)
    }

    /// Açık metin okur; 0 = bağlantı kapandı.
    pub fn read(&mut self, buf: &mut [u8]) -> Result<usize> {
        while self.plain.is_empty() {
            if self.closed {
                return Ok(0);
            }
            match self.recv_record()? {
                Some((APPLICATION_DATA, data)) => self.plain = data,
                Some(_) => {}
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.plain.len());
        buf[..n].copy_from_slice(&self.plain[..n]);
        self.plain.drain(..n);
        Ok(n)
    }

    pub fn read_to_end(&mut self, out: &mut Vec<u8>) -> Result<()> {
        let mut buf = [0u8; 8192];
        loop {
            match self.read(&mut buf)? {
                0 => return Ok(()),
                n => out.extend_from_slice(&buf[..n]),
            }
        }
    }

    pub fn close(&mut self) {
        let _ = self.send_record(ALERT, &[1, 0]);
    }
}

/// El sıkışma iletilerini kayıtlardan ayıklar (bir kayıtta birden çok ileti olabilir).
struct Messages {
    pending: Vec<u8>,
}

impl Messages {
    fn next<T: Transport>(&mut self, s: &mut TlsStream<T>) -> Result<(u8, Vec<u8>)> {
        loop {
            if self.pending.len() >= 4 {
                let len = u32::from_be_bytes([0, self.pending[1], self.pending[2], self.pending[3]])
                    as usize;
                if self.pending.len() >= 4 + len {
                    let msg: Vec<u8> = self.pending.drain(..4 + len).collect();
                    return Ok((msg[0], msg));
                }
            }
            match s.recv_record()? {
                Some((HANDSHAKE, data)) => self.pending.extend_from_slice(&data),
                Some(_) => return Err(Error::Protocol("beklenmeyen kayıt türü")),
                None => return Err(Error::Protocol("sunucu el sıkışmada bağlantıyı kapattı")),
            }
        }
    }
}

fn handshake_msg(kind: u8, body: &[u8]) -> Vec<u8> {
    let mut m = vec![kind];
    m.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
    m.extend_from_slice(body);
    m
}

fn u16_prefixed(data: &[u8]) -> Vec<u8> {
    let mut v = (data.len() as u16).to_be_bytes().to_vec();
    v.extend_from_slice(data);
    v
}

fn client_hello(random: &[u8; 32], host: &str) -> Vec<u8> {
    let mut body = VERSION.to_vec();
    body.extend_from_slice(random);
    body.push(0); // oturum kimliği yok
    body.extend_from_slice(&u16_prefixed(&[0xc0, 0x2b, 0xc0, 0x2f, 0x00, 0xff]));
    body.extend_from_slice(&[1, 0]); // sıkıştırma yok

    let mut ext = Vec::new();
    let mut add = |kind: u16, data: &[u8]| {
        ext.extend_from_slice(&kind.to_be_bytes());
        ext.extend_from_slice(&u16_prefixed(data));
    };
    let mut sni = vec![0u8];
    sni.extend_from_slice(&u16_prefixed(host.as_bytes()));
    add(0x0000, &u16_prefixed(&sni));
    add(0x000a, &u16_prefixed(&[0x00, 0x17])); // secp256r1
    add(0x000b, &[1, 0]); // sıkıştırılmamış noktalar
    // ecdsa_secp256r1_sha256, ecdsa_secp384r1_sha384, rsa_pkcs1_sha256/384/512
    add(
        0x000d,
        &u16_prefixed(&[0x04, 0x03, 0x05, 0x03, 0x04, 0x01, 0x05, 0x01, 0x06, 0x01]),
    );
    body.extend_from_slice(&u16_prefixed(&ext));
    handshake_msg(1, &body)
}

/// TLS bağlantısı kurar. `now` sertifika süresi denetimi için Unix zamanı.
pub fn connect<T: Transport>(
    io: T,
    host: &str,
    now: i64,
    random: &mut dyn FnMut(&mut [u8]),
) -> Result<TlsStream<T>> {
    let mut s = TlsStream {
        io,
        inbuf: Vec::new(),
        plain: Vec::new(),
        read: None,
        write: None,
        closed: false,
    };
    let mut transcript = Vec::new();
    let mut client_random = [0u8; 32];
    random(&mut client_random);
    let hello = client_hello(&client_random, host);
    transcript.extend_from_slice(&hello);
    s.send_record(HANDSHAKE, &hello)?;

    let mut msgs = Messages {
        pending: Vec::new(),
    };
    let proto = Error::Protocol;

    // ServerHello
    let (kind, msg) = msgs.next(&mut s)?;
    if kind != 2 {
        return Err(proto("ServerHello bekleniyordu"));
    }
    transcript.extend_from_slice(&msg);
    let b = &msg[4..];
    let server_random: [u8; 32] = b
        .get(2..34)
        .ok_or(proto("ServerHello kısa"))?
        .try_into()
        .unwrap();
    let sid_len = *b.get(34).ok_or(proto("ServerHello kısa"))? as usize;
    let suite = b
        .get(35 + sid_len..37 + sid_len)
        .map(|c| u16::from_be_bytes([c[0], c[1]]))
        .ok_or(proto("ServerHello kısa"))?;
    if suite != SUITE_ECDSA && suite != SUITE_RSA {
        return Err(proto("sunucu desteklenmeyen şifre takımı seçti"));
    }

    // Certificate
    let (kind, msg) = msgs.next(&mut s)?;
    if kind != 11 {
        return Err(proto("sertifika bekleniyordu"));
    }
    transcript.extend_from_slice(&msg);
    let mut chain = Vec::new();
    let mut list = msg.get(7..).ok_or(proto("sertifika listesi kısa"))?;
    while list.len() >= 3 {
        let len = u32::from_be_bytes([0, list[0], list[1], list[2]]) as usize;
        let der = list.get(3..3 + len).ok_or(proto("sertifika kısa"))?;
        chain.push(der.to_vec());
        list = &list[3 + len..];
    }
    let key = x509::verify_chain(&chain, host, now).map_err(Error::Certificate)?;
    let ec_key = matches!(key, x509::PublicKey::Ec { .. });
    if ec_key != (suite == SUITE_ECDSA) {
        return Err(proto("sertifika anahtarı şifre takımıyla uyuşmuyor"));
    }

    // ServerKeyExchange: named_curve secp256r1, nokta, imza
    let (kind, msg) = msgs.next(&mut s)?;
    if kind != 12 {
        return Err(proto("anahtar değişimi bekleniyordu"));
    }
    transcript.extend_from_slice(&msg);
    let b = &msg[4..];
    if b.get(..3) != Some(&[3, 0x00, 0x17]) {
        return Err(proto("sunucu P-256 dışında bir eğri seçti"));
    }
    let point_len = *b.get(3).ok_or(proto("anahtar kısa"))? as usize;
    let params = b.get(..4 + point_len).ok_or(proto("anahtar kısa"))?;
    let server_point = &params[4..];
    let rest = &b[4 + point_len..];
    let (hash, ecdsa) = match rest.get(..2) {
        Some([0x04, 0x01]) => (Hash::Sha256, false),
        Some([0x05, 0x01]) => (Hash::Sha384, false),
        Some([0x06, 0x01]) => (Hash::Sha512, false),
        Some([0x04, 0x03]) => (Hash::Sha256, true),
        Some([0x05, 0x03]) => (Hash::Sha384, true),
        _ => return Err(proto("desteklenmeyen imza algoritması")),
    };
    if ecdsa != ec_key {
        return Err(proto("imza algoritması anahtarla uyuşmuyor"));
    }
    let sig_len = rest
        .get(2..4)
        .map(|l| u16::from_be_bytes([l[0], l[1]]) as usize)
        .ok_or(proto("imza kısa"))?;
    let signature = rest.get(4..4 + sig_len).ok_or(proto("imza kısa"))?;
    let mut signed = client_random.to_vec();
    signed.extend_from_slice(&server_random);
    signed.extend_from_slice(params);
    if !key.verify(hash, &hash.digest(&signed), signature) {
        return Err(Error::BadSignature);
    }

    // ServerHelloDone
    let (kind, msg) = msgs.next(&mut s)?;
    if kind != 14 {
        return Err(proto("ServerHelloDone bekleniyordu"));
    }
    transcript.extend_from_slice(&msg);

    // İstemci anahtarı, ortak sır, anahtar türetme.
    let mut secret = [0u8; 32];
    random(&mut secret);
    p256::clamp_secret(&mut secret);
    let public = p256::public_key(&secret);
    let premaster =
        p256::shared_secret(&secret, server_point).ok_or(proto("sunucu noktası geçersiz"))?;
    let mut randoms = client_random.to_vec();
    randoms.extend_from_slice(&server_random);
    let master = prf(&premaster, b"master secret", &randoms, 48);
    let mut seed = server_random.to_vec();
    seed.extend_from_slice(&client_random);
    let keys = prf(&master, b"key expansion", &seed, 40);

    let mut cke = vec![65u8];
    cke.extend_from_slice(&public);
    let cke = handshake_msg(16, &cke);
    transcript.extend_from_slice(&cke);
    s.send_record(HANDSHAKE, &cke)?;
    s.send_record(CHANGE_CIPHER_SPEC, &[1])?;
    s.write = Some(Direction {
        gcm: Gcm::new(keys[0..16].try_into().unwrap()),
        iv: keys[32..36].try_into().unwrap(),
        seq: 0,
    });
    let verify = prf(&master, b"client finished", &sha256(&transcript), 12);
    let finished = handshake_msg(20, &verify);
    transcript.extend_from_slice(&finished);
    s.send_record(HANDSHAKE, &finished)?;

    // Sunucunun ChangeCipherSpec + Finished'ı.
    match s.recv_record()? {
        Some((CHANGE_CIPHER_SPEC, _)) => {}
        _ => return Err(proto("ChangeCipherSpec bekleniyordu")),
    }
    s.read = Some(Direction {
        gcm: Gcm::new(keys[16..32].try_into().unwrap()),
        iv: keys[36..40].try_into().unwrap(),
        seq: 0,
    });
    let (kind, msg) = msgs.next(&mut s)?;
    let expected = prf(&master, b"server finished", &sha256(&transcript), 12);
    if kind != 20 || msg[4..] != expected[..] {
        return Err(proto("sunucu Finished doğrulanamadı"));
    }
    Ok(s)
}
