//! Range encoder (RFC 6716 section 5.1), the counterpart of `range_dec`,
//! including the Laplace encoder used for CELT coarse energy.
//!
//! Ported from libopus `celt/entenc.c`, `celt/entcode.c` and
//! `celt/laplace.c`:
//!
//! Copyright (c) 2001-2011 Timothy B. Terriberry
//! Copyright (c) 2008-2009 Xiph.Org Foundation
//! Copyright (c) 2007 CSIRO, Copyright (c) 2007-2009 Xiph.Org Foundation
//! (laplace.c, written by Jean-Marc Valin)
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use crate::range_dec::{
    ec_ilog, ec_tell_frac, EC_CODE_BITS, EC_CODE_BOT, EC_CODE_TOP, EC_SYM_BITS, EC_SYM_MAX,
    EC_UINT_BITS, EC_WINDOW_SIZE, LAPLACE_MINP, LAPLACE_NMIN,
};

const EC_CODE_SHIFT: u32 = EC_CODE_BITS - EC_SYM_BITS - 1;

/// Range encoder writing into a fixed-size buffer: range coded symbols grow
/// from the front, raw bits from the back.
#[derive(Clone)]
pub struct RangeEncoder {
    buf: Vec<u8>,
    storage: u32,
    end_offs: u32,
    end_window: u32,
    nend_bits: i32,
    /// Total number of whole bits written (see `tell`).
    pub nbits_total: i32,
    offs: u32,
    pub rng: u32,
    val: u32,
    ext: u32,
    rem: i32,
    /// Set when the buffer overflowed (the output is then unusable).
    pub error: bool,
}

impl RangeEncoder {
    /// Creates an encoder for a packet of exactly `size` bytes.
    pub fn new(size: usize) -> Self {
        RangeEncoder {
            buf: vec![0; size],
            storage: size as u32,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: EC_CODE_BITS as i32 + 1,
            offs: 0,
            rng: EC_CODE_TOP,
            val: 0,
            ext: 0,
            rem: -1,
            error: false,
        }
    }

    /// Buffer size in bytes.
    pub fn storage(&self) -> u32 {
        self.storage
    }

    fn write_byte(&mut self, v: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.buf[self.offs as usize] = v as u8;
        self.offs += 1;
    }

    fn write_byte_at_end(&mut self, v: u32) {
        if self.offs + self.end_offs >= self.storage {
            self.error = true;
            return;
        }
        self.end_offs += 1;
        self.buf[(self.storage - self.end_offs) as usize] = v as u8;
    }

    /// Outputs a byte with carry propagation (`ec_enc_carry_out`).
    fn carry_out(&mut self, c: i32) {
        if c as u32 != EC_SYM_MAX {
            let carry = c >> EC_SYM_BITS;
            if self.rem >= 0 {
                self.write_byte((self.rem + carry) as u32);
            }
            if self.ext > 0 {
                let sym = (EC_SYM_MAX as i32 + carry) as u32 & EC_SYM_MAX;
                while self.ext > 0 {
                    self.write_byte(sym);
                    self.ext -= 1;
                }
            }
            self.rem = c & EC_SYM_MAX as i32;
        } else {
            self.ext += 1;
        }
    }

    #[inline]
    fn normalize(&mut self) {
        while self.rng <= EC_CODE_BOT {
            self.carry_out((self.val >> EC_CODE_SHIFT) as i32);
            self.val = (self.val << EC_SYM_BITS) & (EC_CODE_TOP - 1);
            self.rng <<= EC_SYM_BITS;
            self.nbits_total += EC_SYM_BITS as i32;
        }
    }

    /// `ec_encode`: codes the symbol range `[fl, fh)` out of `ft`.
    pub fn encode(&mut self, fl: u32, fh: u32, ft: u32) {
        let r = self.rng / ft;
        if fl > 0 {
            self.val = self
                .val
                .wrapping_add(self.rng.wrapping_sub(r.wrapping_mul(ft - fl)));
            self.rng = r.wrapping_mul(fh - fl);
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul(ft - fh));
        }
        self.normalize();
    }

    /// `ec_encode_bin`: like [`RangeEncoder::encode`] with `ft = 1 << bits`.
    pub fn encode_bin(&mut self, fl: u32, fh: u32, bits: u32) {
        let r = self.rng >> bits;
        if fl > 0 {
            self.val = self
                .val
                .wrapping_add(self.rng.wrapping_sub(r.wrapping_mul((1 << bits) - fl)));
            self.rng = r.wrapping_mul(fh - fl);
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul((1 << bits) - fh));
        }
        self.normalize();
    }

    /// Encodes a bit whose probability of being one is `1/(1<<logp)`.
    pub fn bit_logp(&mut self, val: bool, logp: u32) {
        let r = self.rng;
        let l = self.val;
        let s = r >> logp;
        let r = r - s;
        if val {
            self.val = l.wrapping_add(r);
        }
        self.rng = if val { s } else { r };
        self.normalize();
    }

    /// Encodes symbol `s` with an inverse CDF table of `ftb` bits precision.
    pub fn icdf(&mut self, s: usize, icdf: &[u8], ftb: u32) {
        let r = self.rng >> ftb;
        if s > 0 {
            self.val = self
                .val
                .wrapping_add(self.rng.wrapping_sub(r.wrapping_mul(icdf[s - 1] as u32)));
            self.rng = r.wrapping_mul((icdf[s - 1] - icdf[s]) as u32);
        } else {
            self.rng = self.rng.wrapping_sub(r.wrapping_mul(icdf[s] as u32));
        }
        self.normalize();
    }

    /// Encodes `fl`, uniformly distributed in `[0, ft)` (`ft > 1`).
    pub fn uint(&mut self, fl: u32, ft: u32) {
        debug_assert!(ft > 1 && fl < ft);
        let ft1 = ft - 1;
        let mut ftb = ec_ilog(ft1);
        if ftb > EC_UINT_BITS {
            ftb -= EC_UINT_BITS;
            let ftt = (ft1 >> ftb) + 1;
            let f = fl >> ftb;
            self.encode(f, f + 1, ftt);
            self.bits(fl & ((1u32 << ftb) - 1), ftb as u32);
        } else {
            self.encode(fl, fl + 1, ft1 + 1);
        }
    }

    /// Writes `bits` raw bits (at the end of the buffer).
    pub fn bits(&mut self, fl: u32, bits: u32) {
        debug_assert!(bits > 0 && bits <= 25);
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        if used + bits as i32 > EC_WINDOW_SIZE {
            loop {
                self.write_byte_at_end(window & EC_SYM_MAX);
                window >>= EC_SYM_BITS;
                used -= EC_SYM_BITS as i32;
                if used < EC_SYM_BITS as i32 {
                    break;
                }
            }
        }
        window |= fl << used;
        used += bits as i32;
        self.end_window = window;
        self.nend_bits = used;
        self.nbits_total += bits as i32;
    }

    /// Number of bits used so far (rounded up).
    #[inline]
    pub fn tell(&self) -> i32 {
        self.nbits_total - ec_ilog(self.rng)
    }

    /// Number of bits used so far in 1/8 bit units.
    #[inline]
    pub fn tell_frac(&self) -> u32 {
        ec_tell_frac(self.nbits_total, self.rng)
    }

    /// Shrinks the buffer to `size` bytes (`ec_enc_shrink`).
    pub fn shrink(&mut self, size: u32) {
        debug_assert!(self.offs + self.end_offs <= size);
        let eo = self.end_offs as usize;
        let old = self.storage as usize;
        self.buf.copy_within(old - eo..old, size as usize - eo);
        self.storage = size;
        self.buf.truncate(size as usize);
    }

    /// Laplace-distributed value (`ec_laplace_encode`). The value may be
    /// clamped when it falls outside the representable range; the coded
    /// value is returned.
    pub fn laplace(&mut self, value: i32, fs: u32, decay: i32) -> i32 {
        let mut val = value;
        let mut fl = 0u32;
        let mut fs = fs;
        if val != 0 {
            let s = -((val < 0) as i32);
            val = (val + s) ^ s;
            fl = fs;
            fs = laplace_get_freq1(fs, decay);
            let mut i = 1;
            while fs > 0 && i < val {
                fs *= 2;
                fl += fs + 2 * LAPLACE_MINP;
                fs = ((fs as i32 * decay) >> 15) as u32;
                i += 1;
            }
            if fs == 0 {
                let ndi_max = (32768 - fl + LAPLACE_MINP - 1) as i32;
                let ndi_max = (ndi_max - s) >> 1;
                let di = (val - i).min(ndi_max - 1);
                fl = (fl as i32 + (2 * di + 1 + s) * LAPLACE_MINP as i32) as u32;
                fs = LAPLACE_MINP.min(32768 - fl);
                val = (i + di + s) ^ s;
            } else {
                fs += LAPLACE_MINP;
                fl += fs & !(s as u32);
                val = value;
            }
        }
        self.encode_bin(fl, fl + fs, 15);
        val
    }

    /// Finishes the stream (`ec_enc_done`) and returns the packet bytes
    /// (exactly `storage` bytes long).
    pub fn done(mut self) -> Vec<u8> {
        let mut l = EC_CODE_BITS as i32 - ec_ilog(self.rng);
        let mut msk = (EC_CODE_TOP - 1) >> l;
        let mut end = self.val.wrapping_add(msk) & !msk;
        if (end | msk) >= self.val.wrapping_add(self.rng) {
            l += 1;
            msk >>= 1;
            end = self.val.wrapping_add(msk) & !msk;
        }
        while l > 0 {
            self.carry_out((end >> EC_CODE_SHIFT) as i32);
            end = (end << EC_SYM_BITS) & (EC_CODE_TOP - 1);
            l -= EC_SYM_BITS as i32;
        }
        if self.rem >= 0 || self.ext > 0 {
            self.carry_out(0);
        }
        let mut window = self.end_window;
        let mut used = self.nend_bits;
        while used >= EC_SYM_BITS as i32 {
            self.write_byte_at_end(window & EC_SYM_MAX);
            window >>= EC_SYM_BITS;
            used -= EC_SYM_BITS as i32;
        }
        if !self.error {
            let a = self.offs as usize;
            let b = (self.storage - self.end_offs) as usize;
            self.buf[a..b].fill(0);
            if used > 0 {
                if self.end_offs >= self.storage {
                    self.error = true;
                } else {
                    let l = -l;
                    if self.offs + self.end_offs >= self.storage && l < used {
                        window &= (1 << l) - 1;
                        self.error = true;
                    }
                    let idx = (self.storage - self.end_offs - 1) as usize;
                    self.buf[idx] |= window as u8;
                }
            }
        }
        self.buf
    }
}

fn laplace_get_freq1(fs0: u32, decay: i32) -> u32 {
    let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs0;
    ((ft as i32 * (16384 - decay)) >> 15) as u32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::range_dec::RangeDecoder;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u32 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (self.0 >> 33) as u32
        }
    }

    #[derive(Debug, Clone, Copy)]
    enum Sym {
        Encode(u32, u32),
        Bin(u32, u32),
        Logp(bool, u32),
        Icdf(usize),
        Uint(u32, u32),
        Bits(u32, u32),
        Laplace(i32, u32, i32),
    }

    const ICDF: [u8; 4] = [25, 23, 2, 0];

    fn random_syms(rng: &mut Lcg, n: usize) -> Vec<Sym> {
        (0..n)
            .map(|_| match rng.next() % 7 {
                0 => {
                    let ft = 2 + rng.next() % 1000;
                    Sym::Encode(rng.next() % ft, ft)
                }
                1 => {
                    let bits = 1 + rng.next() % 15;
                    Sym::Bin(rng.next() % (1 << bits), bits)
                }
                2 => Sym::Logp(rng.next() & 3 == 0, 1 + rng.next() % 15),
                3 => Sym::Icdf((rng.next() % 4) as usize),
                4 => {
                    let ft = 2 + rng.next() % (if rng.next() & 1 == 0 { 300 } else { 1 << 28 });
                    Sym::Uint(rng.next() % ft, ft)
                }
                5 => {
                    let bits = 1 + rng.next() % 25;
                    Sym::Bits(rng.next() & ((1 << bits) - 1), bits)
                }
                _ => {
                    let v = (rng.next() % 41) as i32 - 20;
                    let fs = (40 + rng.next() % 150) << 7;
                    let decay = ((10 + rng.next() % 170) << 6) as i32;
                    Sym::Laplace(v, fs, decay)
                }
            })
            .collect()
    }

    /// Everything the encoder writes must come back from `RangeDecoder`,
    /// with identical `tell`/`tell_frac` and final range after each symbol.
    #[test]
    fn round_trip_against_decoder() {
        let mut rng = Lcg(42);
        for iter in 0..400 {
            let syms = random_syms(&mut rng, 1 + (iter % 60));
            let mut enc = RangeEncoder::new(1275);
            let mut coded = Vec::new();
            let mut tells = Vec::new();
            for &s in &syms {
                let c = match s {
                    Sym::Encode(f, ft) => {
                        enc.encode(f, f + 1, ft);
                        s
                    }
                    Sym::Bin(f, bits) => {
                        enc.encode_bin(f, f + 1, bits);
                        s
                    }
                    Sym::Logp(b, l) => {
                        enc.bit_logp(b, l);
                        s
                    }
                    Sym::Icdf(v) => {
                        enc.icdf(v, &ICDF, 5);
                        s
                    }
                    Sym::Uint(v, ft) => {
                        enc.uint(v, ft);
                        s
                    }
                    Sym::Bits(v, b) => {
                        enc.bits(v, b);
                        s
                    }
                    Sym::Laplace(v, fs, d) => Sym::Laplace(enc.laplace(v, fs, d), fs, d),
                };
                coded.push(c);
                tells.push((enc.tell(), enc.tell_frac()));
            }
            let rng_final = enc.rng;
            let buf = enc.done();
            let mut dec = RangeDecoder::new(&buf);
            for (i, s) in coded.iter().enumerate() {
                match *s {
                    Sym::Encode(f, ft) => {
                        let d = dec.decode(ft);
                        assert_eq!(d, f);
                        dec.update(f, f + 1, ft);
                    }
                    Sym::Bin(f, bits) => {
                        let d = dec.decode_bin(bits);
                        assert_eq!(d, f);
                        dec.update(f, f + 1, 1 << bits);
                    }
                    Sym::Logp(b, l) => assert_eq!(dec.bit_logp(l), b),
                    Sym::Icdf(v) => assert_eq!(dec.icdf(&ICDF, 5), v),
                    Sym::Uint(v, ft) => assert_eq!(dec.uint(ft), v),
                    Sym::Bits(v, b) => assert_eq!(dec.bits(b), v),
                    Sym::Laplace(v, fs, d) => assert_eq!(dec.laplace(fs, d), v),
                }
                assert_eq!(
                    (dec.tell(), dec.tell_frac()),
                    tells[i],
                    "iter {iter} sym {i}"
                );
            }
            assert_eq!(dec.rng, rng_final);
            assert!(!dec.error);
        }
    }

    #[test]
    fn laplace_extremes_are_clamped_consistently() {
        for &v in &[-200i32, -40, -17, 0, 17, 40, 200] {
            let mut enc = RangeEncoder::new(16);
            let coded = enc.laplace(v, 72 << 7, 127 << 6);
            let buf = enc.done();
            let mut dec = RangeDecoder::new(&buf);
            assert_eq!(dec.laplace(72 << 7, 127 << 6), coded);
            assert!(coded.signum() == v.signum());
        }
    }

    #[test]
    fn small_buffer_fills_exactly() {
        // A full buffer of raw bits and range coded bits must fit exactly.
        let mut enc = RangeEncoder::new(4);
        enc.bits(0x5a5, 12);
        enc.bit_logp(true, 1);
        let buf = enc.clone().done();
        assert_eq!(buf.len(), 4);
        let mut dec = RangeDecoder::new(&buf);
        assert!(dec.bit_logp(1));
        assert_eq!(dec.bits(12), 0x5a5);
    }
}
