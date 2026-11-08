//! Bit reader for the uncompressed parts of AV1 (OBU headers, sequence and
//! frame headers): f(n), su(n), ns(n), le(n), leb128() and uvlc() of the
//! specification.

use crate::Error;

pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Position in bits.
    pos: usize,
}

const TRUNCATED: Error = Error::InvalidData("av1: truncated header");

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }

    pub fn position(&self) -> usize {
        self.pos
    }

    pub fn bit(&mut self) -> Result<u32, Error> {
        let byte = *self.data.get(self.pos >> 3).ok_or(TRUNCATED)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Ok(b as u32)
    }

    pub fn flag(&mut self) -> Result<bool, Error> {
        Ok(self.bit()? == 1)
    }

    /// f(n), n <= 32.
    pub fn f(&mut self, n: u32) -> Result<u32, Error> {
        let mut x: u64 = 0;
        for _ in 0..n {
            x = (x << 1) | self.bit()? as u64;
        }
        Ok(x as u32)
    }

    /// su(n): signed integer of n bits.
    pub fn su(&mut self, n: u32) -> Result<i32, Error> {
        let v = self.f(n)? as i64;
        let sign = 1i64 << (n - 1);
        Ok(if v & sign != 0 { v - 2 * sign } else { v } as i32)
    }

    /// ns(n): value in 0..n with a non-symmetric code.
    pub fn ns(&mut self, n: u32) -> Result<u32, Error> {
        if n <= 1 {
            return Ok(0);
        }
        let w = 32 - n.leading_zeros(); // FloorLog2(n) + 1
        let m = (1u32 << w) - n;
        let v = self.f(w - 1)?;
        if v < m {
            return Ok(v);
        }
        let extra = self.bit()?;
        Ok((v << 1) - m + extra)
    }

    /// le(n): little-endian unsigned of n bytes (byte aligned).
    pub fn le(&mut self, n: u32) -> Result<u32, Error> {
        let mut t: u64 = 0;
        for i in 0..n {
            t += (self.f(8)? as u64) << (i * 8);
        }
        u32::try_from(t).map_err(|_| Error::InvalidData("av1: value too large"))
    }

    pub fn leb128(&mut self) -> Result<u64, Error> {
        let mut value: u64 = 0;
        for i in 0..8 {
            let b = self.f(8)? as u64;
            value |= (b & 0x7f) << (i * 7);
            if b & 0x80 == 0 {
                return Ok(value);
            }
        }
        Err(Error::InvalidData("av1: bad leb128"))
    }

    pub fn uvlc(&mut self) -> Result<u32, Error> {
        let mut leading = 0;
        while self.bit()? == 0 {
            leading += 1;
            if leading >= 32 {
                return Ok(u32::MAX);
            }
        }
        Ok(((1u64 << leading) - 1 + self.f(leading)? as u64) as u32)
    }

    pub fn byte_align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }

    pub fn byte_pos(&self) -> usize {
        self.pos >> 3
    }
}

/// Reads a leb128 value from a byte slice; returns (value, length).
pub(crate) fn leb128(data: &[u8]) -> Option<(u64, usize)> {
    let mut value: u64 = 0;
    for i in 0..8 {
        let b = *data.get(i)? as u64;
        value |= (b & 0x7f) << (i * 7);
        if b & 0x80 == 0 {
            return Some((value, i + 1));
        }
    }
    None
}
