//! Range decoder (RFC 6716 section 4.1), including the Laplace decoder used
//! for CELT coarse energy.
//!
//! Ported from libopus `celt/entdec.c`, `celt/entcode.c` and
//! `celt/laplace.c`:
//!
//! Copyright (c) 2001-2011 Timothy B. Terriberry
//! Copyright (c) 2008-2009 Xiph.Org Foundation
//! Copyright (c) 2007 CSIRO, Copyright (c) 2007-2009 Xiph.Org Foundation
//! (laplace.c, written by Jean-Marc Valin)
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are met:
//! - Redistributions of source code must retain the above copyright notice,
//!   this list of conditions and the following disclaimer.
//! - Redistributions in binary form must reproduce the above copyright
//!   notice, this list of conditions and the following disclaimer in the
//!   documentation and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! AND ANY EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED (see NOTICE for the
//! full license text).

const EC_SYM_BITS: u32 = 8;
const EC_CODE_BITS: u32 = 32;
const EC_SYM_MAX: u32 = (1 << EC_SYM_BITS) - 1;
const EC_CODE_TOP: u32 = 1 << (EC_CODE_BITS - 1);
const EC_CODE_BOT: u32 = EC_CODE_TOP >> EC_SYM_BITS;
const EC_CODE_EXTRA: u32 = (EC_CODE_BITS - 2) % EC_SYM_BITS + 1;
const EC_WINDOW_SIZE: i32 = 32;
const EC_UINT_BITS: i32 = 8;

/// Resolution of fractional bit counts (1/8 bit).
pub const BITRES: i32 = 3;

/// `EC_ILOG`: number of bits needed to represent `x` (0 for 0).
#[inline(always)]
pub fn ec_ilog(x: u32) -> i32 {
    32 - x.leading_zeros() as i32
}

/// Range decoder state over a borrowed packet buffer.
#[derive(Clone)]
pub struct RangeDecoder<'a> {
    buf: &'a [u8],
    /// Number of usable bytes (may be reduced, e.g. for redundancy frames).
    pub storage: u32,
    end_offs: u32,
    end_window: u32,
    nend_bits: i32,
    /// Total number of whole bits read (see `tell`).
    pub nbits_total: i32,
    offs: u32,
    pub rng: u32,
    val: u32,
    ext: u32,
    rem: i32,
    pub error: bool,
}

impl<'a> RangeDecoder<'a> {
    pub fn new(buf: &'a [u8]) -> Self {
        let storage = buf.len().min(u32::MAX as usize) as u32;
        let mut d = RangeDecoder {
            buf,
            storage,
            end_offs: 0,
            end_window: 0,
            nend_bits: 0,
            nbits_total: (EC_CODE_BITS + 1
                - ((EC_CODE_BITS - EC_CODE_EXTRA) / EC_SYM_BITS) * EC_SYM_BITS)
                as i32,
            offs: 0,
            rng: 1 << EC_CODE_EXTRA,
            val: 0,
            ext: 0,
            rem: 0,
            error: false,
        };
        d.rem = d.read_byte();
        d.val = d.rng - 1 - ((d.rem as u32) >> (EC_SYM_BITS - EC_CODE_EXTRA));
        d.normalize();
        d
    }

    #[inline(always)]
    fn read_byte(&mut self) -> i32 {
        if self.offs < self.storage {
            let b = self.buf.get(self.offs as usize).copied().unwrap_or(0);
            self.offs += 1;
            b as i32
        } else {
            0
        }
    }

    #[inline(always)]
    fn read_byte_from_end(&mut self) -> u32 {
        if self.end_offs < self.storage {
            self.end_offs += 1;
            self.buf
                .get((self.storage - self.end_offs) as usize)
                .copied()
                .unwrap_or(0) as u32
        } else {
            0
        }
    }

    #[inline(always)]
    fn normalize(&mut self) {
        while self.rng <= EC_CODE_BOT {
            self.nbits_total += EC_SYM_BITS as i32;
            self.rng <<= EC_SYM_BITS;
            let mut sym = self.rem;
            self.rem = self.read_byte();
            sym = ((sym << EC_SYM_BITS) | self.rem) >> (EC_SYM_BITS - EC_CODE_EXTRA);
            self.val = ((self.val << EC_SYM_BITS).wrapping_add(EC_SYM_MAX & !(sym as u32)))
                & (EC_CODE_TOP - 1);
        }
    }

    /// `ec_decode`: returns the cumulative frequency for a total of `ft`.
    #[inline]
    pub fn decode(&mut self, ft: u32) -> u32 {
        self.ext = self.rng / ft.max(1);
        let s = self.val / self.ext.max(1);
        ft - (s + 1).min(ft)
    }

    #[inline]
    pub fn decode_bin(&mut self, bits: u32) -> u32 {
        self.ext = self.rng >> bits;
        let s = self.val / self.ext.max(1);
        (1u32 << bits) - (s + 1).min(1u32 << bits)
    }

    #[inline]
    pub fn update(&mut self, fl: u32, fh: u32, ft: u32) {
        let s = self.ext.wrapping_mul(ft.wrapping_sub(fh));
        self.val = self.val.wrapping_sub(s);
        self.rng = if fl > 0 {
            self.ext.wrapping_mul(fh.wrapping_sub(fl))
        } else {
            self.rng.wrapping_sub(s)
        };
        self.normalize();
    }

    /// Decodes a bit with probability of a one being `1/(1<<logp)`.
    #[inline]
    pub fn bit_logp(&mut self, logp: u32) -> bool {
        let r = self.rng;
        let d = self.val;
        let s = r >> logp;
        let ret = d < s;
        if !ret {
            self.val = d - s;
        }
        self.rng = if ret { s } else { r - s };
        self.normalize();
        ret
    }

    /// Decodes a symbol using an inverse CDF table with `ftb` bits of precision.
    #[inline]
    pub fn icdf(&mut self, icdf: &[u8], ftb: u32) -> usize {
        let mut s = self.rng;
        let d = self.val;
        let r = s >> ftb;
        let mut ret = 0usize;
        let mut t;
        loop {
            t = s;
            s = r.wrapping_mul(icdf.get(ret).copied().unwrap_or(0) as u32);
            if d >= s {
                break;
            }
            ret += 1;
        }
        self.val = d - s;
        self.rng = t - s;
        self.normalize();
        ret
    }

    /// Decodes a uniformly distributed integer in `[0, ft)`.
    pub fn uint(&mut self, ft: u32) -> u32 {
        if ft <= 1 {
            return 0;
        }
        let ft1 = ft - 1;
        let mut ftb = ec_ilog(ft1);
        if ftb > EC_UINT_BITS {
            ftb -= EC_UINT_BITS;
            let ftt = (ft1 >> ftb) + 1;
            let s = self.decode(ftt);
            self.update(s, s + 1, ftt);
            let t = (s << ftb) | self.bits(ftb as u32);
            if t <= ft1 {
                return t;
            }
            self.error = true;
            ft1
        } else {
            let s = self.decode(ft);
            self.update(s, s + 1, ft);
            s
        }
    }

    /// Reads `bits` raw bits from the end of the buffer.
    pub fn bits(&mut self, bits: u32) -> u32 {
        let mut window = self.end_window;
        let mut available = self.nend_bits;
        if (available as u32) < bits {
            loop {
                window |= self.read_byte_from_end() << available;
                available += EC_SYM_BITS as i32;
                if available > EC_WINDOW_SIZE - EC_SYM_BITS as i32 {
                    break;
                }
            }
        }
        let ret = if bits >= 32 {
            window
        } else {
            window & ((1u32 << bits) - 1)
        };
        window = if bits >= 32 { 0 } else { window >> bits };
        available -= bits as i32;
        self.end_window = window;
        self.nend_bits = available;
        self.nbits_total += bits as i32;
        ret
    }

    /// Number of bits used so far (rounded up).
    #[inline]
    pub fn tell(&self) -> i32 {
        self.nbits_total - ec_ilog(self.rng)
    }

    /// Number of bits used so far in 1/8 bit units.
    pub fn tell_frac(&self) -> u32 {
        const CORRECTION: [u32; 8] = [35733, 38967, 42495, 46340, 50535, 55109, 60097, 65535];
        let nbits = (self.nbits_total as u32).wrapping_shl(BITRES as u32);
        let mut l = ec_ilog(self.rng);
        let r = self.rng >> (l - 16).max(0);
        let mut b = ((r >> 12) as i32 - 8).clamp(0, 7) as u32;
        b += (r > CORRECTION[b as usize]) as u32;
        l = (l << 3) + b as i32;
        nbits.wrapping_sub(l as u32)
    }

    /// Laplace-distributed value (CELT coarse energy).
    pub fn laplace(&mut self, fs: u32, decay: i32) -> i32 {
        const LAPLACE_MINP: u32 = 1;
        const LAPLACE_NMIN: u32 = 16;
        let mut val = 0i32;
        let fm = self.decode_bin(15);
        let mut fl = 0u32;
        let mut fs = fs;
        if fm >= fs {
            val += 1;
            fl = fs;
            let ft = 32768 - LAPLACE_MINP * (2 * LAPLACE_NMIN) - fs;
            fs = ((ft as i32 * (16384 - decay)) >> 15) as u32 + LAPLACE_MINP;
            while fs > LAPLACE_MINP && fm >= fl + 2 * fs {
                fs *= 2;
                fl += fs;
                fs = (((fs - 2 * LAPLACE_MINP) as i32 * decay) >> 15) as u32;
                fs += LAPLACE_MINP;
                val += 1;
            }
            if fs <= LAPLACE_MINP {
                let di = (fm - fl) >> 1;
                val += di as i32;
                fl += 2 * di * LAPLACE_MINP;
            }
            if fm < fl + fs {
                val = -val;
            } else {
                fl += fs;
            }
        }
        let fh = (fl + fs).min(32768);
        // Malformed input could put fl past the end; clamp so update() stays sane.
        let fl = fl.min(fh);
        self.update(fl, fh, 32768);
        val
    }

    /// Raw byte offset of the range coder (bytes consumed from the front).
    #[allow(dead_code)]
    pub fn range_bytes(&self) -> u32 {
        self.offs
    }
}
