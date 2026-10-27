//! CELT band energy decoding (coarse, fine and final refinement).
//!
//! Ported from libopus `celt/quant_bands.c`:
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
use crate::range_dec::RangeDecoder;

pub fn unquant_coarse_energy(
    start: usize,
    end: usize,
    old_ebands: &mut [f32],
    intra: bool,
    dec: &mut RangeDecoder,
    c_count: usize,
    lm: usize,
) {
    let prob_model = &E_PROB_MODEL[lm][intra as usize];
    let mut prev = [0f32; 2];
    let (coef, beta) = if intra {
        (0.0, BETA_INTRA)
    } else {
        (PRED_COEF[lm], BETA_COEF[lm])
    };
    let budget = dec.storage as i32 * 8;
    for i in start..end {
        for c in 0..c_count {
            let tell = dec.tell();
            let qi: i32 = if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                dec.laplace(
                    (prob_model[pi] as u32) << 7,
                    (prob_model[pi + 1] as i32) << 6,
                )
            } else if budget - tell >= 2 {
                let qi = dec.icdf(&SMALL_ENERGY_ICDF, 2) as i32;
                (qi >> 1) ^ -(qi & 1)
            } else if budget - tell >= 1 {
                -(dec.bit_logp(1) as i32)
            } else {
                -1
            };
            let q = qi as f32;
            let idx = i + c * NB_EBANDS;
            old_ebands[idx] = old_ebands[idx].max(-9.0);
            let tmp = coef * old_ebands[idx] + prev[c] + q;
            old_ebands[idx] = tmp;
            prev[c] = prev[c] + q - beta * q;
        }
    }
}

pub fn unquant_fine_energy(
    start: usize,
    end: usize,
    old_ebands: &mut [f32],
    fine_quant: &[i32],
    dec: &mut RangeDecoder,
    c_count: usize,
) {
    for i in start..end {
        let fq = fine_quant[i];
        if fq <= 0 {
            continue;
        }
        for c in 0..c_count {
            let q2 = dec.bits(fq as u32);
            let offset = (q2 as f32 + 0.5) * ((1i32 << (14 - fq)) as f32) * (1.0 / 16384.0) - 0.5;
            old_ebands[i + c * NB_EBANDS] += offset;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn unquant_energy_finalise(
    start: usize,
    end: usize,
    old_ebands: &mut [f32],
    fine_quant: &[i32],
    fine_priority: &[i32],
    mut bits_left: i32,
    dec: &mut RangeDecoder,
    c_count: usize,
) {
    for prio in 0..2 {
        let mut i = start;
        while i < end && bits_left >= c_count as i32 {
            if fine_quant[i] >= MAX_FINE_BITS || fine_priority[i] != prio {
                i += 1;
                continue;
            }
            for c in 0..c_count {
                let q2 = dec.bits(1);
                let offset = (q2 as f32 - 0.5)
                    * ((1i32 << (14 - fine_quant[i] - 1)) as f32)
                    * (1.0 / 16384.0);
                old_ebands[i + c * NB_EBANDS] += offset;
                bits_left -= 1;
            }
            i += 1;
        }
    }
}
