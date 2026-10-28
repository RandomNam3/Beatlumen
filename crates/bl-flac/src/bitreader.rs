//! MSB-first bit reader over a byte slice with a 64-bit cache.
//!
//! The cache is kept left-aligned: the next unread bit is bit 63. Only the
//! top `bits` bits are guaranteed to be valid; the bits below may contain
//! the *correct* upcoming stream bits (from an overlapping 8-byte load) or
//! zeros, so OR-ing freshly loaded bytes at position `bits` is always exact.

/// Reading past the end of the slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Eof;

pub struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte that has not been loaded into the cache.
    pos: usize,
    cache: u64,
    /// Number of valid bits in `cache`.
    bits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        let mut r = BitReader {
            data,
            pos: 0,
            cache: 0,
            bits: 0,
        };
        r.refill();
        r
    }

    /// Tops up the cache to at least 56 valid bits if data remains. Never
    /// called with more than 56 valid bits.
    #[inline(always)]
    fn refill(&mut self) {
        if let Some(chunk) = self.data.get(self.pos..self.pos + 8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(chunk);
            let v = u64::from_be_bytes(b);
            // `bits` <= 56 here, and the overlapping low bits of `v` are the
            // true upcoming bits. Afterwards 56 <= bits <= 63.
            self.cache |= v >> self.bits;
            let n = (63 - self.bits) >> 3;
            self.pos += n as usize;
            self.bits += n << 3;
        } else {
            while self.bits <= 56 {
                match self.data.get(self.pos) {
                    Some(&b) => {
                        self.cache |= (b as u64) << (56 - self.bits);
                        self.bits += 8;
                        self.pos += 1;
                    }
                    None => break,
                }
            }
        }
    }

    /// Reads `n` (0..=32) bits as an unsigned number.
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> Result<u32, Eof> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Ok(0);
        }
        if self.bits < n {
            self.refill();
            if self.bits < n {
                return Err(Eof);
            }
        }
        let v = (self.cache >> (64 - n)) as u32;
        self.cache <<= n;
        self.bits -= n;
        Ok(v)
    }

    /// Reads `n` (0..=56) bits as an unsigned number.
    pub fn read_u64(&mut self, n: u32) -> Result<u64, Eof> {
        debug_assert!(n <= 56);
        if n == 0 {
            return Ok(0);
        }
        if self.bits < n {
            self.refill();
            if self.bits < n {
                return Err(Eof);
            }
        }
        let v = self.cache >> (64 - n);
        self.cache <<= n;
        self.bits -= n;
        Ok(v)
    }

    /// Reads an `n`-bit (0..=32) two's complement number.
    #[inline(always)]
    pub fn read_signed(&mut self, n: u32) -> Result<i32, Eof> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read(n)?;
        Ok(((v << (32 - n)) as i32) >> (32 - n))
    }

    /// Reads an `n`-bit (0..=56) two's complement number.
    pub fn read_signed_i64(&mut self, n: u32) -> Result<i64, Eof> {
        if n == 0 {
            return Ok(0);
        }
        let v = self.read_u64(n)?;
        Ok(((v << (64 - n)) as i64) >> (64 - n))
    }

    /// Reads a unary number: counts zero bits up to and including a one bit.
    #[inline(always)]
    pub fn read_unary(&mut self) -> Result<u32, Eof> {
        let mut count = 0u32;
        loop {
            if self.bits == 0 {
                self.refill();
                if self.bits == 0 {
                    return Err(Eof);
                }
            }
            let lz = self.cache.leading_zeros();
            if lz < self.bits {
                // lz <= 62 here, so the shift by lz + 1 cannot overflow.
                self.cache <<= lz + 1;
                self.bits -= lz + 1;
                return Ok(count.saturating_add(lz));
            }
            count = count.saturating_add(self.bits);
            self.cache = 0;
            self.bits = 0;
        }
    }

    /// Decodes `out.len()` Rice codes with parameter `k` (0..=30) and stores
    /// the unfolded (zigzag-decoded) residuals.
    #[inline]
    pub fn read_rice_block(&mut self, out: &mut [i32], k: u32) -> Result<(), Eof> {
        debug_assert!(k <= 31);
        for o in out.iter_mut() {
            if self.bits < 32 {
                self.refill();
            }
            let lz = self.cache.leading_zeros();
            let total = lz + 1 + k;
            let folded: u32 = if total <= self.bits {
                // Fast path: the whole code word is in the cache.
                // lz < bits <= 64, so `lz <= 63` and the split shifts are safe.
                let c = (self.cache << lz) << 1;
                let low = ((c >> 32) >> (32 - k)) as u32;
                self.cache = c << k;
                self.bits -= total;
                (lz << k) | low
            } else {
                let q = self.read_unary()?;
                let low = self.read(k)?;
                q.wrapping_shl(k) | low
            };
            *o = ((folded >> 1) as i32) ^ -((folded & 1) as i32);
        }
        Ok(())
    }

    /// Skips to the next byte boundary.
    pub fn align(&mut self) {
        let r = self.bits & 7;
        self.cache <<= r;
        self.bits -= r;
    }

    /// Number of whole bytes consumed so far (valid after [`align`]).
    pub fn byte_pos(&self) -> usize {
        self.pos - (self.bits as usize >> 3)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_and_signed() {
        let data = [
            0b1010_1100,
            0xff,
            0x00,
            0x80,
            0x12,
            0x34,
            0x56,
            0x78,
            0x9a,
            0xbc,
        ];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read(1), Ok(1));
        assert_eq!(r.read(3), Ok(0b010));
        assert_eq!(r.read_signed(4), Ok(-4));
        assert_eq!(r.read(8), Ok(0xff));
        assert_eq!(r.read_unary(), Ok(8));
        assert_eq!(r.read(7), Ok(0));
        assert_eq!(r.read(32), Ok(0x1234_5678));
        assert_eq!(r.read(16), Ok(0x9abc));
        assert_eq!(r.read(1), Err(Eof));
    }

    #[test]
    fn rice_roundtrip() {
        // Encode some values with k = 3 and decode them again.
        let vals: Vec<i32> = (-300..300).map(|v| v * 7 % 211).collect();
        for k in [0u32, 1, 3, 7, 14] {
            let mut bits: Vec<bool> = Vec::new();
            for &v in &vals {
                let f = if v >= 0 {
                    (v as u32) << 1
                } else {
                    ((-v as u32) << 1) - 1
                };
                let q = f >> k;
                bits.resize(bits.len() + q as usize, false);
                bits.push(true);
                for b in (0..k).rev() {
                    bits.push((f >> b) & 1 != 0);
                }
            }
            let mut bytes = vec![0u8; bits.len().div_ceil(8)];
            for (i, &b) in bits.iter().enumerate() {
                if b {
                    bytes[i / 8] |= 0x80 >> (i % 8);
                }
            }
            let mut r = BitReader::new(&bytes);
            let mut out = vec![0i32; vals.len()];
            r.read_rice_block(&mut out, k).unwrap();
            assert_eq!(out, vals, "k = {k}");
        }
    }
}
