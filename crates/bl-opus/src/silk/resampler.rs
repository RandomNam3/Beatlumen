//! SILK decoder output resampler (internal 8/12/16 kHz to the API rate).
//!
//! Ported from libopus `silk/resampler.c`, `silk/resampler_private_*.c`:
//!
//! Copyright (c) 2006-2011, Skype Limited. All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use super::macros::*;
use super::tables::*;

const RESAMPLER_DOWN_ORDER_FIR0: usize = 18;
const RESAMPLER_DOWN_ORDER_FIR1: usize = 24;
const RESAMPLER_DOWN_ORDER_FIR2: usize = 36;
const RESAMPLER_ORDER_FIR_12: usize = 8;
const RESAMPLER_MAX_BATCH_SIZE_MS: usize = 10;

static DELAY_MATRIX_DEC: [[u8; 5]; 3] = [[4, 0, 2, 0, 0], [0, 9, 4, 7, 4], [0, 3, 12, 7, 7]];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Func {
    Copy,
    Up2Hq,
    IirFir,
    DownFir,
}

#[derive(Clone)]
pub struct Resampler {
    s_iir: [i32; 6],
    s_fir_i32: [i32; 36],
    s_fir_i16: [i16; 36],
    delay_buf: [i16; 48],
    func: Func,
    batch_size: usize,
    inv_ratio_q16: i32,
    fir_order: usize,
    fir_fracs: i32,
    fs_in_khz: usize,
    fs_out_khz: usize,
    input_delay: usize,
    coefs: &'static [i16],
    buf16: Vec<i16>,
    buf32: Vec<i32>,
}

fn rate_id(r: i32) -> usize {
    (((((r >> 12) - (r > 16000) as i32) >> (r > 24000) as i32) - 1).clamp(0, 4)) as usize
}

impl Default for Resampler {
    fn default() -> Self {
        Resampler {
            s_iir: [0; 6],
            s_fir_i32: [0; 36],
            s_fir_i16: [0; 36],
            delay_buf: [0; 48],
            func: Func::Copy,
            batch_size: 0,
            inv_ratio_q16: 0,
            fir_order: 0,
            fir_fracs: 0,
            fs_in_khz: 0,
            fs_out_khz: 0,
            input_delay: 0,
            coefs: &[],
            buf16: Vec::new(),
            buf32: Vec::new(),
        }
    }
}

impl Resampler {
    /// `silk_resampler_init` (decoder direction).
    pub fn init(&mut self, fs_in: i32, fs_out: i32) {
        *self = Resampler::default();
        if !matches!(fs_in, 8000 | 12000 | 16000)
            || !matches!(fs_out, 8000 | 12000 | 16000 | 24000 | 48000)
        {
            return;
        }
        self.input_delay = DELAY_MATRIX_DEC[rate_id(fs_in)][rate_id(fs_out)] as usize;
        self.fs_in_khz = (fs_in / 1000) as usize;
        self.fs_out_khz = (fs_out / 1000) as usize;
        self.batch_size = self.fs_in_khz * RESAMPLER_MAX_BATCH_SIZE_MS;
        let mut up2x = 0;
        if fs_out > fs_in {
            if fs_out == fs_in * 2 {
                self.func = Func::Up2Hq;
            } else {
                self.func = Func::IirFir;
                up2x = 1;
            }
        } else if fs_out < fs_in {
            self.func = Func::DownFir;
            if fs_out * 4 == fs_in * 3 {
                self.fir_fracs = 3;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR0;
                self.coefs = &SILK_RESAMPLER_3_4_COEFS;
            } else if fs_out * 3 == fs_in * 2 {
                self.fir_fracs = 2;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR0;
                self.coefs = &SILK_RESAMPLER_2_3_COEFS;
            } else if fs_out * 2 == fs_in {
                self.fir_fracs = 1;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR1;
                self.coefs = &SILK_RESAMPLER_1_2_COEFS;
            } else if fs_out * 3 == fs_in {
                self.fir_fracs = 1;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR2;
                self.coefs = &SILK_RESAMPLER_1_3_COEFS;
            } else if fs_out * 4 == fs_in {
                self.fir_fracs = 1;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR2;
                self.coefs = &SILK_RESAMPLER_1_4_COEFS;
            } else if fs_out * 6 == fs_in {
                self.fir_fracs = 1;
                self.fir_order = RESAMPLER_DOWN_ORDER_FIR2;
                self.coefs = &SILK_RESAMPLER_1_6_COEFS;
            } else {
                self.func = Func::Copy;
            }
        } else {
            self.func = Func::Copy;
        }
        self.inv_ratio_q16 = lshift(div32(lshift(fs_in, 14 + up2x), fs_out), 2);
        while smulww(self.inv_ratio_q16, fs_out) < lshift(fs_in, up2x) {
            self.inv_ratio_q16 += 1;
        }
    }

    /// Resamples `input` (at least 1 ms) into `out`; returns samples written.
    pub fn process(&mut self, out: &mut [i16], input: &[i16]) -> usize {
        let in_len = input.len();
        let fs_in = self.fs_in_khz;
        if fs_in == 0 || in_len < fs_in || self.input_delay > fs_in {
            return 0;
        }
        let n_samples = fs_in - self.input_delay;
        self.delay_buf[self.input_delay..fs_in].copy_from_slice(&input[..n_samples]);
        let delay: Vec<i16> = self.delay_buf[..fs_in].to_vec();
        let rest = &input[n_samples..in_len - self.input_delay];
        let mut written;
        match self.func {
            Func::Up2Hq => {
                up2_hq(&mut self.s_iir, out, &delay);
                up2_hq(&mut self.s_iir, &mut out[self.fs_out_khz..], rest);
                written = 2 * in_len;
            }
            Func::IirFir => {
                self.iir_fir(out, &delay);
                written = self.fs_out_khz;
                written += self.iir_fir(&mut out[self.fs_out_khz..], rest);
            }
            Func::DownFir => {
                self.down_fir(out, &delay);
                written = self.fs_out_khz;
                written += self.down_fir(&mut out[self.fs_out_khz..], rest);
            }
            Func::Copy => {
                out[..fs_in].copy_from_slice(&delay);
                out[self.fs_out_khz..self.fs_out_khz + in_len - fs_in]
                    .copy_from_slice(&input[n_samples..n_samples + in_len - fs_in]);
                written = in_len;
            }
        }
        let d = self.input_delay;
        self.delay_buf[..d].copy_from_slice(&input[in_len - d..]);
        written
    }

    fn iir_fir(&mut self, mut out: &mut [i16], mut input: &[i16]) -> usize {
        let batch = self.batch_size;
        let need = 2 * batch + RESAMPLER_ORDER_FIR_12;
        if self.buf16.len() < need {
            self.buf16.resize(need, 0);
        }
        let mut buf = std::mem::take(&mut self.buf16);
        buf[..RESAMPLER_ORDER_FIR_12].copy_from_slice(&self.s_fir_i16[..RESAMPLER_ORDER_FIR_12]);
        let index_increment_q16 = self.inv_ratio_q16;
        let mut total = 0usize;
        let mut n_samples_in;
        loop {
            n_samples_in = input.len().min(batch);
            up2_hq(
                &mut self.s_iir,
                &mut buf[RESAMPLER_ORDER_FIR_12..],
                &input[..n_samples_in],
            );
            let max_index_q16 = lshift(n_samples_in as i32, 16 + 1);
            let mut index_q16 = 0i32;
            let mut o = 0usize;
            while index_q16 < max_index_q16 {
                let table_index = smulwb(index_q16 & 0xFFFF, 12) as usize;
                let bp = &buf[(index_q16 >> 16) as usize..];
                let f = &SILK_RESAMPLER_FRAC_FIR_12[table_index];
                let g = &SILK_RESAMPLER_FRAC_FIR_12[11 - table_index];
                let mut res_q15 = smulbb(bp[0] as i32, f[0] as i32);
                res_q15 = smlabb(res_q15, bp[1] as i32, f[1] as i32);
                res_q15 = smlabb(res_q15, bp[2] as i32, f[2] as i32);
                res_q15 = smlabb(res_q15, bp[3] as i32, f[3] as i32);
                res_q15 = smlabb(res_q15, bp[4] as i32, g[3] as i32);
                res_q15 = smlabb(res_q15, bp[5] as i32, g[2] as i32);
                res_q15 = smlabb(res_q15, bp[6] as i32, g[1] as i32);
                res_q15 = smlabb(res_q15, bp[7] as i32, g[0] as i32);
                out[o] = sat16(rshift_round(res_q15, 15)) as i16;
                o += 1;
                index_q16 = index_q16.wrapping_add(index_increment_q16);
            }
            out = &mut out[o..];
            total += o;
            input = &input[n_samples_in..];
            if !input.is_empty() {
                buf.copy_within(
                    n_samples_in << 1..(n_samples_in << 1) + RESAMPLER_ORDER_FIR_12,
                    0,
                );
            } else {
                break;
            }
        }
        self.s_fir_i16[..RESAMPLER_ORDER_FIR_12]
            .copy_from_slice(&buf[n_samples_in << 1..(n_samples_in << 1) + RESAMPLER_ORDER_FIR_12]);
        self.buf16 = buf;
        total
    }

    fn down_fir(&mut self, mut out: &mut [i16], mut input: &[i16]) -> usize {
        let batch = self.batch_size;
        let order = self.fir_order;
        let need = batch + order;
        if self.buf32.len() < need {
            self.buf32.resize(need, 0);
        }
        let mut buf = std::mem::take(&mut self.buf32);
        buf[..order].copy_from_slice(&self.s_fir_i32[..order]);
        let coefs = self.coefs;
        let fir = &coefs[2..];
        let index_increment_q16 = self.inv_ratio_q16;
        let mut total = 0usize;
        let mut n_samples_in;
        loop {
            n_samples_in = input.len().min(batch);
            // Second-order AR filter (output in Q8)
            for k in 0..n_samples_in {
                let out32 = add_lshift32(self.s_iir[0], input[k] as i32, 8);
                buf[order + k] = out32;
                let out32 = lshift(out32, 2);
                self.s_iir[0] = smlawb(self.s_iir[1], out32, coefs[0] as i32);
                self.s_iir[1] = smulwb(out32, coefs[1] as i32);
            }
            let max_index_q16 = lshift(n_samples_in as i32, 16);
            let mut index_q16 = 0i32;
            let mut o = 0usize;
            while index_q16 < max_index_q16 {
                let bp = &buf[(index_q16 >> 16) as usize..];
                let res_q6 = match order {
                    RESAMPLER_DOWN_ORDER_FIR0 => {
                        let interpol_ind = smulwb(index_q16 & 0xFFFF, self.fir_fracs) as usize;
                        let ip = &fir[RESAMPLER_DOWN_ORDER_FIR0 / 2 * interpol_ind..];
                        let mut r = smulwb(bp[0], ip[0] as i32);
                        for t in 1..9 {
                            r = smlawb(r, bp[t], ip[t] as i32);
                        }
                        let ip = &fir[RESAMPLER_DOWN_ORDER_FIR0 / 2
                            * (self.fir_fracs as usize - 1 - interpol_ind)..];
                        for t in 0..9 {
                            r = smlawb(r, bp[17 - t], ip[t] as i32);
                        }
                        r
                    }
                    RESAMPLER_DOWN_ORDER_FIR1 => {
                        let mut r = smulwb(add32(bp[0], bp[23]), fir[0] as i32);
                        for t in 1..12 {
                            r = smlawb(r, add32(bp[t], bp[23 - t]), fir[t] as i32);
                        }
                        r
                    }
                    _ => {
                        let mut r = smulwb(add32(bp[0], bp[35]), fir[0] as i32);
                        for t in 1..18 {
                            r = smlawb(r, add32(bp[t], bp[35 - t]), fir[t] as i32);
                        }
                        r
                    }
                };
                out[o] = sat16(rshift_round(res_q6, 6)) as i16;
                o += 1;
                index_q16 = index_q16.wrapping_add(index_increment_q16);
            }
            out = &mut out[o..];
            total += o;
            input = &input[n_samples_in..];
            if input.len() > 1 {
                buf.copy_within(n_samples_in..n_samples_in + order, 0);
            } else {
                break;
            }
        }
        self.s_fir_i32[..order].copy_from_slice(&buf[n_samples_in..n_samples_in + order]);
        self.buf32 = buf;
        total
    }
}

/// Upsample by a factor 2, high quality (`silk_resampler_private_up2_HQ`).
fn up2_hq(s: &mut [i32; 6], out: &mut [i16], input: &[i16]) {
    let c0 = &SILK_RESAMPLER_UP2_HQ_0;
    let c1 = &SILK_RESAMPLER_UP2_HQ_1;
    for (k, &x) in input.iter().enumerate() {
        let in32 = lshift(x as i32, 10);
        // even output sample
        let y = sub32(in32, s[0]);
        let xx = smulwb(y, c0[0] as i32);
        let out32_1 = add32(s[0], xx);
        s[0] = add32(in32, xx);
        let y = sub32(out32_1, s[1]);
        let xx = smulwb(y, c0[1] as i32);
        let out32_2 = add32(s[1], xx);
        s[1] = add32(out32_1, xx);
        let y = sub32(out32_2, s[2]);
        let xx = smlawb(y, y, c0[2] as i32);
        let out32_1 = add32(s[2], xx);
        s[2] = add32(out32_2, xx);
        out[2 * k] = sat16(rshift_round(out32_1, 10)) as i16;
        // odd output sample
        let y = sub32(in32, s[3]);
        let xx = smulwb(y, c1[0] as i32);
        let out32_1 = add32(s[3], xx);
        s[3] = add32(in32, xx);
        let y = sub32(out32_1, s[4]);
        let xx = smulwb(y, c1[1] as i32);
        let out32_2 = add32(s[4], xx);
        s[4] = add32(out32_1, xx);
        let y = sub32(out32_2, s[5]);
        let xx = smlawb(y, y, c1[2] as i32);
        let out32_1 = add32(s[5], xx);
        s[5] = add32(out32_2, xx);
        out[2 * k + 1] = sat16(rshift_round(out32_1, 10)) as i16;
    }
}
