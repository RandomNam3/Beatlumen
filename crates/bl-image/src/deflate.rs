//! Deflate (RFC 1951) / zlib (RFC 1950) compression.
//!
//! LZ77 matching uses hash chains over 3-byte prefixes with lazy match
//! evaluation (similar in spirit to zlib's levels). Blocks are emitted with
//! whichever of dynamic Huffman, fixed Huffman or stored coding is smallest;
//! dynamic codes are length-limited optimal codes built with the
//! package-merge algorithm.

use crate::inflate::{adler32, CL_ORDER, DIST_BASE, DIST_EXTRA, LENGTH_BASE, LENGTH_EXTRA};

/// Compresses `data` into a zlib stream. `level` 0 stores, 1..=9 trade speed
/// for compression (6 is a good default).
pub fn zlib_compress(data: &[u8], level: u8) -> Vec<u8> {
    let flevel = match level {
        0 | 1 => 0,
        2..=5 => 1,
        6 => 2,
        _ => 3,
    };
    let cmf = 0x78u8;
    let mut flg = flevel << 6;
    flg += (31 - ((cmf as u16 * 256 + flg as u16) % 31) as u8) % 31;
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    out.push(cmf);
    out.push(flg);
    deflate_into(data, level, &mut out);
    out.extend_from_slice(&adler32(data).to_be_bytes());
    out
}

/// Compresses `data` into a raw deflate stream (see [`zlib_compress`] for
/// `level`).
pub fn deflate(data: &[u8], level: u8) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len() / 2 + 64);
    deflate_into(data, level, &mut out);
    out
}

fn deflate_into(data: &[u8], level: u8, out: &mut Vec<u8>) {
    let mut w = BitWriter { out: std::mem::take(out), buf: 0, cnt: 0 };
    if level == 0 {
        write_stored(&mut w, data, true);
    } else {
        let mut c = Compressor::new(data, params(level));
        c.run(&mut w);
    }
    w.flush();
    *out = w.out;
}

struct BitWriter {
    out: Vec<u8>,
    buf: u64,
    cnt: u32,
}

impl BitWriter {
    #[inline(always)]
    fn put(&mut self, bits: u32, n: u32) {
        self.buf |= (bits as u64) << self.cnt;
        self.cnt += n;
        if self.cnt >= 32 {
            self.out.extend_from_slice(&(self.buf as u32).to_le_bytes());
            self.buf >>= 32;
            self.cnt -= 32;
        }
    }

    /// Pads to a byte boundary and writes out all pending bits.
    fn flush(&mut self) {
        while self.cnt > 0 {
            self.out.push(self.buf as u8);
            self.buf >>= 8;
            self.cnt = self.cnt.saturating_sub(8);
        }
        self.buf = 0;
    }
}

fn write_stored(w: &mut BitWriter, data: &[u8], last: bool) {
    let mut chunks = data.chunks(65535).peekable();
    if data.is_empty() {
        w.put(last as u32, 3);
        w.flush();
        w.out.extend_from_slice(&[0, 0, 0xff, 0xff]);
        return;
    }
    while let Some(chunk) = chunks.next() {
        let final_chunk = last && chunks.peek().is_none();
        w.put(final_chunk as u32, 3);
        w.flush();
        let len = chunk.len() as u16;
        w.out.extend_from_slice(&len.to_le_bytes());
        w.out.extend_from_slice(&(!len).to_le_bytes());
        w.out.extend_from_slice(chunk);
    }
}

#[derive(Clone, Copy)]
struct Params {
    max_chain: u32,
    nice: usize,
    good: usize,
    lazy: bool,
    /// Do not look for a better match when the previous one is this long.
    max_lazy: usize,
}

fn params(level: u8) -> Params {
    let (max_chain, nice, good, lazy, max_lazy) = match level {
        1 => (4, 16, 4, false, 0),
        2 => (8, 32, 4, false, 0),
        3 => (24, 64, 8, false, 0),
        4 => (16, 32, 4, true, 16),
        5 => (32, 64, 8, true, 32),
        6 => (128, 128, 8, true, 32),
        7 => (256, 192, 16, true, 64),
        8 => (1024, 258, 32, true, 258),
        _ => (4096, 258, 32, true, 258),
    };
    Params { max_chain, nice, good, lazy, max_lazy }
}

const WSIZE: usize = 32768;
const WMASK: usize = WSIZE - 1;
const HASH_BITS: u32 = 15;
const NIL: u32 = u32::MAX;
const MIN_MATCH: usize = 3;
const MAX_MATCH: usize = 258;
/// Length-3 matches further away than this are not worth it.
const TOO_FAR: usize = 4096;
const MATCH_FLAG: u32 = 1 << 31;
/// Symbols per block.
const BLOCK_SYMBOLS: usize = 1 << 15;

struct Compressor<'a> {
    data: &'a [u8],
    p: Params,
    head: Vec<u32>,
    prev: Vec<u32>,
    syms: Vec<u32>,
    lit_freq: [u32; 286],
    dist_freq: [u32; 30],
    /// Start of the raw data covered by the current block.
    block_start: usize,
}

#[inline(always)]
fn hash3(d: &[u8], pos: usize) -> usize {
    let v = (d[pos] as u32) | (d[pos + 1] as u32) << 8 | (d[pos + 2] as u32) << 16;
    (v.wrapping_mul(0x9E37_79B1) >> (32 - HASH_BITS)) as usize
}

#[inline(always)]
fn match_len(d: &[u8], a: usize, b: usize, max: usize) -> usize {
    let mut l = 0;
    while l + 8 <= max {
        let x = u64::from_le_bytes(d[a + l..a + l + 8].try_into().unwrap_or([0; 8]))
            ^ u64::from_le_bytes(d[b + l..b + l + 8].try_into().unwrap_or([0; 8]));
        if x != 0 {
            return l + (x.trailing_zeros() / 8) as usize;
        }
        l += 8;
    }
    while l < max && d[a + l] == d[b + l] {
        l += 1;
    }
    l
}

#[inline]
fn len_code(len: usize) -> usize {
    let l1 = len - 3;
    if len == 258 {
        28
    } else if l1 < 8 {
        l1
    } else {
        let nb = 31 - (l1 as u32).leading_zeros();
        (4 * (nb - 1) + ((l1 as u32 >> (nb - 2)) & 3)) as usize
    }
}

#[inline]
fn dist_code(dist: usize) -> usize {
    let d1 = (dist - 1) as u32;
    if d1 < 2 {
        d1 as usize
    } else {
        let nb = 31 - d1.leading_zeros();
        (2 * nb + ((d1 >> (nb - 1)) & 1)) as usize
    }
}

impl<'a> Compressor<'a> {
    fn new(data: &'a [u8], p: Params) -> Self {
        Compressor {
            data,
            p,
            head: vec![NIL; 1 << HASH_BITS],
            prev: vec![NIL; WSIZE],
            syms: Vec::with_capacity(BLOCK_SYMBOLS + 2),
            lit_freq: [0; 286],
            dist_freq: [0; 30],
            block_start: 0,
        }
    }

    /// Inserts `pos` into the hash chains and returns the previous chain head.
    #[inline(always)]
    fn insert(&mut self, pos: usize) -> u32 {
        if pos + MIN_MATCH > self.data.len() {
            return NIL;
        }
        let h = hash3(self.data, pos);
        let old = self.head[h];
        self.prev[pos & WMASK] = old;
        self.head[h] = pos as u32;
        old
    }

    fn longest_match(&self, pos: usize, mut cand: u32, prev_len: usize) -> (usize, usize) {
        let d = self.data;
        let max = (d.len() - pos).min(MAX_MATCH);
        if max < MIN_MATCH {
            return (0, 0);
        }
        let mut best = prev_len.max(MIN_MATCH - 1);
        let mut best_dist = 0;
        let mut chain = if prev_len >= self.p.good { self.p.max_chain / 4 } else { self.p.max_chain };
        let nice = self.p.nice.min(max);
        let mut last = pos as u32;
        while cand != NIL && cand < last && pos - cand as usize <= WSIZE && chain > 0 {
            let c = cand as usize;
            if best < max && d[c + best] == d[pos + best] && d[c] == d[pos] {
                let l = match_len(d, c, pos, max);
                if l > best {
                    best = l;
                    best_dist = pos - c;
                    if l >= nice {
                        break;
                    }
                }
            }
            last = cand;
            cand = self.prev[c & WMASK];
            chain -= 1;
        }
        if best_dist == 0 || (best == MIN_MATCH && best_dist > TOO_FAR) {
            (0, 0)
        } else {
            (best, best_dist)
        }
    }

    #[inline]
    fn emit_literal(&mut self, b: u8) {
        self.syms.push(b as u32);
        self.lit_freq[b as usize] += 1;
    }

    #[inline]
    fn emit_match(&mut self, len: usize, dist: usize) {
        self.syms.push(MATCH_FLAG | (len as u32) << 16 | dist as u32);
        self.lit_freq[257 + len_code(len)] += 1;
        self.dist_freq[dist_code(dist)] += 1;
    }

    /// Ends the current block when it is full; `next` is the first raw byte
    /// not yet covered by emitted symbols.
    #[inline]
    fn maybe_flush(&mut self, w: &mut BitWriter, next: usize) {
        if self.syms.len() >= BLOCK_SYMBOLS {
            self.flush_block(w, next, false);
        }
    }

    fn run(&mut self, w: &mut BitWriter) {
        let n = self.data.len();
        let mut pos = 0;
        if self.p.lazy {
            let mut prev_len = 0usize;
            let mut prev_dist = 0usize;
            let mut have_prev = false;
            while pos < n {
                let head = self.insert(pos);
                let (cur_len, cur_dist) = if prev_len < self.p.max_lazy.min(self.p.nice) {
                    self.longest_match(pos, head, prev_len)
                } else {
                    (0, 0)
                };
                if have_prev && prev_len >= MIN_MATCH && cur_len <= prev_len {
                    // Emit the previous match, which started at pos - 1.
                    self.emit_match(prev_len, prev_dist);
                    let end = pos - 1 + prev_len;
                    for p in pos + 1..end {
                        self.insert(p);
                    }
                    pos = end;
                    have_prev = false;
                    prev_len = 0;
                    self.maybe_flush(w, pos);
                } else {
                    if have_prev {
                        self.emit_literal(self.data[pos - 1]);
                        self.maybe_flush(w, pos);
                    }
                    have_prev = true;
                    prev_len = cur_len;
                    prev_dist = cur_dist;
                    pos += 1;
                }
            }
            if have_prev {
                self.emit_literal(self.data[n - 1]);
            }
        } else {
            while pos < n {
                let head = self.insert(pos);
                let (len, dist) = self.longest_match(pos, head, 0);
                if len >= MIN_MATCH {
                    self.emit_match(len, dist);
                    for p in pos + 1..pos + len {
                        self.insert(p);
                    }
                    pos += len;
                } else {
                    self.emit_literal(self.data[pos]);
                    pos += 1;
                }
                self.maybe_flush(w, pos);
            }
        }
        self.flush_block(w, n, true);
    }

    fn flush_block(&mut self, w: &mut BitWriter, end: usize, last: bool) {
        let raw = &self.data[self.block_start..end];
        self.lit_freq[256] = 1;
        let mut lit_lens = huffman_lengths(&self.lit_freq, 15);
        let mut dist_lens = huffman_lengths(&self.dist_freq, 15);
        ensure_two_codes(&mut lit_lens);
        ensure_two_codes(&mut dist_lens);
        let hlit = 257.max(lit_lens.iter().rposition(|&l| l != 0).map_or(0, |i| i + 1));
        let hdist = 1.max(dist_lens.iter().rposition(|&l| l != 0).map_or(0, |i| i + 1));

        // Run-length code the code lengths.
        let mut all = Vec::with_capacity(hlit + hdist);
        all.extend_from_slice(&lit_lens[..hlit]);
        all.extend_from_slice(&dist_lens[..hdist]);
        let rle = rle_lengths(&all);
        let mut cl_freq = [0u32; 19];
        for &(s, _) in &rle {
            cl_freq[s as usize] += 1;
        }
        let cl_lens = huffman_lengths(&cl_freq, 7);
        let hclen = 4.max(CL_ORDER.iter().rposition(|&i| cl_lens[i] != 0).map_or(0, |i| i + 1));

        // Cost of each alternative in bits.
        let extra_bits: u64 = (0..29)
            .map(|i| self.lit_freq[257 + i] as u64 * LENGTH_EXTRA[i] as u64)
            .chain((0..30).map(|i| self.dist_freq[i] as u64 * DIST_EXTRA[i] as u64))
            .sum();
        let sym_cost = |ll: &[u8], dl: &[u8]| -> u64 {
            let a: u64 = self.lit_freq.iter().zip(ll).map(|(&f, &l)| f as u64 * l as u64).sum();
            let b: u64 = self.dist_freq.iter().zip(dl).map(|(&f, &l)| f as u64 * l as u64).sum();
            a + b + extra_bits
        };
        let mut header_cost = 3 + 5 + 5 + 4 + 3 * hclen as u64;
        for &(s, _) in &rle {
            header_cost += cl_lens[s as usize] as u64
                + match s {
                    16 => 2,
                    17 => 3,
                    18 => 7,
                    _ => 0,
                };
        }
        let dyn_cost = header_cost + sym_cost(&lit_lens, &dist_lens);
        let (fixed_lit, fixed_dist) = fixed_lengths();
        let fixed_cost = 3 + sym_cost(&fixed_lit, &fixed_dist);
        let stored_cost = (raw.len() as u64 + 5 * (raw.len() as u64 / 65535 + 1)) * 8 + 7;

        if stored_cost <= dyn_cost.min(fixed_cost) {
            write_stored(w, raw, last);
        } else if fixed_cost <= dyn_cost {
            w.put(last as u32 | 1 << 1, 3);
            self.write_symbols(w, &fixed_lit, &fixed_dist);
        } else {
            w.put(last as u32 | 2 << 1, 3);
            w.put((hlit - 257) as u32, 5);
            w.put((hdist - 1) as u32, 5);
            w.put((hclen - 4) as u32, 4);
            for &i in CL_ORDER.iter().take(hclen) {
                w.put(cl_lens[i] as u32, 3);
            }
            let cl_codes = canonical_codes(&cl_lens);
            for &(s, extra) in &rle {
                w.put(cl_codes[s as usize], cl_lens[s as usize] as u32);
                match s {
                    16 => w.put(extra as u32, 2),
                    17 => w.put(extra as u32, 3),
                    18 => w.put(extra as u32, 7),
                    _ => {}
                }
            }
            self.write_symbols(w, &lit_lens, &dist_lens);
        }
        self.syms.clear();
        self.lit_freq = [0; 286];
        self.dist_freq = [0; 30];
        self.block_start = end;
    }

    fn write_symbols(&self, w: &mut BitWriter, lit_lens: &[u8], dist_lens: &[u8]) {
        let lc = canonical_codes(lit_lens);
        let dc = canonical_codes(dist_lens);
        for &s in &self.syms {
            if s & MATCH_FLAG == 0 {
                w.put(lc[s as usize], lit_lens[s as usize] as u32);
            } else {
                let len = ((s >> 16) & 0x1ff) as usize;
                let dist = (s & 0xffff) as usize;
                let l = len_code(len);
                w.put(lc[257 + l], lit_lens[257 + l] as u32);
                w.put((len - LENGTH_BASE[l] as usize) as u32, LENGTH_EXTRA[l] as u32);
                let d = dist_code(dist);
                w.put(dc[d], dist_lens[d] as u32);
                w.put((dist - DIST_BASE[d] as usize) as u32, DIST_EXTRA[d] as u32);
            }
        }
        w.put(lc[256], lit_lens[256] as u32);
    }
}

fn fixed_lengths() -> ([u8; 288], [u8; 30]) {
    let mut l = [8u8; 288];
    l[144..256].fill(9);
    l[256..280].fill(7);
    (l, [5; 30])
}

/// Gives at least two symbols a non-zero length so that every decoder
/// accepts the code (a single code would be incomplete).
fn ensure_two_codes(lens: &mut [u8]) {
    let mut used = lens.iter().filter(|&&l| l != 0).count();
    for l in lens.iter_mut() {
        if used >= 2 {
            break;
        }
        if *l == 0 {
            *l = 1;
            used += 1;
        }
    }
}

/// Run-length codes deflate code lengths into (symbol, extra) pairs.
fn rle_lengths(lens: &[u8]) -> Vec<(u8, u8)> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < lens.len() {
        let v = lens[i];
        let mut run = 1;
        while i + run < lens.len() && lens[i + run] == v {
            run += 1;
        }
        let mut r = run;
        if v == 0 {
            while r >= 11 {
                let k = r.min(138);
                out.push((18, (k - 11) as u8));
                r -= k;
            }
            if r >= 3 {
                out.push((17, (r - 3) as u8));
                r = 0;
            }
        } else {
            out.push((v, 0));
            r -= 1;
            while r >= 3 {
                let k = r.min(6);
                out.push((16, (k - 3) as u8));
                r -= k;
            }
        }
        for _ in 0..r {
            out.push((v, 0));
        }
        i += run;
    }
    out
}

/// Canonical Huffman codes, bit-reversed for LSB-first output.
pub(crate) fn canonical_codes(lens: &[u8]) -> Vec<u32> {
    let mut count = [0u32; 16];
    for &l in lens {
        count[l as usize] += 1;
    }
    count[0] = 0;
    let mut next = [0u32; 16];
    let mut code = 0;
    for len in 1..16 {
        code = (code + count[len - 1]) << 1;
        next[len] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                0
            } else {
                let c = next[l as usize];
                next[l as usize] += 1;
                (c << (32 - l as u32)).reverse_bits()
            }
        })
        .collect()
}

/// Optimal length-limited Huffman code lengths (package-merge). Symbols
/// with zero frequency get length 0; a single used symbol gets length 1.
pub(crate) fn huffman_lengths(freq: &[u32], max_bits: u32) -> Vec<u8> {
    let mut lens = vec![0u8; freq.len()];
    let mut leaves: Vec<(u64, i32)> =
        freq.iter().enumerate().filter(|(_, &f)| f > 0).map(|(i, &f)| (f as u64, i as i32)).collect();
    let n = leaves.len();
    if n == 0 {
        return lens;
    }
    if n == 1 {
        lens[leaves[0].1 as usize] = 1;
        return lens;
    }
    leaves.sort_unstable();
    let keep = 2 * n - 2;
    let mut lists: Vec<Vec<(u64, i32)>> = Vec::with_capacity(max_bits as usize);
    lists.push(leaves.clone());
    for _ in 1..max_bits {
        let prev = lists.last().map(|v| v.as_slice()).unwrap_or(&[]);
        let mut merged = Vec::with_capacity(keep);
        let mut packages = prev.chunks_exact(2).map(|p| (p[0].0 + p[1].0, -1)).peekable();
        let mut li = leaves.iter().peekable();
        while merged.len() < keep {
            let take_leaf = match (li.peek(), packages.peek()) {
                (Some(l), Some(p)) => l.0 <= p.0,
                (Some(_), None) => true,
                (None, Some(_)) => false,
                (None, None) => break,
            };
            if take_leaf {
                merged.push(*li.next().unwrap_or(&(0, -1)));
            } else if let Some(p) = packages.next() {
                merged.push(p);
            }
        }
        lists.push(merged);
    }
    let mut take = keep;
    for list in lists.iter().rev() {
        let mut packages = 0;
        for item in &list[..take.min(list.len())] {
            if item.1 >= 0 {
                lens[item.1 as usize] += 1;
            } else {
                packages += 1;
            }
        }
        take = 2 * packages;
    }
    lens
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_helpers() {
        for len in 3..=258 {
            let c = len_code(len);
            assert!(len >= LENGTH_BASE[c] as usize);
            assert!(len - (LENGTH_BASE[c] as usize) < 1 << LENGTH_EXTRA[c]);
            if len < 258 {
                assert_ne!(c, 28);
            }
        }
        for dist in 1..=32768 {
            let c = dist_code(dist);
            assert!(dist >= DIST_BASE[c] as usize);
            assert!(dist - (DIST_BASE[c] as usize) < 1 << DIST_EXTRA[c]);
        }
    }

    #[test]
    fn package_merge_limits_and_is_complete() {
        // Fibonacci frequencies produce very deep unrestricted trees.
        let mut f = vec![1u32, 1];
        for i in 2..30 {
            let v = f[i - 1] + f[i - 2];
            f.push(v);
        }
        let l = huffman_lengths(&f, 15);
        assert!(l.iter().all(|&x| (1..=15).contains(&x)));
        let kraft: f64 = l.iter().map(|&x| 0.5f64.powi(x as i32)).sum();
        assert!((kraft - 1.0).abs() < 1e-9);
    }

    #[test]
    fn round_trip_small() {
        for level in 0..=9 {
            for data in [&b""[..], b"a", b"abcabcabcabcabcabcabc", b"hello hello hello world"] {
                let z = zlib_compress(data, level);
                assert_eq!(crate::inflate::zlib_decompress(&z).unwrap(), data);
            }
        }
    }
}
