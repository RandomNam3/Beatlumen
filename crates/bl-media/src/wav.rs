//! WAV (RIFF), RF64 and BW64 demuxer.
//!
//! Supports PCM 8/16/24/32-bit integer, 32/64-bit float, A-law/µ-law,
//! `WAVE_FORMAT_EXTENSIBLE` (samples are already in WAVE channel-mask
//! order), `ds64` sizes, odd chunk padding, streaming files with unknown
//! data sizes, and `LIST/INFO` + `id3 ` tag chunks.

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::pcm::{PcmDecoder, PcmStream, SampleFormat};
use crate::tags::{id3, riff};
use crate::util::{
    bitrate_kbps, frames_to_ms, invalid, le16, le32, le64, read_up_to, read_vec, stream_len,
    unsupported,
};
use crate::{Decoder, Error, MediaInfo, Picture, Source, Tags};

pub(crate) struct WavFile {
    pub stream: PcmStream,
    pub tags: Tags,
    pub pictures: Vec<Picture>,
}

fn plausible_id(id: &[u8]) -> bool {
    id.iter().all(|&c| (0x20..0x7f).contains(&c))
}

/// Parses the chunk structure. When `want_tags` is false, parsing stops at
/// the data chunk once the format is known.
pub(crate) fn parse<R: Read + Seek + ?Sized>(
    r: &mut R,
    want_tags: bool,
    want_pictures: bool,
) -> Result<WavFile, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut hdr = [0u8; 12];
    if read_up_to(r, &mut hdr)? < 12 || &hdr[8..12] != b"WAVE" {
        return invalid("not a WAVE file");
    }
    let rf64 = match &hdr[0..4] {
        b"RIFF" => false,
        b"RF64" | b"BW64" => true,
        _ => return invalid("not a RIFF file"),
    };
    let mut ds64_data: Option<u64> = None;
    let mut fmt: Option<(SampleFormat, u16, u32, usize)> = None;
    let mut data: Option<(u64, u64)> = None;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    let mut pos = 12u64;
    let mut prev_odd = false;
    loop {
        r.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        if read_up_to(r, &mut ch)? < 8 {
            break;
        }
        let id: [u8; 4] = ch[0..4].try_into().unwrap();
        if !plausible_id(&id) {
            // Some writers forget the pad byte of odd sized chunks: retry
            // one byte earlier before giving up.
            if prev_odd {
                prev_odd = false;
                r.seek(SeekFrom::Start(pos - 1))?;
                let mut ch2 = [0u8; 8];
                if read_up_to(r, &mut ch2)? == 8
                    && plausible_id(&ch2[0..4])
                    && ch2[0..4] != [b' '; 4]
                {
                    pos -= 1;
                    continue;
                }
            }
            break;
        }
        let size32 = u32::from_le_bytes(ch[4..8].try_into().unwrap());
        let body = pos + 8;
        let mut size = size32 as u64;
        match &id {
            b"ds64" => {
                let d = read_vec(r, size.min(1 << 16), 1 << 16)?;
                ds64_data = le64(&d, 8);
            }
            b"fmt " => {
                let d = read_vec(r, size.min(1 << 16), 1 << 16)?;
                fmt = Some(parse_fmt(&d)?);
            }
            b"data" => {
                let avail = file_len.saturating_sub(body);
                if rf64 && size32 == 0xffff_ffff {
                    size = ds64_data.unwrap_or(avail);
                } else if size32 == 0xffff_ffff || (size32 == 0 && avail > 0) {
                    size = avail; // streaming writer, size unknown
                }
                size = size.min(avail);
                data = Some((body, size));
                if !want_tags && fmt.is_some() {
                    break;
                }
            }
            b"LIST" if want_tags => {
                if let Ok(d) = read_vec(r, size, 16 << 20) {
                    riff::parse_list_info(&d, &mut tags);
                }
            }
            b"id3 " | b"ID3 " if want_tags => {
                if let Ok(d) = read_vec(r, size, 64 << 20) {
                    id3::parse_id3v2_bytes(&d, &mut tags, want_pictures.then_some(&mut pictures));
                }
            }
            _ => {}
        }
        pos = body + size + (size & 1);
        prev_odd = size & 1 == 1;
        if pos >= file_len {
            break;
        }
    }
    let Some((fmt, channels, rate, block)) = fmt else {
        return invalid("missing fmt chunk");
    };
    let Some((data_start, data_len)) = data else {
        return invalid("missing data chunk");
    };
    let stream = PcmStream {
        fmt,
        channels,
        sample_rate: rate,
        frame_bytes: block,
        data_start,
        total_frames: data_len / block.max(1) as u64,
    };
    stream.validate()?;
    Ok(WavFile {
        stream,
        tags,
        pictures,
    })
}

/// Parses a `fmt ` chunk: (format, channels, rate, block align).
fn parse_fmt(d: &[u8]) -> Result<(SampleFormat, u16, u32, usize), Error> {
    let (Some(mut tag), Some(channels), Some(rate), Some(block), Some(bits)) =
        (le16(d, 0), le16(d, 2), le32(d, 4), le16(d, 12), le16(d, 14))
    else {
        return invalid("fmt chunk too short");
    };
    if tag == 0xfffe {
        // WAVE_FORMAT_EXTENSIBLE: the sub-format GUID starts with the format tag.
        tag = le16(d, 24).ok_or_else(|| Error::InvalidData("short extensible fmt".into()))?;
    }
    if channels == 0 {
        return invalid("zero channels");
    }
    let block = if block == 0 {
        channels as usize * (bits as usize).div_ceil(8)
    } else {
        block as usize
    };
    let bytes = block / channels as usize;
    let fmt = match tag {
        1 => SampleFormat::int(bytes, true, true),
        3 => match bytes {
            4 => Some(SampleFormat::F32Le),
            8 => Some(SampleFormat::F64Le),
            _ => None,
        },
        6 if bytes == 1 => Some(SampleFormat::ALaw),
        7 if bytes == 1 => Some(SampleFormat::MuLaw),
        _ => return unsupported(format!("WAVE format tag 0x{tag:04x}")),
    };
    match fmt {
        Some(f) => Ok((f, channels, rate, block)),
        None => unsupported(format!("{bits}-bit samples in {bytes}-byte containers")),
    }
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let wav = parse(&mut r, false, false)?;
    Ok(Box::new(PcmDecoder::new(r, wav.stream)?))
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(
    r: &mut R,
    with_pictures: bool,
) -> Result<MediaInfo, Error> {
    let w = parse(r, true, with_pictures)?;
    let s = &w.stream;
    Ok(MediaInfo {
        container: "wav",
        codec: "pcm",
        sample_rate: s.sample_rate,
        channels: s.channels,
        duration_ms: Some(frames_to_ms(s.total_frames, s.sample_rate)),
        bitrate_kbps: bitrate_kbps(
            s.total_frames * s.frame_bytes as u64,
            s.total_frames,
            s.sample_rate,
        ),
        tags: w.tags,
        pictures: w.pictures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn chunk(id: &[u8; 4], body: &[u8], pad: bool) -> Vec<u8> {
        let mut v = id.to_vec();
        v.extend_from_slice(&(body.len() as u32).to_le_bytes());
        v.extend_from_slice(body);
        if pad && body.len() % 2 == 1 {
            v.push(0);
        }
        v
    }

    fn wav(pad: bool) -> Vec<u8> {
        let mut fmt = Vec::new();
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&1u16.to_le_bytes());
        fmt.extend_from_slice(&8000u32.to_le_bytes());
        fmt.extend_from_slice(&16000u32.to_le_bytes());
        fmt.extend_from_slice(&2u16.to_le_bytes());
        fmt.extend_from_slice(&16u16.to_le_bytes());
        let mut body = b"WAVE".to_vec();
        body.extend(chunk(b"fmt ", &fmt, pad));
        body.extend(chunk(b"junk", b"odd", pad));
        body.extend(chunk(b"data", &[0, 0x40, 0, 0xc0, 0xff, 0x7f], pad));
        let mut info = b"INFO".to_vec();
        info.extend(chunk(b"INAM", b"Name\0", true));
        body.extend(chunk(b"LIST", &info, pad));
        let mut out = b"RIFF".to_vec();
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        out.extend(body);
        out
    }

    #[test]
    fn odd_chunks_with_and_without_padding() {
        for pad in [true, false] {
            let w = parse(&mut Cursor::new(wav(pad)), true, false).unwrap();
            assert_eq!(w.stream.total_frames, 3, "pad {pad}");
            assert_eq!(w.tags.title.as_deref(), Some("Name"), "pad {pad}");
            let mut d = PcmDecoder::new(Cursor::new(wav(pad)), w.stream).unwrap();
            let mut out = Vec::new();
            while d.next_chunk(&mut out).unwrap() {}
            assert_eq!(out, vec![0.5, -0.5, 32767.0 / 32768.0]);
        }
    }
}
