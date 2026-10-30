//! CELT encoder for 20 ms fullband frames at 48 kHz (mono or stereo):
//! pre-emphasis, transient detection with short blocks, forward MDCT, band
//! energies, coarse (two-pass intra/inter) and fine energy quantisation,
//! dynamic allocation boosts, spreading and allocation trim analysis,
//! stereo decisions (intensity / dual stereo / mid-side), bit allocation,
//! PVQ band shapes, anti-collapse and energy finalisation.
//!
//! Not implemented: the pitch pre-filter (signalled as off), TF resolution
//! analysis (fixed TF choice per frame type), theta rate-distortion
//! optimisation and VBR.
//!
//! Ported from libopus `celt/celt_encoder.c` (float build):
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2010 Xiph.Org Foundation
//! Copyright (c) 2008 Gregory Maxwell
//! Written by Jean-Marc Valin and Gregory Maxwell
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::enc_bands::{quant_all_bands_enc, EncBandsParams};
use super::energy::{quant_coarse_energy, quant_energy_finalise, quant_fine_energy};
use super::math::inner_prod;
use super::mdct::{mdct_forward, Cpx};
use super::mode::*;
use super::rate::{compute_allocation, AllocEncoder};
use crate::range_dec::BITRES;
use crate::range_enc::RangeEncoder;

/// Frame size in samples per channel.
pub const FRAME_SIZE: usize = 960;
const LM: usize = 3;
const N: usize = FRAME_SIZE;
const SPREAD_LIGHT: i32 = 1;

pub struct CeltEncoder {
    channels: usize,
    /// Pre-emphasised input: per channel the overlap of the previous frame
    /// followed by the current frame.
    inbuf: Vec<f32>,
    preemph_mem: [f32; 2],
    overlap_max: f32,
    old_band_e: [f32; 2 * NB_EBANDS],
    delayed_intra: f32,
    last_coded_bands: usize,
    intensity: usize,
    spread_decision: i32,
    tonal_average: i32,
    consec_transient: i32,
    stereo_saving: f32,
    /// Enables transient detection and short blocks.
    pub transients: bool,
    /// Final range coder state of the last frame.
    pub rng: u32,

    // Scratch buffers (no state).
    freq: Vec<f32>,
    x: Vec<f32>,
    fft: Vec<Cpx>,
    fold: Vec<f32>,
}

impl CeltEncoder {
    pub fn new(channels: usize) -> Option<Self> {
        if !(1..=2).contains(&channels) {
            return None;
        }
        let mut e = CeltEncoder {
            channels,
            inbuf: vec![0.0; channels * (N + OVERLAP)],
            preemph_mem: [0.0; 2],
            overlap_max: 0.0,
            old_band_e: [0.0; 2 * NB_EBANDS],
            delayed_intra: 1.0,
            last_coded_bands: 0,
            intensity: 0,
            spread_decision: SPREAD_NORMAL,
            tonal_average: 256,
            consec_transient: 0,
            stereo_saving: 0.0,
            transients: true,
            rng: 0,
            freq: vec![0.0; channels * N],
            x: vec![0.0; channels * N],
            fft: Vec::with_capacity(480),
            fold: Vec::with_capacity(960),
        };
        e.reset();
        Some(e)
    }

    pub fn reset(&mut self) {
        self.inbuf.fill(0.0);
        self.preemph_mem = [0.0; 2];
        self.overlap_max = 0.0;
        self.old_band_e = [0.0; 2 * NB_EBANDS];
        self.delayed_intra = 1.0;
        self.last_coded_bands = 0;
        self.intensity = 0;
        self.spread_decision = SPREAD_NORMAL;
        self.tonal_average = 256;
        self.consec_transient = 0;
        self.stereo_saving = 0.0;
    }

    /// Encodes one frame of `FRAME_SIZE` interleaved samples (nominally in
    /// [-1, 1]) into a CELT frame of `nb_bytes` bytes (2..=1275). Digital
    /// silence is coded as a 2-byte frame.
    pub fn encode(&mut self, pcm: &[f32], nb_bytes: usize) -> Vec<u8> {
        let cc = self.channels;
        let c_count = cc;
        let start = 0usize;
        let end = NB_EBANDS;
        let eff_end = EFF_EBANDS;
        let m = 1usize << LM;
        let stride = N + OVERLAP;
        let nb_bytes = nb_bytes.clamp(2, 1275);

        // Pre-emphasis (CELT works on a 16-bit signal scale).
        let mut sample_max = self.overlap_max;
        for c in 0..cc {
            let buf = &mut self.inbuf[c * stride..(c + 1) * stride];
            buf.copy_within(N..N + OVERLAP, 0);
            let mut mem = self.preemph_mem[c];
            for i in 0..N {
                let v = pcm[i * cc + c];
                sample_max = sample_max.max(v.abs());
                let x = v * 32768.0;
                buf[OVERLAP + i] = x - mem;
                mem = PREEMPH0 * x;
            }
            self.preemph_mem[c] = mem;
        }
        self.overlap_max = pcm[(N - OVERLAP) * cc..N * cc]
            .iter()
            .fold(0f32, |a, &v| a.max(v.abs()));
        // Silence below the resolution of 24-bit audio.
        let mut silence = sample_max <= 1.0 / (1 << 24) as f32;

        let mut enc = RangeEncoder::new(nb_bytes);
        let mut nb_bytes = nb_bytes;
        let mut total_bits = nb_bytes as i32 * 8;
        let mut tell = enc.tell();
        if tell == 1 {
            enc.bit_logp(silence, 15);
        } else {
            silence = false;
        }
        if silence {
            // No need to send more than the minimum.
            nb_bytes = 2;
            total_bits = 16;
            enc.shrink(2);
            // Pretend all remaining bits were used (as the decoder does).
            tell = nb_bytes as i32 * 8;
            enc.nbits_total += tell - enc.tell();
        }

        // Pitch pre-filter: always off.
        if start == 0 && tell + 16 <= total_bits {
            enc.bit_logp(false, 1);
            tell = enc.tell();
        }

        let mut tf_estimate = 0f32;
        let mut is_transient = false;
        if self.transients && !silence {
            let (t, est) = transient_analysis(&self.inbuf, stride, c_count);
            is_transient = t;
            tf_estimate = est;
        }
        if LM > 0 && tell + 3 <= total_bits {
            enc.bit_logp(is_transient, 3);
        } else {
            is_transient = false;
        }
        let short_blocks = is_transient;

        // Forward MDCTs.
        for c in 0..cc {
            let inp = &self.inbuf[c * stride..(c + 1) * stride];
            let out = &mut self.freq[c * N..(c + 1) * N];
            if short_blocks {
                for b in 0..m {
                    let s = b * SHORT_MDCT_SIZE;
                    mdct_forward(
                        &inp[s..s + SHORT_MDCT_SIZE + OVERLAP],
                        &mut out[b..],
                        &WINDOW120,
                        OVERLAP,
                        MAX_LM,
                        m,
                        &mut self.fft,
                        &mut self.fold,
                    );
                }
            } else {
                mdct_forward(
                    inp,
                    out,
                    &WINDOW120,
                    OVERLAP,
                    0,
                    1,
                    &mut self.fft,
                    &mut self.fold,
                );
            }
        }

        // Band energies (linear and log2 relative to the band means) and
        // the normalised band shapes.
        let mut band_e = [0f32; 2 * NB_EBANDS];
        let mut band_log_e = [-14f32; 2 * NB_EBANDS];
        for c in 0..c_count {
            for i in 0..eff_end {
                let a = c * N + m * EBANDS[i] as usize;
                let b = c * N + m * EBANDS[i + 1] as usize;
                let sum = 1e-27 + inner_prod(&self.freq[a..b], &self.freq[a..b], b - a);
                let e = sum.sqrt();
                band_e[i + c * NB_EBANDS] = e;
                band_log_e[i + c * NB_EBANDS] = std::f32::consts::LOG2_E * e.ln() - E_MEANS[i];
                let g = 1.0 / (1e-27 + e);
                for j in a..b {
                    self.x[j] = self.freq[j] * g;
                }
            }
            let tail = c * N + m * EBANDS[eff_end] as usize;
            self.x[tail..(c + 1) * N].fill(0.0);
        }

        // Energies of a long MDCT for the allocation analysis of transient
        // frames (compensated for the scaling of short vs long MDCTs).
        let mut band_log_e2 = band_log_e;
        if short_blocks {
            for c in 0..cc {
                let inp = &self.inbuf[c * stride..(c + 1) * stride];
                let out = &mut self.freq[c * N..(c + 1) * N];
                mdct_forward(
                    inp,
                    out,
                    &WINDOW120,
                    OVERLAP,
                    0,
                    1,
                    &mut self.fft,
                    &mut self.fold,
                );
                for i in 0..eff_end {
                    let a = m * EBANDS[i] as usize;
                    let b = m * EBANDS[i + 1] as usize;
                    let e = (1e-27 + inner_prod(&out[a..b], &out[a..b], b - a)).sqrt();
                    band_log_e2[i + c * NB_EBANDS] =
                        std::f32::consts::LOG2_E * e.ln() - E_MEANS[i] + 0.5 * LM as f32;
                }
            }
        }
        let mut offsets = [0i32; NB_EBANDS];
        let mut spread_weight = [32i32; NB_EBANDS];
        dynalloc_analysis(
            &band_log_e,
            &band_log_e2,
            end,
            c_count,
            &mut offsets,
            &mut spread_weight,
            is_transient,
            nb_bytes as i32,
        );

        // Stereo decisions.
        let equiv_rate = nb_bytes as i32 * 8 * 50;
        let mut dual_stereo = false;
        if c_count == 2 {
            const INTENSITY_THRESHOLDS: [i32; 21] = [
                1, 2, 3, 4, 5, 6, 7, 8, 16, 24, 36, 44, 50, 56, 62, 67, 72, 79, 88, 106, 134,
            ];
            const INTENSITY_HYSTERESIS: [i32; 21] = [
                1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 3, 3, 4, 5, 6, 8, 8,
            ];
            dual_stereo = stereo_analysis(&self.x, LM);
            self.intensity = hysteresis_decision(
                equiv_rate / 1000,
                &INTENSITY_THRESHOLDS,
                &INTENSITY_HYSTERESIS,
                self.intensity,
            )
            .clamp(start, end);
        }

        // Coarse energy (the intra flag is coded inside).
        let mut error = [0f32; 2 * NB_EBANDS];
        quant_coarse_energy(
            start,
            end,
            eff_end,
            &band_log_e,
            &mut self.old_band_e,
            total_bits,
            &mut error,
            &mut enc,
            c_count,
            LM,
            nb_bytes as i32,
            false,
            &mut self.delayed_intra,
        );

        // TF resolution: keep short blocks as they are, long blocks at full
        // frequency resolution.
        let mut tf_res = [is_transient as i32; NB_EBANDS];
        tf_encode(start, end, is_transient, &mut tf_res, &mut enc);

        // Spreading.
        let spread = if enc.tell() + 4 <= total_bits {
            if short_blocks || nb_bytes < 10 * c_count {
                self.spread_decision = SPREAD_NORMAL;
            } else {
                self.spread_decision = spreading_decision(
                    &self.x,
                    &mut self.tonal_average,
                    self.spread_decision,
                    eff_end,
                    c_count,
                    m,
                    &spread_weight,
                );
            }
            enc.icdf(self.spread_decision as usize, &SPREAD_ICDF, 5);
            self.spread_decision
        } else {
            SPREAD_NORMAL
        };

        // Dynamic allocation boosts.
        let mut cap = [0i32; NB_EBANDS];
        init_caps(&mut cap, LM, c_count);
        let mut total_bits_frac = total_bits << BITRES;
        let mut tell_f = enc.tell_frac() as i32;
        let mut dynalloc_logp = 6i32;
        for i in start..end {
            let width = ((c_count as i32) * (EBANDS[i + 1] - EBANDS[i]) as i32) << LM;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            let mut j = 0;
            while tell_f + (loop_logp << BITRES) < total_bits_frac && boost < cap[i] {
                let flag = j < offsets[i];
                enc.bit_logp(flag, loop_logp as u32);
                tell_f = enc.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits_frac -= quanta;
                loop_logp = 1;
                j += 1;
            }
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
            offsets[i] = boost;
        }

        // Allocation trim.
        let mut alloc_trim = 5;
        if tell_f + (6 << BITRES) <= total_bits_frac {
            alloc_trim = alloc_trim_analysis(
                &self.x,
                &band_log_e,
                end,
                LM,
                c_count,
                &mut self.stereo_saving,
                tf_estimate,
                self.intensity,
                equiv_rate,
            );
            enc.icdf(alloc_trim as usize, &TRIM_ICDF, 7);
        }

        // Bit allocation.
        let mut bits = ((nb_bytes as i32 * 8) << BITRES) - enc.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient && LM >= 2 && bits >= ((LM as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;
        let alloc = {
            let mut coder = AllocEncoder {
                enc: &mut enc,
                intensity: self.intensity,
                dual_stereo,
                prev: self.last_coded_bands,
                signal_bandwidth: end - 1,
            };
            compute_allocation(
                start,
                end,
                &offsets,
                &cap,
                alloc_trim,
                bits,
                c_count as i32,
                LM as i32,
                &mut coder,
            )
        };
        self.last_coded_bands = if self.last_coded_bands != 0 {
            (self.last_coded_bands + 1).min(
                self.last_coded_bands
                    .saturating_sub(1)
                    .max(alloc.coded_bands),
            )
        } else {
            alloc.coded_bands
        };

        quant_fine_energy(
            start,
            end,
            &mut self.old_band_e,
            &mut error,
            &alloc.fine_quant,
            &mut enc,
            c_count,
        );

        // Band shapes.
        {
            let params = EncBandsParams {
                start,
                end,
                c: c_count,
                pulses: &alloc.pulses,
                short_blocks,
                spread,
                dual_stereo: alloc.dual_stereo,
                intensity: alloc.intensity,
                tf_res: &tf_res,
                total_bits: (nb_bytes as i32 * (8 << BITRES)) - anti_collapse_rsv,
                balance: alloc.balance,
                lm: LM,
                coded_bands: alloc.coded_bands,
                band_e: &band_e,
            };
            quant_all_bands_enc(&params, &mut self.x[..c_count * N], &mut enc);
        }

        if anti_collapse_rsv > 0 {
            let on = self.consec_transient < 2;
            enc.bits(on as u32, 1);
        }

        quant_energy_finalise(
            start,
            end,
            &mut self.old_band_e,
            &mut error,
            &alloc.fine_quant,
            &alloc.fine_priority,
            nb_bytes as i32 * 8 - enc.tell(),
            &mut enc,
            c_count,
        );

        if silence {
            self.old_band_e[..c_count * NB_EBANDS].fill(-28.0);
        }
        if is_transient {
            self.consec_transient += 1;
        } else {
            self.consec_transient = 0;
        }
        debug_assert!(!enc.error);
        self.rng = enc.rng;
        enc.done()
    }
}

fn hysteresis_decision(val: i32, thresholds: &[i32], hysteresis: &[i32], prev: usize) -> usize {
    let n = thresholds.len();
    let mut i = 0;
    while i < n && val >= thresholds[i] {
        i += 1;
    }
    let prev = prev.min(n - 1);
    if i > prev && val < thresholds[prev] + hysteresis[prev] {
        i = prev;
    }
    if i < prev && prev > 0 && val > thresholds[prev - 1] - hysteresis[prev - 1] {
        i = prev;
    }
    i
}

/// Chooses dual (L/R) stereo over mid/side when the L1 norm suggests L/R
/// is cheaper (`stereo_analysis`).
fn stereo_analysis(x: &[f32], lm: usize) -> bool {
    let n0 = N;
    let mut sum_lr = 1e-15f32;
    let mut sum_ms = 1e-15f32;
    for i in 0..13 {
        for j in (EBANDS[i] as usize) << lm..(EBANDS[i + 1] as usize) << lm {
            let l = x[j];
            let r = x[n0 + j];
            let m = l + r;
            let s = l - r;
            sum_lr += l.abs() + r.abs();
            sum_ms += m.abs() + s.abs();
        }
    }
    sum_ms *= 0.707_107;
    let mut thetas = 13;
    if lm <= 1 {
        thetas -= 8;
    }
    let e13 = (EBANDS[13] as i32) << (lm + 1);
    (e13 + thetas) as f32 * sum_ms > e13 as f32 * sum_lr
}

/// Detects transients from the high-passed temporal envelope
/// (`transient_analysis`). Returns the decision and the TF estimate.
fn transient_analysis(input: &[f32], stride: usize, c_count: usize) -> (bool, f32) {
    // Table of 6*64/x, trained on real data to minimise the average error.
    #[rustfmt::skip]
    const INV_TABLE: [u8; 128] = [
        255, 255, 156, 110,  86,  70,  59,  51,  45,  40,  37,  33,  31,  28,  26,  25,
         23,  22,  21,  20,  19,  18,  17,  16,  16,  15,  15,  14,  13,  13,  12,  12,
         12,  12,  11,  11,  11,  10,  10,  10,   9,   9,   9,   9,   9,   9,   8,   8,
          8,   8,   8,   7,   7,   7,   7,   7,   7,   6,   6,   6,   6,   6,   6,   6,
          6,   6,   6,   6,   6,   6,   6,   6,   6,   5,   5,   5,   5,   5,   5,   5,
          5,   5,   5,   5,   5,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,
          4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   4,   3,
          3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   3,   2,
    ];
    // Forward masking: 6.7 dB/ms.
    let forward_decay = 0.0625f32;
    let len = stride;
    let len2 = len / 2;
    let mut tmp = vec![0f32; len];
    let mut mask_metric = 0i32;
    for c in 0..c_count {
        let x = &input[c * stride..(c + 1) * stride];
        // High-pass filter: (1 - 2*z^-1 + z^-2) / (1 - z^-1 + .5*z^-2)
        let mut mem0 = 0f32;
        let mut mem1 = 0f32;
        for i in 0..len {
            let xi = x[i];
            let y = mem0 + xi;
            mem0 = mem1 + y - 2.0 * xi;
            mem1 = xi - 0.5 * y;
            tmp[i] = y;
        }
        // The first few samples are bad because the memory is not kept.
        tmp[..12].fill(0.0);
        // Forward pass (post-echo threshold), grouping samples by two.
        let mut mean = 0f32;
        mem0 = 0.0;
        for i in 0..len2 {
            let x2 = tmp[2 * i] * tmp[2 * i] + tmp[2 * i + 1] * tmp[2 * i + 1];
            mean += x2;
            mem0 = x2 + (1.0 - forward_decay) * mem0;
            tmp[i] = forward_decay * mem0;
        }
        // Backward pass (pre-echo threshold), 13.9 dB/ms.
        mem0 = 0.0;
        let mut max_e = 0f32;
        for i in (0..len2).rev() {
            mem0 = tmp[i] + 0.875 * mem0;
            tmp[i] = 0.125 * mem0;
            max_e = max_e.max(0.125 * mem0);
        }
        // Frame energy: geometric mean of the energy and half the max.
        let mean = (mean * max_e * 0.5 * len2 as f32).sqrt();
        let norm = len2 as f32 / (1e-15 + mean);
        if !norm.is_finite() || !tmp[0].is_finite() {
            continue;
        }
        // Harmonic mean, discarding the unreliable boundaries; the data is
        // smooth, so only every 4th sample is used.
        let mut unmask = 0i32;
        let mut i = 12;
        while i < len2 - 5 {
            let id = (64.0 * norm * (tmp[i] + 1e-15)).floor().clamp(0.0, 127.0) as usize;
            unmask += INV_TABLE[id] as i32;
            i += 4;
        }
        let unmask = 64 * unmask * 4 / (6 * (len2 as i32 - 17));
        mask_metric = mask_metric.max(unmask);
    }
    let is_transient = mask_metric > 200;
    let tf_max = ((27 * mask_metric) as f32).sqrt() - 42.0;
    let tf_max = tf_max.max(0.0);
    let tf_estimate = (0.0069 * tf_max.min(163.0) - 0.139).max(0.0).sqrt();
    (is_transient, tf_estimate)
}

fn tf_encode(
    start: usize,
    end: usize,
    is_transient: bool,
    tf_res: &mut [i32; NB_EBANDS],
    enc: &mut RangeEncoder,
) {
    let mut budget = enc.storage() * 8;
    let mut tell = enc.tell() as u32;
    let mut logp: u32 = if is_transient { 2 } else { 4 };
    // Reserve space for the tf_select decision.
    let tf_select_rsv = LM > 0 && tell + logp < budget;
    budget -= tf_select_rsv as u32;
    let mut curr = 0i32;
    let mut tf_changed = 0i32;
    for r in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp <= budget {
            enc.bit_logp((*r ^ curr) != 0, logp);
            tell = enc.tell() as u32;
            curr = *r;
            tf_changed |= curr;
        } else {
            *r = curr;
        }
        logp = if is_transient { 4 } else { 5 };
    }
    let it = 4 * is_transient as usize;
    let tf_select = 0usize;
    if tf_select_rsv
        && TF_SELECT_TABLE[LM][it + tf_changed as usize]
            != TF_SELECT_TABLE[LM][it + 2 + tf_changed as usize]
    {
        enc.bit_logp(tf_select != 0, 1);
    }
    for r in tf_res.iter_mut().take(end).skip(start) {
        *r = TF_SELECT_TABLE[LM][it + 2 * tf_select + *r as usize] as i32;
    }
}

/// Chooses the spreading (rotation) amount from how peaky the normalised
/// spectrum is (`spreading_decision`, without the HF tapset part).
fn spreading_decision(
    x: &[f32],
    average: &mut i32,
    last_decision: i32,
    end: usize,
    c_count: usize,
    m: usize,
    spread_weight: &[i32; NB_EBANDS],
) -> i32 {
    let n0 = m * SHORT_MDCT_SIZE;
    if m * (EBANDS[end] - EBANDS[end - 1]) as usize <= 8 {
        return SPREAD_NONE;
    }
    let mut sum = 0i32;
    let mut nb_bands = 0i32;
    for c in 0..c_count {
        for i in 0..end {
            let n = m * (EBANDS[i + 1] - EBANDS[i]) as usize;
            if n <= 8 {
                continue;
            }
            let xb = &x[m * EBANDS[i] as usize + c * n0..][..n];
            let mut tcount = [0i32; 3];
            for &v in xb {
                let x2n = v * v * n as f32;
                tcount[0] += (x2n < 0.25) as i32;
                tcount[1] += (x2n < 0.0625) as i32;
                tcount[2] += (x2n < 0.015625) as i32;
            }
            let n = n as i32;
            let tmp = (2 * tcount[2] >= n) as i32
                + (2 * tcount[1] >= n) as i32
                + (2 * tcount[0] >= n) as i32;
            sum += tmp * spread_weight[i];
            nb_bands += spread_weight[i];
        }
    }
    let mut sum = (sum << 8) / nb_bands.max(1);
    // Recursive averaging and hysteresis.
    sum = (sum + *average) >> 1;
    *average = sum;
    sum = (3 * sum + (((3 - last_decision) << 7) + 64) + 2) >> 2;
    if sum < 80 {
        SPREAD_AGGRESSIVE
    } else if sum < 256 {
        SPREAD_NORMAL
    } else if sum < 384 {
        SPREAD_LIGHT
    } else {
        SPREAD_NONE
    }
}

/// Chooses the allocation trim (tilt of the allocation towards low or high
/// bands) from the inter-channel correlation and the spectral tilt.
#[allow(clippy::too_many_arguments)]
fn alloc_trim_analysis(
    x: &[f32],
    band_log_e: &[f32],
    end: usize,
    lm: usize,
    c_count: usize,
    stereo_saving: &mut f32,
    tf_estimate: f32,
    intensity: usize,
    equiv_rate: i32,
) -> i32 {
    let n0 = N;
    // At low bitrate, reducing the trim seems to help.
    let mut trim = if equiv_rate < 64000 {
        4.0
    } else if equiv_rate < 80000 {
        4.0 + ((equiv_rate - 64000) >> 10) as f32 / 16.0
    } else {
        5.0f32
    };
    if c_count == 2 {
        let band = |i: usize| {
            let a = (EBANDS[i] as usize) << lm;
            let w = ((EBANDS[i + 1] - EBANDS[i]) as usize) << lm;
            inner_prod(&x[a..a + w], &x[n0 + a..n0 + a + w], w)
        };
        // Inter-channel correlation for low frequencies.
        let mut sum = 0f32;
        for i in 0..8 {
            sum += band(i);
        }
        sum = (sum / 8.0).abs().min(1.0);
        let mut min_xc = sum;
        for i in 8..intensity.min(end) {
            min_xc = min_xc.min(band(i).abs());
        }
        min_xc = min_xc.abs().min(1.0);
        // Mid/side savings estimates.
        let log_xc = (1.001 - sum * sum).log2();
        let log_xc2 = (0.5 * log_xc).max((1.001 - min_xc * min_xc).log2());
        trim += (-4.0f32).max(0.75 * log_xc);
        *stereo_saving = (*stereo_saving + 0.25).min(-0.5 * log_xc2);
    }
    // Spectral tilt.
    let mut diff = 0f32;
    for c in 0..c_count {
        for i in 0..end - 1 {
            diff += band_log_e[i + c * NB_EBANDS] * (2 + 2 * i as i32 - end as i32) as f32;
        }
    }
    diff /= (c_count * (end - 1)) as f32;
    trim -= ((diff + 1.0) / 6.0).clamp(-2.0, 2.0);
    trim -= 2.0 * tf_estimate;
    ((0.5 + trim).floor() as i32).clamp(0, 10)
}

fn median_of_5(x: &[f32]) -> f32 {
    let mut v = [x[0], x[1], x[2], x[3], x[4]];
    v.sort_by(|a, b| a.total_cmp(b));
    v[2]
}

fn median_of_3(x: &[f32]) -> f32 {
    let mut v = [x[0], x[1], x[2]];
    v.sort_by(|a, b| a.total_cmp(b));
    v[1]
}

/// Boosts bands that stick out of the smoothed spectral envelope (tonal
/// peaks) and computes the masking weights for the spreading decision
/// (`dynalloc_analysis`, CBR). `offsets` receives the number of boost
/// steps per band.
#[allow(clippy::too_many_arguments)]
fn dynalloc_analysis(
    band_log_e: &[f32; 2 * NB_EBANDS],
    band_log_e2: &[f32; 2 * NB_EBANDS],
    end: usize,
    c_count: usize,
    offsets: &mut [i32; NB_EBANDS],
    spread_weight: &mut [i32; NB_EBANDS],
    is_transient: bool,
    effective_bytes: i32,
) {
    const LSB_DEPTH: i32 = 24;
    offsets.fill(0);
    // The noise floor accounts for the band means, the depth, the band
    // widths and the pre-emphasis (approx. square of the bark band index).
    let mut noise_floor = [0f32; NB_EBANDS];
    for (i, nf) in noise_floor.iter_mut().enumerate().take(end) {
        *nf = 0.0625 * LOGN400[i] as f32 + 0.5 + (9 - LSB_DEPTH) as f32 - E_MEANS[i]
            + 0.0062 * ((i + 5) * (i + 5)) as f32;
    }
    let mut max_depth = -31.9f32;
    for c in 0..c_count {
        for i in 0..end {
            max_depth = max_depth.max(band_log_e[c * NB_EBANDS + i] - noise_floor[i]);
        }
    }
    // Simple masking model so that masked bands do not count in the
    // spreading decision.
    {
        let mut mask = [0f32; NB_EBANDS];
        for i in 0..end {
            mask[i] = band_log_e[i] - noise_floor[i];
            if c_count == 2 {
                mask[i] = mask[i].max(band_log_e[NB_EBANDS + i] - noise_floor[i]);
            }
        }
        let sig = mask;
        for i in 1..end {
            mask[i] = mask[i].max(mask[i - 1] - 2.0);
        }
        for i in (0..end - 1).rev() {
            mask[i] = mask[i].max(mask[i + 1] - 3.0);
        }
        for i in 0..end {
            // Mask never more than 72 dB below the peak nor below the floor.
            let smr = sig[i] - 0f32.max(max_depth - 12.0).max(mask[i]);
            let shift = (-(0.5 + smr).floor() as i32).clamp(0, 5);
            spread_weight[i] = 32 >> shift;
        }
    }
    // Only from 24 kbit/s on, so dynalloc cannot bust the budget.
    if effective_bytes < 30 + 5 * LM as i32 {
        return;
    }
    let mut follower = [0f32; 2 * NB_EBANDS];
    let mut last = 0;
    for c in 0..c_count {
        let e3 = &band_log_e2[c * NB_EBANDS..c * NB_EBANDS + end];
        let f = &mut follower[c * NB_EBANDS..c * NB_EBANDS + end];
        f[0] = e3[0];
        for i in 1..end {
            // The last band at least 3 dB above the previous one is the
            // last one considered (band-limited signals).
            if e3[i] > e3[i - 1] + 0.5 {
                last = i;
            }
            f[i] = (f[i - 1] + 1.5).min(e3[i]);
        }
        for i in (0..last).rev() {
            f[i] = f[i].min((f[i + 1] + 2.0).min(e3[i]));
        }
        // Median filter so that dynalloc does not trigger needlessly.
        let offset = 1.0f32;
        for i in 2..end - 2 {
            f[i] = f[i].max(median_of_5(&e3[i - 2..]) - offset);
        }
        let tmp = median_of_3(e3) - offset;
        f[0] = f[0].max(tmp);
        f[1] = f[1].max(tmp);
        let tmp = median_of_3(&e3[end - 3..]) - offset;
        f[end - 2] = f[end - 2].max(tmp);
        f[end - 1] = f[end - 1].max(tmp);
        for i in 0..end {
            f[i] = f[i].max(noise_floor[i]);
        }
    }
    if c_count == 2 {
        for i in 0..end {
            // Consider 24 dB "cross-talk".
            follower[NB_EBANDS + i] = follower[NB_EBANDS + i].max(follower[i] - 4.0);
            follower[i] = follower[i].max(follower[NB_EBANDS + i] - 4.0);
            follower[i] = 0.5
                * ((band_log_e[i] - follower[i]).max(0.0)
                    + (band_log_e[NB_EBANDS + i] - follower[NB_EBANDS + i]).max(0.0));
        }
    } else {
        for i in 0..end {
            follower[i] = (band_log_e[i] - follower[i]).max(0.0);
        }
    }
    // Halve the contribution for non-transient CBR frames.
    if !is_transient {
        for f in follower[..end].iter_mut() {
            *f *= 0.5;
        }
    }
    for (i, f) in follower[..end].iter_mut().enumerate() {
        if i < 8 {
            *f *= 2.0;
        }
        if i >= 12 {
            *f *= 0.5;
        }
    }
    let mut tot_boost = 0i32;
    for i in 0..end {
        let f = follower[i].min(4.0);
        let width = (c_count as i32 * (EBANDS[i + 1] - EBANDS[i]) as i32) << LM;
        let (boost, boost_bits) = if width < 6 {
            let b = f as i32;
            (b, b * width << BITRES)
        } else if width > 48 {
            let b = (f * 8.0) as i32;
            (b, (b * width << BITRES) / 8)
        } else {
            let b = (f * width as f32 / 6.0) as i32;
            (b, b * 6 << BITRES)
        };
        // Limit dynalloc to 2/3 of the bits.
        if (tot_boost + boost_bits) >> BITRES >> 3 > 2 * effective_bytes / 3 {
            let cap = (2 * effective_bytes / 3) << BITRES << 3;
            offsets[i] = cap - tot_boost;
            break;
        }
        offsets[i] = boost;
        tot_boost += boost_bits;
    }
}
