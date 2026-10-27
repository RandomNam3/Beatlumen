//! AIFF / AIFF-C demuxer (big-endian PCM, `sowt` little-endian, `fl32` /
//! `fl64` floats, `in24`/`in32`, A-law/µ-law), 80-bit extended sample
//! rates, `ID3 ` chunks and the `NAME`/`AUTH`/`ANNO` text chunks.
//!
//! Multichannel AIFF files are passed through in stored order (modern
//! writers use WAVE order).

use std::io::{BufReader, Read, Seek, SeekFrom};

use crate::pcm::{PcmDecoder, PcmStream, SampleFormat};
use crate::tags::{id3, riff};
use crate::util::{be16, be32, bitrate_kbps, frames_to_ms, invalid, read_up_to, read_vec, stream_len, unsupported};
use crate::{Decoder, Error, MediaInfo, Picture, Source, Tags};

pub(crate) struct AiffFile {
    pub stream: PcmStream,
    pub tags: Tags,
    pub pictures: Vec<Picture>,
}

/// Converts an IEEE 754 80-bit extended float to f64.
pub(crate) fn extended_to_f64(b: &[u8; 10]) -> f64 {
    let exp = (((b[0] & 0x7f) as i32) << 8) | b[1] as i32;
    let mant = u64::from_be_bytes(b[2..10].try_into().unwrap());
    if exp == 0 && mant == 0 {
        return 0.0;
    }
    if exp == 0x7fff {
        return f64::NAN;
    }
    let v = mant as f64 * 2f64.powi(exp - 16383 - 63);
    if b[0] & 0x80 != 0 {
        -v
    } else {
        v
    }
}

pub(crate) fn parse<R: Read + Seek + ?Sized>(r: &mut R, want_tags: bool, want_pictures: bool) -> Result<AiffFile, Error> {
    let file_len = stream_len(r)?;
    r.seek(SeekFrom::Start(0))?;
    let mut hdr = [0u8; 12];
    if read_up_to(r, &mut hdr)? < 12 || &hdr[0..4] != b"FORM" {
        return invalid("not an IFF file");
    }
    let aifc = match &hdr[8..12] {
        b"AIFF" => false,
        b"AIFC" => true,
        _ => return invalid("not an AIFF file"),
    };
    let mut comm: Option<(u16, u32, u16, f64, [u8; 4])> = None;
    let mut ssnd: Option<(u64, u64)> = None;
    let mut tags = Tags::default();
    let mut pictures = Vec::new();
    let mut pos = 12u64;
    while pos + 8 <= file_len {
        r.seek(SeekFrom::Start(pos))?;
        let mut ch = [0u8; 8];
        if read_up_to(r, &mut ch)? < 8 {
            break;
        }
        let id: [u8; 4] = ch[0..4].try_into().unwrap();
        let mut size = u32::from_be_bytes(ch[4..8].try_into().unwrap()) as u64;
        let body = pos + 8;
        match &id {
            b"COMM" => {
                let d = read_vec(r, size.min(4096), 4096)?;
                let (Some(ch), Some(frames), Some(bits)) = (be16(&d, 0), be32(&d, 2), be16(&d, 6)) else {
                    return invalid("short COMM chunk");
                };
                let ext: [u8; 10] = d.get(8..18).ok_or_else(|| Error::InvalidData("short COMM chunk".into()))?.try_into().unwrap();
                let rate = extended_to_f64(&ext);
                let comp = if aifc { d.get(18..22).map(|c| c.try_into().unwrap()).unwrap_or(*b"NONE") } else { *b"NONE" };
                comm = Some((ch, frames, bits, rate, comp));
            }
            b"SSND" => {
                let mut h = [0u8; 8];
                if read_up_to(r, &mut h)? < 8 {
                    return invalid("short SSND chunk");
                }
                let offset = u32::from_be_bytes(h[0..4].try_into().unwrap()) as u64;
                let avail = file_len.saturating_sub(body);
                if size == 0 || size > avail {
                    size = avail; // streaming writer or truncated file
                }
                let start = body + 8 + offset;
                ssnd = Some((start, size.saturating_sub(8 + offset)));
                if !want_tags && comm.is_some() {
                    break;
                }
            }
            b"ID3 " | b"id3 " if want_tags => {
                if let Ok(d) = read_vec(r, size, 64 << 20) {
                    id3::parse_id3v2_bytes(&d, &mut tags, want_pictures.then_some(&mut pictures));
                }
            }
            b"NAME" | b"AUTH" | b"ANNO" if want_tags => {
                if let Ok(d) = read_vec(r, size, 1 << 20) {
                    riff::apply_aiff_text(&id, &d, &mut tags);
                }
            }
            _ => {}
        }
        pos = body + size + (size & 1);
    }
    let Some((channels, frames, bits, rate, comp)) = comm else { return invalid("missing COMM chunk") };
    let Some((data_start, data_len)) = ssnd else { return invalid("missing SSND chunk") };
    let bytes = (bits as usize).div_ceil(8);
    let fmt = match &comp {
        b"NONE" | b"twos" | b"TWOS" => SampleFormat::int(bytes, false, false),
        b"sowt" | b"SOWT" => SampleFormat::int(bytes, true, false),
        b"raw " => Some(SampleFormat::U8),
        b"in24" => Some(SampleFormat::S24Be),
        b"in32" => Some(SampleFormat::S32Be),
        b"42ni" => Some(SampleFormat::S24Le),
        b"23ni" => Some(SampleFormat::S32Le),
        b"fl32" | b"FL32" => Some(SampleFormat::F32Be),
        b"fl64" | b"FL64" => Some(SampleFormat::F64Be),
        b"ulaw" | b"ULAW" => Some(SampleFormat::MuLaw),
        b"alaw" | b"ALAW" => Some(SampleFormat::ALaw),
        _ => return unsupported(format!("AIFC compression '{}'", String::from_utf8_lossy(&comp))),
    };
    let Some(fmt) = fmt else { return unsupported(format!("{bits}-bit AIFF samples")) };
    if !(rate.is_finite() && (1.0..4.0e9).contains(&rate)) {
        return invalid("bad AIFF sample rate");
    }
    let frame_bytes = channels as usize * fmt.bytes();
    let stream = PcmStream {
        fmt,
        channels,
        sample_rate: rate.round() as u32,
        frame_bytes,
        data_start,
        total_frames: (frames as u64).min(data_len / frame_bytes.max(1) as u64),
    };
    stream.validate()?;
    Ok(AiffFile { stream, tags, pictures })
}

pub(crate) fn open(src: Box<dyn Source>) -> Result<Box<dyn Decoder>, Error> {
    let mut r = BufReader::with_capacity(64 * 1024, src);
    let a = parse(&mut r, false, false)?;
    Ok(Box::new(PcmDecoder::new(r, a.stream)?))
}

pub(crate) fn read_info<R: Read + Seek + ?Sized>(r: &mut R, with_pictures: bool) -> Result<MediaInfo, Error> {
    let a = parse(r, true, with_pictures)?;
    let s = &a.stream;
    Ok(MediaInfo {
        container: "aiff",
        codec: "pcm",
        sample_rate: s.sample_rate,
        channels: s.channels,
        duration_ms: Some(frames_to_ms(s.total_frames, s.sample_rate)),
        bitrate_kbps: bitrate_kbps(s.total_frames * s.frame_bytes as u64, s.total_frames, s.sample_rate),
        tags: a.tags,
        pictures: a.pictures,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extended() {
        // 44100 Hz
        let b = [0x40, 0x0e, 0xac, 0x44, 0, 0, 0, 0, 0, 0];
        assert_eq!(extended_to_f64(&b), 44100.0);
        let b = [0x40, 0x0e, 0xbb, 0x80, 0, 0, 0, 0, 0, 0];
        assert_eq!(extended_to_f64(&b), 48000.0);
    }
}
