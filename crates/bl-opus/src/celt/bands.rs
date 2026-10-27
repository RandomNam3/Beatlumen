//! CELT band shape decoding (PVQ with recursive splitting, stereo coupling,
//! folding, TF resolution changes), band denormalisation and anti-collapse.
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

use super::math::*;
use super::mode::*;
use super::rate::{bits2pulses, get_pulses, pulses2bits};
use super::vq::{alg_unquant, renormalise_vector};
use crate::range_dec::{RangeDecoder, BITRES};

/// Largest band size (in coefficients) at LM=3.
const MAX_BAND: usize = 176;

#[inline(always)]
fn min16(a: f32, b: f32) -> f32 {
    if a < b {
        a
    } else {
        b
    }
}

#[inline(always)]
fn max16(a: f32, b: f32) -> f32 {
    if a > b {
        a
    } else {
        b
    }
}

struct BandCtx<'a, 'b> {
    ec: &'a mut RangeDecoder<'b>,
    i: usize,
    intensity: usize,
    spread: i32,
    tf_change: i32,
    remaining_bits: i32,
    seed: u32,
    disable_inv: bool,
}

struct SplitCtx {
    inv: bool,
    imid: i32,
    iside: i32,
    delta: i32,
    itheta: i32,
    qalloc: i32,
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    const EXP2_TABLE8: [i32; 8] = [16384, 17866, 19483, 21247, 23170, 25267, 27554, 30048];
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let mut qb = (b + n2 * offset) / n2;
    qb = qb.min(b - pulse_cap - (4 << BITRES));
    qb = qb.min(8 << BITRES);
    if qb < (1 << BITRES >> 1) {
        1
    } else {
        let qn = EXP2_TABLE8[(qb & 0x7) as usize] >> (14 - (qb >> BITRES)).clamp(0, 31);
        (qn + 1) >> 1 << 1
    }
}

#[allow(clippy::too_many_arguments)]
fn compute_theta(
    ctx: &mut BandCtx,
    n: usize,
    b: &mut i32,
    big_b: i32,
    b0: i32,
    lm: i32,
    stereo: bool,
    fill: &mut i32,
) -> SplitCtx {
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
    let tell = ctx.ec.tell_frac() as i32;
    let mut itheta = 0i32;
    let mut inv = false;
    let ec = &mut *ctx.ec;
    if qn != 1 {
        if stereo && n > 2 {
            let p0 = 3i32;
            let x0 = qn / 2;
            let ft = p0 * (x0 + 1) + x0;
            let fs = ec.decode(ft as u32) as i32;
            let x = if fs < (x0 + 1) * p0 {
                fs / p0
            } else {
                x0 + 1 + (fs - (x0 + 1) * p0)
            };
            let fl = if x <= x0 {
                p0 * x
            } else {
                (x - 1 - x0) + (x0 + 1) * p0
            };
            let fh = if x <= x0 {
                p0 * (x + 1)
            } else {
                (x - x0) + (x0 + 1) * p0
            };
            ec.update(fl as u32, fh as u32, ft as u32);
            itheta = x;
        } else if b0 > 1 || stereo {
            itheta = ec.uint((qn + 1) as u32) as i32;
        } else {
            let half = qn >> 1;
            let ft = (half + 1) * (half + 1);
            let fm = ec.decode(ft as u32) as i32;
            let (fl, fs);
            if fm < (half * (half + 1) >> 1) {
                itheta = (isqrt32(8 * fm as u32 + 1) as i32 - 1) >> 1;
                fs = itheta + 1;
                fl = itheta * (itheta + 1) >> 1;
            } else {
                itheta = (2 * (qn + 1) - isqrt32(8 * (ft - fm - 1) as u32 + 1) as i32) >> 1;
                fs = qn + 1 - itheta;
                fl = ft - ((qn + 1 - itheta) * (qn + 2 - itheta) >> 1);
            }
            ec.update(fl as u32, (fl + fs) as u32, ft as u32);
        }
        itheta = ((itheta as u32 * 16384) / qn as u32) as i32;
    } else if stereo {
        if *b > 2 << BITRES && ctx.remaining_bits > 2 << BITRES {
            inv = ctx.ec.bit_logp(2);
        }
        if ctx.disable_inv {
            inv = false;
        }
        itheta = 0;
    }
    let qalloc = ctx.ec.tell_frac() as i32 - tell;
    *b -= qalloc;

    let (imid, iside, delta);
    if itheta == 0 {
        imid = 32767;
        iside = 0;
        *fill &= (1 << big_b) - 1;
        delta = -16384;
    } else if itheta == 16384 {
        imid = 0;
        iside = 32767;
        *fill &= ((1 << big_b) - 1) << big_b;
        delta = 16384;
    } else {
        imid = bitexact_cos(itheta as i16) as i32;
        iside = bitexact_cos((16384 - itheta) as i16) as i32;
        delta = frac_mul16((n as i32 - 1) << 7, bitexact_log2tan(iside, imid));
    }
    SplitCtx {
        inv,
        imid,
        iside,
        delta,
        itheta,
        qalloc,
    }
}

fn quant_band_n1(
    ctx: &mut BandCtx,
    x: &mut [f32],
    y: Option<&mut [f32]>,
    lowband_out: Option<&mut [f32]>,
) -> u32 {
    let dec_one = |ctx: &mut BandCtx, v: &mut [f32]| {
        let mut sign = 0;
        if ctx.remaining_bits >= 1 << BITRES {
            sign = ctx.ec.bits(1);
            ctx.remaining_bits -= 1 << BITRES;
        }
        v[0] = if sign != 0 { -1.0 } else { 1.0 };
    };
    dec_one(ctx, x);
    if let Some(y) = y {
        dec_one(ctx, y);
    }
    if let Some(lo) = lowband_out {
        lo[0] = x[0];
    }
    1
}

#[allow(clippy::too_many_arguments)]
fn quant_partition(
    ctx: &mut BandCtx,
    x: &mut [f32],
    n: usize,
    mut b: i32,
    mut big_b: i32,
    lowband: Option<&[f32]>,
    mut lm: i32,
    gain: f32,
    mut fill: i32,
) -> u32 {
    let i = ctx.i;
    let b0 = big_b;
    let mut cm: u32 = 0;
    let split = lm != -1 && n > 2 && {
        let cache = cache_bits((lm + 1) as usize, i);
        let last = cache.get(cache[0] as usize).copied().unwrap_or(0) as i32;
        b > last + 12
    };
    if split {
        let n = n >> 1;
        let (xl, yr) = x.split_at_mut(n);
        lm -= 1;
        if big_b == 1 {
            fill = (fill & 1) | (fill << 1);
        }
        big_b = (big_b + 1) >> 1;
        let sctx = compute_theta(ctx, n, &mut b, big_b, b0, lm, false, &mut fill);
        let mid = (1.0f32 / 32768.0) * sctx.imid as f32;
        let side = (1.0f32 / 32768.0) * sctx.iside as f32;
        let itheta = sctx.itheta;
        let mut delta = sctx.delta;
        if b0 > 1 && (itheta & 0x3fff) != 0 {
            if itheta > 8192 {
                delta -= delta >> (4 - lm);
            } else {
                delta = 0.min(delta + ((n as i32) << BITRES >> (5 - lm)));
            }
        }
        let mut mbits = 0.max(b.min((b - delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= sctx.qalloc;
        let next_lowband2 = lowband.map(|l| &l[n..]);
        let mut rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            cm = quant_partition(ctx, xl, n, mbits, big_b, lowband, lm, gain * mid, fill);
            rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(
                ctx,
                yr,
                n,
                sbits,
                big_b,
                next_lowband2,
                lm,
                gain * side,
                fill >> big_b,
            ) << (b0 >> 1);
        } else {
            cm = quant_partition(
                ctx,
                yr,
                n,
                sbits,
                big_b,
                next_lowband2,
                lm,
                gain * side,
                fill >> big_b,
            ) << (b0 >> 1);
            rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            cm |= quant_partition(ctx, xl, n, mbits, big_b, lowband, lm, gain * mid, fill);
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
            let mut iy = [0i32; MAX_BAND];
            cm = alg_unquant(x, n, k, ctx.spread, big_b as usize, ctx.ec, gain, &mut iy);
        } else {
            let cm_mask = ((1u64 << big_b) - 1) as u32;
            fill &= cm_mask as i32;
            if fill == 0 {
                x[..n].fill(0.0);
            } else {
                match lowband {
                    None => {
                        for v in x[..n].iter_mut() {
                            ctx.seed = lcg_rand(ctx.seed);
                            *v = ((ctx.seed as i32) >> 20) as f32;
                        }
                        cm = cm_mask;
                    }
                    Some(lb) => {
                        for j in 0..n {
                            ctx.seed = lcg_rand(ctx.seed);
                            let tmp = if ctx.seed & 0x8000 != 0 {
                                1.0f32 / 256.0
                            } else {
                                -1.0f32 / 256.0
                            };
                            x[j] = lb[j] + tmp;
                        }
                        cm = fill as u32;
                    }
                }
                renormalise_vector(x, n, gain);
            }
        }
    }
    cm
}

fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let n0 = n0 >> 1;
    let s = 0.707_106_78_f32;
    for i in 0..stride {
        for j in 0..n0 {
            let a = stride * 2 * j + i;
            let b = stride * (2 * j + 1) + i;
            let tmp1 = s * x[a];
            let tmp2 = s * x[b];
            x[a] = tmp1 + tmp2;
            x[b] = tmp1 - tmp2;
        }
    }
}

fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = [0f32; MAX_BAND];
    if hadamard {
        let ordery = &ORDERY_TABLE[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[ordery[i] * n0 + j] = x[j * stride + i];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[i * n0 + j] = x[j * stride + i];
            }
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let n = n0 * stride;
    let mut tmp = [0f32; MAX_BAND];
    if hadamard {
        let ordery = &ORDERY_TABLE[stride - 2..];
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[ordery[i] * n0 + j];
            }
        }
    } else {
        for i in 0..stride {
            for j in 0..n0 {
                tmp[j * stride + i] = x[i * n0 + j];
            }
        }
    }
    x[..n].copy_from_slice(&tmp[..n]);
}

/// Mono band decode. `lowband` is a private copy of the folding source
/// (it may be modified).
#[allow(clippy::too_many_arguments)]
fn quant_band(
    ctx: &mut BandCtx,
    x: &mut [f32],
    n: usize,
    b: i32,
    mut big_b: i32,
    mut lowband: Option<&mut [f32]>,
    lm: i32,
    lowband_out: Option<&mut [f32]>,
    gain: f32,
    mut fill: i32,
) -> u32 {
    const BIT_INTERLEAVE_TABLE: [i32; 16] = [0, 1, 1, 1, 2, 3, 3, 3, 2, 3, 3, 3, 2, 3, 3, 3];
    const BIT_DEINTERLEAVE_TABLE: [u32; 16] = [
        0x00, 0x03, 0x0C, 0x0F, 0x30, 0x33, 0x3C, 0x3F, 0xC0, 0xC3, 0xCC, 0xCF, 0xF0, 0xF3, 0xFC,
        0xFF,
    ];
    let n0 = n;
    let b0_orig = big_b;
    let long_blocks = b0_orig == 1;
    let mut n_b = n / big_b.max(1) as usize;
    let mut tf_change = ctx.tf_change;
    let mut time_divide = 0;
    let mut recombine = 0;

    if n == 1 {
        return quant_band_n1(ctx, x, None, lowband_out);
    }
    if tf_change > 0 {
        recombine = tf_change;
    }
    for k in 0..recombine {
        if let Some(lb) = lowband.as_deref_mut() {
            haar1(lb, n >> k, 1 << k);
        }
        fill = BIT_INTERLEAVE_TABLE[(fill & 0xF) as usize]
            | BIT_INTERLEAVE_TABLE[((fill >> 4) & 0xF) as usize] << 2;
    }
    big_b >>= recombine;
    n_b <<= recombine;
    while n_b & 1 == 0 && tf_change < 0 {
        if let Some(lb) = lowband.as_deref_mut() {
            haar1(lb, n_b, big_b as usize);
        }
        fill |= fill << big_b;
        big_b <<= 1;
        n_b >>= 1;
        time_divide += 1;
        tf_change += 1;
    }
    let b0 = big_b;
    let n_b0 = n_b;
    if b0 > 1 {
        if let Some(lb) = lowband.as_deref_mut() {
            deinterleave_hadamard(
                lb,
                n_b >> recombine,
                (b0 as usize) << recombine,
                long_blocks,
            );
        }
    }
    let mut cm = quant_partition(ctx, x, n, b, big_b, lowband.as_deref(), lm, gain, fill);

    if b0 > 1 {
        interleave_hadamard(x, n_b >> recombine, (b0 as usize) << recombine, long_blocks);
    }
    n_b = n_b0;
    big_b = b0;
    for _ in 0..time_divide {
        big_b >>= 1;
        n_b <<= 1;
        cm |= cm >> big_b;
        haar1(x, n_b, big_b as usize);
    }
    for k in 0..recombine {
        cm = BIT_DEINTERLEAVE_TABLE[(cm & 0xF) as usize];
        haar1(x, n0 >> k, 1 << k);
    }
    big_b <<= recombine;
    if let Some(lo) = lowband_out {
        let nn = (n0 as f32).sqrt();
        for j in 0..n0 {
            lo[j] = nn * x[j];
        }
    }
    cm & (((1u64 << big_b) - 1) as u32)
}

fn stereo_merge(x: &mut [f32], y: &mut [f32], mid: f32, n: usize) {
    let (xp, side) = dual_inner_prod(y, x, y, n);
    let xp = mid * xp;
    let mid2 = mid;
    let el = mid2 * mid2 + side - 2.0 * xp;
    let er = mid2 * mid2 + side + 2.0 * xp;
    if er < 6e-4 || el < 6e-4 {
        y[..n].copy_from_slice(&x[..n]);
        return;
    }
    let lgain = celt_rsqrt(el);
    let rgain = celt_rsqrt(er);
    for j in 0..n {
        let l = mid * x[j];
        let r = y[j];
        x[j] = lgain * (l - r);
        y[j] = rgain * (l + r);
    }
}

#[allow(clippy::too_many_arguments)]
fn quant_band_stereo(
    ctx: &mut BandCtx,
    x: &mut [f32],
    y: &mut [f32],
    n: usize,
    mut b: i32,
    big_b: i32,
    lowband: Option<&mut [f32]>,
    lm: i32,
    lowband_out: Option<&mut [f32]>,
    mut fill: i32,
) -> u32 {
    if n == 1 {
        return quant_band_n1(ctx, x, Some(y), lowband_out);
    }
    let orig_fill = fill;
    let sctx = compute_theta(ctx, n, &mut b, big_b, big_b, lm, true, &mut fill);
    let inv = sctx.inv;
    let itheta = sctx.itheta;
    let mid = (1.0f32 / 32768.0) * sctx.imid as f32;
    let side = (1.0f32 / 32768.0) * sctx.iside as f32;
    let mut cm;
    if n == 2 {
        let mut mbits = b;
        let mut sbits = 0;
        if itheta != 0 && itheta != 16384 {
            sbits = 1 << BITRES;
        }
        mbits -= sbits;
        let c = itheta > 8192;
        ctx.remaining_bits -= sctx.qalloc + sbits;
        let mut sign = 0u32;
        if sbits != 0 {
            sign = ctx.ec.bits(1);
        }
        let sign = 1 - 2 * sign as i32;
        {
            let (x2, y2): (&mut [f32], &mut [f32]) = if c {
                (&mut *y, &mut *x)
            } else {
                (&mut *x, &mut *y)
            };
            cm = quant_band(
                ctx,
                x2,
                n,
                mbits,
                big_b,
                lowband,
                lm,
                lowband_out,
                1.0,
                orig_fill,
            );
            y2[0] = (-sign) as f32 * x2[1];
            y2[1] = sign as f32 * x2[0];
        }
        x[0] *= mid;
        x[1] *= mid;
        y[0] *= side;
        y[1] *= side;
        let tmp = x[0];
        x[0] = tmp - y[0];
        y[0] = tmp + y[0];
        let tmp = x[1];
        x[1] = tmp - y[1];
        y[1] = tmp + y[1];
    } else {
        let mut mbits = 0.max(b.min((b - sctx.delta) / 2));
        let mut sbits = b - mbits;
        ctx.remaining_bits -= sctx.qalloc;
        let mut rebalance = ctx.remaining_bits;
        if mbits >= sbits {
            cm = quant_band(ctx, x, n, mbits, big_b, lowband, lm, lowband_out, 1.0, fill);
            rebalance = mbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 0 {
                sbits += rebalance - (3 << BITRES);
            }
            cm |= quant_band(ctx, y, n, sbits, big_b, None, lm, None, side, fill >> big_b);
        } else {
            cm = quant_band(ctx, y, n, sbits, big_b, None, lm, None, side, fill >> big_b);
            rebalance = sbits - (rebalance - ctx.remaining_bits);
            if rebalance > 3 << BITRES && itheta != 16384 {
                mbits += rebalance - (3 << BITRES);
            }
            cm |= quant_band(ctx, x, n, mbits, big_b, lowband, lm, lowband_out, 1.0, fill);
        }
    }
    if n != 2 {
        stereo_merge(x, y, mid, n);
    }
    if inv {
        for v in y[..n].iter_mut() {
            *v = -*v;
        }
    }
    cm
}

/// Parameters for [`quant_all_bands`].
pub struct AllBandsParams<'t> {
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
    pub disable_inv: bool,
}

/// Decodes all band shapes into `x` (`c` channels of `N` coefficients each).
pub fn quant_all_bands(
    p: &AllBandsParams,
    x_all: &mut [f32],
    collapse_masks: &mut [u8],
    dec: &mut RangeDecoder,
    seed: &mut u32,
) {
    let m = 1usize << p.lm;
    let big_b: i32 = if p.short_blocks { m as i32 } else { 1 };
    let n_total = m * SHORT_MDCT_SIZE;
    let norm_offset = m * EBANDS[p.start] as usize;
    let norm_len = m * EBANDS[NB_EBANDS - 1] as usize - norm_offset;
    let mut norm_buf = [0f32; 2 * 8 * 78];
    let (norm, norm2) = norm_buf.split_at_mut(norm_len);
    let (xs, ys) = x_all.split_at_mut(n_total);
    let c_count = p.c;
    let mut ctx = BandCtx {
        ec: dec,
        i: 0,
        intensity: p.intensity,
        spread: p.spread,
        tf_change: 0,
        remaining_bits: 0,
        seed: *seed,
        disable_inv: p.disable_inv,
    };
    let mut balance = p.balance;
    let mut dual_stereo = p.dual_stereo;
    let mut lowband_offset = 0usize;
    let mut update_lowband = true;
    let start = p.start;
    let end = p.end;
    for i in start..end {
        ctx.i = i;
        let last = i == end - 1;
        let band_start = m * EBANDS[i] as usize;
        let n = m * EBANDS[i + 1] as usize - band_start;
        let tell = ctx.ec.tell_frac() as i32;
        if i != start {
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
        if (band_start as i32 - n as i32 >= (m * EBANDS[start] as usize) as i32 || i == start + 1)
            && (update_lowband || lowband_offset == 0)
        {
            lowband_offset = i;
        }
        if i == start + 1 {
            // special_hybrid_folding
            let n1 = m * (EBANDS[start + 1] - EBANDS[start]) as usize;
            let n2 = m * (EBANDS[start + 2] - EBANDS[start + 1]) as usize;
            if n2 > n1 {
                norm.copy_within(2 * n1 - n2..n1, n1);
                if dual_stereo {
                    norm2.copy_within(2 * n1 - n2..n1, n1);
                }
            }
        }
        let tf_change = p.tf_res[i];
        ctx.tf_change = tf_change;

        let mut effective_lowband: Option<usize> = None;
        let (mut x_cm, mut y_cm): (u32, u32);
        if lowband_offset != 0 && (p.spread != SPREAD_AGGRESSIVE || big_b > 1 || tf_change < 0) {
            let eff = 0
                .max(m as i32 * EBANDS[lowband_offset] as i32 - norm_offset as i32 - n as i32)
                as usize;
            effective_lowband = Some(eff);
            let mut fold_start = lowband_offset;
            loop {
                fold_start -= 1;
                if m * EBANDS[fold_start] as usize <= eff + norm_offset || fold_start == 0 {
                    break;
                }
            }
            let mut fold_end = lowband_offset - 1;
            loop {
                fold_end += 1;
                if !(fold_end < i && m * (EBANDS[fold_end] as usize) < eff + norm_offset + n) {
                    break;
                }
            }
            x_cm = 0;
            y_cm = 0;
            let mut fold_i = fold_start;
            loop {
                x_cm |= collapse_masks[fold_i * c_count] as u32;
                y_cm |= collapse_masks[fold_i * c_count + c_count - 1] as u32;
                fold_i += 1;
                if fold_i >= fold_end {
                    break;
                }
            }
        } else {
            x_cm = ((1u64 << big_b) - 1) as u32;
            y_cm = x_cm;
        }

        if dual_stereo && i == p.intensity {
            dual_stereo = false;
            for j in 0..band_start - norm_offset {
                norm[j] = 0.5 * (norm[j] + norm2[j]);
            }
        }
        let out_off = band_start - norm_offset;
        let x = &mut xs[band_start..band_start + n];
        let mut lb = [0f32; MAX_BAND];
        if dual_stereo {
            let y = &mut ys[band_start..band_start + n];
            let lowband = effective_lowband.map(|e| {
                lb[..n].copy_from_slice(&norm[e..e + n]);
                &mut lb[..n]
            });
            let lo_out = if last {
                None
            } else {
                Some(&mut norm[out_off..out_off + n])
            };
            x_cm = quant_band(
                &mut ctx,
                x,
                n,
                b / 2,
                big_b,
                lowband,
                p.lm as i32,
                lo_out,
                1.0,
                x_cm as i32,
            );
            let mut lb2 = [0f32; MAX_BAND];
            let lowband2 = effective_lowband.map(|e| {
                lb2[..n].copy_from_slice(&norm2[e..e + n]);
                &mut lb2[..n]
            });
            let lo_out2 = if last {
                None
            } else {
                Some(&mut norm2[out_off..out_off + n])
            };
            y_cm = quant_band(
                &mut ctx,
                y,
                n,
                b / 2,
                big_b,
                lowband2,
                p.lm as i32,
                lo_out2,
                1.0,
                y_cm as i32,
            );
        } else {
            let lowband = effective_lowband.map(|e| {
                lb[..n].copy_from_slice(&norm[e..e + n]);
                &mut lb[..n]
            });
            let lo_out = if last {
                None
            } else {
                Some(&mut norm[out_off..out_off + n])
            };
            if c_count == 2 {
                let y = &mut ys[band_start..band_start + n];
                x_cm = quant_band_stereo(
                    &mut ctx,
                    x,
                    y,
                    n,
                    b,
                    big_b,
                    lowband,
                    p.lm as i32,
                    lo_out,
                    (x_cm | y_cm) as i32,
                );
            } else {
                x_cm = quant_band(
                    &mut ctx,
                    x,
                    n,
                    b,
                    big_b,
                    lowband,
                    p.lm as i32,
                    lo_out,
                    1.0,
                    (x_cm | y_cm) as i32,
                );
            }
            y_cm = x_cm;
        }
        collapse_masks[i * c_count] = x_cm as u8;
        collapse_masks[i * c_count + c_count - 1] = y_cm as u8;
        balance += p.pulses[i] + tell;
        update_lowband = b > (n as i32) << BITRES;
    }
    *seed = ctx.seed;
}

/// Converts normalised band shapes back to MDCT coefficients.
#[allow(clippy::too_many_arguments)]
pub fn denormalise_bands(
    x: &[f32],
    freq: &mut [f32],
    band_log_e: &[f32],
    mut start: usize,
    mut end: usize,
    m: usize,
    downsample: usize,
    silence: bool,
) {
    let n = m * SHORT_MDCT_SIZE;
    let mut bound = m * EBANDS[end] as usize;
    if downsample != 1 {
        bound = bound.min(n / downsample);
    }
    if silence {
        bound = 0;
        start = 0;
        end = 0;
    }
    let s0 = m * EBANDS[start] as usize;
    freq[..s0].fill(0.0);
    for i in start..end {
        let j0 = m * EBANDS[i] as usize;
        let j1 = m * EBANDS[i + 1] as usize;
        let lg = band_log_e[i] + E_MEANS[i];
        let g = celt_exp2(min16(32.0, lg));
        for j in j0..j1 {
            freq[j] = x[j] * g;
        }
    }
    freq[bound.min(n)..n].fill(0.0);
}

/// Prevents energy collapse for transients with multiple short MDCTs.
#[allow(clippy::too_many_arguments)]
pub fn anti_collapse(
    x_all: &mut [f32],
    collapse_masks: &[u8],
    lm: usize,
    c_count: usize,
    size: usize,
    start: usize,
    end: usize,
    log_e: &[f32],
    prev1_log_e: &[f32],
    prev2_log_e: &[f32],
    pulses: &[i32],
    mut seed: u32,
) {
    for i in start..end {
        let n0 = (EBANDS[i + 1] - EBANDS[i]) as usize;
        let depth = (((1 + pulses[i]) as u32 / n0 as u32) >> lm) as i32;
        let thresh = 0.5f32 * celt_exp2(-0.125f32 * depth as f32);
        let sqrt_1 = celt_rsqrt((n0 << lm) as f32);
        for c in 0..c_count {
            let mut prev1 = prev1_log_e[c * NB_EBANDS + i];
            let mut prev2 = prev2_log_e[c * NB_EBANDS + i];
            if c_count == 1 {
                prev1 = max16(prev1, prev1_log_e[NB_EBANDS + i]);
                prev2 = max16(prev2, prev2_log_e[NB_EBANDS + i]);
            }
            let mut ediff = log_e[c * NB_EBANDS + i] - min16(prev1, prev2);
            ediff = max16(0.0, ediff);
            let mut r = 2.0f32 * celt_exp2(-ediff);
            if lm == 3 {
                r *= 1.414_213_56_f32;
            }
            r = min16(thresh, r);
            r *= sqrt_1;
            let base = c * size + ((EBANDS[i] as usize) << lm);
            let xb = &mut x_all[base..base + (n0 << lm)];
            let mut renormalize = false;
            for k in 0..1usize << lm {
                if collapse_masks[i * c_count + c] as u32 & (1 << k) == 0 {
                    for j in 0..n0 {
                        seed = lcg_rand(seed);
                        xb[(j << lm) + k] = if seed & 0x8000 != 0 { r } else { -r };
                    }
                    renormalize = true;
                }
            }
            if renormalize {
                renormalise_vector(xb, n0 << lm, 1.0);
            }
        }
    }
}
