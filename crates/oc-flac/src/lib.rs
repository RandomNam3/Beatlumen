//! FLAC decoder (native FLAC streams, std only).
//!
//! Output is interleaved `f32` in [-1, 1] in FLAC channel order (which equals
//! the WAVE/SMPTE order: L R C LFE BL BR SL SR).

use std::io::{Read, Seek};

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::Io(e) => write!(f, "io error: {e}"),
            Error::InvalidData(s) => write!(f, "invalid flac data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported flac feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// A raw FLAC metadata block (type, payload) as found in the stream header.
#[derive(Debug, Clone)]
pub struct MetadataBlock {
    pub block_type: u8,
    pub data: Vec<u8>,
}

pub struct FlacDecoder<R: Read + Seek> {
    _reader: R,
}

impl<R: Read + Seek> FlacDecoder<R> {
    /// Opens a stream starting with "fLaC" (an ID3v2 tag before it is skipped).
    pub fn new(reader: R) -> Result<Self, Error> {
        let _ = reader;
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn sample_rate(&self) -> u32 {
        0
    }
    pub fn channels(&self) -> u16 {
        0
    }
    pub fn bits_per_sample(&self) -> u32 {
        0
    }
    pub fn total_frames(&self) -> Option<u64> {
        None
    }
    /// All metadata blocks except STREAMINFO-independent padding (used for
    /// VORBIS_COMMENT and PICTURE parsing by `oc-media`).
    pub fn metadata_blocks(&self) -> &[MetadataBlock] {
        &[]
    }
    pub fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let _ = out;
        Ok(false)
    }
    /// Sample accurate seek (uses SEEKTABLE if present, otherwise bisection).
    pub fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        let _ = frame;
        Err(Error::Unsupported("not implemented yet"))
    }
}

pub fn probe(data: &[u8]) -> bool {
    data.starts_with(b"fLaC")
}
