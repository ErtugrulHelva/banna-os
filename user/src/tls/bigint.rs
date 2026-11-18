//! RSA için büyük tam sayılar: modüler üs alma (Montgomery çarpımı, 64 bit uzuvlar).
//!
//! Yalnızca imza doğrulamada kullanılır (genel üs, gizli veri yok); bu yüzden sabit
//! zamanlı olması gerekmez.

use alloc::vec;
use alloc::vec::Vec;

/// Küçük uçlu 64 bit uzuvlar.
type Limbs = Vec<u64>;

pub(super) fn from_be(bytes: &[u8], limbs: usize) -> Limbs {
    let mut out = vec![0u64; limbs];
    for (i, &b) in bytes.iter().rev().enumerate() {
        if i / 8 < limbs {
            out[i / 8] |= (b as u64) << (8 * (i % 8));
        }
    }
    out
}

pub(super) fn to_be(limbs: &[u64], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = i / 8;
        if limb < limbs.len() {
            out[len - 1 - i] = (limbs[limb] >> (8 * (i % 8))) as u8;
        }
    }
    out
}

/// a >= b ?
pub(super) fn ge(a: &[u64], b: &[u64]) -> bool {
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

pub(super) fn sub_in_place(a: &mut [u64], b: &[u64]) {
    let mut borrow = 0u64;
    for i in 0..a.len() {
        let (d1, o1) = a[i].overflowing_sub(b[i]);
        let (d2, o2) = d1.overflowing_sub(borrow);
        a[i] = d2;
        borrow = (o1 | o2) as u64;
    }
}

/// -n⁻¹ mod 2⁶⁴ (Newton yinelemesi).
fn neg_inverse(n0: u64) -> u64 {
    let mut x: u64 = 1;
    for _ in 0..6 {
        x = x.wrapping_mul(2u64.wrapping_sub(n0.wrapping_mul(x)));
    }
    x.wrapping_neg()
}

pub(super) struct Montgomery {
    pub(super) n: Limbs,
    n0inv: u64,
    /// R² mod n
    pub(super) r2: Limbs,
}

impl Montgomery {
    pub(super) fn new(n: Limbs) -> Self {
        let k = n.len();
        // R² mod n: 1'i 2·64·k kez ikiye katla, her adımda n'yi aşarsa çıkar.
        let mut r2 = vec![0u64; k];
        r2[0] = 1;
        for _ in 0..(2 * 64 * k) {
            let carry = r2[k - 1] >> 63;
            for i in (1..k).rev() {
                r2[i] = (r2[i] << 1) | (r2[i - 1] >> 63);
            }
            r2[0] <<= 1;
            if carry != 0 || ge(&r2, &n) {
                sub_in_place(&mut r2, &n);
            }
        }
        Self {
            n0inv: neg_inverse(n[0]),
            n,
            r2,
        }
    }

    /// a·b·R⁻¹ mod n (CIOS).
    pub(super) fn mul(&self, a: &[u64], b: &[u64]) -> Limbs {
        let k = self.n.len();
        let mut t = vec![0u64; k + 2];
        for &bi in b.iter().take(k) {
            let mut carry = 0u128;
            for j in 0..k {
                let v = t[j] as u128 + a[j] as u128 * bi as u128 + carry;
                t[j] = v as u64;
                carry = v >> 64;
            }
            let v = t[k] as u128 + carry;
            t[k] = v as u64;
            t[k + 1] = (v >> 64) as u64;

            let m = t[0].wrapping_mul(self.n0inv);
            let v = t[0] as u128 + m as u128 * self.n[0] as u128;
            let mut carry = v >> 64;
            for j in 1..k {
                let v = t[j] as u128 + m as u128 * self.n[j] as u128 + carry;
                t[j - 1] = v as u64;
                carry = v >> 64;
            }
            let v = t[k] as u128 + carry;
            t[k - 1] = v as u64;
            t[k] = t[k + 1] + (v >> 64) as u64;
            t[k + 1] = 0;
        }
        let overflow = t[k] != 0;
        let mut out: Limbs = t[..k].to_vec();
        if overflow || ge(&out, &self.n) {
            sub_in_place(&mut out, &self.n);
        }
        out
    }
}

/// `base^exp mod modulus`; girdiler ve çıktı büyük uçlu bayt dizileri. Modül tek olmalı.
pub fn mod_pow(base: &[u8], exp: &[u8], modulus: &[u8]) -> Option<Vec<u8>> {
    let modulus = strip(modulus);
    if modulus.is_empty() || modulus[modulus.len() - 1] & 1 == 0 {
        return None;
    }
    let k = modulus.len().div_ceil(8);
    let n = from_be(modulus, k);
    let b = from_be(strip(base), k);
    if strip(base).len() > modulus.len() || ge(&b, &n) {
        return None;
    }
    let mont = Montgomery::new(n);
    let x = mont.mul(&b, &mont.r2);
    let mut one = vec![0u64; k];
    one[0] = 1;
    let mut r = mont.mul(&one, &mont.r2);
    for &byte in strip(exp) {
        for bit in (0..8).rev() {
            r = mont.mul(&r, &r);
            if byte >> bit & 1 != 0 {
                r = mont.mul(&r, &x);
            }
        }
    }
    let r = mont.mul(&r, &one);
    Some(to_be(&r, modulus.len()))
}

/// Baştaki sıfır baytları atar.
pub fn strip(bytes: &[u8]) -> &[u8] {
    let first = bytes.iter().position(|&b| b != 0).unwrap_or(bytes.len());
    &bytes[first..]
}

/// RSASSA-PKCS1-v1_5 imza doğrulaması; `digest` `hash` ile hesaplanmış özet.
pub fn rsa_verify(n: &[u8], e: &[u8], hash: super::Hash, digest: &[u8], signature: &[u8]) -> bool {
    let n = strip(n);
    let len = n.len();
    if signature.len() != len || len < 64 || digest.len() != hash.len() {
        return false;
    }
    let Some(em) = mod_pow(signature, e, n) else {
        return false;
    };
    let prefix = hash.digest_info();
    let Some(pad) = len.checked_sub(3 + prefix.len() + digest.len()) else {
        return false;
    };
    if pad < 8 {
        return false;
    }
    let mut expected = vec![0u8, 1];
    expected.extend(core::iter::repeat_n(0xffu8, pad));
    expected.push(0);
    expected.extend_from_slice(prefix);
    expected.extend_from_slice(digest);
    expected == em
}
