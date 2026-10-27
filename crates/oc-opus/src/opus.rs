//! Single-stream Opus decoder: mode switching between SILK, hybrid and CELT,
//! redundancy (transition) frames, cross-fades and multi-frame packets.
//!
//! Ported from libopus `src/opus_decoder.c`:
//!
//! Copyright (c) 2010 Xiph.Org Foundation, Skype Limited
//! Written by Jean-Marc Valin and Koen Vos
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use crate::celt::decoder::CeltDecoder;
use crate::celt::mode::WINDOW120;
use crate::packet::*;
use crate::range_dec::RangeDecoder;
use crate::silk::{SilkDecControl, SilkDecoder};
use crate::Error;

pub struct StreamDecoder {
    channels: usize,
    fs: u32,
    celt: CeltDecoder,
    silk: SilkDecoder,
    dec_control: SilkDecControl,

    stream_channels: usize,
    bandwidth: i32,
    mode: i32,
    prev_mode: i32,
    frame_size: usize,
    prev_redundancy: bool,
    pub last_packet_duration: usize,
    pub range_final: u32,

    pcm_silk: Vec<i16>,
}

fn smooth_fade(
    in1: &[f32],
    in2: &[f32],
    out: &mut [f32],
    overlap: usize,
    channels: usize,
    fs: u32,
) {
    let inc = (48000 / fs) as usize;
    for c in 0..channels {
        for i in 0..overlap {
            let w = WINDOW120[i * inc] * WINDOW120[i * inc];
            out[i * channels + c] = w * in2[i * channels + c] + (1.0 - w) * in1[i * channels + c];
        }
    }
}

impl StreamDecoder {
    pub fn new(fs: u32, channels: usize) -> Result<Self, Error> {
        if !matches!(fs, 8000 | 12000 | 16000 | 24000 | 48000) || !(1..=2).contains(&channels) {
            return Err(Error::Unsupported("sample rate / channel count"));
        }
        let celt = CeltDecoder::new(fs, channels).ok_or(Error::Unsupported("celt init"))?;
        Ok(StreamDecoder {
            channels,
            fs,
            celt,
            silk: SilkDecoder::new(),
            dec_control: SilkDecControl::new(channels, fs),
            stream_channels: channels,
            bandwidth: 0,
            mode: 0,
            prev_mode: 0,
            frame_size: (fs / 400) as usize,
            prev_redundancy: false,
            last_packet_duration: 0,
            range_final: 0,
            pcm_silk: Vec::new(),
        })
    }

    pub fn channels(&self) -> usize {
        self.channels
    }

    /// `OPUS_RESET_STATE`
    pub fn reset(&mut self) {
        self.celt.reset();
        self.silk.reset();
        self.stream_channels = self.channels;
        self.bandwidth = 0;
        self.mode = 0;
        self.prev_mode = 0;
        self.frame_size = (self.fs / 400) as usize;
        self.prev_redundancy = false;
        self.last_packet_duration = 0;
        self.range_final = 0;
    }

    /// `opus_decode_frame`. `data == None` (or a 0/1-byte frame) runs the
    /// concealment of the previous mode.
    fn decode_frame(
        &mut self,
        data: Option<&[u8]>,
        pcm: &mut [f32],
        frame_size: usize,
        decode_fec: bool,
    ) -> Result<usize, Error> {
        let ch = self.channels;
        let f20 = (self.fs / 50) as usize;
        let f10 = f20 >> 1;
        let f5 = f10 >> 1;
        let f2_5 = f5 >> 1;
        if frame_size < f2_5 {
            return Err(Error::InvalidData("buffer too small"));
        }
        let mut frame_size = frame_size.min((self.fs / 25 * 3) as usize);
        let data = match data {
            Some(d) if d.len() > 1 => Some(d),
            _ => {
                frame_size = frame_size.min(self.frame_size);
                None
            }
        };
        let mut len = data.map(|d| d.len()).unwrap_or(0);
        let mut audiosize;
        let mode;
        let bandwidth;
        if data.is_some() {
            audiosize = self.frame_size;
            mode = self.mode;
            bandwidth = self.bandwidth;
        } else {
            audiosize = frame_size;
            mode = if self.prev_redundancy {
                MODE_CELT_ONLY
            } else {
                self.prev_mode
            };
            bandwidth = 0;
            if mode == 0 {
                pcm[..audiosize * ch].fill(0.0);
                return Ok(audiosize);
            }
            if audiosize > f20 {
                let mut off = 0;
                while audiosize > 0 {
                    let r =
                        self.decode_frame(None, &mut pcm[off * ch..], audiosize.min(f20), false)?;
                    off += r;
                    audiosize = audiosize.saturating_sub(r);
                    if r == 0 {
                        break;
                    }
                }
                return Ok(frame_size);
            } else if audiosize < f20 {
                if audiosize > f10 {
                    audiosize = f10;
                } else if mode != MODE_SILK_ONLY && audiosize > f5 && audiosize < f10 {
                    audiosize = f5;
                }
            }
        }
        let mut dec_storage;
        let mut dec: Option<&mut RangeDecoder> = match data {
            Some(d) => {
                dec_storage = RangeDecoder::new(d);
                Some(&mut dec_storage)
            }
            None => None,
        };

        let mut transition = false;
        if data.is_some()
            && self.prev_mode > 0
            && ((mode == MODE_CELT_ONLY
                && self.prev_mode != MODE_CELT_ONLY
                && !self.prev_redundancy)
                || (mode != MODE_CELT_ONLY && self.prev_mode == MODE_CELT_ONLY))
        {
            transition = true;
        }
        let mut pcm_transition: Vec<f32> = Vec::new();
        if transition && mode == MODE_CELT_ONLY {
            pcm_transition = vec![0.0; f5 * ch];
            let n = f5.min(audiosize);
            self.decode_frame(None, &mut pcm_transition, n, false)?;
        }
        if audiosize > frame_size {
            return Err(Error::InvalidData("frame larger than buffer"));
        }
        frame_size = audiosize;

        // SILK processing
        if mode != MODE_CELT_ONLY {
            let silk_len = f10.max(frame_size) * ch;
            if self.pcm_silk.len() < silk_len {
                self.pcm_silk.resize(silk_len, 0);
            }
            if self.prev_mode == MODE_CELT_ONLY {
                self.silk.reset();
            }
            self.dec_control.payload_size_ms = 10.max(1000 * audiosize as i32 / self.fs as i32);
            if data.is_some() {
                self.dec_control.n_channels_internal = self.stream_channels;
                self.dec_control.internal_sample_rate = if mode == MODE_SILK_ONLY {
                    match bandwidth {
                        BANDWIDTH_NARROWBAND => 8000,
                        BANDWIDTH_MEDIUMBAND => 12000,
                        _ => 16000,
                    }
                } else {
                    16000
                };
            }
            let lost_flag = if data.is_none() {
                1
            } else if decode_fec {
                2
            } else {
                0
            };
            let mut decoded = 0usize;
            while decoded < frame_size {
                let first_frame = decoded == 0;
                let out = &mut self.pcm_silk[decoded * ch..];
                let res = self.silk.decode(
                    &mut self.dec_control,
                    lost_flag,
                    first_frame,
                    dec.as_deref_mut(),
                    out,
                );
                let n = match res {
                    Ok(n) => n,
                    Err(_) => {
                        if lost_flag != 0 {
                            let fs = frame_size;
                            let end = ((decoded + fs) * ch).min(self.pcm_silk.len());
                            for v in self.pcm_silk[decoded * ch..end].iter_mut() {
                                *v = 0;
                            }
                            fs
                        } else {
                            return Err(Error::InvalidData("silk decode error"));
                        }
                    }
                };
                if n == 0 {
                    return Err(Error::InvalidData("silk decoded no samples"));
                }
                decoded += n;
                if decoded > frame_size {
                    break;
                }
            }
        }

        let mut start_band = 0;
        let mut redundancy = false;
        let mut redundancy_bytes = 0usize;
        let mut celt_to_silk = false;
        if !decode_fec && mode != MODE_CELT_ONLY && data.is_some() {
            let d = dec.as_deref_mut().unwrap();
            if d.tell() + 17 + 20 * (mode == MODE_HYBRID) as i32 <= 8 * len as i32 {
                redundancy = if mode == MODE_HYBRID {
                    d.bit_logp(12)
                } else {
                    true
                };
                if redundancy {
                    celt_to_silk = d.bit_logp(1);
                    redundancy_bytes = if mode == MODE_HYBRID {
                        d.uint(256) as usize + 2
                    } else {
                        (len as i32 - ((d.tell() + 7) >> 3)).max(0) as usize
                    };
                    if redundancy_bytes > len || (len - redundancy_bytes) * 8 < d.tell() as usize {
                        len = 0;
                        redundancy_bytes = 0;
                        redundancy = false;
                    } else {
                        len -= redundancy_bytes;
                    }
                    d.storage = d.storage.saturating_sub(redundancy_bytes as u32);
                }
            }
        }
        if mode != MODE_CELT_ONLY {
            start_band = 17;
        }
        if redundancy {
            transition = false;
        }
        if transition && mode != MODE_CELT_ONLY {
            pcm_transition = vec![0.0; f5 * ch];
            let n = f5.min(audiosize);
            self.decode_frame(None, &mut pcm_transition, n, false)?;
        }

        if bandwidth != 0 {
            let endband = match bandwidth {
                BANDWIDTH_NARROWBAND => 13,
                BANDWIDTH_MEDIUMBAND | BANDWIDTH_WIDEBAND => 17,
                BANDWIDTH_SUPERWIDEBAND => 19,
                _ => 21,
            };
            self.celt.end = endband;
        }
        self.celt.stream_channels = self.stream_channels;

        let mut redundant_audio: Vec<f32> = Vec::new();
        let mut redundant_rng = 0u32;
        let data_bytes = data.unwrap_or(&[]);
        if redundancy {
            redundant_audio = vec![0.0; f5 * ch];
        }
        if redundancy && celt_to_silk {
            self.celt.start = 0;
            let rd = &data_bytes[len..len + redundancy_bytes];
            let _ = self.celt.decode(Some(rd), &mut redundant_audio, f5, None);
            redundant_rng = self.celt.rng;
        }
        self.celt.start = start_band;

        let mut celt_err = false;
        if mode != MODE_SILK_ONLY {
            let celt_frame_size = f20.min(frame_size);
            if mode != self.prev_mode && self.prev_mode > 0 && !self.prev_redundancy {
                self.celt.reset();
            }
            let cdata = if decode_fec {
                None
            } else {
                data.map(|d| &d[..len])
            };
            let r = self
                .celt
                .decode(cdata, pcm, celt_frame_size, dec.as_deref_mut());
            if r.is_err() {
                celt_err = true;
            }
        } else {
            pcm[..frame_size * ch].fill(0.0);
            if self.prev_mode == MODE_HYBRID
                && !(redundancy && celt_to_silk && self.prev_redundancy)
            {
                self.celt.start = 0;
                let silence = [0xFFu8, 0xFF];
                let _ = self.celt.decode(Some(&silence), pcm, f2_5, None);
            }
        }

        if mode != MODE_CELT_ONLY {
            for i in 0..frame_size * ch {
                pcm[i] += (1.0f32 / 32768.0) * self.pcm_silk[i] as f32;
            }
        }

        if redundancy && !celt_to_silk {
            self.celt.reset();
            self.celt.start = 0;
            let rd = &data_bytes[len..len + redundancy_bytes];
            let _ = self.celt.decode(Some(rd), &mut redundant_audio, f5, None);
            redundant_rng = self.celt.rng;
            let off = ch * (frame_size - f2_5);
            let tmp: Vec<f32> = pcm[off..off + f2_5 * ch].to_vec();
            smooth_fade(
                &tmp,
                &redundant_audio[ch * f2_5..],
                &mut pcm[off..],
                f2_5,
                ch,
                self.fs,
            );
        }
        if redundancy && celt_to_silk && (self.prev_mode != MODE_SILK_ONLY || self.prev_redundancy)
        {
            pcm[..f2_5 * ch].copy_from_slice(&redundant_audio[..f2_5 * ch]);
            let tmp: Vec<f32> = pcm[ch * f2_5..ch * f5].to_vec();
            smooth_fade(
                &redundant_audio[ch * f2_5..],
                &tmp,
                &mut pcm[ch * f2_5..],
                f2_5,
                ch,
                self.fs,
            );
        }
        if transition {
            if audiosize >= f5 {
                pcm[..ch * f2_5].copy_from_slice(&pcm_transition[..ch * f2_5]);
                let tmp: Vec<f32> = pcm[ch * f2_5..ch * f5].to_vec();
                smooth_fade(
                    &pcm_transition[ch * f2_5..],
                    &tmp,
                    &mut pcm[ch * f2_5..],
                    f2_5,
                    ch,
                    self.fs,
                );
            } else {
                let tmp: Vec<f32> = pcm[..ch * f2_5].to_vec();
                smooth_fade(&pcm_transition, &tmp, pcm, f2_5, ch, self.fs);
            }
        }

        self.range_final = if len <= 1 {
            0
        } else {
            dec.as_deref().map(|d| d.rng).unwrap_or(0) ^ redundant_rng
        };
        self.prev_mode = mode;
        self.prev_redundancy = redundancy && !celt_to_silk;
        if celt_err {
            return Err(Error::InvalidData("celt decode error"));
        }
        Ok(audiosize)
    }

    /// `opus_decode_native` for one complete (non self-delimited) packet,
    /// or `None` for packet loss of `loss_samples` samples.
    pub fn decode_packet(
        &mut self,
        packet: Option<&[u8]>,
        self_delimited: bool,
        pcm: &mut Vec<f32>,
        max_frame: usize,
    ) -> Result<(usize, usize), Error> {
        let ch = self.channels;
        let data = match packet {
            Some(d) if !d.is_empty() => d,
            _ => {
                // Concealment: libopus requires a multiple of 2.5 ms.
                let frame_size = self
                    .last_packet_duration
                    .max(self.fs as usize / 400)
                    .min(max_frame);
                let frame_size = frame_size / (self.fs as usize / 400) * (self.fs as usize / 400);
                let start = pcm.len();
                pcm.resize(start + frame_size * ch, 0.0);
                let mut count = 0;
                while count < frame_size {
                    let r = self.decode_frame(
                        None,
                        &mut pcm[start + count * ch..],
                        frame_size - count,
                        false,
                    )?;
                    if r == 0 {
                        break;
                    }
                    count += r;
                }
                pcm.truncate(start + count * ch);
                self.last_packet_duration = count;
                return Ok((count, 0));
            }
        };
        let packet_mode = packet_mode(data[0]);
        let packet_bandwidth = packet_bandwidth(data[0]);
        let packet_frame_size = samples_per_frame(data[0], self.fs);
        let packet_stream_channels = packet_channels(data[0]);
        let parsed =
            parse_packet(data, self_delimited).ok_or(Error::InvalidData("invalid packet"))?;
        let count = parsed.frames.len();
        if count * packet_frame_size > max_frame {
            return Err(Error::InvalidData("packet too long"));
        }
        self.mode = packet_mode;
        self.bandwidth = packet_bandwidth;
        self.frame_size = packet_frame_size;
        self.stream_channels = packet_stream_channels;
        let start = pcm.len();
        pcm.resize(start + count * packet_frame_size * ch, 0.0);
        let mut nb_samples = 0usize;
        for &(off, size) in &parsed.frames {
            let frame = &data[off..off + size];
            let r = self.decode_frame(
                Some(frame),
                &mut pcm[start + nb_samples * ch..],
                packet_frame_size,
                false,
            );
            match r {
                Ok(n) => nb_samples += n,
                Err(e) => {
                    pcm.truncate(start);
                    return Err(e);
                }
            }
        }
        pcm.truncate(start + nb_samples * ch);
        self.last_packet_duration = nb_samples;
        Ok((nb_samples, parsed.packet_len))
    }
}
