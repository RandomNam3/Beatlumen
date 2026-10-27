//! zlib (RFC 1950) / raw deflate (RFC 1951) decompression (also used by
//! other crates, e.g. for compressed ID3 frames).
//!
//! The decoder keeps a 64-bit bit buffer that is refilled eight bytes at a
//! time and decodes symbols through two-level lookup tables, so one refill
//! covers a complete length/distance pair.

use crate::huffman::{HuffTable, INVALID};
use crate::Error;

/// Decompresses a zlib stream (RFC 1950). The Adler-32 checksum is verified.
pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, Error> {
    zlib_decompress_with_limit(data, usize::MAX)
}

/// Decompresses a raw deflate stream (RFC 1951).
pub fn inflate(data: &[u8]) -> Result<Vec<u8>, Error> {
    inflate_with_limit(data, usize::MAX)
}

/// Like [`zlib_decompress`], but fails when the output would exceed
/// `max_output` bytes (protection against decompression bombs).
pub fn zlib_decompress_with_limit(data: &[u8], max_output: usize) -> Result<Vec<u8>, Error> {
    let body = zlib_header(data)?;
    let mut d = Inflater::new(body, 0, max_output);
    d.run()?;
    let consumed = d.bytes_consumed();
    let trailer = body
        .get(consumed..consumed + 4)
        .ok_or(Error::InvalidData("zlib: missing checksum"))?;
    let expected = u32::from_be_bytes([trailer[0], trailer[1], trailer[2], trailer[3]]);
    let out = d.finish();
    if adler32(&out) != expected {
        return Err(Error::InvalidData("zlib: checksum mismatch"));
    }
    Ok(out)
}

/// Like [`inflate`], but fails when the output would exceed `max_output`
/// bytes.
pub fn inflate_with_limit(data: &[u8], max_output: usize) -> Result<Vec<u8>, Error> {
    let mut d = Inflater::new(data, 0, max_output);
    d.run()?;
    Ok(d.finish())
}

/// Lenient zlib decompression for image decoders: the checksum is ignored,
/// decoding stops silently once `limit` bytes were produced, and on errors the
/// data decoded so far is returned together with the error.
pub(crate) fn zlib_decompress_partial(
    data: &[u8],
    size_hint: usize,
    limit: usize,
) -> (Vec<u8>, Option<Error>) {
    let body = match zlib_header(data) {
        Ok(b) => b,
        Err(e) => return (Vec::new(), Some(e)),
    };
    let mut d = Inflater::new(body, size_hint, limit);
    d.stop_at_limit = true;
    let r = d.run();
    (d.finish(), r.err())
}

fn zlib_header(data: &[u8]) -> Result<&[u8], Error> {
    if data.len() < 2 {
        return Err(Error::InvalidData("zlib: truncated header"));
    }
    let (cmf, flg) = (data[0], data[1]);
    if cmf & 0x0f != 8 || cmf >> 4 > 7 {
        return Err(Error::InvalidData("zlib: bad compression method"));
    }
    if (cmf as u16 * 256 + flg as u16) % 31 != 0 {
        return Err(Error::InvalidData("zlib: bad header check"));
    }
    if flg & 0x20 != 0 {
        return Err(Error::Unsupported("zlib: preset dictionary"));
    }
    Ok(&data[2..])
}

/// Adler-32 checksum (RFC 1950).
pub fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        let mut quads = chunk.chunks_exact(4);
        for q in &mut quads {
            a += q[0] as u32;
            b += a;
            a += q[1] as u32;
            b += a;
            a += q[2] as u32;
            b += a;
            a += q[3] as u32;
            b += a;
        }
        for &x in quads.remainder() {
            a += x as u32;
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

// Entry flags (bits 8..14 of a table entry, see `huffman`).
const LITERAL: u32 = 1 << 8;
const END_OF_BLOCK: u32 = 1 << 9;

pub(crate) const LENGTH_BASE: [u16; 29] = [
    3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115, 131,
    163, 195, 227, 258,
];
pub(crate) const LENGTH_EXTRA: [u8; 29] = [
    0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
];
pub(crate) const DIST_BASE: [u16; 30] = [
    1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
    2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
];
pub(crate) const DIST_EXTRA: [u8; 30] = [
    0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13,
    13,
];
pub(crate) const CL_ORDER: [usize; 19] = [
    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
];

const LITLEN_BITS: u32 = 10;
const DIST_BITS: u32 = 8;

fn litlen_payload(sym: usize) -> u32 {
    match sym {
        0..=255 => LITERAL | ((sym as u32) << 16),
        256 => END_OF_BLOCK,
        257..=285 => {
            let i = sym - 257;
            ((LENGTH_BASE[i] as u32) << 16) | ((LENGTH_EXTRA[i] as u32) << 4)
        }
        _ => INVALID,
    }
}

fn dist_payload(sym: usize) -> u32 {
    if sym < 30 {
        ((DIST_BASE[sym] as u32) << 16) | ((DIST_EXTRA[sym] as u32) << 4)
    } else {
        INVALID
    }
}

/// LSB-first bit reader over a byte slice. Reads past the end yield zero bits;
/// `overrun` counts the virtual zero bytes so truncation can be detected.
#[derive(Clone, Copy)]
pub(crate) struct BitReader<'a> {
    data: &'a [u8],
    /// Next byte to load (may exceed `data.len()` by the virtual bytes).
    pos: usize,
    pub bitbuf: u64,
    pub bitcnt: u32,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BitReader {
            data,
            pos: 0,
            bitbuf: 0,
            bitcnt: 0,
        }
    }

    /// Ensures at least 56 bits are buffered.
    #[inline(always)]
    pub fn refill(&mut self) {
        if let Some(b) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            self.bitbuf |= w << self.bitcnt;
            self.pos += ((63 - self.bitcnt) >> 3) as usize;
            self.bitcnt |= 56;
        } else {
            self.refill_slow();
        }
    }

    #[cold]
    fn refill_slow(&mut self) {
        while self.bitcnt <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.bitbuf |= (b as u64) << self.bitcnt;
            self.pos += 1;
            self.bitcnt += 8;
        }
    }

    /// True once bits beyond the end of the input were consumed.
    #[inline]
    pub fn overrun(&self) -> bool {
        self.pos > self.data.len() && (self.pos - self.data.len()) * 8 > self.bitcnt as usize
    }

    #[inline(always)]
    pub fn consume(&mut self, n: u32) {
        self.bitbuf >>= n;
        self.bitcnt -= n;
    }

    /// Reads `n <= 32` bits (requires a prior refill providing them).
    #[inline(always)]
    pub fn take(&mut self, n: u32) -> u32 {
        let v = (self.bitbuf & ((1u64 << n) - 1)) as u32;
        self.consume(n);
        v
    }

    /// Reads `n <= 32` bits, refilling as needed.
    #[inline]
    pub fn bits(&mut self, n: u32) -> u32 {
        if self.bitcnt < n {
            self.refill();
        }
        self.take(n)
    }

    /// Discards bits up to the next byte boundary and returns the byte
    /// position of the next unread byte (may exceed the input length).
    pub fn align_to_byte(&mut self) -> usize {
        let drop = self.bitcnt & 7;
        self.consume(drop);
        let p = self.pos - (self.bitcnt / 8) as usize;
        self.bitbuf = 0;
        self.bitcnt = 0;
        self.pos = p;
        p
    }

    pub fn set_pos(&mut self, pos: usize) {
        self.pos = pos;
        self.bitbuf = 0;
        self.bitcnt = 0;
    }

    pub fn data(&self) -> &'a [u8] {
        self.data
    }
}

struct Inflater<'a> {
    br: BitReader<'a>,
    out: Vec<u8>,
    /// Number of valid bytes in `out` (the vector is kept zero-filled beyond).
    opos: usize,
    limit: usize,
    stop_at_limit: bool,
    litlen: HuffTable,
    dist: HuffTable,
}

impl<'a> Inflater<'a> {
    fn new(data: &'a [u8], size_hint: usize, limit: usize) -> Self {
        let initial = if size_hint > 0 {
            size_hint
        } else {
            data.len().saturating_mul(4).max(1024)
        }
        .min(limit);
        Inflater {
            br: BitReader::new(data),
            out: vec![0; initial],
            opos: 0,
            limit,
            stop_at_limit: false,
            litlen: HuffTable::default(),
            dist: HuffTable::default(),
        }
    }

    fn finish(mut self) -> Vec<u8> {
        self.out.truncate(self.opos);
        self.out
    }

    fn bytes_consumed(&self) -> usize {
        let mut br = self.br;
        br.align_to_byte()
    }

    /// Makes room for `n` more output bytes. Returns false when the limit is
    /// reached in `stop_at_limit` mode.
    #[inline]
    fn reserve(&mut self, n: usize) -> Result<bool, Error> {
        if self.opos + n <= self.out.len() {
            return Ok(true);
        }
        self.grow(n)
    }

    #[cold]
    fn grow(&mut self, n: usize) -> Result<bool, Error> {
        let needed = self.opos + n;
        if needed > self.limit {
            if self.stop_at_limit {
                if self.out.len() < self.limit {
                    self.out.resize(self.limit, 0);
                }
                return Ok(false);
            }
            return Err(Error::InvalidData("inflate: output size limit exceeded"));
        }
        let new_len = needed
            .max(self.out.len().saturating_mul(2))
            .max(4096)
            .min(self.limit);
        self.out.resize(new_len, 0);
        Ok(true)
    }

    fn run(&mut self) -> Result<(), Error> {
        loop {
            self.br.refill();
            let header = self.br.take(3);
            let r = match header >> 1 {
                0 => self.stored_block(),
                1 => {
                    self.build_fixed()?;
                    self.huffman_block()
                }
                2 => {
                    self.read_dynamic_tables()?;
                    self.huffman_block()
                }
                _ => Err(Error::InvalidData("inflate: invalid block type")),
            };
            match r {
                Ok(true) => {}
                Ok(false) => return Ok(()), // output limit reached (lenient mode)
                Err(e) => return Err(e),
            }
            if self.br.overrun() {
                return Err(Error::InvalidData("inflate: truncated stream"));
            }
            if header & 1 != 0 {
                return Ok(());
            }
        }
    }

    fn stored_block(&mut self) -> Result<bool, Error> {
        let p = self.br.align_to_byte();
        let data = self.br.data();
        let hdr = data
            .get(p..p + 4)
            .ok_or(Error::InvalidData("inflate: truncated stored block"))?;
        let len = u16::from_le_bytes([hdr[0], hdr[1]]) as usize;
        let nlen = u16::from_le_bytes([hdr[2], hdr[3]]) as usize;
        if len != !nlen & 0xffff {
            return Err(Error::InvalidData("inflate: stored block length mismatch"));
        }
        let start = p + 4;
        let avail = data.len().saturating_sub(start).min(len);
        let mut n = avail;
        if !self.reserve(n)? {
            n = self.limit - self.opos;
            self.out[self.opos..self.opos + n].copy_from_slice(&data[start..start + n]);
            self.opos += n;
            return Ok(false);
        }
        self.out[self.opos..self.opos + n].copy_from_slice(&data[start..start + n]);
        self.opos += n;
        if avail < len {
            return Err(Error::InvalidData("inflate: truncated stored block"));
        }
        self.br.set_pos(start + len);
        Ok(true)
    }

    fn build_fixed(&mut self) -> Result<(), Error> {
        let mut lens = [0u8; 288];
        lens[..144].fill(8);
        lens[144..256].fill(9);
        lens[256..280].fill(7);
        lens[280..].fill(8);
        self.litlen = HuffTable::build(&lens, LITLEN_BITS, false, litlen_payload)?;
        self.dist = HuffTable::build(&[5u8; 30], DIST_BITS, false, dist_payload)?;
        Ok(())
    }

    fn read_dynamic_tables(&mut self) -> Result<(), Error> {
        let br = &mut self.br;
        br.refill();
        let hlit = br.take(5) as usize + 257;
        let hdist = br.take(5) as usize + 1;
        let hclen = br.take(4) as usize + 4;
        if hlit > 286 || hdist > 30 {
            return Err(Error::InvalidData(
                "inflate: too many length or distance codes",
            ));
        }
        let mut cl_lens = [0u8; 19];
        for &idx in CL_ORDER.iter().take(hclen) {
            cl_lens[idx] = br.bits(3) as u8;
        }
        let cl = HuffTable::build(&cl_lens, 7, false, |s| (s as u32) << 16)?;
        let mut lens = [0u8; 286 + 30];
        let total = hlit + hdist;
        let mut i = 0;
        while i < total {
            br.refill();
            if br.overrun() {
                return Err(Error::InvalidData("inflate: truncated code lengths"));
            }
            let e = cl.lookup(br.bitbuf);
            if e & INVALID != 0 {
                return Err(Error::InvalidData("inflate: invalid code length code"));
            }
            br.consume(e & 15);
            let sym = (e >> 16) as u8;
            let (val, rep) = match sym {
                0..=15 => (sym, 1),
                16 => {
                    if i == 0 {
                        return Err(Error::InvalidData(
                            "inflate: repeat without previous length",
                        ));
                    }
                    (lens[i - 1], 3 + br.take(2) as usize)
                }
                17 => (0, 3 + br.take(3) as usize),
                _ => (0, 11 + br.take(7) as usize),
            };
            if i + rep > total {
                return Err(Error::InvalidData("inflate: code lengths overflow"));
            }
            lens[i..i + rep].fill(val);
            i += rep;
        }
        if lens[256] == 0 {
            return Err(Error::InvalidData("inflate: missing end-of-block code"));
        }
        self.litlen = HuffTable::build(&lens[..hlit], LITLEN_BITS, false, litlen_payload)?;
        self.dist = HuffTable::build(&lens[hlit..total], DIST_BITS, false, dist_payload)?;
        Ok(())
    }

    /// Decodes one Huffman-coded block. Returns Ok(false) when the output
    /// limit was reached in lenient mode.
    fn huffman_block(&mut self) -> Result<bool, Error> {
        // Work on local copies of the bit reader state so that it stays in
        // registers (stores into `out` could otherwise alias it).
        let mut br = self.br;
        let r = self.huffman_block_inner(&mut br);
        self.br = br;
        r
    }

    #[inline(always)]
    fn huffman_block_inner(&mut self, br: &mut BitReader<'a>) -> Result<bool, Error> {
        let litlen = std::mem::take(&mut self.litlen);
        let dist_t = std::mem::take(&mut self.dist);
        let r = self.decode_symbols(br, &litlen, &dist_t);
        self.litlen = litlen;
        self.dist = dist_t;
        r
    }

    fn decode_symbols(
        &mut self,
        br: &mut BitReader<'a>,
        litlen: &HuffTable,
        dist_t: &HuffTable,
    ) -> Result<bool, Error> {
        let data = br.data;
        let (mut pos, mut bitbuf, mut bitcnt) = (br.pos, br.bitbuf, br.bitcnt);
        let mut opos = self.opos;
        macro_rules! save {
            () => {
                br.pos = pos;
                br.bitbuf = bitbuf;
                br.bitcnt = bitcnt;
                self.opos = opos;
            };
        }
        let result = loop {
            // Refill to at least 56 bits.
            if let Some(b) = data.get(pos..pos + 8) {
                let w = u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
                bitbuf |= w << bitcnt;
                pos += ((63 - bitcnt) >> 3) as usize;
                bitcnt |= 56;
            } else {
                while bitcnt <= 56 {
                    bitbuf |= (data.get(pos).copied().unwrap_or(0) as u64) << bitcnt;
                    pos += 1;
                    bitcnt += 8;
                }
                if pos > data.len() + 16 {
                    break Err(Error::InvalidData("inflate: truncated stream"));
                }
            }
            let e = litlen.lookup(bitbuf);
            bitbuf >>= e & 15;
            bitcnt -= e & 15;
            if e & LITERAL != 0 {
                // Up to three literals per refill (each code is <= 15 bits
                // and at least 41 bits remain after the first).
                if opos + 3 > self.out.len() {
                    // Near the end of the buffer: one literal at a time.
                    self.opos = opos;
                    if opos >= self.out.len() && !self.grow(1)? {
                        break Ok(false);
                    }
                    self.out[opos] = (e >> 16) as u8;
                    opos += 1;
                    continue;
                }
                self.out[opos] = (e >> 16) as u8;
                opos += 1;
                let e2 = litlen.lookup(bitbuf);
                if e2 & LITERAL != 0 {
                    bitbuf >>= e2 & 15;
                    bitcnt -= e2 & 15;
                    self.out[opos] = (e2 >> 16) as u8;
                    opos += 1;
                    let e3 = litlen.lookup(bitbuf);
                    if e3 & LITERAL != 0 && bitcnt >= 15 {
                        bitbuf >>= e3 & 15;
                        bitcnt -= e3 & 15;
                        self.out[opos] = (e3 >> 16) as u8;
                        opos += 1;
                    }
                }
                continue;
            }
            if e & (END_OF_BLOCK | INVALID) != 0 {
                if e & INVALID != 0 {
                    break Err(Error::InvalidData("inflate: invalid literal/length code"));
                }
                save!();
                if br.overrun() {
                    break Err(Error::InvalidData("inflate: truncated stream"));
                }
                break Ok(true);
            }
            let eb = (e >> 4) & 15;
            let len = (e >> 16) as usize + (bitbuf & ((1u64 << eb) - 1)) as usize;
            bitbuf >>= eb;
            bitcnt -= eb;
            let d = dist_t.lookup(bitbuf);
            if d & INVALID != 0 {
                break Err(Error::InvalidData("inflate: invalid distance code"));
            }
            bitbuf >>= d & 15;
            bitcnt -= d & 15;
            let db = (d >> 4) & 15;
            let dist = (d >> 16) as usize + (bitbuf & ((1u64 << db) - 1)) as usize;
            bitbuf >>= db;
            bitcnt -= db;
            if dist > opos {
                break Err(Error::InvalidData("inflate: distance too far back"));
            }
            if opos + len > self.out.len() {
                self.opos = opos;
                if !self.grow(len)? {
                    let n = self.limit - opos;
                    copy_match(&mut self.out, opos, dist, n);
                    opos += n;
                    save!();
                    break Ok(false);
                }
            }
            copy_match(&mut self.out, opos, dist, len);
            opos += len;
        };
        save!();
        result
    }
}

/// Copies `len` bytes from `pos - dist` to `pos` (regions may overlap).
#[inline(always)]
fn copy_match(out: &mut [u8], pos: usize, dist: usize, len: usize) {
    let src = pos - dist;
    if dist >= len {
        out.copy_within(src..src + len, pos);
    } else if dist == 1 {
        let b = out[src];
        out[pos..pos + len].fill(b);
    } else {
        // Overlapping copy: the output is periodic with period `dist`, so
        // copy whole periods in chunks that double in size.
        let mut copied = 0;
        while copied < len {
            let n = (pos + copied - src).min(len - copied);
            out.copy_within(src..src + n, pos + copied);
            copied += n;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stored_and_fixed() {
        // "hello" stored: BFINAL=1, BTYPE=00
        let data = [0x01, 0x05, 0x00, 0xfa, 0xff, b'h', b'e', b'l', b'l', b'o'];
        assert_eq!(inflate(&data).unwrap(), b"hello");
        // zlib of "a" (fixed huffman), from zlib itself.
        let z = [0x78, 0x9c, 0x4b, 0x04, 0x00, 0x00, 0x62, 0x00, 0x62];
        assert_eq!(zlib_decompress(&z).unwrap(), b"a");
    }

    #[test]
    fn overlapping_copy() {
        let mut v = vec![1u8, 2, 3, 0, 0, 0, 0, 0, 0, 0, 0];
        copy_match(&mut v, 3, 3, 8);
        assert_eq!(v, [1, 2, 3, 1, 2, 3, 1, 2, 3, 1, 2]);
        let mut v = vec![7u8, 9, 0, 0, 0, 0, 0];
        copy_match(&mut v, 2, 2, 5);
        assert_eq!(v, [7, 9, 7, 9, 7, 9, 7]);
    }

    #[test]
    fn adler() {
        assert_eq!(adler32(b"Wikipedia"), 0x11E60398);
    }
}
