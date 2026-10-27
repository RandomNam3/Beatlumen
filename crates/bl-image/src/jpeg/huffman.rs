//! JPEG entropy decoding primitives: an MSB-first bit reader that removes
//! byte stuffing and stops at markers, and Huffman tables with a 9-bit fast
//! lookup (plus a combined run/size/value table for AC coefficients).

use crate::Error;

pub const FAST_BITS: u32 = 9;

pub struct HuffTable {
    /// (length << 8) | symbol for codes of up to FAST_BITS bits, 0 = slow path.
    fast: [u16; 1 << FAST_BITS],
    /// For AC tables: value << 16 | run << 4 | total length (code + magnitude
    /// bits) when both fit into FAST_BITS bits and the size is non-zero.
    pub fast_ac: Vec<i32>,
    /// Largest code of each length (-1 if none), index 1..=16.
    maxcode: [i32; 18],
    /// Symbol index offset for each length.
    delta: [i32; 17],
    symbols: [u8; 256],
}

impl HuffTable {
    /// Builds a table from the DHT `counts` (codes per length 1..=16) and
    /// symbols.
    pub fn new(counts: &[u8; 16], symbols: &[u8], is_ac: bool) -> Result<HuffTable, Error> {
        let mut t = HuffTable {
            fast: [0; 1 << FAST_BITS],
            fast_ac: Vec::new(),
            maxcode: [-1; 18],
            delta: [0; 17],
            symbols: [0; 256],
        };
        let n = symbols.len().min(256);
        t.symbols[..n].copy_from_slice(&symbols[..n]);
        let mut code = 0u32;
        let mut k = 0usize;
        for len in 1..=16usize {
            let cnt = counts[len - 1] as usize;
            t.delta[len] = k as i32 - code as i32;
            for _ in 0..cnt {
                if k >= n {
                    return Err(Error::InvalidData("jpeg: bad huffman table"));
                }
                if len as u32 <= FAST_BITS {
                    let shift = FAST_BITS - len as u32;
                    let first = (code << shift) as usize;
                    let last = ((code + 1) << shift) as usize;
                    for e in t.fast.iter_mut().take(last.min(1 << FAST_BITS)).skip(first) {
                        *e = ((len as u16) << 8) | t.symbols[k] as u16;
                    }
                }
                code += 1;
                k += 1;
            }
            if cnt > 0 {
                t.maxcode[len] = code as i32 - 1;
            }
            if code > 1 << len {
                return Err(Error::InvalidData("jpeg: over-subscribed huffman table"));
            }
            code <<= 1;
        }
        t.maxcode[17] = i32::MAX;
        if is_ac {
            t.fast_ac = vec![0; 1 << FAST_BITS];
            for (peek, f) in t.fast_ac.iter_mut().enumerate() {
                let e = t.fast[peek];
                if e == 0 {
                    continue;
                }
                let len = (e >> 8) as u32;
                let rs = (e & 0xff) as u32;
                let (run, size) = (rs >> 4, rs & 15);
                if size != 0 && len + size <= FAST_BITS {
                    let bits = ((peek as u32) >> (FAST_BITS - len - size)) & ((1 << size) - 1);
                    let v = extend(bits as i32, size);
                    *f = (v << 16) | (run << 4) as i32 | (len + size) as i32;
                }
            }
        }
        Ok(t)
    }
}

/// Sign-extends a `size`-bit magnitude category value (JPEG EXTEND).
#[inline(always)]
pub fn extend(v: i32, size: u32) -> i32 {
    if size == 0 {
        0
    } else if v < 1 << (size - 1) {
        v - (1 << size) + 1
    } else {
        v
    }
}

/// MSB-first bit reader over entropy-coded data.
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
    /// Next bit at bit 63.
    bits: u64,
    cnt: u32,
    /// A marker (or the end of data) was reached; zeros are supplied.
    pub marker_hit: bool,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8], pos: usize) -> Self {
        BitReader { data, pos, bits: 0, cnt: 0, marker_hit: false }
    }

    #[inline(always)]
    pub fn refill(&mut self) {
        if self.cnt > 56 {
            return;
        }
        if let Some(b) = self.data.get(self.pos..self.pos + 8) {
            let w = u64::from_be_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]]);
            // No 0xFF byte among the next eight: append them directly.
            let x = !w;
            if (x.wrapping_sub(0x0101_0101_0101_0101) & !x & 0x8080_8080_8080_8080) == 0 {
                self.bits |= w >> self.cnt;
                self.pos += ((63 - self.cnt) >> 3) as usize;
                self.cnt |= 56;
                return;
            }
        }
        self.refill_slow();
    }

    #[cold]
    fn refill_slow(&mut self) {
        while self.cnt <= 56 {
            let mut b = 0u8;
            if !self.marker_hit {
                match self.data.get(self.pos) {
                    Some(&0xff) => match self.data.get(self.pos + 1) {
                        Some(0) => {
                            b = 0xff;
                            self.pos += 2;
                        }
                        _ => self.marker_hit = true,
                    },
                    Some(&v) => {
                        b = v;
                        self.pos += 1;
                    }
                    None => self.marker_hit = true,
                }
            }
            self.bits |= (b as u64) << (56 - self.cnt);
            self.cnt += 8;
        }
    }

    #[inline(always)]
    pub fn peek(&self, n: u32) -> u32 {
        (self.bits >> (64 - n)) as u32
    }

    #[inline(always)]
    pub fn consume(&mut self, n: u32) {
        self.bits <<= n;
        self.cnt -= n;
    }

    /// Reads `n` (<= 16) bits; requires `cnt >= n` (ensured by refills before
    /// each symbol, since symbol + magnitude bits <= 32).
    #[inline(always)]
    pub fn get(&mut self, n: u32) -> i32 {
        if n == 0 {
            return 0;
        }
        if self.cnt < n {
            self.refill();
        }
        let v = self.peek(n) as i32;
        self.consume(n);
        v
    }

    #[inline(always)]
    pub fn bit(&mut self) -> bool {
        if self.cnt < 1 {
            self.refill();
        }
        let v = self.bits >> 63 != 0;
        self.consume(1);
        v
    }

    /// Reads a magnitude category value and sign-extends it.
    #[inline(always)]
    pub fn receive_extend(&mut self, size: u32) -> i32 {
        let v = self.get(size);
        extend(v, size)
    }

    /// Decodes one Huffman symbol.
    #[inline(always)]
    pub fn decode(&mut self, t: &HuffTable) -> Result<u8, Error> {
        if self.cnt < 16 {
            self.refill();
        }
        let e = t.fast[self.peek(FAST_BITS) as usize];
        if e != 0 {
            self.consume((e >> 8) as u32);
            return Ok(e as u8);
        }
        self.decode_slow(t)
    }

    #[cold]
    fn decode_slow(&mut self, t: &HuffTable) -> Result<u8, Error> {
        let code16 = self.peek(16) as i32;
        for len in FAST_BITS as usize + 1..=16 {
            let c = code16 >> (16 - len);
            if c <= t.maxcode[len] {
                self.consume(len as u32);
                let idx = (c + t.delta[len]) as usize;
                return t.symbols.get(idx).copied().ok_or(Error::InvalidData("jpeg: bad huffman code"));
            }
        }
        Err(Error::InvalidData("jpeg: bad huffman code"))
    }

    /// Number of buffered bits.
    #[inline(always)]
    pub fn available(&self) -> u32 {
        self.cnt
    }

    /// Skips to the next restart marker. Returns false when a different
    /// marker (or the end of data) is found instead.
    pub fn restart(&mut self) -> bool {
        self.bits = 0;
        self.cnt = 0;
        self.marker_hit = false;
        // Find the next marker.
        let d = self.data;
        let mut p = self.pos;
        loop {
            while p < d.len() && d[p] != 0xff {
                p += 1;
            }
            while p + 1 < d.len() && d[p + 1] == 0xff {
                p += 1;
            }
            if p + 1 >= d.len() {
                self.pos = d.len();
                self.marker_hit = true;
                return false;
            }
            let m = d[p + 1];
            if m == 0 {
                p += 2;
                continue;
            }
            if (0xd0..=0xd7).contains(&m) {
                self.pos = p + 2;
                return true;
            }
            self.pos = p;
            self.marker_hit = true;
            return false;
        }
    }

    /// Byte position from which to search for the next marker.
    pub fn position(&self) -> usize {
        self.pos
    }
}
