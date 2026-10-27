//! PVQ codeword decoding (CWRS) and the vector-quantisation helpers used by
//! the band decoder (spreading rotation, normalisation).
//!
//! Ported from libopus `celt/cwrs.c` and `celt/vq.c`:
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2009 Xiph.Org Foundation
//! Copyright (c) 2007-2009 Timothy B. Terriberry
//! Written by Timothy B. Terriberry and Jean-Marc Valin
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::math::{celt_cos_norm, celt_rsqrt, inner_prod};
use super::mode::SPREAD_NONE;
use crate::range_dec::RangeDecoder;
use std::sync::OnceLock;

const U_DIM: usize = 180;

/// Table of U(N,K) (number of PVQ codewords helper), symmetric, computed
/// with wrapping 32-bit arithmetic exactly like the static libopus table for
/// all entries that fit in 32 bits (the only ones ever used).
fn u_table() -> &'static [u32] {
    static T: OnceLock<Vec<u32>> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = vec![0u32; U_DIM * U_DIM];
        t[0] = 1;
        for n in 1..U_DIM {
            for k in 1..U_DIM {
                t[n * U_DIM + k] = t[(n - 1) * U_DIM + k]
                    .wrapping_add(t[n * U_DIM + k - 1])
                    .wrapping_add(t[(n - 1) * U_DIM + k - 1]);
            }
        }
        t
    })
}

#[inline(always)]
fn pvq_u(t: &[u32], n: usize, k: usize) -> u32 {
    t.get(n * U_DIM + k).copied().unwrap_or(0)
}

/// Decodes `k` pulses in `n` dimensions into `y`; returns sum(y^2).
pub fn decode_pulses(y: &mut [i32], n: usize, k: usize, dec: &mut RangeDecoder) -> f32 {
    let t = u_table();
    if n >= U_DIM || k + 1 >= U_DIM || n < 2 || k == 0 {
        // Not reachable for valid allocations; fill with zeros.
        y[..n].fill(0);
        return 0.0;
    }
    let v = pvq_u(t, n, k).wrapping_add(pvq_u(t, n, k + 1));
    let i = dec.uint(v);
    cwrsi(n, k, i, y, t)
}

fn cwrsi(mut n: usize, mut k: usize, mut i: u32, y: &mut [i32], t: &[u32]) -> f32 {
    let mut yy = 0f32;
    let mut pos = 0usize;
    while n > 2 {
        if k >= n {
            // Lots of pulses case.
            let mut p = pvq_u(t, n, k + 1);
            let s: i32 = -((i >= p) as i32);
            i = i.wrapping_sub(p & s as u32);
            let k0 = k;
            let q = pvq_u(t, n, n);
            if q > i {
                k = n;
                loop {
                    k -= 1;
                    p = pvq_u(t, k, n);
                    if p <= i || k == 0 {
                        break;
                    }
                }
            } else {
                p = pvq_u(t, n, k);
                while p > i && k > 0 {
                    k -= 1;
                    p = pvq_u(t, n, k);
                }
            }
            i = i.wrapping_sub(p);
            let val = ((k0 as i32 - k as i32 + s) ^ s) as i16 as i32;
            y[pos] = val;
            pos += 1;
            yy += (val as f32) * (val as f32);
        } else {
            // Lots of dimensions case.
            let p = pvq_u(t, k, n);
            let q = pvq_u(t, k + 1, n);
            if p <= i && i < q {
                i -= p;
                y[pos] = 0;
                pos += 1;
            } else {
                let s: i32 = -((i >= q) as i32);
                i = i.wrapping_sub(q & s as u32);
                let k0 = k;
                let mut p = 0;
                while k > 0 {
                    k -= 1;
                    p = pvq_u(t, k, n);
                    if p <= i {
                        break;
                    }
                }
                i = i.wrapping_sub(p);
                let val = ((k0 as i32 - k as i32 + s) ^ s) as i16 as i32;
                y[pos] = val;
                pos += 1;
                yy += (val as f32) * (val as f32);
            }
        }
        n -= 1;
    }
    // n == 2
    let p = 2 * k as u32 + 1;
    let s: i32 = -((i >= p) as i32);
    i = i.wrapping_sub(p & s as u32);
    let k0 = k as i32;
    let kk = (i.wrapping_add(1) >> 1) as i32;
    if kk != 0 {
        i = i.wrapping_sub((2 * kk - 1) as u32);
    }
    let val = ((k0 - kk + s) ^ s) as i16 as i32;
    y[pos] = val;
    pos += 1;
    yy += (val as f32) * (val as f32);
    // n == 1
    let s = -(i as i32);
    let val = ((kk + s) ^ s) as i16 as i32;
    y[pos] = val;
    yy += (val as f32) * (val as f32);
    yy
}

fn exp_rotation1(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    let ms = -s;
    if len > stride {
        for i in 0..len - stride {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 + ms * x2;
        }
    }
    if len > 2 * stride {
        let mut i = len - 2 * stride - 1;
        loop {
            let x1 = x[i];
            let x2 = x[i + stride];
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 + ms * x2;
            if i == 0 {
                break;
            }
            i -= 1;
        }
    }
}

/// Spreading rotation (`exp_rotation`).
pub fn exp_rotation(x: &mut [f32], len: usize, dir: i32, stride: usize, k: usize, spread: i32) {
    const SPREAD_FACTOR: [i32; 3] = [15, 10, 5];
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let factor = SPREAD_FACTOR[(spread - 1).clamp(0, 2) as usize];
    let gain = (1.0f32 * len as f32) / ((len as i32 + factor * k as i32) as f32);
    let theta = 0.5f32 * (gain * gain);
    let c = celt_cos_norm(theta);
    let s = celt_cos_norm(1.0 - theta);
    let mut stride2 = 0usize;
    if len >= 8 * stride {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride.max(1);
    for i in 0..stride {
        let xs = &mut x[i * len..(i + 1) * len];
        if dir < 0 {
            if stride2 != 0 {
                exp_rotation1(xs, len, stride2, s, c);
            }
            exp_rotation1(xs, len, 1, c, s);
        } else {
            exp_rotation1(xs, len, 1, c, -s);
            if stride2 != 0 {
                exp_rotation1(xs, len, stride2, s, -c);
            }
        }
    }
}

fn extract_collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut mask = 0u32;
    for i in 0..b {
        let mut tmp = 0i32;
        for j in 0..n0 {
            tmp |= iy[i * n0 + j];
        }
        mask |= ((tmp != 0) as u32) << i;
    }
    mask
}

/// Decodes a PVQ vector of `k` pulses into `x` (unit norm times `gain`).
/// Returns the collapse mask.
pub fn alg_unquant(
    x: &mut [f32],
    n: usize,
    k: usize,
    spread: i32,
    b: usize,
    dec: &mut RangeDecoder,
    gain: f32,
    iy: &mut [i32],
) -> u32 {
    let ryy = decode_pulses(iy, n, k, dec);
    // normalise_residual
    let g = celt_rsqrt(ryy) * gain;
    for i in 0..n {
        x[i] = g * iy[i] as f32;
    }
    exp_rotation(x, n, -1, b, k, spread);
    extract_collapse_mask(iy, n, b)
}

/// Rescales `x` to have norm `gain`.
pub fn renormalise_vector(x: &mut [f32], n: usize, gain: f32) {
    let e = 1e-15f32 + inner_prod(x, x, n);
    let g = celt_rsqrt(e) * gain;
    for v in x[..n].iter_mut() {
        *v *= g;
    }
}
