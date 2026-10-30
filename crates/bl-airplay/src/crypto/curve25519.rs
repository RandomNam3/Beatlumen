//! Arithmetic modulo 2^255 - 19 and X25519 (RFC 7748), the key exchange of
//! AirPlay 2 pair-verify. Field elements are five 51-bit limbs; all
//! operations on secret data run without data-dependent branches or
//! memory accesses.

const MASK: u64 = (1 << 51) - 1;

/// A field element (limbs below 2^52 after every operation).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Fe(pub(crate) [u64; 5]);

impl Fe {
    pub(crate) const ZERO: Fe = Fe([0; 5]);
    pub(crate) const ONE: Fe = Fe([1, 0, 0, 0, 0]);

    pub(crate) fn from_u64(v: u64) -> Fe {
        Fe::carry([v, 0, 0, 0, 0])
    }

    /// Little-endian bytes; the top bit is ignored.
    pub(crate) fn from_bytes(b: &[u8; 32]) -> Fe {
        let load = |i: usize| u64::from_le_bytes(b[i..i + 8].try_into().expect("8 bytes"));
        Fe([
            load(0) & MASK,
            (load(6) >> 3) & MASK,
            (load(12) >> 6) & MASK,
            (load(19) >> 1) & MASK,
            (load(24) >> 12) & MASK,
        ])
    }

    /// The canonical (fully reduced) little-endian encoding.
    pub(crate) fn to_bytes(self) -> [u8; 32] {
        let mut l = Fe::carry(self.0).0;
        // Subtract p if the value is at least p: q is 1 exactly then.
        let mut q = (l[0] + 19) >> 51;
        for &v in &l[1..] {
            q = (v + q) >> 51;
        }
        l[0] += 19 * q;
        for i in 0..4 {
            l[i + 1] += l[i] >> 51;
            l[i] &= MASK;
        }
        l[4] &= MASK;
        let mut out = [0u8; 32];
        let mut acc: u128 = 0;
        let mut bits = 0;
        let mut pos = 0;
        for &v in &l {
            acc |= (v as u128) << bits;
            bits += 51;
            while bits >= 8 {
                out[pos] = acc as u8;
                pos += 1;
                acc >>= 8;
                bits -= 8;
            }
        }
        out[pos] = acc as u8;
        out
    }

    /// Propagates carries (input limbs below 2^64).
    fn carry(l: [u64; 5]) -> Fe {
        let c = l.map(|v| v >> 51);
        Fe([
            (l[0] & MASK) + c[4] * 19,
            (l[1] & MASK) + c[0],
            (l[2] & MASK) + c[1],
            (l[3] & MASK) + c[2],
            (l[4] & MASK) + c[3],
        ])
    }

    pub(crate) fn add(&self, b: &Fe) -> Fe {
        Fe::carry(core::array::from_fn(|i| self.0[i] + b.0[i]))
    }

    pub(crate) fn sub(&self, b: &Fe) -> Fe {
        // Adds 16p first, so no limb goes negative.
        const P16: [u64; 5] = [
            0x7ffffffffffed0,
            0x7ffffffffffff0,
            0x7ffffffffffff0,
            0x7ffffffffffff0,
            0x7ffffffffffff0,
        ];
        Fe::carry(core::array::from_fn(|i| self.0[i] + P16[i] - b.0[i]))
    }

    pub(crate) fn neg(&self) -> Fe {
        Fe::ZERO.sub(self)
    }

    pub(crate) fn mul(&self, b: &Fe) -> Fe {
        let m = |x: u64, y: u64| x as u128 * y as u128;
        let a = &self.0;
        let b = &b.0;
        let b19: [u64; 5] = b.map(|v| v * 19);
        let c0 =
            m(a[0], b[0]) + m(a[4], b19[1]) + m(a[3], b19[2]) + m(a[2], b19[3]) + m(a[1], b19[4]);
        let mut c1 =
            m(a[1], b[0]) + m(a[0], b[1]) + m(a[4], b19[2]) + m(a[3], b19[3]) + m(a[2], b19[4]);
        let mut c2 =
            m(a[2], b[0]) + m(a[1], b[1]) + m(a[0], b[2]) + m(a[4], b19[3]) + m(a[3], b19[4]);
        let mut c3 =
            m(a[3], b[0]) + m(a[2], b[1]) + m(a[1], b[2]) + m(a[0], b[3]) + m(a[4], b19[4]);
        let mut c4 = m(a[4], b[0]) + m(a[3], b[1]) + m(a[2], b[2]) + m(a[1], b[3]) + m(a[0], b[4]);
        let mut out = [0u64; 5];
        c1 += c0 >> 51;
        out[0] = c0 as u64 & MASK;
        c2 += c1 >> 51;
        out[1] = c1 as u64 & MASK;
        c3 += c2 >> 51;
        out[2] = c2 as u64 & MASK;
        c4 += c3 >> 51;
        out[3] = c3 as u64 & MASK;
        let carry = (c4 >> 51) as u64;
        out[4] = c4 as u64 & MASK;
        out[0] += carry * 19;
        out[1] += out[0] >> 51;
        out[0] &= MASK;
        Fe(out)
    }

    pub(crate) fn square(&self) -> Fe {
        self.mul(self)
    }

    /// `self^e` for a public exponent (little-endian bytes).
    fn pow(&self, e: &[u8; 32]) -> Fe {
        let mut r = Fe::ONE;
        for i in (0..256).rev() {
            r = r.square();
            if e[i / 8] >> (i % 8) & 1 == 1 {
                r = r.mul(self);
            }
        }
        r
    }

    /// `self^(p-2)`, the inverse (zero for zero).
    pub(crate) fn invert(&self) -> Fe {
        let mut e = [0xffu8; 32];
        e[0] = 0xeb;
        e[31] = 0x7f;
        self.pow(&e)
    }

    /// `self^((p-5)/8)`, used for square roots.
    pub(crate) fn pow_p58(&self) -> Fe {
        let mut e = [0xffu8; 32];
        e[0] = 0xfd;
        e[31] = 0x0f;
        self.pow(&e)
    }

    /// A square root of -1.
    pub(crate) fn sqrt_m1() -> Fe {
        let mut e = [0xffu8; 32];
        e[0] = 0xfb;
        e[31] = 0x1f;
        Fe::from_u64(2).pow(&e)
    }

    pub(crate) fn is_zero(&self) -> bool {
        self.to_bytes().iter().fold(0u8, |a, &b| a | b) == 0
    }

    pub(crate) fn is_negative(&self) -> bool {
        self.to_bytes()[0] & 1 == 1
    }

    pub(crate) fn ct_eq(&self, b: &Fe) -> bool {
        self.sub(b).is_zero()
    }

    /// `b` when `choice` is 1, `a` when it is 0.
    pub(crate) fn select(a: &Fe, b: &Fe, choice: u64) -> Fe {
        let mask = 0u64.wrapping_sub(choice);
        Fe(core::array::from_fn(|i| {
            a.0[i] ^ (mask & (a.0[i] ^ b.0[i]))
        }))
    }

    fn cswap(a: &mut Fe, b: &mut Fe, choice: u64) {
        let mask = 0u64.wrapping_sub(choice);
        for i in 0..5 {
            let t = mask & (a.0[i] ^ b.0[i]);
            a.0[i] ^= t;
            b.0[i] ^= t;
        }
    }
}

/// X25519 (RFC 7748 5): scalar multiplication of the u-coordinate `u` by
/// the clamped scalar `k`.
pub fn x25519(k: &[u8; 32], u: &[u8; 32]) -> [u8; 32] {
    let mut k = *k;
    k[0] &= 248;
    k[31] &= 127;
    k[31] |= 64;
    let x1 = Fe::from_bytes(u);
    let a24 = Fe::from_u64(121665);
    let (mut x2, mut z2, mut x3, mut z3) = (Fe::ONE, Fe::ZERO, x1, Fe::ONE);
    let mut swap = 0u64;
    for t in (0..255).rev() {
        let bit = (k[t / 8] >> (t % 8) & 1) as u64;
        swap ^= bit;
        Fe::cswap(&mut x2, &mut x3, swap);
        Fe::cswap(&mut z2, &mut z3, swap);
        swap = bit;
        let a = x2.add(&z2);
        let aa = a.square();
        let b = x2.sub(&z2);
        let bb = b.square();
        let e = aa.sub(&bb);
        let c = x3.add(&z3);
        let d = x3.sub(&z3);
        let da = d.mul(&a);
        let cb = c.mul(&b);
        x3 = da.add(&cb).square();
        z3 = x1.mul(&da.sub(&cb).square());
        x2 = aa.mul(&bb);
        z2 = e.mul(&aa.add(&a24.mul(&e)));
    }
    Fe::cswap(&mut x2, &mut x3, swap);
    Fe::cswap(&mut z2, &mut z3, swap);
    x2.mul(&z2.invert()).to_bytes()
}

/// The public key of a private key.
pub fn x25519_base(k: &[u8; 32]) -> [u8; 32] {
    let mut nine = [0u8; 32];
    nine[0] = 9;
    x25519(k, &nine)
}

/// The shared secret with a peer's public key; `None` for low-order
/// points (an all-zero result, RFC 7748 6.1).
pub fn x25519_shared(k: &[u8; 32], peer: &[u8; 32]) -> Option<[u8; 32]> {
    let s = x25519(k, peer);
    (s.iter().fold(0u8, |a, &b| a | b) != 0).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::hex;
    use crate::crypto::test_util::unhex32;

    #[test]
    fn field_basics() {
        let a = Fe::from_bytes(&unhex32(
            "e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c",
        ));
        assert!(a.mul(&a.invert()).ct_eq(&Fe::ONE));
        assert!(a.sub(&a).is_zero());
        assert!(a.add(&a.neg()).is_zero());
        let i = Fe::sqrt_m1();
        assert!(i.square().ct_eq(&Fe::ONE.neg()));
        // p itself encodes as zero; p + 1 as one.
        let mut p = [0xffu8; 32];
        p[0] = 0xed;
        p[31] = 0x7f;
        assert!(Fe::from_bytes(&p).is_zero());
        p[0] = 0xee;
        assert_eq!(Fe::from_bytes(&p).to_bytes(), Fe::ONE.to_bytes());
    }

    #[test]
    fn x25519_rfc7748_vectors() {
        let k = unhex32("a546e36bf0527c9d3b16154b82465edd62144c0ac1fc5a18506a2244ba449ac4");
        let u = unhex32("e6db6867583030db3594c1a424b15f7c726624ec26b3353b10a903a6d0ab1c4c");
        assert_eq!(
            hex(&x25519(&k, &u)),
            "c3da55379de9c6908e94ea4df28d084f32eccf03491c71f754b4075577a28552"
        );
        let k = unhex32("4b66e9d4d1b4673c5ad22691957d6af5c11b6421e0ea01d42ca4169e7918ba0d");
        let u = unhex32("e5210f12786811d3f4b7959d0538ae2c31dbe7106fc03c3efc4cd549c715a493");
        assert_eq!(
            hex(&x25519(&k, &u)),
            "95cbde9476e8907d7aade45cb4b873f88b595a68799fa152e6f8f7647aac7957"
        );
    }

    #[test]
    fn x25519_iterated() {
        let mut k = [0u8; 32];
        k[0] = 9;
        let mut u = k;
        for i in 1..=1000 {
            let r = x25519(&k, &u);
            u = k;
            k = r;
            if i == 1 {
                assert_eq!(
                    hex(&k),
                    "422c8e7a6227d7bca1350b3e2bb7279f7897b87bb6854b783c60e80311ae3079"
                );
            }
        }
        assert_eq!(
            hex(&k),
            "684cf59ba83309552800ef566f2f4d3c1c3887c49360e3875f2eb94d99532c51"
        );
    }

    #[test]
    fn x25519_diffie_hellman() {
        let a = unhex32("77076d0a7318a57d3c16c17251b26645df4c2f87ebc0992ab177fba51db92c2a");
        let b = unhex32("5dab087e624a8a4b79e17f8b83800ee66f3bb1292618b6fd1c2f8b27ff88e0eb");
        let pa = x25519_base(&a);
        let pb = x25519_base(&b);
        assert_eq!(
            hex(&pa),
            "8520f0098930a754748b7ddcb43ef75a0dbf3a0d26381af4eba4a98eaa9b4e6a"
        );
        assert_eq!(
            hex(&pb),
            "de9edb7d7b7dc1b4d35b61c2ece435373f8343c85b78674dadfc7e146f882b4f"
        );
        let k = "4a5d9d5ba4ce2de1728e3bf480350f25e07e21c947d19e3376f09b3c1e161742";
        assert_eq!(hex(&x25519_shared(&a, &pb).unwrap()), k);
        assert_eq!(hex(&x25519_shared(&b, &pa).unwrap()), k);
        // The zero point is rejected.
        assert!(x25519_shared(&a, &[0; 32]).is_none());
    }
}
