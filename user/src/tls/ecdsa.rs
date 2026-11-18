//! ECDSA imza doğrulaması (P-256, P-384): a = -3 eğrileri, Jacobian koordinatlar,
//! `bigint`'in Montgomery çarpımı. Yalnızca açık verilerle çalışır (sabit zaman gerekmez).

use alloc::vec;
use alloc::vec::Vec;

use super::bigint::{Montgomery, from_be, ge, sub_in_place, to_be};

type Limbs = Vec<u64>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Curve {
    P256,
    P384,
}

struct Params {
    p: &'static str,
    n: &'static str,
    b: &'static str,
    gx: &'static str,
    gy: &'static str,
}

const P256: Params = Params {
    p: "ffffffff00000001000000000000000000000000ffffffffffffffffffffffff",
    n: "ffffffff00000000ffffffffffffffffbce6faada7179e84f3b9cac2fc632551",
    b: "5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b",
    gx: "6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296",
    gy: "4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5",
};

const P384: Params = Params {
    p: "fffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffeffffffff0000000000000000ffffffff",
    n: "ffffffffffffffffffffffffffffffffffffffffffffffffc7634d81f4372ddf581a0db248b0a77aecec196accc52973",
    b: "b3312fa7e23ee7e4988e056be3f82d19181d9c6efe8141120314088f5013875ac656398d8a2ed19d2a85c8edd3ec2aef",
    gx: "aa87ca22be8b05378eb1c71ef320ad746e1d3b628ba79b9859f741e082542a385502f25dbf55296c3a545e3872760ab7",
    gy: "3617de4a96262c6f5d9e98bf9292dc29f8f41dbd289a147ce9da3113b5f0b8c00a60b1ce1d7e819d7a431d7c90ea0e5f",
};

impl Curve {
    fn params(self) -> &'static Params {
        match self {
            Curve::P256 => &P256,
            Curve::P384 => &P384,
        }
    }

    /// Koordinat uzunluğu (bayt).
    pub fn size(self) -> usize {
        match self {
            Curve::P256 => 32,
            Curve::P384 => 48,
        }
    }
}

fn hex(s: &str) -> Vec<u8> {
    crate::crypto::from_hex(s).unwrap_or_default()
}

/// Montgomery biçiminde bir mod m aritmetiği.
struct Ring {
    m: Montgomery,
    k: usize,
}

impl Ring {
    fn new(modulus: &[u8]) -> Self {
        let k = modulus.len().div_ceil(8);
        Ring {
            m: Montgomery::new(from_be(modulus, k)),
            k,
        }
    }

    fn add(&self, a: &[u64], b: &[u64]) -> Limbs {
        let mut out = vec![0u64; self.k];
        let mut carry = 0u64;
        for i in 0..self.k {
            let (s1, o1) = a[i].overflowing_add(b[i]);
            let (s2, o2) = s1.overflowing_add(carry);
            out[i] = s2;
            carry = (o1 | o2) as u64;
        }
        if carry != 0 || ge(&out, &self.m.n) {
            sub_in_place(&mut out, &self.m.n);
        }
        out
    }

    fn sub(&self, a: &[u64], b: &[u64]) -> Limbs {
        let mut out = a.to_vec();
        if ge(a, b) {
            sub_in_place(&mut out, b);
        } else {
            // a - b + m = a + (m - b)
            let mut mb = self.m.n.clone();
            sub_in_place(&mut mb, b);
            out = self.add(a, &mb);
        }
        out
    }

    fn mul(&self, a: &[u64], b: &[u64]) -> Limbs {
        self.m.mul(a, b)
    }

    fn to_mont(&self, a: &[u64]) -> Limbs {
        self.m.mul(a, &self.m.r2)
    }

    fn from_mont(&self, a: &[u64]) -> Limbs {
        let mut one = vec![0u64; self.k];
        one[0] = 1;
        self.m.mul(a, &one)
    }

    /// a^(m-2): Montgomery biçiminde ters (m asal).
    fn invert(&self, a: &[u64]) -> Limbs {
        let mut e = self.m.n.clone();
        let mut two = vec![0u64; self.k];
        two[0] = 2;
        sub_in_place(&mut e, &two);
        let mut one = vec![0u64; self.k];
        one[0] = 1;
        let mut r = self.to_mont(&one);
        for i in (0..self.k * 64).rev() {
            r = self.mul(&r, &r);
            if e[i / 64] >> (i % 64) & 1 != 0 {
                r = self.mul(&r, a);
            }
        }
        r
    }

    fn is_zero(a: &[u64]) -> bool {
        a.iter().all(|&l| l == 0)
    }
}

#[derive(Clone)]
struct Point {
    x: Limbs,
    y: Limbs,
    z: Limbs,
}

struct Group {
    f: Ring,
    one: Limbs,
}

impl Group {
    fn infinity(&self) -> Point {
        Point {
            x: vec![0; self.f.k],
            y: vec![0; self.f.k],
            z: vec![0; self.f.k],
        }
    }

    fn double(&self, p: &Point) -> Point {
        let f = &self.f;
        if Ring::is_zero(&p.z) || Ring::is_zero(&p.y) {
            return self.infinity();
        }
        let delta = f.mul(&p.z, &p.z);
        let gamma = f.mul(&p.y, &p.y);
        let beta = f.mul(&p.x, &gamma);
        let t = f.mul(&f.sub(&p.x, &delta), &f.add(&p.x, &delta));
        let alpha = f.add(&f.add(&t, &t), &t);
        let beta2 = f.add(&beta, &beta);
        let beta4 = f.add(&beta2, &beta2);
        let beta8 = f.add(&beta4, &beta4);
        let x3 = f.sub(&f.mul(&alpha, &alpha), &beta8);
        let yz = f.add(&p.y, &p.z);
        let z3 = f.sub(&f.sub(&f.mul(&yz, &yz), &gamma), &delta);
        let g2 = f.mul(&gamma, &gamma);
        let g4 = f.add(&g2, &g2);
        let g8 = f.add(&f.add(&g4, &g4), &f.add(&g4, &g4));
        let y3 = f.sub(&f.mul(&alpha, &f.sub(&beta4, &x3)), &g8);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    fn add(&self, a: &Point, b: &Point) -> Point {
        let f = &self.f;
        if Ring::is_zero(&a.z) {
            return b.clone();
        }
        if Ring::is_zero(&b.z) {
            return a.clone();
        }
        let z1z1 = f.mul(&a.z, &a.z);
        let z2z2 = f.mul(&b.z, &b.z);
        let u1 = f.mul(&a.x, &z2z2);
        let u2 = f.mul(&b.x, &z1z1);
        let s1 = f.mul(&f.mul(&a.y, &b.z), &z2z2);
        let s2 = f.mul(&f.mul(&b.y, &a.z), &z1z1);
        let h = f.sub(&u2, &u1);
        let r = f.sub(&s2, &s1);
        if Ring::is_zero(&h) {
            return if Ring::is_zero(&r) {
                self.double(a)
            } else {
                self.infinity()
            };
        }
        let r = f.add(&r, &r);
        let h2 = f.add(&h, &h);
        let i = f.mul(&h2, &h2);
        let j = f.mul(&h, &i);
        let v = f.mul(&u1, &i);
        let x3 = f.sub(&f.sub(&f.mul(&r, &r), &j), &f.add(&v, &v));
        let s1j = f.mul(&s1, &j);
        let y3 = f.sub(&f.mul(&r, &f.sub(&v, &x3)), &f.add(&s1j, &s1j));
        let zz = f.add(&a.z, &b.z);
        let z3 = f.mul(&f.sub(&f.sub(&f.mul(&zz, &zz), &z1z1), &z2z2), &h);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    /// u1·G + u2·Q (Shamir hilesi).
    fn double_mul(&self, u1: &[u64], g: &Point, u2: &[u64], q: &Point) -> Point {
        let gq = self.add(g, q);
        let mut r = self.infinity();
        for i in (0..u1.len() * 64).rev() {
            r = self.double(&r);
            let a = u1[i / 64] >> (i % 64) & 1 != 0;
            let b = u2[i / 64] >> (i % 64) & 1 != 0;
            match (a, b) {
                (true, true) => r = self.add(&r, &gq),
                (true, false) => r = self.add(&r, g),
                (false, true) => r = self.add(&r, q),
                _ => {}
            }
        }
        r
    }

    /// Sıkıştırılmamış nokta (04 || X || Y); eğri üzerinde değilse `None`.
    fn decode(&self, curve: Curve, bytes: &[u8], b: &[u64]) -> Option<Point> {
        let size = curve.size();
        if bytes.len() != 1 + 2 * size || bytes[0] != 4 {
            return None;
        }
        let f = &self.f;
        let x = from_be(&bytes[1..1 + size], f.k);
        let y = from_be(&bytes[1 + size..], f.k);
        if ge(&x, &f.m.n) || ge(&y, &f.m.n) {
            return None;
        }
        let (x, y) = (f.to_mont(&x), f.to_mont(&y));
        let x3 = f.mul(&f.mul(&x, &x), &x);
        let rhs = f.add(&f.sub(&x3, &f.add(&f.add(&x, &x), &x)), b);
        if f.mul(&y, &y) != rhs {
            return None;
        }
        Some(Point {
            x,
            y,
            z: self.one.clone(),
        })
    }
}

/// DER `SEQUENCE { INTEGER r, INTEGER s }` → (r, s) büyük uçlu.
fn parse_signature(der: &[u8]) -> Option<(&[u8], &[u8])> {
    let int = |d: &'_ [u8]| -> Option<(usize, usize)> {
        if *d.first()? != 0x02 {
            return None;
        }
        let len = *d.get(1)? as usize;
        (len < 0x80 && d.len() >= 2 + len).then_some((2, len))
    };
    if *der.first()? != 0x30 {
        return None;
    }
    let body = if *der.get(1)? == 0x81 {
        der.get(3..)?
    } else {
        der.get(2..)?
    };
    let (h, rl) = int(body)?;
    let r = &body[h..h + rl];
    let rest = &body[h + rl..];
    let (h, sl) = int(rest)?;
    Some((r, &rest[h..h + sl]))
}

/// `point` (04||X||Y) açık anahtarıyla `digest` üzerindeki DER imzayı doğrular.
pub fn verify(curve: Curve, point: &[u8], digest: &[u8], signature: &[u8]) -> bool {
    let params = curve.params();
    let size = curve.size();
    let field = Ring::new(&hex(params.p));
    let order = Ring::new(&hex(params.n));
    let mut one = vec![0u64; field.k];
    one[0] = 1;
    let group = Group {
        one: field.to_mont(&one),
        f: field,
    };
    let b = group.f.to_mont(&from_be(&hex(params.b), group.f.k));
    let Some(q) = group.decode(curve, point, &b) else {
        return false;
    };
    let Some((r, s)) = parse_signature(signature) else {
        return false;
    };
    let strip = super::bigint::strip;
    let (r, s) = (strip(r), strip(s));
    if r.is_empty() || s.is_empty() || r.len() > size || s.len() > size {
        return false;
    }
    let k = order.k;
    let rl = from_be(r, k);
    let sl = from_be(s, k);
    if ge(&rl, &order.m.n) || ge(&sl, &order.m.n) {
        return false;
    }
    // e: özetin en soldaki |n| biti (n'nin bit uzunluğu bayt sınırında).
    let e_bytes = &digest[..digest.len().min(size)];
    let mut e = from_be(e_bytes, k);
    if ge(&e, &order.m.n) {
        sub_in_place(&mut e, &order.m.n);
    }
    let w = order.invert(&order.to_mont(&sl));
    let u1 = order.from_mont(&order.mul(&order.to_mont(&e), &w));
    let u2 = order.from_mont(&order.mul(&order.to_mont(&rl), &w));
    let g = Group::decode(
        &group,
        curve,
        &[&[4u8][..], &hex(params.gx), &hex(params.gy)].concat(),
        &b,
    );
    let Some(g) = g else {
        return false;
    };
    let p = group.double_mul(&u1, &g, &u2, &q);
    if Ring::is_zero(&p.z) {
        return false;
    }
    let f = &group.f;
    let zi = f.invert(&p.z);
    let x = f.from_mont(&f.mul(&p.x, &f.mul(&zi, &zi)));
    let mut x = to_be(&x, size);
    // x mod n
    let mut xl = from_be(&x, k);
    if ge(&xl, &order.m.n) {
        sub_in_place(&mut xl, &order.m.n);
        x = to_be(&xl, size);
    }
    from_be(&x, k) == rl
}
