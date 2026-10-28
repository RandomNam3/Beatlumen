//! MPEG audio frame header parsing (ISO/IEC 11172-3 2.4.1.3, 13818-3 2.4.2.3).

/// MPEG audio version.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Version {
    Mpeg1,
    Mpeg2,
    Mpeg25,
}

/// Channel mode field of the header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChannelMode {
    Stereo,
    JointStereo,
    DualChannel,
    Mono,
}

/// Bitrates in kbit/s, indexed by `[lsf][layer - 1][bitrate_index]`.
const BITRATES: [[[u16; 15]; 3]; 2] = [
    [
        [
            0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448,
        ],
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384,
        ],
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ],
    ],
    [
        [
            0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256,
        ],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],
    ],
];

const SAMPLE_RATES: [u32; 3] = [44100, 48000, 32000];

/// A parsed 4-byte frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FrameHeader {
    pub raw: [u8; 4],
    pub version: Version,
    /// 1, 2 or 3.
    pub layer: u8,
    /// True when a 16-bit CRC follows the header.
    pub crc: bool,
    pub bitrate_index: u8,
    pub sample_rate_index: u8,
    pub sample_rate: u32,
    pub padding: bool,
    pub mode: ChannelMode,
    pub mode_ext: u8,
}

impl FrameHeader {
    /// Parses a header from the first four bytes of `b`. Returns `None` if
    /// the bytes are not a valid header (bad sync, reserved fields).
    pub fn parse(b: &[u8]) -> Option<Self> {
        if b.len() < 4 || b[0] != 0xFF || b[1] & 0xE0 != 0xE0 {
            return None;
        }
        let version = match (b[1] >> 3) & 3 {
            0 => Version::Mpeg25,
            2 => Version::Mpeg2,
            3 => Version::Mpeg1,
            _ => return None,
        };
        let layer = match (b[1] >> 1) & 3 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => return None,
        };
        let bitrate_index = b[2] >> 4;
        if bitrate_index == 15 {
            return None;
        }
        let sample_rate_index = (b[2] >> 2) & 3;
        if sample_rate_index == 3 {
            return None;
        }
        let shift = match version {
            Version::Mpeg1 => 0,
            Version::Mpeg2 => 1,
            Version::Mpeg25 => 2,
        };
        let mode = match b[3] >> 6 {
            0 => ChannelMode::Stereo,
            1 => ChannelMode::JointStereo,
            2 => ChannelMode::DualChannel,
            _ => ChannelMode::Mono,
        };
        // Emphasis value 2 is reserved; tolerate it (some encoders set it).
        Some(FrameHeader {
            raw: [b[0], b[1], b[2], b[3]],
            version,
            layer,
            crc: b[1] & 1 == 0,
            bitrate_index,
            sample_rate_index,
            sample_rate: SAMPLE_RATES[sample_rate_index as usize] >> shift,
            padding: b[2] & 2 != 0,
            mode,
            mode_ext: (b[3] >> 4) & 3,
        })
    }

    #[inline]
    pub fn is_mpeg1(&self) -> bool {
        self.version == Version::Mpeg1
    }

    #[inline]
    pub fn channels(&self) -> usize {
        if self.mode == ChannelMode::Mono {
            1
        } else {
            2
        }
    }

    /// Bitrate in kbit/s, 0 for free format.
    pub fn bitrate_kbps(&self) -> u32 {
        let lsf = usize::from(!self.is_mpeg1());
        u32::from(BITRATES[lsf][self.layer as usize - 1][self.bitrate_index as usize])
    }

    #[inline]
    pub fn is_free_format(&self) -> bool {
        self.bitrate_index == 0
    }

    /// PCM samples per channel produced by one frame.
    pub fn samples_per_frame(&self) -> usize {
        match self.layer {
            1 => 384,
            3 if !self.is_mpeg1() => 576,
            _ => 1152,
        }
    }

    /// Size of one padding slot in bytes.
    fn slot_bytes(&self) -> usize {
        if self.layer == 1 {
            4
        } else {
            1
        }
    }

    /// Frame length in bytes (header included) for a frame with a non-free
    /// bitrate, or using `free_format_bytes` (length without padding) for a
    /// free-format frame. Returns 0 if the length is unknown.
    pub fn frame_len(&self, free_format_bytes: usize) -> usize {
        let base = if self.is_free_format() {
            free_format_bytes
        } else {
            let spf = self.samples_per_frame();
            let bytes = spf * self.bitrate_kbps() as usize * 125 / self.sample_rate as usize;
            if self.layer == 1 {
                bytes & !3
            } else {
                bytes
            }
        };
        if base == 0 {
            return 0;
        }
        base + if self.padding { self.slot_bytes() } else { 0 }
    }

    /// Length of the padding slot of this frame (0 if not padded).
    pub fn padding_len(&self) -> usize {
        if self.padding {
            self.slot_bytes()
        } else {
            0
        }
    }

    /// True if `other` belongs to the same elementary stream (same version,
    /// layer, sample rate and free-format-ness). Channel mode may differ.
    pub fn compatible(&self, other: &FrameHeader) -> bool {
        self.version == other.version
            && self.layer == other.layer
            && self.sample_rate_index == other.sample_rate_index
            && self.is_free_format() == other.is_free_format()
    }

    /// Joint stereo with mid/side coding enabled.
    #[inline]
    pub fn ms_stereo(&self) -> bool {
        self.mode == ChannelMode::JointStereo && self.mode_ext & 2 != 0
    }

    /// Joint stereo with intensity coding enabled (Layer III).
    #[inline]
    pub fn i_stereo(&self) -> bool {
        self.mode == ChannelMode::JointStereo && self.mode_ext & 1 != 0
    }

    /// Sample-rate index over all versions: 0..=2 MPEG-2.5, 3..=5 MPEG-2,
    /// 6..=8 MPEG-1 (each in the order 44.1/48/32 kHz scaled).
    pub fn sr_index_all(&self) -> usize {
        let v = match self.version {
            Version::Mpeg25 => 0,
            Version::Mpeg2 => 3,
            Version::Mpeg1 => 6,
        };
        v + self.sample_rate_index as usize
    }

    /// Length of the Layer III side information in bytes.
    pub fn side_info_len(&self) -> usize {
        match (self.is_mpeg1(), self.channels()) {
            (true, 1) => 17,
            (true, _) => 32,
            (false, 1) => 9,
            (false, _) => 17,
        }
    }

    /// Bytes of this (Layer III) frame available for main data.
    pub fn main_data_len(&self, frame_len: usize) -> usize {
        frame_len.saturating_sub(4 + if self.crc { 2 } else { 0 } + self.side_info_len())
    }

    /// Maximum bit reservoir look-back in bytes (Layer III).
    pub fn max_reservoir(&self) -> usize {
        if self.is_mpeg1() {
            511
        } else {
            255
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mpeg1_layer3() {
        // 128 kbps, 44.1 kHz, joint stereo, no CRC.
        let h = FrameHeader::parse(&[0xFF, 0xFB, 0x90, 0x64]).unwrap();
        assert_eq!(h.version, Version::Mpeg1);
        assert_eq!(h.layer, 3);
        assert!(!h.crc);
        assert_eq!(h.bitrate_kbps(), 128);
        assert_eq!(h.sample_rate, 44100);
        assert_eq!(h.mode, ChannelMode::JointStereo);
        assert_eq!(h.frame_len(0), 417);
        assert!(h.ms_stereo());
        assert!(!h.i_stereo());
    }

    #[test]
    fn parse_lsf_and_layer1() {
        // MPEG-2 layer III 64 kbps 22.05 kHz mono with padding.
        let h = FrameHeader::parse(&[0xFF, 0xF3, 0x82, 0xC4]).unwrap();
        assert_eq!(h.version, Version::Mpeg2);
        assert_eq!(h.sample_rate, 22050);
        assert_eq!(h.samples_per_frame(), 576);
        assert_eq!(h.frame_len(0), 72 * 64000 / 22050 + 1);
        // MPEG-1 layer I 32 kbps 44.1 kHz.
        let h = FrameHeader::parse(&[0xFF, 0xFF, 0x10, 0x00]).unwrap();
        assert_eq!(h.layer, 1);
        assert_eq!(h.frame_len(0), 32);
        assert!(FrameHeader::parse(&[0xFF, 0xF9, 0x90, 0x64]).is_none()); // layer 0
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0xF0, 0x64]).is_none()); // bitrate 15
        assert!(FrameHeader::parse(&[0xFF, 0xFB, 0x9C, 0x64]).is_none()); // sr 3
        assert!(FrameHeader::parse(&[0xFF, 0xEB, 0x90, 0x64]).is_none()); // version 1
    }
}
