//! iTunes-style MP4 metadata (`moov/udta/meta/ilst`) reader and the shared
//! ISO BMFF child box iterator.

use super::{apply_common, id3v1_genre, make_picture, parse_year, set_str};
use crate::util::{be16, be32, be64};
use crate::{Picture, Tags};

/// Iterates over the child boxes contained in `data`, yielding
/// `(type, body)`. Stops at the first malformed box.
pub(crate) struct BoxIter<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BoxIter<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        BoxIter { data, pos: 0 }
    }
}

impl<'a> Iterator for BoxIter<'a> {
    type Item = ([u8; 4], &'a [u8]);
    fn next(&mut self) -> Option<Self::Item> {
        let d = self.data;
        let size32 = be32(d, self.pos)? as u64;
        let typ: [u8; 4] = d.get(self.pos + 4..self.pos + 8)?.try_into().ok()?;
        let remaining = (d.len() - self.pos) as u64;
        let (header, size) = match size32 {
            0 => (8u64, remaining),
            1 => (16u64, be64(d, self.pos + 8)?),
            s => (8u64, s),
        };
        if size < header || size > remaining {
            self.pos = d.len();
            return None;
        }
        let body = &d[self.pos + header as usize..self.pos + size as usize];
        self.pos += size as usize;
        Some((typ, body))
    }
}

/// Finds the first child box of the given type.
pub(crate) fn find_box<'a>(data: &'a [u8], typ: &[u8; 4]) -> Option<&'a [u8]> {
    BoxIter::new(data).find(|(t, _)| t == typ).map(|(_, b)| b)
}

/// Gapless information from the `iTunSMPB` freeform tag.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ITunSmpb {
    pub delay: u32,
    pub padding: u32,
    pub samples: u64,
}

fn parse_itunsmpb(s: &str) -> Option<ITunSmpb> {
    let f: Vec<&str> = s.split_whitespace().collect();
    if f.len() < 4 {
        return None;
    }
    Some(ITunSmpb {
        delay: u32::from_str_radix(f[1], 16).ok()?,
        padding: u32::from_str_radix(f[2], 16).ok()?,
        samples: u64::from_str_radix(f[3], 16).ok()?,
    })
}

/// Returns `(type indicator, value)` of every `data` box in an item.
fn data_boxes(item: &[u8]) -> impl Iterator<Item = (u32, &[u8])> {
    BoxIter::new(item).filter(|(t, _)| t == b"data").filter_map(|(_, b)| {
        let typ = be32(b, 0)? & 0x00ff_ffff;
        Some((typ, b.get(8..)?))
    })
}

fn data_text(typ: u32, v: &[u8]) -> String {
    if typ == 2 {
        let units = v.chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]]));
        char::decode_utf16(units).map(|c| c.unwrap_or('\u{fffd}')).collect()
    } else {
        String::from_utf8_lossy(v).into_owned()
    }
}

/// Parses a QuickTime `keys` box (used with `mdta` metadata handlers,
/// e.g. ffmpeg's `use_metadata_tags`): returns the key names, indexed from 1
/// by the `ilst` item types.
pub(crate) fn parse_keys(b: &[u8]) -> Vec<String> {
    let count = be32(b, 4).unwrap_or(0) as usize;
    let mut keys = Vec::new();
    let mut pos = 8usize;
    for _ in 0..count.min(4096) {
        let Some(size) = be32(b, pos).map(|s| s as usize) else { break };
        if size < 8 || pos + size > b.len() {
            break;
        }
        keys.push(String::from_utf8_lossy(&b[pos + 8..pos + size]).into_owned());
        pos += size;
    }
    keys
}

/// Parses the body of an `ilst` box. `keys` maps integer item types to
/// names (QuickTime `mdta` metadata). Returns iTunes gapless info if present.
pub(crate) fn parse_ilst(data: &[u8], keys: &[String], tags: &mut Tags, mut pictures: Option<&mut Vec<Picture>>) -> Option<ITunSmpb> {
    let mut smpb = None;
    for (typ, item) in BoxIter::new(data) {
        let index = u32::from_be_bytes(typ) as usize;
        if index >= 1 && index <= keys.len() {
            if let Some((ty, v)) = data_boxes(item).next() {
                if ty == 1 || ty == 2 {
                    let key = keys[index - 1].as_str();
                    let value = data_text(ty, v);
                    if key.eq_ignore_ascii_case("iTunSMPB") {
                        smpb = parse_itunsmpb(&value);
                    } else {
                        let k = match key.rsplit('.').next().unwrap_or(key) {
                            // Apple's com.apple.quicktime.* names
                            "displayname" | "name" => "TITLE",
                            "creationdate" => "DATE",
                            "author" => "ARTIST",
                            k => k,
                        };
                        apply_common(tags, k, &value);
                    }
                } else if (ty == 13 || ty == 14) && keys[index - 1].ends_with("artwork") {
                    if let Some(p) = pictures.as_deref_mut() {
                        p.push(make_picture(if ty == 13 { "image/jpeg" } else { "image/png" }, 3, "", v.to_vec()));
                    }
                }
            }
            continue;
        }
        match &typ {
            b"----" => {
                let mut name = String::new();
                let mut value = None;
                for (t, b) in BoxIter::new(item) {
                    match &t {
                        b"name" if b.len() >= 4 => name = String::from_utf8_lossy(&b[4..]).into_owned(),
                        b"data" if b.len() >= 8 => {
                            let ty = be32(b, 0).unwrap_or(1) & 0x00ff_ffff;
                            value = Some(data_text(ty, &b[8..]));
                        }
                        _ => {}
                    }
                }
                if let Some(v) = value {
                    if name.eq_ignore_ascii_case("iTunSMPB") {
                        smpb = parse_itunsmpb(&v);
                    } else {
                        apply_common(tags, &name, &v);
                    }
                }
            }
            b"covr" => {
                if let Some(p) = pictures.as_deref_mut() {
                    for (ty, v) in data_boxes(item) {
                        let mime = match ty {
                            13 => "image/jpeg",
                            14 => "image/png",
                            27 => "image/bmp",
                            _ => "",
                        };
                        if !v.is_empty() {
                            p.push(make_picture(mime, 3, "", v.to_vec()));
                        }
                    }
                }
            }
            b"trkn" | b"disk" => {
                if let Some((_, v)) = data_boxes(item).next() {
                    let n = be16(v, 2).filter(|&n| n > 0).map(|n| n as u32);
                    let t = be16(v, 4).filter(|&n| n > 0).map(|n| n as u32);
                    let (nf, tf) = if &typ == b"trkn" {
                        (&mut tags.track, &mut tags.track_total)
                    } else {
                        (&mut tags.disc, &mut tags.disc_total)
                    };
                    if nf.is_none() {
                        *nf = n;
                    }
                    if tf.is_none() {
                        *tf = t;
                    }
                }
            }
            b"gnre" => {
                if let Some((_, v)) = data_boxes(item).next() {
                    if let Some(g) = be16(v, 0).and_then(|i| (i as usize).checked_sub(1)).and_then(id3v1_genre) {
                        set_str(&mut tags.genre, g);
                    }
                }
            }
            _ => {
                let Some((ty, v)) = data_boxes(item).next() else { continue };
                if ty != 1 && ty != 2 {
                    continue;
                }
                let s = data_text(ty, v);
                match &typ {
                    b"\xa9nam" => set_str(&mut tags.title, &s),
                    b"\xa9ART" => set_str(&mut tags.artist, &s),
                    b"aART" => set_str(&mut tags.album_artist, &s),
                    b"\xa9alb" => set_str(&mut tags.album, &s),
                    b"\xa9wrt" => set_str(&mut tags.composer, &s),
                    b"\xa9gen" => set_str(&mut tags.genre, &s),
                    b"\xa9day" => {
                        if tags.year.is_none() {
                            tags.year = parse_year(&s);
                        }
                    }
                    b"\xa9cmt" => set_str(&mut tags.comment, &s),
                    _ => {}
                }
            }
        }
    }
    smpb
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn mkbox(typ: &[u8; 4], body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(typ);
        v.extend_from_slice(body);
        v
    }

    pub(crate) fn data_box(ty: u32, value: &[u8]) -> Vec<u8> {
        let mut b = ty.to_be_bytes().to_vec();
        b.extend_from_slice(&[0, 0, 0, 0]);
        b.extend_from_slice(value);
        mkbox(b"data", &b)
    }

    #[test]
    fn ilst_items() {
        let mut ilst = Vec::new();
        ilst.extend(mkbox(b"\xa9nam", &data_box(1, b"Name")));
        ilst.extend(mkbox(b"\xa9ART", &data_box(1, b"Artist")));
        ilst.extend(mkbox(b"trkn", &data_box(0, &[0, 0, 0, 5, 0, 11, 0, 0])));
        ilst.extend(mkbox(b"disk", &data_box(0, &[0, 0, 0, 1, 0, 2])));
        ilst.extend(mkbox(b"gnre", &data_box(0, &[0, 18])));
        ilst.extend(mkbox(b"covr", &data_box(14, b"\x89PNG..")));
        let mut free = Vec::new();
        free.extend(mkbox(b"mean", b"\0\0\0\0com.apple.iTunes"));
        free.extend(mkbox(b"name", b"\0\0\0\0replaygain_track_gain"));
        free.extend(data_box(1, b"-4.00 dB"));
        ilst.extend(mkbox(b"----", &free));
        let mut free = Vec::new();
        free.extend(mkbox(b"mean", b"\0\0\0\0com.apple.iTunes"));
        free.extend(mkbox(b"name", b"\0\0\0\0iTunSMPB"));
        free.extend(data_box(1, b" 00000000 00000840 000001CA 00000000000F34B6 00000000"));
        ilst.extend(mkbox(b"----", &free));
        let mut tags = Tags::default();
        let mut pics = Vec::new();
        let smpb = parse_ilst(&ilst, &[], &mut tags, Some(&mut pics));
        assert_eq!(tags.title.as_deref(), Some("Name"));
        assert_eq!(tags.artist.as_deref(), Some("Artist"));
        assert_eq!((tags.track, tags.track_total), (Some(5), Some(11)));
        assert_eq!((tags.disc, tags.disc_total), (Some(1), Some(2)));
        assert_eq!(tags.genre.as_deref(), Some("Rock"));
        assert_eq!(tags.replaygain_track_gain, Some(-4.0));
        assert_eq!(pics[0].mime, "image/png");
        assert_eq!(smpb, Some(ITunSmpb { delay: 0x840, padding: 0x1ca, samples: 0xf34b6 }));
    }
}
