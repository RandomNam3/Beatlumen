//! Multistream decoding (RFC 7845 section 5.1.1): several elementary Opus
//! streams (coupled stereo streams first, then mono streams) packed into one
//! packet with self-delimited framing, mapped to output channels.
//!
//! Ported from libopus `src/opus_multistream_decoder.c`:
//!
//! Copyright (c) 2011 Xiph.Org Foundation
//! Written by Jean-Marc Valin
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

use crate::opus::StreamDecoder;
use crate::packet::{parse_packet, samples_per_frame};
use crate::Error;

pub struct MultistreamDecoder {
    fs: u32,
    channels: usize,
    coupled: usize,
    /// Output channel -> decoded channel index (255 = silent).
    mapping: Vec<u8>,
    streams: Vec<StreamDecoder>,
    buf: Vec<f32>,
}

impl MultistreamDecoder {
    pub fn new(
        fs: u32,
        channels: usize,
        streams: usize,
        coupled: usize,
        mapping: &[u8],
    ) -> Result<Self, Error> {
        if channels == 0
            || channels > 255
            || streams == 0
            || coupled > streams
            || streams + coupled > 255
        {
            return Err(Error::InvalidData("bad stream layout"));
        }
        if mapping.len() < channels {
            return Err(Error::InvalidData("channel mapping too short"));
        }
        for &m in &mapping[..channels] {
            if m != 255 && m as usize >= streams + coupled {
                return Err(Error::InvalidData("channel mapping out of range"));
            }
        }
        let mut decs = Vec::with_capacity(streams);
        for s in 0..streams {
            decs.push(StreamDecoder::new(fs, if s < coupled { 2 } else { 1 })?);
        }
        Ok(MultistreamDecoder {
            fs,
            channels,
            coupled,
            mapping: mapping[..channels].to_vec(),
            streams: decs,
            buf: Vec::new(),
        })
    }

    pub fn reset(&mut self) {
        for s in &mut self.streams {
            s.reset();
        }
    }

    pub fn final_range(&self) -> u32 {
        self.streams.iter().fold(0, |a, s| a ^ s.range_final)
    }

    /// Validates the packet layout (`opus_multistream_packet_validate`) and
    /// returns the number of samples per channel.
    fn validate(&self, data: &[u8]) -> Result<usize, Error> {
        let n = self.streams.len();
        let mut pos = 0usize;
        let mut samples = 0usize;
        for s in 0..n {
            if pos >= data.len() {
                return Err(Error::InvalidData("truncated multistream packet"));
            }
            let p = parse_packet(&data[pos..], s != n - 1)
                .ok_or(Error::InvalidData("invalid packet"))?;
            let tmp = p.frames.len() * samples_per_frame(p.toc, self.fs);
            if tmp * 25 > self.fs as usize * 3 {
                return Err(Error::InvalidData("packet too long"));
            }
            if s != 0 && samples != tmp {
                return Err(Error::InvalidData("stream durations differ"));
            }
            samples = tmp;
            pos += p.packet_len;
        }
        Ok(samples)
    }

    /// Decodes one packet (empty = concealment), appending interleaved
    /// samples to `out`. Returns the number of samples per channel.
    pub fn decode(&mut self, data: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        let max_frame = self.fs as usize / 25 * 3;
        let do_plc = data.is_empty();
        if !do_plc {
            if data.len() < 2 * self.streams.len() - 1 {
                return Err(Error::InvalidData("multistream packet too short"));
            }
            self.validate(data)?;
        }
        let ch = self.channels;
        let start = out.len();
        let mut frame_size = 0usize;
        let mut pos = 0usize;
        let n = self.streams.len();
        for s in 0..n {
            self.buf.clear();
            let pkt = if do_plc { None } else { Some(&data[pos..]) };
            let res = self.streams[s].decode_packet(pkt, s != n - 1, &mut self.buf, max_frame);
            let (ns, used) = match res {
                Ok(v) => v,
                Err(e) => {
                    out.truncate(start);
                    return Err(e);
                }
            };
            pos += used;
            if s == 0 {
                frame_size = ns;
                out.resize(start + frame_size * ch, 0.0);
            } else if ns != frame_size {
                // Concealment of streams with different histories may differ
                // in length; keep the common part.
                if ns < frame_size {
                    frame_size = ns;
                    out.truncate(start + frame_size * ch);
                }
            }
            let sch = self.streams[s].channels();
            let dst = &mut out[start..];
            for c in 0..ch {
                let m = self.mapping[c];
                if m == 255 {
                    continue;
                }
                let m = m as usize;
                let src_ch = if s < self.coupled {
                    if m == 2 * s {
                        0
                    } else if m == 2 * s + 1 {
                        1
                    } else {
                        continue;
                    }
                } else if m == s + self.coupled {
                    0
                } else {
                    continue;
                };
                for i in 0..frame_size {
                    dst[i * ch + c] = self.buf[i * sch + src_ch];
                }
            }
        }
        Ok(frame_size)
    }
}
