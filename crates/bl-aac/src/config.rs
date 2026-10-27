//! AudioSpecificConfig / GASpecificConfig / program_config_element parsing
//! (ISO/IEC 14496-3 1.6.2.1, 4.4.1) and channel layout derivation.

use crate::bits::BitReader;
use crate::tables::sf_index_for_rate;
use crate::{Error, SAMPLE_RATES};

/// Audio object types relevant to this decoder.
pub mod aot {
    pub const AAC_MAIN: u8 = 1;
    pub const AAC_LC: u8 = 2;
    pub const AAC_SSR: u8 = 3;
    pub const AAC_LTP: u8 = 4;
    pub const SBR: u8 = 5;
    pub const PS: u8 = 29;
}

/// Parsed MPEG-4 AudioSpecificConfig.
#[derive(Debug, Clone)]
pub struct AudioSpecificConfig {
    /// Core audio object type (2 = AAC LC). For explicitly signalled HE-AAC
    /// (object type 5 or 29) this is the underlying core type.
    pub object_type: u8,
    /// Core sampling rate in Hz.
    pub sample_rate: u32,
    /// Sampling frequency index used for the core tables (derived from the
    /// rate when signalled explicitly with escape index 15).
    pub sf_index: u8,
    pub channel_config: u8,
    /// `frameLengthFlag` (960-sample frames); not supported for decoding.
    pub frame_length_960: bool,
    /// `Some(true)`: SBR explicitly signalled, `Some(false)`: explicitly
    /// signalled as absent, `None`: not signalled (implicit SBR possible).
    pub sbr: Option<bool>,
    /// Same convention as `sbr` for parametric stereo.
    pub ps: Option<bool>,
    /// Extension (SBR output) sampling rate, when signalled.
    pub ext_sample_rate: Option<u32>,
    /// Program config element (channel configuration 0).
    pub pce: Option<ProgramConfig>,
}

/// Parsed program_config_element.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ProgramConfig {
    pub object_type: u8,
    pub sf_index: u8,
    /// `(is_cpe, tag)` per front element.
    pub front: Vec<(bool, u8)>,
    pub side: Vec<(bool, u8)>,
    pub back: Vec<(bool, u8)>,
    pub lfe: Vec<u8>,
}

impl ProgramConfig {
    pub fn num_channels(&self) -> usize {
        let count = |v: &Vec<(bool, u8)>| {
            v.iter()
                .map(|&(cpe, _)| if cpe { 2 } else { 1 })
                .sum::<usize>()
        };
        count(&self.front) + count(&self.side) + count(&self.back) + self.lfe.len()
    }

    pub(crate) fn parse(r: &mut BitReader) -> Result<ProgramConfig, Error> {
        let _tag = r.read(4);
        let object_type = r.read(2) as u8 + 1;
        let sf_index = r.read(4) as u8;
        let nfront = r.read(4);
        let nside = r.read(4);
        let nback = r.read(4);
        let nlfe = r.read(2);
        let nassoc = r.read(3);
        let ncc = r.read(4);
        if r.read_bit() {
            r.skip(4); // mono mixdown element
        }
        if r.read_bit() {
            r.skip(4); // stereo mixdown element
        }
        if r.read_bit() {
            r.skip(3); // matrix mixdown idx + pseudo surround
        }
        let mut list = |n: u32| -> Vec<(bool, u8)> {
            (0..n).map(|_| (r.read_bit(), r.read(4) as u8)).collect()
        };
        let front = list(nfront);
        let side = list(nside);
        let back = list(nback);
        let lfe = (0..nlfe).map(|_| r.read(4) as u8).collect();
        r.skip(4 * nassoc as usize);
        r.skip(5 * ncc as usize);
        r.byte_align();
        let comment_bytes = r.read(8) as usize;
        r.skip(8 * comment_bytes);
        r.check()?;
        Ok(ProgramConfig {
            object_type,
            sf_index,
            front,
            side,
            back,
            lfe,
        })
    }
}

fn read_object_type(r: &mut BitReader) -> u8 {
    let t = r.read(5) as u8;
    if t == 31 {
        32 + r.read(6) as u8
    } else {
        t
    }
}

fn read_sample_rate(r: &mut BitReader) -> Result<(u32, u8), Error> {
    let idx = r.read(4) as usize;
    if idx == 15 {
        let rate = r.read(24);
        if rate == 0 {
            return Err(Error::InvalidData("zero sampling rate"));
        }
        Ok((rate, sf_index_for_rate(rate) as u8))
    } else if idx < SAMPLE_RATES.len() {
        Ok((SAMPLE_RATES[idx], idx as u8))
    } else {
        Err(Error::InvalidData("reserved sampling frequency index"))
    }
}

impl AudioSpecificConfig {
    /// Parses an AudioSpecificConfig (e.g. the payload of an MP4 `esds`
    /// DecoderSpecificInfo).
    pub fn parse(data: &[u8]) -> Result<AudioSpecificConfig, Error> {
        let mut r = BitReader::new(data);
        let mut object_type = read_object_type(&mut r);
        let (mut sample_rate, mut sf_index) = read_sample_rate(&mut r)?;
        let channel_config = r.read(4) as u8;
        r.check()?;
        let mut sbr = None;
        let mut ps = None;
        let mut ext_sample_rate = None;
        if object_type == aot::SBR || object_type == aot::PS {
            sbr = Some(true);
            if object_type == aot::PS {
                ps = Some(true);
            }
            ext_sample_rate = Some(read_sample_rate(&mut r)?.0);
            object_type = read_object_type(&mut r);
            if object_type == 22 {
                r.skip(4); // extensionChannelConfiguration (ER BSAC)
            }
        }
        let mut frame_length_960 = false;
        let mut pce = None;
        match object_type {
            1..=4 | 6 | 7 | 17 | 19..=23 => {
                // GASpecificConfig
                frame_length_960 = r.read_bit();
                if r.read_bit() {
                    r.skip(14); // coreCoderDelay
                }
                let extension_flag = r.read_bit();
                if channel_config == 0 {
                    // ADTS-derived configs carry the PCE in-band instead.
                    if r.bits_left() >= 32 {
                        let mut pr = r.clone();
                        if let Ok(p) = ProgramConfig::parse(&mut pr) {
                            r = pr;
                            pce = Some(p);
                        }
                    }
                }
                if object_type == 6 || object_type == 20 {
                    r.skip(3); // layerNr
                }
                if extension_flag {
                    if object_type == 22 {
                        r.skip(16);
                    }
                    if matches!(object_type, 17 | 19 | 20 | 23) {
                        r.skip(3);
                    }
                    r.skip(1); // extensionFlag3
                }
            }
            _ => {}
        }
        // Backward-compatible explicit SBR/PS signalling (sync extension).
        if sbr.is_none() && r.bits_left() >= 16 && !r.overrun() {
            let mut er = r.clone();
            if er.read(11) == 0x2b7 {
                let ext_type = read_object_type(&mut er);
                if ext_type == aot::SBR {
                    let present = er.read_bit();
                    sbr = Some(present);
                    if present {
                        if let Ok((rate, _)) = read_sample_rate(&mut er) {
                            ext_sample_rate = Some(rate);
                        }
                        if er.bits_left() >= 12 && er.read(11) == 0x548 {
                            ps = Some(er.read_bit());
                        }
                    }
                    if er.overrun() {
                        sbr = None;
                        ps = None;
                        ext_sample_rate = None;
                    }
                }
            }
        }
        if sample_rate == 0 {
            return Err(Error::InvalidData("zero sampling rate"));
        }
        if sf_index > 12 {
            sf_index = sf_index_for_rate(sample_rate) as u8;
        }
        if sample_rate > 1_000_000 {
            sample_rate = 1_000_000;
        }
        Ok(AudioSpecificConfig {
            object_type,
            sample_rate,
            sf_index,
            channel_config,
            frame_length_960,
            sbr,
            ps,
            ext_sample_rate,
            pce,
        })
    }
}

/// WAVE/SMPTE speaker roles used to order output channels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Role {
    FrontLeft = 0,
    FrontRight,
    FrontCenter,
    Lfe,
    BackLeft,
    BackRight,
    FrontLeftCenter,
    FrontRightCenter,
    BackCenter,
    SideLeft,
    SideRight,
    Other,
}

/// Speaker roles of the decoded channels in bitstream (element) order for a
/// channel configuration 1..=7.
pub(crate) fn config_roles(channel_config: u8) -> Option<Vec<Role>> {
    use Role::*;
    Some(match channel_config {
        1 => vec![FrontCenter],
        2 => vec![FrontLeft, FrontRight],
        3 => vec![FrontCenter, FrontLeft, FrontRight],
        4 => vec![FrontCenter, FrontLeft, FrontRight, BackCenter],
        5 => vec![FrontCenter, FrontLeft, FrontRight, BackLeft, BackRight],
        6 => vec![FrontCenter, FrontLeft, FrontRight, BackLeft, BackRight, Lfe],
        7 => vec![
            FrontCenter,
            FrontLeft,
            FrontRight,
            SideLeft,
            SideRight,
            BackLeft,
            BackRight,
            Lfe,
        ],
        _ => return None,
    })
}

/// Speaker roles for a program config element, in element order.
pub(crate) fn pce_roles(p: &ProgramConfig) -> Vec<Role> {
    use Role::*;
    let mut roles = Vec::new();
    let front_cpes = p.front.iter().filter(|e| e.0).count();
    let mut cpe_seen = 0;
    for (i, &(cpe, _)) in p.front.iter().enumerate() {
        if cpe {
            cpe_seen += 1;
            if front_cpes >= 2 && cpe_seen == 1 {
                roles.extend([FrontLeftCenter, FrontRightCenter]);
            } else if (front_cpes >= 2 && cpe_seen == 2) || (front_cpes == 1) {
                roles.extend([FrontLeft, FrontRight]);
            } else {
                roles.extend([Other, Other]);
            }
        } else if i == 0 {
            roles.push(FrontCenter);
        } else {
            roles.push(Other);
        }
    }
    let mut side_pair = false;
    for &(cpe, _) in &p.side {
        if cpe && !side_pair {
            side_pair = true;
            roles.extend([SideLeft, SideRight]);
        } else if cpe {
            roles.extend([Other, Other]);
        } else {
            roles.push(Other);
        }
    }
    let mut back_pair = false;
    let mut back_center = false;
    for &(cpe, _) in &p.back {
        if cpe && !back_pair {
            back_pair = true;
            roles.extend([BackLeft, BackRight]);
        } else if cpe {
            roles.extend([Other, Other]);
        } else if !back_center {
            back_center = true;
            roles.push(BackCenter);
        } else {
            roles.push(Other);
        }
    }
    for (i, _) in p.lfe.iter().enumerate() {
        roles.push(if i == 0 { Lfe } else { Other });
    }
    roles
}

/// Output permutation: `perm[out_channel] = decoded_channel` (stable sort by
/// WAVE role order).
pub(crate) fn output_permutation(roles: &[Role]) -> Vec<usize> {
    let mut idx: Vec<usize> = (0..roles.len()).collect();
    idx.sort_by_key(|&i| roles[i]);
    idx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_lc_stereo_44k() {
        let c = AudioSpecificConfig::parse(&[0x12, 0x10]).unwrap();
        assert_eq!(
            (c.object_type, c.sample_rate, c.channel_config),
            (2, 44100, 2)
        );
        assert_eq!(c.sbr, None);
    }

    #[test]
    fn parse_explicit_he_aac() {
        // AOT 5, 24 kHz core (idx 6), stereo, ext idx 3 (48 kHz), core AOT 2.
        // 00101 0110 0010 0011 00010 0 0 0 -> bits
        let bits = "00101011000100011000100000";
        let mut bytes = vec![0u8; 4];
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let c = AudioSpecificConfig::parse(&bytes).unwrap();
        assert_eq!(c.object_type, 2);
        assert_eq!(c.sample_rate, 24000);
        assert_eq!(c.ext_sample_rate, Some(48000));
        assert_eq!(c.sbr, Some(true));
        assert_eq!(c.channel_config, 2);
    }

    #[test]
    fn parse_sync_extension() {
        // LC 22050 Hz mono + sync ext 0x2b7, SBR present, 44100, 0x548 PS=1.
        let bits = "00010 0111 0001 000 01010110111 00101 1 0100 10101001000 1";
        let bits: String = bits.chars().filter(|c| !c.is_whitespace()).collect();
        let mut bytes = vec![0u8; (bits.len() + 7) / 8];
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let c = AudioSpecificConfig::parse(&bytes).unwrap();
        assert_eq!(c.sample_rate, 22050);
        assert_eq!(c.sbr, Some(true));
        assert_eq!(c.ps, Some(true));
        assert_eq!(c.ext_sample_rate, Some(44100));
    }

    #[test]
    fn escape_rate() {
        // LC, index 15, rate 22000 (0x0055f0), mono.
        let bits = format!("00010{}{:024b}0001000", "1111", 22000);
        let mut bytes = vec![0u8; (bits.len() + 7) / 8];
        for (i, c) in bits.chars().enumerate() {
            if c == '1' {
                bytes[i / 8] |= 0x80 >> (i % 8);
            }
        }
        let c = AudioSpecificConfig::parse(&bytes).unwrap();
        assert_eq!(c.sample_rate, 22000);
        assert_eq!(c.sf_index, 7);
    }

    #[test]
    fn layout_permutations() {
        let p6 = output_permutation(&config_roles(6).unwrap());
        assert_eq!(p6, vec![1, 2, 0, 5, 3, 4]);
        let p7 = output_permutation(&config_roles(7).unwrap());
        assert_eq!(p7, vec![1, 2, 0, 7, 5, 6, 3, 4]);
    }
}
