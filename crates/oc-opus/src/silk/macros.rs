//! SILK fixed-point arithmetic helpers (bit-exact with the C macros on a
//! 64-bit two's complement platform; wrapping where C relies on it).
//!
//! Ported from libopus `silk/macros.h`, `silk/SigProc_FIX.h` and
//! `silk/Inlines.h`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

#![allow(dead_code)]

#[inline(always)]
pub fn smulwb(a: i32, b: i32) -> i32 {
    ((a as i64 * (b as i16) as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}

#[inline(always)]
pub fn smulwt(a: i32, b: i32) -> i32 {
    ((a as i64 * (b >> 16) as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlawt(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwt(b, c))
}

#[inline(always)]
pub fn smulbb(a: i32, b: i32) -> i32 {
    (a as i16 as i32) * (b as i16 as i32)
}

#[inline(always)]
pub fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulbb(b, c))
}

#[inline(always)]
pub fn smultt(a: i32, b: i32) -> i32 {
    (a >> 16).wrapping_mul(b >> 16)
}

#[inline(always)]
pub fn smulww(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 16) as i32
}

#[inline(always)]
pub fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulww(b, c))
}

#[inline(always)]
pub fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}

#[inline(always)]
pub fn smull(a: i32, b: i32) -> i64 {
    a as i64 * b as i64
}

#[inline(always)]
pub fn mul(a: i32, b: i32) -> i32 {
    a.wrapping_mul(b)
}

#[inline(always)]
pub fn mla(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(b.wrapping_mul(c))
}

#[inline(always)]
pub fn add32(a: i32, b: i32) -> i32 {
    a.wrapping_add(b)
}

#[inline(always)]
pub fn sub32(a: i32, b: i32) -> i32 {
    a.wrapping_sub(b)
}

#[inline(always)]
pub fn lshift(a: i32, s: i32) -> i32 {
    ((a as u32) << (s & 31)) as i32
}

#[inline(always)]
pub fn rshift(a: i32, s: i32) -> i32 {
    a >> (s & 31)
}

#[inline(always)]
pub fn add_lshift32(a: i32, b: i32, s: i32) -> i32 {
    a.wrapping_add(lshift(b, s))
}

#[inline(always)]
pub fn add_rshift32(a: i32, b: i32, s: i32) -> i32 {
    a.wrapping_add(b >> s)
}

#[inline(always)]
pub fn sub_lshift32(a: i32, b: i32, s: i32) -> i32 {
    a.wrapping_sub(lshift(b, s))
}

#[inline(always)]
pub fn rshift_round(a: i32, s: i32) -> i32 {
    if s == 1 {
        (a >> 1).wrapping_add(a & 1)
    } else {
        ((a >> (s - 1)).wrapping_add(1)) >> 1
    }
}

#[inline(always)]
pub fn rshift_round64(a: i64, s: i32) -> i64 {
    if s == 1 {
        (a >> 1).wrapping_add(a & 1)
    } else {
        ((a >> (s - 1)).wrapping_add(1)) >> 1
    }
}

#[inline(always)]
pub fn sat16(a: i32) -> i32 {
    a.clamp(i16::MIN as i32, i16::MAX as i32)
}

#[inline(always)]
pub fn add_sat16(a: i32, b: i32) -> i32 {
    sat16(a.wrapping_add(b))
}

#[inline(always)]
pub fn add_sat32(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}

#[inline(always)]
pub fn sub_sat32(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}

#[inline(always)]
pub fn limit(a: i32, l1: i32, l2: i32) -> i32 {
    if l1 > l2 {
        if a > l1 {
            l1
        } else if a < l2 {
            l2
        } else {
            a
        }
    } else if a > l2 {
        l2
    } else if a < l1 {
        l1
    } else {
        a
    }
}

#[inline(always)]
pub fn lshift_sat32(a: i32, s: i32) -> i32 {
    lshift(limit(a, i32::MIN >> s, i32::MAX >> s), s)
}

#[inline(always)]
pub fn div32(a: i32, b: i32) -> i32 {
    if b == 0 {
        0
    } else {
        a.wrapping_div(b)
    }
}

#[inline(always)]
pub fn abs(a: i32) -> i32 {
    if a > 0 {
        a
    } else {
        a.wrapping_neg()
    }
}

#[inline(always)]
pub fn clz32(x: i32) -> i32 {
    (x as u32).leading_zeros() as i32
}

#[inline(always)]
pub fn clz16(x: i16) -> i32 {
    (((x as u16 as u32) << 16) | 0x8000).leading_zeros() as i32
}

#[inline(always)]
pub fn ror32(a: i32, rot: i32) -> i32 {
    (a as u32).rotate_right(rot.rem_euclid(32) as u32) as i32
}

#[inline(always)]
pub fn clz_frac(x: i32) -> (i32, i32) {
    let lz = clz32(x);
    (lz, ror32(x, 24 - lz) & 0x7f)
}

pub fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let (lz, frac_q7) = clz_frac(x);
    let mut y: i32 = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, smulbb(213, frac_q7))
}

/// Approximation of `(a32 << Qres) / b32`.
pub fn div32_varq(a32: i32, b32: i32, qres: i32) -> i32 {
    if b32 == 0 {
        return 0;
    }
    let a_headrm = clz32(abs(a32)) - 1;
    let mut a32_nrm = lshift(a32, a_headrm);
    let b_headrm = clz32(abs(b32)) - 1;
    let b32_nrm = lshift(b32, b_headrm);
    let b32_inv = div32(i32::MAX >> 2, b32_nrm >> 16);
    let mut result = smulwb(a32_nrm, b32_inv);
    a32_nrm = a32_nrm.wrapping_sub(lshift(smmul(b32_nrm, result), 3));
    result = smlawb(result, a32_nrm, b32_inv);
    let lshift_v = 29 + a_headrm - b_headrm - qres;
    if lshift_v < 0 {
        lshift_sat32(result, -lshift_v)
    } else if lshift_v < 32 {
        result >> lshift_v
    } else {
        0
    }
}

/// Approximation of `(1 << Qres) / b32`.
pub fn inverse32_varq(b32: i32, qres: i32) -> i32 {
    if b32 == 0 {
        return i32::MAX;
    }
    let b_headrm = clz32(abs(b32)) - 1;
    let b32_nrm = lshift(b32, b_headrm);
    let b32_inv = div32(i32::MAX >> 2, b32_nrm >> 16);
    let mut result = lshift(b32_inv, 16);
    let err_q32 = lshift((1i32 << 29).wrapping_sub(smulwb(b32_nrm, b32_inv)), 3);
    result = smlaww(result, err_q32, b32_inv);
    let lshift_v = 61 - b_headrm - qres;
    if lshift_v <= 0 {
        lshift_sat32(result, -lshift_v)
    } else if lshift_v < 32 {
        result >> lshift_v
    } else {
        0
    }
}

/// `silk_RAND`
#[inline(always)]
pub fn silk_rand(seed: i32) -> i32 {
    907_633_515i32.wrapping_add(seed.wrapping_mul(196_314_165))
}

/// `silk_log2lin`: approximation of 2^(x/128).
pub fn log2lin(in_log_q7: i32) -> i32 {
    if in_log_q7 < 0 {
        return 0;
    } else if in_log_q7 >= 3967 {
        return i32::MAX;
    }
    let mut out = lshift(1, in_log_q7 >> 7);
    let frac_q7 = in_log_q7 & 0x7F;
    if in_log_q7 < 2048 {
        out = add_rshift32(
            out,
            mul(out, smlawb(frac_q7, smulbb(frac_q7, 128 - frac_q7), -174)),
            7,
        );
    } else {
        out = mla(
            out,
            out >> 7,
            smlawb(frac_q7, smulbb(frac_q7, 128 - frac_q7), -174),
        );
    }
    out
}

/// `silk_sum_sqr_shift`
pub fn sum_sqr_shift(x: &[i16]) -> (i32, i32) {
    let len = x.len() as i32;
    let mut shft = 31 - clz32(len);
    let mut nrg: i32 = len;
    let n = x.len();
    let mut i = 0usize;
    while i + 1 < n {
        let t = (smulbb(x[i] as i32, x[i] as i32) as u32)
            .wrapping_add(smulbb(x[i + 1] as i32, x[i + 1] as i32) as u32);
        nrg = (nrg as u32).wrapping_add(t >> shft) as i32;
        i += 2;
    }
    if i < n {
        let t = smulbb(x[i] as i32, x[i] as i32) as u32;
        nrg = (nrg as u32).wrapping_add(t >> shft) as i32;
    }
    shft = 0.max(shft + 3 - clz32(nrg));
    nrg = 0;
    i = 0;
    while i + 1 < n {
        let t = (smulbb(x[i] as i32, x[i] as i32) as u32)
            .wrapping_add(smulbb(x[i + 1] as i32, x[i + 1] as i32) as u32);
        nrg = (nrg as u32).wrapping_add(t >> shft) as i32;
        i += 2;
    }
    if i < n {
        let t = smulbb(x[i] as i32, x[i] as i32) as u32;
        nrg = (nrg as u32).wrapping_add(t >> shft) as i32;
    }
    (nrg, shft)
}
