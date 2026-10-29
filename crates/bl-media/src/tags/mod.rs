//! Tag readers (ID3v1/v2, APEv2, Vorbis comments / FLAC pictures, MP4
//! `ilst`, RIFF `INFO`) and helpers that normalise their values into
//! [`Tags`](crate::Tags).

pub(crate) mod ape;
pub(crate) mod id3;
pub(crate) mod mp4;
pub(crate) mod riff;
pub(crate) mod vorbis_comment;

use std::io::{Read, Seek, SeekFrom};

use crate::util::read_up_to;
use crate::{Error, Picture, Tags};

/// Tags found at the end of a file (APEv2 and ID3v1).
#[derive(Default)]
pub(crate) struct EndTags {
    pub ape: Tags,
    pub id3v1: Tags,
    /// Offset where the audio data ends (start of the first trailing tag).
    pub audio_end: u64,
}

/// Reads an ID3v1 tag and/or an APE tag at the end of a stream of length
/// `file_len`. Errors inside the tags are ignored.
pub(crate) fn read_end_tags<R: Read + Seek + ?Sized>(
    r: &mut R,
    file_len: u64,
    pictures: Option<&mut Vec<Picture>>,
) -> Result<EndTags, Error> {
    let mut et = EndTags {
        audio_end: file_len,
        ..Default::default()
    };
    if file_len >= 128 {
        r.seek(SeekFrom::Start(file_len - 128))?;
        let mut b = [0u8; 128];
        if read_up_to(r, &mut b)? == 128 {
            if let Some(t) = id3::parse_id3v1(&b) {
                et.id3v1 = t;
                et.audio_end = file_len - 128;
            }
        }
    }
    if let Ok(Some(ape)) = ape::read_ape_tag(r, et.audio_end, &mut et.ape, pictures) {
        et.audio_end = ape.start;
    }
    Ok(et)
}

/// Sets an optional string field if it is still unset and `value` is not
/// empty after trimming.
pub(crate) fn set_str(field: &mut Option<String>, value: &str) {
    if field.is_none() {
        let v = value.trim_matches(|c: char| c == '\0' || c.is_whitespace());
        if !v.is_empty() {
            *field = Some(v.to_string());
        }
    }
}

/// Longest song text kept (bytes).
pub(crate) const MAX_LYRICS: usize = 256 * 1024;

/// Sets the song text; synced text replaces unsynced text found earlier.
pub(crate) fn set_lyrics(tags: &mut Tags, value: &str) {
    let v = value.trim_matches(|c: char| c == '\0' || c.is_whitespace());
    if v.is_empty() || v.len() > MAX_LYRICS {
        return;
    }
    let synced = |s: &str| s.lines().any(|l| is_lrc_line(l.trim_start()));
    match &tags.lyrics {
        Some(old) if synced(old) || !synced(v) => {}
        _ => tags.lyrics = Some(v.replace("\r\n", "\n")),
    }
}

/// Whether a line starts with an LRC time tag ("[01:23.45]").
fn is_lrc_line(l: &str) -> bool {
    let b = l.as_bytes();
    b.len() >= 7
        && b[0] == b'['
        && b[1].is_ascii_digit()
        && l[1..].find(':').is_some_and(|c| (1..=4).contains(&c))
        && l.find(']').is_some()
}

fn set_opt<T>(field: &mut Option<T>, value: Option<T>) {
    if field.is_none() {
        *field = value;
    }
}

/// Parses "3", "03/12", "3 of 12" into (number, total).
pub(crate) fn parse_number_pair(s: &str) -> (Option<u32>, Option<u32>) {
    let s = s.trim();
    let (a, b) = match s.find(['/', '\\']) {
        Some(p) => (&s[..p], Some(&s[p + 1..])),
        None => match s.to_ascii_lowercase().find(" of ") {
            Some(p) => (&s[..p], Some(&s[p + 4..])),
            None => (s, None),
        },
    };
    (parse_leading_uint(a), b.and_then(parse_leading_uint))
}

/// Parses the leading decimal integer of a string ("07abc" -> 7).
pub(crate) fn parse_leading_uint(s: &str) -> Option<u32> {
    let s = s.trim();
    let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        return None;
    }
    digits
        .parse::<u64>()
        .ok()
        .map(|v| v.min(u32::MAX as u64) as u32)
}

/// Extracts a year from strings like "2004", "2004-05-12", "12/05/2004".
pub(crate) fn parse_year(s: &str) -> Option<i32> {
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            if i - start == 4 {
                return s[start..i].parse().ok();
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Parses ReplayGain-style values ("-6.54 dB", "+1.2", "0.988").
pub(crate) fn parse_float(s: &str) -> Option<f32> {
    let s = s.trim();
    let end = s
        .char_indices()
        .find(|&(i, c)| {
            !(c.is_ascii_digit() || c == '.' || c == ',' || ((c == '-' || c == '+') && i == 0))
        })
        .map_or(s.len(), |(i, _)| i);
    let num = s[..end].replace(',', ".");
    let v: f32 = num.parse().ok()?;
    if v.is_finite() {
        Some(v)
    } else {
        None
    }
}

/// Normalises a tag key: upper case, spaces/dashes become underscores.
fn norm_key(key: &str) -> String {
    key.trim()
        .chars()
        .map(|c| {
            if c == ' ' || c == '-' {
                '_'
            } else {
                c.to_ascii_uppercase()
            }
        })
        .collect()
}

/// Applies a generic key/value pair (Vorbis comment, APE, Matroska,
/// iTunes freeform and similar naming schemes). Returns true if the key was
/// recognised. Existing values are never overwritten.
pub(crate) fn apply_common(tags: &mut Tags, key: &str, value: &str) -> bool {
    let k = norm_key(key);
    match k.as_str() {
        "TITLE" => set_str(&mut tags.title, value),
        "ARTIST" => set_str(&mut tags.artist, value),
        "ALBUM" => set_str(&mut tags.album, value),
        "ALBUMARTIST" | "ALBUM_ARTIST" => set_str(&mut tags.album_artist, value),
        "COMPOSER" => set_str(&mut tags.composer, value),
        "GENRE" => {
            let g = resolve_id3_genre(value);
            set_str(&mut tags.genre, &g)
        }
        "DATE" | "YEAR" | "DATE_RELEASED" | "DATE_RECORDED" | "ORIGINALDATE" | "ORIGINALYEAR"
        | "RELEASEDATE" => set_opt(&mut tags.year, parse_year(value)),
        "TRACKNUMBER" | "TRACK" | "TRACKNUM" => {
            let (n, t) = parse_number_pair(value);
            set_opt(&mut tags.track, n);
            set_opt(&mut tags.track_total, t);
        }
        "TRACKTOTAL" | "TOTALTRACKS" | "TRACK_TOTAL" | "TOTAL_TRACKS" => {
            set_opt(&mut tags.track_total, parse_leading_uint(value))
        }
        "DISCNUMBER" | "DISC" | "DISK" | "DISKNUMBER" => {
            let (n, t) = parse_number_pair(value);
            set_opt(&mut tags.disc, n);
            set_opt(&mut tags.disc_total, t);
        }
        "DISCTOTAL" | "TOTALDISCS" | "DISC_TOTAL" | "TOTAL_DISCS" | "DISKTOTAL" => {
            set_opt(&mut tags.disc_total, parse_leading_uint(value))
        }
        "COMMENT" | "DESCRIPTION" => set_str(&mut tags.comment, value),
        "LYRICS" | "UNSYNCEDLYRICS" | "UNSYNCED_LYRICS" | "SYNCEDLYRICS" => set_lyrics(tags, value),
        "REPLAYGAIN_TRACK_GAIN" => set_opt(&mut tags.replaygain_track_gain, parse_float(value)),
        "REPLAYGAIN_TRACK_PEAK" => set_opt(&mut tags.replaygain_track_peak, parse_float(value)),
        "REPLAYGAIN_ALBUM_GAIN" => set_opt(&mut tags.replaygain_album_gain, parse_float(value)),
        "REPLAYGAIN_ALBUM_PEAK" => set_opt(&mut tags.replaygain_album_peak, parse_float(value)),
        _ => return false,
    }
    true
}

/// Fills unset fields of `dst` from `src`.
pub(crate) fn merge_missing(dst: &mut Tags, src: &Tags) {
    macro_rules! m {
        ($($f:ident),*) => { $( if dst.$f.is_none() { dst.$f = src.$f.clone(); } )* };
    }
    m!(
        title,
        artist,
        album,
        album_artist,
        composer,
        genre,
        year,
        track,
        track_total,
        disc,
        disc_total,
        comment,
        lyrics,
        replaygain_track_gain,
        replaygain_track_peak,
        replaygain_album_gain,
        replaygain_album_peak
    );
}

/// Guesses an image MIME type from magic bytes.
pub(crate) fn guess_image_mime(data: &[u8]) -> &'static str {
    if data.starts_with(&[0xff, 0xd8, 0xff]) {
        "image/jpeg"
    } else if data.starts_with(b"\x89PNG") {
        "image/png"
    } else if data.starts_with(b"GIF8") {
        "image/gif"
    } else if data.starts_with(b"BM") {
        "image/bmp"
    } else if data.len() >= 12 && &data[0..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

/// Normalises picture MIME types ("JPG", "image/jpg", "") to `image/...`.
pub(crate) fn normalise_mime(mime: &str, data: &[u8]) -> String {
    let m = mime.trim().to_ascii_lowercase();
    match m.as_str() {
        "" | "image/" | "image" => guess_image_mime(data).to_string(),
        "jpg" | "jpeg" | "image/jpg" => "image/jpeg".into(),
        "png" => "image/png".into(),
        "gif" => "image/gif".into(),
        "bmp" => "image/bmp".into(),
        _ if !m.contains('/') => guess_image_mime(data).to_string(),
        _ => m,
    }
}

/// Builds a picture record.
pub(crate) fn make_picture(mime: &str, kind: u8, description: &str, data: Vec<u8>) -> Picture {
    let mime = normalise_mime(mime, &data);
    Picture {
        mime,
        kind,
        description: description.to_string(),
        data,
    }
}

/// The ID3v1 genre list including the Winamp extensions (0..=191).
pub(crate) const ID3V1_GENRES: [&str; 192] = [
    "Blues",
    "Classic Rock",
    "Country",
    "Dance",
    "Disco",
    "Funk",
    "Grunge",
    "Hip-Hop",
    "Jazz",
    "Metal",
    "New Age",
    "Oldies",
    "Other",
    "Pop",
    "R&B",
    "Rap",
    "Reggae",
    "Rock",
    "Techno",
    "Industrial",
    "Alternative",
    "Ska",
    "Death Metal",
    "Pranks",
    "Soundtrack",
    "Euro-Techno",
    "Ambient",
    "Trip-Hop",
    "Vocal",
    "Jazz+Funk",
    "Fusion",
    "Trance",
    "Classical",
    "Instrumental",
    "Acid",
    "House",
    "Game",
    "Sound Clip",
    "Gospel",
    "Noise",
    "AlternRock",
    "Bass",
    "Soul",
    "Punk",
    "Space",
    "Meditative",
    "Instrumental Pop",
    "Instrumental Rock",
    "Ethnic",
    "Gothic",
    "Darkwave",
    "Techno-Industrial",
    "Electronic",
    "Pop-Folk",
    "Eurodance",
    "Dream",
    "Southern Rock",
    "Comedy",
    "Cult",
    "Gangsta",
    "Top 40",
    "Christian Rap",
    "Pop/Funk",
    "Jungle",
    "Native American",
    "Cabaret",
    "New Wave",
    "Psychedelic",
    "Rave",
    "Showtunes",
    "Trailer",
    "Lo-Fi",
    "Tribal",
    "Acid Punk",
    "Acid Jazz",
    "Polka",
    "Retro",
    "Musical",
    "Rock & Roll",
    "Hard Rock",
    "Folk",
    "Folk-Rock",
    "National Folk",
    "Swing",
    "Fast Fusion",
    "Bebop",
    "Latin",
    "Revival",
    "Celtic",
    "Bluegrass",
    "Avantgarde",
    "Gothic Rock",
    "Progressive Rock",
    "Psychedelic Rock",
    "Symphonic Rock",
    "Slow Rock",
    "Big Band",
    "Chorus",
    "Easy Listening",
    "Acoustic",
    "Humour",
    "Speech",
    "Chanson",
    "Opera",
    "Chamber Music",
    "Sonata",
    "Symphony",
    "Booty Bass",
    "Primus",
    "Porn Groove",
    "Satire",
    "Slow Jam",
    "Club",
    "Tango",
    "Samba",
    "Folklore",
    "Ballad",
    "Power Ballad",
    "Rhythmic Soul",
    "Freestyle",
    "Duet",
    "Punk Rock",
    "Drum Solo",
    "A capella",
    "Euro-House",
    "Dance Hall",
    "Goa",
    "Drum & Bass",
    "Club-House",
    "Hardcore Techno",
    "Terror",
    "Indie",
    "BritPop",
    "Negerpunk",
    "Polsk Punk",
    "Beat",
    "Christian Gangsta Rap",
    "Heavy Metal",
    "Black Metal",
    "Crossover",
    "Contemporary Christian",
    "Christian Rock",
    "Merengue",
    "Salsa",
    "Thrash Metal",
    "Anime",
    "Jpop",
    "Synthpop",
    "Abstract",
    "Art Rock",
    "Baroque",
    "Bhangra",
    "Big Beat",
    "Breakbeat",
    "Chillout",
    "Downtempo",
    "Dub",
    "EBM",
    "Eclectic",
    "Electro",
    "Electroclash",
    "Emo",
    "Experimental",
    "Garage",
    "Global",
    "IDM",
    "Illbient",
    "Industro-Goth",
    "Jam Band",
    "Krautrock",
    "Leftfield",
    "Lounge",
    "Math Rock",
    "New Romantic",
    "Nu-Breakz",
    "Post-Punk",
    "Post-Rock",
    "Psytrance",
    "Shoegaze",
    "Space Rock",
    "Trop Rock",
    "World Music",
    "Neoclassical",
    "Audiobook",
    "Audio Theatre",
    "Neue Deutsche Welle",
    "Podcast",
    "Indie Rock",
    "G-Funk",
    "Dubstep",
    "Garage Rock",
    "Psybient",
];

pub(crate) fn id3v1_genre(idx: usize) -> Option<&'static str> {
    ID3V1_GENRES.get(idx).copied()
}

/// Resolves ID3-style genre references: "(13)" -> "Pop", "(13)Britpop" ->
/// "Britpop", "13" -> "Pop", "(RX)" -> "Remix", "(CR)" -> "Cover". Other
/// strings are returned unchanged.
pub(crate) fn resolve_id3_genre(s: &str) -> String {
    let s = s.trim();
    if let Some(rest) = s.strip_prefix('(') {
        if let Some(close) = rest.find(')') {
            let inner = &rest[..close];
            let after = rest[close + 1..].trim();
            if !after.is_empty() {
                // Refinement text wins ("(4)Eurodisco"); "((" escapes a
                // literal parenthesis.
                return if let Some(lit) = after.strip_prefix('(') {
                    format!("({lit}")
                } else {
                    after.to_string()
                };
            }
            if inner == "RX" {
                return "Remix".into();
            }
            if inner == "CR" {
                return "Cover".into();
            }
            if let Ok(n) = inner.parse::<usize>() {
                if let Some(g) = id3v1_genre(n) {
                    return g.to_string();
                }
            }
            return s.to_string();
        }
    }
    if !s.is_empty() && s.len() <= 3 && s.bytes().all(|b| b.is_ascii_digit()) {
        if let Some(g) = s.parse::<usize>().ok().and_then(id3v1_genre) {
            return g.to_string();
        }
    }
    s.to_string()
}

/// Decodes standard (RFC 4648) base64, ignoring whitespace and invalid
/// trailing data. Returns `None` on invalid characters.
pub(crate) fn base64_decode(s: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let mut acc = 0u32;
    let mut bits = 0u32;
    for &c in s {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\r' | b'\n' | b'\t' => continue,
            _ => return None,
        } as u32;
        acc = (acc << 6) | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genres() {
        assert_eq!(resolve_id3_genre("(13)"), "Pop");
        assert_eq!(resolve_id3_genre("(4)Eurodisco"), "Eurodisco");
        assert_eq!(resolve_id3_genre("17"), "Rock");
        assert_eq!(resolve_id3_genre("(RX)"), "Remix");
        assert_eq!(resolve_id3_genre("Synthwave"), "Synthwave");
        assert_eq!(resolve_id3_genre("(999)"), "(999)");
    }

    #[test]
    fn numbers() {
        assert_eq!(parse_number_pair("03/12"), (Some(3), Some(12)));
        assert_eq!(parse_number_pair("7"), (Some(7), None));
        assert_eq!(parse_number_pair("/5"), (None, Some(5)));
        assert_eq!(parse_year("2004-05-12"), Some(2004));
        assert_eq!(parse_year("12.05.1999"), Some(1999));
        assert_eq!(parse_float("-6.54 dB"), Some(-6.54));
        assert_eq!(parse_float("+1,5 dB"), Some(1.5));
        assert_eq!(parse_float("abc"), None);
    }

    #[test]
    fn base64() {
        assert_eq!(base64_decode(b"aGVsbG8=").unwrap(), b"hello");
        assert_eq!(
            base64_decode(b"aGVs\nbG8gd29ybGQ=").unwrap(),
            b"hello world"
        );
        assert!(base64_decode(b"a*b").is_none());
    }
}
