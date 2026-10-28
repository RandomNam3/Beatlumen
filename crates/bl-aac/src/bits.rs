//! MSB-first bit reader.
//!
//! Reads past the end of the buffer return zero bits instead of failing; the
//! reader remembers the overrun and callers check [`BitReader::check`] at
//! syntactic checkpoints. This keeps the hot Huffman paths branch-light while
//! still never panicking or looping forever on truncated input (every loop
//! in the bitstream syntax is bounded by counts parsed from the stream or
//! terminates on zero bits).

use crate::Error;

#[derive(Clone)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Current position in bits.
    pos: usize,
    /// End position in bits (may be smaller than `data.len() * 8` for
    /// sub-readers).
    end: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            end: data.len() * 8,
        }
    }

    /// A reader over the next `nbits` bits (clamped to this reader's end).
    pub fn sub(&self, nbits: usize) -> BitReader<'a> {
        let end = self.pos.saturating_add(nbits).min(self.end);
        BitReader {
            data: self.data,
            pos: self.pos.min(end),
            end,
        }
    }

    /// Next 32 bits, MSB-aligned; bits beyond the end read as zero.
    #[inline(always)]
    pub fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let shift = (self.pos & 7) as u32;
        let v = if byte + 8 <= self.data.len() && self.pos + 32 <= self.end {
            let b: [u8; 8] = self.data[byte..byte + 8].try_into().unwrap_or([0; 8]);
            u64::from_be_bytes(b)
        } else {
            self.peek_slow()
        };
        ((v << shift) >> 32) as u32
    }

    #[cold]
    fn peek_slow(&self) -> u64 {
        let byte = self.pos >> 3;
        let mut v = 0u64;
        for i in 0..8 {
            let bit0 = (byte + i) * 8;
            let b = if bit0 < self.end && byte + i < self.data.len() {
                let mut b = self.data[byte + i];
                // Mask bits beyond `end` within the last partial byte.
                if bit0 + 8 > self.end {
                    let keep = self.end - bit0;
                    b &= !(0xffu8 >> keep);
                }
                b
            } else {
                0
            };
            v = (v << 8) | b as u64;
        }
        v
    }

    #[inline(always)]
    pub fn skip(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }

    /// Reads `n` (0..=32) bits.
    #[inline(always)]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek32() >> (32 - n);
        self.pos = self.pos.saturating_add(n as usize);
        v
    }

    #[inline(always)]
    pub fn read_bit(&mut self) -> bool {
        self.read(1) != 0
    }

    pub fn byte_align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn set_position(&mut self, pos: usize) {
        self.pos = pos;
    }

    /// Bits remaining (negative after an overrun).
    pub fn bits_left(&self) -> isize {
        self.end as isize - self.pos.min(isize::MAX as usize) as isize
    }

    pub fn overrun(&self) -> bool {
        self.pos > self.end
    }

    #[inline]
    pub fn check(&self) -> Result<(), Error> {
        if self.pos > self.end {
            Err(Error::InvalidData("truncated bitstream"))
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_bits_and_pads() {
        let d = [0b1010_1100, 0xff, 0x01];
        let mut r = BitReader::new(&d);
        assert_eq!(r.read(1), 1);
        assert_eq!(r.read(3), 0b010);
        assert_eq!(r.read(8), 0b1100_1111);
        assert_eq!(r.read(12), 0b1111_0000_0001);
        assert!(!r.overrun());
        assert_eq!(r.read(5), 0);
        assert!(r.overrun());
        assert!(r.check().is_err());
    }

    #[test]
    fn sub_reader_masks_tail() {
        let d = [0xff, 0xff, 0xff];
        let mut r = BitReader::new(&d);
        r.skip(3);
        let mut s = r.sub(6);
        assert_eq!(s.read(8), 0b1111_1100);
        assert!(s.overrun());
    }
}
