//! SHA-512 (FIPS 180-4), HMAC-SHA-512 (RFC 2104) and HKDF-SHA-512
//! (RFC 5869): the hash of AirPlay 2 pairing (SRP, Ed25519) and the key
//! derivation of its session keys.

const K: [u64; 80] = [
    0x428a2f98d728ae22,
    0x7137449123ef65cd,
    0xb5c0fbcfec4d3b2f,
    0xe9b5dba58189dbbc,
    0x3956c25bf348b538,
    0x59f111f1b605d019,
    0x923f82a4af194f9b,
    0xab1c5ed5da6d8118,
    0xd807aa98a3030242,
    0x12835b0145706fbe,
    0x243185be4ee4b28c,
    0x550c7dc3d5ffb4e2,
    0x72be5d74f27b896f,
    0x80deb1fe3b1696b1,
    0x9bdc06a725c71235,
    0xc19bf174cf692694,
    0xe49b69c19ef14ad2,
    0xefbe4786384f25e3,
    0x0fc19dc68b8cd5b5,
    0x240ca1cc77ac9c65,
    0x2de92c6f592b0275,
    0x4a7484aa6ea6e483,
    0x5cb0a9dcbd41fbd4,
    0x76f988da831153b5,
    0x983e5152ee66dfab,
    0xa831c66d2db43210,
    0xb00327c898fb213f,
    0xbf597fc7beef0ee4,
    0xc6e00bf33da88fc2,
    0xd5a79147930aa725,
    0x06ca6351e003826f,
    0x142929670a0e6e70,
    0x27b70a8546d22ffc,
    0x2e1b21385c26c926,
    0x4d2c6dfc5ac42aed,
    0x53380d139d95b3df,
    0x650a73548baf63de,
    0x766a0abb3c77b2a8,
    0x81c2c92e47edaee6,
    0x92722c851482353b,
    0xa2bfe8a14cf10364,
    0xa81a664bbc423001,
    0xc24b8b70d0f89791,
    0xc76c51a30654be30,
    0xd192e819d6ef5218,
    0xd69906245565a910,
    0xf40e35855771202a,
    0x106aa07032bbd1b8,
    0x19a4c116b8d2d0c8,
    0x1e376c085141ab53,
    0x2748774cdf8eeb99,
    0x34b0bcb5e19b48a8,
    0x391c0cb3c5c95a63,
    0x4ed8aa4ae3418acb,
    0x5b9cca4f7763e373,
    0x682e6ff3d6b2b8a3,
    0x748f82ee5defb2fc,
    0x78a5636f43172f60,
    0x84c87814a1f0ab72,
    0x8cc702081a6439ec,
    0x90befffa23631e28,
    0xa4506cebde82bde9,
    0xbef9a3f7b2c67915,
    0xc67178f2e372532b,
    0xca273eceea26619c,
    0xd186b8c721c0c207,
    0xeada7dd6cde0eb1e,
    0xf57d4f7fee6ed178,
    0x06f067aa72176fba,
    0x0a637dc5a2c898a6,
    0x113f9804bef90dae,
    0x1b710b35131c471b,
    0x28db77f523047d84,
    0x32caab7b40c72493,
    0x3c9ebe0a15c9bebc,
    0x431d67c49c100d4c,
    0x4cc5d4becb3e42b6,
    0x597f299cfc657e2a,
    0x5fcb6fab3ad6faec,
    0x6c44198c4a475817,
];

const H0: [u64; 8] = [
    0x6a09e667f3bcc908,
    0xbb67ae8584caa73b,
    0x3c6ef372fe94f82b,
    0xa54ff53a5f1d36f1,
    0x510e527fade682d1,
    0x9b05688c2b3e6c1f,
    0x1f83d9abfb41bd6b,
    0x5be0cd19137e2179,
];

/// Incremental SHA-512.
#[derive(Clone)]
pub struct Sha512 {
    h: [u64; 8],
    buf: [u8; 128],
    buf_len: usize,
    total: u128,
}

impl Default for Sha512 {
    fn default() -> Self {
        Sha512::new()
    }
}

impl Sha512 {
    pub fn new() -> Sha512 {
        Sha512 {
            h: H0,
            buf: [0; 128],
            buf_len: 0,
            total: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total += data.len() as u128;
        if self.buf_len > 0 {
            let n = (128 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len < 128 {
                return;
            }
            let block = self.buf;
            self.compress(&block);
            self.buf_len = 0;
        }
        let mut chunks = data.chunks_exact(128);
        for block in &mut chunks {
            self.compress(block);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    pub fn finish(mut self) -> [u8; 64] {
        let bits = self.total.wrapping_mul(8);
        let mut pad = vec![0x80u8];
        let used = (self.buf_len + 1) % 128;
        pad.resize(1 + if used <= 112 { 112 - used } else { 240 - used }, 0);
        pad.extend_from_slice(&bits.to_be_bytes());
        let total = self.total;
        self.update(&pad);
        self.total = total;
        let mut out = [0u8; 64];
        for (i, v) in self.h.iter().enumerate() {
            out[i * 8..i * 8 + 8].copy_from_slice(&v.to_be_bytes());
        }
        out
    }

    fn compress(&mut self, block: &[u8]) {
        let mut w = [0u64; 80];
        for (i, c) in block.chunks_exact(8).enumerate() {
            w[i] = u64::from_be_bytes(c.try_into().expect("8 bytes"));
        }
        for i in 16..80 {
            let s0 = w[i - 15].rotate_right(1) ^ w[i - 15].rotate_right(8) ^ (w[i - 15] >> 7);
            let s1 = w[i - 2].rotate_right(19) ^ w[i - 2].rotate_right(61) ^ (w[i - 2] >> 6);
            w[i] = w[i - 16]
                .wrapping_add(s0)
                .wrapping_add(w[i - 7])
                .wrapping_add(s1);
        }
        let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = self.h;
        for i in 0..80 {
            let s1 = e.rotate_right(14) ^ e.rotate_right(18) ^ e.rotate_right(41);
            let ch = (e & f) ^ (!e & g);
            let t1 = h
                .wrapping_add(s1)
                .wrapping_add(ch)
                .wrapping_add(K[i])
                .wrapping_add(w[i]);
            let s0 = a.rotate_right(28) ^ a.rotate_right(34) ^ a.rotate_right(39);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(t1);
            d = c;
            c = b;
            b = a;
            a = t1.wrapping_add(t2);
        }
        for (x, v) in self.h.iter_mut().zip([a, b, c, d, e, f, g, h]) {
            *x = x.wrapping_add(v);
        }
    }
}

/// SHA-512 of the concatenated parts.
pub fn sha512(parts: &[&[u8]]) -> [u8; 64] {
    let mut h = Sha512::new();
    for p in parts {
        h.update(p);
    }
    h.finish()
}

/// HMAC-SHA-512 of the concatenated message parts.
pub fn hmac_sha512(key: &[u8], parts: &[&[u8]]) -> [u8; 64] {
    let mut k = [0u8; 128];
    if key.len() > 128 {
        k[..64].copy_from_slice(&sha512(&[key]));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha512::new();
    inner.update(&k.map(|b| b ^ 0x36));
    for p in parts {
        inner.update(p);
    }
    let inner = inner.finish();
    let mut outer = Sha512::new();
    outer.update(&k.map(|b| b ^ 0x5c));
    outer.update(&inner);
    outer.finish()
}

/// HKDF-SHA-512: extract with `salt`, expand with `info` to `len` bytes
/// (at most 255 * 64).
pub fn hkdf_sha512(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    assert!(len <= 255 * 64, "HKDF output too long");
    let prk = hmac_sha512(salt, &[ikm]);
    let mut out = Vec::with_capacity(len + 64);
    let mut t: Vec<u8> = Vec::new();
    let mut counter = 1u8;
    while out.len() < len {
        let block = hmac_sha512(&prk, &[&t, info, &[counter]]);
        out.extend_from_slice(&block);
        t = block.to_vec();
        counter = counter.wrapping_add(1);
    }
    out.truncate(len);
    out
}

/// A 32-byte key from HKDF-SHA-512 (AirPlay's salt/info pairs).
pub fn hkdf_key(ikm: &[u8], salt: &str, info: &str) -> [u8; 32] {
    let mut k = [0u8; 32];
    k.copy_from_slice(&hkdf_sha512(salt.as_bytes(), ikm, info.as_bytes(), 32));
    k
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::hex;
    use crate::crypto::test_util::unhex;

    #[test]
    fn sha512_vectors() {
        assert_eq!(
            hex(&sha512(&[b"abc"])),
            "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a\
             2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
        );
        assert_eq!(
            hex(&sha512(&[b""])),
            "cf83e1357eefb8bdf1542850d66d8007d620e4050b5715dc83f4a921d36ce9ce\
             47d0d13c5d85f2b0ff8318d2877eec2f63b931bd47417a81a538327af927da3e"
        );
        let msg = b"abcdefghbcdefghicdefghijdefghijkefghijklfghijklmghijklmnhijklmnoijklmnopjklmnopqklmnopqrlmnopqrsmnopqrstnopqrstu";
        assert_eq!(
            hex(&sha512(&[msg])),
            "8e959b75dae313da8cf4f72814fc143f8f7779c6eb9f7fa17299aeadb6889018\
             501d289e4900f7e4331b99dec4b5433ac7d329eeb6dd26545e96e55b874be909"
        );
        // Incremental updates across block boundaries give the same digest.
        let data: Vec<u8> = (0..1000u32).map(|i| i as u8).collect();
        let mut h = Sha512::new();
        for c in data.chunks(37) {
            h.update(c);
        }
        assert_eq!(h.finish(), sha512(&[&data]));
        assert_eq!(
            hex(&sha512(&[&vec![b'a'; 1_000_000]])),
            "e718483d0ce769644e2e42c7bc15b4638e1f98b13b2044285632a803afa973eb\
             de0ff244877ea60a4cb0432ce577c31beb009c5c2c49aa2e4eadb217ad8cc09b"
        );
    }

    #[test]
    fn hmac_rfc4231() {
        // Test case 1.
        let key = [0x0bu8; 20];
        assert_eq!(
            hex(&hmac_sha512(&key, &[b"Hi There"])),
            "87aa7cdea5ef619d4ff0b4241a1d6cb02379f4e2ce4ec2787ad0b30545e17cde\
             daa833b7d6b8a702038b274eaea3f4e4be9d914eeb61f1702e696c203a126854"
        );
        // Test case 2.
        assert_eq!(
            hex(&hmac_sha512(
                b"Jefe",
                &[b"what do ya want ", b"for nothing?"]
            )),
            "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea250554\
             9758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
        );
        // Test case 6: key longer than the block.
        let key = [0xaau8; 131];
        assert_eq!(
            hex(&hmac_sha512(
                &key,
                &[b"Test Using Larger Than Block-Size Key - Hash Key First"]
            )),
            "80b24263c7c1a3ebb71493c1dd7be8b49b46d1f41b4aeec1121b013783f8f352\
             6b56d037e05f2598bd0fd2215d6a1e5295e64f73f63f0aec8b915a985d786598"
        );
    }

    #[test]
    fn hkdf_vectors() {
        // RFC 5869 test case 1 inputs; HKDF-SHA-512 output (checked with
        // an independent implementation).
        let ikm = [0x0bu8; 22];
        let salt = unhex("000102030405060708090a0b0c");
        let info = unhex("f0f1f2f3f4f5f6f7f8f9");
        assert_eq!(
            hex(&hkdf_sha512(&salt, &ikm, &info, 42)),
            "832390086cda71fb47625bb5ceb168e4c8e26a1a16ed34d9fc7fe92c14815793\
             38da362cb8d9f925d7cb"
        );
        // Empty salt and info.
        assert_eq!(
            hex(&hkdf_sha512(&[], &ikm, &[], 42)),
            "f5fa02b18298a72a8c23898a8703472c6eb179dc204c03425c970e3b164bf90f\
             ff22d04836d0e2343bac"
        );
    }
}
