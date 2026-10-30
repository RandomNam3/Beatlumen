//! CELT bit allocation, shared by the decoder and the encoder. The only
//! encoder-side choices (band skipping, intensity and dual stereo) go
//! through the [`AllocCoder`] trait.
//!
//! Ported from libopus `celt/rate.c` and `celt/rate.h`:
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2009 Xiph.Org Foundation
//! Written by Jean-Marc Valin
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::mode::*;
use crate::range_dec::{RangeDecoder, BITRES};
use crate::range_enc::RangeEncoder;

const ALLOC_STEPS: i32 = 6;

#[inline]
pub fn get_pulses(i: i32) -> i32 {
    if i < 8 {
        i
    } else {
        (8 + (i & 7)) << ((i >> 3) - 1)
    }
}

pub fn bits2pulses(band: usize, lm: i32, bits: i32) -> i32 {
    let cache = cache_bits((lm + 1) as usize, band);
    let mut lo = 0i32;
    let mut hi = cache[0] as i32;
    let bits = bits - 1;
    for _ in 0..LOG_MAX_PSEUDO {
        let mid = (lo + hi + 1) >> 1;
        if cache.get(mid as usize).copied().unwrap_or(255) as i32 >= bits {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let clo = if lo == 0 {
        -1
    } else {
        cache.get(lo as usize).copied().unwrap_or(0) as i32
    };
    let chi = cache.get(hi as usize).copied().unwrap_or(0) as i32;
    if bits - clo <= chi - bits {
        lo
    } else {
        hi
    }
}

pub fn pulses2bits(band: usize, lm: i32, pulses: i32) -> i32 {
    if pulses == 0 {
        0
    } else {
        let cache = cache_bits((lm + 1) as usize, band);
        cache.get(pulses as usize).copied().unwrap_or(0) as i32 + 1
    }
}

/// Codes the side decisions of the allocation: the decoder reads them, the
/// encoder chooses and writes them.
pub trait AllocCoder {
    /// Whether to stop skipping at band `j` (the last of `coded_bands`).
    #[allow(clippy::too_many_arguments)]
    fn keep_band(
        &mut self,
        j: usize,
        start: usize,
        coded_bands: usize,
        band_bits: i32,
        band_width: i32,
        lm: i32,
    ) -> bool;
    /// First intensity-coded band, in `[start, coded_bands]`.
    fn intensity(&mut self, start: usize, coded_bands: usize) -> usize;
    fn dual_stereo(&mut self) -> bool;
}

impl AllocCoder for RangeDecoder<'_> {
    fn keep_band(&mut self, _: usize, _: usize, _: usize, _: i32, _: i32, _: i32) -> bool {
        self.bit_logp(1)
    }
    fn intensity(&mut self, start: usize, coded_bands: usize) -> usize {
        start + self.uint((coded_bands + 1 - start) as u32) as usize
    }
    fn dual_stereo(&mut self) -> bool {
        self.bit_logp(1)
    }
}

/// Encoder side of [`AllocCoder`] (decisions of libopus' `interp_bits2pulses`).
pub struct AllocEncoder<'a> {
    pub enc: &'a mut RangeEncoder,
    /// Requested intensity band (lowered to the number of coded bands).
    pub intensity: usize,
    pub dual_stereo: bool,
    /// Coded bands of the previous frame (hysteresis), 0 for none.
    pub prev: usize,
    /// Last band with significant signal.
    pub signal_bandwidth: usize,
}

impl AllocCoder for AllocEncoder<'_> {
    fn keep_band(
        &mut self,
        j: usize,
        start: usize,
        coded_bands: usize,
        band_bits: i32,
        band_width: i32,
        lm: i32,
    ) -> bool {
        // Hysteresis keeps bands from fluctuating in and out, but we try not
        // to fold below a certain point.
        let depth_threshold = if coded_bands > 17 {
            if j < self.prev {
                7
            } else {
                9
            }
        } else {
            0
        };
        let keep = coded_bands <= start + 2
            || (band_bits > (depth_threshold * band_width << lm << BITRES) >> 4
                && j <= self.signal_bandwidth);
        self.enc.bit_logp(keep, 1);
        keep
    }
    fn intensity(&mut self, start: usize, coded_bands: usize) -> usize {
        self.intensity = self.intensity.clamp(start, coded_bands);
        self.enc.uint(
            (self.intensity - start) as u32,
            (coded_bands + 1 - start) as u32,
        );
        self.intensity
    }
    fn dual_stereo(&mut self) -> bool {
        self.enc.bit_logp(self.dual_stereo, 1);
        self.dual_stereo
    }
}

/// Output of [`compute_allocation`].
pub struct Allocation {
    pub coded_bands: usize,
    pub intensity: usize,
    pub dual_stereo: bool,
    pub balance: i32,
    pub pulses: [i32; NB_EBANDS],
    pub fine_quant: [i32; NB_EBANDS],
    pub fine_priority: [i32; NB_EBANDS],
}

#[allow(clippy::too_many_arguments)]
fn interp_bits2pulses(
    start: usize,
    end: usize,
    skip_start: usize,
    bits1: &[i32; NB_EBANDS],
    bits2: &[i32; NB_EBANDS],
    thresh: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    mut total: i32,
    skip_rsv: i32,
    mut intensity_rsv: i32,
    mut dual_stereo_rsv: i32,
    c: i32,
    lm: i32,
    coder: &mut impl AllocCoder,
    out: &mut Allocation,
) {
    let alloc_floor = c << BITRES;
    let stereo = (c > 1) as i32;
    let log_m = lm << BITRES;
    let mut lo = 0i32;
    let mut hi = 1i32 << ALLOC_STEPS;
    let bits = &mut out.pulses;
    let ebits = &mut out.fine_quant;
    let fine_priority = &mut out.fine_priority;
    for _ in 0..ALLOC_STEPS {
        let mid = (lo + hi) >> 1;
        let mut psum = 0i32;
        let mut done = false;
        for j in (start..end).rev() {
            let tmp = bits1[j] + (mid * bits2[j] >> ALLOC_STEPS);
            if tmp >= thresh[j] || done {
                done = true;
                psum += tmp.min(cap[j]);
            } else if tmp >= alloc_floor {
                psum += alloc_floor;
            }
        }
        if psum > total {
            hi = mid;
        } else {
            lo = mid;
        }
    }
    let mut psum = 0i32;
    let mut done = false;
    for j in (start..end).rev() {
        let mut tmp = bits1[j] + (lo * bits2[j] >> ALLOC_STEPS);
        if tmp < thresh[j] && !done {
            tmp = if tmp >= alloc_floor { alloc_floor } else { 0 };
        } else {
            done = true;
        }
        tmp = tmp.min(cap[j]);
        bits[j] = tmp;
        psum += tmp;
    }

    let mut coded_bands = end;
    loop {
        let j = coded_bands - 1;
        if j <= skip_start {
            total += skip_rsv;
            break;
        }
        let mut left = total.wrapping_sub(psum);
        let span = (EBANDS[coded_bands] - EBANDS[start]) as i32;
        let percoeff = (left as u32 / span as u32) as i32;
        left = left.wrapping_sub(span.wrapping_mul(percoeff));
        let rem = (left - (EBANDS[j] - EBANDS[start]) as i32).max(0);
        let band_width = (EBANDS[coded_bands] - EBANDS[j]) as i32;
        let mut band_bits = bits[j]
            .wrapping_add(percoeff.wrapping_mul(band_width))
            .wrapping_add(rem);
        if band_bits >= thresh[j].max(alloc_floor + (1 << BITRES)) {
            if coder.keep_band(j, start, coded_bands, band_bits, band_width, lm) {
                break;
            }
            psum += 1 << BITRES;
            band_bits -= 1 << BITRES;
        }
        psum -= bits[j] + intensity_rsv;
        if intensity_rsv > 0 {
            intensity_rsv = LOG2_FRAC_TABLE[j - start] as i32;
        }
        psum += intensity_rsv;
        if band_bits >= alloc_floor {
            psum += alloc_floor;
            bits[j] = alloc_floor;
        } else {
            bits[j] = 0;
        }
        coded_bands -= 1;
    }

    if intensity_rsv > 0 {
        out.intensity = coder.intensity(start, coded_bands);
    } else {
        out.intensity = 0;
    }
    if out.intensity <= start {
        total += dual_stereo_rsv;
        dual_stereo_rsv = 0;
    }
    out.dual_stereo = if dual_stereo_rsv > 0 {
        coder.dual_stereo()
    } else {
        false
    };

    let mut left = total.wrapping_sub(psum);
    let span = (EBANDS[coded_bands] - EBANDS[start]) as i32;
    let percoeff = (left as u32 / span as u32) as i32;
    left = left.wrapping_sub(span.wrapping_mul(percoeff));
    for j in start..coded_bands {
        bits[j] = bits[j].wrapping_add(percoeff.wrapping_mul((EBANDS[j + 1] - EBANDS[j]) as i32));
    }
    for j in start..coded_bands {
        let tmp = left.min((EBANDS[j + 1] - EBANDS[j]) as i32);
        bits[j] += tmp;
        left -= tmp;
    }

    let mut balance = 0i32;
    let mut j = start;
    while j < coded_bands {
        let n0 = (EBANDS[j + 1] - EBANDS[j]) as i32;
        let n = n0 << lm;
        let bit = bits[j] + balance;
        let mut excess;
        if n > 1 {
            excess = (bit - cap[j]).max(0);
            bits[j] = bit - excess;
            let den = c * n
                + if c == 2 && n > 2 && !out.dual_stereo && j < out.intensity {
                    1
                } else {
                    0
                };
            let nclogn = den * (LOGN400[j] as i32 + log_m);
            let mut offset = (nclogn >> 1) - den * FINE_OFFSET;
            if n == 2 {
                offset += den << BITRES >> 2;
            }
            if bits[j] + offset < den * 2 << BITRES {
                offset += nclogn >> 2;
            } else if bits[j] + offset < den * 3 << BITRES {
                offset += nclogn >> 3;
            }
            ebits[j] = (bits[j] + offset + (den << (BITRES - 1))).max(0);
            ebits[j] = ((ebits[j] as u32 / den as u32) >> BITRES) as i32;
            if c * ebits[j] > (bits[j] >> BITRES) {
                ebits[j] = bits[j] >> stereo >> BITRES;
            }
            ebits[j] = ebits[j].min(MAX_FINE_BITS);
            fine_priority[j] = (ebits[j] * (den << BITRES) >= bits[j] + offset) as i32;
            bits[j] -= c * ebits[j] << BITRES;
        } else {
            excess = (bit - (c << BITRES)).max(0);
            bits[j] = bit - excess;
            ebits[j] = 0;
            fine_priority[j] = 1;
        }
        if excess > 0 {
            let extra_fine = (excess >> (stereo + BITRES)).min(MAX_FINE_BITS - ebits[j]);
            ebits[j] += extra_fine;
            let extra_bits = extra_fine * c << BITRES;
            fine_priority[j] = (extra_bits >= excess - balance) as i32;
            excess -= extra_bits;
        }
        balance = excess;
        j += 1;
    }
    out.balance = balance;
    while j < end {
        ebits[j] = bits[j] >> stereo >> BITRES;
        bits[j] = 0;
        fine_priority[j] = (ebits[j] < 1) as i32;
        j += 1;
    }
    out.coded_bands = coded_bands;
}

/// `clt_compute_allocation`.
#[allow(clippy::too_many_arguments)]
pub fn compute_allocation(
    start: usize,
    end: usize,
    offsets: &[i32; NB_EBANDS],
    cap: &[i32; NB_EBANDS],
    alloc_trim: i32,
    total: i32,
    c: i32,
    lm: i32,
    coder: &mut impl AllocCoder,
) -> Allocation {
    let mut total = total.max(0);
    let len = NB_EBANDS;
    let mut skip_start = start;
    let skip_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
    total -= skip_rsv;
    let mut intensity_rsv = 0;
    let mut dual_stereo_rsv = 0;
    if c == 2 {
        intensity_rsv = LOG2_FRAC_TABLE[end - start] as i32;
        if intensity_rsv > total {
            intensity_rsv = 0;
        } else {
            total -= intensity_rsv;
            dual_stereo_rsv = if total >= 1 << BITRES { 1 << BITRES } else { 0 };
            total -= dual_stereo_rsv;
        }
    }
    let mut bits1 = [0i32; NB_EBANDS];
    let mut bits2 = [0i32; NB_EBANDS];
    let mut thresh = [0i32; NB_EBANDS];
    let mut trim_offset = [0i32; NB_EBANDS];
    for j in start..end {
        let w = (EBANDS[j + 1] - EBANDS[j]) as i32;
        thresh[j] = (c << BITRES).max((3 * w << lm << BITRES) >> 4);
        trim_offset[j] =
            c * w * (alloc_trim - 5 - lm) * (end as i32 - j as i32 - 1) * (1 << (lm + BITRES)) >> 6;
        if w << lm == 1 {
            trim_offset[j] -= c << BITRES;
        }
    }
    let mut lo = 1i32;
    let mut hi = NB_ALLOC_VECTORS as i32 - 1;
    loop {
        let mut done = false;
        let mut psum = 0i32;
        let mid = (lo + hi) >> 1;
        for j in (start..end).rev() {
            let n = (EBANDS[j + 1] - EBANDS[j]) as i32;
            let mut bitsj = (c * n * BAND_ALLOCATION[mid as usize * len + j] as i32) << lm >> 2;
            if bitsj > 0 {
                bitsj = (bitsj + trim_offset[j]).max(0);
            }
            bitsj += offsets[j];
            if bitsj >= thresh[j] || done {
                done = true;
                psum += bitsj.min(cap[j]);
            } else if bitsj >= c << BITRES {
                psum += c << BITRES;
            }
        }
        if psum > total {
            hi = mid - 1;
        } else {
            lo = mid + 1;
        }
        if lo > hi {
            break;
        }
    }
    hi = lo;
    lo -= 1;
    for j in start..end {
        let n = (EBANDS[j + 1] - EBANDS[j]) as i32;
        let mut bits1j = (c * n * BAND_ALLOCATION[lo as usize * len + j] as i32) << lm >> 2;
        let mut bits2j = if hi >= NB_ALLOC_VECTORS as i32 {
            cap[j]
        } else {
            (c * n * BAND_ALLOCATION[hi as usize * len + j] as i32) << lm >> 2
        };
        if bits1j > 0 {
            bits1j = (bits1j + trim_offset[j]).max(0);
        }
        if bits2j > 0 {
            bits2j = (bits2j + trim_offset[j]).max(0);
        }
        if lo > 0 {
            bits1j += offsets[j];
        }
        bits2j += offsets[j];
        if offsets[j] > 0 {
            skip_start = j;
        }
        bits2j = (bits2j - bits1j).max(0);
        bits1[j] = bits1j;
        bits2[j] = bits2j;
    }
    let mut out = Allocation {
        coded_bands: 0,
        intensity: 0,
        dual_stereo: false,
        balance: 0,
        pulses: [0; NB_EBANDS],
        fine_quant: [0; NB_EBANDS],
        fine_priority: [0; NB_EBANDS],
    };
    interp_bits2pulses(
        start,
        end,
        skip_start,
        &bits1,
        &bits2,
        &thresh,
        cap,
        total,
        skip_rsv,
        intensity_rsv,
        dual_stereo_rsv,
        c,
        lm,
        coder,
        &mut out,
    );
    out
}
