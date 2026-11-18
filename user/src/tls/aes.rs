//! AES-128 ve GCM (TLS_ECDHE_RSA_WITH_AES_128_GCM_SHA256 kayıt şifrelemesi).

use alloc::vec::Vec;

const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

fn xtime(b: u8) -> u8 {
    (b << 1) ^ if b & 0x80 != 0 { 0x1b } else { 0 }
}

#[derive(Clone)]
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Self {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[4 * i..4 * i + 4]);
        }
        let mut rcon = 1u8;
        for i in 4..44 {
            let mut t = w[i - 1];
            if i % 4 == 0 {
                t = [
                    SBOX[t[1] as usize],
                    SBOX[t[2] as usize],
                    SBOX[t[3] as usize],
                    SBOX[t[0] as usize],
                ];
                t[0] ^= rcon;
                rcon = xtime(rcon);
            }
            for j in 0..4 {
                w[i][j] = w[i - 4][j] ^ t[j];
            }
        }
        let mut round_keys = [[0u8; 16]; 11];
        for (r, rk) in round_keys.iter_mut().enumerate() {
            for c in 0..4 {
                rk[4 * c..4 * c + 4].copy_from_slice(&w[4 * r + c]);
            }
        }
        Self { round_keys }
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        let add = |s: &mut [u8; 16], k: &[u8; 16]| {
            for i in 0..16 {
                s[i] ^= k[i];
            }
        };
        add(block, &self.round_keys[0]);
        for (round, key) in self.round_keys.iter().enumerate().skip(1) {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            // ShiftRows (sütun öncelikli durum: bayt = s[satır + 4·sütun]).
            let s = *block;
            for r in 1..4 {
                for c in 0..4 {
                    block[r + 4 * c] = s[r + 4 * ((c + r) % 4)];
                }
            }
            if round != 10 {
                for c in 0..4 {
                    let a = [
                        block[4 * c],
                        block[4 * c + 1],
                        block[4 * c + 2],
                        block[4 * c + 3],
                    ];
                    let all = a[0] ^ a[1] ^ a[2] ^ a[3];
                    for r in 0..4 {
                        block[4 * c + r] = a[r] ^ all ^ xtime(a[r] ^ a[(r + 1) % 4]);
                    }
                }
            }
            add(block, key);
        }
    }
}

/// GF(2¹²⁸) çarpımı (GCM bit sırası).
fn gf_mul(x: u128, y: u128) -> u128 {
    let r: u128 = 0xe1 << 120;
    let mut z = 0u128;
    let mut v = y;
    for i in 0..128 {
        if x >> (127 - i) & 1 != 0 {
            z ^= v;
        }
        v = if v & 1 != 0 { (v >> 1) ^ r } else { v >> 1 };
    }
    z
}

/// H'nin 4 bitlik çarpım tablosu (Shoup): GHASH'i bit döngüsünden ~10 kat hızlı yapar.
struct Ghash {
    table: [u128; 16],
}

impl Ghash {
    fn new(h: u128) -> Self {
        let mut table = [0u128; 16];
        for (i, t) in table.iter_mut().enumerate() {
            // i'nin bitleri x'in en yüksek 4 bitine karşılık gelir.
            *t = gf_mul((i as u128) << 124, h);
        }
        Self { table }
    }

    /// x·H: Horner yöntemi. GCM'de u128'in düşük bitleri polinomun yüksek dereceleridir;
    /// en yüksek dereceli dörtlükten başlanır: z = z·x⁴ + T[dörtlük].
    fn mul(&self, x: u128) -> u128 {
        let mut z = 0u128;
        for k in 0..32 {
            for _ in 0..4 {
                z = if z & 1 != 0 {
                    (z >> 1) ^ (0xe1 << 120)
                } else {
                    z >> 1
                };
            }
            z ^= self.table[((x >> (4 * k)) & 0xf) as usize];
        }
        z
    }

    fn update(&self, y: &mut u128, data: &[u8]) {
        for chunk in data.chunks(16) {
            let mut block = [0u8; 16];
            block[..chunk.len()].copy_from_slice(chunk);
            *y = self.mul(*y ^ u128::from_be_bytes(block));
        }
    }
}

pub struct Gcm {
    aes: Aes128,
    ghash: Ghash,
}

impl Gcm {
    pub fn new(key: &[u8; 16]) -> Self {
        let aes = Aes128::new(key);
        let mut h = [0u8; 16];
        aes.encrypt_block(&mut h);
        Self {
            ghash: Ghash::new(u128::from_be_bytes(h)),
            aes,
        }
    }

    fn ctr(&self, nonce: &[u8; 12], start: u32, data: &mut [u8]) {
        let mut counter = start;
        for chunk in data.chunks_mut(16) {
            let mut block = [0u8; 16];
            block[..12].copy_from_slice(nonce);
            block[12..].copy_from_slice(&counter.to_be_bytes());
            self.aes.encrypt_block(&mut block);
            for (d, k) in chunk.iter_mut().zip(block) {
                *d ^= k;
            }
            counter = counter.wrapping_add(1);
        }
    }

    fn tag(&self, nonce: &[u8; 12], aad: &[u8], ciphertext: &[u8]) -> [u8; 16] {
        let mut y = 0u128;
        self.ghash.update(&mut y, aad);
        self.ghash.update(&mut y, ciphertext);
        let lengths = ((aad.len() as u128 * 8) << 64) | (ciphertext.len() as u128 * 8);
        y = self.ghash.mul(y ^ lengths);
        let mut j0 = [0u8; 16];
        j0[..12].copy_from_slice(nonce);
        j0[15] = 1;
        self.aes.encrypt_block(&mut j0);
        (y ^ u128::from_be_bytes(j0)).to_be_bytes()
    }

    /// Şifreler; çıktı = şifreli metin || 16 baytlık etiket.
    pub fn seal(&self, nonce: &[u8; 12], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
        let mut out = plaintext.to_vec();
        self.ctr(nonce, 2, &mut out);
        let tag = self.tag(nonce, aad, &out);
        out.extend_from_slice(&tag);
        out
    }

    /// Etiketi doğrular ve çözer; etiket tutmazsa `None`.
    pub fn open(&self, nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
        if sealed.len() < 16 {
            return None;
        }
        let (ciphertext, tag) = sealed.split_at(sealed.len() - 16);
        let expected = self.tag(nonce, aad, ciphertext);
        if !crate::crypto::constant_time_eq(&expected, tag) {
            return None;
        }
        let mut out = ciphertext.to_vec();
        self.ctr(nonce, 2, &mut out);
        Some(out)
    }
}
