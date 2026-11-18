//! NIST P-256 (secp256r1) ECDH: Jacobian koordinatlar, Montgomery alan aritmetiği.

type Limbs = [u64; 4];

const P: Limbs = [
    0xffff_ffff_ffff_ffff,
    0x0000_0000_ffff_ffff,
    0x0000_0000_0000_0000,
    0xffff_ffff_0000_0001,
];
/// Grup mertebesi n.
const N: Limbs = [
    0xf3b9_cac2_fc63_2551,
    0xbce6_faad_a717_9e84,
    0xffff_ffff_ffff_ffff,
    0xffff_ffff_0000_0000,
];
const B: [u8; 32] = hex32("5ac635d8aa3a93e7b3ebbd55769886bc651d06b0cc53b0f63bce3c3e27d2604b");
const GX: [u8; 32] = hex32("6b17d1f2e12c4247f8bce6e563a440f277037d812deb33a0f4a13945d898c296");
const GY: [u8; 32] = hex32("4fe342e2fe1a7f9b8ee7eb4a7c0f9e162bce33576b315ececbb6406837bf51f5");

const fn hex32(s: &str) -> [u8; 32] {
    let b = s.as_bytes();
    let mut out = [0u8; 32];
    let mut i = 0;
    while i < 32 {
        out[i] = (nibble(b[2 * i]) << 4) | nibble(b[2 * i + 1]);
        i += 1;
    }
    out
}

const fn nibble(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

fn from_be(bytes: &[u8; 32]) -> Limbs {
    let mut out = [0u64; 4];
    for (i, limb) in out.iter_mut().enumerate() {
        let start = 32 - 8 * (i + 1);
        *limb = u64::from_be_bytes(bytes[start..start + 8].try_into().unwrap());
    }
    out
}

fn to_be(l: &Limbs) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, limb) in l.iter().enumerate() {
        let start = 32 - 8 * (i + 1);
        out[start..start + 8].copy_from_slice(&limb.to_be_bytes());
    }
    out
}

fn ge(a: &Limbs, b: &Limbs) -> bool {
    for i in (0..4).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn sub_raw(a: &Limbs, b: &Limbs) -> (Limbs, bool) {
    let mut out = [0u64; 4];
    let mut borrow = false;
    for i in 0..4 {
        let (d1, o1) = a[i].overflowing_sub(b[i]);
        let (d2, o2) = d1.overflowing_sub(borrow as u64);
        out[i] = d2;
        borrow = o1 | o2;
    }
    (out, borrow)
}

fn add_raw(a: &Limbs, b: &Limbs) -> (Limbs, bool) {
    let mut out = [0u64; 4];
    let mut carry = false;
    for i in 0..4 {
        let (s1, o1) = a[i].overflowing_add(b[i]);
        let (s2, o2) = s1.overflowing_add(carry as u64);
        out[i] = s2;
        carry = o1 | o2;
    }
    (out, carry)
}

/// Montgomery biçiminde alan elemanı (a·R mod p).
#[derive(Clone, Copy, PartialEq, Eq)]
struct Fe(Limbs);

impl Fe {
    const ZERO: Fe = Fe([0; 4]);

    fn add(self, o: Fe) -> Fe {
        let (s, carry) = add_raw(&self.0, &o.0);
        if carry || ge(&s, &P) {
            Fe(sub_raw(&s, &P).0)
        } else {
            Fe(s)
        }
    }

    fn sub(self, o: Fe) -> Fe {
        let (d, borrow) = sub_raw(&self.0, &o.0);
        if borrow { Fe(add_raw(&d, &P).0) } else { Fe(d) }
    }

    /// a·b·R⁻¹ mod p (-p⁻¹ mod 2⁶⁴ = 1 çünkü p ≡ -1 mod 2⁶⁴).
    fn mul(self, o: Fe) -> Fe {
        let (a, b) = (self.0, o.0);
        let mut t = [0u64; 6];
        for &bi in &b {
            let mut carry = 0u128;
            for j in 0..4 {
                let v = t[j] as u128 + a[j] as u128 * bi as u128 + carry;
                t[j] = v as u64;
                carry = v >> 64;
            }
            let v = t[4] as u128 + carry;
            t[4] = v as u64;
            t[5] = (v >> 64) as u64;
            let m = t[0];
            let v = t[0] as u128 + m as u128 * P[0] as u128;
            let mut carry = v >> 64;
            for j in 1..4 {
                let v = t[j] as u128 + m as u128 * P[j] as u128 + carry;
                t[j - 1] = v as u64;
                carry = v >> 64;
            }
            let v = t[4] as u128 + carry;
            t[3] = v as u64;
            t[4] = t[5] + (v >> 64) as u64;
            t[5] = 0;
        }
        let out = [t[0], t[1], t[2], t[3]];
        if t[4] != 0 || ge(&out, &P) {
            Fe(sub_raw(&out, &P).0)
        } else {
            Fe(out)
        }
    }

    fn square(self) -> Fe {
        self.mul(self)
    }

    fn is_zero(self) -> bool {
        self.0 == [0; 4]
    }
}

struct Field {
    r2: Fe,
    one: Fe,
}

impl Field {
    fn new() -> Self {
        // R² mod p: 1'i 512 kez ikiye katla.
        let mut r: Limbs = [1, 0, 0, 0];
        for _ in 0..512 {
            let (s, carry) = add_raw(&r, &r);
            r = if carry || ge(&s, &P) {
                sub_raw(&s, &P).0
            } else {
                s
            };
        }
        let r2 = Fe(r);
        let one = Fe([1, 0, 0, 0]).mul(r2);
        Self { r2, one }
    }

    /// Bayt dizisi → Montgomery biçimi; p'den büyükse `None`.
    fn decode_fe(&self, b: &[u8; 32]) -> Option<Fe> {
        let l = from_be(b);
        if ge(&l, &P) {
            return None;
        }
        Some(Fe(l).mul(self.r2))
    }

    fn to_bytes(&self, f: Fe) -> [u8; 32] {
        to_be(&f.mul(Fe([1, 0, 0, 0])).0)
    }

    /// a^(p-2) (Fermat).
    fn invert(&self, a: Fe) -> Fe {
        let (e, _) = sub_raw(&P, &[2, 0, 0, 0]);
        let mut r = self.one;
        for i in (0..256).rev() {
            r = r.square();
            if e[i / 64] >> (i % 64) & 1 != 0 {
                r = r.mul(a);
            }
        }
        r
    }
}

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
}

impl Point {
    fn infinity() -> Self {
        Point {
            x: Fe::ZERO,
            y: Fe::ZERO,
            z: Fe::ZERO,
        }
    }

    fn is_infinity(&self) -> bool {
        self.z.is_zero()
    }

    fn double(&self) -> Point {
        if self.is_infinity() || self.y.is_zero() {
            return Point::infinity();
        }
        let delta = self.z.square();
        let gamma = self.y.square();
        let beta = self.x.mul(gamma);
        let t = self.x.sub(delta).mul(self.x.add(delta));
        let alpha = t.add(t).add(t);
        let beta4 = beta.add(beta).add(beta.add(beta));
        let x3 = alpha.square().sub(beta4.add(beta4));
        let z3 = self.y.add(self.z).square().sub(gamma).sub(delta);
        let g2 = gamma.square();
        let g8 = {
            let g2 = g2.add(g2);
            let g4 = g2.add(g2);
            g4.add(g4)
        };
        let y3 = alpha.mul(beta4.sub(x3)).sub(g8);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    fn add(&self, o: &Point) -> Point {
        if self.is_infinity() {
            return *o;
        }
        if o.is_infinity() {
            return *self;
        }
        let z1z1 = self.z.square();
        let z2z2 = o.z.square();
        let u1 = self.x.mul(z2z2);
        let u2 = o.x.mul(z1z1);
        let s1 = self.y.mul(o.z).mul(z2z2);
        let s2 = o.y.mul(self.z).mul(z1z1);
        let h = u2.sub(u1);
        let r = s2.sub(s1);
        if h.is_zero() {
            return if r.is_zero() {
                self.double()
            } else {
                Point::infinity()
            };
        }
        let r = r.add(r);
        let i = h.add(h).square();
        let j = h.mul(i);
        let v = u1.mul(i);
        let x3 = r.square().sub(j).sub(v.add(v));
        let s1j = s1.mul(j);
        let y3 = r.mul(v.sub(x3)).sub(s1j.add(s1j));
        let z3 = self.z.add(o.z).square().sub(z1z1).sub(z2z2).mul(h);
        Point {
            x: x3,
            y: y3,
            z: z3,
        }
    }

    fn mul(&self, k: &[u8; 32]) -> Point {
        let mut r = Point::infinity();
        for byte in k {
            for bit in (0..8).rev() {
                r = r.double();
                if byte >> bit & 1 != 0 {
                    r = r.add(self);
                }
            }
        }
        r
    }

    fn affine(&self, f: &Field) -> Option<([u8; 32], [u8; 32])> {
        if self.is_infinity() {
            return None;
        }
        let zi = f.invert(self.z);
        let zi2 = zi.square();
        let x = self.x.mul(zi2);
        let y = self.y.mul(zi2.mul(zi));
        Some((f.to_bytes(x), f.to_bytes(y)))
    }
}

/// Noktayı çözer ve eğri üzerinde olduğunu doğrular (0x04 || X || Y).
fn decode(f: &Field, bytes: &[u8]) -> Option<Point> {
    if bytes.len() != 65 || bytes[0] != 4 {
        return None;
    }
    let x = f.decode_fe(bytes[1..33].try_into().ok()?)?;
    let y = f.decode_fe(bytes[33..65].try_into().ok()?)?;
    let b = f.decode_fe(&B)?;
    // y² = x³ - 3x + b
    let rhs = x.square().mul(x).sub(x.add(x).add(x)).add(b);
    if y.square() != rhs {
        return None;
    }
    Some(Point { x, y, z: f.one })
}

fn generator(f: &Field) -> Point {
    Point {
        x: f.decode_fe(&GX).unwrap(),
        y: f.decode_fe(&GY).unwrap(),
        z: f.one,
    }
}

/// Gizli skaleri geçerli aralığa getirir: 1 ≤ d < n.
pub fn clamp_secret(secret: &mut [u8; 32]) {
    secret[0] &= 0x7f;
    if secret.iter().all(|&b| b == 0) {
        secret[31] = 1;
    }
    debug_assert!(!ge(&from_be(secret), &N));
}

/// Açık anahtar: d·G, sıkıştırılmamış 65 bayt.
pub fn public_key(secret: &[u8; 32]) -> [u8; 65] {
    let f = Field::new();
    let (x, y) = generator(&f).mul(secret).affine(&f).unwrap();
    let mut out = [0u8; 65];
    out[0] = 4;
    out[1..33].copy_from_slice(&x);
    out[33..].copy_from_slice(&y);
    out
}

/// Ortak sır: (d·Q).x; Q geçersizse `None`.
pub fn shared_secret(secret: &[u8; 32], peer: &[u8]) -> Option<[u8; 32]> {
    let f = Field::new();
    let q = decode(&f, peer)?;
    q.mul(secret).affine(&f).map(|(x, _)| x)
}
