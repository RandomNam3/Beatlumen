//! Container-level metadata: ID3v2 / ID3v1 / APEv2 / Lyrics3 tag skipping
//! and the Xing/Info (+ LAME extension) and VBRI VBR headers.

use crate::header::FrameHeader;
use std::io::{self, Read, Seek, SeekFrom};

/// If `b` starts with an ID3v2 header, returns the total tag length
/// (header, body and optional footer). `b` must hold at least 10 bytes.
pub(crate) fn id3v2_len(b: &[u8]) -> Option<u64> {
    if b.len() < 10 || &b[..3] != b"ID3" || b[3] == 0xFF || b[4] == 0xFF {
        return None;
    }
    if b[6..10].iter().any(|&x| x & 0x80 != 0) {
        return None;
    }
    let size = b[6..10]
        .iter()
        .fold(0u64, |acc, &x| (acc << 7) | u64::from(x));
    let footer = if b[3] >= 4 && b[5] & 0x10 != 0 { 10 } else { 0 };
    Some(10 + size + footer)
}

fn read_at<R: Read + Seek>(r: &mut R, pos: u64, buf: &mut [u8]) -> io::Result<bool> {
    r.seek(SeekFrom::Start(pos))?;
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) => return Ok(false),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Determines where the audio data ends by stripping trailing ID3v1,
/// APEv2, Lyrics3v2 and appended ID3v2 tags. `start` is the lowest offset
/// that can belong to audio and `len` the stream length.
pub(crate) fn find_audio_end<R: Read + Seek>(r: &mut R, start: u64, len: u64) -> io::Result<u64> {
    let mut end = len;
    for _ in 0..8 {
        let avail = end.saturating_sub(start);
        // One read of the last (up to) 128 bytes serves all footer checks.
        let n = avail.min(128) as usize;
        if n < 10 {
            break;
        }
        let mut buf = [0u8; 128];
        let tail = &mut buf[128 - n..];
        if !read_at(r, end - n as u64, tail)? {
            break;
        }
        let b = &buf[..];
        if n == 128 && &b[..3] == b"TAG" {
            end -= 128;
            continue;
        }
        if n >= 32 && &b[96..104] == b"APETAGEX" {
            let f = &b[96..];
            let size = u64::from(u32::from_le_bytes([f[12], f[13], f[14], f[15]]));
            let flags = u32::from_le_bytes([f[20], f[21], f[22], f[23]]);
            let total = size + if flags & 0x8000_0000 != 0 { 32 } else { 0 };
            if total >= 32 && total <= avail {
                end -= total;
                continue;
            }
        }
        if n >= 15 && &b[119..] == b"LYRICS200" && b[113..119].iter().all(u8::is_ascii_digit) {
            let size = b[113..119]
                .iter()
                .fold(0u64, |a, &d| a * 10 + u64::from(d - b'0'));
            if size + 15 <= avail {
                end -= size + 15;
                continue;
            }
        }
        if n >= 20 && &b[118..121] == b"3DI" && b[124..].iter().all(|&x| x & 0x80 == 0) {
            let size = b[124..]
                .iter()
                .fold(0u64, |acc, &x| (acc << 7) | u64::from(x));
            if size + 20 <= avail {
                end -= size + 20;
                continue;
            }
        }
        break;
    }
    Ok(end)
}

/// Contents of a Xing/Info header (and its LAME extension).
#[derive(Clone, Debug, Default)]
pub(crate) struct XingInfo {
    /// "Info" tag (CBR) rather than "Xing" (VBR).
    pub is_info: bool,
    pub frames: Option<u32>,
    pub bytes: Option<u32>,
    pub toc: Option<[u8; 100]>,
    /// Encoder delay and padding from the LAME extension, in samples.
    pub lame: Option<(u32, u32)>,
}

/// VBRI (Fraunhofer) header.
#[derive(Clone, Debug, Default)]
pub(crate) struct VbriInfo {
    pub frames: u32,
    /// Byte sizes of consecutive groups of `frames_per_entry` frames.
    pub table: Vec<u32>,
    pub frames_per_entry: u32,
}

fn be32(b: &[u8]) -> u32 {
    u32::from_be_bytes([b[0], b[1], b[2], b[3]])
}

/// Parses a Xing/Info header in the (Layer III) frame `frame`.
pub(crate) fn parse_xing(h: &FrameHeader, frame: &[u8]) -> Option<XingInfo> {
    if h.layer != 3 {
        return None;
    }
    let mut off = 4 + if h.crc { 2 } else { 0 } + h.side_info_len();
    let tag = frame.get(off..off + 8)?;
    let is_info = match &tag[..4] {
        b"Xing" => false,
        b"Info" => true,
        _ => return None,
    };
    let flags = be32(&tag[4..]);
    off += 8;
    let mut info = XingInfo {
        is_info,
        ..XingInfo::default()
    };
    if flags & 1 != 0 {
        info.frames = Some(be32(frame.get(off..off + 4)?));
        off += 4;
    }
    if flags & 2 != 0 {
        info.bytes = Some(be32(frame.get(off..off + 4)?));
        off += 4;
    }
    if flags & 4 != 0 {
        let t = frame.get(off..off + 100)?;
        let mut toc = [0u8; 100];
        toc.copy_from_slice(t);
        info.toc = Some(toc);
        off += 100;
    }
    if flags & 8 != 0 {
        off += 4;
    }
    if let Some(lame) = frame.get(off..off + 24) {
        let enc = &lame[..4];
        if enc == b"LAME" || enc == b"Lavf" || enc == b"Lavc" || enc == b"L3.9" {
            let d = &lame[21..24];
            let delay = (u32::from(d[0]) << 4) | (u32::from(d[1]) >> 4);
            let padding = ((u32::from(d[1]) & 0x0F) << 8) | u32::from(d[2]);
            info.lame = Some((delay, padding));
        }
    }
    Some(info)
}

/// Parses a VBRI header in the (Layer III) frame `frame`.
pub(crate) fn parse_vbri(h: &FrameHeader, frame: &[u8]) -> Option<VbriInfo> {
    if h.layer != 3 {
        return None;
    }
    let b = frame.get(36..62)?;
    if &b[..4] != b"VBRI" {
        return None;
    }
    let frames = be32(&b[14..18]);
    let entries = usize::from(u16::from_be_bytes([b[18], b[19]]));
    let scale = u32::from(u16::from_be_bytes([b[20], b[21]]));
    let entry_size = usize::from(u16::from_be_bytes([b[22], b[23]]));
    let frames_per_entry = u32::from(u16::from_be_bytes([b[24], b[25]]));
    let mut table = Vec::new();
    if (1..=4).contains(&entry_size) && frames_per_entry > 0 {
        if let Some(t) = frame.get(62..62 + entries * entry_size) {
            table = t
                .chunks_exact(entry_size)
                .map(|c| {
                    c.iter()
                        .fold(0u32, |a, &x| (a << 8) | u32::from(x))
                        .saturating_mul(scale.max(1))
                })
                .collect();
        }
    }
    Some(VbriInfo {
        frames,
        table,
        frames_per_entry,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn id3v2_sizes() {
        let mut h = *b"ID3\x04\x00\x00\x00\x00\x02\x01";
        assert_eq!(id3v2_len(&h), Some(10 + 257));
        h[5] = 0x10;
        assert_eq!(id3v2_len(&h), Some(10 + 257 + 10));
        h[3] = 3;
        assert_eq!(id3v2_len(&h), Some(10 + 257));
        h[8] = 0x80;
        assert_eq!(id3v2_len(&h), None);
    }

    #[test]
    fn trailing_tags() {
        let mut data = vec![0u8; 1000];
        // APEv2 with header (32 + 50 items + 32 footer).
        let mut ape_hdr = b"APETAGEX".to_vec();
        ape_hdr.extend_from_slice(&2000u32.to_le_bytes());
        ape_hdr.extend_from_slice(&82u32.to_le_bytes());
        ape_hdr.extend_from_slice(&0u32.to_le_bytes());
        ape_hdr.extend_from_slice(&0xA000_0000u32.to_le_bytes());
        ape_hdr.extend_from_slice(&[0; 8]);
        let mut ape_ftr = ape_hdr.clone();
        ape_ftr[20..24].copy_from_slice(&0x8000_0000u32.to_le_bytes());
        data.extend_from_slice(&ape_hdr);
        data.extend_from_slice(&[0xFF; 50]);
        data.extend_from_slice(&ape_ftr);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, 0xFF);
        data.extend_from_slice(&v1);
        let len = data.len() as u64;
        assert_eq!(
            find_audio_end(&mut Cursor::new(data), 0, len).unwrap(),
            1000
        );
    }

    #[test]
    fn lyrics3_and_appended_id3v2() {
        let mut data = vec![0xFFu8; 1000];
        let mut lyr = b"LYRICSBEGIN".to_vec();
        lyr.extend_from_slice(&[b'x'; 20]);
        let size = lyr.len();
        lyr.extend_from_slice(format!("{size:06}LYRICS200").as_bytes());
        data.extend_from_slice(&lyr);
        let mut v1 = b"TAG".to_vec();
        v1.resize(128, 0);
        data.extend_from_slice(&v1);
        let len = data.len() as u64;
        assert_eq!(
            find_audio_end(&mut Cursor::new(&data), 0, len).unwrap(),
            1000
        );

        let mut data = vec![0xFFu8; 1000];
        let mut tag = b"ID3\x04\x00\x10\x00\x00\x00\x05".to_vec();
        tag.extend_from_slice(&[0; 5]);
        tag.extend_from_slice(b"3DI\x04\x00\x10\x00\x00\x00\x05");
        data.extend_from_slice(&tag);
        let len = data.len() as u64;
        assert_eq!(
            find_audio_end(&mut Cursor::new(&data), 0, len).unwrap(),
            1000
        );
        // Too little data for any tag: unchanged.
        assert_eq!(
            find_audio_end(&mut Cursor::new(&data[..5]), 0, 5).unwrap(),
            5
        );
    }
}
