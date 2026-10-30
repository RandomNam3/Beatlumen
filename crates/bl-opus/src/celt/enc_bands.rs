//! CELT band shape encoding: the encoder side of `bands` (PVQ with recursive
//! splitting, stereo angle coding with mid/side and intensity stereo, TF
//! resolution changes).
//!
//! The encoder does not resynthesise the quantised bands: folding and
//! noise filling only happen in the decoder and never influence the
//! bitstream, so only the decisions and the bit accounting are mirrored
//! here (libopus' `quant_all_bands` with `encode=1, resynth=0`).
//!
//! Ported from libopus `celt/bands.c`:
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2009 Xiph.Org Foundation
//! Copyright (c) 2008-2009 Gregory Maxwell
//! Written by Jean-Marc Valin and Gregory Maxwell
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::bands::{compute_qn, deinterleave_hadamard, haar1};
use super::math::*;
use super::mode::*;
use super::rate::{bits2pulses, get_pulses, pulses2bits};
use super::vq::alg_quant;
use crate::range_dec::BITRES;
use crate::range_enc::RangeEncoder;

const EPSILON: f32 = 1e-15;

struct BandCtx<'a> {
    ec: &'a mut RangeEncoder,
    i: usize,
    intensity: usize,
    spread: i32,
    tf_change: i32,
    remaining_bits: i32,
    avoid_split_noise: bool,
    /// Linear band amplitudes (for intensity stereo).
    band_e: &'a [f32],
}

struct Split {
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

/// Angle between two (unit norm) vectors, or between mid and side for
/// stereo, in Q14 units of pi/2.
fn stereo_itheta(x: &[f32], y: &[f32], stereo: bool, n: usize) -> i32 {
    let (emid, eside) = if stereo {
        let mut em = EPSILON;
        let mut es = EPSILON;
        for i in 0..n {
            let m = x[i] + y[i];
            let s = x[i] - y[i];
            em += m * m;
            es += s * s;
        }
        (em, es)
    } else {
        (EPSILON + inner_prod(x, x, n), EPSILON + inner_prod(y, y, n))
    };
    let mid = emid.sqrt();
    let side = eside.sqrt();
    (0.5 + 16384.0 * 0.63662 * side.atan2(mid)).floor() as i32
}

/// Mixes L/R into the mid channel weighted by the band amplitudes.
fn intensity_stereo(x: &mut [f32], y: &[f32], band_e: &[f32], i: usize, n: usize) {
    let left = band_e[i];
    let right = band_e[i + NB_EBANDS];
    let norm = EPSILON + (EPSILON + left * left + right * right).sqrt();
    let a1 = left / norm;
    let a2 = right / norm;
    for j in 0..n {
        x[j] = a1 * x[j] + a2 * y[j];
    }
}

/// L/R to mid/side.
fn stereo_split(x: &mut [f32], y: &mut [f32], n: usize) {
    for j in 0..n {
        let l = 0.707_106_78 * x[j];
        let r = 0.707_106_78 * y[j];
        x[j] = l + r;
        y[j] = r - l;
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_theta(
    ctx: &mut BandCtx,
    x: &mut [f32],
    y: &mut [f32],
    n: usize,
    b: &mut i32,
    b0: i32,
    lm: i32,
    stereo: bool,
) -> Split {
    let i = ctx.i;
    let pulse_cap = LOGN400[i] as i32 + lm * (1 << BITRES);
    let offset = (pulse_cap >> 1)
        - if stereo && n == 2 {
            QTHETA_OFFSET_TWOPHASE
        } else {
            QTHETA_OFFSET
        };
    let mut qn = compute_qn(n as i32, *b, offset, pulse_cap, stereo);
    if stereo && i >= ctx.intensity {
        qn = 1;
    }
    // theta is the atan() of the ratio between the (normalised) side and
    // mid; both have unit norm and are orthogonal.
    let mut itheta = stereo_itheta(x, y, stereo, n);
    let tell = ctx.ec.tell_frac() as i32;
    if qn != 1 {
        itheta = (itheta * qn + 8192) >> 14;
        if !stereo && ctx.avoid_split_noise && itheta > 0 && itheta < qn {
            // Make sure the chosen angle does not make the allocation inject
            // noise on one side without energy.
            let unquantized = itheta * 16384 / qn;
            let imid = bitexact_cos(unquantized as i16) as i32;
            let iside = bitexact_cos((16384 - unquantized) as i16) as i32;
            let delta = frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid));
            if delta > *b {
                itheta = qn;
            } else if delta < -*b {
                itheta = 0;
            }
        }
        let ec = &mut *ctx.ec;
        if stereo && n > 2 {
            // Step pdf: probability p0 up to itheta = 8192, 1 after.
            let p0 = 3;
            let x0 = qn / 2;
            let ft = p0 * (x0 + 1) + x0;
            let (fl, fh) = if itheta <= x0 {
                (p0 * itheta, p0 * (itheta + 1))
            } else {
                (
                    (itheta - 1 - x0) + (x0 + 1) * p0,
                    (itheta - x0) + (x0 + 1) * p0,
                )
            };
            ec.encode(fl as u32, fh as u32, ft as u32);
        } else if b0 > 1 || stereo {
            ec.uint(itheta as u32, (qn + 1) as u32);
        } else {
            // Triangular pdf.
            let half = qn >> 1;
            let ft = (half + 1) * (half + 1);
            let fs = if itheta <= half {
                itheta + 1
            } else {
                qn + 1 - itheta
            };
            let fl = if itheta <= half {
                itheta * (itheta + 1) >> 1
            } else {
                ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1)
            };
            ec.encode(fl as u32, (fl + fs) as u32, ft as u32);
        }
        itheta = itheta * 16384 / qn;
        if stereo {
            if itheta == 0 {
                intensity_stereo(x, y, ctx.band_e, i, n);
            } else {
                stereo_split(x, y, n);
            }
        }
    } else if stereo {
        // Phase inversion (only signalled when there are bits for it).
        let inv = itheta > 8192;
        if inv {
            for v in y[..n].iter_mut() {
                *v = -*v;
            }
        }
        intensity_stereo(x, y, ctx.band_e, i, n);
        if *b > 2 << BITRES && ctx.remaining_bits > 2 << BITRES {
            ctx.ec.bit_logp(inv, 2);
        }
        itheta = 0;
    }
    let qalloc = ctx.ec.tell_frac() as i32 - tell;
    *b -= qalloc;
    let delta = if itheta == 0 {
        -16384
    } else if itheta == 16384 {
        16384
    } else {
        let imid = bitexact_cos(itheta as i16) as i32;
        let iside = bitexact_cos((16384 - itheta) as i16) as i32;
        frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid))
    };
    Split {
        delta,
        itheta,
        qalloc,
    }
}

fn quant_band_n1(ctx: &mut BandCtx, x: &[f32], y: Option<&[f32]>) {
    for v in std::iter::once(x).chain(y) {
        if ctx.remaining_bits >= 1 << BITRES {
            ctx.ec.bits((v[0] < 0.0) as u32, 1);
            ctx.remaining_bits -= 1 << BITRES;
        }
    }
}

fn quant_partition(ctx: &mut BandCtx, x: &mut [f32], n: usize, mut b: i32, big_b: i32, lm: i32) {
    let i = ctx.i;
    let b0 = big_b;
    let split = lm != -1 && n > 2 && {
        let cache = cache_bits((lm + 1) as usize, i);
        let last = cache.get(cache[0] as usize).copied().unwrap_or(0) as i32;
        b > last + 12
    };
    if split {
        let n = n >> 1;
        let (xl, yr) = x.split_at_mut(n);
        let lm = lm - 1;
        let big_b = (big_b + 1) >> 1;
        let s = compute_theta(ctx, xl, yr, n, &mut b, b0, lm, false);
        let itheta = s.itheta;
        let mut delta = s.delta;
        if b0 > 1 && (itheta & 0x3fff) != 0 {
            if itheta > 8192 {
                delta -= delta >> (4 - lm);
            } else {
                delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
            }
        }
        let mut mbits = 0.max(b.min((b - delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= s.qalloc;
        let rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            quant_partition(ctx, xl, n, mbits, big_b, lm);
            let rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            quant_partition(ctx, yr, n, sbits, big_b, lm);
        } else {
            quant_partition(ctx, yr, n, sbits, big_b, lm);
            let rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            quant_partition(ctx, xl, n, mbits, big_b, lm);
        }
    } else {
        let mut q = bits2pulses(i, lm, b);
        let mut curr_bits = pulses2bits(i, lm, q);
        ctx.remaining_bits -= curr_bits;
        while ctx.remaining_bits < 0 && q > 0 {
            ctx.remaining_bits += curr_bits;
            q -= 1;
            curr_bits = pulses2bits(i, lm, q);
            ctx.remaining_bits -= curr_bits;
        }
        if q != 0 {
            let k = get_pulses(q) as usize;
            alg_quant(x, n, k, ctx.spread, big_b as usize, ctx.ec);
        }
    }
}

/// Mono band encode (also used for each half of dual stereo and for mid
/// and side).
fn quant_band(ctx: &mut BandCtx, x: &mut [f32], n: usize, b: i32, mut big_b: i32, lm: i32) {
    if n == 1 {
        quant_band_n1(ctx, x, None);
        return;
    }
    let long_blocks = big_b == 1;
    let mut n_b = n / big_b.max(1) as usize;
    let mut tf_change = ctx.tf_change;
    let recombine = tf_change.max(0);
    // Band recombining to increase frequency resolution.
    for k in 0..recombine {
        haar1(x, n >> k, 1 << k);
    }
    big_b >>= recombine;
    n_b <<= recombine;
    // Increasing the time resolution.
    while n_b & 1 == 0 && tf_change < 0 {
        haar1(x, n_b, big_b as usize);
        big_b <<= 1;
        n_b >>= 1;
        tf_change += 1;
    }
    // Reorganise the samples in time order instead of frequency order.
    if big_b > 1 {
        deinterleave_hadamard(
            x,
            n_b >> recombine,
            (big_b as usize) << recombine,
            long_blocks,
        );
    }
    quant_partition(ctx, x, n, b, big_b, lm);
}

#[allow(clippy::too_many_arguments)]
fn quant_band_stereo(
    ctx: &mut BandCtx,
    x: &mut [f32],
    y: &mut [f32],
    n: usize,
    mut b: i32,
    big_b: i32,
    lm: i32,
) {
    if n == 1 {
        quant_band_n1(ctx, x, Some(y));
        return;
    }
    let s = compute_theta(ctx, x, y, n, &mut b, big_b, lm, true);
    let itheta = s.itheta;
    if n == 2 {
        let mut mbits = b;
        let mut sbits = 0;
        // Only one bit is needed for the side.
        if itheta != 0 && itheta != 16384 {
            sbits = 1 << BITRES;
        }
        mbits -= sbits;
        let c = itheta > 8192;
        ctx.remaining_bits -= s.qalloc + sbits;
        let (x2, y2): (&mut [f32], &mut [f32]) = if c { (y, x) } else { (x, y) };
        if sbits != 0 {
            let sign = x2[0] * y2[1] - x2[1] * y2[0] < 0.0;
            ctx.ec.bits(sign as u32, 1);
        }
        quant_band(ctx, x2, n, mbits, big_b, lm);
    } else {
        let mut mbits = 0.max(b.min((b - s.delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= s.qalloc;
        let rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            quant_band(ctx, x, n, mbits, big_b, lm);
            let rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            quant_band(ctx, y, n, sbits, big_b, lm);
        } else {
            quant_band(ctx, y, n, sbits, big_b, lm);
            let rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            quant_band(ctx, x, n, mbits, big_b, lm);
        }
    }
}

/// Parameters for [`quant_all_bands_enc`].
pub struct EncBandsParams<'t> {
    pub start: usize,
    pub end: usize,
    pub c: usize,
    pub pulses: &'t [i32; NB_EBANDS],
    pub short_blocks: bool,
    pub spread: i32,
    pub dual_stereo: bool,
    pub intensity: usize,
    pub tf_res: &'t [i32; NB_EBANDS],
    pub total_bits: i32,
    pub balance: i32,
    pub lm: usize,
    pub coded_bands: usize,
    /// Linear band amplitudes, `c * NB_EBANDS`.
    pub band_e: &'t [f32],
}

/// Encodes all band shapes of `x_all` (`c` channels of normalised MDCT
/// coefficients, clobbered).
pub fn quant_all_bands_enc(p: &EncBandsParams, x_all: &mut [f32], enc: &mut RangeEncoder) {
    let m = 1usize << p.lm;
    let big_b: i32 = if p.short_blocks { m as i32 } else { 1 };
    let n_total = m * SHORT_MDCT_SIZE;
    let (xs, ys) = x_all.split_at_mut(n_total);
    let mut ctx = BandCtx {
        ec: enc,
        i: 0,
        intensity: p.intensity,
        spread: p.spread,
        tf_change: 0,
        remaining_bits: 0,
        avoid_split_noise: big_b > 1,
        band_e: p.band_e,
    };
    let mut balance = p.balance;
    let mut dual_stereo = p.dual_stereo;
    for i in p.start..p.end {
        ctx.i = i;
        let band_start = m * EBANDS[i] as usize;
        let n = m * EBANDS[i + 1] as usize - band_start;
        let tell = ctx.ec.tell_frac() as i32;
        if i != p.start {
            balance -= tell;
        }
        let remaining_bits = p.total_bits - tell - 1;
        ctx.remaining_bits = remaining_bits;
        let b = if i < p.coded_bands {
            let curr_balance = balance / (3.min(p.coded_bands as i32 - i as i32));
            0.max(16383.min((remaining_bits + 1).min(p.pulses[i] + curr_balance)))
        } else {
            0
        };
        ctx.tf_change = p.tf_res[i];
        if dual_stereo && i == p.intensity {
            dual_stereo = false;
        }
        let x = &mut xs[band_start..band_start + n];
        if dual_stereo {
            let y = &mut ys[band_start..band_start + n];
            quant_band(&mut ctx, x, n, b / 2, big_b, p.lm as i32);
            quant_band(&mut ctx, y, n, b / 2, big_b, p.lm as i32);
        } else if p.c == 2 {
            let y = &mut ys[band_start..band_start + n];
            quant_band_stereo(&mut ctx, x, y, n, b, big_b, p.lm as i32);
        } else {
            quant_band(&mut ctx, x, n, b, big_b, p.lm as i32);
        }
        balance += p.pulses[i] + tell;
    }
}
