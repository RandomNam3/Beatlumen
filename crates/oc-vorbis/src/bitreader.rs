//! LSB-first bit reader as used by all Vorbis packets.
//!
//! Vorbis packs values starting at the least significant bit of each byte.
//! The reader keeps a 64-bit accumulator; reads past the end of the packet
//! fail (returning `None`) and set a sticky end-of-packet flag, which the
//! audio decoder treats as the "nominal" end-of-packet condition described
//! by the specification.

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Next byte of `data` to load into the accumulator.
    pos: usize,
    /// Buffered bits; bit 0 is the next bit of the stream.
    acc: u64,
    /// Number of valid bits in `acc`.
    nbits: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            acc: 0,
            nbits: 0,
        }
    }

    /// Tops up the accumulator to at least 57 bits when enough data remains.
    #[inline(always)]
    fn refill(&mut self) {
        if self.pos + 8 <= self.data.len() {
            let mut b = [0u8; 8];
            b.copy_from_slice(&self.data[self.pos..self.pos + 8]);
            let w = u64::from_le_bytes(b);
            // `nbits` is < 64 here; bytes beyond the consumed ones are
            // shifted in as well, but they are identical to what a later
            // refill ORs in, so no masking is needed.
            self.acc |= w << self.nbits;
            let n = (63 - self.nbits) >> 3;
            self.pos += n as usize;
            self.nbits += n * 8;
        } else {
            while self.nbits <= 56 && self.pos < self.data.len() {
                self.acc |= (self.data[self.pos] as u64) << self.nbits;
                self.pos += 1;
                self.nbits += 8;
            }
        }
    }

    /// Reads `n` (0..=32) bits as an unsigned integer.
    #[inline]
    pub fn read(&mut self, n: u32) -> Option<u32> {
        debug_assert!(n <= 32);
        if n == 0 {
            return Some(0);
        }
        if self.nbits < n {
            self.refill();
            if self.nbits < n {
                self.set_eop();
                return None;
            }
        }
        let v = (self.acc & ((1u64 << n) - 1)) as u32;
        self.acc >>= n;
        self.nbits -= n;
        Some(v)
    }

    #[inline]
    pub fn read_bit(&mut self) -> Option<bool> {
        self.read(1).map(|b| b != 0)
    }

    /// Returns the next 32 bits without consuming them (zero-padded past the
    /// end of the packet).
    #[inline(always)]
    pub fn peek32(&mut self) -> u32 {
        if self.nbits < 32 {
            self.refill();
        }
        // Bits above `nbits` are either zero (end of data) or the correct
        // following stream bits (see `refill`).
        self.acc as u32
    }

    /// Consumes `n` (1..=32) bits previously inspected with `peek32`.
    /// Returns false (and flags end-of-packet) if fewer bits remain.
    #[inline(always)]
    pub fn consume(&mut self, n: u32) -> bool {
        if n > self.nbits {
            self.set_eop();
            return false;
        }
        self.acc >>= n;
        self.nbits -= n;
        true
    }

    /// Flags the end-of-packet condition (e.g. for an invalid codeword).
    pub fn mark_eop(&mut self) {
        self.set_eop();
    }

    fn set_eop(&mut self) {
        self.acc = 0;
        self.nbits = 0;
        self.pos = self.data.len();
    }

    /// Number of unread bits left in the packet.
    pub fn bits_left(&self) -> u64 {
        (self.data.len() - self.pos) as u64 * 8 + self.nbits as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lsb_first() {
        let data = [
            0b1010_1100u8,
            0xFF,
            0x01,
            0x80,
            0x12,
            0x34,
            0x56,
            0x78,
            0x9A,
            0xBC,
        ];
        let mut br = BitReader::new(&data);
        assert_eq!(br.read(2), Some(0b00));
        assert_eq!(br.read(3), Some(0b011));
        assert_eq!(br.read(3), Some(0b101));
        assert_eq!(br.read(8), Some(0xFF));
        assert_eq!(br.read(16), Some(0x8001));
        assert_eq!(br.read(32), Some(0x7856_3412));
        assert_eq!(br.peek32() & 0xFFFF, 0xBC9A);
        assert!(br.consume(4));
        assert_eq!(br.read(12), Some(0xBC9));
        assert_eq!(br.bits_left(), 0);
        assert_eq!(br.read(1), None);
        assert!(!br.consume(1));
    }
}
