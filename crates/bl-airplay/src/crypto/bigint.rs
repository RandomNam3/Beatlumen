//! Modular arithmetic on big-endian byte strings: RSA with the (public)
//! RAOP key and SRP-6a of AirPlay 2 pairing. Numbers are little-endian
//! `u32` limbs, multiplication in Montgomery form.
//!
//! Exponentiation performs the same operations for every exponent bit
//! (the SRP exponents are secret); the Montgomery reduction itself has a
//! data-dependent final subtraction.

/// Montgomery context of an odd modulus.
struct Mont {
    m: Vec<u32>,
    n0: u32,
    /// R^2 mod m, R = 2^(32k).
    r2: Vec<u32>,
}

impl Mont {
    fn new(m: &[u8]) -> Mont {
        let m = from_be(m);
        let k = m.len();
        let n0 = neg_inv(m[0]);
        let mut r2 = vec![0u32; k];
        r2[0] = 1;
        for _ in 0..64 * k {
            double_mod(&mut r2, &m);
        }
        Mont { m, n0, r2 }
    }

    fn limbs(&self, x: &[u8]) -> Vec<u32> {
        let mut v = from_be(x);
        v.resize(self.m.len(), 0);
        v
    }

    fn mul(&self, a: &[u32], b: &[u32]) -> Vec<u32> {
        mont_mul(a, b, &self.m, self.n0)
    }

    /// Into Montgomery form.
    fn to_mont(&self, a: &[u32]) -> Vec<u32> {
        self.mul(a, &self.r2)
    }

    /// Out of Montgomery form.
    fn demont(&self, a: &[u32]) -> Vec<u32> {
        let mut one = vec![0u32; self.m.len()];
        one[0] = 1;
        self.mul(a, &one)
    }

    fn bytes(&self, a: &[u32]) -> Vec<u8> {
        to_be(a, self.m.len() * 4)
    }
}

/// `base^exp mod m` for big-endian byte strings; `m` must be odd and
/// `base < m`. The result has the length of `m` rounded up to 4 bytes.
pub fn mod_pow(base: &[u8], exp: &[u8], m: &[u8]) -> Vec<u8> {
    let ctx = Mont::new(m);
    let mut one = vec![0u32; ctx.m.len()];
    one[0] = 1;
    let bm = ctx.to_mont(&ctx.limbs(base));
    let mut acc = ctx.to_mont(&one);
    for byte in exp {
        for bit in (0..8).rev() {
            acc = ctx.mul(&acc, &acc);
            let with = ctx.mul(&acc, &bm);
            let mask = 0u32.wrapping_sub((byte >> bit & 1) as u32);
            for (a, w) in acc.iter_mut().zip(&with) {
                *a ^= mask & (*a ^ w);
            }
        }
    }
    ctx.bytes(&ctx.demont(&acc))
}

/// `a * b mod m` (`a, b < m`, `m` odd).
pub fn mod_mul(a: &[u8], b: &[u8], m: &[u8]) -> Vec<u8> {
    let ctx = Mont::new(m);
    let ab = ctx.mul(&ctx.limbs(a), &ctx.limbs(b)); // a*b/R
    ctx.bytes(&ctx.mul(&ab, &ctx.r2))
}

/// `a + b mod m` (`a, b < m`).
pub fn mod_add(a: &[u8], b: &[u8], m: &[u8]) -> Vec<u8> {
    let mm = from_be(m);
    let k = mm.len();
    let (mut x, mut y) = (from_be(a), from_be(b));
    x.resize(k + 1, 0);
    y.resize(k + 1, 0);
    let mut carry = 0u64;
    for i in 0..=k {
        let s = x[i] as u64 + y[i] as u64 + carry;
        x[i] = s as u32;
        carry = s >> 32;
    }
    let mut m1 = mm.clone();
    m1.push(0);
    if geq(&x, &m1) {
        sub_in_place(&mut x, &m1);
    }
    to_be(&x[..k], k * 4)
}

/// `a - b mod m` (`a, b < m`).
pub fn mod_sub(a: &[u8], b: &[u8], m: &[u8]) -> Vec<u8> {
    let mm = from_be(m);
    let k = mm.len();
    let (mut x, mut y) = (from_be(a), from_be(b));
    x.resize(k, 0);
    y.resize(k, 0);
    if !geq(&x, &y) {
        // a + m - b
        let mut carry = 0u64;
        let mut ext = x.clone();
        ext.push(0);
        for i in 0..k {
            let s = ext[i] as u64 + mm[i] as u64 + carry;
            ext[i] = s as u32;
            carry = s >> 32;
        }
        ext[k] = carry as u32;
        y.push(0);
        sub_in_place(&mut ext, &y);
        return to_be(&ext[..k], k * 4);
    }
    sub_in_place(&mut x, &y);
    to_be(&x, k * 4)
}

/// `a < b` as numbers (leading zero bytes ignored).
pub fn less_than(a: &[u8], b: &[u8]) -> bool {
    let a = strip(a);
    let b = strip(b);
    a.len() < b.len() || (a.len() == b.len() && a < b)
}

/// The number without leading zero bytes.
pub fn strip(a: &[u8]) -> &[u8] {
    let first = a.iter().position(|&b| b != 0).unwrap_or(a.len());
    &a[first..]
}

/// Plain (non-modular) product.
#[cfg(test)]
pub fn mul(a: &[u8], b: &[u8]) -> Vec<u8> {
    let (x, y) = (from_be(a), from_be(b));
    let mut out = vec![0u32; x.len() + y.len()];
    for (i, &xi) in x.iter().enumerate() {
        let mut carry = 0u64;
        for (j, &yj) in y.iter().enumerate() {
            let s = out[i + j] as u64 + xi as u64 * yj as u64 + carry;
            out[i + j] = s as u32;
            carry = s >> 32;
        }
        out[i + y.len()] = carry as u32;
    }
    to_be(&out, out.len() * 4)
}

/// Plain (non-modular) sum.
#[cfg(test)]
pub fn add(a: &[u8], b: &[u8]) -> Vec<u8> {
    let n = a.len().max(b.len()) / 4 + 2;
    let (mut x, mut y) = (from_be(a), from_be(b));
    x.resize(n, 0);
    y.resize(n, 0);
    let mut carry = 0u64;
    for i in 0..n {
        let s = x[i] as u64 + y[i] as u64 + carry;
        x[i] = s as u32;
        carry = s >> 32;
    }
    to_be(&x, n * 4)
}

fn from_be(bytes: &[u8]) -> Vec<u32> {
    let mut limbs = vec![0u32; bytes.len().div_ceil(4).max(1)];
    for (i, &b) in bytes.iter().rev().enumerate() {
        limbs[i / 4] |= (b as u32) << (8 * (i % 4));
    }
    limbs
}

fn to_be(limbs: &[u32], len: usize) -> Vec<u8> {
    let mut out = vec![0u8; len];
    for i in 0..len {
        let limb = limbs.get(i / 4).copied().unwrap_or(0);
        out[len - 1 - i] = (limb >> (8 * (i % 4))) as u8;
    }
    out
}

/// `-m^-1 mod 2^32` for odd `m` (Newton iteration).
fn neg_inv(m: u32) -> u32 {
    let mut x: u32 = 1;
    for _ in 0..5 {
        x = x.wrapping_mul(2u32.wrapping_sub(m.wrapping_mul(x)));
    }
    x.wrapping_neg()
}

fn geq(a: &[u32], b: &[u32]) -> bool {
    for i in (0..a.len()).rev() {
        if a[i] != b[i] {
            return a[i] > b[i];
        }
    }
    true
}

fn sub_in_place(a: &mut [u32], b: &[u32]) {
    let mut borrow = 0i64;
    for i in 0..a.len() {
        let d = a[i] as i64 - b[i] as i64 - borrow;
        a[i] = d as u32;
        borrow = (d < 0) as i64;
    }
}

/// `x = 2x mod m` for `x < m`.
fn double_mod(x: &mut [u32], m: &[u32]) {
    let mut carry = 0u32;
    for limb in x.iter_mut() {
        let next = *limb >> 31;
        *limb = *limb << 1 | carry;
        carry = next;
    }
    if carry == 1 || geq(x, m) {
        sub_in_place(x, m);
    }
}

/// Montgomery product `a * b * R^-1 mod m` (CIOS method).
fn mont_mul(a: &[u32], b: &[u32], m: &[u32], n0: u32) -> Vec<u32> {
    let k = m.len();
    let mut t = vec![0u32; k + 2];
    for &ai in a.iter().take(k) {
        let mut carry = 0u64;
        for j in 0..k {
            let s = t[j] as u64 + ai as u64 * b[j] as u64 + carry;
            t[j] = s as u32;
            carry = s >> 32;
        }
        let s = t[k] as u64 + carry;
        t[k] = s as u32;
        t[k + 1] = (s >> 32) as u32;
        let u = t[0].wrapping_mul(n0);
        let s = t[0] as u64 + u as u64 * m[0] as u64;
        let mut carry = s >> 32;
        for j in 1..k {
            let s = t[j] as u64 + u as u64 * m[j] as u64 + carry;
            t[j - 1] = s as u32;
            carry = s >> 32;
        }
        let s = t[k] as u64 + carry;
        t[k - 1] = s as u32;
        let s = t[k + 1] as u64 + (s >> 32);
        t[k] = s as u32;
        t[k + 1] = 0;
    }
    let mut r = t[..k].to_vec();
    if t[k] != 0 || geq(&r, m) {
        sub_in_place(&mut r, m);
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;

    fn num(v: u128, len: usize) -> Vec<u8> {
        v.to_be_bytes()[16 - len..].to_vec()
    }

    #[test]
    fn small_powers() {
        // 4^13 mod 497 = 445
        assert_eq!(mod_pow(&num(4, 2), &num(13, 1), &num(497, 2)), num(445, 4));
        // Fermat: 2^(p-1) mod p = 1 for the prime 2^61-1.
        let p: u128 = (1 << 61) - 1;
        assert_eq!(mod_pow(&num(2, 8), &num(p - 1, 8), &num(p, 8)), num(1, 8));
        // 3^1000003 mod (2^89-1)
        let m: u128 = (1 << 89) - 1;
        let mut want: u128 = 1;
        let mut b: u128 = 3;
        let mut e: u128 = 1000003;
        let mulmod = |x: u128, y: u128| -> u128 {
            // Shift-and-add to avoid overflow.
            let (mut r, mut x, mut y) = (0u128, x % m, y);
            while y > 0 {
                if y & 1 == 1 {
                    r = (r + x) % m;
                }
                x = (x << 1) % m;
                y >>= 1;
            }
            r
        };
        while e > 0 {
            if e & 1 == 1 {
                want = mulmod(want, b);
            }
            b = mulmod(b, b);
            e >>= 1;
        }
        assert_eq!(
            mod_pow(&num(3, 12), &num(1000003, 4), &num(m, 12)),
            num(want, 12)
        );
    }
}
