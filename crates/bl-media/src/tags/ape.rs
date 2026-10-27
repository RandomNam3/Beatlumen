//! APEv1/APEv2 tag reader (tags at the end of MP3 / other files).

use std::io::{Read, Seek, SeekFrom};

use super::{apply_common, make_picture};
use crate::util::{le32, read_up_to, read_vec, utf8_or_latin1};
use crate::{Error, Picture, Tags};

/// Location of an APE tag found at the end of a stream.
pub(crate) struct ApeTag {
    /// Offset of the first byte of the tag (header if present, else items).
    pub start: u64,
}

/// Looks for an APE tag whose footer ends at `end`. Parses it into `tags` /
/// `pictures` and returns its start offset.
pub(crate) fn read_ape_tag<R: Read + Seek + ?Sized>(
    r: &mut R,
    end: u64,
    tags: &mut Tags,
    pictures: Option<&mut Vec<Picture>>,
) -> Result<Option<ApeTag>, Error> {
    if end < 32 {
        return Ok(None);
    }
    r.seek(SeekFrom::Start(end - 32))?;
    let mut f = [0u8; 32];
    if read_up_to(r, &mut f)? < 32 || &f[0..8] != b"APETAGEX" {
        return Ok(None);
    }
    let version = le32(&f, 8).unwrap_or(0);
    let size = le32(&f, 12).unwrap_or(0) as u64; // items + footer
    let count = le32(&f, 16).unwrap_or(0);
    let flags = le32(&f, 20).unwrap_or(0);
    if size < 32 || size > end {
        return Ok(None);
    }
    let items_start = end - size;
    let has_header = flags & (1 << 31) != 0;
    let start = if has_header && items_start >= 32 { items_start - 32 } else { items_start };
    r.seek(SeekFrom::Start(items_start))?;
    let items = read_vec(r, size - 32, 64 << 20)?;
    parse_items(&items, count, version, tags, pictures);
    Ok(Some(ApeTag { start }))
}

fn parse_items(d: &[u8], count: u32, version: u32, tags: &mut Tags, mut pictures: Option<&mut Vec<Picture>>) {
    let mut pos = 0usize;
    for _ in 0..count.min(4096) {
        let (Some(len), Some(flags)) = (le32(d, pos), le32(d, pos + 4)) else { break };
        let len = len as usize;
        pos += 8;
        let Some(key_end) = d[pos.min(d.len())..].iter().position(|&b| b == 0).map(|p| pos + p) else { break };
        let key = String::from_utf8_lossy(&d[pos..key_end]).into_owned();
        pos = key_end + 1;
        if pos + len > d.len() {
            break;
        }
        let value = &d[pos..pos + len];
        pos += len;
        let kind = (flags >> 1) & 3;
        let lkey = key.to_ascii_lowercase();
        if kind == 1 {
            // Binary item: cover art is "<filename>\0<image data>".
            if lkey.starts_with("cover art") {
                if let Some(p) = pictures.as_deref_mut() {
                    let split = value.iter().position(|&b| b == 0).map_or(0, |i| i + 1);
                    let desc = String::from_utf8_lossy(&value[..split.saturating_sub(1)]).into_owned();
                    let pic_kind = if lkey.contains("front") {
                        3
                    } else if lkey.contains("back") {
                        4
                    } else {
                        0
                    };
                    if split < value.len() {
                        p.push(make_picture("", pic_kind, &desc, value[split..].to_vec()));
                    }
                }
            }
            continue;
        }
        if kind != 0 {
            continue;
        }
        let text = if version >= 2000 { String::from_utf8_lossy(value).into_owned() } else { utf8_or_latin1(value) };
        // Multiple values are NUL separated; use the first.
        let first = text.split('\0').next().unwrap_or("");
        let k = match lkey.as_str() {
            "year" => "DATE",
            "track" => "TRACKNUMBER",
            "disc" | "disk" => "DISCNUMBER",
            "album artist" | "albumartist" => "ALBUMARTIST",
            _ => key.as_str(),
        };
        apply_common(tags, k, first);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub(crate) fn build_ape(items: &[(&str, &[u8], u32)]) -> Vec<u8> {
        let mut body = Vec::new();
        for (k, v, flags) in items {
            body.extend_from_slice(&(v.len() as u32).to_le_bytes());
            body.extend_from_slice(&flags.to_le_bytes());
            body.extend_from_slice(k.as_bytes());
            body.push(0);
            body.extend_from_slice(v);
        }
        let mut out = body.clone();
        out.extend_from_slice(b"APETAGEX");
        out.extend_from_slice(&2000u32.to_le_bytes());
        out.extend_from_slice(&((body.len() + 32) as u32).to_le_bytes());
        out.extend_from_slice(&(items.len() as u32).to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&[0u8; 8]);
        out
    }

    #[test]
    fn ape_items() {
        let mut data = b"audio data".to_vec();
        data.extend_from_slice(&build_ape(&[
            ("Title", b"Song", 0),
            ("Artist", b"Band", 0),
            ("Track", b"4/9", 0),
            ("REPLAYGAIN_ALBUM_GAIN", b"-3.5 dB", 0),
            ("Cover Art (Front)", b"c.jpg\0\xff\xd8\xff\xe0", 2),
        ]));
        let mut c = std::io::Cursor::new(&data);
        let mut tags = Tags::default();
        let mut pics = Vec::new();
        let t = read_ape_tag(&mut c, data.len() as u64, &mut tags, Some(&mut pics)).unwrap().unwrap();
        assert_eq!(t.start, 10);
        assert_eq!(tags.title.as_deref(), Some("Song"));
        assert_eq!(tags.artist.as_deref(), Some("Band"));
        assert_eq!((tags.track, tags.track_total), (Some(4), Some(9)));
        assert_eq!(tags.replaygain_album_gain, Some(-3.5));
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].mime, "image/jpeg");
        assert_eq!(pics[0].kind, 3);
    }
}
