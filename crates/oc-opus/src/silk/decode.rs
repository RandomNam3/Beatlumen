//! SILK per-channel frame decoding: side information (indices), excitation
//! pulses (shell coding), parameter dequantisation and the core LTP/LPC
//! synthesis.
//!
//! Ported from libopus `silk/decoder_set_fs.c`, `silk/init_decoder.c`,
//! `silk/decode_frame.c`, `silk/decode_indices.c`, `silk/decode_pulses.c`,
//! `silk/shell_coder.c`, `silk/code_signs.c`, `silk/decode_parameters.c`,
//! `silk/decode_pitch.c`, `silk/gain_quant.c` and `silk/decode_core.c`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::lpc::*;
use super::macros::*;
use super::plc::{CngState, PlcState};
use super::resampler::Resampler;
use super::tables::*;
use crate::range_dec::RangeDecoder;

pub const MAX_NB_SUBFR: usize = 4;
pub const LTP_ORDER: usize = 5;
pub const MAX_FRAME_LENGTH: usize = 320;
pub const MAX_SUB_FRAME_LENGTH: usize = 80;
pub const TYPE_NO_VOICE_ACTIVITY: i32 = 0;
pub const TYPE_VOICED: i32 = 2;
pub const CODE_INDEPENDENTLY: i32 = 0;
pub const CODE_INDEPENDENTLY_NO_LTP_SCALING: i32 = 1;
pub const CODE_CONDITIONALLY: i32 = 2;
pub const FLAG_DECODE_NORMAL: i32 = 0;
pub const FLAG_PACKET_LOST: i32 = 1;
pub const FLAG_DECODE_LBRR: i32 = 2;

const SHELL_CODEC_FRAME_LENGTH: usize = 16;
const MAX_NB_SHELL_BLOCKS: usize = MAX_FRAME_LENGTH / SHELL_CODEC_FRAME_LENGTH;
const N_RATE_LEVELS: usize = 10;
const SILK_MAX_PULSES: usize = 16;
const QUANT_LEVEL_ADJUST_Q10: i32 = 80;
const BWE_AFTER_LOSS_Q16: i32 = 63570;
const GAIN_OFFSET: i32 = 2090;
const INV_SCALE_Q16: i32 = 1907825;
const N_LEVELS_QGAIN: i32 = 64;
const MIN_DELTA_GAIN_QUANT: i32 = -4;
const MAX_DELTA_GAIN_QUANT: i32 = 36;

#[derive(Clone, Default)]
pub struct SideInfoIndices {
    pub gains_indices: [i8; MAX_NB_SUBFR],
    pub ltp_index: [i8; MAX_NB_SUBFR],
    pub nlsf_indices: [i8; MAX_LPC_ORDER + 1],
    pub lag_index: i16,
    pub contour_index: i8,
    pub signal_type: i8,
    pub quant_offset_type: i8,
    pub nlsf_interp_coef_q2: i8,
    pub per_index: i8,
    pub ltp_scale_index: i8,
    pub seed: i8,
}

#[derive(Clone, Default)]
pub struct DecoderControl {
    pub pitch_l: [i32; MAX_NB_SUBFR],
    pub gains_q16: [i32; MAX_NB_SUBFR],
    pub pred_coef_q12: [[i16; MAX_LPC_ORDER]; 2],
    pub ltp_coef_q14: [i16; LTP_ORDER * MAX_NB_SUBFR],
    pub ltp_scale_q14: i32,
}

/// `silk_decoder_state`
#[derive(Clone)]
pub struct ChannelState {
    pub prev_gain_q16: i32,
    pub exc_q14: [i32; MAX_FRAME_LENGTH],
    pub s_lpc_q14_buf: [i32; MAX_LPC_ORDER],
    pub out_buf: [i16; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
    pub lag_prev: i32,
    pub last_gain_index: i8,
    pub fs_khz: i32,
    pub fs_api_hz: i32,
    pub nb_subfr: usize,
    pub frame_length: usize,
    pub subfr_length: usize,
    pub ltp_mem_length: usize,
    pub lpc_order: usize,
    pub prev_nlsf_q15: [i16; MAX_LPC_ORDER],
    pub first_frame_after_reset: bool,
    pub pitch_lag_low_bits_icdf: &'static [u8],
    pub pitch_contour_icdf: &'static [u8],
    pub n_frames_decoded: usize,
    pub n_frames_per_packet: usize,
    pub ec_prev_signal_type: i32,
    pub ec_prev_lag_index: i16,
    pub vad_flags: [bool; 3],
    pub lbrr_flag: bool,
    pub lbrr_flags: [bool; 3],
    pub resampler: Resampler,
    pub nlsf_cb: &'static NlsfCb,
    pub indices: SideInfoIndices,
    pub cng: CngState,
    pub loss_cnt: i32,
    pub prev_signal_type: i32,
    pub plc: PlcState,
}

impl ChannelState {
    fn blank() -> Self {
        ChannelState {
            prev_gain_q16: 0,
            exc_q14: [0; MAX_FRAME_LENGTH],
            s_lpc_q14_buf: [0; MAX_LPC_ORDER],
            out_buf: [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH],
            lag_prev: 0,
            last_gain_index: 0,
            fs_khz: 0,
            fs_api_hz: 0,
            nb_subfr: 0,
            frame_length: 0,
            subfr_length: 0,
            ltp_mem_length: 0,
            lpc_order: 0,
            prev_nlsf_q15: [0; MAX_LPC_ORDER],
            first_frame_after_reset: false,
            pitch_lag_low_bits_icdf: &SILK_UNIFORM4_ICDF,
            pitch_contour_icdf: &SILK_PITCH_CONTOUR_NB_ICDF,
            n_frames_decoded: 0,
            n_frames_per_packet: 0,
            ec_prev_signal_type: 0,
            ec_prev_lag_index: 0,
            vad_flags: [false; 3],
            lbrr_flag: false,
            lbrr_flags: [false; 3],
            resampler: Resampler::default(),
            nlsf_cb: &NLSF_CB_NB_MB,
            indices: SideInfoIndices::default(),
            cng: CngState::default(),
            loss_cnt: 0,
            prev_signal_type: 0,
            plc: PlcState::default(),
        }
    }

    pub fn new() -> Self {
        let mut s = Self::blank();
        s.reset();
        s
    }

    /// `silk_init_decoder` / `silk_reset_decoder` (identical without OSCE).
    pub fn reset(&mut self) {
        *self = Self::blank();
        self.first_frame_after_reset = true;
        self.prev_gain_q16 = 65536;
        self.cng_reset();
        self.plc_reset();
    }

    /// `silk_decoder_set_fs`
    pub fn set_fs(&mut self, fs_khz: i32, fs_api_hz: i32) {
        self.subfr_length = smulbb(5, fs_khz) as usize;
        let frame_length = self.nb_subfr * self.subfr_length;
        if self.fs_khz != fs_khz || self.fs_api_hz != fs_api_hz {
            self.resampler.init(fs_khz * 1000, fs_api_hz);
            self.fs_api_hz = fs_api_hz;
        }
        if self.fs_khz != fs_khz || frame_length != self.frame_length {
            if fs_khz == 8 {
                self.pitch_contour_icdf = if self.nb_subfr == MAX_NB_SUBFR {
                    &SILK_PITCH_CONTOUR_NB_ICDF
                } else {
                    &SILK_PITCH_CONTOUR_10_MS_NB_ICDF
                };
            } else {
                self.pitch_contour_icdf = if self.nb_subfr == MAX_NB_SUBFR {
                    &SILK_PITCH_CONTOUR_ICDF
                } else {
                    &SILK_PITCH_CONTOUR_10_MS_ICDF
                };
            }
            if self.fs_khz != fs_khz {
                self.ltp_mem_length = smulbb(20, fs_khz) as usize;
                if fs_khz == 8 || fs_khz == 12 {
                    self.lpc_order = 10;
                    self.nlsf_cb = &NLSF_CB_NB_MB;
                } else {
                    self.lpc_order = 16;
                    self.nlsf_cb = &NLSF_CB_WB;
                }
                self.pitch_lag_low_bits_icdf = match fs_khz {
                    16 => &SILK_UNIFORM8_ICDF,
                    12 => &SILK_UNIFORM6_ICDF,
                    _ => &SILK_UNIFORM4_ICDF,
                };
                self.first_frame_after_reset = true;
                self.lag_prev = 100;
                self.last_gain_index = 10;
                self.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
                self.out_buf = [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH];
                self.s_lpc_q14_buf = [0; MAX_LPC_ORDER];
            }
            self.fs_khz = fs_khz;
            self.frame_length = frame_length;
        }
    }

    /// `silk_decode_indices`
    pub fn decode_indices(
        &mut self,
        dec: &mut RangeDecoder,
        frame_index: usize,
        decode_lbrr: bool,
        cond_coding: i32,
    ) {
        let ix = if decode_lbrr || self.vad_flags[frame_index.min(2)] {
            dec.icdf(&SILK_TYPE_OFFSET_VAD_ICDF, 8) as i32 + 2
        } else {
            dec.icdf(&SILK_TYPE_OFFSET_NO_VAD_ICDF, 8) as i32
        };
        let ind = &mut self.indices;
        ind.signal_type = (ix >> 1) as i8;
        ind.quant_offset_type = (ix & 1) as i8;
        if cond_coding == CODE_CONDITIONALLY {
            ind.gains_indices[0] = dec.icdf(&SILK_DELTA_GAIN_ICDF, 8) as i8;
        } else {
            let a = (dec.icdf(&SILK_GAIN_ICDF[ind.signal_type as usize], 8) as i32) << 3;
            let b = dec.icdf(&SILK_UNIFORM8_ICDF, 8) as i32;
            ind.gains_indices[0] = (a as i8).wrapping_add(b as i8);
        }
        for i in 1..self.nb_subfr {
            ind.gains_indices[i] = dec.icdf(&SILK_DELTA_GAIN_ICDF, 8) as i8;
        }
        let cb = self.nlsf_cb;
        let off = (ind.signal_type as usize >> 1) * cb.n_vectors;
        ind.nlsf_indices[0] = dec.icdf(&cb.cb1_icdf[off..], 8) as i8;
        let mut ec_ix = [0i16; MAX_LPC_ORDER];
        let mut pred_q8 = [0u8; MAX_LPC_ORDER];
        nlsf_unpack(&mut ec_ix, &mut pred_q8, cb, ind.nlsf_indices[0] as usize);
        for i in 0..cb.order {
            let mut ix = dec.icdf(&cb.ec_icdf[ec_ix[i] as usize..], 8) as i32;
            if ix == 0 {
                ix -= dec.icdf(&SILK_NLSF_EXT_ICDF, 8) as i32;
            } else if ix == 8 {
                ix += dec.icdf(&SILK_NLSF_EXT_ICDF, 8) as i32;
            }
            ind.nlsf_indices[i + 1] = (ix - 4) as i8;
        }
        ind.nlsf_interp_coef_q2 = if self.nb_subfr == MAX_NB_SUBFR {
            dec.icdf(&SILK_NLSF_INTERPOLATION_FACTOR_ICDF, 8) as i8
        } else {
            4
        };
        if ind.signal_type as i32 == TYPE_VOICED {
            let mut decode_absolute = true;
            if cond_coding == CODE_CONDITIONALLY && self.ec_prev_signal_type == TYPE_VOICED {
                let delta = dec.icdf(&SILK_PITCH_DELTA_ICDF, 8) as i32;
                if delta > 0 {
                    ind.lag_index = (self.ec_prev_lag_index as i32 + delta - 9) as i16;
                    decode_absolute = false;
                }
            }
            if decode_absolute {
                let hi = dec.icdf(&SILK_PITCH_LAG_ICDF, 8) as i32 * (self.fs_khz >> 1);
                let lo = dec.icdf(self.pitch_lag_low_bits_icdf, 8) as i32;
                ind.lag_index = (hi + lo) as i16;
            }
            self.ec_prev_lag_index = ind.lag_index;
            ind.contour_index = dec.icdf(self.pitch_contour_icdf, 8) as i8;
            ind.per_index = dec.icdf(&SILK_LTP_PER_INDEX_ICDF, 8) as i8;
            for k in 0..self.nb_subfr {
                ind.ltp_index[k] = dec.icdf(ltp_gain_icdf(ind.per_index as usize), 8) as i8;
            }
            ind.ltp_scale_index = if cond_coding == CODE_INDEPENDENTLY {
                dec.icdf(&SILK_LTPSCALE_ICDF, 8) as i8
            } else {
                0
            };
        }
        self.ec_prev_signal_type = ind.signal_type as i32;
        ind.seed = dec.icdf(&SILK_UNIFORM4_ICDF, 8) as i8;
    }

    /// `silk_decode_parameters`
    fn decode_parameters(&mut self, ctrl: &mut DecoderControl, cond_coding: i32) {
        // Dequant gains
        let mut prev_ind = self.last_gain_index as i32;
        for k in 0..self.nb_subfr {
            let ind = self.indices.gains_indices[k] as i32;
            if k == 0 && cond_coding != CODE_CONDITIONALLY {
                prev_ind = ind.max(prev_ind - 16);
            } else {
                let ind_tmp = ind + MIN_DELTA_GAIN_QUANT;
                let thr = 2 * MAX_DELTA_GAIN_QUANT - N_LEVELS_QGAIN + prev_ind;
                if ind_tmp > thr {
                    prev_ind += (ind_tmp << 1) - thr;
                } else {
                    prev_ind += ind_tmp;
                }
            }
            prev_ind = (prev_ind as i8 as i32).clamp(0, N_LEVELS_QGAIN - 1);
            ctrl.gains_q16[k] = log2lin((smulwb(INV_SCALE_Q16, prev_ind) + GAIN_OFFSET).min(3967));
        }
        self.last_gain_index = prev_ind as i8;

        // NLSFs
        let mut nlsf_q15 = [0i16; MAX_LPC_ORDER];
        nlsf_decode(&mut nlsf_q15, &self.indices.nlsf_indices, self.nlsf_cb);
        let order = self.lpc_order;
        nlsf2a(&mut ctrl.pred_coef_q12[1], &nlsf_q15, order);
        if self.first_frame_after_reset {
            self.indices.nlsf_interp_coef_q2 = 4;
        }
        if self.indices.nlsf_interp_coef_q2 < 4 {
            let mut nlsf0 = [0i16; MAX_LPC_ORDER];
            for i in 0..order {
                nlsf0[i] = (self.prev_nlsf_q15[i] as i32
                    + ((self.indices.nlsf_interp_coef_q2 as i32
                        * (nlsf_q15[i] as i32 - self.prev_nlsf_q15[i] as i32))
                        >> 2)) as i16;
            }
            nlsf2a(&mut ctrl.pred_coef_q12[0], &nlsf0, order);
        } else {
            ctrl.pred_coef_q12[0] = ctrl.pred_coef_q12[1];
        }
        self.prev_nlsf_q15[..order].copy_from_slice(&nlsf_q15[..order]);
        if self.loss_cnt != 0 {
            bwexpander(&mut ctrl.pred_coef_q12[0][..order], BWE_AFTER_LOSS_Q16);
            bwexpander(&mut ctrl.pred_coef_q12[1][..order], BWE_AFTER_LOSS_Q16);
        }
        if self.indices.signal_type as i32 == TYPE_VOICED {
            decode_pitch(
                self.indices.lag_index,
                self.indices.contour_index,
                &mut ctrl.pitch_l,
                self.fs_khz,
                self.nb_subfr,
            );
            let cbk = ltp_vq_q7(self.indices.per_index as usize);
            for k in 0..self.nb_subfr {
                let ix = (self.indices.ltp_index[k] as usize).min(cbk.len() - 1);
                for i in 0..LTP_ORDER {
                    ctrl.ltp_coef_q14[k * LTP_ORDER + i] = lshift(cbk[ix][i] as i32, 7) as i16;
                }
            }
            ctrl.ltp_scale_q14 =
                SILK_LTPSCALES_TABLE_Q14[(self.indices.ltp_scale_index as usize).min(2)] as i32;
        } else {
            ctrl.pitch_l = [0; MAX_NB_SUBFR];
            ctrl.ltp_coef_q14 = [0; LTP_ORDER * MAX_NB_SUBFR];
            self.indices.per_index = 0;
            ctrl.ltp_scale_q14 = 0;
        }
    }

    /// `silk_decode_core`
    fn decode_core(&mut self, ctrl: &mut DecoderControl, xq: &mut [i16], pulses: &[i16]) {
        let ltp_mem = self.ltp_mem_length;
        let flen = self.frame_length;
        let sub = self.subfr_length;
        let order = self.lpc_order;
        let mut s_ltp = [0i16; MAX_FRAME_LENGTH];
        let mut s_ltp_q15 = [0i32; 2 * MAX_FRAME_LENGTH];
        let mut res_q14 = [0i32; MAX_SUB_FRAME_LENGTH];
        let mut s_lpc_q14 = [0i32; MAX_SUB_FRAME_LENGTH + MAX_LPC_ORDER];
        let offset_q10 = SILK_QUANTIZATION_OFFSETS_Q10
            [(self.indices.signal_type as usize >> 1).min(1)]
            [(self.indices.quant_offset_type as usize).min(1)] as i32;
        let nlsf_interpolation_flag = self.indices.nlsf_interp_coef_q2 < 4;

        let mut rand_seed = self.indices.seed as i32;
        for i in 0..flen {
            rand_seed = silk_rand(rand_seed);
            let mut e = lshift(pulses[i] as i32, 14);
            if e > 0 {
                e -= QUANT_LEVEL_ADJUST_Q10 << 4;
            } else if e < 0 {
                e += QUANT_LEVEL_ADJUST_Q10 << 4;
            }
            e += offset_q10 << 4;
            if rand_seed < 0 {
                e = -e;
            }
            self.exc_q14[i] = e;
            rand_seed = rand_seed.wrapping_add(pulses[i] as i32);
        }

        s_lpc_q14[..MAX_LPC_ORDER].copy_from_slice(&self.s_lpc_q14_buf);
        let mut s_ltp_buf_idx = ltp_mem;
        let mut lag = 0i32;
        for k in 0..self.nb_subfr {
            let a_q12 = ctrl.pred_coef_q12[k >> 1];
            let mut signal_type = self.indices.signal_type as i32;
            let gain_q10 = ctrl.gains_q16[k] >> 6;
            let mut inv_gain_q31 = inverse32_varq(ctrl.gains_q16[k], 47);
            let gain_adj_q16 = if ctrl.gains_q16[k] != self.prev_gain_q16 {
                let g = div32_varq(self.prev_gain_q16, ctrl.gains_q16[k], 16);
                for v in s_lpc_q14[..MAX_LPC_ORDER].iter_mut() {
                    *v = smulww(g, *v);
                }
                g
            } else {
                1 << 16
            };
            self.prev_gain_q16 = ctrl.gains_q16[k];

            if self.loss_cnt != 0
                && self.prev_signal_type == TYPE_VOICED
                && self.indices.signal_type as i32 != TYPE_VOICED
                && k < MAX_NB_SUBFR / 2
            {
                for v in ctrl.ltp_coef_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER].iter_mut() {
                    *v = 0;
                }
                ctrl.ltp_coef_q14[k * LTP_ORDER + LTP_ORDER / 2] = 4096;
                signal_type = TYPE_VOICED;
                ctrl.pitch_l[k] = self.lag_prev;
            }
            let b_q14: [i16; LTP_ORDER] = ctrl.ltp_coef_q14[k * LTP_ORDER..(k + 1) * LTP_ORDER]
                .try_into()
                .unwrap();

            if signal_type == TYPE_VOICED {
                lag = ctrl.pitch_l[k];
                let lagu = lag.clamp(0, ltp_mem as i32) as usize;
                if k == 0 || (k == 2 && nlsf_interpolation_flag) {
                    let start_idx = ltp_mem as isize
                        - lagu as isize
                        - order as isize
                        - (LTP_ORDER / 2) as isize;
                    let start_idx = start_idx.max(0) as usize;
                    if k == 2 {
                        self.out_buf[ltp_mem..ltp_mem + 2 * sub].copy_from_slice(&xq[..2 * sub]);
                    }
                    let in_off = start_idx + k * sub;
                    lpc_analysis_filter(
                        &mut s_ltp[start_idx..ltp_mem],
                        &self.out_buf[in_off..in_off + ltp_mem - start_idx],
                        &a_q12[..order],
                        ltp_mem - start_idx,
                        order,
                    );
                    if k == 0 {
                        inv_gain_q31 = lshift(smulwb(inv_gain_q31, ctrl.ltp_scale_q14), 2);
                    }
                    for i in 0..(lagu + LTP_ORDER / 2).min(ltp_mem) {
                        s_ltp_q15[s_ltp_buf_idx - i - 1] =
                            smulwb(inv_gain_q31, s_ltp[ltp_mem - i - 1] as i32);
                    }
                } else if gain_adj_q16 != 1 << 16 {
                    for i in 0..(lagu + LTP_ORDER / 2).min(s_ltp_buf_idx) {
                        let idx = s_ltp_buf_idx - i - 1;
                        s_ltp_q15[idx] = smulww(gain_adj_q16, s_ltp_q15[idx]);
                    }
                }
            }

            let pexc = &self.exc_q14[k * sub..(k + 1) * sub];
            if signal_type == TYPE_VOICED {
                let lagu = lag.clamp(0, ltp_mem as i32) as usize;
                let mut pred = s_ltp_buf_idx + LTP_ORDER / 2 - lagu;
                for i in 0..sub {
                    let mut ltp_pred_q13: i32 = 2;
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred], b_q14[0] as i32);
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred - 1], b_q14[1] as i32);
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred - 2], b_q14[2] as i32);
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred - 3], b_q14[3] as i32);
                    ltp_pred_q13 = smlawb(ltp_pred_q13, s_ltp_q15[pred - 4], b_q14[4] as i32);
                    pred += 1;
                    res_q14[i] = add_lshift32(pexc[i], ltp_pred_q13, 1);
                    s_ltp_q15[s_ltp_buf_idx] = lshift(res_q14[i], 1);
                    s_ltp_buf_idx += 1;
                }
            } else {
                res_q14[..sub].copy_from_slice(pexc);
            }

            let pxq = &mut xq[k * sub..(k + 1) * sub];
            for i in 0..sub {
                let mut lpc_pred_q10 = (order >> 1) as i32;
                for j in 0..order {
                    lpc_pred_q10 = smlawb(
                        lpc_pred_q10,
                        s_lpc_q14[MAX_LPC_ORDER + i - j - 1],
                        a_q12[j] as i32,
                    );
                }
                s_lpc_q14[MAX_LPC_ORDER + i] = add_sat32(res_q14[i], lshift_sat32(lpc_pred_q10, 4));
                pxq[i] = sat16(rshift_round(
                    smulww(s_lpc_q14[MAX_LPC_ORDER + i], gain_q10),
                    8,
                )) as i16;
            }
            s_lpc_q14.copy_within(sub..sub + MAX_LPC_ORDER, 0);
        }
        self.s_lpc_q14_buf
            .copy_from_slice(&s_lpc_q14[..MAX_LPC_ORDER]);
    }

    /// `silk_decode_frame`: decodes (or conceals) one frame into `out`.
    pub fn decode_frame(
        &mut self,
        dec: Option<&mut RangeDecoder>,
        out: &mut [i16],
        lost_flag: i32,
        cond_coding: i32,
    ) -> usize {
        let l = self.frame_length;
        let mut ctrl = DecoderControl::default();
        let normal = match dec {
            Some(_) => {
                lost_flag == FLAG_DECODE_NORMAL
                    || (lost_flag == FLAG_DECODE_LBRR
                        && self.lbrr_flags[self.n_frames_decoded.min(2)])
            }
            None => false,
        };
        if normal {
            let dec = dec.unwrap();
            let mut pulses = [0i16; MAX_FRAME_LENGTH];
            let fi = self.n_frames_decoded;
            self.decode_indices(dec, fi, lost_flag == FLAG_DECODE_LBRR, cond_coding);
            decode_pulses(
                dec,
                &mut pulses,
                self.indices.signal_type as i32,
                self.indices.quant_offset_type as i32,
                l,
            );
            self.decode_parameters(&mut ctrl, cond_coding);
            self.decode_core(&mut ctrl, &mut out[..l], &pulses);
            let mv = self.ltp_mem_length - l;
            self.out_buf.copy_within(l..l + mv, 0);
            self.out_buf[mv..mv + l].copy_from_slice(&out[..l]);
            self.plc_update(&ctrl);
            self.loss_cnt = 0;
            self.prev_signal_type = self.indices.signal_type as i32;
            self.first_frame_after_reset = false;
        } else {
            self.plc_conceal(&mut ctrl, &mut out[..l]);
            self.loss_cnt += 1;
            let mv = self.ltp_mem_length - l;
            self.out_buf.copy_within(l..l + mv, 0);
            self.out_buf[mv..mv + l].copy_from_slice(&out[..l]);
        }
        self.cng(&ctrl, &mut out[..l]);
        self.plc_glue_frames(&mut out[..l]);
        self.lag_prev = ctrl.pitch_l[self.nb_subfr - 1];
        l
    }
}

/// `silk_decode_pitch`
fn decode_pitch(
    lag_index: i16,
    contour_index: i8,
    pitch_lags: &mut [i32; MAX_NB_SUBFR],
    fs_khz: i32,
    nb_subfr: usize,
) {
    let ci = contour_index.max(0) as usize;
    let cb = |k: usize| -> i32 {
        if fs_khz == 8 {
            if nb_subfr == MAX_NB_SUBFR {
                SILK_CB_LAGS_STAGE2[k][ci.min(10)] as i32
            } else {
                SILK_CB_LAGS_STAGE2_10_MS[k][ci.min(2)] as i32
            }
        } else if nb_subfr == MAX_NB_SUBFR {
            SILK_CB_LAGS_STAGE3[k][ci.min(33)] as i32
        } else {
            SILK_CB_LAGS_STAGE3_10_MS[k][ci.min(11)] as i32
        }
    };
    let min_lag = smulbb(2, fs_khz);
    let max_lag = smulbb(18, fs_khz);
    let lag = min_lag + lag_index as i32;
    for (k, p) in pitch_lags.iter_mut().enumerate().take(nb_subfr) {
        *p = limit(lag + cb(k), min_lag, max_lag);
    }
}

fn shell_split(dec: &mut RangeDecoder, p: i32, table: &[u8]) -> (i16, i16) {
    if p > 0 {
        let off = SILK_SHELL_CODE_TABLE_OFFSETS[(p as usize).min(16)] as usize;
        let c1 = dec.icdf(&table[off..], 8) as i32;
        (c1 as i16, (p - c1) as i16)
    } else {
        (0, 0)
    }
}

/// `silk_shell_decoder`: one block of 16 pulse amplitudes.
fn shell_decoder(out: &mut [i16], dec: &mut RangeDecoder, pulses4: i32) {
    let t0 = &SILK_SHELL_CODE_TABLE0;
    let t1 = &SILK_SHELL_CODE_TABLE1;
    let t2 = &SILK_SHELL_CODE_TABLE2;
    let t3 = &SILK_SHELL_CODE_TABLE3;
    let mut p3 = [0i16; 2];
    let mut p2 = [0i16; 4];
    let mut p1 = [0i16; 8];
    (p3[0], p3[1]) = shell_split(dec, pulses4, t3);
    (p2[0], p2[1]) = shell_split(dec, p3[0] as i32, t2);
    (p1[0], p1[1]) = shell_split(dec, p2[0] as i32, t1);
    (out[0], out[1]) = shell_split(dec, p1[0] as i32, t0);
    (out[2], out[3]) = shell_split(dec, p1[1] as i32, t0);
    (p1[2], p1[3]) = shell_split(dec, p2[1] as i32, t1);
    (out[4], out[5]) = shell_split(dec, p1[2] as i32, t0);
    (out[6], out[7]) = shell_split(dec, p1[3] as i32, t0);
    (p2[2], p2[3]) = shell_split(dec, p3[1] as i32, t2);
    (p1[4], p1[5]) = shell_split(dec, p2[2] as i32, t1);
    (out[8], out[9]) = shell_split(dec, p1[4] as i32, t0);
    (out[10], out[11]) = shell_split(dec, p1[5] as i32, t0);
    (p1[6], p1[7]) = shell_split(dec, p2[3] as i32, t1);
    (out[12], out[13]) = shell_split(dec, p1[6] as i32, t0);
    (out[14], out[15]) = shell_split(dec, p1[7] as i32, t0);
}

/// `silk_decode_pulses` (including LSBs and signs).
pub fn decode_pulses(
    dec: &mut RangeDecoder,
    pulses: &mut [i16; MAX_FRAME_LENGTH],
    signal_type: i32,
    quant_offset_type: i32,
    frame_length: usize,
) {
    let rate_level = dec.icdf(
        &SILK_RATE_LEVELS_ICDF[(signal_type >> 1).clamp(0, 1) as usize],
        8,
    );
    let mut iter = frame_length >> 4;
    if iter * SHELL_CODEC_FRAME_LENGTH < frame_length {
        iter += 1;
    }
    let iter = iter.min(MAX_NB_SHELL_BLOCKS);
    let mut sum_pulses = [0i32; MAX_NB_SHELL_BLOCKS];
    let mut n_lshifts = [0i32; MAX_NB_SHELL_BLOCKS];
    let cdf = &SILK_PULSES_PER_BLOCK_ICDF[rate_level.min(N_RATE_LEVELS - 1)];
    for i in 0..iter {
        sum_pulses[i] = dec.icdf(cdf, 8) as i32;
        while sum_pulses[i] == SILK_MAX_PULSES as i32 + 1 {
            n_lshifts[i] += 1;
            let off = (n_lshifts[i] == 10) as usize;
            sum_pulses[i] =
                dec.icdf(&SILK_PULSES_PER_BLOCK_ICDF[N_RATE_LEVELS - 1][off..], 8) as i32;
            if n_lshifts[i] > 10 {
                break;
            }
        }
    }
    // The pulse buffer may be one block longer than frame_length (10 ms at 12 kHz).
    let mut buf = [0i16; MAX_FRAME_LENGTH + SHELL_CODEC_FRAME_LENGTH];
    for i in 0..iter {
        let blk = &mut buf[i * 16..i * 16 + 16];
        if sum_pulses[i] > 0 {
            shell_decoder(blk, dec, sum_pulses[i]);
        } else {
            blk.fill(0);
        }
    }
    for i in 0..iter {
        if n_lshifts[i] > 0 {
            let nls = n_lshifts[i];
            for v in buf[i * 16..i * 16 + 16].iter_mut() {
                let mut abs_q = *v as i32;
                for _ in 0..nls {
                    abs_q = lshift(abs_q, 1);
                    abs_q += dec.icdf(&SILK_LSB_ICDF, 8) as i32;
                }
                *v = abs_q as i16;
            }
            sum_pulses[i] |= nls << 5;
        }
    }
    // Signs
    let mut icdf = [0u8; 2];
    let base = smulbb(7, add_lshift32(quant_offset_type, signal_type, 1)).clamp(0, 35) as usize;
    let icdf_ptr = &SILK_SIGN_ICDF[base..];
    let len = (frame_length + SHELL_CODEC_FRAME_LENGTH / 2) >> 4;
    for i in 0..len.min(iter) {
        let p = sum_pulses[i];
        if p > 0 {
            icdf[0] = icdf_ptr[((p & 0x1F) as usize).min(6)];
            for v in buf[i * 16..i * 16 + 16].iter_mut() {
                if *v > 0 {
                    let s = dec.icdf(&icdf, 8) as i32;
                    *v = (*v as i32 * (2 * s - 1)) as i16;
                }
            }
        }
    }
    pulses[..frame_length].copy_from_slice(&buf[..frame_length]);
}
