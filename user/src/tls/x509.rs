//! X.509 sertifikaları (DER): RSA ve EC (P-256/P-384) anahtarları, adlar, geçerlilik, SAN;
//! zincir doğrulama.

use alloc::string::String;
use alloc::vec::Vec;

use super::ecdsa::{self, Curve};
use super::{Hash, bigint};

const OID_SHA256_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b];
const OID_SHA384_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0c];
const OID_SHA512_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0d];
const OID_ECDSA_SHA256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x02];
const OID_ECDSA_SHA384: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x03];
const OID_ECDSA_SHA512: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x04, 0x03, 0x04];
const OID_RSA: &[u8] = &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x01];
const OID_EC: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07];
const OID_P384: &[u8] = &[0x2b, 0x81, 0x04, 0x00, 0x22];
const OID_SAN: &[u8] = &[0x55, 0x1d, 0x11];
const OID_BASIC_CONSTRAINTS: &[u8] = &[0x55, 0x1d, 0x13];

/// Güvenilen kökler (`roots/` altındaki DER dosyaları; liste `roots/README`).
const ROOTS: &[&[u8]] = &[
    include_bytes!("roots/isrg-root-x1.der"),
    include_bytes!("roots/isrg-root-x2.der"),
    include_bytes!("roots/digicert-global-root-ca.der"),
    include_bytes!("roots/digicert-global-root-g2.der"),
    include_bytes!("roots/digicert-global-root-g3.der"),
    include_bytes!("roots/globalsign-root-ca.der"),
    include_bytes!("roots/globalsign-root-r3.der"),
    include_bytes!("roots/gts-root-r1.der"),
    include_bytes!("roots/gts-root-r4.der"),
    include_bytes!("roots/amazon-root-ca-1.der"),
    include_bytes!("roots/amazon-root-ca-3.der"),
    include_bytes!("roots/usertrust-rsa.der"),
    include_bytes!("roots/usertrust-ecc.der"),
    include_bytes!("roots/starfield-root-g2.der"),
    include_bytes!("roots/go-daddy-root-g2.der"),
];

/// Sertifikanın açık anahtarı.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PublicKey {
    Rsa { n: Vec<u8>, e: Vec<u8> },
    Ec { curve: Curve, point: Vec<u8> },
    Unsupported,
}

impl PublicKey {
    /// `digest` (`hash` ile) üzerindeki imzayı doğrular.
    pub fn verify(&self, hash: Hash, digest: &[u8], signature: &[u8]) -> bool {
        match self {
            PublicKey::Rsa { n, e } => bigint::rsa_verify(n, e, hash, digest, signature),
            PublicKey::Ec { curve, point } => ecdsa::verify(*curve, point, digest, signature),
            PublicKey::Unsupported => false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertError {
    Parse,
    Expired,
    UnknownIssuer,
    BadSignature,
    NotCa,
    WrongHost,
    Unsupported,
}

/// DER öğesi: (etiket, içerik, tüm öğe, kalan).
struct Tlv<'a> {
    tag: u8,
    content: &'a [u8],
    whole: &'a [u8],
}

fn read(data: &[u8]) -> Option<(Tlv<'_>, &[u8])> {
    let tag = *data.first()?;
    let first = *data.get(1)?;
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return None;
        }
        let mut len = 0usize;
        for i in 0..n {
            len = (len << 8) | *data.get(2 + i)? as usize;
        }
        (len, 2 + n)
    };
    let end = header.checked_add(len)?;
    if end > data.len() {
        return None;
    }
    Some((
        Tlv {
            tag,
            content: &data[header..end],
            whole: &data[..end],
        },
        &data[end..],
    ))
}

fn expect(data: &[u8], tag: u8) -> Option<(Tlv<'_>, &[u8])> {
    let (tlv, rest) = read(data)?;
    (tlv.tag == tag).then_some((tlv, rest))
}

pub struct Certificate<'a> {
    pub tbs: &'a [u8],
    pub sig_alg: &'a [u8],
    pub signature: &'a [u8],
    pub issuer: &'a [u8],
    pub subject: &'a [u8],
    pub not_before: i64,
    pub not_after: i64,
    pub key: PublicKey,
    pub dns_names: Vec<String>,
    pub is_ca: bool,
}

/// "YYMMDDHHMMSSZ" ya da "YYYYMMDDHHMMSSZ" → Unix zamanı.
fn parse_time(tlv: &Tlv) -> Option<i64> {
    let s = core::str::from_utf8(tlv.content).ok()?;
    let digits = |r: core::ops::Range<usize>| s.get(r)?.parse::<i64>().ok();
    let (year, rest) = match tlv.tag {
        0x17 => {
            let y = digits(0..2)?;
            (if y < 50 { 2000 + y } else { 1900 + y }, 2)
        }
        0x18 => (digits(0..4)?, 4),
        _ => return None,
    };
    let month = digits(rest..rest + 2)?;
    let day = digits(rest + 2..rest + 4)?;
    let hour = digits(rest + 4..rest + 6)?;
    let minute = digits(rest + 6..rest + 8)?;
    let second = digits(rest + 8..rest + 10)?;
    // Gregoryen takvimden gün sayısı (Howard Hinnant).
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days * 86_400 + hour * 3600 + minute * 60 + second)
}

impl<'a> Certificate<'a> {
    pub fn parse(der: &'a [u8]) -> Option<Self> {
        let (cert, _) = expect(der, 0x30)?;
        let (tbs, rest) = expect(cert.content, 0x30)?;
        let (alg, rest) = expect(rest, 0x30)?;
        let (sig, _) = expect(rest, 0x03)?;
        let (sig_oid, _) = expect(alg.content, 0x06)?;

        let mut t = tbs.content;
        if t.first() == Some(&0xa0) {
            t = read(t)?.1;
        }
        let (_serial, t) = expect(t, 0x02)?;
        let (_alg, t) = expect(t, 0x30)?;
        let (issuer, t) = expect(t, 0x30)?;
        let (validity, t) = expect(t, 0x30)?;
        let (subject, t) = expect(t, 0x30)?;
        let (spki, mut t) = expect(t, 0x30)?;

        let (nb, rest) = read(validity.content)?;
        let (na, _) = read(rest)?;

        // SubjectPublicKeyInfo: { { algoritma, parametre }, BIT STRING anahtar }
        let (key_alg, rest) = expect(spki.content, 0x30)?;
        let (key_oid, key_params) = expect(key_alg.content, 0x06)?;
        let (bits, _) = expect(rest, 0x03)?;
        let key = if key_oid.content == OID_RSA {
            let (seq, _) = expect(bits.content.get(1..)?, 0x30)?;
            let (n, rest) = expect(seq.content, 0x02)?;
            let (e, _) = expect(rest, 0x02)?;
            PublicKey::Rsa {
                n: n.content.to_vec(),
                e: e.content.to_vec(),
            }
        } else if key_oid.content == OID_EC {
            let (curve, _) = expect(key_params, 0x06)?;
            let point = bits.content.get(1..)?.to_vec();
            match curve.content {
                OID_P256 => PublicKey::Ec {
                    curve: Curve::P256,
                    point,
                },
                OID_P384 => PublicKey::Ec {
                    curve: Curve::P384,
                    point,
                },
                _ => PublicKey::Unsupported,
            }
        } else {
            PublicKey::Unsupported
        };

        let mut dns_names = Vec::new();
        let mut is_ca = false;
        while let Some((field, rest)) = read(t) {
            t = rest;
            if field.tag != 0xa3 {
                continue;
            }
            let (exts, _) = expect(field.content, 0x30)?;
            let mut list = exts.content;
            while let Some((ext, rest)) = expect(list, 0x30) {
                list = rest;
                let (oid, mut body) = expect(ext.content, 0x06)?;
                if body.first() == Some(&0x01) {
                    body = read(body)?.1;
                }
                let (value, _) = expect(body, 0x04)?;
                if oid.content == OID_SAN {
                    let (names, _) = expect(value.content, 0x30)?;
                    let mut n = names.content;
                    while let Some((name, rest)) = read(n) {
                        n = rest;
                        if name.tag == 0x82 {
                            dns_names
                                .push(String::from_utf8_lossy(name.content).to_ascii_lowercase());
                        }
                    }
                } else if oid.content == OID_BASIC_CONSTRAINTS {
                    let (seq, _) = expect(value.content, 0x30)?;
                    if let Some((flag, _)) = expect(seq.content, 0x01) {
                        is_ca = flag.content.first().is_some_and(|&b| b != 0);
                    }
                }
            }
        }

        Some(Certificate {
            tbs: tbs.whole,
            sig_alg: sig_oid.content,
            signature: sig.content.get(1..)?,
            issuer: issuer.whole,
            subject: subject.whole,
            not_before: parse_time(&nb)?,
            not_after: parse_time(&na)?,
            key,
            dns_names,
            is_ca,
        })
    }

    /// `self`, `issuer` anahtarıyla imzalanmış mı?
    fn signed_by(&self, issuer: &Certificate) -> Result<(), CertError> {
        let (hash, ec) = match self.sig_alg {
            OID_SHA256_RSA => (Hash::Sha256, false),
            OID_SHA384_RSA => (Hash::Sha384, false),
            OID_SHA512_RSA => (Hash::Sha512, false),
            OID_ECDSA_SHA256 => (Hash::Sha256, true),
            OID_ECDSA_SHA384 => (Hash::Sha384, true),
            OID_ECDSA_SHA512 => (Hash::Sha512, true),
            _ => return Err(CertError::Unsupported),
        };
        if ec != matches!(issuer.key, PublicKey::Ec { .. }) {
            return Err(CertError::Unsupported);
        }
        if issuer
            .key
            .verify(hash, &hash.digest(self.tbs), self.signature)
        {
            Ok(())
        } else {
            Err(CertError::BadSignature)
        }
    }

    fn matches_host(&self, host: &str) -> bool {
        let host = host.to_ascii_lowercase();
        self.dns_names
            .iter()
            .any(|name| match name.strip_prefix("*.") {
                Some(suffix) => host
                    .split_once('.')
                    .is_some_and(|(label, rest)| !label.is_empty() && rest == suffix),
                None => *name == host,
            })
    }
}

/// Sunucunun gönderdiği zinciri (yaprak önce) güvenilen bir köke kadar doğrular.
/// Başarıda yaprağın açık anahtarını döndürür.
pub fn verify_chain(chain: &[Vec<u8>], host: &str, now: i64) -> Result<PublicKey, CertError> {
    let certs: Vec<Certificate> = chain
        .iter()
        .map(|der| Certificate::parse(der))
        .collect::<Option<_>>()
        .ok_or(CertError::Parse)?;
    let leaf = certs.first().ok_or(CertError::Parse)?;
    if !leaf.matches_host(host) {
        return Err(CertError::WrongHost);
    }
    let roots: Vec<Certificate> = ROOTS.iter().filter_map(|d| Certificate::parse(d)).collect();
    for (i, cert) in certs.iter().enumerate() {
        // Saat ayarlı değilse (1970) süre denetimi atlanır.
        if now > 1_600_000_000 && (now < cert.not_before || now > cert.not_after) {
            return Err(CertError::Expired);
        }
        if i > 0 && !cert.is_ca {
            return Err(CertError::NotCa);
        }
        if let Some(root) = roots.iter().find(|r| r.subject == cert.issuer) {
            return cert.signed_by(root).map(|_| leaf.key.clone());
        }
        let issuer = certs.get(i + 1).ok_or(CertError::UnknownIssuer)?;
        if issuer.subject != cert.issuer {
            return Err(CertError::UnknownIssuer);
        }
        cert.signed_by(issuer)?;
    }
    Err(CertError::UnknownIssuer)
}
