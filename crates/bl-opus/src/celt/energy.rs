//! CELT band energy coding (coarse, fine and final refinement), decoder and
//! encoder side.
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
use crate::range_enc::RangeEncoder;

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

/// One pass of coarse energy encoding (`quant_coarse_energy_impl`).
/// Returns the "badness" (sum of the clamping applied to the residuals).
#[allow(clippy::too_many_arguments)]
fn quant_coarse_energy_impl(
    start: usize,
    end: usize,
    e_bands: &[f32],
    old_ebands: &mut [f32],
    budget: i32,
    tell: i32,
    error: &mut [f32],
    enc: &mut RangeEncoder,
    c_count: usize,
    lm: usize,
    intra: bool,
    max_decay: f32,
) -> i32 {
    let prob_model = &E_PROB_MODEL[lm][intra as usize];
    let mut badness = 0;
    let mut prev = [0f32; 2];
    if tell + 3 <= budget {
        enc.bit_logp(intra, 3);
    }
    let (coef, beta) = if intra {
        (0.0, BETA_INTRA)
    } else {
        (PRED_COEF[lm], BETA_COEF[lm])
    };
    for i in start..end {
        for c in 0..c_count {
            let idx = i + c * NB_EBANDS;
            let x = e_bands[idx];
            let old_e = old_ebands[idx].max(-9.0);
            let f = x - coef * old_e - prev[c];
            // Rounding to nearest integer here is really important.
            let mut qi = (0.5 + f).floor() as i32;
            let decay_bound = old_ebands[idx].max(-28.0) - max_decay;
            // Keep the energy from going down too quickly.
            if qi < 0 && x < decay_bound {
                qi += (decay_bound - x) as i32;
                if qi > 0 {
                    qi = 0;
                }
            }
            let qi0 = qi;
            // Not enough bits for all the energy: assume something safe.
            let tell = enc.tell();
            let bits_left = budget - tell - 3 * c_count as i32 * (end - i) as i32;
            if i != start && bits_left < 30 {
                if bits_left < 24 {
                    qi = qi.min(1);
                }
                if bits_left < 16 {
                    qi = qi.max(-1);
                }
            }
            if budget - tell >= 15 {
                let pi = 2 * i.min(20);
                qi = enc.laplace(
                    qi,
                    (prob_model[pi] as u32) << 7,
                    (prob_model[pi + 1] as i32) << 6,
                );
            } else if budget - tell >= 2 {
                qi = qi.clamp(-1, 1);
                enc.icdf(
                    (2 * qi ^ -((qi < 0) as i32)) as usize,
                    &SMALL_ENERGY_ICDF,
                    2,
                );
            } else if budget - tell >= 1 {
                qi = qi.min(0);
                enc.bit_logp(qi != 0, 1);
            } else {
                qi = -1;
            }
            error[idx] = f - qi as f32;
            badness += (qi0 - qi).abs();
            let q = qi as f32;
            // Same operations as the decoder so both stay in sync.
            old_ebands[idx] = old_e;
            let tmp = coef * old_ebands[idx] + prev[c] + q;
            old_ebands[idx] = tmp;
            prev[c] = prev[c] + q - beta * q;
        }
    }
    badness
}

/// Coarse energy encoding with the intra / inter decision
/// (`quant_coarse_energy`, two-pass). `delayed_intra` is encoder state.
#[allow(clippy::too_many_arguments)]
pub fn quant_coarse_energy(
    start: usize,
    end: usize,
    eff_end: usize,
    e_bands: &[f32],
    old_ebands: &mut [f32],
    budget: i32,
    error: &mut [f32],
    enc: &mut RangeEncoder,
    c_count: usize,
    lm: usize,
    nb_available_bytes: i32,
    force_intra: bool,
    delayed_intra: &mut f32,
) {
    // Always two-pass (libopus complexity >= 4): intra is only forced.
    let mut two_pass = true;
    let mut intra = force_intra;
    let mut new_distortion = 0f32;
    for c in 0..c_count {
        for i in start..eff_end {
            let d = e_bands[i + c * NB_EBANDS] - old_ebands[i + c * NB_EBANDS];
            new_distortion += d * d;
        }
    }
    let new_distortion = new_distortion.min(200.0);
    let tell = enc.tell();
    if tell + 3 > budget {
        two_pass = false;
        intra = false;
    }
    let mut max_decay = 16.0f32;
    if end - start > 10 {
        max_decay = max_decay.min(0.125 * nb_available_bytes as f32);
    }
    let start_state = enc.clone();
    let n = c_count * NB_EBANDS;
    let mut old_intra = [0f32; 2 * NB_EBANDS];
    let mut error_intra = [0f32; 2 * NB_EBANDS];
    old_intra[..n].copy_from_slice(&old_ebands[..n]);
    let mut badness1 = 0;
    if two_pass || intra {
        badness1 = quant_coarse_energy_impl(
            start,
            end,
            e_bands,
            &mut old_intra,
            budget,
            tell,
            &mut error_intra,
            enc,
            c_count,
            lm,
            true,
            max_decay,
        );
    }
    if !intra {
        let tell_intra = enc.tell_frac() as i32;
        let intra_state = std::mem::replace(enc, start_state);
        let badness2 = quant_coarse_energy_impl(
            start, end, e_bands, old_ebands, budget, tell, error, enc, c_count, lm, false,
            max_decay,
        );
        if two_pass
            && (badness1 < badness2
                || (badness1 == badness2 && enc.tell_frac() as i32 > tell_intra))
        {
            *enc = intra_state;
            old_ebands[..n].copy_from_slice(&old_intra[..n]);
            error[..n].copy_from_slice(&error_intra[..n]);
            intra = true;
        }
    } else {
        old_ebands[..n].copy_from_slice(&old_intra[..n]);
        error[..n].copy_from_slice(&error_intra[..n]);
    }
    if intra {
        *delayed_intra = new_distortion;
    } else {
        *delayed_intra = PRED_COEF[lm] * PRED_COEF[lm] * *delayed_intra + new_distortion;
    }
}

/// Fine energy encoding (`quant_fine_energy`).
pub fn quant_fine_energy(
    start: usize,
    end: usize,
    old_ebands: &mut [f32],
    error: &mut [f32],
    fine_quant: &[i32],
    enc: &mut RangeEncoder,
    c_count: usize,
) {
    for i in start..end {
        let fq = fine_quant[i];
        if fq <= 0 {
            continue;
        }
        let frac = 1i32 << fq;
        for c in 0..c_count {
            let idx = i + c * NB_EBANDS;
            let q2 = (((error[idx] + 0.5) * frac as f32).floor() as i32).clamp(0, frac - 1);
            enc.bits(q2 as u32, fq as u32);
            let offset = (q2 as f32 + 0.5) * ((1i32 << (14 - fq)) as f32) * (1.0 / 16384.0) - 0.5;
            old_ebands[idx] += offset;
            error[idx] -= offset;
        }
    }
}

/// Spends the leftover bits on energy refinement (`quant_energy_finalise`).
#[allow(clippy::too_many_arguments)]
pub fn quant_energy_finalise(
    start: usize,
    end: usize,
    old_ebands: &mut [f32],
    error: &mut [f32],
    fine_quant: &[i32],
    fine_priority: &[i32],
    mut bits_left: i32,
    enc: &mut RangeEncoder,
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
                let idx = i + c * NB_EBANDS;
                let q2 = (error[idx] >= 0.0) as u32;
                enc.bits(q2, 1);
                let offset = (q2 as f32 - 0.5)
                    * ((1i32 << (14 - fine_quant[i] - 1)) as f32)
                    * (1.0 / 16384.0);
                old_ebands[idx] += offset;
                error[idx] -= offset;
                bits_left -= 1;
            }
            i += 1;
        }
    }
}
