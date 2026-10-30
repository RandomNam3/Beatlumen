//! Ed25519 signatures (RFC 8032): the long-term identity of the receiver
//! in AirPlay 2 pairing (signing) and the senders' identities (verifying).
//! Points use extended twisted Edwards coordinates; scalar multiplication
//! and the scalar arithmetic run in constant time.

use super::curve25519::Fe;
use super::sha512::sha512;
use std::sync::OnceLock;

#[derive(Clone, Copy)]
struct Point {
    x: Fe,
    y: Fe,
    z: Fe,
    t: Fe,
}

struct Consts {
    d: Fe,
    d2: Fe,
    sqrt_m1: Fe,
    base: Point,
}

fn consts() -> &'static Consts {
    static C: OnceLock<Consts> = OnceLock::new();
    C.get_or_init(|| {
        let d = Fe::from_u64(121665)
            .neg()
            .mul(&Fe::from_u64(121666).invert());
        let sqrt_m1 = Fe::sqrt_m1();
        // y = 4/5 with an even x.
        let mut enc = [0x66u8; 32];
        enc[0] = 0x58;
        let base = decode_with(&enc, &d, &sqrt_m1).expect("base point");
        Consts {
            d,
            d2: d.add(&d),
            sqrt_m1,
            base,
        }
    })
}

const IDENTITY: Point = Point {
    x: Fe::ZERO,
    y: Fe::ONE,
    z: Fe::ONE,
    t: Fe::ZERO,
};

impl Point {
    /// Unified addition (also used for doubling).
    fn add(&self, q: &Point) -> Point {
        let c = consts();
        let a = self.y.sub(&self.x).mul(&q.y.sub(&q.x));
        let b = self.y.add(&self.x).mul(&q.y.add(&q.x));
        let cc = self.t.mul(&c.d2).mul(&q.t);
        let zz = self.z.mul(&q.z);
        let d = zz.add(&zz);
        let e = b.sub(&a);
        let f = d.sub(&cc);
        let g = d.add(&cc);
        let h = b.add(&a);
        Point {
            x: e.mul(&f),
            y: g.mul(&h),
            t: e.mul(&h),
            z: f.mul(&g),
        }
    }

    fn neg(&self) -> Point {
        Point {
            x: self.x.neg(),
            y: self.y,
            z: self.z,
            t: self.t.neg(),
        }
    }

    fn select(a: &Point, b: &Point, choice: u64) -> Point {
        Point {
            x: Fe::select(&a.x, &b.x, choice),
            y: Fe::select(&a.y, &b.y, choice),
            z: Fe::select(&a.z, &b.z, choice),
            t: Fe::select(&a.t, &b.t, choice),
        }
    }

    /// `[k]P` for a 256-bit little-endian scalar (double and always add).
    fn mul(&self, k: &[u8; 32]) -> Point {
        let mut r = IDENTITY;
        for i in (0..256).rev() {
            r = r.add(&r);
            let s = r.add(self);
            r = Point::select(&r, &s, (k[i / 8] >> (i % 8) & 1) as u64);
        }
        r
    }

    fn encode(&self) -> [u8; 32] {
        let zi = self.z.invert();
        let x = self.x.mul(&zi);
        let mut out = self.y.mul(&zi).to_bytes();
        out[31] |= (x.is_negative() as u8) << 7;
        out
    }
}

/// Decodes a point (RFC 8032 5.1.3); `None` for invalid encodings.
fn decode_with(b: &[u8; 32], d: &Fe, sqrt_m1: &Fe) -> Option<Point> {
    let sign = b[31] >> 7;
    let mut yb = *b;
    yb[31] &= 0x7f;
    let y = Fe::from_bytes(&yb);
    if y.to_bytes() != yb {
        return None; // y >= p
    }
    let yy = y.square();
    let u = yy.sub(&Fe::ONE);
    let v = d.mul(&yy).add(&Fe::ONE);
    let v3 = v.square().mul(&v);
    let v7 = v3.square().mul(&v);
    let mut x = u.mul(&v3).mul(&u.mul(&v7).pow_p58());
    let vxx = v.mul(&x.square());
    if !vxx.ct_eq(&u) {
        if vxx.ct_eq(&u.neg()) {
            x = x.mul(sqrt_m1);
        } else {
            return None;
        }
    }
    if x.is_zero() && sign == 1 {
        return None;
    }
    if x.is_negative() as u8 != sign {
        x = x.neg();
    }
    Some(Point {
        x,
        y,
        z: Fe::ONE,
        t: x.mul(&y),
    })
}

fn decode(b: &[u8; 32]) -> Option<Point> {
    let c = consts();
    decode_with(b, &c.d, &c.sqrt_m1)
}

// ---- scalars modulo L = 2^252 + 27742317777372353535851937790883648493 ----

const L: [u64; 4] = [
    0x5812631a5cf5d3ed,
    0x14def9dea2f79cd6,
    0,
    0x1000000000000000,
];

/// `a - b` and whether it borrowed.
fn sub4(a: &[u64; 4], b: &[u64; 4]) -> ([u64; 4], u64) {
    let mut out = [0u64; 4];
    let mut borrow = 0u64;
    for i in 0..4 {
        let (d1, b1) = a[i].overflowing_sub(b[i]);
        let (d2, b2) = d1.overflowing_sub(borrow);
        out[i] = d2;
        borrow = (b1 | b2) as u64;
    }
    (out, borrow)
}

/// Reduces a little-endian number of up to 512 bits modulo L, one bit at
/// a time (constant time).
fn reduce(bytes: &[u8]) -> [u64; 4] {
    let mut r = [0u64; 4];
    for i in (0..bytes.len() * 8).rev() {
        let bit = (bytes[i / 8] >> (i % 8) & 1) as u64;
        // r < L < 2^253, so 2r + 1 fits.
        r[3] = r[3] << 1 | r[2] >> 63;
        r[2] = r[2] << 1 | r[1] >> 63;
        r[1] = r[1] << 1 | r[0] >> 63;
        r[0] = r[0] << 1 | bit;
        let (d, borrow) = sub4(&r, &L);
        let keep = 0u64.wrapping_sub(borrow);
        for j in 0..4 {
            r[j] = (r[j] & keep) | (d[j] & !keep);
        }
    }
    r
}

fn scalar_bytes(s: &[u64; 4]) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (i, v) in s.iter().enumerate() {
        out[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    out
}

/// `(a * b + c) mod L`.
fn mul_add(a: &[u64; 4], b: &[u64; 4], c: &[u64; 4]) -> [u64; 4] {
    let mut wide = [0u64; 9];
    for i in 0..4 {
        let mut carry = 0u128;
        for j in 0..4 {
            let v = wide[i + j] as u128 + a[i] as u128 * b[j] as u128 + carry;
            wide[i + j] = v as u64;
            carry = v >> 64;
        }
        wide[i + 4] = carry as u64;
    }
    let mut carry = 0u128;
    for i in 0..9 {
        let v = wide[i] as u128 + if i < 4 { c[i] as u128 } else { 0 } + carry;
        wide[i] = v as u64;
        carry = v >> 64;
    }
    let mut bytes = [0u8; 72];
    for (i, v) in wide.iter().enumerate() {
        bytes[i * 8..i * 8 + 8].copy_from_slice(&v.to_le_bytes());
    }
    reduce(&bytes)
}

fn limbs(b: &[u8; 32]) -> [u64; 4] {
    core::array::from_fn(|i| u64::from_le_bytes(b[i * 8..i * 8 + 8].try_into().expect("8")))
}

/// Whether `s < L` (signature malleability check; `s` is public).
fn is_canonical(s: &[u8; 32]) -> bool {
    sub4(&limbs(s), &L).1 == 1
}

/// A private key, kept as the 32-byte seed of RFC 8032.
#[derive(Clone)]
pub struct SigningKey {
    scalar: [u8; 32],
    prefix: [u8; 32],
    public: [u8; 32],
}

impl SigningKey {
    pub fn from_seed(seed: &[u8; 32]) -> SigningKey {
        let h = sha512(&[seed]);
        let mut scalar = [0u8; 32];
        scalar.copy_from_slice(&h[..32]);
        scalar[0] &= 248;
        scalar[31] &= 127;
        scalar[31] |= 64;
        let mut prefix = [0u8; 32];
        prefix.copy_from_slice(&h[32..]);
        let public = consts().base.mul(&scalar).encode();
        SigningKey {
            scalar,
            prefix,
            public,
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.public
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        let r = reduce(&sha512(&[&self.prefix, msg]));
        let big_r = consts().base.mul(&scalar_bytes(&r)).encode();
        let k = reduce(&sha512(&[&big_r, &self.public, msg]));
        let a = reduce(&self.scalar);
        let s = mul_add(&k, &a, &r);
        let mut sig = [0u8; 64];
        sig[..32].copy_from_slice(&big_r);
        sig[32..].copy_from_slice(&scalar_bytes(&s));
        sig
    }
}

/// Checks a signature (cofactorless verification, RFC 8032 5.1.7).
pub fn verify(public: &[u8; 32], msg: &[u8], sig: &[u8]) -> bool {
    let Ok(sig) = <&[u8; 64]>::try_from(sig) else {
        return false;
    };
    let Some(a) = decode(public) else {
        return false;
    };
    let r: [u8; 32] = sig[..32].try_into().expect("32 bytes");
    let s: [u8; 32] = sig[32..].try_into().expect("32 bytes");
    if !is_canonical(&s) {
        return false;
    }
    let k = scalar_bytes(&reduce(&sha512(&[&r, public, msg])));
    let check = consts().base.mul(&s).add(&a.neg().mul(&k));
    check.encode() == r
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::hex;
    use crate::crypto::test_util::{unhex, unhex32};

    fn case(seed: &str, public: &str, msg: &str, sig: &str) {
        let key = SigningKey::from_seed(&unhex32(seed));
        assert_eq!(hex(&key.public_key()), public);
        let msg = unhex(msg);
        let s = key.sign(&msg);
        assert_eq!(hex(&s), sig);
        assert!(verify(&key.public_key(), &msg, &s));
        // Changed message, signature or key fail.
        let mut other = msg.clone();
        other.push(0);
        assert!(!verify(&key.public_key(), &other, &s));
        let mut bad = s;
        bad[10] ^= 4;
        assert!(!verify(&key.public_key(), &msg, &bad));
        let mut bad = s;
        bad[40] ^= 4;
        assert!(!verify(&key.public_key(), &msg, &bad));
        let k2 = SigningKey::from_seed(&[1; 32]);
        assert!(!verify(&k2.public_key(), &msg, &s));
    }

    #[test]
    fn rfc8032_vectors() {
        case(
            "9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60",
            "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a",
            "",
            "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e06522490155\
             5fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b",
        );
        case(
            "4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb",
            "3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c",
            "72",
            "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da\
             085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00",
        );
        case(
            "c5aa8df43f9f837bedb7442f31dcb7b166d38535076f094b85ce3a2e0b4458f7",
            "fc51cd8e6218a1a38da47ed00230f0580816ed13ba3303ac5deb911548908025",
            "af82",
            "6291d657deec24024827e69c3abe01a30ce548a284743a445e3680d7db5ac3ac\
             18ff9b538d16f290ae67f760984dc6594a7c15e9716ed28dc027beceea1ec40a",
        );
        case(
            "833fe62409237b9d62ec77587520911e9a759cec1d19755b7da901b96dca3d42",
            "ec172b93ad5e563bf4932c70e1245034c35467ef2efd4d64ebf819683467e2bf",
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f",
            "dc2a4459e7369633a52b1bf277839a00201009a3efbf3ecb69bea2186c26b589\
             09351fc9ac90b3ecfdfbc7c66431e0303dca179c138ac17ad9bef1177331a704",
        );
    }

    #[test]
    fn rejects_malformed_input() {
        let key = SigningKey::from_seed(&[3; 32]);
        let s = key.sign(b"msg");
        // s + L is the same scalar but not canonical.
        let mut high = s;
        let (sum, _) = {
            let a = limbs(&s[32..].try_into().unwrap());
            let mut out = [0u64; 4];
            let mut carry = 0u128;
            for i in 0..4 {
                let v = a[i] as u128 + L[i] as u128 + carry;
                out[i] = v as u64;
                carry = v >> 64;
            }
            (out, carry)
        };
        high[32..].copy_from_slice(&scalar_bytes(&sum));
        assert!(!verify(&key.public_key(), b"msg", &high));
        assert!(!verify(&key.public_key(), b"msg", &s[..63]));
        // A public key that is not a curve point.
        let mut bad = key.public_key();
        bad[0] ^= 1;
        assert!(decode(&bad).is_none() || !verify(&bad, b"msg", &s));
        assert!(decode(&[0xff; 32]).is_none());
    }

    #[test]
    fn scalar_reduction() {
        // L reduces to zero, L - 1 stays.
        assert_eq!(reduce(&scalar_bytes(&L)), [0; 4]);
        let mut lm1 = L;
        lm1[0] -= 1;
        assert_eq!(reduce(&scalar_bytes(&lm1)), lm1);
        // (L - 1)^2 = 1 mod L.
        assert_eq!(mul_add(&lm1, &lm1, &[0; 4]), [1, 0, 0, 0]);
    }
}
