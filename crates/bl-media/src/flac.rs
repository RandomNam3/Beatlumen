//! Native FLAC support: metadata parsing for `read_info` (STREAMINFO,
//! VORBIS_COMMENT, PICTURE) and the adapter around the stream-level
//! [`bl_flac::FlacDecoder`].

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::tags::{self, id3, vorbis_comment};
use crate::util::{bitrate_kbps, frames_to_ms, invalid, read_up_to, read_vec, stream_len};
use crate::window::Window;
use crate::{AudioSpec, Decoder, Error, MediaInfo, Source, Tags};

/// Decoded STREAMINFO fields.
#[derive(Debug, Clone, Copy)]
pub(crate) struct StreamInfo {
    pub sample_rate: u32,
    pub channels: u16,
    /// Total samples per channel, `None` if unknown.
    pub total: Option<u64>,
}

/// Parses a STREAMINFO block body (34 bytes).
pub(crate) fn parse_streaminfo(d: &[u8]) -> Option<StreamInfo> {
    if d.len() < 18 {
        return None;
    }
    let v = u64::from_be_bytes(d[10..18].try_into().ok()?);
    let sample_rate = (v >> 44) as u32;
    let channels = ((v >> 41) & 7) as u16 + 1;
    let total = v & 0xf_ffff_ffff;
    if sample_rate == 0 {
        return None;
    }
    Some(StreamInfo {
        sample_rate,
        channels,
        total: (total > 0).then_some(total),
    })
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(
    r: &mut R,
    with_pictures: bool,
) -> Result<MediaInfo, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut id3_tags = Tags::default();
    let mut pictures = Vec::new();
    let start = id3::read_id3v2_tags(r, &mut id3_tags, with_pictures.then_some(&mut pictures))?;
    let mut magic = [0u8; 4];
    if read_up_to(r, &mut magic)? < 4 || &magic != b"fLaC" {
        return invalid("missing fLaC marker");
    }
    let mut pos = start + 4;
    let mut info = None;
    let mut tags = Tags::default();
    loop {
        r.seek(SeekFrom::Start(pos))?;
        let mut h = [0u8; 4];
        if read_up_to(r, &mut h)? < 4 {
            return invalid("truncated FLAC metadata");
        }
        let last = h[0] & 0x80 != 0;
        let typ = h[0] & 0x7f;
        let len = u32::from_be_bytes([0, h[1], h[2], h[3]]) as u64;
        match typ {
            0 => info = parse_streaminfo(&read_vec(r, len, 1 << 16)?),
            4 => {
                let d = read_vec(r, len, 16 << 20)?;
                vorbis_comment::parse_vorbis_comment(
                    &d,
                    &mut tags,
                    with_pictures.then_some(&mut pictures),
                );
            }
            6 if with_pictures => {
                let d = read_vec(r, len, 16 << 20)?;
                if let Some(p) = vorbis_comment::parse_flac_picture(&d) {
                    pictures.push(p);
                }
            }
            127 => return invalid("invalid FLAC metadata block type"),
            _ => {}
        }
        pos += 4 + len;
        if last || pos >= file_len {
            break;
        }
    }
    let info = info.ok_or_else(|| Error::InvalidData("missing STREAMINFO".into()))?;
    tags::merge_missing(&mut tags, &id3_tags);
    let end = tags::read_end_tags(r, file_len, None)?;
    tags::merge_missing(&mut tags, &end.ape);
    tags::merge_missing(&mut tags, &end.id3v1);
    let audio_bytes = end.audio_end.saturating_sub(pos);
    Ok(MediaInfo {
        container: "flac",
        codec: "flac",
        sample_rate: info.sample_rate,
        channels: info.channels,
        duration_ms: info.total.map(|t| frames_to_ms(t, info.sample_rate)),
        bitrate_kbps: info
            .total
            .and_then(|t| bitrate_kbps(audio_bytes, t, info.sample_rate)),
        tags,
        pictures,
    })
}

type Inner = bl_flac::FlacDecoder<BufReader<Window<Box<dyn Source>>>>;

struct FlacAdapter {
    dec: Inner,
    spec: AudioSpec,
}

pub(crate) fn open(mut src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let file_len = stream_len(&mut src)?;
    let end = tags::read_end_tags(&mut src, file_len, None)?;
    let win = Window::new(src, 0, end.audio_end)?;
    let dec =
        bl_flac::FlacDecoder::new(BufReader::with_capacity(64 * 1024, win)).map_err(Error::from)?;
    let spec = AudioSpec {
        sample_rate: dec.sample_rate(),
        channels: dec.channels(),
    };
    if spec.sample_rate == 0 || spec.channels == 0 {
        return invalid("FLAC decoder reported an empty stream format");
    }
    Ok(Box::new(FlacAdapter { dec, spec }))
}

impl Decoder for FlacAdapter {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.dec.total_frames()
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        self.dec.next_chunk(out).map_err(Error::from)
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        self.dec.seek(frame).map_err(Error::from)
    }
    fn codec_name(&self) -> &'static str {
        "flac"
    }
}
