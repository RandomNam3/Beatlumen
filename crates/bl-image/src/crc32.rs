//! CRC-32 (ISO 3309 / PNG) with a slice-by-4 table.

const fn make_tables() -> [[u32; 256]; 4] {
    let mut t = [[0u32; 256]; 4];
    let mut i = 0;
    while i < 256 {
        let mut c = i as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
            k += 1;
        }
        t[0][i] = c;
        i += 1;
    }
    let mut i = 0;
    while i < 256 {
        let mut k = 1;
        while k < 4 {
            let prev = t[k - 1][i];
            t[k][i] = (prev >> 8) ^ t[0][(prev & 0xff) as usize];
            k += 1;
        }
        i += 1;
    }
    t
}

static TABLES: [[u32; 256]; 4] = make_tables();

/// Updates a running CRC (start with 0).
pub fn update(crc: u32, data: &[u8]) -> u32 {
    let mut c = !crc;
    let mut chunks = data.chunks_exact(4);
    for q in &mut chunks {
        c ^= u32::from_le_bytes([q[0], q[1], q[2], q[3]]);
        c = TABLES[3][(c & 0xff) as usize]
            ^ TABLES[2][((c >> 8) & 0xff) as usize]
            ^ TABLES[1][((c >> 16) & 0xff) as usize]
            ^ TABLES[0][(c >> 24) as usize];
    }
    for &b in chunks.remainder() {
        c = TABLES[0][((c ^ b as u32) & 0xff) as usize] ^ (c >> 8);
    }
    !c
}

#[cfg(test)]
mod tests {
    #[test]
    fn known_value() {
        assert_eq!(super::update(0, b"123456789"), 0xCBF4_3926);
        assert_eq!(super::update(super::update(0, b"1234"), b"56789"), 0xCBF4_3926);
    }
}
