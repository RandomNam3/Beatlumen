//! ID3v2.2 / 2.3 / 2.4 and ID3v1 tag reader.
//!
//! Supports all four text encodings, tag- and frame-level
//! unsynchronisation, extended headers, grouping / data-length frame flags,
//! the iTunes non-syncsafe v2.4 frame size quirk, `TXXX` ReplayGain values,
//! `COMM`, `APIC`/`PIC` pictures and numeric genre references. Compressed
//! and encrypted frames are skipped.

use std::io::{Cursor, Read, Seek, SeekFrom};

use super::{
    apply_common, id3v1_genre, make_picture, parse_number_pair, parse_year, resolve_id3_genre,
    set_str,
};
use crate::util::{read_up_to, read_vec, utf8_or_latin1};
use crate::{Error, Picture, Tags};

/// Upper bound for a single frame we are willing to load (pictures).
const MAX_FRAME: u64 = 64 << 20;

/// Parsed ID3v2 tag header.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Id3Header {
    pub major: u8,
    pub flags: u8,
    /// Size of the tag body (excluding header and footer).
    pub size: u32,
    /// Total size including header and optional footer.
    pub total: u64,
}

fn syncsafe(b: &[u8]) -> u32 {
    b.iter()
        .fold(0u32, |acc, &v| (acc << 7) | (v & 0x7f) as u32)
}

/// Parses a 10-byte ID3v2 header.
pub(crate) fn parse_header(h: &[u8]) -> Option<Id3Header> {
    if h.len() < 10 || &h[0..3] != b"ID3" || h[3] == 0xff || h[4] == 0xff {
        return None;
    }
    if h[6..10].iter().any(|&b| b & 0x80 != 0) {
        return None;
    }
    let size = syncsafe(&h[6..10]);
    let footer = h[3] >= 4 && h[5] & 0x10 != 0;
    Some(Id3Header {
        major: h[3],
        flags: h[5],
        size,
        total: 10 + size as u64 + if footer { 10 } else { 0 },
    })
}

/// Skips all consecutive ID3v2 tags at the current position. Returns the
/// position after the last tag (equal to the start if there is none).
pub(crate) fn skip_id3v2<R: Read + Seek + ?Sized>(r: &mut R) -> std::io::Result<u64> {
    let mut pos = r.stream_position()?;
    loop {
        let mut h = [0u8; 10];
        let n = read_up_to(r, &mut h)?;
        match (n == 10).then(|| parse_header(&h)).flatten() {
            Some(hdr) => pos += hdr.total,
            None => break,
        }
        r.seek(SeekFrom::Start(pos))?;
    }
    r.seek(SeekFrom::Start(pos))?;
    Ok(pos)
}

/// Reads all consecutive ID3v2 tags at the current position, merging them
/// into `tags`. Leaves the stream positioned after the last tag and returns
/// that position.
pub(crate) fn read_id3v2_tags<R: Read + Seek + ?Sized>(
    r: &mut R,
    tags: &mut Tags,
    mut pictures: Option<&mut Vec<Picture>>,
) -> Result<u64, Error> {
    let mut pos = r.stream_position()?;
    loop {
        let mut h = [0u8; 10];
        let n = read_up_to(r, &mut h)?;
        let hdr = match (n == 10).then(|| parse_header(&h)).flatten() {
            Some(hdr) => hdr,
            None => break,
        };
        // A damaged tag must not prevent reading the audio: errors inside
        // the tag body are ignored.
        let _ = read_tag_body(r, pos, &hdr, tags, pictures.as_deref_mut());
        pos += hdr.total;
        r.seek(SeekFrom::Start(pos))?;
    }
    r.seek(SeekFrom::Start(pos))?;
    Ok(pos)
}

/// Parses a complete in-memory ID3v2 tag (e.g. the WAV `id3 ` chunk).
pub(crate) fn parse_id3v2_bytes(data: &[u8], tags: &mut Tags, pictures: Option<&mut Vec<Picture>>) {
    let mut c = Cursor::new(data);
    let _ = read_id3v2_tags(&mut c, tags, pictures);
}

fn read_tag_body<R: Read + Seek + ?Sized>(
    r: &mut R,
    start: u64,
    h: &Id3Header,
    tags: &mut Tags,
    pictures: Option<&mut Vec<Picture>>,
) -> Result<(), Error> {
    if !(2..=4).contains(&h.major) {
        return Ok(());
    }
    if h.major == 2 && h.flags & 0x40 != 0 {
        // ID3v2.2 compression: no defined scheme, skip the tag.
        return Ok(());
    }
    let unsync = h.flags & 0x80 != 0;
    if unsync && h.major < 4 {
        // Whole-tag unsynchronisation: undo it in memory first.
        let body = read_vec(r, h.size as u64, MAX_FRAME * 2)?;
        let body = resync(&body);
        let len = body.len() as u64;
        let mut c = Cursor::new(body);
        parse_frames(&mut c, h.major, h.flags, 0, len, tags, pictures)
    } else {
        parse_frames(
            r,
            h.major,
            h.flags,
            start + 10,
            start + 10 + h.size as u64,
            tags,
            pictures,
        )
    }
}

/// Removes unsynchronisation (0xFF 0x00 -> 0xFF).
fn resync(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        out.push(data[i]);
        if data[i] == 0xff && i + 1 < data.len() && data[i + 1] == 0 {
            i += 1;
        }
        i += 1;
    }
    out
}

fn valid_id(id: &[u8]) -> bool {
    id.iter()
        .all(|&c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

/// Checks whether a plausible frame header (or padding / end of tag) starts
/// at `pos`.
fn plausible_next<R: Read + Seek + ?Sized>(r: &mut R, pos: u64, end: u64) -> Result<bool, Error> {
    if pos == end {
        return Ok(true);
    }
    if pos > end {
        return Ok(false);
    }
    if end - pos < 4 {
        return Ok(true);
    }
    let save = r.stream_position()?;
    r.seek(SeekFrom::Start(pos))?;
    let mut id = [0u8; 4];
    let n = read_up_to(r, &mut id)?;
    r.seek(SeekFrom::Start(save))?;
    Ok(n == 4 && (id[0] == 0 || valid_id(&id)))
}

fn parse_frames<R: Read + Seek + ?Sized>(
    r: &mut R,
    major: u8,
    tag_flags: u8,
    body_start: u64,
    body_end: u64,
    tags: &mut Tags,
    mut pictures: Option<&mut Vec<Picture>>,
) -> Result<(), Error> {
    let mut pos = body_start;
    r.seek(SeekFrom::Start(pos))?;
    if major >= 3 && tag_flags & 0x40 != 0 {
        let mut b = [0u8; 4];
        if read_up_to(r, &mut b)? < 4 {
            return Ok(());
        }
        pos += if major == 3 {
            4 + u32::from_be_bytes(b) as u64
        } else {
            syncsafe(&b) as u64
        };
    }
    let header_len: u64 = if major == 2 { 6 } else { 10 };
    let mut found = Found::default();
    while pos + header_len <= body_end {
        r.seek(SeekFrom::Start(pos))?;
        let mut fh = [0u8; 10];
        if read_up_to(r, &mut fh[..header_len as usize])? < header_len as usize {
            break;
        }
        let id_len = if major == 2 { 3 } else { 4 };
        let id = &fh[..id_len];
        if id[0] == 0 || !valid_id(id) {
            break;
        }
        let (size, flags) = match major {
            2 => (
                ((fh[3] as u64) << 16) | ((fh[4] as u64) << 8) | fh[5] as u64,
                [0u8, 0u8],
            ),
            3 => (
                u32::from_be_bytes([fh[4], fh[5], fh[6], fh[7]]) as u64,
                [fh[8], fh[9]],
            ),
            _ => {
                let plain = u32::from_be_bytes([fh[4], fh[5], fh[6], fh[7]]) as u64;
                let ss = syncsafe(&fh[4..8]) as u64;
                let not_syncsafe = fh[4..8].iter().any(|&b| b & 0x80 != 0);
                let size = if not_syncsafe
                    || (ss != plain
                        && !plausible_next(r, pos + 10 + ss, body_end)?
                        && plausible_next(r, pos + 10 + plain, body_end)?)
                {
                    plain
                } else {
                    ss
                };
                (size, [fh[8], fh[9]])
            }
        };
        let data_start = pos + header_len;
        if data_start + size > body_end {
            break;
        }
        pos = data_start + size;
        let id_str = std::str::from_utf8(id).unwrap_or("");
        let canon = canonical_id(id_str);
        let is_pic = canon == "APIC" || canon == "PIC";
        if !wanted(canon) || (is_pic && pictures.is_none()) || size == 0 || size > MAX_FRAME {
            continue;
        }
        r.seek(SeekFrom::Start(data_start))?;
        let raw = read_vec(r, size, MAX_FRAME)?;
        let data = match frame_payload(major, tag_flags, flags, raw) {
            Some(d) => d,
            None => continue,
        };
        handle_frame(canon, &data, tags, pictures.as_deref_mut(), &mut found);
    }
    // Year fallbacks (TDRC preferred, then TYER, then original date).
    tags.year = tags.year.or(found.tyer).or(found.other_year);
    Ok(())
}

#[derive(Default)]
struct Found {
    tyer: Option<i32>,
    other_year: Option<i32>,
}

/// Strips frame flag related prefixes, undoes frame unsynchronisation.
/// Returns `None` for compressed or encrypted frames.
fn frame_payload(major: u8, tag_flags: u8, flags: [u8; 2], mut data: Vec<u8>) -> Option<Vec<u8>> {
    match major {
        3 => {
            if flags[1] & 0xc0 != 0 {
                return None;
            }
            if flags[1] & 0x20 != 0 {
                data.drain(..1.min(data.len()));
            }
            Some(data)
        }
        4 => {
            if flags[1] & 0x0c != 0 {
                return None;
            }
            let mut skip = 0;
            if flags[1] & 0x40 != 0 {
                skip += 1;
            }
            if flags[1] & 0x01 != 0 {
                skip += 4;
            }
            if skip > data.len() {
                return None;
            }
            data.drain(..skip);
            if flags[1] & 0x02 != 0 || tag_flags & 0x80 != 0 {
                data = resync(&data);
            }
            Some(data)
        }
        _ => Some(data),
    }
}

fn canonical_id(id: &str) -> &str {
    match id {
        "TT2" => "TIT2",
        "TP1" => "TPE1",
        "TP2" => "TPE2",
        "TAL" => "TALB",
        "TCM" => "TCOM",
        "TCO" => "TCON",
        "TYE" => "TYER",
        "TRK" => "TRCK",
        "TPA" => "TPOS",
        "COM" => "COMM",
        "TXX" => "TXXX",
        "TOR" => "TORY",
        "PIC" => "PIC",
        other => other,
    }
}

fn wanted(id: &str) -> bool {
    matches!(
        id,
        "TIT2"
            | "TPE1"
            | "TPE2"
            | "TALB"
            | "TCOM"
            | "TCON"
            | "TYER"
            | "TDRC"
            | "TDRL"
            | "TORY"
            | "TDOR"
            | "TRCK"
            | "TPOS"
            | "COMM"
            | "TXXX"
            | "APIC"
            | "PIC"
    )
}

/// Decodes a terminated (or final) string in the given encoding, returning
/// it and the remaining bytes after the terminator.
fn split_text(enc: u8, data: &[u8]) -> (String, &[u8]) {
    match enc {
        1 | 2 => {
            let mut i = 0;
            while i + 1 < data.len() {
                if data[i] == 0 && data[i + 1] == 0 {
                    return (decode_utf16(enc, &data[..i]), &data[i + 2..]);
                }
                i += 2;
            }
            (decode_utf16(enc, data), &[])
        }
        _ => match data.iter().position(|&b| b == 0) {
            Some(p) => (decode_8bit(enc, &data[..p]), &data[p + 1..]),
            None => (decode_8bit(enc, data), &[]),
        },
    }
}

fn decode_8bit(enc: u8, data: &[u8]) -> String {
    if enc == 3 {
        String::from_utf8_lossy(data).into_owned()
    } else {
        data.iter().map(|&b| b as char).collect()
    }
}

fn decode_utf16(enc: u8, mut data: &[u8]) -> String {
    let mut big_endian = enc == 2;
    if data.len() >= 2 {
        if data[0] == 0xff && data[1] == 0xfe {
            big_endian = false;
            data = &data[2..];
        } else if data[0] == 0xfe && data[1] == 0xff {
            big_endian = true;
            data = &data[2..];
        }
    }
    let units = data.chunks_exact(2).map(|c| {
        if big_endian {
            u16::from_be_bytes([c[0], c[1]])
        } else {
            u16::from_le_bytes([c[0], c[1]])
        }
    });
    char::decode_utf16(units)
        .map(|c| c.unwrap_or('\u{fffd}'))
        .collect()
}

/// All (NUL-separated) values of a text frame.
fn text_values(data: &[u8]) -> Vec<String> {
    let Some((&enc, mut rest)) = data.split_first() else {
        return Vec::new();
    };
    let mut values = Vec::new();
    while !rest.is_empty() {
        let (s, r) = split_text(enc, rest);
        let t = s
            .trim_matches(|c: char| c == '\0' || c.is_whitespace())
            .to_string();
        if !t.is_empty() {
            values.push(t);
        }
        rest = r;
    }
    values
}

fn handle_frame(
    id: &str,
    data: &[u8],
    tags: &mut Tags,
    pictures: Option<&mut Vec<Picture>>,
    found: &mut Found,
) {
    match id {
        "TIT2" | "TPE1" | "TPE2" | "TALB" | "TCOM" | "TCON" | "TYER" | "TDRC" | "TDRL" | "TORY"
        | "TDOR" | "TRCK" | "TPOS" => {
            let values = text_values(data);
            let Some(first) = values.first() else { return };
            match id {
                "TIT2" => set_str(&mut tags.title, first),
                "TPE1" => set_str(&mut tags.artist, &values.join("; ")),
                "TPE2" => set_str(&mut tags.album_artist, &values.join("; ")),
                "TALB" => set_str(&mut tags.album, first),
                "TCOM" => set_str(&mut tags.composer, &values.join("; ")),
                "TCON" => {
                    let g: Vec<String> = values.iter().map(|v| resolve_id3_genre(v)).collect();
                    set_str(&mut tags.genre, &g.join("; "));
                }
                "TDRC" => {
                    if tags.year.is_none() {
                        tags.year = parse_year(first);
                    }
                }
                "TYER" => found.tyer = found.tyer.or(parse_year(first)),
                "TDRL" | "TORY" | "TDOR" => {
                    found.other_year = found.other_year.or(parse_year(first))
                }
                "TRCK" => {
                    let (n, t) = parse_number_pair(first);
                    if tags.track.is_none() {
                        tags.track = n;
                    }
                    if tags.track_total.is_none() {
                        tags.track_total = t;
                    }
                }
                "TPOS" => {
                    let (n, t) = parse_number_pair(first);
                    if tags.disc.is_none() {
                        tags.disc = n;
                    }
                    if tags.disc_total.is_none() {
                        tags.disc_total = t;
                    }
                }
                _ => {}
            }
        }
        "TXXX" => {
            let Some((&enc, rest)) = data.split_first() else {
                return;
            };
            let (desc, value) = split_text(enc, rest);
            let (value, _) = split_text(enc, value);
            let key = desc.trim().to_ascii_uppercase();
            if key.starts_with("REPLAYGAIN_")
                || matches!(
                    key.as_str(),
                    "ALBUM ARTIST"
                        | "ALBUMARTIST"
                        | "TOTALTRACKS"
                        | "TOTALDISCS"
                        | "TRACKTOTAL"
                        | "DISCTOTAL"
                )
            {
                apply_common(tags, &key, &value);
            }
        }
        "COMM" => {
            let Some((&enc, rest)) = data.split_first() else {
                return;
            };
            if rest.len() < 3 {
                return;
            }
            let (desc, text) = split_text(enc, &rest[3..]);
            if desc.starts_with("iTun") {
                return;
            }
            let (text, _) = split_text(enc, text);
            set_str(&mut tags.comment, &text);
        }
        "APIC" | "PIC" => {
            let Some(pics) = pictures else { return };
            let Some((&enc, rest)) = data.split_first() else {
                return;
            };
            let (mime, rest) = if id == "PIC" {
                if rest.len() < 3 {
                    return;
                }
                (String::from_utf8_lossy(&rest[..3]).into_owned(), &rest[3..])
            } else {
                split_text(0, rest)
            };
            let Some((&kind, rest)) = rest.split_first() else {
                return;
            };
            let (desc, img) = split_text(enc, rest);
            if mime == "-->" || img.is_empty() {
                return; // linked picture or empty
            }
            pics.push(make_picture(&mime, kind, &desc, img.to_vec()));
        }
        _ => {}
    }
}

/// Parses a 128-byte ID3v1 / ID3v1.1 tag.
pub(crate) fn parse_id3v1(d: &[u8]) -> Option<Tags> {
    if d.len() < 128 || &d[0..3] != b"TAG" {
        return None;
    }
    let field = |a: usize, b: usize| -> String {
        let s = &d[a..b];
        let end = s.iter().position(|&c| c == 0).unwrap_or(s.len());
        utf8_or_latin1(&s[..end]).trim().to_string()
    };
    let mut tags = Tags::default();
    set_str(&mut tags.title, &field(3, 33));
    set_str(&mut tags.artist, &field(33, 63));
    set_str(&mut tags.album, &field(63, 93));
    tags.year = parse_year(&field(93, 97));
    if d[125] == 0 && d[126] != 0 {
        tags.track = Some(d[126] as u32);
        set_str(&mut tags.comment, &field(97, 125));
    } else {
        set_str(&mut tags.comment, &field(97, 127));
    }
    if let Some(g) = id3v1_genre(d[127] as usize) {
        tags.genre = Some(g.to_string());
    }
    Some(tags)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn syncsafe_bytes(v: u32) -> [u8; 4] {
        [
            (v >> 21) as u8 & 0x7f,
            (v >> 14) as u8 & 0x7f,
            (v >> 7) as u8 & 0x7f,
            v as u8 & 0x7f,
        ]
    }

    /// Builds an ID3v2.3/2.4 tag from (id, payload) frames.
    pub(crate) fn build_tag(major: u8, frames: &[(&str, Vec<u8>)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (id, data) in frames {
            body.extend_from_slice(id.as_bytes());
            if major == 4 {
                body.extend_from_slice(&syncsafe_bytes(data.len() as u32));
            } else {
                body.extend_from_slice(&(data.len() as u32).to_be_bytes());
            }
            body.extend_from_slice(&[0, 0]);
            body.extend_from_slice(data);
        }
        body.extend_from_slice(&[0u8; 16]); // padding
        let mut tag = b"ID3".to_vec();
        tag.extend_from_slice(&[major, 0, 0]);
        tag.extend_from_slice(&syncsafe_bytes(body.len() as u32));
        tag.extend_from_slice(&body);
        tag
    }

    fn text(enc: u8, s: &str) -> Vec<u8> {
        let mut v = vec![enc];
        match enc {
            1 => {
                v.extend_from_slice(&[0xff, 0xfe]);
                for u in s.encode_utf16() {
                    v.extend_from_slice(&u.to_le_bytes());
                }
            }
            2 => {
                for u in s.encode_utf16() {
                    v.extend_from_slice(&u.to_be_bytes());
                }
            }
            _ => v.extend_from_slice(s.as_bytes()),
        }
        v
    }

    #[test]
    fn v23_and_v24_frames() {
        for major in [3u8, 4] {
            let mut apic = vec![0u8];
            apic.extend_from_slice(b"image/png\0");
            apic.push(3);
            apic.extend_from_slice(b"cover\0");
            apic.extend_from_slice(b"\x89PNGdata");
            let mut txxx = vec![3u8];
            txxx.extend_from_slice(b"REPLAYGAIN_TRACK_GAIN\0-7.25 dB");
            let mut comm = vec![0u8];
            comm.extend_from_slice(b"eng\0Nice song");
            let tag = build_tag(
                major,
                &[
                    ("TIT2", text(1, "Tïtle")),
                    ("TPE1", text(2, "Ärtist")),
                    ("TALB", text(3, "Albüm")),
                    ("TCON", text(0, "(13)")),
                    ("TRCK", text(0, "3/12")),
                    ("TPOS", text(0, "1/2")),
                    (if major == 4 { "TDRC" } else { "TYER" }, text(0, "1999")),
                    ("TXXX", txxx),
                    ("COMM", comm),
                    ("APIC", apic),
                ],
            );
            let mut tags = Tags::default();
            let mut pics = Vec::new();
            parse_id3v2_bytes(&tag, &mut tags, Some(&mut pics));
            assert_eq!(tags.title.as_deref(), Some("Tïtle"));
            assert_eq!(tags.artist.as_deref(), Some("Ärtist"));
            assert_eq!(tags.album.as_deref(), Some("Albüm"));
            assert_eq!(tags.genre.as_deref(), Some("Pop"));
            assert_eq!((tags.track, tags.track_total), (Some(3), Some(12)));
            assert_eq!((tags.disc, tags.disc_total), (Some(1), Some(2)));
            assert_eq!(tags.year, Some(1999));
            assert_eq!(tags.replaygain_track_gain, Some(-7.25));
            assert_eq!(tags.comment.as_deref(), Some("Nice song"));
            assert_eq!(pics.len(), 1);
            assert_eq!(pics[0].mime, "image/png");
            assert_eq!(pics[0].kind, 3);
            assert_eq!(pics[0].description, "cover");
        }
    }

    #[test]
    fn unsynchronised_v23() {
        let tag = build_tag(3, &[("TIT2", vec![0, b'a', 0xff, 0xe0, b'b'])]);
        // Apply unsynchronisation to the body.
        let mut out = tag[..10].to_vec();
        out[5] |= 0x80;
        let mut body = Vec::new();
        for (i, &b) in tag[10..].iter().enumerate() {
            body.push(b);
            if b == 0xff
                && tag
                    .get(10 + i + 1)
                    .map_or(true, |&n| n & 0xe0 == 0xe0 || n == 0)
            {
                body.push(0);
            }
        }
        let s = syncsafe_bytes(body.len() as u32);
        out[6..10].copy_from_slice(&s);
        out.extend_from_slice(&body);
        let mut tags = Tags::default();
        parse_id3v2_bytes(&out, &mut tags, None);
        assert_eq!(tags.title.as_deref(), Some("a\u{ff}\u{e0}b"));
    }

    #[test]
    fn v22_and_extended_header() {
        // ID3v2.2: 3-char IDs, 3-byte sizes, PIC with image format.
        let mut body = Vec::new();
        for (id, data) in [
            (&b"TT2"[..], &b"\0Old title"[..]),
            (b"TCO", b"\0(17)"),
            (b"PIC", b"\0PNG\x03desc\0\x89PNGxx"),
        ] {
            body.extend_from_slice(id);
            body.extend_from_slice(&(data.len() as u32).to_be_bytes()[1..]);
            body.extend_from_slice(data);
        }
        let mut tag = b"ID3\x02\0\0".to_vec();
        tag.extend_from_slice(&syncsafe_bytes(body.len() as u32));
        tag.extend_from_slice(&body);
        let mut tags = Tags::default();
        let mut pics = Vec::new();
        parse_id3v2_bytes(&tag, &mut tags, Some(&mut pics));
        assert_eq!(tags.title.as_deref(), Some("Old title"));
        assert_eq!(tags.genre.as_deref(), Some("Rock"));
        assert_eq!(pics.len(), 1);
        assert_eq!(
            (
                pics[0].mime.as_str(),
                pics[0].kind,
                pics[0].description.as_str()
            ),
            ("image/png", 3, "desc")
        );

        // ID3v2.3 with an extended header (10 bytes after the size field).
        let plain = build_tag(3, &[("TIT2", b"\0Ext".to_vec())]);
        let mut ext = plain[..10].to_vec();
        ext[5] |= 0x40;
        let mut body = 6u32.to_be_bytes().to_vec();
        body.extend_from_slice(&[0u8; 6]);
        body.extend_from_slice(&plain[10..]);
        ext[6..10].copy_from_slice(&syncsafe_bytes(body.len() as u32));
        ext.extend_from_slice(&body);
        let mut tags = Tags::default();
        parse_id3v2_bytes(&ext, &mut tags, None);
        assert_eq!(tags.title.as_deref(), Some("Ext"));
    }

    #[test]
    fn id3v1() {
        let mut d = vec![0u8; 128];
        d[..3].copy_from_slice(b"TAG");
        d[3..8].copy_from_slice(b"Hello");
        d[33..39].copy_from_slice(b"Artist");
        d[93..97].copy_from_slice(b"2001");
        d[126] = 7;
        d[127] = 17;
        let t = parse_id3v1(&d).unwrap();
        assert_eq!(t.title.as_deref(), Some("Hello"));
        assert_eq!(t.year, Some(2001));
        assert_eq!(t.track, Some(7));
        assert_eq!(t.genre.as_deref(), Some("Rock"));
    }

    #[test]
    fn garbage_does_not_panic() {
        let base = build_tag(4, &[("TIT2", text(1, "abc")), ("APIC", vec![1, 2, 3])]);
        let mut seed = 12345u32;
        for _ in 0..2000 {
            let mut d = base.clone();
            for _ in 0..4 {
                seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
                let i = (seed >> 8) as usize % d.len();
                d[i] = (seed >> 16) as u8;
            }
            let cut = (seed as usize >> 3) % (d.len() + 1);
            let mut tags = Tags::default();
            let mut pics = Vec::new();
            parse_id3v2_bytes(&d[..cut], &mut tags, Some(&mut pics));
        }
    }
}
