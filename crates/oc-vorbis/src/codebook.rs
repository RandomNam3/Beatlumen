//! Vorbis codebooks (spec section 3): Huffman entropy decoding plus the
//! optional vector-quantisation (VQ) value mapping (lookup types 1 and 2).
//!
//! The setup sanity limits (`ilog(dims) + ilog(entries) <= 24`, length
//! list size checks) mirror libvorbis' `vorbis_staticbook_unpack`
//! (Copyright (c) 2002-2020 Xiph.org Foundation, BSD-3-Clause, see NOTICE).
//!
//! Decoding uses a direct lookup table for codewords of up to
//! [`FAST_BITS`] bits and a binary search over the sorted (MSB-aligned)
//! codewords for longer ones.

use crate::bitreader::BitReader;
use crate::Error;

/// Width of the direct Huffman lookup table.
const FAST_BITS: u32 = 10;
/// Codebooks whose VQ table would exceed this many floats compute vector
/// values on the fly instead of precomputing them.
const MAX_VQ_TABLE: usize = 1 << 20;
/// Upper bound on precomputed VQ floats over all codebooks of a stream.
const MAX_VQ_TOTAL: usize = 1 << 23;

pub(crate) fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

/// Unpacks the Vorbis 32-bit float format (spec 9.2.2).
pub(crate) fn float32_unpack(x: u32) -> f32 {
    let mantissa = (x & 0x1f_ffff) as f64;
    let exponent = ((x & 0x7fe0_0000) >> 21) as i32;
    let v = mantissa * 2f64.powi(exponent - 788);
    (if x & 0x8000_0000 != 0 { -v } else { v }) as f32
}

/// Returns true if `base^exp <= limit`.
fn pow_le(base: u64, exp: u32, limit: u64) -> bool {
    if base <= 1 {
        return base <= limit || exp == 0;
    }
    let mut acc = 1u64;
    for _ in 0..exp {
        acc = acc.saturating_mul(base);
        if acc > limit {
            return false;
        }
    }
    true
}

/// Greatest `r` with `r^dims <= entries` (spec 9.2.3).
pub(crate) fn lookup1_values(entries: u32, dims: u32) -> u32 {
    if dims == 0 {
        return 0;
    }
    let mut r = (entries as f64).powf(1.0 / dims as f64).floor() as u64;
    while r > 0 && !pow_le(r, dims, entries as u64) {
        r -= 1;
    }
    while pow_le(r + 1, dims, entries as u64) {
        r += 1;
    }
    r as u32
}

/// Huffman decoder for one codebook.
#[derive(Default)]
struct Huffman {
    /// Number of index bits of `fast` (0 if the book has no used entries).
    fast_bits: u32,
    /// Indexed by the next `fast_bits` stream bits: `(len << 24) | symbol`,
    /// or 0 when the codeword is longer than `fast_bits` (or invalid).
    fast: Vec<u32>,
    /// MSB-aligned codewords longer than `fast_bits`, ascending.
    long_codes: Vec<u32>,
    /// `(len << 24) | symbol` for each entry of `long_codes`.
    long_info: Vec<u32>,
}

impl Huffman {
    /// Builds the decoder from codeword lengths (0 = unused entry), assigning
    /// codewords as described in spec section 3.2.1.
    fn new(lengths: &[u8]) -> Result<Self, Error> {
        let mut available = [0u64; 33];
        // (MSB-aligned codeword, symbol, length)
        let mut codes: Vec<(u32, u32, u8)> = Vec::new();
        let mut iter = lengths.iter().enumerate().filter(|(_, &l)| l > 0);
        let Some((first, &len0)) = iter.next() else {
            return Ok(Huffman::default());
        };
        codes.push((0, first as u32, len0));
        for (i, a) in available
            .iter_mut()
            .enumerate()
            .take(len0 as usize + 1)
            .skip(1)
        {
            *a = 1u64 << (32 - i);
        }
        for (sym, &len) in iter {
            let len = len as usize;
            let mut z = len;
            while z > 0 && available[z] == 0 {
                z -= 1;
            }
            if z == 0 {
                return Err(Error::InvalidData("overspecified huffman tree"));
            }
            let res = available[z];
            available[z] = 0;
            codes.push((res as u32, sym as u32, len as u8));
            for y in (z + 1..=len).rev() {
                available[y] = res + (1u64 << (32 - y));
            }
        }
        let single = codes.len() == 1;
        if single {
            // Errata 20150226: a single used entry must have length 1; it
            // consumes one bit whatever its value.
            if len0 != 1 {
                return Err(Error::InvalidData("single-entry codebook with length > 1"));
            }
        } else if available.iter().any(|&a| a != 0) {
            return Err(Error::InvalidData("underspecified huffman tree"));
        }

        let max_len = codes.iter().map(|c| c.2 as u32).max().unwrap_or(1);
        let fast_bits = max_len.min(FAST_BITS);
        let mut fast = vec![0u32; 1 << fast_bits];
        let mut long: Vec<(u32, u32)> = Vec::new();
        for &(code, sym, len) in &codes {
            let len = len as u32;
            let info = (len << 24) | sym;
            if single {
                fast.iter_mut().for_each(|e| *e = info);
            } else if len <= fast_bits {
                let rev = (code >> (32 - len)).reverse_bits() >> (32 - len);
                let mut idx = rev as usize;
                while idx < fast.len() {
                    fast[idx] = info;
                    idx += 1 << len;
                }
            } else {
                long.push((code, info));
            }
        }
        long.sort_unstable_by_key(|e| e.0);
        Ok(Huffman {
            fast_bits,
            fast,
            long_codes: long.iter().map(|e| e.0).collect(),
            long_info: long.iter().map(|e| e.1).collect(),
        })
    }

    #[inline(always)]
    fn decode(&self, br: &mut BitReader) -> Option<u32> {
        let bits = br.peek32();
        let mask = (1u32 << self.fast_bits) - 1;
        let e = *self.fast.get((bits & mask) as usize)?;
        if e != 0 {
            return if br.consume(e >> 24) {
                Some(e & 0xff_ffff)
            } else {
                None
            };
        }
        self.decode_slow(bits, br)
    }

    #[cold]
    fn decode_slow(&self, bits: u32, br: &mut BitReader) -> Option<u32> {
        let x = bits.reverse_bits();
        // Index of the last codeword <= x.
        let idx = self
            .long_codes
            .partition_point(|&c| c <= x)
            .checked_sub(1)?;
        let code = self.long_codes[idx];
        let info = self.long_info[idx];
        let len = info >> 24;
        if ((x - code) as u64) >= (1u64 << (32 - len)) {
            // Not a valid codeword: treat like the end of the packet.
            br.mark_eop();
            return None;
        }
        if br.consume(len) {
            Some(info & 0xff_ffff)
        } else {
            None
        }
    }
}

/// VQ value mapping of a codebook.
struct VqLookup {
    lookup_type: u8,
    minimum: f32,
    delta: f32,
    sequence_p: bool,
    lookup_values: u32,
    multiplicands: Vec<u32>,
}

pub(crate) struct Codebook {
    pub dims: usize,
    pub entries: u32,
    huffman: Huffman,
    vq: Option<VqLookup>,
    /// Precomputed `entries * dims` VQ vectors (empty if not precomputed).
    table: Vec<f32>,
}

impl Codebook {
    /// Parses one codebook from the setup header (spec 3.2.1).
    pub fn parse(br: &mut BitReader) -> Result<Self, Error> {
        const EOP: Error = Error::InvalidData("truncated codebook");
        if br.read(24).ok_or(EOP)? != 0x56_4342 {
            return Err(Error::InvalidData("bad codebook sync pattern"));
        }
        let dims = br.read(16).ok_or(EOP)?;
        let entries = br.read(24).ok_or(EOP)?;
        if ilog(dims) + ilog(entries) > 24 {
            return Err(Error::InvalidData("codebook too large"));
        }
        let ordered = br.read_bit().ok_or(EOP)?;
        let mut lengths = Vec::new();
        if !ordered {
            let sparse = br.read_bit().ok_or(EOP)?;
            let min_bits = entries as u64 * if sparse { 1 } else { 5 };
            if min_bits > br.bits_left() {
                return Err(EOP);
            }
            lengths.reserve_exact(entries as usize);
            for _ in 0..entries {
                let used = if sparse {
                    br.read_bit().ok_or(EOP)?
                } else {
                    true
                };
                lengths.push(if used {
                    br.read(5).ok_or(EOP)? as u8 + 1
                } else {
                    0
                });
            }
        } else {
            let mut current_len = br.read(5).ok_or(EOP)? + 1;
            lengths.reserve_exact(entries as usize);
            while (lengths.len() as u32) < entries {
                let remaining = entries - lengths.len() as u32;
                let number = br.read(ilog(remaining)).ok_or(EOP)?;
                if number > remaining || (number > 0 && current_len > 32) {
                    return Err(Error::InvalidData("bad ordered codebook"));
                }
                // More codewords of one length than the tree can hold.
                if current_len < 32 && number as u64 > (1u64 << current_len) {
                    return Err(Error::InvalidData("overspecified huffman tree"));
                }
                lengths.resize(lengths.len() + number as usize, current_len as u8);
                current_len += 1;
                if current_len > 33 {
                    return Err(Error::InvalidData("bad ordered codebook"));
                }
            }
        }

        let lookup_type = br.read(4).ok_or(EOP)? as u8;
        let vq = match lookup_type {
            0 => None,
            1 | 2 => {
                let minimum = float32_unpack(br.read(32).ok_or(EOP)?);
                let delta = float32_unpack(br.read(32).ok_or(EOP)?);
                let value_bits = br.read(4).ok_or(EOP)? + 1;
                let sequence_p = br.read_bit().ok_or(EOP)?;
                let lookup_values = if lookup_type == 1 {
                    lookup1_values(entries, dims)
                } else {
                    entries * dims
                };
                if lookup_values as u64 * value_bits as u64 > br.bits_left() {
                    return Err(EOP);
                }
                let mut multiplicands = Vec::with_capacity(lookup_values as usize);
                for _ in 0..lookup_values {
                    multiplicands.push(br.read(value_bits).ok_or(EOP)?);
                }
                Some(VqLookup {
                    lookup_type,
                    minimum,
                    delta,
                    sequence_p,
                    lookup_values,
                    multiplicands,
                })
            }
            _ => return Err(Error::InvalidData("bad codebook lookup type")),
        };

        let huffman = Huffman::new(&lengths)?;
        Ok(Codebook {
            dims: dims as usize,
            entries,
            huffman,
            vq,
            table: Vec::new(),
        })
    }

    /// True if the book has a VQ value mapping usable in VQ context.
    pub fn has_vq(&self) -> bool {
        self.vq.is_some() && self.dims > 0
    }

    /// Precomputes the VQ table if it is small enough; `budget` is the
    /// remaining number of floats that may be precomputed for this stream.
    pub fn precompute(&mut self, budget: &mut usize) {
        let Some(vq) = &self.vq else { return };
        let size = self.entries as usize * self.dims;
        if size == 0 || size > MAX_VQ_TABLE || size > *budget || vq.multiplicands.is_empty() {
            return;
        }
        *budget -= size;
        let mut table = vec![0f32; size];
        for (e, chunk) in table.chunks_exact_mut(self.dims).enumerate() {
            self.compute_vector(e as u32, chunk);
        }
        self.table = table;
    }

    pub const MAX_VQ_TOTAL: usize = MAX_VQ_TOTAL;

    /// Computes VQ vector `entry` into `out` (`out.len() == dims`).
    fn compute_vector(&self, entry: u32, out: &mut [f32]) {
        let Some(vq) = &self.vq else {
            out.iter_mut().for_each(|v| *v = 0.0);
            return;
        };
        let mut last = 0f32;
        if vq.lookup_type == 1 {
            let mut divisor = 1u64;
            for v in out.iter_mut() {
                let off = ((entry as u64 / divisor) % vq.lookup_values.max(1) as u64) as usize;
                let m = vq.multiplicands.get(off).copied().unwrap_or(0);
                let val = m as f32 * vq.delta + vq.minimum + last;
                *v = val;
                if vq.sequence_p {
                    last = val;
                }
                divisor = divisor.saturating_mul(vq.lookup_values as u64);
            }
        } else {
            let base = entry as usize * self.dims;
            for (j, v) in out.iter_mut().enumerate() {
                let m = vq.multiplicands.get(base + j).copied().unwrap_or(0);
                let val = m as f32 * vq.delta + vq.minimum + last;
                *v = val;
                if vq.sequence_p {
                    last = val;
                }
            }
        }
    }

    /// Decodes one entry number (scalar context).
    #[inline(always)]
    pub fn decode_scalar(&self, br: &mut BitReader) -> Option<u32> {
        self.huffman.decode(br)
    }

    /// Decodes one VQ vector; returns a slice of `dims` values, either from
    /// the precomputed table or computed into `scratch`.
    #[inline(always)]
    pub fn decode_vector<'s>(
        &'s self,
        br: &mut BitReader,
        scratch: &'s mut Vec<f32>,
    ) -> Option<&'s [f32]> {
        let e = self.huffman.decode(br)? as usize;
        if !self.table.is_empty() {
            return self.table.get(e * self.dims..(e + 1) * self.dims);
        }
        scratch.clear();
        scratch.resize(self.dims, 0.0);
        self.compute_vector(e as u32, scratch);
        Some(&scratch[..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ilog_values() {
        assert_eq!(ilog(0), 0);
        assert_eq!(ilog(1), 1);
        assert_eq!(ilog(2), 2);
        assert_eq!(ilog(3), 2);
        assert_eq!(ilog(4), 3);
        assert_eq!(ilog(7), 3);
    }

    #[test]
    fn lookup1() {
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(lookup1_values(1, 65535), 1);
        assert_eq!(lookup1_values(1 << 23, 1), 1 << 23);
        assert_eq!(lookup1_values(1000, 3), 10);
        assert_eq!(lookup1_values(999, 3), 9);
    }

    #[test]
    fn float_unpack() {
        // 1.0 = mantissa 1 << 20 with exponent 788 - 20
        let x = (1u32 << 20) | ((788 - 20) << 21);
        assert_eq!(float32_unpack(x), 1.0);
        assert_eq!(float32_unpack(x | 0x8000_0000), -1.0);
    }

    #[test]
    fn huffman_spec_example() {
        // Spec example: lengths 2 4 4 4 4 2 3 3 -> 00 0100 0101 0110 0111 10 110 111
        let h = Huffman::new(&[2, 4, 4, 4, 4, 2, 3, 3]).unwrap();
        let words = [
            (0b00u32, 2u32),
            (0b0100, 4),
            (0b0101, 4),
            (0b0110, 4),
            (0b0111, 4),
            (0b10, 2),
            (0b110, 3),
            (0b111, 3),
        ];
        for (sym, &(w, len)) in words.iter().enumerate() {
            // Write the codeword MSB first into an LSB-first stream.
            let mut v = 0u32;
            for i in 0..len {
                v |= ((w >> (len - 1 - i)) & 1) << i;
            }
            let data = v.to_le_bytes();
            let mut br = BitReader::new(&data);
            assert_eq!(h.decode(&mut br), Some(sym as u32));
        }
        assert!(Huffman::new(&[1, 2, 2, 2]).is_err());
        assert!(Huffman::new(&[1, 2]).is_err());
        assert!(Huffman::new(&[0, 2]).is_err());
        assert!(Huffman::new(&[0, 1, 0]).is_ok());
    }

    #[test]
    fn huffman_long_codes() {
        // Lengths 1, 2, ..., 15, 15: a degenerate (but complete) tree.
        let mut lens: Vec<u8> = (1..=15).collect();
        lens.push(15);
        let h = Huffman::new(&lens).unwrap();
        for sym in 0..16u32 {
            // Codeword for sym k < 15 is k ones followed by a zero; the last
            // is fifteen ones.
            let len = if sym < 15 { sym + 1 } else { 15 };
            let ones = if sym < 15 { sym } else { 15 };
            let v: u32 = (1u32 << ones) - 1;
            let _ = len;
            let data = v.to_le_bytes();
            let mut br = BitReader::new(&data);
            assert_eq!(h.decode(&mut br), Some(sym));
        }
    }
}
