//! SILK NLSF decoding and LPC helpers (NLSF -> LPC conversion, stability
//! check, bandwidth expansion, analysis filter).
//!
//! Ported from libopus `silk/NLSF_decode.c`, `silk/NLSF_unpack.c`,
//! `silk/NLSF_stabilize.c`, `silk/NLSF2A.c`, `silk/LPC_fit.c`,
//! `silk/LPC_inv_pred_gain.c`, `silk/bwexpander.c`, `silk/bwexpander_32.c`,
//! `silk/LPC_analysis_filter.c` and `silk/sort.c`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::macros::*;
use super::tables::*;

pub const MAX_LPC_ORDER: usize = 16;
const QA: i32 = 16;
const A_LIMIT_QA24: i32 = 16773022; // SILK_FIX_CONST(0.99975, 24)
const MIN_INV_GAIN_Q30: i32 = 107374; // SILK_FIX_CONST(1/MAX_PREDICTION_POWER_GAIN, 30)

/// `silk_NLSF_unpack`
pub fn nlsf_unpack(
    ec_ix: &mut [i16; MAX_LPC_ORDER],
    pred_q8: &mut [u8; MAX_LPC_ORDER],
    cb: &NlsfCb,
    cb1_index: usize,
) {
    let order = cb.order;
    let sel = &cb.ec_sel[cb1_index * order / 2..];
    let mut i = 0;
    while i < order {
        let entry = sel[i / 2];
        ec_ix[i] = smulbb(((entry >> 1) & 7) as i32, 9) as i16;
        pred_q8[i] = cb.pred_q8[i + (entry & 1) as usize * (order - 1)];
        ec_ix[i + 1] = smulbb(((entry >> 5) & 7) as i32, 9) as i16;
        pred_q8[i + 1] = cb.pred_q8[i + ((entry >> 4) & 1) as usize * (order - 1) + 1];
        i += 2;
    }
}

/// `silk_NLSF_decode`
pub fn nlsf_decode(
    nlsf_q15: &mut [i16; MAX_LPC_ORDER],
    indices: &[i8; MAX_LPC_ORDER + 1],
    cb: &NlsfCb,
) {
    let mut ec_ix = [0i16; MAX_LPC_ORDER];
    let mut pred_q8 = [0u8; MAX_LPC_ORDER];
    let cb1 = (indices[0] as usize).min(cb.n_vectors - 1);
    nlsf_unpack(&mut ec_ix, &mut pred_q8, cb, cb1);
    let order = cb.order;
    // Predictive residual dequantizer
    let mut res_q10 = [0i16; MAX_LPC_ORDER];
    let mut out_q10: i32 = 0;
    for i in (0..order).rev() {
        let pred_q10 = smulbb(out_q10, pred_q8[i] as i16 as i32) >> 8;
        out_q10 = lshift(indices[i + 1] as i32, 10);
        if out_q10 > 0 {
            out_q10 -= 102;
        } else if out_q10 < 0 {
            out_q10 += 102;
        }
        out_q10 = smlawb(pred_q10, out_q10, cb.quant_step_size_q16);
        res_q10[i] = out_q10 as i16;
    }
    let cb_el = &cb.cb1_nlsf_q8[cb1 * order..];
    let cb_w = &cb.cb1_wght_q9[cb1 * order..];
    for i in 0..order {
        let tmp = add_lshift32(
            div32(lshift(res_q10[i] as i32, 14), cb_w[i] as i32),
            cb_el[i] as i16 as i32,
            7,
        );
        nlsf_q15[i] = limit(tmp, 0, 32767) as i16;
    }
    nlsf_stabilize(&mut nlsf_q15[..order], cb.delta_min_q15);
}

/// `silk_NLSF_stabilize`
pub fn nlsf_stabilize(nlsf: &mut [i16], delta_min: &[i16]) {
    let l = nlsf.len();
    const MAX_LOOPS: usize = 20;
    let mut loops = 0;
    while loops < MAX_LOOPS {
        let mut min_diff = nlsf[0] as i32 - delta_min[0] as i32;
        let mut ii = 0usize;
        for i in 1..l {
            let diff = nlsf[i] as i32 - (nlsf[i - 1] as i32 + delta_min[i] as i32);
            if diff < min_diff {
                min_diff = diff;
                ii = i;
            }
        }
        let diff = (1 << 15) - (nlsf[l - 1] as i32 + delta_min[l] as i32);
        if diff < min_diff {
            min_diff = diff;
            ii = l;
        }
        if min_diff >= 0 {
            return;
        }
        if ii == 0 {
            nlsf[0] = delta_min[0];
        } else if ii == l {
            nlsf[l - 1] = ((1 << 15) - delta_min[l] as i32) as i16;
        } else {
            let mut min_center: i32 = 0;
            for k in 0..ii {
                min_center += delta_min[k] as i32;
            }
            min_center += (delta_min[ii] >> 1) as i32;
            let mut max_center: i32 = 1 << 15;
            for k in (ii + 1..=l).rev() {
                max_center -= delta_min[k] as i32;
            }
            max_center -= (delta_min[ii] >> 1) as i32;
            let center = limit(
                rshift_round(nlsf[ii - 1] as i32 + nlsf[ii] as i32, 1),
                min_center,
                max_center,
            ) as i16;
            nlsf[ii - 1] = (center as i32 - (delta_min[ii] >> 1) as i32) as i16;
            nlsf[ii] = (nlsf[ii - 1] as i32 + delta_min[ii] as i32) as i16;
        }
        loops += 1;
    }
    // Fall back: insertion sort and clamping.
    for i in 1..l {
        let value = nlsf[i];
        let mut j = i as isize - 1;
        while j >= 0 && value < nlsf[j as usize] {
            nlsf[j as usize + 1] = nlsf[j as usize];
            j -= 1;
        }
        nlsf[(j + 1) as usize] = value;
    }
    nlsf[0] = nlsf[0].max(delta_min[0]);
    for i in 1..l {
        nlsf[i] = (nlsf[i] as i32).max(add_sat16(nlsf[i - 1] as i32, delta_min[i] as i32)) as i16;
    }
    nlsf[l - 1] = (nlsf[l - 1] as i32).min((1 << 15) - delta_min[l] as i32) as i16;
    for i in (0..l - 1).rev() {
        nlsf[i] = (nlsf[i] as i32).min(nlsf[i + 1] as i32 - delta_min[i + 1] as i32) as i16;
    }
}

fn nlsf2a_find_poly(out: &mut [i32], c_lsf: &[i32], dd: usize) {
    out[0] = lshift(1, QA);
    out[1] = -c_lsf[0];
    for k in 1..dd {
        let ftmp = c_lsf[2 * k];
        out[k + 1] =
            lshift(out[k - 1], 1).wrapping_sub(rshift_round64(smull(ftmp, out[k]), QA) as i32);
        let mut n = k;
        while n > 1 {
            out[n] = out[n].wrapping_add(
                out[n - 2].wrapping_sub(rshift_round64(smull(ftmp, out[n - 1]), QA) as i32),
            );
            n -= 1;
        }
        out[1] = out[1].wrapping_sub(ftmp);
    }
}

/// `silk_NLSF2A`
pub fn nlsf2a(a_q12: &mut [i16], nlsf: &[i16], d: usize) {
    const ORDERING16: [usize; 16] = [0, 15, 8, 7, 4, 11, 12, 3, 2, 13, 10, 5, 6, 9, 14, 1];
    const ORDERING10: [usize; 10] = [0, 9, 6, 3, 4, 5, 8, 1, 2, 7];
    let ordering: &[usize] = if d == 16 { &ORDERING16 } else { &ORDERING10 };
    let mut cos_lsf_qa = [0i32; MAX_LPC_ORDER];
    for k in 0..d {
        let n = (nlsf[k] as i32).clamp(0, 32767);
        let f_int = n >> (15 - 7);
        let f_frac = n - lshift(f_int, 15 - 7);
        let cos_val = SILK_LSFCOSTAB_FIX_Q12[f_int as usize] as i32;
        let delta = SILK_LSFCOSTAB_FIX_Q12[f_int as usize + 1] as i32 - cos_val;
        cos_lsf_qa[ordering[k]] = rshift_round(lshift(cos_val, 8) + delta * f_frac, 20 - QA);
    }
    let dd = d >> 1;
    let mut p = [0i32; MAX_LPC_ORDER / 2 + 1];
    let mut q = [0i32; MAX_LPC_ORDER / 2 + 1];
    nlsf2a_find_poly(&mut p, &cos_lsf_qa[..], dd);
    nlsf2a_find_poly(&mut q, &cos_lsf_qa[1..], dd);
    let mut a32_qa1 = [0i32; MAX_LPC_ORDER];
    for k in 0..dd {
        let ptmp = p[k + 1].wrapping_add(p[k]);
        let qtmp = q[k + 1].wrapping_sub(q[k]);
        a32_qa1[k] = qtmp.wrapping_neg().wrapping_sub(ptmp);
        a32_qa1[d - k - 1] = qtmp.wrapping_sub(ptmp);
    }
    lpc_fit(a_q12, &mut a32_qa1[..d], 12, QA + 1);
    let mut i = 0;
    while lpc_inverse_pred_gain(&a_q12[..d]) == 0 && i < 16 {
        bwexpander_32(&mut a32_qa1[..d], 65536 - lshift(2, i));
        for k in 0..d {
            a_q12[k] = rshift_round(a32_qa1[k], QA + 1 - 12) as i16;
        }
        i += 1;
    }
}

/// `silk_LPC_fit`
fn lpc_fit(a_qout: &mut [i16], a_qin: &mut [i32], qout: i32, qin: i32) {
    let d = a_qin.len();
    let mut idx = 0usize;
    let mut i = 0;
    while i < 10 {
        let mut maxabs = 0i32;
        for (k, &v) in a_qin.iter().enumerate() {
            let absval = abs(v);
            if absval > maxabs {
                maxabs = absval;
                idx = k;
            }
        }
        maxabs = rshift_round(maxabs, qin - qout);
        if maxabs > i16::MAX as i32 {
            maxabs = maxabs.min(163838);
            let chirp_q16 = 65470
                - div32(
                    lshift(maxabs - i16::MAX as i32, 14),
                    mul(maxabs, idx as i32 + 1) >> 2,
                );
            bwexpander_32(a_qin, chirp_q16);
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..d {
            a_qout[k] = sat16(rshift_round(a_qin[k], qin - qout)) as i16;
            a_qin[k] = lshift(a_qout[k] as i32, qin - qout);
        }
    } else {
        for k in 0..d {
            a_qout[k] = rshift_round(a_qin[k], qin - qout) as i16;
        }
    }
}

fn mul32_frac_q(a: i32, b: i32, q: i32) -> i32 {
    rshift_round64(smull(a, b), q) as i32
}

fn lpc_inverse_pred_gain_qa(a_qa: &mut [i32; MAX_LPC_ORDER], order: usize) -> i32 {
    let mut inv_gain_q30: i32 = 1 << 30;
    let mut k = order - 1;
    while k > 0 {
        if a_qa[k] > A_LIMIT_QA24 || a_qa[k] < -A_LIMIT_QA24 {
            return 0;
        }
        let rc_q31 = lshift(a_qa[k], 31 - 24).wrapping_neg();
        let rc_mult1_q30 = (1i32 << 30).wrapping_sub(smmul(rc_q31, rc_q31));
        inv_gain_q30 = lshift(smmul(inv_gain_q30, rc_mult1_q30), 2);
        if inv_gain_q30 < MIN_INV_GAIN_Q30 {
            return 0;
        }
        let mult2q = 32 - clz32(abs(rc_mult1_q30));
        let rc_mult2 = inverse32_varq(rc_mult1_q30, mult2q + 30);
        for n in 0..(k + 1) >> 1 {
            let tmp1 = a_qa[n];
            let tmp2 = a_qa[k - n - 1];
            let t64 = rshift_round64(
                smull(sub_sat32(tmp1, mul32_frac_q(tmp2, rc_q31, 31)), rc_mult2),
                mult2q,
            );
            if t64 > i32::MAX as i64 || t64 < i32::MIN as i64 {
                return 0;
            }
            a_qa[n] = t64 as i32;
            let t64 = rshift_round64(
                smull(sub_sat32(tmp2, mul32_frac_q(tmp1, rc_q31, 31)), rc_mult2),
                mult2q,
            );
            if t64 > i32::MAX as i64 || t64 < i32::MIN as i64 {
                return 0;
            }
            a_qa[k - n - 1] = t64 as i32;
        }
        k -= 1;
    }
    if a_qa[0] > A_LIMIT_QA24 || a_qa[0] < -A_LIMIT_QA24 {
        return 0;
    }
    let rc_q31 = lshift(a_qa[0], 31 - 24).wrapping_neg();
    let rc_mult1_q30 = (1i32 << 30).wrapping_sub(smmul(rc_q31, rc_q31));
    inv_gain_q30 = lshift(smmul(inv_gain_q30, rc_mult1_q30), 2);
    if inv_gain_q30 < MIN_INV_GAIN_Q30 {
        return 0;
    }
    inv_gain_q30
}

/// `silk_LPC_inverse_pred_gain` (Q12 input). Returns 0 if unstable.
pub fn lpc_inverse_pred_gain(a_q12: &[i16]) -> i32 {
    let order = a_q12.len();
    let mut atmp = [0i32; MAX_LPC_ORDER];
    let mut dc_resp: i32 = 0;
    for k in 0..order {
        dc_resp += a_q12[k] as i32;
        atmp[k] = lshift(a_q12[k] as i32, 24 - 12);
    }
    if dc_resp >= 4096 {
        return 0;
    }
    lpc_inverse_pred_gain_qa(&mut atmp, order)
}

/// `silk_bwexpander` (16-bit coefficients)
pub fn bwexpander(ar: &mut [i16], mut chirp_q16: i32) {
    let d = ar.len();
    let chirp_minus_one_q16 = chirp_q16 - 65536;
    for v in ar[..d - 1].iter_mut() {
        *v = rshift_round(mul(chirp_q16, *v as i32), 16) as i16;
        chirp_q16 += rshift_round(mul(chirp_q16, chirp_minus_one_q16), 16);
    }
    ar[d - 1] = rshift_round(mul(chirp_q16, ar[d - 1] as i32), 16) as i16;
}

/// `silk_bwexpander_32`
pub fn bwexpander_32(ar: &mut [i32], mut chirp_q16: i32) {
    let d = ar.len();
    let chirp_minus_one_q16 = chirp_q16 - 65536;
    for v in ar[..d - 1].iter_mut() {
        *v = smulww(chirp_q16, *v);
        chirp_q16 += rshift_round(mul(chirp_q16, chirp_minus_one_q16), 16);
    }
    ar[d - 1] = smulww(chirp_q16, ar[d - 1]);
}

/// `silk_LPC_analysis_filter`: `out[ix] = in[ix] - sum(B[j]*in[ix-1-j])`.
pub fn lpc_analysis_filter(out: &mut [i16], input: &[i16], b: &[i16], len: usize, d: usize) {
    for ix in d..len {
        let base = ix - 1;
        let mut acc = smulbb(input[base] as i32, b[0] as i32);
        for j in 1..d {
            acc = acc.wrapping_add(smulbb(input[base - j] as i32, b[j] as i32));
        }
        let o = lshift(input[base + 1] as i32, 12).wrapping_sub(acc);
        out[ix] = sat16(rshift_round(o, 12)) as i16;
    }
    for v in out[..d].iter_mut() {
        *v = 0;
    }
}
