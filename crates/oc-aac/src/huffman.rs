//! Table-driven Huffman decoding for the AAC scalefactor and spectral
//! codebooks (two-level lookup tables built once from the standard
//! `(codeword, length)` lists in [`crate::huffman_tables`]).

use crate::bits::BitReader;
use crate::huffman_tables::*;
use std::sync::OnceLock;

const SUB_FLAG: u32 = 1 << 5;

/// Two-level Huffman lookup table.
///
/// Entry layout: bits 0..5 = code length (0 = invalid) or, with `SUB_FLAG`,
/// the number of index bits of the second-level table; bits 8.. = symbol or
/// second-level offset.
pub(crate) struct HuffTable {
    root_bits: u32,
    entries: Vec<u32>,
}

impl HuffTable {
    /// Builds a table from `(code, len)` pairs; the symbol is the index.
    pub fn new(codes: &[(u32, u8)], root_bits: u32) -> HuffTable {
        let root_size = 1usize << root_bits;
        let mut entries = vec![0u32; root_size];
        // Short codes fill the root directly.
        for (sym, &(code, len)) in codes.iter().enumerate() {
            let len = len as u32;
            if len == 0 || len > root_bits {
                continue;
            }
            let first = (code << (root_bits - len)) as usize;
            let count = 1usize << (root_bits - len);
            for e in &mut entries[first..first + count] {
                *e = ((sym as u32) << 8) | len;
            }
        }
        // Long codes: group by root prefix.
        let mut prefixes: Vec<u32> = codes
            .iter()
            .filter(|&&(_, l)| l as u32 > root_bits)
            .map(|&(c, l)| c >> (l as u32 - root_bits))
            .collect();
        prefixes.sort_unstable();
        prefixes.dedup();
        for p in prefixes {
            let max_len = codes
                .iter()
                .filter(|&&(c, l)| l as u32 > root_bits && c >> (l as u32 - root_bits) == p)
                .map(|&(_, l)| l as u32)
                .max()
                .unwrap_or(root_bits);
            let sub_bits = max_len - root_bits;
            let offset = entries.len();
            entries.resize(offset + (1usize << sub_bits), 0);
            entries[p as usize] = ((offset as u32) << 8) | SUB_FLAG | sub_bits;
            for (sym, &(code, len)) in codes.iter().enumerate() {
                let len = len as u32;
                if len <= root_bits || code >> (len - root_bits) != p {
                    continue;
                }
                let rem = len - root_bits;
                let low = code & ((1 << rem) - 1);
                let first = (low << (sub_bits - rem)) as usize;
                let count = 1usize << (sub_bits - rem);
                for e in &mut entries[offset + first..offset + first + count] {
                    *e = ((sym as u32) << 8) | rem;
                }
            }
        }
        HuffTable { root_bits, entries }
    }

    /// Decodes one symbol; `None` for an invalid code.
    #[inline(always)]
    pub fn decode(&self, r: &mut BitReader) -> Option<u32> {
        let bits = r.peek32();
        let e = self.entries[(bits >> (32 - self.root_bits)) as usize];
        if e & SUB_FLAG == 0 {
            let len = e & 31;
            if len == 0 {
                return None;
            }
            r.skip(len as usize);
            return Some(e >> 8);
        }
        let sub_bits = e & 31;
        let idx = (bits << self.root_bits) >> (32 - sub_bits);
        let e2 = *self.entries.get((e >> 8) as usize + idx as usize)?;
        let len = e2 & 31;
        if len == 0 {
            return None;
        }
        r.skip((self.root_bits + len) as usize);
        Some(e2 >> 8)
    }
}

/// A spectral codebook with its unpacked value table.
pub(crate) struct SpectralBook {
    pub table: HuffTable,
    /// 2 or 4 values per codeword.
    pub dim: usize,
    pub signed: bool,
    /// Unpacked values per symbol (unused trailing entries are 0).
    pub values: Vec<[i8; 4]>,
}

pub(crate) struct Books {
    pub scalefactor: HuffTable,
    /// Spectral codebooks 1..=11 at indices 0..=10.
    pub spectral: Vec<SpectralBook>,
}

fn spectral_book(codes: &[(u32, u8)], cb: usize) -> SpectralBook {
    let (dim, signed, modulo, offset) = match cb {
        1 | 2 => (4, true, 3, 1),
        3 | 4 => (4, false, 3, 0),
        5 | 6 => (2, true, 9, 4),
        7 | 8 => (2, false, 8, 0),
        9 | 10 => (2, false, 13, 0),
        _ => (2, false, 17, 0),
    };
    let values = (0..codes.len())
        .map(|idx| {
            let mut v = [0i8; 4];
            let mut rem = idx as i32;
            for d in (0..dim).rev() {
                v[d] = (rem % modulo - offset) as i8;
                rem /= modulo;
            }
            v
        })
        .collect();
    SpectralBook { table: HuffTable::new(codes, 9), dim, signed, values }
}

pub(crate) fn books() -> &'static Books {
    static BOOKS: OnceLock<Books> = OnceLock::new();
    BOOKS.get_or_init(|| {
        let lists: [&[(u32, u8)]; 11] = [
            &SPECTRUM_CB1,
            &SPECTRUM_CB2,
            &SPECTRUM_CB3,
            &SPECTRUM_CB4,
            &SPECTRUM_CB5,
            &SPECTRUM_CB6,
            &SPECTRUM_CB7,
            &SPECTRUM_CB8,
            &SPECTRUM_CB9,
            &SPECTRUM_CB10,
            &SPECTRUM_CB11,
        ];
        Books {
            scalefactor: HuffTable::new(&SCALEFACTOR_CB, 9),
            spectral: lists.iter().enumerate().map(|(i, l)| spectral_book(l, i + 1)).collect(),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes codes MSB first and checks that every symbol round-trips.
    fn roundtrip(codes: &[(u32, u8)]) {
        let t = HuffTable::new(codes, 9);
        for (sym, &(code, len)) in codes.iter().enumerate() {
            let v = (code as u64) << (64 - len as u32);
            let bytes = v.to_be_bytes();
            let mut r = BitReader::new(&bytes);
            assert_eq!(t.decode(&mut r), Some(sym as u32));
            assert_eq!(r.position(), len as usize);
        }
    }

    #[test]
    fn all_codebooks_roundtrip() {
        roundtrip(&SCALEFACTOR_CB);
        for l in [
            &SPECTRUM_CB1[..],
            &SPECTRUM_CB2,
            &SPECTRUM_CB3,
            &SPECTRUM_CB4,
            &SPECTRUM_CB5,
            &SPECTRUM_CB6,
            &SPECTRUM_CB7,
            &SPECTRUM_CB8,
            &SPECTRUM_CB9,
            &SPECTRUM_CB10,
            &SPECTRUM_CB11,
        ] {
            roundtrip(l);
        }
    }

    #[test]
    fn unpacking() {
        let b = books();
        // Codebook 1 index 40 is (0,0,0,0); index 0 is (-1,-1,-1,-1).
        assert_eq!(b.spectral[0].values[40], [0, 0, 0, 0]);
        assert_eq!(b.spectral[0].values[0], [-1, -1, -1, -1]);
        assert_eq!(b.spectral[4].values[0][..2], [-4, -4]);
        assert_eq!(b.spectral[10].values[288][..2], [16, 16]);
        // Scalefactor index 60 (delta 0) has the 1-bit code 0.
        assert_eq!(SCALEFACTOR_CB[60], (0, 1));
    }
}
