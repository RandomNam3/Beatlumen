//! CRC-8 (frame header) and CRC-16 (whole frame) checksums used by FLAC
//! (RFC 9639, sections 9.1.8 and 9.3).
//!
//! Both are MSB-first, non-reflected, initialised with zero:
//! * CRC-8: polynomial x^8 + x^2 + x^1 + x^0 (0x07)
//! * CRC-16: polynomial x^16 + x^15 + x^2 + x^0 (0x8005)

const fn build_crc8() -> [u8; 256] {
    let mut t = [0u8; 256];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
            b += 1;
        }
        t[i] = c;
        i += 1;
    }
    t
}

/// Slicing-by-4 tables for CRC-16: `T[0]` is the classic byte table, `T[k]`
/// advances a byte that is followed by `k` further bytes.
const fn build_crc16() -> [[u16; 256]; 4] {
    let mut t = [[0u16; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut c = (i as u16) << 8;
        let mut b = 0;
        while b < 8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
            b += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut k = 1;
    while k < 4 {
        let mut i = 0;
        while i < 256 {
            let prev = t[k - 1][i];
            t[k][i] = (prev << 8) ^ t[0][(prev >> 8) as usize];
            i += 1;
        }
        k += 1;
    }
    t
}

static CRC8_TABLE: [u8; 256] = build_crc8();
static CRC16_TABLE: [[u16; 256]; 4] = build_crc16();

/// CRC-8 of `data` (frame header check).
pub fn crc8(data: &[u8]) -> u8 {
    data.iter().fold(0u8, |c, &b| CRC8_TABLE[(c ^ b) as usize])
}

/// CRC-16 of `data` (frame footer check).
pub fn crc16(data: &[u8]) -> u16 {
    let t = &CRC16_TABLE;
    let mut crc = 0u16;
    let mut chunks = data.chunks_exact(4);
    for c in &mut chunks {
        let hi = ((crc >> 8) as u8 ^ c[0]) as usize;
        let lo = ((crc & 0xff) as u8 ^ c[1]) as usize;
        crc = t[3][hi] ^ t[2][lo] ^ t[1][c[2] as usize] ^ t[0][c[3] as usize];
    }
    for &b in chunks.remainder() {
        crc = (crc << 8) ^ t[0][((crc >> 8) as u8 ^ b) as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn crc16_bitwise(data: &[u8]) -> u16 {
        let mut crc = 0u16;
        for &b in data {
            crc ^= (b as u16) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
            }
        }
        crc
    }

    #[test]
    fn crc16_slicing_matches_bitwise() {
        let data: Vec<u8> = (0..1000u32).map(|i| (i.wrapping_mul(2654435761) >> 13) as u8).collect();
        for len in 0..40 {
            assert_eq!(crc16(&data[..len]), crc16_bitwise(&data[..len]));
        }
        assert_eq!(crc16(&data), crc16_bitwise(&data));
    }

    #[test]
    fn crc8_known_value() {
        // Header of the example frame in RFC 9639 Appendix D.1:
        // FF F8 69 18 00 00 with CRC-8 0xBF.
        assert_eq!(crc8(&[0xff, 0xf8, 0x69, 0x18, 0x00, 0x00]), 0xbf);
    }
}
