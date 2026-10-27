//! Pitch post-filter (comb filter) and the pitch analysis / LPC helpers used
//! by the CELT packet loss concealment.
//!
//! Ported from libopus `celt/celt.c` (comb_filter), `celt/pitch.c` and
//! `celt/celt_lpc.c`:
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2010 Xiph.Org Foundation
//! Copyright (c) 2008 Gregory Maxwell
//! Copyright (c) 2009-2010 Xiph.Org Foundation (celt_lpc.c)
//! Written by Jean-Marc Valin and Gregory Maxwell
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::math::{inner_prod, pitch_xcorr, xcorr_kernel};
use super::mode::COMBFILTER_MINPERIOD;

const GAINS: [[f32; 3]; 3] = [
    [0.306_640_625, 0.217_041_015_6, 0.129_638_671_9],
    [0.463_867_187_5, 0.268_066_406_2, 0.0],
    [0.799_804_687_5, 0.100_097_656_2, 0.0],
];

/// Comb filter (pitch post-filter). Reads `src[x_off - T - 2 ..]` and
/// writes `n` samples to `dst[y_off..]`. When `dst` and `src` are the same
/// buffer (the decoder's in-place use), pass `None` for `src`: the filter is
/// then recursive exactly like the C code with `x == y`.
#[allow(clippy::too_many_arguments)]
pub fn comb_filter(
    buf: &mut [f32],
    y_off: usize,
    src: Option<(&[f32], usize)>,
    t0: i32,
    t1: i32,
    n: usize,
    g0: f32,
    g1: f32,
    tapset0: usize,
    tapset1: usize,
    window: &[f32],
    mut overlap: usize,
) {
    // Accessor for x[] (either the separate source or the in-place buffer).
    macro_rules! xr {
        ($b:expr, $idx:expr) => {
            match src {
                Some((s, xo)) => s[(xo as isize + $idx) as usize],
                None => $b[(y_off as isize + $idx) as usize],
            }
        };
    }
    if g0 == 0.0 && g1 == 0.0 {
        if let Some((s, xo)) = src {
            buf[y_off..y_off + n].copy_from_slice(&s[xo..xo + n]);
        }
        return;
    }
    let t0 = t0.max(COMBFILTER_MINPERIOD) as isize;
    let t1 = t1.max(COMBFILTER_MINPERIOD) as isize;
    let tapset0 = tapset0.min(2);
    let tapset1 = tapset1.min(2);
    let g00 = g0 * GAINS[tapset0][0];
    let g01 = g0 * GAINS[tapset0][1];
    let g02 = g0 * GAINS[tapset0][2];
    let g10 = g1 * GAINS[tapset1][0];
    let g11 = g1 * GAINS[tapset1][1];
    let g12 = g1 * GAINS[tapset1][2];
    let mut x1 = xr!(buf, -t1 + 1);
    let mut x2 = xr!(buf, -t1);
    let mut x3 = xr!(buf, -t1 - 1);
    let mut x4 = xr!(buf, -t1 - 2);
    if g0 == g1 && t0 == t1 && tapset0 == tapset1 {
        overlap = 0;
    }
    let overlap = overlap.min(n);
    for i in 0..overlap {
        let ii = i as isize;
        let x0 = xr!(buf, ii - t1 + 2);
        let f = window[i] * window[i];
        let v = xr!(buf, ii)
            + ((1.0 - f) * g00) * xr!(buf, ii - t0)
            + ((1.0 - f) * g01) * (xr!(buf, ii - t0 + 1) + xr!(buf, ii - t0 - 1))
            + ((1.0 - f) * g02) * (xr!(buf, ii - t0 + 2) + xr!(buf, ii - t0 - 2))
            + (f * g10) * x2
            + (f * g11) * (x1 + x3)
            + (f * g12) * (x0 + x4);
        buf[y_off + i] = v;
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
    if g1 == 0.0 {
        if let Some((s, xo)) = src {
            buf[y_off + overlap..y_off + n].copy_from_slice(&s[xo + overlap..xo + n]);
        }
        return;
    }
    // Constant filter part, with the accumulation order of the SSE kernel
    // (comb_filter_const_sse): y = (x + g10*x[-T]) + (g11*(x[-T+1]+x[-T-1]) + g12*(x[-T+2]+x[-T-2])).
    for i in overlap..n {
        let ii = i as isize;
        let a = xr!(buf, ii) + g10 * xr!(buf, ii - t1);
        let b = g11 * (xr!(buf, ii - t1 + 1) + xr!(buf, ii - t1 - 1))
            + g12 * (xr!(buf, ii - t1 + 2) + xr!(buf, ii - t1 - 2));
        buf[y_off + i] = a + b;
    }
}

fn find_best_pitch(
    xcorr: &[f32],
    y: &[f32],
    len: usize,
    max_pitch: usize,
    best_pitch: &mut [usize; 2],
) {
    let mut syy = 1f32;
    let mut best_num = [-1f32; 2];
    let mut best_den = [0f32; 2];
    best_pitch[0] = 0;
    best_pitch[1] = 1;
    for &v in &y[..len] {
        syy += v * v;
    }
    for i in 0..max_pitch {
        if xcorr[i] > 0.0 {
            let xcorr16 = xcorr[i] * 1e-12;
            let num = xcorr16 * xcorr16;
            if num * best_den[1] > best_num[1] * syy {
                if num * best_den[0] > best_num[0] * syy {
                    best_num[1] = best_num[0];
                    best_den[1] = best_den[0];
                    best_pitch[1] = best_pitch[0];
                    best_num[0] = num;
                    best_den[0] = syy;
                    best_pitch[0] = i;
                } else {
                    best_num[1] = num;
                    best_den[1] = syy;
                    best_pitch[1] = i;
                }
            }
        }
        syy += y[i + len] * y[i + len] - y[i] * y[i];
        syy = if 1.0 > syy { 1.0 } else { syy };
    }
}

fn celt_fir5(x: &mut [f32], num: &[f32; 5]) {
    let (mut m0, mut m1, mut m2, mut m3, mut m4) = (0f32, 0f32, 0f32, 0f32, 0f32);
    for v in x.iter_mut() {
        let mut sum = *v;
        sum += num[0] * m0;
        sum += num[1] * m1;
        sum += num[2] * m2;
        sum += num[3] * m3;
        sum += num[4] * m4;
        m4 = m3;
        m3 = m2;
        m2 = m1;
        m1 = m0;
        m0 = *v;
        *v = sum;
    }
}

/// `_celt_lpc`: Levinson-Durbin recursion.
pub fn celt_lpc(lpc: &mut [f32], ac: &[f32], p: usize) {
    let mut error = ac[0];
    lpc[..p].fill(0.0);
    if ac[0] > 1e-10 {
        for i in 0..p {
            let mut rr = 0f32;
            for j in 0..i {
                rr += lpc[j] * ac[i - j];
            }
            rr += ac[i + 1];
            let r = -(rr / error);
            lpc[i] = r;
            for j in 0..(i + 1) >> 1 {
                let tmp1 = lpc[j];
                let tmp2 = lpc[i - 1 - j];
                lpc[j] = tmp1 + r * tmp2;
                lpc[i - 1 - j] = tmp2 + r * tmp1;
            }
            error -= (r * r) * error;
            if error <= 0.001 * ac[0] {
                break;
            }
        }
    }
}

/// `_celt_autocorr` (float, optional window).
pub fn celt_autocorr(
    x: &[f32],
    ac: &mut [f32],
    window: Option<&[f32]>,
    overlap: usize,
    lag: usize,
    n: usize,
) {
    let fast_n = n - lag;
    let mut xx = vec![0f32; n];
    xx.copy_from_slice(&x[..n]);
    if let Some(w) = window {
        for i in 0..overlap {
            xx[i] = x[i] * w[i];
            xx[n - i - 1] = x[n - i - 1] * w[i];
        }
    }
    pitch_xcorr(&xx, &xx, ac, fast_n, lag + 1);
    for k in 0..=lag {
        let mut d = 0f32;
        for i in k + fast_n..n {
            d += xx[i] * xx[i - k];
        }
        ac[k] += d;
    }
}

/// `pitch_downsample` (float): low-passes and decimates by 2.
pub fn pitch_downsample(x: &[&[f32]], x_lp: &mut [f32], len: usize) {
    let half = len >> 1;
    let x0 = x[0];
    for i in 1..half {
        x_lp[i] = 0.25 * x0[2 * i - 1] + 0.25 * x0[2 * i + 1] + 0.5 * x0[2 * i];
    }
    x_lp[0] = 0.25 * x0[1] + 0.5 * x0[0];
    if x.len() == 2 {
        let x1 = x[1];
        for i in 1..half {
            x_lp[i] += 0.25 * x1[2 * i - 1] + 0.25 * x1[2 * i + 1] + 0.5 * x1[2 * i];
        }
        x_lp[0] += 0.25 * x1[1] + 0.5 * x1[0];
    }
    let mut ac = [0f32; 5];
    celt_autocorr(&x_lp[..half], &mut ac, None, 0, 4, half);
    ac[0] *= 1.0001;
    for (i, a) in ac.iter_mut().enumerate().skip(1) {
        *a -= *a * (0.008 * i as f32) * (0.008 * i as f32);
    }
    let mut lpc = [0f32; 4];
    celt_lpc(&mut lpc, &ac, 4);
    let mut tmp = 1.0f32;
    for l in lpc.iter_mut() {
        tmp *= 0.9;
        *l *= tmp;
    }
    let c1 = 0.8f32;
    let lpc2 = [
        lpc[0] + 0.8,
        lpc[1] + c1 * lpc[0],
        lpc[2] + c1 * lpc[1],
        lpc[3] + c1 * lpc[2],
        c1 * lpc[3],
    ];
    celt_fir5(&mut x_lp[..half], &lpc2);
}

/// `pitch_search`: returns the pitch period (in samples of the 2x-decimated
/// signal times 2).
pub fn pitch_search(x_lp: &[f32], y: &[f32], len: usize, max_pitch: usize) -> i32 {
    let lag = len + max_pitch;
    let x_lp4: Vec<f32> = (0..len >> 2).map(|j| x_lp[2 * j]).collect();
    let y_lp4: Vec<f32> = (0..lag >> 2).map(|j| y[2 * j]).collect();
    let mut xcorr = vec![0f32; max_pitch >> 1];
    let mut best_pitch = [0usize, 0usize];
    pitch_xcorr(&x_lp4, &y_lp4, &mut xcorr, len >> 2, max_pitch >> 2);
    find_best_pitch(&xcorr, &y_lp4, len >> 2, max_pitch >> 2, &mut best_pitch);
    for i in 0..max_pitch >> 1 {
        xcorr[i] = 0.0;
        if (i as i32 - 2 * best_pitch[0] as i32).abs() > 2
            && (i as i32 - 2 * best_pitch[1] as i32).abs() > 2
        {
            continue;
        }
        let sum = inner_prod(x_lp, &y[i..], len >> 1);
        xcorr[i] = if -1.0 > sum { -1.0 } else { sum };
    }
    find_best_pitch(&xcorr, y, len >> 1, max_pitch >> 1, &mut best_pitch);
    let offset;
    if best_pitch[0] > 0 && best_pitch[0] < (max_pitch >> 1) - 1 {
        let a = xcorr[best_pitch[0] - 1];
        let b = xcorr[best_pitch[0]];
        let c = xcorr[best_pitch[0] + 1];
        if (c - a) > 0.7 * (b - a) {
            offset = 1;
        } else if (a - c) > 0.7 * (b - c) {
            offset = -1;
        } else {
            offset = 0;
        }
    } else {
        offset = 0;
    }
    2 * best_pitch[0] as i32 - offset
}

/// `celt_fir` (x has `ord` samples of history before `x_off`).
pub fn celt_fir(x: &[f32], x_off: usize, num: &[f32], y: &mut [f32], n: usize, ord: usize) {
    let mut rnum = [0f32; 32];
    for i in 0..ord {
        rnum[i] = num[ord - i - 1];
    }
    let mut i = 0;
    while i + 3 < n {
        let mut sum = [
            x[x_off + i],
            x[x_off + i + 1],
            x[x_off + i + 2],
            x[x_off + i + 3],
        ];
        xcorr_kernel(&rnum[..ord], &x[x_off + i - ord..], &mut sum, ord);
        y[i..i + 4].copy_from_slice(&sum);
        i += 4;
    }
    while i < n {
        let mut sum = x[x_off + i];
        for j in 0..ord {
            sum += rnum[j] * x[x_off + i + j - ord];
        }
        y[i] = sum;
        i += 1;
    }
}

/// `celt_iir` (in place on `buf[off..off+n]`), updating `mem`.
pub fn celt_iir(buf: &mut [f32], off: usize, den: &[f32], n: usize, ord: usize, mem: &mut [f32]) {
    let mut rden = [0f32; 32];
    for i in 0..ord {
        rden[i] = den[ord - i - 1];
    }
    let mut y = vec![0f32; n + ord];
    for i in 0..ord {
        y[i] = -mem[ord - i - 1];
    }
    let mut i = 0;
    while i + 3 < n {
        let mut sum = [
            buf[off + i],
            buf[off + i + 1],
            buf[off + i + 2],
            buf[off + i + 3],
        ];
        xcorr_kernel(&rden[..ord], &y[i..], &mut sum, ord);
        y[i + ord] = -sum[0];
        buf[off + i] = sum[0];
        sum[1] += y[i + ord] * den[0];
        y[i + ord + 1] = -sum[1];
        buf[off + i + 1] = sum[1];
        sum[2] += y[i + ord + 1] * den[0];
        sum[2] += y[i + ord] * den[1];
        y[i + ord + 2] = -sum[2];
        buf[off + i + 2] = sum[2];
        sum[3] += y[i + ord + 2] * den[0];
        sum[3] += y[i + ord + 1] * den[1];
        sum[3] += y[i + ord] * den[2];
        y[i + ord + 3] = -sum[3];
        buf[off + i + 3] = sum[3];
        i += 4;
    }
    while i < n {
        let mut sum = buf[off + i];
        for j in 0..ord {
            sum -= rden[j] * y[i + j];
        }
        y[i + ord] = sum;
        buf[off + i] = sum;
        i += 1;
    }
    for i in 0..ord {
        mem[i] = buf[off + n - i - 1];
    }
}
