//! AES-128 in CBC mode (FIPS 197), for the audio packets of encrypted RAOP
//! streams (decrypted when receiving, encrypted when sending). Each packet is decrypted on its own with the
//! stream's IV; a trailing partial block is sent in the clear.

const SBOX: [u8; 256] = {
    // Computed at compile time from the multiplicative inverse in GF(2^8)
    // followed by the affine transform.
    let mut sbox = [0u8; 256];
    let mut p: u8 = 1;
    let mut q: u8 = 1;
    loop {
        // p *= 3
        p = p ^ (p << 1) ^ (if p & 0x80 != 0 { 0x1b } else { 0 });
        // q /= 3
        q ^= q << 1;
        q ^= q << 2;
        q ^= q << 4;
        if q & 0x80 != 0 {
            q ^= 0x09;
        }
        let x = q ^ q.rotate_left(1) ^ q.rotate_left(2) ^ q.rotate_left(3) ^ q.rotate_left(4);
        sbox[p as usize] = x ^ 0x63;
        if p == 1 {
            break;
        }
    }
    sbox[0] = 0x63;
    sbox
};

const INV_SBOX: [u8; 256] = {
    let mut inv = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        inv[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inv
};

fn xtime(x: u8) -> u8 {
    (x << 1) ^ if x & 0x80 != 0 { 0x1b } else { 0 }
}

fn mul(mut a: u8, mut b: u8) -> u8 {
    let mut r = 0;
    while b != 0 {
        if b & 1 != 0 {
            r ^= a;
        }
        a = xtime(a);
        b >>= 1;
    }
    r
}

/// AES-128 with an expanded key, decryption direction.
pub struct Aes128 {
    round_keys: [[u8; 16]; 11],
}

impl Aes128 {
    pub fn new(key: &[u8; 16]) -> Aes128 {
        let mut w = [[0u8; 4]; 44];
        for i in 0..4 {
            w[i].copy_from_slice(&key[i * 4..i * 4 + 4]);
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
                rk[c * 4..c * 4 + 4].copy_from_slice(&w[r * 4 + c]);
            }
        }
        Aes128 { round_keys }
    }

    pub fn decrypt_block(&self, block: &mut [u8; 16]) {
        add(block, &self.round_keys[10]);
        for round in (1..10).rev() {
            inv_shift_rows(block);
            for b in block.iter_mut() {
                *b = INV_SBOX[*b as usize];
            }
            add(block, &self.round_keys[round]);
            inv_mix_columns(block);
        }
        inv_shift_rows(block);
        for b in block.iter_mut() {
            *b = INV_SBOX[*b as usize];
        }
        add(block, &self.round_keys[0]);
    }

    pub fn encrypt_block(&self, block: &mut [u8; 16]) {
        add(block, &self.round_keys[0]);
        for round in 1..10 {
            for b in block.iter_mut() {
                *b = SBOX[*b as usize];
            }
            shift_rows(block);
            mix_columns(block);
            add(block, &self.round_keys[round]);
        }
        for b in block.iter_mut() {
            *b = SBOX[*b as usize];
        }
        shift_rows(block);
        add(block, &self.round_keys[10]);
    }

    /// Encrypts the whole 16-byte blocks of `data` in CBC mode, leaving a
    /// trailing partial block in the clear (as RAOP senders do).
    pub fn encrypt_cbc(&self, iv: &[u8; 16], data: &mut [u8]) {
        let mut prev = *iv;
        let whole = data.len() / 16 * 16;
        for chunk in data[..whole].chunks_exact_mut(16) {
            let mut block: [u8; 16] = chunk.try_into().expect("16 bytes");
            for i in 0..16 {
                block[i] ^= prev[i];
            }
            self.encrypt_block(&mut block);
            chunk.copy_from_slice(&block);
            prev = block;
        }
    }

    /// Decrypts the whole 16-byte blocks of `data` in CBC mode; a trailing
    /// partial block stays as it is (RAOP sends it unencrypted).
    pub fn decrypt_cbc(&self, iv: &[u8; 16], data: &mut [u8]) {
        let mut prev = *iv;
        let whole = data.len() / 16 * 16;
        for chunk in data[..whole].chunks_exact_mut(16) {
            let mut block: [u8; 16] = chunk.try_into().expect("16 bytes");
            let cipher = block;
            self.decrypt_block(&mut block);
            for i in 0..16 {
                chunk[i] = block[i] ^ prev[i];
            }
            prev = cipher;
        }
    }
}

fn add(block: &mut [u8; 16], key: &[u8; 16]) {
    for (b, k) in block.iter_mut().zip(key) {
        *b ^= k;
    }
}

fn inv_shift_rows(s: &mut [u8; 16]) {
    // State is column-major: s[c * 4 + r].
    for r in 1..4 {
        let row = [s[r], s[4 + r], s[8 + r], s[12 + r]];
        for c in 0..4 {
            s[((c + r) % 4) * 4 + r] = row[c];
        }
    }
}

fn shift_rows(s: &mut [u8; 16]) {
    for r in 1..4 {
        let row = [s[r], s[4 + r], s[8 + r], s[12 + r]];
        for c in 0..4 {
            s[c * 4 + r] = row[(c + r) % 4];
        }
    }
}

fn mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
        for r in 0..4 {
            s[c * 4 + r] = mul(a[r], 2) ^ mul(a[(r + 1) % 4], 3) ^ a[(r + 2) % 4] ^ a[(r + 3) % 4];
        }
    }
}

fn inv_mix_columns(s: &mut [u8; 16]) {
    for c in 0..4 {
        let a = [s[c * 4], s[c * 4 + 1], s[c * 4 + 2], s[c * 4 + 3]];
        for r in 0..4 {
            s[c * 4 + r] = mul(a[r], 14)
                ^ mul(a[(r + 1) % 4], 11)
                ^ mul(a[(r + 2) % 4], 13)
                ^ mul(a[(r + 3) % 4], 9);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bytes(hex: &str) -> Vec<u8> {
        (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect()
    }

    #[test]
    fn sbox() {
        assert_eq!(SBOX[0x00], 0x63);
        assert_eq!(SBOX[0x53], 0xed);
        assert_eq!(SBOX[0xff], 0x16);
        assert_eq!(INV_SBOX[0x63], 0x00);
    }

    #[test]
    fn fips197_block() {
        let key: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let mut block: [u8; 16] = bytes("69c4e0d86a7b0430d8cdb78070b4c55a")
            .try_into()
            .unwrap();
        Aes128::new(&key).decrypt_block(&mut block);
        assert_eq!(block.to_vec(), bytes("00112233445566778899aabbccddeeff"));
    }

    #[test]
    fn fips197_encrypt_and_cbc_roundtrip() {
        let key: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let aes = Aes128::new(&key);
        let mut block: [u8; 16] = bytes("00112233445566778899aabbccddeeff")
            .try_into()
            .unwrap();
        aes.encrypt_block(&mut block);
        assert_eq!(block.to_vec(), bytes("69c4e0d86a7b0430d8cdb78070b4c55a"));
        // NIST SP 800-38A F.2.1 (CBC-AES128.Encrypt), plus a clear tail.
        let key: [u8; 16] = bytes("2b7e151628aed2a6abf7158809cf4f3c")
            .try_into()
            .unwrap();
        let iv: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let mut data =
            bytes("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51abcd");
        Aes128::new(&key).encrypt_cbc(&iv, &mut data);
        assert_eq!(
            data,
            bytes("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2abcd")
        );
    }

    #[test]
    fn sp800_38a_cbc() {
        // NIST SP 800-38A F.2.2 (CBC-AES128.Decrypt), plus a clear tail.
        let key: [u8; 16] = bytes("2b7e151628aed2a6abf7158809cf4f3c")
            .try_into()
            .unwrap();
        let iv: [u8; 16] = bytes("000102030405060708090a0b0c0d0e0f")
            .try_into()
            .unwrap();
        let mut data =
            bytes("7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2abcd");
        Aes128::new(&key).decrypt_cbc(&iv, &mut data);
        assert_eq!(
            data,
            bytes("6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51abcd")
        );
    }
}
