//! RIFF `LIST`/`INFO` chunk reader (WAV) and AIFF text chunks.

use super::{apply_common, set_str};
use crate::util::{le32, utf8_or_latin1};
use crate::Tags;

/// Parses the body of a `LIST` chunk (starting with the list type). Only
/// `INFO` lists are interpreted.
pub(crate) fn parse_list_info(data: &[u8], tags: &mut Tags) {
    if data.len() < 4 || &data[0..4] != b"INFO" {
        return;
    }
    let mut pos = 4usize;
    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let Some(size) = le32(data, pos + 4) else {
            break;
        };
        let size = size as usize;
        let start = pos + 8;
        let end = start.saturating_add(size).min(data.len());
        let value = utf8_or_latin1(&data[start..end]);
        let key = match id {
            b"INAM" => "TITLE",
            b"IART" => "ARTIST",
            b"IPRD" => "ALBUM",
            b"ICMT" => "COMMENT",
            b"IGNR" => "GENRE",
            b"ICRD" => "DATE",
            b"ITRK" | b"IPRT" | b"TRCK" => "TRACKNUMBER",
            b"IMUS" | b"ICOM" => "COMPOSER",
            b"IAAR" => "ALBUMARTIST",
            _ => "",
        };
        if !key.is_empty() {
            apply_common(tags, key, &value);
        }
        pos = start.saturating_add(size).saturating_add(size & 1);
    }
}

/// Applies an AIFF text chunk (`NAME`, `AUTH`, `ANNO`).
pub(crate) fn apply_aiff_text(id: &[u8; 4], data: &[u8], tags: &mut Tags) {
    let value = utf8_or_latin1(data);
    match id {
        b"NAME" => set_str(&mut tags.title, &value),
        b"AUTH" => set_str(&mut tags.artist, &value),
        b"ANNO" => set_str(&mut tags.comment, &value),
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_list() {
        let mut d = b"INFO".to_vec();
        for (id, v) in [
            (b"INAM", &b"Song\0"[..]),
            (b"IART", b"Me\0\0"),
            (b"ITRK", b"7\0"),
        ] {
            d.extend_from_slice(id);
            d.extend_from_slice(&(v.len() as u32).to_le_bytes());
            d.extend_from_slice(v);
            if v.len() % 2 == 1 {
                d.push(0);
            }
        }
        let mut t = Tags::default();
        parse_list_info(&d, &mut t);
        assert_eq!(t.title.as_deref(), Some("Song"));
        assert_eq!(t.artist.as_deref(), Some("Me"));
        assert_eq!(t.track, Some(7));
    }
}
