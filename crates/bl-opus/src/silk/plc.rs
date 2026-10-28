//! SILK packet loss concealment and comfort noise generation.
//!
//! Ported from libopus `silk/PLC.c` and `silk/CNG.c`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::decode::*;
use super::lpc::*;
use super::macros::*;

const NB_ATT: usize = 2;
const HARM_ATT_Q15: [i32; NB_ATT] = [32440, 31130];
const PLC_RAND_ATTENUATE_V_Q15: [i32; NB_ATT] = [31130, 26214];
const PLC_RAND_ATTENUATE_UV_Q15: [i32; NB_ATT] = [32440, 29491];
const BWE_COEF_Q16: i32 = 64881;
const V_PITCH_GAIN_START_MIN_Q14: i32 = 11469;
const V_PITCH_GAIN_START_MAX_Q14: i32 = 15565;
const MAX_PITCH_LAG_MS: i32 = 18;
const RAND_BUF_SIZE: usize = 128;
const RAND_BUF_MASK: i32 = RAND_BUF_SIZE as i32 - 1;
const LOG2_INV_LPC_GAIN_HIGH_THRES: i32 = 3;
const LOG2_INV_LPC_GAIN_LOW_THRES: i32 = 8;
const PITCH_DRIFT_FAC_Q16: i32 = 655;

const CNG_BUF_MASK_MAX: i32 = 255;
const CNG_GAIN_SMTH_Q16: i32 = 4634;
const CNG_GAIN_SMTH_THRESHOLD_Q16: i32 = 46396;
const CNG_NLSF_SMTH_Q16: i32 = 16348;

#[derive(Clone, Default)]
pub struct PlcState {
    pub pitch_l_q8: i32,
    pub ltp_coef_q14: [i16; LTP_ORDER],
    pub prev_lpc_q12: [i16; MAX_LPC_ORDER],
    pub last_frame_lost: bool,
    pub rand_seed: i32,
    pub rand_scale_q14: i16,
    pub conc_energy: i32,
    pub conc_energy_shift: i32,
    pub prev_ltp_scale_q14: i16,
    pub prev_gain_q16: [i32; 2],
    pub fs_khz: i32,
    pub nb_subfr: usize,
    pub subfr_length: usize,
}

#[derive(Clone)]
pub struct CngState {
    pub exc_buf_q14: [i32; MAX_FRAME_LENGTH],
    pub smth_nlsf_q15: [i16; MAX_LPC_ORDER],
    pub synth_state: [i32; MAX_LPC_ORDER],
    pub smth_gain_q16: i32,
    pub rand_seed: i32,
    pub fs_khz: i32,
}

impl Default for CngState {
    fn default() -> Self {
        CngState {
            exc_buf_q14: [0; MAX_FRAME_LENGTH],
            smth_nlsf_q15: [0; MAX_LPC_ORDER],
            synth_state: [0; MAX_LPC_ORDER],
            smth_gain_q16: 0,
            rand_seed: 0,
            fs_khz: 0,
        }
    }
}

impl ChannelState {
    pub fn plc_reset(&mut self) {
        self.plc.pitch_l_q8 = lshift(self.frame_length as i32, 8 - 1);
        self.plc.prev_gain_q16 = [65536, 65536];
        self.plc.subfr_length = 20;
        self.plc.nb_subfr = 2;
    }

    pub fn cng_reset(&mut self) {
        let step = div32(i16::MAX as i32, self.lpc_order as i32 + 1);
        let mut acc = 0i32;
        for i in 0..self.lpc_order {
            acc += step;
            self.cng.smth_nlsf_q15[i] = acc as i16;
        }
        self.cng.smth_gain_q16 = 0;
        self.cng.rand_seed = 3176576;
    }

    fn plc_check_fs(&mut self) {
        if self.fs_khz != self.plc.fs_khz {
            self.plc_reset();
            self.plc.fs_khz = self.fs_khz;
        }
    }

    /// `silk_PLC` with `lost == 0`.
    pub fn plc_update(&mut self, ctrl: &DecoderControl) {
        self.plc_check_fs();
        let nb = self.nb_subfr;
        self.prev_signal_type = self.indices.signal_type as i32;
        let mut ltp_gain_q14: i32 = 0;
        let plc = &mut self.plc;
        if self.indices.signal_type as i32 == TYPE_VOICED {
            let mut j = 0usize;
            while ((j * self.subfr_length) as i32) < ctrl.pitch_l[nb - 1] {
                if j == nb {
                    break;
                }
                let mut temp: i32 = 0;
                for i in 0..LTP_ORDER {
                    temp += ctrl.ltp_coef_q14[(nb - 1 - j) * LTP_ORDER + i] as i32;
                }
                if temp > ltp_gain_q14 {
                    ltp_gain_q14 = temp;
                    plc.ltp_coef_q14.copy_from_slice(
                        &ctrl.ltp_coef_q14[(nb - 1 - j) * LTP_ORDER..(nb - j) * LTP_ORDER],
                    );
                    plc.pitch_l_q8 = lshift(ctrl.pitch_l[nb - 1 - j], 8);
                }
                j += 1;
            }
            plc.ltp_coef_q14 = [0; LTP_ORDER];
            plc.ltp_coef_q14[LTP_ORDER / 2] = ltp_gain_q14 as i16;
            if ltp_gain_q14 < V_PITCH_GAIN_START_MIN_Q14 {
                let tmp = lshift(V_PITCH_GAIN_START_MIN_Q14, 10);
                let scale_q10 = div32(tmp, ltp_gain_q14.max(1));
                for v in plc.ltp_coef_q14.iter_mut() {
                    *v = (smulbb(*v as i32, scale_q10) >> 10) as i16;
                }
            } else if ltp_gain_q14 > V_PITCH_GAIN_START_MAX_Q14 {
                let tmp = lshift(V_PITCH_GAIN_START_MAX_Q14, 14);
                let scale_q14 = div32(tmp, ltp_gain_q14.max(1));
                for v in plc.ltp_coef_q14.iter_mut() {
                    *v = (smulbb(*v as i32, scale_q14) >> 14) as i16;
                }
            }
        } else {
            plc.pitch_l_q8 = lshift(smulbb(self.fs_khz, 18), 8);
            plc.ltp_coef_q14 = [0; LTP_ORDER];
        }
        let order = self.lpc_order;
        plc.prev_lpc_q12[..order].copy_from_slice(&ctrl.pred_coef_q12[1][..order]);
        plc.prev_ltp_scale_q14 = ctrl.ltp_scale_q14 as i16;
        plc.prev_gain_q16 = [ctrl.gains_q16[nb - 2], ctrl.gains_q16[nb - 1]];
        plc.subfr_length = self.subfr_length;
        plc.nb_subfr = nb;
    }

    /// `silk_PLC` with `lost == 1` (`silk_PLC_conceal`).
    pub fn plc_conceal(&mut self, ctrl: &mut DecoderControl, frame: &mut [i16]) {
        self.plc_check_fs();
        let ltp_mem = self.ltp_mem_length;
        let flen = self.frame_length;
        let sub = self.subfr_length;
        let order = self.lpc_order;
        let mut s_ltp_q14 = [0i32; 2 * MAX_FRAME_LENGTH];
        let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
        let prev_gain_q10 = [
            self.plc.prev_gain_q16[0] >> 6,
            self.plc.prev_gain_q16[1] >> 6,
        ];
        if self.first_frame_after_reset {
            self.plc.prev_lpc_q12 = [0; MAX_LPC_ORDER];
        }
        // silk_PLC_energy
        let (energy1, shift1, energy2, shift2) = {
            let mut exc_buf = [0i16; 2 * MAX_SUB_FRAME_LENGTH];
            for k in 0..2 {
                for i in 0..sub {
                    let idx = i + (k + self.nb_subfr - 2) * sub;
                    exc_buf[k * sub + i] =
                        sat16(smulww(self.exc_q14[idx], prev_gain_q10[k]) >> 8) as i16;
                }
            }
            let (e1, s1) = sum_sqr_shift(&exc_buf[..sub]);
            let (e2, s2) = sum_sqr_shift(&exc_buf[sub..2 * sub]);
            (e1, s1, e2, s2)
        };
        let rand_off = if (energy1 >> shift2) < (energy2 >> shift1) {
            0.max(
                (self.plc.nb_subfr as i32 - 1) * self.plc.subfr_length as i32
                    - RAND_BUF_SIZE as i32,
            ) as usize
        } else {
            0.max(self.plc.nb_subfr as i32 * self.plc.subfr_length as i32 - RAND_BUF_SIZE as i32)
                as usize
        };
        let mut b_q14 = self.plc.ltp_coef_q14;
        let mut rand_scale_q14 = self.plc.rand_scale_q14 as i32;
        let att = (self.loss_cnt as usize).min(NB_ATT - 1);
        let harm_gain_q15 = HARM_ATT_Q15[att];
        let mut rand_gain_q15 = if self.prev_signal_type == TYPE_VOICED {
            PLC_RAND_ATTENUATE_V_Q15[att]
        } else {
            PLC_RAND_ATTENUATE_UV_Q15[att]
        };
        bwexpander(&mut self.plc.prev_lpc_q12[..order], BWE_COEF_Q16);
        let mut a_q12 = [0i16; MAX_LPC_ORDER];
        a_q12[..order].copy_from_slice(&self.plc.prev_lpc_q12[..order]);
        if self.loss_cnt == 0 {
            rand_scale_q14 = 1 << 14;
            if self.prev_signal_type == TYPE_VOICED {
                for &b in b_q14.iter() {
                    rand_scale_q14 -= b as i32;
                }
                rand_scale_q14 = (rand_scale_q14 as i16).max(3277) as i32;
                rand_scale_q14 = (smulbb(rand_scale_q14, self.plc.prev_ltp_scale_q14 as i32) >> 14)
                    as i16 as i32;
            } else {
                let inv_gain_q30 = lpc_inverse_pred_gain(&self.plc.prev_lpc_q12[..order]);
                let mut down_scale_q30 =
                    ((1i32 << 30) >> LOG2_INV_LPC_GAIN_HIGH_THRES).min(inv_gain_q30);
                down_scale_q30 = ((1i32 << 30) >> LOG2_INV_LPC_GAIN_LOW_THRES).max(down_scale_q30);
                down_scale_q30 = lshift(down_scale_q30, LOG2_INV_LPC_GAIN_HIGH_THRES);
                rand_gain_q15 = smulwb(down_scale_q30, rand_gain_q15) >> 14;
            }
        }
        let mut rand_seed = self.plc.rand_seed;
        let mut lag = rshift_round(self.plc.pitch_l_q8, 8);
        let mut s_ltp_buf_idx = ltp_mem;

        // Rewhiten LTP state
        let lagu = lag.clamp(0, ltp_mem as i32) as usize;
        let idx = (ltp_mem as isize - lagu as isize - order as isize - (LTP_ORDER / 2) as isize)
            .max(0) as usize;
        lpc_analysis_filter(
            &mut s_ltp[idx..ltp_mem],
            &self.out_buf[idx..ltp_mem],
            &a_q12[..order],
            ltp_mem - idx,
            order,
        );
        let mut inv_gain_q30 = inverse32_varq(self.plc.prev_gain_q16[1], 46);
        inv_gain_q30 = inv_gain_q30.min(i32::MAX >> 1);
        for i in idx + order..ltp_mem {
            s_ltp_q14[i] = smulwb(inv_gain_q30, s_ltp[i] as i32);
        }

        // LTP synthesis filtering
        for _k in 0..self.nb_subfr {
            let lagu = lag.clamp(0, s_ltp_buf_idx as i32 - 2) as usize;
            let mut pred = s_ltp_buf_idx - lagu + LTP_ORDER / 2;
            for _i in 0..sub {
                let mut ltp_pred_q12: i32 = 2;
                ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred], b_q14[0] as i32);
                ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred - 1], b_q14[1] as i32);
                ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred - 2], b_q14[2] as i32);
                ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred - 3], b_q14[3] as i32);
                ltp_pred_q12 = smlawb(ltp_pred_q12, s_ltp_q14[pred - 4], b_q14[4] as i32);
                pred += 1;
                rand_seed = silk_rand(rand_seed);
                let ridx = ((rand_seed >> 25) & RAND_BUF_MASK) as usize;
                let rv = self.exc_q14.get(rand_off + ridx).copied().unwrap_or(0);
                s_ltp_q14[s_ltp_buf_idx] = lshift(smlawb(ltp_pred_q12, rv, rand_scale_q14), 2);
                s_ltp_buf_idx += 1;
            }
            for b in b_q14.iter_mut() {
                *b = (smulbb(harm_gain_q15, *b as i32) >> 15) as i16;
            }
            rand_scale_q14 = (smulbb(rand_scale_q14, rand_gain_q15) >> 15) as i16 as i32;
            self.plc.pitch_l_q8 = smlawb(
                self.plc.pitch_l_q8,
                self.plc.pitch_l_q8,
                PITCH_DRIFT_FAC_Q16,
            );
            self.plc.pitch_l_q8 = self
                .plc
                .pitch_l_q8
                .min(lshift(smulbb(MAX_PITCH_LAG_MS, self.fs_khz), 8));
            lag = rshift_round(self.plc.pitch_l_q8, 8);
        }

        // LPC synthesis filtering
        let base = ltp_mem - MAX_LPC_ORDER;
        s_ltp_q14[base..ltp_mem].copy_from_slice(&self.s_lpc_q14_buf);
        for i in 0..flen {
            let mut lpc_pred_q10 = (order >> 1) as i32;
            for j in 0..order {
                lpc_pred_q10 = smlawb(
                    lpc_pred_q10,
                    s_ltp_q14[base + MAX_LPC_ORDER + i - j - 1],
                    a_q12[j] as i32,
                );
            }
            let p = base + MAX_LPC_ORDER + i;
            s_ltp_q14[p] = add_sat32(s_ltp_q14[p], lshift_sat32(lpc_pred_q10, 4));
            frame[i] = sat16(sat16(rshift_round(
                smulww(s_ltp_q14[p], prev_gain_q10[1]),
                8,
            ))) as i16;
        }
        self.s_lpc_q14_buf
            .copy_from_slice(&s_ltp_q14[base + flen..base + flen + MAX_LPC_ORDER]);

        self.plc.ltp_coef_q14 = b_q14;
        self.plc.rand_seed = rand_seed;
        self.plc.rand_scale_q14 = rand_scale_q14 as i16;
        ctrl.pitch_l = [lag; MAX_NB_SUBFR];
    }

    /// `silk_PLC_glue_frames`
    pub fn plc_glue_frames(&mut self, frame: &mut [i16]) {
        let length = frame.len() as i32;
        if self.loss_cnt != 0 {
            let (e, s) = sum_sqr_shift(frame);
            self.plc.conc_energy = e;
            self.plc.conc_energy_shift = s;
            self.plc.last_frame_lost = true;
        } else {
            if self.plc.last_frame_lost {
                let (mut energy, energy_shift) = sum_sqr_shift(frame);
                if energy_shift > self.plc.conc_energy_shift {
                    self.plc.conc_energy >>= energy_shift - self.plc.conc_energy_shift;
                } else if energy_shift < self.plc.conc_energy_shift {
                    energy >>= self.plc.conc_energy_shift - energy_shift;
                }
                if energy > self.plc.conc_energy {
                    let lz = clz32(self.plc.conc_energy) - 1;
                    self.plc.conc_energy = lshift(self.plc.conc_energy, lz);
                    energy >>= (24 - lz).max(0);
                    let frac_q24 = div32(self.plc.conc_energy, energy.max(1));
                    let mut gain_q16 = lshift(sqrt_approx(frac_q24), 4);
                    let mut slope_q16 = div32((1i32 << 16) - gain_q16, length);
                    slope_q16 = lshift(slope_q16, 2);
                    for v in frame.iter_mut() {
                        *v = smulwb(gain_q16, *v as i32) as i16;
                        gain_q16 += slope_q16;
                        if gain_q16 > 1 << 16 {
                            break;
                        }
                    }
                }
            }
            self.plc.last_frame_lost = false;
        }
    }

    /// `silk_CNG`
    pub fn cng(&mut self, ctrl: &DecoderControl, frame: &mut [i16]) {
        let length = frame.len();
        if self.fs_khz != self.cng.fs_khz {
            self.cng_reset();
            self.cng.fs_khz = self.fs_khz;
        }
        let order = self.lpc_order;
        if self.loss_cnt == 0 && self.prev_signal_type == TYPE_NO_VOICE_ACTIVITY {
            for i in 0..order {
                let d = self.prev_nlsf_q15[i] as i32 - self.cng.smth_nlsf_q15[i] as i32;
                self.cng.smth_nlsf_q15[i] =
                    (self.cng.smth_nlsf_q15[i] as i32 + smulwb(d, CNG_NLSF_SMTH_Q16)) as i16;
            }
            let mut max_gain_q16 = 0;
            let mut subfr = 0;
            for i in 0..self.nb_subfr {
                if ctrl.gains_q16[i] > max_gain_q16 {
                    max_gain_q16 = ctrl.gains_q16[i];
                    subfr = i;
                }
            }
            let sub = self.subfr_length;
            let n = (self.nb_subfr - 1) * sub;
            self.cng.exc_buf_q14.copy_within(0..n, sub);
            self.cng.exc_buf_q14[..sub]
                .copy_from_slice(&self.exc_q14[subfr * sub..(subfr + 1) * sub]);
            for i in 0..self.nb_subfr {
                self.cng.smth_gain_q16 = self.cng.smth_gain_q16.wrapping_add(smulwb(
                    ctrl.gains_q16[i].wrapping_sub(self.cng.smth_gain_q16),
                    CNG_GAIN_SMTH_Q16,
                ));
                if smulww(self.cng.smth_gain_q16, CNG_GAIN_SMTH_THRESHOLD_Q16) > ctrl.gains_q16[i] {
                    self.cng.smth_gain_q16 = ctrl.gains_q16[i];
                }
            }
        }
        if self.loss_cnt != 0 {
            let mut sig = [0i32; MAX_FRAME_LENGTH + MAX_LPC_ORDER];
            let mut gain_q16 = smulww(self.plc.rand_scale_q14 as i32, self.plc.prev_gain_q16[1]);
            if gain_q16 >= (1 << 21) || self.cng.smth_gain_q16 > (1 << 23) {
                gain_q16 = smultt(gain_q16, gain_q16);
                gain_q16 = sub_lshift32(
                    smultt(self.cng.smth_gain_q16, self.cng.smth_gain_q16),
                    gain_q16,
                    5,
                );
                gain_q16 = lshift(sqrt_approx(gain_q16), 16);
            } else {
                gain_q16 = smulww(gain_q16, gain_q16);
                gain_q16 = sub_lshift32(
                    smulww(self.cng.smth_gain_q16, self.cng.smth_gain_q16),
                    gain_q16,
                    5,
                );
                gain_q16 = lshift(sqrt_approx(gain_q16), 8);
            }
            let gain_q10 = gain_q16 >> 6;
            // silk_CNG_exc
            let mut exc_mask = CNG_BUF_MASK_MAX;
            while exc_mask > length as i32 {
                exc_mask >>= 1;
            }
            let mut seed = self.cng.rand_seed;
            for i in 0..length {
                seed = silk_rand(seed);
                let idx = ((seed >> 24) & exc_mask) as usize;
                sig[MAX_LPC_ORDER + i] = self.cng.exc_buf_q14[idx];
            }
            self.cng.rand_seed = seed;
            let mut a_q12 = [0i16; MAX_LPC_ORDER];
            let nlsf = self.cng.smth_nlsf_q15;
            nlsf2a(&mut a_q12, &nlsf, order);
            sig[..MAX_LPC_ORDER].copy_from_slice(&self.cng.synth_state);
            for i in 0..length {
                let mut lpc_pred_q10 = (order >> 1) as i32;
                for j in 0..order {
                    lpc_pred_q10 = smlawb(
                        lpc_pred_q10,
                        sig[MAX_LPC_ORDER + i - j - 1],
                        a_q12[j] as i32,
                    );
                }
                let p = MAX_LPC_ORDER + i;
                sig[p] = add_sat32(sig[p], lshift_sat32(lpc_pred_q10, 4));
                frame[i] = add_sat16(
                    frame[i] as i32,
                    sat16(rshift_round(smulww(sig[p], gain_q10), 8)),
                ) as i16;
            }
            self.cng
                .synth_state
                .copy_from_slice(&sig[length..length + MAX_LPC_ORDER]);
        } else {
            for v in self.cng.synth_state[..order].iter_mut() {
                *v = 0;
            }
        }
    }
}
