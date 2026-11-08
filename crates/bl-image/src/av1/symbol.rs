//! Symbol decoder (AV1 specification section 8.2): multi-symbol arithmetic
//! decoding with adaptive CDFs.
//!
//! The 15-bit SymbolValue of the specification is the top of a 64-bit
//! window `dif`; the bits below it are the following bits of the tile data,
//! inverted (the specification's renormalisation shifts in inverted data
//! bits, and ones after the end of the data).

pub(crate) struct SymbolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    /// SymbolValue in bits 48..63, then inverted data bits.
    dif: u64,
    /// Number of valid bits below the top 16 bits of `dif`.
    cnt: i32,
    rng: u32,
    adapt: bool,
}

impl<'a> SymbolDecoder<'a> {
    pub fn new(data: &'a [u8], disable_cdf_update: bool) -> Self {
        let mut d = SymbolDecoder {
            data,
            pos: 0,
            dif: 0,
            cnt: -15,
            rng: 0x8000,
            adapt: !disable_cdf_update,
        };
        // SymbolValue = (2^15 - 1) ^ first 15 bits: the window starts with a
        // zero bit followed by the inverted data.
        d.refill();
        d
    }

    /// Fills `dif` with inverted bytes so that at least 40 bits are valid
    /// below the SymbolValue.
    #[inline]
    fn refill(&mut self) {
        // Bits 0..=(47 - cnt) are free; byte k lands at shift 40 - cnt - 8*k.
        let mut c = self.cnt;
        let mut dif = self.dif;
        while c <= 40 {
            let b = if self.pos < self.data.len() {
                let v = self.data[self.pos];
                self.pos += 1;
                !v
            } else {
                0xff
            };
            // The first byte goes right below bit 63 - 1 (cnt = -15 means
            // the 15-bit value is still empty).
            let shift = 40 - c;
            dif |= (b as u64) << shift;
            c += 8;
        }
        self.dif = dif;
        self.cnt = c;
    }

    #[inline]
    fn renormalize(&mut self, rng: u32, dif: u64) {
        let d = 15 - (31 - rng.leading_zeros()) as i32;
        self.rng = rng << d;
        // The bits shifted into SymbolValue are the inverted data bits
        // waiting below it.
        self.dif = dif << d;
        self.cnt -= d;
        if self.cnt < 0 {
            self.refill();
        }
    }

    /// read_symbol( cdf ): `cdf` has N + 1 entries (the last is the
    /// adaptation counter).
    #[inline]
    pub fn symbol(&mut self, cdf: &mut [u16]) -> usize {
        let n = cdf.len() - 1;
        let value = (self.dif >> 48) as u32;
        let r8 = self.rng >> 8;
        let mut cur = self.rng;
        let mut prev;
        let mut symbol = 0;
        loop {
            prev = cur;
            let f = (32768 - cdf[symbol] as u32) >> 6;
            cur = ((r8 * f) >> 1) + 4 * (n - symbol - 1) as u32;
            if value >= cur {
                break;
            }
            symbol += 1;
        }
        let rng = prev - cur;
        let dif = self.dif - ((cur as u64) << 48);
        self.renormalize(rng, dif);
        if self.adapt {
            let count = cdf[n];
            let rate = 3 + (count > 15) as u32 + (count > 31) as u32 + (n as u32).ilog2().min(2);
            for (i, c) in cdf.iter_mut().enumerate().take(n - 1) {
                if i >= symbol {
                    *c += (32768 - *c) >> rate;
                } else {
                    *c -= *c >> rate;
                }
            }
            cdf[n] += (count < 32) as u16;
        }
        symbol
    }

    /// read_bool(): one equiprobable bit.
    #[inline]
    pub fn bool(&mut self) -> bool {
        let value = (self.dif >> 48) as u32;
        let cur = ((self.rng >> 8) << 7) + 4;
        // Symbol 0 (false) when value >= cur.
        let bit = value < cur;
        let (rng, dif) = if !bit {
            (self.rng - cur, self.dif - ((cur as u64) << 48))
        } else {
            (cur, self.dif)
        };
        self.renormalize(rng, dif);
        bit
    }

    /// L(n)
    pub fn literal(&mut self, n: u32) -> u32 {
        let mut x = 0;
        for _ in 0..n {
            x = 2 * x + self.bool() as u32;
        }
        x
    }

    /// NS(n)
    pub fn ns(&mut self, n: u32) -> u32 {
        if n <= 1 {
            return 0;
        }
        let w = 32 - n.leading_zeros();
        let m = (1u32 << w) - n;
        let v = self.literal(w - 1);
        if v < m {
            return v;
        }
        let extra = self.literal(1);
        (v << 1) - m + extra
    }
}
