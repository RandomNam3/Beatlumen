//! CELT decoder (frame decoding, synthesis, post-filter, de-emphasis and
//! packet loss concealment).
//!
//! Ported from libopus `celt/celt_decoder.c`:
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

use super::bands::{anti_collapse, denormalise_bands, quant_all_bands, AllBandsParams};
use super::energy::{unquant_coarse_energy, unquant_energy_finalise, unquant_fine_energy};
use super::math::lcg_rand;
use super::mdct::{mdct_backward, Cpx};
use super::mode::*;
use super::pitch::{
    celt_autocorr, celt_fir, celt_iir, celt_lpc, comb_filter, pitch_downsample, pitch_search,
};
use super::rate::compute_allocation;
use super::vq::renormalise_vector;
use crate::range_dec::{RangeDecoder, BITRES};

const PLC_PITCH_LAG_MAX: usize = 720;
const PLC_PITCH_LAG_MIN: usize = 100;
const CHAN_STRIDE: usize = DECODE_BUFFER_SIZE + OVERLAP;
const VERY_SMALL: f32 = 1e-30;

/// Error from the CELT layer (maps to OPUS_INTERNAL_ERROR / BAD_ARG).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CeltError;

pub struct CeltDecoder {
    channels: usize,
    pub stream_channels: usize,
    downsample: usize,
    pub start: usize,
    pub end: usize,
    disable_inv: bool,

    pub rng: u32,
    pub error: bool,
    last_pitch_index: i32,
    loss_duration: i32,
    skip_plc: bool,
    postfilter_period: i32,
    postfilter_period_old: i32,
    postfilter_gain: f32,
    postfilter_gain_old: f32,
    postfilter_tapset: usize,
    postfilter_tapset_old: usize,
    prefilter_and_fold: bool,
    preemph_mem_d: [f32; 2],

    decode_mem: Vec<f32>,
    lpc: [f32; 2 * CELT_LPC_ORDER],
    old_band_e: [f32; 2 * NB_EBANDS],
    old_log_e: [f32; 2 * NB_EBANDS],
    old_log_e2: [f32; 2 * NB_EBANDS],
    background_log_e: [f32; 2 * NB_EBANDS],

    // Scratch buffers (no state).
    x: Vec<f32>,
    freq: Vec<f32>,
    freq2: Vec<f32>,
    fft_scratch: Vec<Cpx>,
}

impl CeltDecoder {
    pub fn new(sample_rate: u32, channels: usize) -> Option<Self> {
        let downsample = resampling_factor(sample_rate);
        if downsample == 0 || !(1..=2).contains(&channels) {
            return None;
        }
        let mut d = CeltDecoder {
            channels,
            stream_channels: channels,
            downsample,
            start: 0,
            end: EFF_EBANDS,
            disable_inv: channels == 1,
            rng: 0,
            error: false,
            last_pitch_index: 0,
            loss_duration: 0,
            skip_plc: true,
            postfilter_period: 0,
            postfilter_period_old: 0,
            postfilter_gain: 0.0,
            postfilter_gain_old: 0.0,
            postfilter_tapset: 0,
            postfilter_tapset_old: 0,
            prefilter_and_fold: false,
            preemph_mem_d: [0.0; 2],
            decode_mem: vec![0.0; channels * CHAN_STRIDE],
            lpc: [0.0; 2 * CELT_LPC_ORDER],
            old_band_e: [0.0; 2 * NB_EBANDS],
            old_log_e: [-28.0; 2 * NB_EBANDS],
            old_log_e2: [-28.0; 2 * NB_EBANDS],
            background_log_e: [0.0; 2 * NB_EBANDS],
            x: vec![0.0; 2 * 960],
            freq: vec![0.0; 960],
            freq2: vec![0.0; 960],
            fft_scratch: Vec::with_capacity(480),
        };
        d.reset();
        Some(d)
    }

    /// `OPUS_RESET_STATE`
    pub fn reset(&mut self) {
        self.rng = 0;
        self.error = false;
        self.last_pitch_index = 0;
        self.loss_duration = 0;
        self.skip_plc = true;
        self.postfilter_period = 0;
        self.postfilter_period_old = 0;
        self.postfilter_gain = 0.0;
        self.postfilter_gain_old = 0.0;
        self.postfilter_tapset = 0;
        self.postfilter_tapset_old = 0;
        self.prefilter_and_fold = false;
        self.preemph_mem_d = [0.0; 2];
        self.decode_mem.fill(0.0);
        self.lpc = [0.0; 2 * CELT_LPC_ORDER];
        self.old_band_e = [0.0; 2 * NB_EBANDS];
        self.old_log_e = [-28.0; 2 * NB_EBANDS];
        self.old_log_e2 = [-28.0; 2 * NB_EBANDS];
        self.background_log_e = [0.0; 2 * NB_EBANDS];
    }

    fn deemphasis(&mut self, n: usize, pcm: &mut [f32]) {
        let cc = self.channels;
        let coef0 = PREEMPH0;
        let ds = self.downsample;
        let nd = n / ds;
        for c in 0..cc {
            let base = c * CHAN_STRIDE + DECODE_BUFFER_SIZE - n;
            let x = &self.decode_mem[base..base + n];
            let mut m = self.preemph_mem_d[c];
            if ds > 1 {
                let scratch = &mut self.freq2[..n];
                for j in 0..n {
                    let tmp = x[j] + VERY_SMALL + m;
                    m = coef0 * tmp;
                    scratch[j] = tmp;
                }
                for j in 0..nd {
                    pcm[j * cc + c] = scratch[j * ds] * (1.0 / 32768.0);
                }
            } else {
                for j in 0..n {
                    let tmp = x[j] + VERY_SMALL + m;
                    m = coef0 * tmp;
                    pcm[j * cc + c] = tmp * (1.0 / 32768.0);
                }
            }
            self.preemph_mem_d[c] = m;
        }
    }

    /// `celt_synthesis`: denormalises `self.x` and runs the inverse MDCT into
    /// the decode buffer.
    #[allow(clippy::too_many_arguments)]
    fn synthesis(
        &mut self,
        start: usize,
        eff_end: usize,
        c_count: usize,
        is_transient: bool,
        lm: usize,
        silence: bool,
    ) {
        let cc = self.channels;
        let m = 1usize << lm;
        let n = SHORT_MDCT_SIZE << lm;
        let (b_cnt, nb, shift) = if is_transient {
            (m, SHORT_MDCT_SIZE, MAX_LM)
        } else {
            (1, SHORT_MDCT_SIZE << lm, MAX_LM - lm)
        };
        let ds = self.downsample;
        let out_off = DECODE_BUFFER_SIZE - n;
        if cc == 2 && c_count == 1 {
            denormalise_bands(
                &self.x[..n],
                &mut self.freq[..n],
                &self.old_band_e,
                start,
                eff_end,
                m,
                ds,
                silence,
            );
            self.freq2[..n].copy_from_slice(&self.freq[..n]);
            for ch in 0..2 {
                let src = if ch == 0 { &self.freq2 } else { &self.freq };
                let ob = ch * CHAN_STRIDE + out_off;
                for b in 0..b_cnt {
                    mdct_backward(
                        &src[b..n],
                        &mut self.decode_mem[ob + nb * b..ob + nb * b + nb + OVERLAP],
                        &WINDOW120,
                        OVERLAP,
                        shift,
                        b_cnt,
                        &mut self.fft_scratch,
                    );
                }
            }
        } else if cc == 1 && c_count == 2 {
            denormalise_bands(
                &self.x[..n],
                &mut self.freq[..n],
                &self.old_band_e,
                start,
                eff_end,
                m,
                ds,
                silence,
            );
            denormalise_bands(
                &self.x[n..2 * n],
                &mut self.freq2[..n],
                &self.old_band_e[NB_EBANDS..],
                start,
                eff_end,
                m,
                ds,
                silence,
            );
            for i in 0..n {
                self.freq[i] = 0.5 * self.freq[i] + 0.5 * self.freq2[i];
            }
            for b in 0..b_cnt {
                mdct_backward(
                    &self.freq[b..n],
                    &mut self.decode_mem[out_off + nb * b..out_off + nb * b + nb + OVERLAP],
                    &WINDOW120,
                    OVERLAP,
                    shift,
                    b_cnt,
                    &mut self.fft_scratch,
                );
            }
        } else {
            for c in 0..cc {
                denormalise_bands(
                    &self.x[c * n..(c + 1) * n],
                    &mut self.freq[..n],
                    &self.old_band_e[c * NB_EBANDS..],
                    start,
                    eff_end,
                    m,
                    ds,
                    silence,
                );
                let ob = c * CHAN_STRIDE + out_off;
                for b in 0..b_cnt {
                    mdct_backward(
                        &self.freq[b..n],
                        &mut self.decode_mem[ob + nb * b..ob + nb * b + nb + OVERLAP],
                        &WINDOW120,
                        OVERLAP,
                        shift,
                        b_cnt,
                        &mut self.fft_scratch,
                    );
                }
            }
        }
    }

    fn prefilter_and_fold(&mut self, n: usize) {
        let mut etmp = [0f32; OVERLAP];
        for c in 0..self.channels {
            let base = c * CHAN_STRIDE;
            let off = base + DECODE_BUFFER_SIZE - n;
            {
                // comb_filter(etmp, decode_mem+DECODE_BUFFER_SIZE-N, ...) with a
                // separate source: copy the needed history into a temp buffer.
                let src = &self.decode_mem[base..base + CHAN_STRIDE];
                comb_filter(
                    &mut etmp,
                    0,
                    Some((src, DECODE_BUFFER_SIZE - n)),
                    self.postfilter_period_old,
                    self.postfilter_period,
                    OVERLAP,
                    -self.postfilter_gain_old,
                    -self.postfilter_gain,
                    self.postfilter_tapset_old,
                    self.postfilter_tapset,
                    &WINDOW120,
                    0,
                );
            }
            for i in 0..OVERLAP / 2 {
                self.decode_mem[off + i] =
                    WINDOW120[i] * etmp[OVERLAP - 1 - i] + WINDOW120[OVERLAP - i - 1] * etmp[i];
            }
        }
    }

    fn plc_pitch_search(&self) -> i32 {
        let mut lp = vec![0f32; DECODE_BUFFER_SIZE >> 1];
        let chans: Vec<&[f32]> = (0..self.channels)
            .map(|c| &self.decode_mem[c * CHAN_STRIDE..c * CHAN_STRIDE + DECODE_BUFFER_SIZE])
            .collect();
        pitch_downsample(&chans, &mut lp, DECODE_BUFFER_SIZE);
        let p = pitch_search(
            &lp[PLC_PITCH_LAG_MAX >> 1..],
            &lp,
            DECODE_BUFFER_SIZE - PLC_PITCH_LAG_MAX,
            PLC_PITCH_LAG_MAX - PLC_PITCH_LAG_MIN,
        );
        PLC_PITCH_LAG_MAX as i32 - p
    }

    /// `celt_decode_lost`: packet loss concealment for one frame of N samples.
    fn decode_lost(&mut self, n: usize, lm: usize) {
        let c_count = self.channels;
        let start = self.start;
        let loss_duration = self.loss_duration;
        let noise_based = loss_duration >= 40 || start != 0 || self.skip_plc;
        if noise_based {
            let end = self.end;
            let eff_end = start.max(end.min(EFF_EBANDS));
            for c in 0..c_count {
                let base = c * CHAN_STRIDE;
                self.decode_mem
                    .copy_within(base + n..base + CHAN_STRIDE, base);
            }
            if self.prefilter_and_fold {
                self.prefilter_and_fold(n);
            }
            let decay = if loss_duration == 0 { 1.5f32 } else { 0.5f32 };
            for c in 0..c_count {
                for i in start..end {
                    let idx = c * NB_EBANDS + i;
                    let v = self.old_band_e[idx] - decay;
                    self.old_band_e[idx] = if self.background_log_e[idx] > v {
                        self.background_log_e[idx]
                    } else {
                        v
                    };
                }
            }
            let mut seed = self.rng;
            for c in 0..c_count {
                for i in start..eff_end {
                    let boffs = n * c + ((EBANDS[i] as usize) << lm);
                    let blen = ((EBANDS[i + 1] - EBANDS[i]) as usize) << lm;
                    for j in 0..blen {
                        seed = lcg_rand(seed);
                        self.x[boffs + j] = ((seed as i32) >> 20) as f32;
                    }
                    renormalise_vector(&mut self.x[boffs..boffs + blen], blen, 1.0);
                }
            }
            self.rng = seed;
            self.synthesis(start, eff_end, c_count, false, lm, false);
            self.prefilter_and_fold = false;
            self.skip_plc = true;
        } else {
            let mut fade = 1.0f32;
            let pitch_index;
            if loss_duration == 0 {
                pitch_index = self.plc_pitch_search();
                self.last_pitch_index = pitch_index;
            } else {
                pitch_index = self.last_pitch_index;
                fade = 0.8;
            }
            let pitch_index = pitch_index.clamp(1, MAX_PERIOD as i32) as usize;
            let exc_length = (2 * pitch_index).min(MAX_PERIOD);
            let mut exc_buf = vec![0f32; MAX_PERIOD + CELT_LPC_ORDER];
            let mut fir_tmp = vec![0f32; exc_length];
            const EO: usize = CELT_LPC_ORDER; // offset of exc[0] in exc_buf
            for c in 0..c_count {
                let base = c * CHAN_STRIDE;
                for i in 0..MAX_PERIOD + CELT_LPC_ORDER {
                    exc_buf[i] = self.decode_mem
                        [base + DECODE_BUFFER_SIZE - MAX_PERIOD - CELT_LPC_ORDER + i];
                }
                let lpc = &mut self.lpc[c * CELT_LPC_ORDER..(c + 1) * CELT_LPC_ORDER];
                if loss_duration == 0 {
                    let mut ac = [0f32; CELT_LPC_ORDER + 1];
                    celt_autocorr(
                        &exc_buf[EO..],
                        &mut ac,
                        Some(&WINDOW120),
                        OVERLAP,
                        CELT_LPC_ORDER,
                        MAX_PERIOD,
                    );
                    ac[0] *= 1.0001;
                    for (i, a) in ac.iter_mut().enumerate().skip(1) {
                        *a -= *a * (0.008f32 * 0.008f32) * i as f32 * i as f32;
                    }
                    celt_lpc(lpc, &ac, CELT_LPC_ORDER);
                }
                celt_fir(
                    &exc_buf,
                    EO + MAX_PERIOD - exc_length,
                    lpc,
                    &mut fir_tmp,
                    exc_length,
                    CELT_LPC_ORDER,
                );
                exc_buf[EO + MAX_PERIOD - exc_length..EO + MAX_PERIOD].copy_from_slice(&fir_tmp);
                let exc = &exc_buf[EO..];

                let decay;
                {
                    let mut e1 = 1f32;
                    let mut e2 = 1f32;
                    let decay_length = exc_length >> 1;
                    for i in 0..decay_length {
                        let e = exc[MAX_PERIOD - decay_length + i];
                        e1 += e * e;
                        let e = exc[MAX_PERIOD - 2 * decay_length + i];
                        e2 += e * e;
                    }
                    e1 = if e1 < e2 { e1 } else { e2 };
                    decay = (e1 / e2).sqrt();
                }
                let buf = &mut self.decode_mem[base..base + CHAN_STRIDE];
                buf.copy_within(n..DECODE_BUFFER_SIZE, 0);
                let extrapolation_offset = MAX_PERIOD - pitch_index;
                let extrapolation_len = n + OVERLAP;
                let mut attenuation = fade * decay;
                let mut s1 = 0f32;
                let mut j = 0usize;
                for i in 0..extrapolation_len {
                    if j >= pitch_index {
                        j -= pitch_index;
                        attenuation *= decay;
                    }
                    buf[DECODE_BUFFER_SIZE - n + i] = attenuation * exc[extrapolation_offset + j];
                    let tmp = buf[DECODE_BUFFER_SIZE - MAX_PERIOD - n + extrapolation_offset + j];
                    s1 += tmp * tmp;
                    j += 1;
                }
                let mut lpc_mem = [0f32; CELT_LPC_ORDER];
                for (i, lm_v) in lpc_mem.iter_mut().enumerate() {
                    *lm_v = buf[DECODE_BUFFER_SIZE - n - 1 - i];
                }
                let lpc = &self.lpc[c * CELT_LPC_ORDER..(c + 1) * CELT_LPC_ORDER];
                celt_iir(
                    buf,
                    DECODE_BUFFER_SIZE - n,
                    lpc,
                    extrapolation_len,
                    CELT_LPC_ORDER,
                    &mut lpc_mem,
                );
                let mut s2 = 0f32;
                for i in 0..extrapolation_len {
                    let tmp = buf[DECODE_BUFFER_SIZE - n + i];
                    s2 += tmp * tmp;
                }
                #[allow(clippy::neg_cmp_op_on_partial_ord)]
                if !(s1 > 0.2 * s2) {
                    buf[DECODE_BUFFER_SIZE - n..DECODE_BUFFER_SIZE - n + extrapolation_len]
                        .fill(0.0);
                } else if s1 < s2 {
                    let ratio = ((s1 + 1.0) / (s2 + 1.0)).sqrt();
                    for i in 0..OVERLAP {
                        let tmp_g = 1.0 - WINDOW120[i] * (1.0 - ratio);
                        buf[DECODE_BUFFER_SIZE - n + i] *= tmp_g;
                    }
                    for i in OVERLAP..extrapolation_len {
                        buf[DECODE_BUFFER_SIZE - n + i] *= ratio;
                    }
                }
            }
            self.prefilter_and_fold = true;
        }
        self.loss_duration = (loss_duration + (1 << lm)).min(10000);
    }

    /// `celt_decode_with_ec`. `data == None` (or len <= 1) runs the PLC.
    /// Output is `frame_size` samples per channel (at the API rate),
    /// interleaved with `self.channels` channels.
    pub fn decode<'a>(
        &mut self,
        data: Option<&'a [u8]>,
        pcm: &mut [f32],
        frame_size: usize,
        dec: Option<&mut RangeDecoder<'a>>,
    ) -> Result<usize, CeltError> {
        let cc = self.channels;
        let c_count = self.stream_channels;
        let start = self.start;
        let end = self.end;
        let frame_size_full = frame_size * self.downsample;
        let mut lm = usize::MAX;
        for l in 0..=MAX_LM {
            if SHORT_MDCT_SIZE << l == frame_size_full {
                lm = l;
                break;
            }
        }
        if lm == usize::MAX {
            return Err(CeltError);
        }
        let m = 1usize << lm;
        let len = data.map(|d| d.len()).unwrap_or(0);
        if len > 1275 || pcm.len() < frame_size * cc {
            return Err(CeltError);
        }
        let n = m * SHORT_MDCT_SIZE;
        let eff_end = end.min(EFF_EBANDS);

        let data = match data {
            Some(d) if d.len() > 1 => d,
            _ => {
                self.decode_lost(n, lm);
                self.deemphasis(n, pcm);
                return Ok(frame_size);
            }
        };

        if self.loss_duration == 0 {
            self.skip_plc = false;
        }
        let mut local_dec;
        let dec: &mut RangeDecoder<'a> = match dec {
            Some(d) => d,
            None => {
                local_dec = RangeDecoder::new(data);
                &mut local_dec
            }
        };

        if c_count == 1 {
            for i in 0..NB_EBANDS {
                let a = self.old_band_e[i];
                let b = self.old_band_e[NB_EBANDS + i];
                self.old_band_e[i] = if a > b { a } else { b };
            }
        }

        let mut total_bits = len as i32 * 8;
        let mut tell = dec.tell();
        let silence = if tell >= total_bits {
            true
        } else if tell == 1 {
            dec.bit_logp(15)
        } else {
            false
        };
        if silence {
            tell = len as i32 * 8;
            dec.nbits_total += tell - dec.tell();
        }

        let mut postfilter_gain = 0f32;
        let mut postfilter_pitch = 0i32;
        let mut postfilter_tapset = 0usize;
        if start == 0 && tell + 16 <= total_bits {
            if dec.bit_logp(1) {
                let octave = dec.uint(6) as i32;
                postfilter_pitch = (16 << octave) + dec.bits((4 + octave) as u32) as i32 - 1;
                let qg = dec.bits(3) as i32;
                if dec.tell() + 2 <= total_bits {
                    postfilter_tapset = dec.icdf(&TAPSET_ICDF, 2);
                }
                postfilter_gain = 0.09375 * (qg + 1) as f32;
            }
            tell = dec.tell();
        }

        let is_transient = if lm > 0 && tell + 3 <= total_bits {
            let t = dec.bit_logp(3);
            tell = dec.tell();
            t
        } else {
            false
        };
        let short_blocks = is_transient;

        let intra_ener = if tell + 3 <= total_bits {
            dec.bit_logp(3)
        } else {
            false
        };
        if !intra_ener && self.loss_duration != 0 {
            let missing = 10.min(self.loss_duration >> lm);
            let safety = match lm {
                0 => 1.5f32,
                1 => 0.5f32,
                _ => 0.0f32,
            };
            for c in 0..2 {
                for i in start..end {
                    let idx = c * NB_EBANDS + i;
                    let lo = self.old_log_e[idx];
                    let lo2 = self.old_log_e2[idx];
                    let mx = if lo > lo2 { lo } else { lo2 };
                    if self.old_band_e[idx] < mx {
                        let e0 = self.old_band_e[idx];
                        let e1 = self.old_log_e[idx];
                        let e2 = self.old_log_e2[idx];
                        let a = e1 - e0;
                        let b = 0.5 * (e2 - e0);
                        let slope = if a > b { a } else { b };
                        let d = (1 + missing) as f32 * slope;
                        let e0 = e0 - if 0.0 > d { 0.0 } else { d };
                        self.old_band_e[idx] = if -20.0 > e0 { -20.0 } else { e0 };
                    } else {
                        let a = self.old_band_e[idx];
                        let mn = if a < lo { a } else { lo };
                        self.old_band_e[idx] = if mn < lo2 { mn } else { lo2 };
                    }
                    self.old_band_e[idx] -= safety;
                }
            }
        }

        unquant_coarse_energy(
            start,
            end,
            &mut self.old_band_e,
            intra_ener,
            dec,
            c_count,
            lm,
        );

        let mut tf_res = [0i32; NB_EBANDS];
        tf_decode(start, end, is_transient, &mut tf_res, lm, dec);

        tell = dec.tell();
        let mut spread_decision = SPREAD_NORMAL;
        if tell + 4 <= total_bits {
            spread_decision = dec.icdf(&SPREAD_ICDF, 5) as i32;
        }

        let mut cap = [0i32; NB_EBANDS];
        init_caps(&mut cap, lm, c_count);

        let mut offsets = [0i32; NB_EBANDS];
        let mut dynalloc_logp = 6i32;
        total_bits <<= BITRES;
        let mut tell_f = dec.tell_frac() as i32;
        for i in start..end {
            let width = ((c_count as i32) * (EBANDS[i + 1] - EBANDS[i]) as i32) << lm;
            let quanta = (width << BITRES).min((6 << BITRES).max(width));
            let mut dynalloc_loop_logp = dynalloc_logp;
            let mut boost = 0i32;
            while tell_f + (dynalloc_loop_logp << BITRES) < total_bits && boost < cap[i] {
                let flag = dec.bit_logp(dynalloc_loop_logp as u32);
                tell_f = dec.tell_frac() as i32;
                if !flag {
                    break;
                }
                boost += quanta;
                total_bits -= quanta;
                dynalloc_loop_logp = 1;
            }
            offsets[i] = boost;
            if boost > 0 {
                dynalloc_logp = 2.max(dynalloc_logp - 1);
            }
        }

        let alloc_trim = if tell_f + (6 << BITRES) <= total_bits {
            dec.icdf(&TRIM_ICDF, 7) as i32
        } else {
            5
        };

        let mut bits = ((len as i32 * 8) << BITRES) - dec.tell_frac() as i32 - 1;
        let anti_collapse_rsv = if is_transient && lm >= 2 && bits >= ((lm as i32 + 2) << BITRES) {
            1 << BITRES
        } else {
            0
        };
        bits -= anti_collapse_rsv;

        let alloc = compute_allocation(
            start,
            end,
            &offsets,
            &cap,
            alloc_trim,
            bits,
            c_count as i32,
            lm as i32,
            dec,
        );

        unquant_fine_energy(
            start,
            end,
            &mut self.old_band_e,
            &alloc.fine_quant,
            dec,
            c_count,
        );

        for c in 0..cc {
            let base = c * CHAN_STRIDE;
            self.decode_mem
                .copy_within(base + n..base + CHAN_STRIDE, base);
        }

        let mut collapse_masks = [0u8; 2 * NB_EBANDS];
        {
            let params = AllBandsParams {
                start,
                end,
                c: c_count,
                pulses: &alloc.pulses,
                short_blocks,
                spread: spread_decision,
                dual_stereo: alloc.dual_stereo,
                intensity: alloc.intensity,
                tf_res: &tf_res,
                total_bits: (len as i32 * (8 << BITRES)) - anti_collapse_rsv,
                balance: alloc.balance,
                lm,
                coded_bands: alloc.coded_bands,
                disable_inv: self.disable_inv,
            };
            let mut rng = self.rng;
            quant_all_bands(
                &params,
                &mut self.x[..c_count * n],
                &mut collapse_masks,
                dec,
                &mut rng,
            );
            self.rng = rng;
        }

        let mut anti_collapse_on = false;
        if anti_collapse_rsv > 0 {
            anti_collapse_on = dec.bits(1) != 0;
        }

        unquant_energy_finalise(
            start,
            end,
            &mut self.old_band_e,
            &alloc.fine_quant,
            &alloc.fine_priority,
            len as i32 * 8 - dec.tell(),
            dec,
            c_count,
        );

        if anti_collapse_on {
            anti_collapse(
                &mut self.x,
                &collapse_masks,
                lm,
                c_count,
                n,
                start,
                end,
                &self.old_band_e,
                &self.old_log_e,
                &self.old_log_e2,
                &alloc.pulses,
                self.rng,
            );
        }

        if silence {
            for v in self.old_band_e[..c_count * NB_EBANDS].iter_mut() {
                *v = -28.0;
            }
        }
        if self.prefilter_and_fold {
            self.prefilter_and_fold(n);
        }
        self.synthesis(start, eff_end, c_count, is_transient, lm, silence);

        for c in 0..cc {
            self.postfilter_period = self.postfilter_period.max(COMBFILTER_MINPERIOD);
            self.postfilter_period_old = self.postfilter_period_old.max(COMBFILTER_MINPERIOD);
            let base = c * CHAN_STRIDE;
            let buf = &mut self.decode_mem[base..base + CHAN_STRIDE];
            let off = DECODE_BUFFER_SIZE - n;
            comb_filter(
                buf,
                off,
                None,
                self.postfilter_period_old,
                self.postfilter_period,
                SHORT_MDCT_SIZE,
                self.postfilter_gain_old,
                self.postfilter_gain,
                self.postfilter_tapset_old,
                self.postfilter_tapset,
                &WINDOW120,
                OVERLAP,
            );
            if lm != 0 {
                comb_filter(
                    buf,
                    off + SHORT_MDCT_SIZE,
                    None,
                    self.postfilter_period,
                    postfilter_pitch,
                    n - SHORT_MDCT_SIZE,
                    self.postfilter_gain,
                    postfilter_gain,
                    self.postfilter_tapset,
                    postfilter_tapset,
                    &WINDOW120,
                    OVERLAP,
                );
            }
        }
        self.postfilter_period_old = self.postfilter_period;
        self.postfilter_gain_old = self.postfilter_gain;
        self.postfilter_tapset_old = self.postfilter_tapset;
        self.postfilter_period = postfilter_pitch;
        self.postfilter_gain = postfilter_gain;
        self.postfilter_tapset = postfilter_tapset;
        if lm != 0 {
            self.postfilter_period_old = self.postfilter_period;
            self.postfilter_gain_old = self.postfilter_gain;
            self.postfilter_tapset_old = self.postfilter_tapset;
        }

        if c_count == 1 {
            let (a, b) = self.old_band_e.split_at_mut(NB_EBANDS);
            b.copy_from_slice(a);
        }
        if !is_transient {
            self.old_log_e2 = self.old_log_e;
            self.old_log_e = self.old_band_e;
        } else {
            for i in 0..2 * NB_EBANDS {
                let a = self.old_log_e[i];
                let b = self.old_band_e[i];
                self.old_log_e[i] = if a < b { a } else { b };
            }
        }
        let max_background_increase = (160.min(self.loss_duration + m as i32)) as f32 * 0.001;
        for i in 0..2 * NB_EBANDS {
            let a = self.background_log_e[i] + max_background_increase;
            let b = self.old_band_e[i];
            self.background_log_e[i] = if a < b { a } else { b };
        }
        for c in 0..2 {
            for i in (0..start).chain(end..NB_EBANDS) {
                let idx = c * NB_EBANDS + i;
                self.old_band_e[idx] = 0.0;
                self.old_log_e[idx] = -28.0;
                self.old_log_e2[idx] = -28.0;
            }
        }
        self.rng = dec.rng;

        self.deemphasis(n, pcm);
        self.loss_duration = 0;
        self.prefilter_and_fold = false;
        if dec.tell() > 8 * len as i32 {
            return Err(CeltError);
        }
        if dec.error {
            self.error = true;
        }
        Ok(frame_size)
    }
}

fn tf_decode(
    start: usize,
    end: usize,
    is_transient: bool,
    tf_res: &mut [i32; NB_EBANDS],
    lm: usize,
    dec: &mut RangeDecoder,
) {
    let mut budget = dec.storage * 8;
    let mut tell = dec.tell() as u32;
    let mut logp: u32 = if is_transient { 2 } else { 4 };
    let tf_select_rsv = lm > 0 && tell + logp < budget;
    budget -= tf_select_rsv as u32;
    let mut tf_changed = 0i32;
    let mut curr = 0i32;
    for r in tf_res.iter_mut().take(end).skip(start) {
        if tell + logp <= budget {
            curr ^= dec.bit_logp(logp) as i32;
            tell = dec.tell() as u32;
            tf_changed |= curr;
        }
        *r = curr;
        logp = if is_transient { 4 } else { 5 };
    }
    let mut tf_select = 0usize;
    let it = 4 * is_transient as usize;
    if tf_select_rsv
        && TF_SELECT_TABLE[lm][it + tf_changed as usize]
            != TF_SELECT_TABLE[lm][it + 2 + tf_changed as usize]
    {
        tf_select = dec.bit_logp(1) as usize;
    }
    for r in tf_res.iter_mut().take(end).skip(start) {
        *r = TF_SELECT_TABLE[lm][it + 2 * tf_select + *r as usize] as i32;
    }
}
