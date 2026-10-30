//! ChaCha20-Poly1305 (RFC 8439): the authenticated encryption of AirPlay 2
//! pairing messages, of the encrypted control connection and of the audio
//! packets.

fn quarter(s: &mut [u32; 16], a: usize, b: usize, c: usize, d: usize) {
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(16);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(12);
    s[a] = s[a].wrapping_add(s[b]);
    s[d] = (s[d] ^ s[a]).rotate_left(8);
    s[c] = s[c].wrapping_add(s[d]);
    s[b] = (s[b] ^ s[c]).rotate_left(7);
}

fn le32(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// One 64-byte ChaCha20 block.
fn block(key: &[u8; 32], counter: u32, nonce: &[u8; 12]) -> [u8; 64] {
    let mut init = [0u32; 16];
    init[..4].copy_from_slice(&[0x61707865, 0x3320646e, 0x79622d32, 0x6b206574]);
    for i in 0..8 {
        init[4 + i] = le32(&key[i * 4..]);
    }
    init[12] = counter;
    for i in 0..3 {
        init[13 + i] = le32(&nonce[i * 4..]);
    }
    let mut s = init;
    for _ in 0..10 {
        quarter(&mut s, 0, 4, 8, 12);
        quarter(&mut s, 1, 5, 9, 13);
        quarter(&mut s, 2, 6, 10, 14);
        quarter(&mut s, 3, 7, 11, 15);
        quarter(&mut s, 0, 5, 10, 15);
        quarter(&mut s, 1, 6, 11, 12);
        quarter(&mut s, 2, 7, 8, 13);
        quarter(&mut s, 3, 4, 9, 14);
    }
    let mut out = [0u8; 64];
    for i in 0..16 {
        out[i * 4..i * 4 + 4].copy_from_slice(&s[i].wrapping_add(init[i]).to_le_bytes());
    }
    out
}

/// XORs `data` with the ChaCha20 key stream starting at block `counter`.
pub fn chacha20_xor(key: &[u8; 32], counter: u32, nonce: &[u8; 12], data: &mut [u8]) {
    for (i, chunk) in data.chunks_mut(64).enumerate() {
        let ks = block(key, counter.wrapping_add(i as u32), nonce);
        for (d, k) in chunk.iter_mut().zip(ks) {
            *d ^= k;
        }
    }
}

/// Poly1305 one-time authenticator (26-bit limbs).
struct Poly1305 {
    r: [u32; 5],
    h: [u32; 5],
    pad: [u32; 4],
    buf: [u8; 16],
    buf_len: usize,
}

impl Poly1305 {
    fn new(key: &[u8; 32]) -> Poly1305 {
        Poly1305 {
            r: [
                le32(&key[0..]) & 0x3ffffff,
                (le32(&key[3..]) >> 2) & 0x3ffff03,
                (le32(&key[6..]) >> 4) & 0x3ffc0ff,
                (le32(&key[9..]) >> 6) & 0x3f03fff,
                (le32(&key[12..]) >> 8) & 0x00fffff,
            ],
            h: [0; 5],
            pad: [
                le32(&key[16..]),
                le32(&key[20..]),
                le32(&key[24..]),
                le32(&key[28..]),
            ],
            buf: [0; 16],
            buf_len: 0,
        }
    }

    fn blocks(&mut self, m: &[u8; 16], hibit: u32) {
        let [r0, r1, r2, r3, r4] = self.r.map(|v| v as u64);
        let (s1, s2, s3, s4) = (r1 * 5, r2 * 5, r3 * 5, r4 * 5);
        let mut h = self.h.map(|v| v as u64);
        h[0] += (le32(&m[0..]) & 0x3ffffff) as u64;
        h[1] += ((le32(&m[3..]) >> 2) & 0x3ffffff) as u64;
        h[2] += ((le32(&m[6..]) >> 4) & 0x3ffffff) as u64;
        h[3] += ((le32(&m[9..]) >> 6) & 0x3ffffff) as u64;
        h[4] += ((le32(&m[12..]) >> 8) | hibit) as u64;
        let d0 = h[0] * r0 + h[1] * s4 + h[2] * s3 + h[3] * s2 + h[4] * s1;
        let mut d1 = h[0] * r1 + h[1] * r0 + h[2] * s4 + h[3] * s3 + h[4] * s2;
        let mut d2 = h[0] * r2 + h[1] * r1 + h[2] * r0 + h[3] * s4 + h[4] * s3;
        let mut d3 = h[0] * r3 + h[1] * r2 + h[2] * r1 + h[3] * r0 + h[4] * s4;
        let mut d4 = h[0] * r4 + h[1] * r3 + h[2] * r2 + h[3] * r1 + h[4] * r0;
        let mut c = d0 >> 26;
        h[0] = d0 & 0x3ffffff;
        d1 += c;
        c = d1 >> 26;
        h[1] = d1 & 0x3ffffff;
        d2 += c;
        c = d2 >> 26;
        h[2] = d2 & 0x3ffffff;
        d3 += c;
        c = d3 >> 26;
        h[3] = d3 & 0x3ffffff;
        d4 += c;
        c = d4 >> 26;
        h[4] = d4 & 0x3ffffff;
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= 0x3ffffff;
        h[1] += c;
        self.h = h.map(|v| v as u32);
    }

    fn update(&mut self, mut data: &[u8]) {
        if self.buf_len > 0 {
            let n = (16 - self.buf_len).min(data.len());
            self.buf[self.buf_len..self.buf_len + n].copy_from_slice(&data[..n]);
            self.buf_len += n;
            data = &data[n..];
            if self.buf_len < 16 {
                return;
            }
            let b = self.buf;
            self.blocks(&b, 1 << 24);
            self.buf_len = 0;
        }
        let mut chunks = data.chunks_exact(16);
        for c in &mut chunks {
            self.blocks(c.try_into().expect("16 bytes"), 1 << 24);
        }
        let rest = chunks.remainder();
        self.buf[..rest.len()].copy_from_slice(rest);
        self.buf_len = rest.len();
    }

    fn finish(mut self) -> [u8; 16] {
        if self.buf_len > 0 {
            let mut b = [0u8; 16];
            b[..self.buf_len].copy_from_slice(&self.buf[..self.buf_len]);
            b[self.buf_len] = 1;
            self.blocks(&b, 0);
        }
        let mut h = self.h;
        let mut c = h[1] >> 26;
        h[1] &= 0x3ffffff;
        for v in h.iter_mut().skip(2) {
            *v += c;
            c = *v >> 26;
            *v &= 0x3ffffff;
        }
        h[0] += c * 5;
        c = h[0] >> 26;
        h[0] &= 0x3ffffff;
        h[1] += c;
        // g = h - p; keep h when g is negative.
        let mut g = [0u32; 5];
        let mut c = 5u32;
        for i in 0..4 {
            g[i] = h[i].wrapping_add(c);
            c = g[i] >> 26;
            g[i] &= 0x3ffffff;
        }
        g[4] = h[4].wrapping_add(c).wrapping_sub(1 << 26);
        let mask = (g[4] >> 31).wrapping_sub(1);
        for i in 0..5 {
            h[i] = (h[i] & !mask) | (g[i] & mask);
        }
        let w = [
            h[0] | (h[1] << 26),
            (h[1] >> 6) | (h[2] << 20),
            (h[2] >> 12) | (h[3] << 14),
            (h[3] >> 18) | (h[4] << 8),
        ];
        let mut out = [0u8; 16];
        let mut f = 0u64;
        for i in 0..4 {
            f = w[i] as u64 + self.pad[i] as u64 + (f >> 32);
            out[i * 4..i * 4 + 4].copy_from_slice(&(f as u32).to_le_bytes());
        }
        out
    }
}

/// The Poly1305 tag of `msg` (RFC 8439 2.5).
pub fn poly1305(key: &[u8; 32], msg: &[u8]) -> [u8; 16] {
    let mut p = Poly1305::new(key);
    p.update(msg);
    p.finish()
}

fn aead_tag(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], ct: &[u8]) -> [u8; 16] {
    let otk = block(key, 0, nonce);
    let mut p = Poly1305::new(otk[..32].try_into().expect("32 bytes"));
    let zeros = [0u8; 16];
    p.update(aad);
    p.update(&zeros[..(16 - aad.len() % 16) % 16]);
    p.update(ct);
    p.update(&zeros[..(16 - ct.len() % 16) % 16]);
    p.update(&(aad.len() as u64).to_le_bytes());
    p.update(&(ct.len() as u64).to_le_bytes());
    p.finish()
}

/// Encrypts `data` in place and returns the tag.
pub fn seal_in_place(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], data: &mut [u8]) -> [u8; 16] {
    chacha20_xor(key, 1, nonce, data);
    aead_tag(key, nonce, aad, data)
}

/// Checks the tag and decrypts `data` in place; `false` (and `data`
/// unchanged) when the tag does not match.
pub fn open_in_place(
    key: &[u8; 32],
    nonce: &[u8; 12],
    aad: &[u8],
    data: &mut [u8],
    tag: &[u8],
) -> bool {
    let want = aead_tag(key, nonce, aad, data);
    if tag.len() != 16 || want.iter().zip(tag).fold(0u8, |acc, (a, b)| acc | (a ^ b)) != 0 {
        return false;
    }
    chacha20_xor(key, 1, nonce, data);
    true
}

/// Ciphertext followed by the 16-byte tag.
pub fn seal(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], plain: &[u8]) -> Vec<u8> {
    let mut out = plain.to_vec();
    let tag = seal_in_place(key, nonce, aad, &mut out);
    out.extend_from_slice(&tag);
    out
}

/// Opens ciphertext followed by its tag.
pub fn open(key: &[u8; 32], nonce: &[u8; 12], aad: &[u8], sealed: &[u8]) -> Option<Vec<u8>> {
    let split = sealed.len().checked_sub(16)?;
    let mut out = sealed[..split].to_vec();
    open_in_place(key, nonce, aad, &mut out, &sealed[split..]).then_some(out)
}

/// The 12-byte nonce of pairing messages: four zero bytes and an
/// eight-byte label such as "PS-Msg05".
pub fn label_nonce(label: &[u8; 8]) -> [u8; 12] {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(label);
    n
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::hex;
    use crate::crypto::test_util::unhex;

    #[test]
    fn chacha20_block_rfc8439() {
        // 2.3.2
        let key: [u8; 32] = core::array::from_fn(|i| i as u8);
        let nonce: [u8; 12] = unhex("000000090000004a00000000").try_into().unwrap();
        assert_eq!(
            hex(&block(&key, 1, &nonce)),
            "10f1e7e4d13b5915500fdd1fa32071c4c7d1f4c733c068030422aa9ac3d46c4e\
             d2826446079faa0914c2d705d98b02a2b5129cd1de164eb9cbd083e8a2503c4e"
        );
    }

    #[test]
    fn poly1305_rfc8439() {
        // 2.5.2
        let key: [u8; 32] =
            unhex("85d6be7857556d337f4452fe42d506a80103808afb0db2fd4abff6af4149f51b")
                .try_into()
                .unwrap();
        assert_eq!(
            hex(&poly1305(&key, b"Cryptographic Forum Research Group")),
            "a8061dc1305136c6c22b8baf0c0127a9"
        );
    }

    #[test]
    fn aead_rfc8439() {
        // 2.8.2
        let plain = b"Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
        let aad = unhex("50515253c0c1c2c3c4c5c6c7");
        let key: [u8; 32] = core::array::from_fn(|i| 0x80 + i as u8);
        let nonce: [u8; 12] = unhex("070000004041424344454647").try_into().unwrap();
        let sealed = seal(&key, &nonce, &aad, plain);
        let ct = "d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d6\
                  3dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b36\
                  92ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc\
                  3ff4def08e4b7a9de576d26586cec64b6116";
        assert_eq!(hex(&sealed[..plain.len()]), ct);
        assert_eq!(
            hex(&sealed[plain.len()..]),
            "1ae10b594f09e26a7e902ecbd0600691"
        );
        assert_eq!(open(&key, &nonce, &aad, &sealed).unwrap(), plain);
        // Any change is detected.
        let mut bad = sealed.clone();
        bad[3] ^= 1;
        assert!(open(&key, &nonce, &aad, &bad).is_none());
        assert!(open(&key, &nonce, b"other", &sealed).is_none());
        assert!(open(&key, &nonce, &aad, &sealed[..10]).is_none());
    }

    #[test]
    fn partial_updates_match() {
        let key = [7u8; 32];
        let msg: Vec<u8> = (0..300u32).map(|i| (i * 7) as u8).collect();
        let mut p = Poly1305::new(&key);
        for c in msg.chunks(13) {
            p.update(c);
        }
        assert_eq!(p.finish(), poly1305(&key, &msg));
    }
}
