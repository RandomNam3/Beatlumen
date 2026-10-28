//! Opus packet parsing (TOC byte, frame packing codes 0-3, padding,
//! self-delimited framing for multistream packets).
//!
//! Ported from libopus `src/opus.c` and `src/opus_decoder.c`:
//!
//! Copyright (c) 2010 Xiph.Org Foundation, Skype Limited
//! Copyright (c) 2011 Xiph.Org Foundation, Skype Limited
//! Written by Jean-Marc Valin and Koen Vos
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

pub const MODE_SILK_ONLY: i32 = 1000;
pub const MODE_HYBRID: i32 = 1001;
pub const MODE_CELT_ONLY: i32 = 1002;

pub const BANDWIDTH_NARROWBAND: i32 = 1101;
pub const BANDWIDTH_MEDIUMBAND: i32 = 1102;
pub const BANDWIDTH_WIDEBAND: i32 = 1103;
pub const BANDWIDTH_SUPERWIDEBAND: i32 = 1104;
pub const BANDWIDTH_FULLBAND: i32 = 1105;

pub fn packet_mode(toc: u8) -> i32 {
    if toc & 0x80 != 0 {
        MODE_CELT_ONLY
    } else if toc & 0x60 == 0x60 {
        MODE_HYBRID
    } else {
        MODE_SILK_ONLY
    }
}

pub fn packet_bandwidth(toc: u8) -> i32 {
    if toc & 0x80 != 0 {
        let bw = BANDWIDTH_MEDIUMBAND + ((toc >> 5) & 0x3) as i32;
        if bw == BANDWIDTH_MEDIUMBAND {
            BANDWIDTH_NARROWBAND
        } else {
            bw
        }
    } else if toc & 0x60 == 0x60 {
        if toc & 0x10 != 0 {
            BANDWIDTH_FULLBAND
        } else {
            BANDWIDTH_SUPERWIDEBAND
        }
    } else {
        BANDWIDTH_NARROWBAND + ((toc >> 5) & 0x3) as i32
    }
}

pub fn packet_channels(toc: u8) -> usize {
    if toc & 0x4 != 0 {
        2
    } else {
        1
    }
}

/// Samples per frame at sampling rate `fs`.
pub fn samples_per_frame(toc: u8, fs: u32) -> usize {
    let fs = fs as usize;
    if toc & 0x80 != 0 {
        let a = ((toc >> 3) & 0x3) as usize;
        (fs << a) / 400
    } else if toc & 0x60 == 0x60 {
        if toc & 0x08 != 0 {
            fs / 50
        } else {
            fs / 100
        }
    } else {
        let a = ((toc >> 3) & 0x3) as usize;
        if a == 3 {
            fs * 60 / 1000
        } else {
            (fs << a) / 100
        }
    }
}

/// Result of parsing one (possibly self-delimited) Opus packet.
pub struct ParsedPacket {
    pub toc: u8,
    /// Byte ranges of the frames within the input slice.
    pub frames: Vec<(usize, usize)>,
    /// Total bytes consumed (including padding); used for multistream.
    pub packet_len: usize,
}

fn parse_size(data: &[u8]) -> Option<(usize, usize)> {
    match data.len() {
        0 => None,
        _ if data[0] < 252 => Some((data[0] as usize, 1)),
        1 => None,
        _ => Some((4 * data[1] as usize + data[0] as usize, 2)),
    }
}

/// `opus_packet_parse_impl`. Returns `None` for an invalid packet.
pub fn parse_packet(data: &[u8], self_delimited: bool) -> Option<ParsedPacket> {
    if data.is_empty() {
        return None;
    }
    let framesize = samples_per_frame(data[0], 48000);
    let toc = data[0];
    let mut pos = 1usize;
    let mut len = data.len() as isize - 1;
    let mut last_size = len;
    let mut cbr = false;
    let count: usize;
    let mut sizes = [0isize; 48];
    let mut pad: isize = 0;
    match toc & 0x3 {
        0 => count = 1,
        1 => {
            count = 2;
            cbr = true;
            if !self_delimited {
                if len & 1 != 0 {
                    return None;
                }
                last_size = len / 2;
                sizes[0] = last_size;
            }
        }
        2 => {
            count = 2;
            let (sz, bytes) = parse_size(&data[pos..])?;
            len -= bytes as isize;
            if sz as isize > len {
                return None;
            }
            sizes[0] = sz as isize;
            pos += bytes;
            last_size = len - sizes[0];
        }
        _ => {
            if len < 1 {
                return None;
            }
            let ch = data[pos];
            pos += 1;
            count = (ch & 0x3f) as usize;
            if count == 0 || framesize * count > 5760 {
                return None;
            }
            len -= 1;
            if ch & 0x40 != 0 {
                loop {
                    if len <= 0 {
                        return None;
                    }
                    let p = data[pos];
                    pos += 1;
                    len -= 1;
                    let tmp = if p == 255 { 254 } else { p as isize };
                    len -= tmp;
                    pad += tmp;
                    if p != 255 {
                        break;
                    }
                }
            }
            if len < 0 {
                return None;
            }
            cbr = ch & 0x80 == 0;
            if !cbr {
                last_size = len;
                for size in sizes.iter_mut().take(count - 1) {
                    let (sz, bytes) = parse_size(data.get(pos..)?)?;
                    // parse_size may read beyond `len` bytes (into padding);
                    // libopus bounds it by len, so check the same way.
                    if bytes as isize > len {
                        return None;
                    }
                    len -= bytes as isize;
                    if sz as isize > len {
                        return None;
                    }
                    *size = sz as isize;
                    pos += bytes;
                    last_size -= bytes as isize + sz as isize;
                }
                if last_size < 0 {
                    return None;
                }
            } else if !self_delimited {
                last_size = len / count as isize;
                if last_size * count as isize != len {
                    return None;
                }
                for size in sizes.iter_mut().take(count - 1) {
                    *size = last_size;
                }
            }
        }
    }
    if self_delimited {
        let avail = (len.max(0) as usize).min(data.len().saturating_sub(pos));
        let (sz, bytes) = parse_size(&data[pos..pos + avail])?;
        len -= bytes as isize;
        let sz = sz as isize;
        if sz > len {
            return None;
        }
        pos += bytes;
        sizes[count - 1] = sz;
        if cbr {
            if sz * count as isize > len {
                return None;
            }
            for size in sizes.iter_mut().take(count - 1) {
                *size = sz;
            }
        } else if bytes as isize + sz > last_size {
            return None;
        }
    } else {
        if last_size > 1275 {
            return None;
        }
        sizes[count - 1] = last_size;
    }
    let mut frames = Vec::with_capacity(count);
    for &s in sizes.iter().take(count) {
        if !(0..=1275).contains(&s) || pos + s as usize > data.len() {
            return None;
        }
        frames.push((pos, s as usize));
        pos += s as usize;
    }
    let packet_len = pos + pad as usize;
    if packet_len > data.len() {
        return None;
    }
    Some(ParsedPacket {
        toc,
        frames,
        packet_len,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code0_and_code1() {
        let p = parse_packet(&[0xF8, 1, 2, 3], false).unwrap();
        assert_eq!(p.frames, vec![(1, 3)]);
        let p = parse_packet(&[0xF9, 1, 2, 3, 4], false).unwrap();
        assert_eq!(p.frames, vec![(1, 2), (3, 2)]);
        assert!(parse_packet(&[0xF9, 1, 2, 3], false).is_none()); // odd CBR payload
    }

    #[test]
    fn code2_vbr() {
        let p = parse_packet(&[0xFA, 1, 9, 8, 7], false).unwrap();
        assert_eq!(p.frames, vec![(2, 1), (3, 2)]);
        assert!(parse_packet(&[0xFA, 5, 9], false).is_none());
    }

    #[test]
    fn code3_padding_and_vbr() {
        // 3 CBR frames of 2 bytes with 2 bytes of padding (count byte 0x43).
        let p = parse_packet(&[0xFB, 0x43, 2, 1, 1, 2, 2, 3, 3, 0, 0], false).unwrap();
        assert_eq!(p.frames, vec![(3, 2), (5, 2), (7, 2)]);
        assert_eq!(p.packet_len, 11);
        // VBR: 2 frames, first is 1 byte.
        let p = parse_packet(&[0xFB, 0x82, 1, 7, 8, 9], false).unwrap();
        assert_eq!(p.frames, vec![(3, 1), (4, 2)]);
        // Zero frames / too long are invalid.
        assert!(parse_packet(&[0xFB, 0x00], false).is_none());
        assert!(parse_packet(&[0x1B, 0x07], false).is_none()); // 7 x 60 ms > 120 ms
    }

    #[test]
    fn self_delimited() {
        let data = [0xF8, 2, 5, 6, 0xF8, 7];
        let p = parse_packet(&data, true).unwrap();
        assert_eq!(p.frames, vec![(2, 2)]);
        assert_eq!(p.packet_len, 4);
        let p = parse_packet(&data[4..], false).unwrap();
        assert_eq!(p.frames, vec![(1, 1)]);
    }

    #[test]
    fn toc_fields() {
        assert_eq!(packet_mode(0x00), MODE_SILK_ONLY);
        assert_eq!(packet_mode(0x60), MODE_HYBRID);
        assert_eq!(packet_mode(0x80), MODE_CELT_ONLY);
        assert_eq!(samples_per_frame(0x18, 48000), 2880);
        assert_eq!(samples_per_frame(0x80, 48000), 120);
        assert_eq!(samples_per_frame(0x68, 48000), 960);
        assert_eq!(packet_bandwidth(0x80), BANDWIDTH_NARROWBAND);
        assert_eq!(packet_bandwidth(0xF0), BANDWIDTH_FULLBAND);
        assert_eq!(packet_channels(0x04), 2);
    }
}
