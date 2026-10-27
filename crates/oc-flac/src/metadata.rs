//! Metadata block parsing (RFC 9639 section 8): STREAMINFO and SEEKTABLE.
//! Other block types are kept raw for the caller.

use crate::Error;

/// A raw FLAC metadata block (type, payload) as found in the stream header.
#[derive(Debug, Clone)]
pub struct MetadataBlock {
    pub block_type: u8,
    pub data: Vec<u8>,
}

impl MetadataBlock {
    pub const STREAMINFO: u8 = 0;
    pub const PADDING: u8 = 1;
    pub const APPLICATION: u8 = 2;
    pub const SEEKTABLE: u8 = 3;
    pub const VORBIS_COMMENT: u8 = 4;
    pub const CUESHEET: u8 = 5;
    pub const PICTURE: u8 = 6;
}

/// Decoded STREAMINFO block (RFC 9639 section 8.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamInfo {
    pub min_block_size: u16,
    pub max_block_size: u16,
    /// Minimum frame size in bytes, 0 if unknown.
    pub min_frame_size: u32,
    /// Maximum frame size in bytes, 0 if unknown.
    pub max_frame_size: u32,
    pub sample_rate: u32,
    /// 1..=8
    pub channels: u8,
    /// 4..=32
    pub bits_per_sample: u8,
    /// Total number of inter-channel samples, 0 if unknown.
    pub total_samples: u64,
    pub md5: [u8; 16],
}

impl StreamInfo {
    /// Size of the STREAMINFO payload in bytes.
    pub const SIZE: usize = 34;

    /// Parses a STREAMINFO payload (at least 34 bytes, without block header).
    pub fn parse(d: &[u8]) -> Result<StreamInfo, Error> {
        if d.len() < Self::SIZE {
            return Err(Error::InvalidData("STREAMINFO block too short"));
        }
        let min_block_size = u16::from_be_bytes([d[0], d[1]]);
        let max_block_size = u16::from_be_bytes([d[2], d[3]]);
        let min_frame_size = u32::from_be_bytes([0, d[4], d[5], d[6]]);
        let max_frame_size = u32::from_be_bytes([0, d[7], d[8], d[9]]);
        let packed = u64::from_be_bytes([d[10], d[11], d[12], d[13], d[14], d[15], d[16], d[17]]);
        let sample_rate = (packed >> 44) as u32;
        let channels = ((packed >> 41) & 7) as u8 + 1;
        let bits_per_sample = ((packed >> 36) & 31) as u8 + 1;
        let total_samples = packed & ((1u64 << 36) - 1);
        let mut md5 = [0u8; 16];
        md5.copy_from_slice(&d[18..34]);
        if bits_per_sample < 4 {
            return Err(Error::Unsupported("bits per sample below 4"));
        }
        if max_block_size != 0 && min_block_size > max_block_size {
            return Err(Error::InvalidData("STREAMINFO min block size > max block size"));
        }
        Ok(StreamInfo {
            min_block_size,
            max_block_size,
            min_frame_size,
            max_frame_size,
            sample_rate,
            channels,
            bits_per_sample,
            total_samples,
            md5,
        })
    }

    /// Locates the 34-byte STREAMINFO payload in codec setup data. Accepted
    /// forms:
    /// * the bare 34-byte payload,
    /// * a metadata block header followed by the payload (as in FLAC files),
    /// * `fLaC` followed by metadata blocks (Matroska `CodecPrivate`),
    /// * the Ogg FLAC first header packet (`0x7F "FLAC" major minor count
    ///   "fLaC" header streaminfo`, RFC 9639 section 10.1).
    pub fn find_in_codec_private(data: &[u8]) -> Option<&[u8]> {
        let mut d = data;
        if d.len() >= 9 && d[0] == 0x7f && &d[1..5] == b"FLAC" {
            d = &d[9..];
        }
        if d.starts_with(b"fLaC") {
            d = &d[4..];
        }
        if d.len() >= 4 + Self::SIZE && d[0] & 0x7f == 0 {
            let len = u32::from_be_bytes([0, d[1], d[2], d[3]]) as usize;
            if len == Self::SIZE {
                return Some(&d[4..4 + Self::SIZE]);
            }
        }
        if d.len() == Self::SIZE {
            return Some(d);
        }
        None
    }
}

/// A seek point (RFC 9639 section 8.5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SeekPoint {
    /// Sample number of the first sample in the target frame.
    pub sample: u64,
    /// Byte offset of the frame relative to the first frame header.
    pub offset: u64,
    /// Number of samples in the target frame.
    pub samples: u16,
}

/// Parses a SEEKTABLE payload. Placeholder points are dropped, and points
/// that are not strictly increasing (in sample number and offset) are
/// ignored, so the result is always sorted.
pub fn parse_seek_table(d: &[u8]) -> Vec<SeekPoint> {
    let mut out: Vec<SeekPoint> = Vec::with_capacity(d.len() / 18);
    for p in d.chunks_exact(18) {
        let sample = u64::from_be_bytes([p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]]);
        let offset = u64::from_be_bytes([p[8], p[9], p[10], p[11], p[12], p[13], p[14], p[15]]);
        let samples = u16::from_be_bytes([p[16], p[17]]);
        if sample == u64::MAX {
            continue;
        }
        if let Some(last) = out.last() {
            if sample <= last.sample || offset <= last.offset {
                continue;
            }
        }
        out.push(SeekPoint { sample, offset, samples });
    }
    out
}
