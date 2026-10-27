//! MPEG audio (MP1/MP2/MP3) container support: frame header parsing,
//! Xing/Info/LAME/VBRI headers for fast `read_info`, and the adapter around
//! the stream-level [`bl_mp3::Mp3Decoder`].

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::tags::{self, id3};
use crate::util::{be16, be32, bitrate_kbps, frames_to_ms, mul_div, read_up_to, stream_len};
use crate::window::Window;
use crate::{AudioSpec, Decoder, Error, MediaInfo, Source, Tags};

/// A parsed MPEG audio frame header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MpegHeader {
    /// 1 = MPEG-1, 2 = MPEG-2, 25 = MPEG-2.5.
    pub version: u8,
    pub layer: u8,
    pub bitrate_kbps: u32,
    pub sample_rate: u32,
    pub channels: u16,
    pub frame_len: usize,
    pub samples: u32,
    pub protected: bool,
}

const BITRATES: [[u16; 15]; 5] = [
    [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448], // V1 L1
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],    // V1 L2
    [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],     // V1 L3
    [0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256],    // V2 L1
    [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160],         // V2 L2/L3
];

/// Parses a 4-byte frame header. Free-format streams are not accepted.
pub(crate) fn parse_header(b: &[u8]) -> Option<MpegHeader> {
    let h = be32(b, 0)?;
    if h >> 21 != 0x7ff {
        return None;
    }
    let version = match (h >> 19) & 3 {
        0 => 25,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let layer = match (h >> 17) & 3 {
        1 => 3,
        2 => 2,
        3 => 1,
        _ => return None,
    };
    let br_idx = ((h >> 12) & 15) as usize;
    let sr_idx = ((h >> 10) & 3) as usize;
    if br_idx == 0 || br_idx == 15 || sr_idx == 3 || h & 3 == 2 {
        return None;
    }
    let table = match (version, layer) {
        (1, l) => (l - 1) as usize,
        (_, 1) => 3,
        _ => 4,
    };
    let bitrate = BITRATES[table][br_idx] as u32;
    let base = [44100u32, 48000, 32000][sr_idx];
    let sample_rate = match version {
        1 => base,
        2 => base / 2,
        _ => base / 4,
    };
    let padding = (h >> 9) & 1;
    let channels = if (h >> 6) & 3 == 3 { 1 } else { 2 };
    let (frame_len, samples) = match layer {
        1 => (((12 * bitrate * 1000 / sample_rate) + padding) * 4, 384),
        2 => (144 * bitrate * 1000 / sample_rate + padding, 1152),
        _ => {
            if version == 1 {
                (144 * bitrate * 1000 / sample_rate + padding, 1152)
            } else {
                (72 * bitrate * 1000 / sample_rate + padding, 576)
            }
        }
    };
    if frame_len < 4 {
        return None;
    }
    Some(MpegHeader {
        version,
        layer,
        bitrate_kbps: bitrate,
        sample_rate,
        channels,
        frame_len: frame_len as usize,
        samples,
        protected: (h >> 16) & 1 == 0,
    })
}

fn compatible(a: &MpegHeader, b: &MpegHeader) -> bool {
    a.version == b.version && a.layer == b.layer && a.sample_rate == b.sample_rate
}

/// Finds the first frame in `buf` that is followed by `confirm` further
/// consistent frame headers (frames running past the buffer end count as
/// confirmed).
pub(crate) fn find_sync(buf: &[u8], confirm: usize) -> Option<(usize, MpegHeader)> {
    let mut i = 0;
    while i + 4 <= buf.len() {
        if buf[i] == 0xff && buf[i + 1] & 0xe0 == 0xe0 {
            if let Some(h) = parse_header(&buf[i..]) {
                let mut ok = true;
                let mut pos = i + h.frame_len;
                for _ in 0..confirm {
                    if pos + 4 > buf.len() {
                        break;
                    }
                    match parse_header(&buf[pos..]) {
                        Some(n) if compatible(&h, &n) => pos += n.frame_len,
                        _ => {
                            ok = false;
                            break;
                        }
                    }
                }
                if ok {
                    return Some((i, h));
                }
            }
        }
        i += 1;
    }
    None
}

/// Information from a Xing/Info/LAME or VBRI header.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct VbrInfo {
    pub frames: Option<u32>,
    pub bytes: Option<u32>,
    pub delay: u32,
    pub padding: u32,
}

/// Parses a Xing/Info (+LAME) or VBRI header in the first frame.
pub(crate) fn parse_vbr_header(frame: &[u8], h: &MpegHeader) -> Option<VbrInfo> {
    let side = match (h.version == 1, h.channels == 1) {
        (true, true) => 17,
        (true, false) => 32,
        (false, true) => 9,
        (false, false) => 17,
    };
    for off in [4 + side, 4 + side + 2] {
        let tag = frame.get(off..off + 4)?;
        if tag != b"Xing" && tag != b"Info" {
            continue;
        }
        let flags = be32(frame, off + 4)?;
        let mut p = off + 8;
        let mut info = VbrInfo::default();
        if flags & 1 != 0 {
            info.frames = be32(frame, p);
            p += 4;
        }
        if flags & 2 != 0 {
            info.bytes = be32(frame, p);
            p += 4;
        }
        if flags & 4 != 0 {
            p += 100;
        }
        if flags & 8 != 0 {
            p += 4;
        }
        // LAME / Lavc extension: 9-byte encoder string, delay/padding at +21.
        if let Some(enc) = frame.get(p..p + 4) {
            if enc.iter().all(|c| c.is_ascii_alphanumeric()) {
                if let Some(d) = frame.get(p + 21..p + 24) {
                    info.delay = ((d[0] as u32) << 4) | (d[1] as u32 >> 4);
                    info.padding = (((d[1] & 0x0f) as u32) << 8) | d[2] as u32;
                }
            }
        }
        return Some(info);
    }
    if frame.get(36..40) == Some(b"VBRI") {
        return Some(VbrInfo {
            frames: be32(frame, 36 + 14),
            bytes: be32(frame, 36 + 10),
            delay: be16(frame, 36 + 6).unwrap_or(0) as u32,
            padding: 0,
        });
    }
    None
}

pub(crate) fn codec_name(layer: u8) -> &'static str {
    match layer {
        1 => "mp1",
        2 => "mp2",
        _ => "mp3",
    }
}

/// Stream layout determined without decoding.
struct Layout {
    header: MpegHeader,
    first_frame: u64,
    audio_end: u64,
    vbr: Option<VbrInfo>,
}

impl Layout {
    fn total_frames(&self) -> u64 {
        let h = &self.header;
        match self.vbr.and_then(|v| v.frames.map(|f| (f, v))) {
            Some((frames, v)) => (frames as u64 * h.samples as u64).saturating_sub(v.delay as u64 + v.padding as u64),
            None => {
                // CBR estimate from the stream size and first frame bitrate.
                let bytes = self.audio_end.saturating_sub(self.first_frame);
                mul_div(bytes, 8 * h.sample_rate as u64, h.bitrate_kbps as u64 * 1000)
            }
        }
    }
}

fn scan_layout<R: Read + Seek + ?Sized>(r: &mut R, audio_start: u64, audio_end: u64) -> Result<Layout, Error> {
    // Look for the first frame within the first 256 KiB after the tags.
    let mut buf = vec![0u8; (audio_end.saturating_sub(audio_start)).min(256 * 1024) as usize];
    r.seek(SeekFrom::Start(audio_start))?;
    let n = read_up_to(r, &mut buf)?;
    buf.truncate(n);
    let (off, header) = find_sync(&buf, 2).ok_or_else(|| Error::InvalidData("no MPEG audio frame found".into()))?;
    let frame = &buf[off..(off + header.frame_len).min(buf.len())];
    let vbr = parse_vbr_header(frame, &header);
    Ok(Layout { header, first_frame: audio_start + off as u64, audio_end, vbr })
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    let audio_start = id3::read_id3v2_tags(r, &mut tags, with_pictures.then_some(&mut pictures))?;
    let end = tags::read_end_tags(r, file_len, with_pictures.then_some(&mut pictures))?;
    tags::merge_missing(&mut tags, &end.ape);
    tags::merge_missing(&mut tags, &end.id3v1);
    let layout = scan_layout(r, audio_start, end.audio_end)?;
    let h = layout.header;
    let total = layout.total_frames();
    let bitrate = match layout.vbr {
        Some(VbrInfo { frames: Some(_), bytes, .. }) => {
            let bytes = bytes.map_or(end.audio_end.saturating_sub(layout.first_frame), |b| b as u64);
            bitrate_kbps(bytes, total, h.sample_rate)
        }
        _ => Some(h.bitrate_kbps),
    };
    Ok(MediaInfo {
        container: "mp3",
        codec: codec_name(h.layer),
        sample_rate: h.sample_rate,
        channels: h.channels,
        duration_ms: Some(frames_to_ms(total, h.sample_rate)),
        bitrate_kbps: bitrate,
        tags,
        pictures,
    })
}

type Inner = bl_mp3::Mp3Decoder<BufReader<Window<Box<dyn Source>>>>;

struct Mp3Adapter {
    dec: Inner,
    codec: &'static str,
    total: Option<u64>,
    spec: AudioSpec,
}

pub(crate) fn open(mut src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let file_len = stream_len(&mut src)?;
    src.seek(SeekFrom::Start(0))?;
    let audio_start = id3::skip_id3v2(&mut src)?;
    let end = tags::read_end_tags(&mut src, file_len, None)?;
    let layout = scan_layout(&mut src, audio_start, end.audio_end)?;
    let win = Window::new(src, 0, end.audio_end)?;
    let dec = bl_mp3::Mp3Decoder::new(BufReader::with_capacity(64 * 1024, win)).map_err(Error::from)?;
    let spec = AudioSpec {
        sample_rate: if dec.sample_rate() > 0 { dec.sample_rate() } else { layout.header.sample_rate },
        channels: if dec.channels() > 0 { dec.channels() } else { layout.header.channels },
    };
    let total = dec.total_frames().or(Some(layout.total_frames()));
    Ok(Box::new(Mp3Adapter { dec, codec: codec_name(layout.header.layer), total, spec }))
}

impl Decoder for Mp3Adapter {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        self.total
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        self.dec.next_chunk(out).map_err(Error::from)
    }
    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        self.dec.seek(frame).map_err(Error::from)
    }
    fn codec_name(&self) -> &'static str {
        self.codec
    }
}

/// Validates that `data` starts with an MPEG frame (used by the prober).
pub(crate) fn looks_like_mpeg(data: &[u8]) -> bool {
    data.len() >= 4 && parse_header(data).is_some() && find_sync(data, 2).map(|(o, _)| o) == Some(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header() {
        // MPEG-1 Layer III, 128 kbps, 44.1 kHz, stereo.
        let h = parse_header(&[0xff, 0xfb, 0x90, 0x00]).unwrap();
        assert_eq!((h.version, h.layer, h.bitrate_kbps, h.sample_rate, h.channels), (1, 3, 128, 44100, 2));
        assert_eq!(h.frame_len, 417);
        assert!(parse_header(&[0xff, 0xfb, 0xf0, 0x00]).is_none());
        assert!(parse_header(&[0xff, 0xf1, 0x50, 0x80]).is_none()); // ADTS (layer 0)
    }
}
