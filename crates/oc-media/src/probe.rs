//! Container detection from magic bytes. ID3v2 tags are skipped to decide
//! between MP3, FLAC and ADTS; the file extension only breaks ties.

use std::io::{Read, Seek, SeekFrom};

use crate::tags::id3;
use crate::util::read_up_to;
use crate::{mp3, Error};

/// Detected container format.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Format {
    Wav,
    Aiff,
    Ogg,
    Mp4,
    Adts,
    Matroska,
    Mp3,
    Flac,
}

fn from_ext(ext: Option<&str>) -> Option<Format> {
    let e = ext?.trim_start_matches('.').to_ascii_lowercase();
    Some(match e.as_str() {
        "mp3" | "mp2" | "mp1" | "mpga" => Format::Mp3,
        "flac" => Format::Flac,
        "ogg" | "oga" | "opus" => Format::Ogg,
        "m4a" | "m4b" | "mp4" | "m4p" | "mov" => Format::Mp4,
        "aac" | "adts" => Format::Adts,
        "wav" | "wave" => Format::Wav,
        "aif" | "aiff" | "aifc" => Format::Aiff,
        "mka" | "mkv" | "webm" => Format::Matroska,
        _ => return None,
    })
}

/// Checks for an ADTS frame at the start of `d` followed by another one.
pub(crate) fn looks_like_adts(d: &[u8]) -> bool {
    let Some(h) = oc_aac::AdtsHeader::parse(d) else { return false };
    if d[1] & 0x06 != 0 {
        return false; // layer must be 0
    }
    match d.get(h.frame_length..) {
        Some(next) if next.len() >= 7 => oc_aac::AdtsHeader::parse(next).is_some(),
        _ => true,
    }
}

fn classify_head(d: &[u8]) -> Option<Format> {
    if d.len() >= 12 && &d[8..12] == b"WAVE" && matches!(&d[0..4], b"RIFF" | b"RF64" | b"BW64") {
        return Some(Format::Wav);
    }
    if d.len() >= 12 && &d[0..4] == b"FORM" && matches!(&d[8..12], b"AIFF" | b"AIFC") {
        return Some(Format::Aiff);
    }
    if d.starts_with(b"OggS") {
        return Some(Format::Ogg);
    }
    if d.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if d.starts_with(&[0x1a, 0x45, 0xdf, 0xa3]) {
        return Some(Format::Matroska);
    }
    if d.len() >= 8 && matches!(&d[4..8], b"ftyp" | b"moov" | b"mdat" | b"wide" | b"free" | b"skip" | b"pnot") {
        return Some(Format::Mp4);
    }
    None
}

fn classify_stream(d: &[u8]) -> Option<Format> {
    if d.starts_with(b"fLaC") {
        return Some(Format::Flac);
    }
    if d.len() >= 2 && d[0] == 0xff && d[1] & 0xf6 == 0xf0 && looks_like_adts(d) {
        return Some(Format::Adts);
    }
    if mp3::looks_like_mpeg(d) {
        return Some(Format::Mp3);
    }
    None
}

/// Detects the container of the stream (position is restored to 0).
pub(crate) fn probe<R: Read + Seek + ?Sized>(r: &mut R, ext_hint: Option<&str>) -> Result<Format, Error> {
    r.seek(SeekFrom::Start(0))?;
    let mut head = vec![0u8; 16 * 1024];
    let n = read_up_to(r, &mut head)?;
    head.truncate(n);
    let hint = from_ext(ext_hint);
    let result = if let Some(f) = classify_head(&head) {
        Some(f)
    } else {
        let mut d = head.clone();
        let mut had_id3 = false;
        if head.starts_with(b"ID3") {
            r.seek(SeekFrom::Start(0))?;
            let off = id3::skip_id3v2(r)?;
            had_id3 = off > 0;
            r.seek(SeekFrom::Start(off))?;
            let mut buf = vec![0u8; 16 * 1024];
            let n = read_up_to(r, &mut buf)?;
            buf.truncate(n);
            d = buf;
        }
        match classify_stream(&d) {
            Some(f) => Some(f),
            None => {
                // Junk or padding before the first frame: search for a
                // well-confirmed MPEG or ADTS sync.
                let confirm = if matches!(hint, Some(Format::Mp3)) || had_id3 { 2 } else { 4 };
                let mpeg = mp3::find_sync(&d, confirm).map(|(o, _)| o);
                let adts = (0..d.len().saturating_sub(7)).find(|&i| d[i] == 0xff && d[i + 1] & 0xf6 == 0xf0 && looks_like_adts(&d[i..]));
                match (mpeg, adts, hint) {
                    (_, Some(_), Some(Format::Adts)) => Some(Format::Adts),
                    (Some(_), _, Some(Format::Mp3)) => Some(Format::Mp3),
                    (Some(a), Some(b), _) => Some(if a <= b { Format::Mp3 } else { Format::Adts }),
                    (Some(_), None, _) => Some(Format::Mp3),
                    (None, Some(_), _) if had_id3 || hint.is_some() => Some(Format::Adts),
                    _ if had_id3 => hint.filter(|h| matches!(h, Format::Mp3 | Format::Flac | Format::Adts)),
                    _ => None,
                }
            }
        }
    };
    r.seek(SeekFrom::Start(0))?;
    result.ok_or_else(|| Error::Unsupported("unrecognised container format".into()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn magic() {
        let mut wav = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        wav.resize(64, 0);
        assert_eq!(probe(&mut Cursor::new(wav), None).unwrap(), Format::Wav);
        assert_eq!(probe(&mut Cursor::new(b"OggS\0\x02".to_vec()), None).unwrap(), Format::Ogg);
        assert_eq!(probe(&mut Cursor::new(b"\0\0\0\x20ftypM4A ".to_vec()), None).unwrap(), Format::Mp4);
        assert!(probe(&mut Cursor::new(b"hello world, this is text".to_vec()), Some("mp3")).is_err());
        // ID3 tag followed by fLaC
        let mut d = b"ID3\x04\0\0\0\0\0\x05\0\0\0\0\0fLaC\0\0\0\x22".to_vec();
        d.resize(100, 0);
        assert_eq!(probe(&mut Cursor::new(d), Some("mp3")).unwrap(), Format::Flac);
    }

    #[test]
    fn mpeg_after_id3() {
        let mut d = b"ID3\x03\0\0\0\0\0\x02\0\0".to_vec();
        for _ in 0..5 {
            let mut f = vec![0u8; 417];
            f[..4].copy_from_slice(&[0xff, 0xfb, 0x90, 0x00]);
            d.extend_from_slice(&f);
        }
        assert_eq!(probe(&mut Cursor::new(d), None).unwrap(), Format::Mp3);
    }
}
