//! Apple Lossless (ALAC) decoder.
//!
//! Ported from Apple's ALAC reference decoder (`ALACDecoder.cpp`,
//! `ag_dec.c`, `dp_dec.c`, `matrix_dec.c`):
//!
//! > Copyright (c) 2011 Apple Inc. All rights reserved.
//! > Licensed under the Apache License, Version 2.0 (the "License"); you may
//! > not use this file except in compliance with the License. You may obtain
//! > a copy of the License at <http://www.apache.org/licenses/LICENSE-2.0>
//! > Unless required by applicable law or agreed to in writing, software
//! > distributed under the License is distributed on an "AS IS" BASIS,
//! > WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//!
//! Changes from the reference: rewritten in safe Rust, all arithmetic made
//! explicitly wrapping, bounds/overrun checks that turn corrupted input
//! into errors, `f32` output in WAVE channel order (the ALAC channel
//! layouts are remapped), 16/20/24/32-bit support.

use crate::Error;

const QBSHIFT: u32 = 9;
const QB: u32 = 1 << QBSHIFT;
const MMULSHIFT: u32 = 2;
const MDENSHIFT: u32 = QBSHIFT - MMULSHIFT - 1;
const MOFF: u32 = 1 << (MDENSHIFT - 2);
const BITOFF: i32 = 24;
const MAX_PREFIX_16: u32 = 9;
const MAX_PREFIX_32: u32 = 9;
const MAX_DATATYPE_BITS_16: u32 = 16;
const N_MAX_MEAN_CLAMP: u32 = 0xffff;
const N_MEAN_CLAMP_VAL: u32 = 0xffff;

const ID_SCE: u32 = 0;
const ID_CPE: u32 = 1;
const ID_CCE: u32 = 2;
const ID_LFE: u32 = 3;
const ID_DSE: u32 = 4;
const ID_PCE: u32 = 5;
const ID_FIL: u32 = 6;
const ID_END: u32 = 7;

const MAX_FRAME_LENGTH: u32 = 1 << 18;

/// ALAC channel order index -> WAVE order position, per channel count.
const CHANNEL_MAPS: [&[usize]; 8] = [
    &[0],
    &[0, 1],
    &[2, 0, 1],                // C L R
    &[2, 0, 1, 3],             // C L R Cs
    &[2, 0, 1, 3, 4],          // C L R Ls Rs
    &[2, 0, 1, 4, 5, 3],       // C L R Ls Rs LFE
    &[2, 0, 1, 4, 5, 6, 3],    // C L R Ls Rs Cs LFE
    &[2, 6, 7, 0, 1, 4, 5, 3], // C Lc Rc L R Ls Rs LFE
];

fn err(msg: &str) -> Error {
    Error::InvalidData(format!("alac: {msg}"))
}

/// MSB-first bit reader; reads past the end yield zero bits and are
/// detected with [`BitReader::overrun`].
#[derive(Clone)]
struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        BitReader { data, pos: 0 }
    }
    #[inline]
    fn peek32(&self) -> u32 {
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        if byte + 8 <= self.data.len() {
            let v = u64::from_be_bytes(self.data[byte..byte + 8].try_into().unwrap());
            ((v << sh) >> 32) as u32
        } else {
            let mut v = 0u64;
            for i in 0..8 {
                v = (v << 8) | *self.data.get(byte + i).unwrap_or(&0) as u64;
            }
            ((v << sh) >> 32) as u32
        }
    }
    #[inline]
    fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let n = n.min(32);
        let v = self.peek32() >> (32 - n);
        self.pos += n as usize;
        v
    }
    #[inline]
    fn skip(&mut self, n: usize) {
        self.pos = self.pos.saturating_add(n);
    }
    fn byte_align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
    fn bits_total(&self) -> usize {
        self.data.len() * 8
    }
    fn overrun(&self) -> bool {
        self.pos > self.bits_total()
    }
}

/// Decoder configuration (`ALACSpecificConfig`).
#[derive(Debug, Clone, Copy)]
pub struct AlacConfig {
    pub frame_length: u32,
    pub compatible_version: u8,
    pub bit_depth: u8,
    pub pb: u8,
    pub mb: u8,
    pub kb: u8,
    pub num_channels: u8,
    pub max_run: u16,
    pub max_frame_bytes: u32,
    pub avg_bit_rate: u32,
    pub sample_rate: u32,
}

impl AlacConfig {
    /// Parses a magic cookie: a bare 24-byte `ALACSpecificConfig`, optionally
    /// preceded by `frma` and/or `alac` atom headers (QuickTime style).
    pub fn parse(cookie: &[u8]) -> Result<AlacConfig, Error> {
        let mut c = cookie;
        if c.len() >= 12 && &c[4..8] == b"frma" {
            c = &c[12..];
        }
        if c.len() >= 12 && &c[4..8] == b"alac" {
            c = &c[12..];
        }
        if c.len() < 24 {
            return Err(err("magic cookie too short"));
        }
        let be32 = |i: usize| u32::from_be_bytes(c[i..i + 4].try_into().unwrap());
        let cfg = AlacConfig {
            frame_length: be32(0),
            compatible_version: c[4],
            bit_depth: c[5],
            pb: c[6],
            mb: c[7],
            kb: c[8],
            num_channels: c[9],
            max_run: u16::from_be_bytes([c[10], c[11]]),
            max_frame_bytes: be32(12),
            avg_bit_rate: be32(16),
            sample_rate: be32(20),
        };
        if cfg.compatible_version > 0 {
            return Err(Error::Unsupported(format!(
                "alac: compatible version {}",
                cfg.compatible_version
            )));
        }
        if !matches!(cfg.bit_depth, 16 | 20 | 24 | 32) {
            return Err(Error::Unsupported(format!(
                "alac: {}-bit samples",
                cfg.bit_depth
            )));
        }
        if cfg.num_channels == 0 || cfg.num_channels > 8 {
            return Err(err("bad channel count"));
        }
        if cfg.frame_length == 0 || cfg.frame_length > MAX_FRAME_LENGTH {
            return Err(err("bad frame length"));
        }
        if cfg.kb == 0 || cfg.kb > 31 {
            return Err(err("bad kb parameter"));
        }
        Ok(cfg)
    }
}

/// Adaptive Golomb decoding parameters.
struct AgParams {
    mb0: u32,
    pb: u32,
    kb: u32,
    wb: u32,
}

#[inline]
fn mask(k: u32) -> u32 {
    if k >= 32 {
        u32::MAX
    } else {
        (1u32 << k) - 1
    }
}

#[inline]
fn lead(m: u32) -> u32 {
    m.leading_zeros()
}

#[inline]
fn lg3a(x: u32) -> u32 {
    31 - lead(x.wrapping_add(3))
}

#[inline]
fn dyn_get(br: &mut BitReader, m: u32, k: u32) -> u32 {
    let stream = br.peek32();
    let pre = lead(!stream);
    if pre >= MAX_PREFIX_16 {
        br.skip(MAX_PREFIX_16 as usize);
        br.read(MAX_DATATYPE_BITS_16)
    } else {
        let s = stream << (pre + 1);
        let v = if k == 0 { 0 } else { s >> (32 - k) };
        if v < 2 {
            br.skip((pre + k) as usize);
            pre.wrapping_mul(m)
        } else {
            br.skip((pre + 1 + k) as usize);
            pre.wrapping_mul(m).wrapping_add(v - 1)
        }
    }
}

#[inline]
fn dyn_get_32bit(br: &mut BitReader, m: u32, k: u32, maxbits: u32) -> u32 {
    let stream = br.peek32();
    let mut result = lead(!stream);
    if result >= MAX_PREFIX_32 {
        br.skip(MAX_PREFIX_32 as usize);
        br.read(maxbits)
    } else {
        br.skip(result as usize + 1);
        if k != 1 {
            let s = stream << (result + 1);
            let v = if k == 0 { 0 } else { s >> (32 - k) };
            br.skip(k as usize - 1);
            result = result.wrapping_mul(m);
            if v >= 2 {
                result = result.wrapping_add(v - 1);
                br.skip(1);
            }
        }
        result
    }
}

fn dyn_decomp(
    p: &AgParams,
    br: &mut BitReader,
    pc: &mut [i32],
    num_samples: usize,
    max_size: u32,
) -> Result<(), Error> {
    let total_bits = br.bits_total();
    let mut mb = p.mb0;
    let mut zmode = 0u32;
    let mut c = 0usize;
    while c < num_samples {
        if br.pos >= total_bits {
            return Err(err("bitstream overrun"));
        }
        let m0 = mb >> QBSHIFT;
        let k = lg3a(m0).min(p.kb);
        let m = mask(k);
        let n = dyn_get_32bit(br, m, k, max_size);
        let ndecode = n.wrapping_add(zmode);
        let multiplier = (-((ndecode & 1) as i32)) | 1;
        let del = ((ndecode.wrapping_add(1) >> 1) as i32).wrapping_mul(multiplier);
        pc[c] = del;
        c += 1;
        mb =
            p.pb.wrapping_mul(n.wrapping_add(zmode))
                .wrapping_add(mb)
                .wrapping_sub(p.pb.wrapping_mul(mb) >> QBSHIFT);
        if n > N_MAX_MEAN_CLAMP {
            mb = N_MEAN_CLAMP_VAL;
        }
        zmode = 0;
        if (mb << MMULSHIFT) < QB && c < num_samples {
            zmode = 1;
            let k = (lead(mb) as i32 - BITOFF + ((mb.wrapping_add(MOFF)) >> MDENSHIFT) as i32)
                .clamp(0, 31) as u32;
            let mz = mask(k) & p.wb;
            let n = dyn_get(br, mz, k) as usize;
            if c + n > num_samples {
                return Err(err("zero run too long"));
            }
            pc[c..c + n].fill(0);
            c += n;
            if n >= 65535 {
                zmode = 0;
            }
            mb = 0;
        }
    }
    if br.overrun() {
        return Err(err("bitstream overrun"));
    }
    Ok(())
}

#[inline]
fn sign_of(i: i32) -> i32 {
    (i > 0) as i32 - (i < 0) as i32
}

#[inline]
fn sext(v: i32, shift: u32) -> i32 {
    if shift == 0 || shift >= 32 {
        v
    } else {
        (v << shift) >> shift
    }
}

/// The special "numactive == 31" first-order predictor, in place.
fn unpc_first_order(buf: &mut [i32], num: usize, chanbits: u32) {
    let chanshift = 32u32.wrapping_sub(chanbits);
    let mut prev = buf[0];
    for v in buf.iter_mut().take(num).skip(1) {
        prev = sext(v.wrapping_add(prev), chanshift);
        *v = prev;
    }
}

/// Dynamic predictor reconstruction (`unpc_block`).
fn unpc_block(
    pc1: &[i32],
    out: &mut [i32],
    num: usize,
    coefs: &mut [i16],
    numactive: usize,
    chanbits: u32,
    denshift: u32,
) {
    if num == 0 {
        return;
    }
    let chanshift = 32u32.wrapping_sub(chanbits);
    let denhalf: i32 = if denshift == 0 {
        0
    } else {
        1i32.wrapping_shl(denshift - 1)
    };
    out[0] = pc1[0];
    if numactive == 0 {
        out[1..num].copy_from_slice(&pc1[1..num]);
        return;
    }
    let warm = numactive.min(num - 1);
    for j in 1..=warm {
        out[j] = sext(pc1[j].wrapping_add(out[j - 1]), chanshift);
    }
    let lim = numactive + 1;
    // Coefficients in reverse order so that `rc[i]` pairs with `w[i + 1]`
    // where `w = out[j - lim..j]` (w[0] is the "top" sample).
    let mut rc = [0i16; 32];
    for i in 0..numactive {
        rc[i] = coefs[numactive - 1 - i];
    }
    let rc = &mut rc[..numactive];
    for j in lim..num {
        let w = &out[j - lim..j];
        let top = w[0];
        let hist = &w[1..];
        let mut sum1: i32 = 0;
        for (&c, &h) in rc.iter().zip(hist) {
            sum1 = sum1.wrapping_add((c as i32).wrapping_mul(h.wrapping_sub(top)));
        }
        let del_in = pc1[j];
        let mut del0 = del_in;
        let sg = sign_of(del_in);
        let del = del_in
            .wrapping_add(top)
            .wrapping_add(sum1.wrapping_add(denhalf).wrapping_shr(denshift));
        if sg > 0 {
            for (i, (c, &h)) in rc.iter_mut().zip(hist).enumerate() {
                let dd = top.wrapping_sub(h);
                let sgn = sign_of(dd);
                *c = c.wrapping_sub(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((i + 1) as i32).wrapping_mul(sgn.wrapping_mul(dd).wrapping_shr(denshift)),
                );
                if del0 <= 0 {
                    break;
                }
            }
        } else if sg < 0 {
            for (i, (c, &h)) in rc.iter_mut().zip(hist).enumerate() {
                let dd = top.wrapping_sub(h);
                let sgn = sign_of(dd);
                *c = c.wrapping_add(sgn as i16);
                del0 = del0.wrapping_sub(
                    ((i + 1) as i32).wrapping_mul((-sgn).wrapping_mul(dd).wrapping_shr(denshift)),
                );
                if del0 >= 0 {
                    break;
                }
            }
        }
        out[j] = sext(del, chanshift);
    }
    for i in 0..numactive {
        coefs[numactive - 1 - i] = rc[i];
    }
}

/// Per-channel predictor parameters read from an element header.
struct ChannelParams {
    mode: u32,
    den_shift: u32,
    pb_factor: u32,
    num: usize,
    coefs: [i16; 32],
}

fn read_channel_params(br: &mut BitReader) -> ChannelParams {
    let h = br.read(8);
    let mode = h >> 4;
    let den_shift = h & 0xf;
    let h = br.read(8);
    let pb_factor = h >> 5;
    let num = (h & 0x1f) as usize;
    let mut coefs = [0i16; 32];
    for c in coefs.iter_mut().take(num) {
        *c = br.read(16) as u16 as i16;
    }
    ChannelParams {
        mode,
        den_shift,
        pb_factor,
        num,
        coefs,
    }
}

/// Apple Lossless decoder producing interleaved `f32` in WAVE order.
pub struct AlacDecoder {
    cfg: AlacConfig,
    mix_u: Vec<i32>,
    mix_v: Vec<i32>,
    predictor: Vec<i32>,
    shift_buf: Vec<u16>,
    /// Decoded samples in ALAC channel order (interleaved).
    samples: Vec<i32>,
}

impl AlacDecoder {
    /// Creates a decoder from an ALAC magic cookie (`ALACSpecificConfig`,
    /// as stored in the MP4 `alac` box or Matroska CodecPrivate).
    pub fn new(cookie: &[u8]) -> Result<AlacDecoder, Error> {
        let cfg = AlacConfig::parse(cookie)?;
        let n = cfg.frame_length as usize;
        Ok(AlacDecoder {
            cfg,
            mix_u: vec![0; n],
            mix_v: vec![0; n],
            predictor: vec![0; n],
            shift_buf: vec![0; n * 2],
            samples: vec![0; n * cfg.num_channels as usize],
        })
    }

    pub fn config(&self) -> &AlacConfig {
        &self.cfg
    }
    pub fn sample_rate(&self) -> u32 {
        self.cfg.sample_rate
    }
    pub fn channels(&self) -> u16 {
        self.cfg.num_channels as u16
    }
    pub fn bit_depth(&self) -> u32 {
        self.cfg.bit_depth as u32
    }
    pub fn frame_length(&self) -> u32 {
        self.cfg.frame_length
    }

    /// Decodes one ALAC packet, appending interleaved samples to `out`.
    /// Returns the number of PCM frames appended.
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let nch = self.cfg.num_channels as usize;
        let n = self.decode_ints(packet)?;
        let scale = 1.0 / (1u64 << (self.cfg.bit_depth - 1)) as f32;
        let map = CHANNEL_MAPS[nch - 1];
        let start = out.len();
        out.resize(start + n * nch, 0.0);
        let dst = &mut out[start..];
        for (frame_in, frame_out) in self.samples[..n * nch]
            .chunks_exact(nch)
            .zip(dst.chunks_exact_mut(nch))
        {
            for (c, &v) in frame_in.iter().enumerate() {
                frame_out[map[c]] = v as f32 * scale;
            }
        }
        Ok(n)
    }

    /// Decodes a packet into `self.samples` (ALAC order, sign-correct values
    /// at the configured bit depth). Returns the number of frames.
    fn decode_ints(&mut self, packet: &[u8]) -> Result<usize, Error> {
        let cfg = self.cfg;
        let nch = cfg.num_channels as usize;
        let bit_depth = cfg.bit_depth as u32;
        let mut br = BitReader::new(packet);
        let mut channel_index = 0usize;
        let mut num_samples = cfg.frame_length as usize;
        let mut out_samples = num_samples;
        loop {
            if br.pos >= br.bits_total() {
                return Err(err("unexpected end of packet"));
            }
            let tag = br.read(3);
            match tag {
                ID_SCE | ID_LFE | ID_CPE => {
                    let pair = tag == ID_CPE;
                    if channel_index + if pair { 2 } else { 1 } > nch {
                        break;
                    }
                    let _instance = br.read(4);
                    if br.read(12) != 0 {
                        return Err(err("bad element header"));
                    }
                    let header = br.read(4);
                    let partial = header >> 3;
                    let bytes_shifted = (header >> 1) & 3;
                    if bytes_shifted == 3 {
                        return Err(err("bad shift"));
                    }
                    let escape = header & 1;
                    if partial != 0 {
                        let ns = br.read(32) as usize;
                        if ns > cfg.frame_length as usize {
                            return Err(err("partial frame too long"));
                        }
                        num_samples = ns;
                    }
                    let shift_bits_count = bytes_shifted * 8;
                    let chan_bits = (bit_depth.saturating_sub(shift_bits_count)) + pair as u32;
                    let (mix_bits, mix_res, shifted);
                    if escape == 0 {
                        mix_bits = br.read(8);
                        mix_res = br.read(8) as u8 as i8 as i32;
                        let mut pu = read_channel_params(&mut br);
                        let mut pv = if pair {
                            Some(read_channel_params(&mut br))
                        } else {
                            None
                        };
                        let shift_reader = if bytes_shifted != 0 {
                            let sr = br.clone();
                            br.skip(
                                shift_bits_count as usize * num_samples * if pair { 2 } else { 1 },
                            );
                            Some(sr)
                        } else {
                            None
                        };
                        self.decode_channel(&mut br, &mut pu, false, num_samples, chan_bits)?;
                        if let Some(pv) = pv.as_mut() {
                            self.decode_channel(&mut br, pv, true, num_samples, chan_bits)?;
                        }
                        shifted = shift_reader;
                    } else {
                        // Uncompressed escape frame.
                        let cb = if pair { bit_depth } else { chan_bits };
                        let shift = 32u32.saturating_sub(cb);
                        let read_sample = |br: &mut BitReader| -> i32 {
                            if cb <= 16 {
                                sext(br.read(cb) as i32, shift)
                            } else {
                                let hi = ((br.read(16) as i32) << 16).wrapping_shr(shift);
                                hi | br.read(cb - 16) as i32
                            }
                        };
                        for i in 0..num_samples {
                            self.mix_u[i] = read_sample(&mut br);
                            if pair {
                                self.mix_v[i] = read_sample(&mut br);
                            }
                        }
                        mix_bits = 0;
                        mix_res = 0;
                        shifted = None;
                    }
                    if br.overrun() {
                        return Err(err("bitstream overrun"));
                    }
                    let bytes_shifted = if shifted.is_some() { bytes_shifted } else { 0 };
                    if let Some(mut sr) = shifted {
                        let count = num_samples * if pair { 2 } else { 1 };
                        for v in &mut self.shift_buf[..count] {
                            *v = sr.read(bytes_shifted * 8) as u16;
                        }
                    }
                    self.output(
                        channel_index,
                        pair,
                        num_samples,
                        mix_bits,
                        mix_res,
                        bytes_shifted,
                    );
                    channel_index += if pair { 2 } else { 1 };
                    out_samples = num_samples;
                }
                ID_CCE | ID_PCE => return Err(Error::Unsupported("alac: CCE/PCE elements".into())),
                ID_DSE => {
                    let _instance = br.read(4);
                    let align = br.read(1);
                    let mut count = br.read(8);
                    if count == 255 {
                        count += br.read(8);
                    }
                    if align != 0 {
                        br.byte_align();
                    }
                    br.skip(count as usize * 8);
                    if br.overrun() {
                        return Err(err("bitstream overrun"));
                    }
                }
                ID_FIL => {
                    let mut count = br.read(4) as i32;
                    if count == 15 {
                        count += br.read(8) as i32 - 1;
                    }
                    br.skip(count.max(0) as usize * 8);
                    if br.overrun() {
                        return Err(err("bitstream overrun"));
                    }
                }
                _ => {
                    debug_assert_eq!(tag, ID_END);
                    br.byte_align();
                    break;
                }
            }
            if channel_index >= nch {
                break;
            }
        }
        // Missing channels are filled with silence.
        for c in channel_index..nch {
            for f in 0..out_samples {
                self.samples[f * nch + c] = 0;
            }
        }
        Ok(out_samples)
    }

    /// Entropy-decodes and runs the predictor for one channel of an element.
    fn decode_channel(
        &mut self,
        br: &mut BitReader,
        p: &mut ChannelParams,
        second: bool,
        n: usize,
        chan_bits: u32,
    ) -> Result<(), Error> {
        let cfg = self.cfg;
        let ag = AgParams {
            mb0: cfg.mb as u32,
            pb: (cfg.pb as u32 * p.pb_factor) / 4,
            kb: cfg.kb as u32,
            wb: mask(cfg.kb as u32),
        };
        dyn_decomp(&ag, br, &mut self.predictor[..n], n, chan_bits)?;
        if p.mode != 0 {
            unpc_first_order(&mut self.predictor, n, chan_bits);
        }
        let mix = if second {
            &mut self.mix_v
        } else {
            &mut self.mix_u
        };
        unpc_block(
            &self.predictor,
            mix,
            n,
            &mut p.coefs,
            p.num,
            chan_bits,
            p.den_shift,
        );
        Ok(())
    }

    /// Un-mixes / shifts the decoded element and stores it in `samples`.
    fn output(
        &mut self,
        ch: usize,
        pair: bool,
        n: usize,
        mix_bits: u32,
        mix_res: i32,
        bytes_shifted: u32,
    ) {
        let nch = self.cfg.num_channels as usize;
        let depth = self.cfg.bit_depth as u32;
        let shift = bytes_shifted * 8;
        let finish = |v: i32, low: u16| -> i32 {
            let v = if shift > 0 {
                (v << shift) | low as i32
            } else {
                v
            };
            match depth {
                16 => v as i16 as i32,
                20 => sext(v, 12),
                24 => sext(v, 8),
                _ => v,
            }
        };
        if pair {
            for j in 0..n {
                let (u, v) = (self.mix_u[j], self.mix_v[j]);
                let (l, r) = if mix_res != 0 {
                    let l = u
                        .wrapping_add(v)
                        .wrapping_sub(mix_res.wrapping_mul(v).wrapping_shr(mix_bits));
                    (l, l.wrapping_sub(v))
                } else {
                    (u, v)
                };
                self.samples[j * nch + ch] = finish(l, self.shift_buf[2 * j]);
                self.samples[j * nch + ch + 1] = finish(r, self.shift_buf[2 * j + 1]);
            }
        } else {
            for j in 0..n {
                self.samples[j * nch + ch] = finish(self.mix_u[j], self.shift_buf[j]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cookie(channels: u8, depth: u8) -> Vec<u8> {
        let mut c = 4096u32.to_be_bytes().to_vec();
        c.extend_from_slice(&[0, depth, 40, 10, 14, channels]);
        c.extend_from_slice(&255u16.to_be_bytes());
        c.extend_from_slice(&0u32.to_be_bytes());
        c.extend_from_slice(&0u32.to_be_bytes());
        c.extend_from_slice(&44100u32.to_be_bytes());
        c
    }

    #[test]
    fn config_validation() {
        assert!(AlacDecoder::new(&cookie(2, 16)).is_ok());
        assert!(AlacDecoder::new(&cookie(9, 16)).is_err());
        assert!(AlacDecoder::new(&cookie(2, 12)).is_err());
        assert!(AlacDecoder::new(&cookie(2, 16)[..20]).is_err());
    }

    #[test]
    fn escape_frame() {
        // Mono 16-bit uncompressed partial frame with 3 samples.
        let mut bits: Vec<(u32, u32)> = vec![(ID_SCE, 3), (0, 4), (0, 12), (0b1001, 4), (3, 32)];
        for s in [1000i32, -2, 32767] {
            bits.push((s as u32 & 0xffff, 16));
        }
        bits.push((ID_END, 3));
        let mut data = Vec::new();
        let (mut acc, mut n) = (0u64, 0u32);
        for (v, w) in bits {
            acc = (acc << w) | v as u64;
            n += w;
            while n >= 8 {
                data.push((acc >> (n - 8)) as u8);
                n -= 8;
            }
        }
        if n > 0 {
            data.push((acc << (8 - n)) as u8);
        }
        let mut d = AlacDecoder::new(&cookie(1, 16)).unwrap();
        let mut out = Vec::new();
        assert_eq!(d.decode_packet(&data, &mut out).unwrap(), 3);
        assert_eq!(
            out,
            vec![1000.0 / 32768.0, -2.0 / 32768.0, 32767.0 / 32768.0]
        );
    }

    #[test]
    fn garbage_never_panics() {
        let mut d = AlacDecoder::new(&cookie(2, 24)).unwrap();
        let mut seed = 99u32;
        let mut out = Vec::new();
        for len in 0..400usize {
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
                    (seed >> 24) as u8
                })
                .collect();
            out.clear();
            let _ = d.decode_packet(&data, &mut out);
        }
    }
}
