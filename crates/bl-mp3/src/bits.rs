//! MSB-first bit reader with a 64-bit cache.
//!
//! Reading past the end of the data yields zero bits (the position keeps
//! advancing), so malformed streams can never cause out-of-bounds access;
//! callers detect overruns by comparing [`BitReader::position`] against
//! their limits.

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Index of the next byte to load into the cache.
    next: usize,
    /// Left-aligned cached bits.
    cache: u64,
    /// Number of valid bits in `cache`.
    bits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self::at(data, 0)
    }

    /// Creates a reader positioned at bit offset `bit_pos`.
    pub fn at(data: &'a [u8], bit_pos: usize) -> Self {
        let mut r = BitReader {
            data,
            next: bit_pos / 8,
            cache: 0,
            bits: 0,
        };
        r.refill();
        let skip = (bit_pos % 8) as u32;
        r.cache <<= skip;
        r.bits -= skip;
        r
    }

    /// Current position in bits from the start of the data.
    #[inline]
    pub fn position(&self) -> usize {
        self.next * 8 - self.bits as usize
    }

    /// Ensures at least 56 bits are cached.
    #[inline]
    pub fn refill(&mut self) {
        if self.bits > 56 {
            return;
        }
        if let Some(chunk) = self.data.get(self.next..self.next + 8) {
            let mut b = [0u8; 8];
            b.copy_from_slice(chunk);
            let v = u64::from_be_bytes(b);
            self.cache |= v >> self.bits;
            let n = (63 - self.bits) >> 3;
            self.next += n as usize;
            self.bits += n * 8;
        } else {
            while self.bits <= 56 {
                let byte = self.data.get(self.next).copied().unwrap_or(0);
                self.cache |= u64::from(byte) << (56 - self.bits);
                self.next += 1;
                self.bits += 8;
            }
        }
    }

    /// Peeks `n` bits (1..=32) without consuming. Requires a prior refill
    /// guaranteeing `n` cached bits.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!((1..=32).contains(&n));
        (self.cache >> (64 - n)) as u32
    }

    /// Consumes `n` cached bits (0..=bits).
    #[inline]
    pub fn skip(&mut self, n: u32) {
        debug_assert!(n <= self.bits);
        self.cache <<= n;
        self.bits -= n;
    }

    /// Returns the top cached bit as a sign (true = negative) without
    /// consuming it.
    #[inline]
    pub fn top_bit(&self) -> bool {
        (self.cache as i64) < 0
    }

    /// Reads `n` bits (0..=32).
    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        if self.bits < n {
            self.refill();
        }
        let v = self.peek(n);
        self.skip(n);
        v
    }

    #[inline]
    pub fn read_bool(&mut self) -> bool {
        self.read(1) != 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_and_zero_pads() {
        let data = [0b1010_1100, 0xFF, 0x01];
        let mut r = BitReader::new(&data);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.read(3), 0b010);
        assert_eq!(r.read(8), 0b1100_1111);
        assert_eq!(r.position(), 12);
        assert_eq!(r.read(12), 0b1111_0000_0001);
        assert_eq!(r.read(32), 0);
        assert_eq!(r.position(), 56);
        let mut r = BitReader::at(&data, 9);
        assert_eq!(r.read(7), 0x7F);
    }
}
