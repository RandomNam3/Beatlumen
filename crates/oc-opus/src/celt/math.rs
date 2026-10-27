//! Math helpers for the float CELT decoder.
//!
//! The inner-product / cross-correlation kernels reproduce the exact
//! accumulation order of libopus' x86 SSE kernels (`celt/x86/pitch_sse.c`),
//! which is what the reference float decoder uses on x86-64, so results match
//! it bit for bit. They are written as plain scalar code over 4 lanes so that
//! they are portable (and auto-vectorise well).
//!
//! Ported from libopus `celt/mathops.c`, `celt/mathops.h`, `celt/bands.c`
//! and `celt/x86/pitch_sse.c`:
//!
//! Copyright (c) 2002-2008 Jean-Marc Valin
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2009 Xiph.Org Foundation
//! Copyright (c) 2008-2009 Gregory Maxwell
//! Copyright (c) 2014 Cisco Systems, Inc. (SSE kernels)
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use crate::range_dec::ec_ilog;

/// `celt_exp2` (float build, without FLOAT_APPROX).
#[inline]
pub fn celt_exp2(x: f32) -> f32 {
    (0.693_147_180_559_945_3_f64 * x as f64).exp() as f32
}

/// `celt_cos_norm`: cos(pi/2 * x) in float.
#[inline]
pub fn celt_cos_norm(x: f32) -> f32 {
    ((0.5f32 * 3.141_592_653_f32 * x) as f64).cos() as f32
}

#[inline]
pub fn celt_rsqrt(x: f32) -> f32 {
    1.0 / x.sqrt()
}

/// Linear congruential generator used for noise filling.
#[inline]
pub fn lcg_rand(seed: u32) -> u32 {
    seed.wrapping_mul(1664525).wrapping_add(1013904223)
}

#[inline]
pub fn frac_mul16(a: i32, b: i32) -> i32 {
    (16384 + (a as i16 as i32) * (b as i16 as i32)) >> 15
}

/// Bit-exact cosine approximation used by the band splitting.
pub fn bitexact_cos(x: i16) -> i16 {
    let tmp = (4096 + (x as i32) * (x as i32)) >> 13;
    let x2 = tmp;
    let x2 = (32767 - x2) + frac_mul16(x2, -7651 + frac_mul16(x2, 8277 + frac_mul16(-626, x2)));
    (1 + x2) as i16
}

pub fn bitexact_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ec_ilog(icos as u32);
    let ls = ec_ilog(isin as u32);
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls - lc) * (1 << 11) + frac_mul16(isin, frac_mul16(isin, -2597) + 7932)
        - frac_mul16(icos, frac_mul16(icos, -2597) + 7932)
}

/// floor(sqrt(val)) with exact arithmetic.
pub fn isqrt32(mut val: u32) -> u32 {
    if val == 0 {
        return 0;
    }
    let mut g: u32 = 0;
    let mut bshift = (ec_ilog(val) - 1) >> 1;
    let mut b: u32 = 1 << bshift;
    loop {
        let t = ((g << 1) + b) << bshift;
        if t <= val {
            g += b;
            val -= t;
        }
        b >>= 1;
        bshift -= 1;
        if bshift < 0 {
            break;
        }
    }
    g
}

/// Horizontal sum of 4 lanes in the order used by `_mm_movehl_ps` +
/// `_mm_add_ss(shuffle 0x55)`.
#[inline(always)]
fn hsum(s: [f32; 4]) -> f32 {
    let a0 = s[0] + s[2];
    let a1 = s[1] + s[3];
    a0 + a1
}

/// `celt_inner_prod` (SSE accumulation order).
#[inline]
pub fn inner_prod(x: &[f32], y: &[f32], n: usize) -> f32 {
    let x = &x[..n];
    let y = &y[..n];
    let mut s = [0f32; 4];
    let n4 = n & !3;
    let mut i = 0;
    while i < n4 {
        s[0] += x[i] * y[i];
        s[1] += x[i + 1] * y[i + 1];
        s[2] += x[i + 2] * y[i + 2];
        s[3] += x[i + 3] * y[i + 3];
        i += 4;
    }
    let mut xy = hsum(s);
    while i < n {
        xy += x[i] * y[i];
        i += 1;
    }
    xy
}

/// `dual_inner_prod` (SSE accumulation order).
#[inline]
pub fn dual_inner_prod(x: &[f32], y01: &[f32], y02: &[f32], n: usize) -> (f32, f32) {
    let x = &x[..n];
    let y01 = &y01[..n];
    let y02 = &y02[..n];
    let mut s1 = [0f32; 4];
    let mut s2 = [0f32; 4];
    let n4 = n & !3;
    let mut i = 0;
    while i < n4 {
        for k in 0..4 {
            s1[k] += x[i + k] * y01[i + k];
            s2[k] += x[i + k] * y02[i + k];
        }
        i += 4;
    }
    let mut xy1 = hsum(s1);
    let mut xy2 = hsum(s2);
    while i < n {
        xy1 += x[i] * y01[i];
        xy2 += x[i] * y02[i];
        i += 1;
    }
    (xy1, xy2)
}

/// `xcorr_kernel` (SSE version): `sum[k] += sum_j x[j]*y[j+k]` for k=0..3,
/// with the two-accumulator ordering of `xcorr_kernel_sse`.
#[inline]
pub fn xcorr_kernel(x: &[f32], y: &[f32], sum: &mut [f32; 4], len: usize) {
    let mut s1 = *sum;
    let mut s2 = [0f32; 4];
    let mut j = 0usize;
    if len >= 4 {
        while j < len - 3 {
            let x0 = x[j];
            let x1 = x[j + 1];
            let x2 = x[j + 2];
            let x3 = x[j + 3];
            for k in 0..4 {
                s1[k] += x0 * y[j + k];
            }
            for k in 0..4 {
                s2[k] += x1 * y[j + 1 + k];
            }
            for k in 0..4 {
                s1[k] += x2 * y[j + 2 + k];
            }
            for k in 0..4 {
                s2[k] += x3 * y[j + 3 + k];
            }
            j += 4;
        }
    }
    if j < len {
        for k in 0..4 {
            s1[k] += x[j] * y[j + k];
        }
        j += 1;
        if j < len {
            for k in 0..4 {
                s2[k] += x[j] * y[j + k];
            }
            j += 1;
            if j < len {
                for k in 0..4 {
                    s1[k] += x[j] * y[j + k];
                }
            }
        }
    }
    for k in 0..4 {
        sum[k] = s1[k] + s2[k];
    }
}

/// `celt_pitch_xcorr_c` with the SSE kernel.
pub fn pitch_xcorr(x: &[f32], y: &[f32], xcorr: &mut [f32], len: usize, max_pitch: usize) {
    let mut i = 0usize;
    while i + 3 < max_pitch {
        let mut sum = [0f32; 4];
        xcorr_kernel(x, &y[i..], &mut sum, len);
        xcorr[i..i + 4].copy_from_slice(&sum);
        i += 4;
    }
    while i < max_pitch {
        xcorr[i] = inner_prod(x, &y[i..], len);
        i += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn isqrt_matches() {
        for v in [1u32, 2, 3, 4, 15, 16, 17, 1 << 20, u32::MAX, 123456789] {
            let r = isqrt32(v) as u64;
            assert!(r * r <= v as u64 && (r + 1) * (r + 1) > v as u64);
        }
    }
}
