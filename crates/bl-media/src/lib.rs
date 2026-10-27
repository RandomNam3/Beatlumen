//! Audio container demuxing, tag reading and a unified decoder facade.
//!
//! This crate ties the codec crates (`bl-mp3`, `bl-flac`, `bl-vorbis`,
//! `bl-opus`, `bl-aac`) together with container parsers (WAV/RIFF/RF64,
//! AIFF/AIFC, Ogg, MP4/M4A, ADTS, Matroska/WebM) and tag readers (ID3v1/v2,
//! APEv2, Vorbis comments, FLAC metadata, MP4 `ilst`, RIFF INFO).
//!
//! All decoders output interleaved `f32` samples in WAVE/SMPTE channel order
//! (L R C LFE BL BR SL SR).
//!
//! Module overview: `probe` detects the container from magic bytes;
//! `wav`, `aiff`, `ogg`, `mp4`, `adts`, `mkv`, `mp3` and `flac` implement
//! demuxing, `read_info` and the decoder adapters; `tags/*` holds the tag
//! readers; `alac` is a native Apple Lossless decoder; `downmix` and
//! `resample` provide output conversion. Gapless trimming is applied where
//! the container signals it (MP3 LAME/Lavc headers via `bl-mp3`, MP4 edit
//! lists / `iTunSMPB`, Ogg granules, Matroska CodecDelay/DiscardPadding).
//!
//! Errors returned by [`Decoder::next_chunk`] leave the decoder usable:
//! callers may skip the damaged packet by calling it again.

#![forbid(unsafe_code)]

use std::io::{BufReader, Read, Seek};

mod adts;
mod aiff;
mod alac;
mod downmix;
mod flac;
mod mkv;
mod mp3;
mod mp4;
mod ogg;
mod pcm;
mod probe;
mod resample;
mod tags;
mod util;
mod wav;
mod window;

pub use alac::{AlacConfig, AlacDecoder};

/// A seekable byte source (file, in-memory buffer, HTTP range reader, ...).
pub trait Source: Read + Seek + Send {}
impl<T: Read + Seek + Send> Source for T {}

#[derive(Debug)]
pub enum Error {
    Io(std::io::Error),
    /// Not a recognised/supported container or codec.
    Unsupported(String),
    InvalidData(String),
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
            Error::Unsupported(s) => write!(f, "unsupported: {s}"),
            Error::InvalidData(s) => write!(f, "invalid data: {s}"),
        }
    }
}

impl std::error::Error for Error {}

macro_rules! codec_error {
    ($($krate:ident),*) => {$(
        impl From<$krate::Error> for Error {
            fn from(e: $krate::Error) -> Self {
                match e {
                    $krate::Error::InvalidData(s) => Error::InvalidData(format!("{}: {s}", stringify!($krate))),
                    $krate::Error::Unsupported(s) => Error::Unsupported(format!("{}: {s}", stringify!($krate))),
                    #[allow(unreachable_patterns)]
                    _ => Error::InvalidData(format!("{}: {e}", stringify!($krate))),
                }
            }
        }
    )*};
}
codec_error!(bl_vorbis, bl_opus, bl_aac);

impl From<bl_mp3::Error> for Error {
    fn from(e: bl_mp3::Error) -> Self {
        match e {
            bl_mp3::Error::Io(e) => Error::Io(e),
            bl_mp3::Error::InvalidData(s) => Error::InvalidData(format!("bl_mp3: {s}")),
            bl_mp3::Error::Unsupported(s) => Error::Unsupported(format!("bl_mp3: {s}")),
        }
    }
}

impl From<bl_flac::Error> for Error {
    fn from(e: bl_flac::Error) -> Self {
        match e {
            bl_flac::Error::Io(e) => Error::Io(e),
            bl_flac::Error::InvalidData(s) => Error::InvalidData(format!("bl_flac: {s}")),
            bl_flac::Error::Unsupported(s) => Error::Unsupported(format!("bl_flac: {s}")),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioSpec {
    pub sample_rate: u32,
    pub channels: u16,
}

/// A streaming audio decoder.
pub trait Decoder: Send {
    fn spec(&self) -> AudioSpec;
    /// Total length in PCM frames (samples per channel) if known, after
    /// gapless trimming.
    fn total_frames(&self) -> Option<u64>;
    /// Decodes the next chunk, appending interleaved samples to `out`.
    /// Returns `Ok(false)` at the end of the stream.
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error>;
    /// Seeks to the given PCM frame. Returns the frame actually positioned at
    /// (sample accurate where the format allows it).
    fn seek(&mut self, frame: u64) -> Result<u64, Error>;
    /// Short codec name, e.g. "mp3", "flac", "aac", "opus", "vorbis", "alac", "pcm".
    fn codec_name(&self) -> &'static str;
}

/// Textual tags normalised across tag formats.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Tags {
    pub title: Option<String>,
    pub artist: Option<String>,
    pub album: Option<String>,
    pub album_artist: Option<String>,
    pub composer: Option<String>,
    pub genre: Option<String>,
    pub year: Option<i32>,
    pub track: Option<u32>,
    pub track_total: Option<u32>,
    pub disc: Option<u32>,
    pub disc_total: Option<u32>,
    pub comment: Option<String>,
    /// ReplayGain values in dB / linear peak.
    pub replaygain_track_gain: Option<f32>,
    pub replaygain_track_peak: Option<f32>,
    pub replaygain_album_gain: Option<f32>,
    pub replaygain_album_peak: Option<f32>,
}

/// An embedded picture (cover art).
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    pub mime: String,
    /// ID3v2/FLAC picture type (3 = front cover, 0 = other, ...).
    pub kind: u8,
    pub description: String,
    pub data: Vec<u8>,
}

/// Information about a media file without decoding audio.
#[derive(Debug, Clone)]
pub struct MediaInfo {
    /// Container name, e.g. "mp3", "flac", "ogg", "mp4", "wav", "aiff", "adts", "matroska".
    pub container: &'static str,
    /// Codec name as in [`Decoder::codec_name`].
    pub codec: &'static str,
    pub sample_rate: u32,
    pub channels: u16,
    pub duration_ms: Option<u64>,
    pub bitrate_kbps: Option<u32>,
    pub tags: Tags,
    pub pictures: Vec<Picture>,
}

/// File extensions (lower case, without dot) the library scanner should
/// consider.
pub const SUPPORTED_EXTENSIONS: &[&str] =
    &["mp3", "mp2", "flac", "ogg", "oga", "opus", "m4a", "m4b", "mp4", "aac", "wav", "wave", "aif", "aiff", "aifc", "mka", "webm"];

pub fn is_supported_extension(ext: &str) -> bool {
    let e = ext.to_ascii_lowercase();
    SUPPORTED_EXTENSIONS.contains(&e.as_str())
}

/// Opens a decoder for the given source. The container is detected from the
/// content; `ext_hint` (lower/upper case extension without dot) is only used
/// to break ties.
pub fn open(mut src: Box<dyn Source>, ext_hint: Option<&str>) -> Result<Box<dyn Decoder>, Error> {
    use probe::Format;
    let format = probe::probe(&mut src, ext_hint)?;
    match format {
        Format::Wav => wav::open(src),
        Format::Aiff => aiff::open(src),
        Format::Ogg => ogg::open(src),
        Format::Mp4 => mp4::open(src),
        Format::Adts => adts::open(src),
        Format::Matroska => mkv::open(src),
        Format::Mp3 => mp3::open(src),
        Format::Flac => flac::open(src),
    }
}

/// Reads stream information and tags. Pictures are only extracted when
/// `with_pictures` is true (they can be large).
pub fn read_info(src: &mut dyn Source, ext_hint: Option<&str>, with_pictures: bool) -> Result<MediaInfo, Error> {
    use probe::Format;
    let mut r = BufReader::with_capacity(32 * 1024, src);
    let format = probe::probe(&mut r, ext_hint)?;
    match format {
        Format::Wav => wav::read_info(&mut r, with_pictures),
        Format::Aiff => aiff::read_info(&mut r, with_pictures),
        Format::Ogg => ogg::read_info(&mut r, with_pictures),
        Format::Mp4 => mp4::read_info(&mut r, with_pictures),
        Format::Adts => adts::read_info(&mut r, with_pictures),
        Format::Matroska => mkv::read_info(&mut r, with_pictures),
        Format::Mp3 => mp3::read_info(&mut r, with_pictures),
        Format::Flac => flac::read_info(&mut r, with_pictures),
    }
}

/// Downmixes interleaved WAVE-order audio with `channels` channels to
/// interleaved stereo (ITU-R BS.775 style coefficients, clipped-safe).
pub fn downmix_to_stereo(input: &[f32], channels: u16, out: &mut Vec<f32>) {
    downmix::downmix_to_stereo(input, channels, out)
}

pub use resample::Resampler;
