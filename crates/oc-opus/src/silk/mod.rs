//! SILK layer of the Opus decoder (RFC 6716 section 4.2): top-level frame
//! handling (`silk_Decode`), VAD/LBRR flags, stereo prediction and
//! mid/side to left/right conversion, and resampling to the API rate.
//!
//! The SILK decoder is fixed point in libopus (also in the float build), so
//! this port is bit exact with the reference.
//!
//! Ported from libopus `silk/dec_API.c`, `silk/stereo_decode_pred.c` and
//! `silk/stereo_MS_to_LR.c`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

mod decode;
mod lpc;
mod macros;
mod plc;
mod resampler;
#[rustfmt::skip]
mod tables;

use crate::range_dec::RangeDecoder;
use decode::*;
use macros::*;
use tables::*;

/// Decoder control (`silk_DecControlStruct`).
pub struct SilkDecControl {
    pub n_channels_api: usize,
    pub n_channels_internal: usize,
    pub api_sample_rate: i32,
    pub internal_sample_rate: i32,
    pub payload_size_ms: i32,
}

impl SilkDecControl {
    pub fn new(channels: usize, fs: u32) -> Self {
        SilkDecControl {
            n_channels_api: channels,
            n_channels_internal: channels,
            api_sample_rate: fs as i32,
            internal_sample_rate: 16000,
            payload_size_ms: 20,
        }
    }
}

#[derive(Default, Clone)]
struct StereoState {
    pred_prev_q13: [i16; 2],
    s_mid: [i16; 2],
    s_side: [i16; 2],
}

/// Error from the SILK layer.
#[derive(Debug)]
pub struct SilkError;

pub struct SilkDecoder {
    ch: [ChannelState; 2],
    stereo: StereoState,
    n_channels_api: usize,
    n_channels_internal: usize,
    prev_decode_only_middle: bool,
    tmp: [Vec<i16>; 2],
    resample_out: Vec<i16>,
}

fn stereo_decode_pred(dec: &mut RangeDecoder) -> [i32; 2] {
    let mut ix = [[0i32; 3]; 2];
    let n = dec.icdf(&SILK_STEREO_PRED_JOINT_ICDF, 8) as i32;
    ix[0][2] = n / 5;
    ix[1][2] = n - 5 * ix[0][2];
    for row in ix.iter_mut() {
        row[0] = dec.icdf(&SILK_UNIFORM3_ICDF, 8) as i32;
        row[1] = dec.icdf(&SILK_UNIFORM5_ICDF, 8) as i32;
    }
    let mut pred = [0i32; 2];
    for n in 0..2 {
        ix[n][0] += 3 * ix[n][2];
        let i0 = (ix[n][0] as usize).min(14);
        let low_q13 = SILK_STEREO_PRED_QUANT_Q13[i0] as i32;
        let step_q13 = smulwb(SILK_STEREO_PRED_QUANT_Q13[i0 + 1] as i32 - low_q13, 6554);
        pred[n] = smlabb(low_q13, step_q13, 2 * ix[n][1] + 1);
    }
    pred[0] -= pred[1];
    pred
}

fn stereo_ms_to_lr(
    st: &mut StereoState,
    x1: &mut [i16],
    x2: &mut [i16],
    pred_q13: &[i32; 2],
    fs_khz: i32,
    frame_length: usize,
) {
    x1[..2].copy_from_slice(&st.s_mid);
    x2[..2].copy_from_slice(&st.s_side);
    st.s_mid
        .copy_from_slice(&x1[frame_length..frame_length + 2]);
    st.s_side
        .copy_from_slice(&x2[frame_length..frame_length + 2]);
    let mut pred0_q13 = st.pred_prev_q13[0] as i32;
    let mut pred1_q13 = st.pred_prev_q13[1] as i32;
    let interp = (8 * fs_khz) as usize;
    let denom_q16 = div32(1 << 16, 8 * fs_khz);
    let delta0_q13 = rshift_round(
        smulbb(pred_q13[0] - st.pred_prev_q13[0] as i32, denom_q16),
        16,
    );
    let delta1_q13 = rshift_round(
        smulbb(pred_q13[1] - st.pred_prev_q13[1] as i32, denom_q16),
        16,
    );
    let one = |x1: &[i16], x2: &mut [i16], n: usize, p0: i32, p1: i32| {
        let mut sum = lshift(
            add_lshift32(x1[n] as i32 + x1[n + 2] as i32, x1[n + 1] as i32, 1),
            9,
        );
        sum = smlawb(lshift(x2[n + 1] as i32, 8), sum, p0);
        sum = smlawb(sum, lshift(x1[n + 1] as i32, 11), p1);
        x2[n + 1] = sat16(rshift_round(sum, 8)) as i16;
    };
    for n in 0..interp.min(frame_length) {
        pred0_q13 += delta0_q13;
        pred1_q13 += delta1_q13;
        one(x1, x2, n, pred0_q13, pred1_q13);
    }
    pred0_q13 = pred_q13[0];
    pred1_q13 = pred_q13[1];
    for n in interp.min(frame_length)..frame_length {
        one(x1, x2, n, pred0_q13, pred1_q13);
    }
    st.pred_prev_q13 = [pred_q13[0] as i16, pred_q13[1] as i16];
    for n in 0..frame_length {
        let sum = x1[n + 1] as i32 + x2[n + 1] as i32;
        let diff = x1[n + 1] as i32 - x2[n + 1] as i32;
        x1[n + 1] = sat16(sum) as i16;
        x2[n + 1] = sat16(diff) as i16;
    }
}

impl SilkDecoder {
    pub fn new() -> Self {
        SilkDecoder {
            ch: [ChannelState::new(), ChannelState::new()],
            stereo: StereoState::default(),
            n_channels_api: 0,
            n_channels_internal: 0,
            prev_decode_only_middle: false,
            tmp: [vec![0; MAX_FRAME_LENGTH + 2], vec![0; MAX_FRAME_LENGTH + 2]],
            resample_out: vec![0; 960],
        }
    }

    /// `silk_ResetDecoder`
    pub fn reset(&mut self) {
        self.ch[0].reset();
        self.ch[1].reset();
        self.stereo = StereoState::default();
        self.prev_decode_only_middle = false;
    }

    /// `silk_Decode`: decodes one SILK frame (10 or 20 ms) into `out`
    /// (interleaved at the API rate). Returns the number of samples per
    /// channel written.
    pub fn decode(
        &mut self,
        ctrl: &mut SilkDecControl,
        lost_flag: i32,
        new_packet: bool,
        mut dec: Option<&mut RangeDecoder>,
        out: &mut [i16],
    ) -> Result<usize, SilkError> {
        let nci = ctrl.n_channels_internal.clamp(1, 2);
        let napi = ctrl.n_channels_api.clamp(1, 2);
        if new_packet {
            for n in 0..nci {
                self.ch[n].n_frames_decoded = 0;
            }
        }
        if nci > self.n_channels_internal {
            self.ch[1].reset();
        }
        let stereo_to_mono = nci == 1
            && self.n_channels_internal == 2
            && ctrl.internal_sample_rate == 1000 * self.ch[0].fs_khz;

        if self.ch[0].n_frames_decoded == 0 {
            for n in 0..nci {
                let (fpp, nb) = match ctrl.payload_size_ms {
                    0 | 10 => (1, 2),
                    20 => (1, 4),
                    40 => (2, 4),
                    60 => (3, 4),
                    _ => return Err(SilkError),
                };
                self.ch[n].n_frames_per_packet = fpp;
                self.ch[n].nb_subfr = nb;
                let fs_khz_dec = (ctrl.internal_sample_rate >> 10) + 1;
                if fs_khz_dec != 8 && fs_khz_dec != 12 && fs_khz_dec != 16 {
                    return Err(SilkError);
                }
                self.ch[n].set_fs(fs_khz_dec, ctrl.api_sample_rate);
            }
        }
        if napi == 2 && nci == 2 && (self.n_channels_api == 1 || self.n_channels_internal == 1) {
            self.stereo.pred_prev_q13 = [0; 2];
            self.stereo.s_side = [0; 2];
            self.ch[1].resampler = self.ch[0].resampler.clone();
        }
        self.n_channels_api = napi;
        self.n_channels_internal = nci;
        if ctrl.api_sample_rate > 48000 || ctrl.api_sample_rate < 8000 {
            return Err(SilkError);
        }

        if lost_flag != FLAG_PACKET_LOST && self.ch[0].n_frames_decoded == 0 {
            let Some(d) = dec.as_deref_mut() else {
                return Err(SilkError);
            };
            for n in 0..nci {
                for i in 0..self.ch[n].n_frames_per_packet {
                    self.ch[n].vad_flags[i] = d.bit_logp(1);
                }
                self.ch[n].lbrr_flag = d.bit_logp(1);
            }
            for n in 0..nci {
                self.ch[n].lbrr_flags = [false; 3];
                if self.ch[n].lbrr_flag {
                    if self.ch[n].n_frames_per_packet == 1 {
                        self.ch[n].lbrr_flags[0] = true;
                    } else {
                        let icdf: &[u8] = if self.ch[n].n_frames_per_packet == 2 {
                            &SILK_LBRR_FLAGS_2_ICDF
                        } else {
                            &SILK_LBRR_FLAGS_3_ICDF
                        };
                        let sym = d.icdf(icdf, 8) as i32 + 1;
                        for i in 0..self.ch[n].n_frames_per_packet {
                            self.ch[n].lbrr_flags[i] = (sym >> i) & 1 != 0;
                        }
                    }
                }
            }
            if lost_flag == FLAG_DECODE_NORMAL {
                // Skip all LBRR data.
                for i in 0..self.ch[0].n_frames_per_packet {
                    for n in 0..nci {
                        if self.ch[n].lbrr_flags[i] {
                            if nci == 2 && n == 0 {
                                stereo_decode_pred(d);
                                if !self.ch[1].lbrr_flags[i] {
                                    d.icdf(&SILK_STEREO_ONLY_CODE_MID_ICDF, 8);
                                }
                            }
                            let cond = if i > 0 && self.ch[n].lbrr_flags[i - 1] {
                                CODE_CONDITIONALLY
                            } else {
                                CODE_INDEPENDENTLY
                            };
                            self.ch[n].decode_indices(d, i, true, cond);
                            let mut pulses = [0i16; MAX_FRAME_LENGTH];
                            let ind = &self.ch[n].indices;
                            let (st, qo) = (ind.signal_type as i32, ind.quant_offset_type as i32);
                            decode_pulses(d, &mut pulses, st, qo, self.ch[n].frame_length);
                        }
                    }
                }
            }
        }

        let mut ms_pred_q13 = [0i32; 2];
        let mut decode_only_middle = false;
        let nfd = self.ch[0].n_frames_decoded.min(2);
        if nci == 2 {
            if lost_flag == FLAG_DECODE_NORMAL
                || (lost_flag == FLAG_DECODE_LBRR && self.ch[0].lbrr_flags[nfd])
            {
                let Some(d) = dec.as_deref_mut() else {
                    return Err(SilkError);
                };
                ms_pred_q13 = stereo_decode_pred(d);
                if (lost_flag == FLAG_DECODE_NORMAL && !self.ch[1].vad_flags[nfd])
                    || (lost_flag == FLAG_DECODE_LBRR && !self.ch[1].lbrr_flags[nfd])
                {
                    decode_only_middle = d.icdf(&SILK_STEREO_ONLY_CODE_MID_ICDF, 8) != 0;
                } else {
                    decode_only_middle = false;
                }
            } else {
                ms_pred_q13 = [
                    self.stereo.pred_prev_q13[0] as i32,
                    self.stereo.pred_prev_q13[1] as i32,
                ];
            }
        }
        if nci == 2 && !decode_only_middle && self.prev_decode_only_middle {
            let c = &mut self.ch[1];
            c.out_buf = [0; MAX_FRAME_LENGTH + 2 * MAX_SUB_FRAME_LENGTH];
            c.s_lpc_q14_buf = [0; 16];
            c.lag_prev = 100;
            c.last_gain_index = 10;
            c.prev_signal_type = TYPE_NO_VOICE_ACTIVITY;
            c.first_frame_after_reset = true;
        }

        let has_side = if lost_flag == FLAG_DECODE_NORMAL {
            !decode_only_middle
        } else {
            !self.prev_decode_only_middle
                || (nci == 2
                    && lost_flag == FLAG_DECODE_LBRR
                    && self.ch[1].lbrr_flags[self.ch[1].n_frames_decoded.min(2)])
        };

        let flen = self.ch[0].frame_length;
        let mut n_samples_out_dec = flen;
        for n in 0..nci {
            if self.tmp[n].len() < flen + 2 {
                self.tmp[n].resize(flen + 2, 0);
            }
            if n == 0 || has_side {
                let frame_index = self.ch[0].n_frames_decoded as i32 - n as i32;
                let cond = if frame_index <= 0 {
                    CODE_INDEPENDENTLY
                } else if lost_flag == FLAG_DECODE_LBRR {
                    if self.ch[n].lbrr_flags[(frame_index as usize - 1).min(2)] {
                        CODE_CONDITIONALLY
                    } else {
                        CODE_INDEPENDENTLY
                    }
                } else if n > 0 && self.prev_decode_only_middle {
                    CODE_INDEPENDENTLY_NO_LTP_SCALING
                } else {
                    CODE_CONDITIONALLY
                };
                let tmp = &mut self.tmp[n];
                n_samples_out_dec =
                    self.ch[n].decode_frame(dec.as_deref_mut(), &mut tmp[2..], lost_flag, cond);
            } else {
                let tmp = &mut self.tmp[n];
                for v in tmp[2..2 + n_samples_out_dec].iter_mut() {
                    *v = 0;
                }
            }
            self.ch[n].n_frames_decoded += 1;
        }

        let (t0, t1) = self.tmp.split_at_mut(1);
        let t0 = &mut t0[0];
        let t1 = &mut t1[0];
        if napi == 2 && nci == 2 {
            stereo_ms_to_lr(
                &mut self.stereo,
                t0,
                t1,
                &ms_pred_q13,
                self.ch[0].fs_khz,
                n_samples_out_dec,
            );
        } else {
            t0[..2].copy_from_slice(&self.stereo.s_mid);
            self.stereo
                .s_mid
                .copy_from_slice(&t0[n_samples_out_dec..n_samples_out_dec + 2]);
        }

        let fs_khz = self.ch[0].fs_khz.max(1);
        let n_samples_out = (n_samples_out_dec as i64 * ctrl.api_sample_rate as i64
            / (fs_khz as i64 * 1000)) as usize;
        if out.len() < n_samples_out * napi {
            return Err(SilkError);
        }
        if self.resample_out.len() < n_samples_out {
            self.resample_out.resize(n_samples_out, 0);
        }
        for n in 0..napi.min(nci) {
            let src = if n == 0 {
                &t0[1..1 + n_samples_out_dec]
            } else {
                &t1[1..1 + n_samples_out_dec]
            };
            if napi == 2 {
                self.ch[n].resampler.process(&mut self.resample_out, src);
                for i in 0..n_samples_out {
                    out[n + 2 * i] = self.resample_out[i];
                }
            } else {
                self.ch[n].resampler.process(out, src);
            }
        }
        if napi == 2 && nci == 1 {
            if stereo_to_mono {
                self.ch[1]
                    .resampler
                    .process(&mut self.resample_out, &t0[1..1 + n_samples_out_dec]);
                for i in 0..n_samples_out {
                    out[1 + 2 * i] = self.resample_out[i];
                }
            } else {
                for i in 0..n_samples_out {
                    out[1 + 2 * i] = out[2 * i];
                }
            }
        }

        if lost_flag == FLAG_PACKET_LOST {
            for n in 0..self.n_channels_internal {
                self.ch[n].last_gain_index = 10;
            }
        } else {
            self.prev_decode_only_middle = decode_only_middle;
        }
        Ok(n_samples_out)
    }
}
