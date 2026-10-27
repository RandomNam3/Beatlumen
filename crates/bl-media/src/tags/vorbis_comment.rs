//! Vorbis comment reader (Ogg Vorbis/Opus/FLAC, Matroska) including
//! `METADATA_BLOCK_PICTURE` (base64 FLAC picture blocks), legacy
//! `COVERART`, Opus R128 gains, and the FLAC `PICTURE` block parser.

use super::{apply_common, base64_decode, make_picture};
use crate::util::ByteCursor;
use crate::{Picture, Tags};

/// Parses a comment header body (without the codec specific packet prefix
/// such as `\x03vorbis` or `OpusTags`).
pub(crate) fn parse_vorbis_comment(data: &[u8], tags: &mut Tags, mut pictures: Option<&mut Vec<Picture>>) {
    let mut c = ByteCursor::new(data);
    let Ok(vendor_len) = c.le32() else { return };
    if c.skip(vendor_len as usize).is_err() {
        return;
    }
    let Ok(count) = c.le32() else { return };
    let mut r128_track: Option<f32> = None;
    let mut r128_album: Option<f32> = None;
    for _ in 0..count {
        let Ok(len) = c.le32() else { break };
        let Ok(entry) = c.bytes(len as usize) else { break };
        let Some(eq) = entry.iter().position(|&b| b == b'=') else { continue };
        let key = String::from_utf8_lossy(&entry[..eq]).to_ascii_uppercase();
        let value = &entry[eq + 1..];
        match key.as_str() {
            "METADATA_BLOCK_PICTURE" => {
                if let Some(p) = pictures.as_deref_mut() {
                    if let Some(pic) = base64_decode(value).and_then(|raw| parse_flac_picture(&raw)) {
                        p.push(pic);
                    }
                }
            }
            "COVERART" => {
                if let Some(p) = pictures.as_deref_mut() {
                    if let Some(raw) = base64_decode(value) {
                        if !raw.is_empty() {
                            p.push(make_picture("", 3, "", raw));
                        }
                    }
                }
            }
            "R128_TRACK_GAIN" | "R128_ALBUM_GAIN" => {
                // Q7.8 dB relative to -23 LUFS; ReplayGain uses -18 LUFS.
                if let Ok(q) = String::from_utf8_lossy(value).trim().parse::<i32>() {
                    let db = q as f32 / 256.0 + 5.0;
                    if key == "R128_TRACK_GAIN" {
                        r128_track = Some(db);
                    } else {
                        r128_album = Some(db);
                    }
                }
            }
            _ => {
                let v = String::from_utf8_lossy(value);
                apply_common(tags, &key, &v);
            }
        }
    }
    if tags.replaygain_track_gain.is_none() {
        tags.replaygain_track_gain = r128_track;
    }
    if tags.replaygain_album_gain.is_none() {
        tags.replaygain_album_gain = r128_album;
    }
}

/// Parses a FLAC `PICTURE` metadata block body.
pub(crate) fn parse_flac_picture(data: &[u8]) -> Option<Picture> {
    let mut c = ByteCursor::new(data);
    let kind = c.be32().ok()?;
    let mime_len = c.be32().ok()? as usize;
    let mime = String::from_utf8_lossy(c.bytes(mime_len).ok()?).into_owned();
    let desc_len = c.be32().ok()? as usize;
    let desc = String::from_utf8_lossy(c.bytes(desc_len).ok()?).into_owned();
    c.skip(16).ok()?; // width, height, depth, colours
    let len = c.be32().ok()? as usize;
    let img = c.bytes(len).ok()?;
    if mime == "-->" {
        return None;
    }
    Some(make_picture(&mime, kind.min(255) as u8, &desc, img.to_vec()))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn build_comment(entries: &[&str]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&3u32.to_le_bytes());
        v.extend_from_slice(b"oc!");
        v.extend_from_slice(&(entries.len() as u32).to_le_bytes());
        for e in entries {
            v.extend_from_slice(&(e.len() as u32).to_le_bytes());
            v.extend_from_slice(e.as_bytes());
        }
        v
    }

    fn base64(data: &[u8]) -> String {
        const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = String::new();
        for ch in data.chunks(3) {
            let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
            let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
            for i in 0..4 {
                if i <= ch.len() {
                    s.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
                } else {
                    s.push('=');
                }
            }
        }
        s
    }

    #[test]
    fn comments_and_picture() {
        let mut pic = Vec::new();
        pic.extend_from_slice(&3u32.to_be_bytes());
        pic.extend_from_slice(&9u32.to_be_bytes());
        pic.extend_from_slice(b"image/png");
        pic.extend_from_slice(&5u32.to_be_bytes());
        pic.extend_from_slice(b"front");
        pic.extend_from_slice(&[0u8; 16]);
        pic.extend_from_slice(&4u32.to_be_bytes());
        pic.extend_from_slice(b"\x89PNG");
        let mbp = format!("METADATA_BLOCK_PICTURE={}", base64(&pic));
        let data = build_comment(&[
            "TITLE=Tune",
            "artist=Someone",
            "TRACKNUMBER=2",
            "TRACKTOTAL=10",
            "DATE=2010-01-01",
            "R128_TRACK_GAIN=-512",
            &mbp,
        ]);
        let mut tags = Tags::default();
        let mut pics = Vec::new();
        parse_vorbis_comment(&data, &mut tags, Some(&mut pics));
        assert_eq!(tags.title.as_deref(), Some("Tune"));
        assert_eq!(tags.artist.as_deref(), Some("Someone"));
        assert_eq!((tags.track, tags.track_total), (Some(2), Some(10)));
        assert_eq!(tags.year, Some(2010));
        assert_eq!(tags.replaygain_track_gain, Some(3.0));
        assert_eq!(pics.len(), 1);
        assert_eq!(pics[0].description, "front");
        assert_eq!(pics[0].data, b"\x89PNG");
    }
}
