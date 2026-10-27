//! Table-driven decoding of canonical Huffman codes whose bits are packed
//! LSB first (deflate, WebP lossless).
//!
//! A table consists of a primary level indexed by the next `primary_bits`
//! input bits and, for longer codes, second-level tables. Each `u32` entry
//! packs:
//!
//! * bits 0..4: total code length in bits (number of bits to consume),
//! * bits 4..8: number of extra bits (payload defined by the user) or, for a
//!   subtable pointer, the number of index bits of the subtable,
//! * bits 8..16: flags ([`SUB`], [`INVALID`] and user flags),
//! * bits 16..32: value (symbol / base value / subtable offset).

use crate::Error;

/// Entry flag: pointer to a second-level table.
pub const SUB: u32 = 1 << 15;
/// Entry flag: no code maps to these bits (incomplete code).
pub const INVALID: u32 = 1 << 14;

pub struct HuffTable {
    pub entries: Vec<u32>,
    pub primary_bits: u32,
    mask: usize,
}

impl Default for HuffTable {
    fn default() -> Self {
        HuffTable { entries: vec![INVALID], primary_bits: 0, mask: 0 }
    }
}

impl HuffTable {
    /// Builds a table from code lengths (0 = unused symbol, max 15).
    /// `payload(symbol)` gives the bits 4..32 of the entry. Over-subscribed
    /// codes are rejected; incomplete codes are accepted (unused bit patterns
    /// decode to [`INVALID`]). When `single_zero_len` is set and exactly one
    /// symbol is used, it is decoded with zero bits (WebP lossless).
    pub fn build(
        lengths: &[u8],
        primary_bits: u32,
        single_zero_len: bool,
        payload: impl Fn(usize) -> u32,
    ) -> Result<HuffTable, Error> {
        let mut count = [0u32; 16];
        for &l in lengths {
            if l > 15 {
                return Err(Error::InvalidData("huffman code length > 15"));
            }
            count[l as usize] += 1;
        }
        count[0] = 0;
        let used: u32 = count.iter().sum();
        if used == 0 {
            return Ok(HuffTable { entries: vec![INVALID], primary_bits: 0, mask: 0 });
        }
        if used == 1 && single_zero_len {
            let sym = lengths.iter().position(|&l| l != 0).unwrap_or(0);
            return Ok(HuffTable { entries: vec![payload(sym) & !0xf], primary_bits: 0, mask: 0 });
        }
        let mut left: i32 = 1;
        let mut max_len = 0;
        for (len, &c) in count.iter().enumerate().skip(1) {
            left = (left << 1) - c as i32;
            if left < 0 {
                return Err(Error::InvalidData("over-subscribed huffman code"));
            }
            if c > 0 {
                max_len = len as u32;
            }
        }
        let pb = primary_bits.min(max_len).max(1);
        let mut next = [0u32; 16];
        let mut code = 0u32;
        for len in 1..16 {
            code = (code + count[len - 1]) << 1;
            next[len] = code;
        }
        let psize = 1usize << pb;
        let pmask = psize - 1;
        // Reversed codes (bit order as they appear in the stream).
        let mut rev = vec![0u32; lengths.len()];
        for (sym, &l) in lengths.iter().enumerate() {
            if l != 0 {
                let c = next[l as usize];
                next[l as usize] += 1;
                rev[sym] = (c << (32 - l as u32)).reverse_bits();
            }
        }
        // Size of the subtable for each primary prefix.
        let mut sub_len = vec![0u8; if max_len > pb { psize } else { 0 }];
        if max_len > pb {
            for (sym, &l) in lengths.iter().enumerate() {
                if l as u32 > pb {
                    let p = rev[sym] as usize & pmask;
                    sub_len[p] = sub_len[p].max(l - pb as u8);
                }
            }
        }
        let mut total = psize;
        let mut sub_off = vec![0u32; sub_len.len()];
        for (p, &sl) in sub_len.iter().enumerate() {
            if sl > 0 {
                sub_off[p] = total as u32;
                total += 1 << sl;
            }
        }
        if total > 1 << 16 {
            return Err(Error::InvalidData("huffman table too large"));
        }
        let mut entries = vec![INVALID; total];
        for (p, &sl) in sub_len.iter().enumerate() {
            if sl > 0 {
                entries[p] = SUB | ((sl as u32) << 4) | (sub_off[p] << 16) | pb;
            }
        }
        for (sym, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as u32;
            let e = (payload(sym) & !0xf) | l;
            let r = rev[sym] as usize;
            if l <= pb {
                let mut i = r;
                while i < psize {
                    entries[i] = e;
                    i += 1 << l;
                }
            } else {
                let p = r & pmask;
                let sl = sub_len[p] as u32;
                let base = sub_off[p] as usize;
                let mut i = r >> pb;
                while i < 1 << sl {
                    entries[base + i] = e;
                    i += 1 << (l - pb);
                }
            }
        }
        Ok(HuffTable { entries, primary_bits: pb, mask: pmask })
    }

    /// Looks up the entry for the next input bits (LSB = next bit). The
    /// caller must have at least 15 valid bits in `bits` (missing bits read
    /// as zero) and consumes `entry & 15` bits.
    #[inline(always)]
    pub fn lookup(&self, bits: u64) -> u32 {
        let e = self.entries[bits as usize & self.mask];
        if e & SUB == 0 {
            return e;
        }
        let sb = (e >> 4) & 15;
        let idx = (e >> 16) as usize + ((bits >> self.primary_bits) as usize & ((1 << sb) - 1));
        self.entries.get(idx).copied().unwrap_or(INVALID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_and_decodes_long_codes() {
        // Lengths 1,2,3,...,15,15 form a complete code.
        let mut lens: Vec<u8> = (1..=15).collect();
        lens.push(15);
        let t = HuffTable::build(&lens, 8, false, |s| (s as u32) << 16).unwrap();
        // Canonical code for symbol k (k < 15) is k ones followed by a zero.
        for sym in 0..15u32 {
            let len = sym + 1;
            let code: u64 = (1u64 << sym) - 1; // LSB-first: `sym` ones then a zero
            let e = t.lookup(code);
            assert_eq!(e >> 16, sym);
            assert_eq!(e & 15, len);
        }
        let e = t.lookup((1 << 15) - 1);
        assert_eq!(e >> 16, 15);
    }

    #[test]
    fn rejects_oversubscribed() {
        assert!(HuffTable::build(&[1, 1, 1], 8, false, |_| 0).is_err());
    }
}
