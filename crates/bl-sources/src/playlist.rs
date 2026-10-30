//! Playlist files: M3U/M3U8, PLS and XSPF.
//!
//! [`parse`] reads a playlist into [`Entry`]s; the format comes from the
//! content (`#EXTM3U`, `[playlist]`, `<playlist`), the extension only
//! decides for files without a header. [`resolve`] turns an entry's
//! location into an absolute path (relative to the playlist's folder) or
//! keeps a URL, and [`within`] checks that a path stays inside the allowed
//! folders: a playlist must not reach arbitrary files
//! (`../../etc/passwd`). Both work on the path text only and do not touch
//! the file system, so symbolic links inside an allowed folder are trusted.
//! [`write_m3u8`] writes extended M3U.
//!
//! Text is UTF-8 (a byte order mark is skipped; UTF-16 with a BOM is
//! converted) with a Latin-1 fallback for old `.m3u`/`.pls` files. Input
//! is limited to [`MAX_SIZE`] bytes and [`MAX_ENTRIES`] entries (more are
//! ignored); lines longer than 8 KiB are skipped. The XSPF reader is a
//! linear scan for the few elements it needs, not an XML parser.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

/// Largest playlist read (bytes).
pub const MAX_SIZE: usize = 1 << 20;
/// Most entries read from one playlist.
pub const MAX_ENTRIES: usize = 10_000;
/// Longer lines are skipped.
const MAX_LINE: usize = 8192;

/// An entry of a playlist.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Entry {
    /// A path (absolute or relative to the playlist) or a URL, as written;
    /// `file:` URLs and relative references of XSPF are decoded to paths.
    pub location: String,
    pub title: Option<String>,
    pub artist: Option<String>,
    pub duration_secs: Option<f64>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    M3u,
    Pls,
    Xspf,
}

impl Format {
    /// The format of a playlist extension ("m3u", ".PLS", …).
    pub fn from_ext(ext: &str) -> Option<Format> {
        match ext.trim_start_matches('.').to_ascii_lowercase().as_str() {
            "m3u" | "m3u8" => Some(Format::M3u),
            "pls" => Some(Format::Pls),
            "xspf" => Some(Format::Xspf),
            _ => None,
        }
    }

    /// The usual extension.
    pub fn ext(self) -> &'static str {
        match self {
            Format::M3u => "m3u",
            Format::Pls => "pls",
            Format::Xspf => "xspf",
        }
    }
}

/// Decodes playlist bytes: UTF-8 (BOM skipped) or UTF-16 with a BOM, else
/// Latin-1.
pub fn decode_text(data: &[u8]) -> String {
    if let Some(d) = data.strip_prefix(b"\xEF\xBB\xBF") {
        return String::from_utf8_lossy(d).into_owned();
    }
    let utf16 = |d: &[u8], be: bool| {
        let units: Vec<u16> = d
            .chunks_exact(2)
            .map(|c| {
                if be {
                    u16::from_be_bytes([c[0], c[1]])
                } else {
                    u16::from_le_bytes([c[0], c[1]])
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(d) = data.strip_prefix(b"\xFF\xFE") {
        return utf16(d, false);
    }
    if let Some(d) = data.strip_prefix(b"\xFE\xFF") {
        return utf16(d, true);
    }
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => data.iter().map(|&b| b as char).collect(),
    }
}

/// Text without control characters other than tabs and line breaks.
fn is_text(t: &str) -> bool {
    !t.chars()
        .any(|c| (c as u32) < 0x20 && !matches!(c, '\t' | '\r' | '\n'))
}

/// Non-empty trimmed lines (LF, CRLF or CR), overlong ones skipped.
fn lines(text: &str) -> impl Iterator<Item = &str> {
    text.split(['\n', '\r'])
        .map(str::trim)
        .filter(|l| !l.is_empty() && l.len() <= MAX_LINE)
}

/// `s` without the ASCII prefix `p` (any case).
fn strip_prefix_ci<'a>(s: &'a str, p: &str) -> Option<&'a str> {
    let head = s.get(..p.len())?;
    head.eq_ignore_ascii_case(p).then(|| &s[p.len()..])
}

/// Detects the format from the content; `ext_hint` decides for text
/// without a recognisable header.
pub fn detect(data: &[u8], ext_hint: Option<&str>) -> Option<Format> {
    let t = decode_text(&data[..data.len().min(4096)]);
    let head: String = t
        .trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
        .chars()
        .take(1024)
        .collect::<String>()
        .to_ascii_lowercase();
    if head.starts_with("[playlist]") {
        return Some(Format::Pls);
    }
    if head.starts_with("#extm3u") {
        return Some(Format::M3u);
    }
    if head.starts_with('<') {
        return (head.contains("<playlist") || head.contains("xspf.org")).then_some(Format::Xspf);
    }
    if !is_text(&t) {
        return None;
    }
    if let Some(f) = ext_hint.and_then(Format::from_ext) {
        return Some(f);
    }
    // A plain list: the first entry is a URL or an audio file.
    let first = lines(&t).find(|l| !l.starts_with('#'))?;
    let looks_like_entry = first.contains("://")
        || bl_media::is_supported_extension(&crate::ext_of(&first.replace('\\', "/")));
    looks_like_entry.then_some(Format::M3u)
}

/// Reads a playlist (at most [`MAX_SIZE`] bytes).
pub fn parse(data: &[u8], ext_hint: Option<&str>) -> Result<Vec<Entry>, String> {
    if data.len() > MAX_SIZE {
        return Err("playlist too large".into());
    }
    let format = detect(data, ext_hint).ok_or("not a playlist (M3U, PLS or XSPF)")?;
    let text = decode_text(data);
    Ok(match format {
        Format::M3u => parse_m3u(&text),
        Format::Pls => parse_pls(&text),
        Format::Xspf => parse_xspf(&text),
    })
}

/// Splits "Artist - Title"; without the separator all of it is the title.
pub fn split_artist_title(s: &str) -> (Option<String>, Option<String>) {
    let s = s.trim();
    if s.is_empty() {
        return (None, None);
    }
    match s.split_once(" - ") {
        Some((a, t)) if !a.trim().is_empty() && !t.trim().is_empty() => {
            (Some(a.trim().to_string()), Some(t.trim().to_string()))
        }
        _ => (None, Some(s.to_string())),
    }
}

/// A positive, finite duration.
fn duration(s: &str) -> Option<f64> {
    s.trim()
        .parse::<f64>()
        .ok()
        .filter(|d| d.is_finite() && *d > 0.0)
}

/// `#EXTINF:` value: `duration[ key="value" …],Artist - Title`.
fn parse_extinf(v: &str) -> Entry {
    // The name starts after the first comma outside quotes (attribute
    // values of IPTV-style lists may contain commas).
    let mut quoted = false;
    let mut comma = None;
    for (i, c) in v.char_indices() {
        match c {
            '"' => quoted = !quoted,
            ',' if !quoted => {
                comma = Some(i);
                break;
            }
            _ => {}
        }
    }
    let (head, name) = match comma {
        Some(i) => (&v[..i], &v[i + 1..]),
        None => (v, ""),
    };
    let (artist, title) = split_artist_title(name);
    Entry {
        location: String::new(),
        title,
        artist,
        duration_secs: head.split_whitespace().next().and_then(duration),
    }
}

/// Reads M3U/M3U8 (plain or extended).
pub fn parse_m3u(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut info: Option<Entry> = None;
    for line in lines(text) {
        if out.len() >= MAX_ENTRIES {
            break;
        }
        if line.starts_with('#') {
            if let Some(v) = strip_prefix_ci(line, "#EXTINF:") {
                info = Some(parse_extinf(v));
            }
            continue;
        }
        let mut e = info.take().unwrap_or_default();
        e.location = line.to_string();
        out.push(e);
    }
    out
}

/// Reads PLS (`FileN=`, `TitleN=`, `LengthN=`; entries in the order of N).
pub fn parse_pls(text: &str) -> Vec<Entry> {
    let mut entries: BTreeMap<u32, Entry> = BTreeMap::new();
    let mut in_playlist = true;
    for line in lines(text) {
        if line.starts_with('[') {
            in_playlist = line.eq_ignore_ascii_case("[playlist]");
            continue;
        }
        if !in_playlist || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim();
        let (field, n) = if let Some(n) = key.strip_prefix("file") {
            (0, n)
        } else if let Some(n) = key.strip_prefix("title") {
            (1, n)
        } else if let Some(n) = key.strip_prefix("length") {
            (2, n)
        } else {
            continue;
        };
        let Ok(n) = n.parse::<u32>() else {
            continue;
        };
        if !entries.contains_key(&n) && entries.len() >= MAX_ENTRIES {
            continue;
        }
        let e = entries.entry(n).or_default();
        match field {
            0 => e.location = value.to_string(),
            1 => e.title = (!value.is_empty()).then(|| value.to_string()),
            _ => e.duration_secs = duration(value),
        }
    }
    entries
        .into_values()
        .filter(|e| !e.location.is_empty())
        .collect()
}

/// The text of an element whose content starts at `pos` (up to the next
/// tag; CDATA sections are taken as they are). Returns it and where the
/// scan continues.
fn element_text(xml: &str, mut pos: usize) -> (String, usize) {
    let mut out = String::new();
    loop {
        let rest = &xml[pos..];
        let Some(lt) = rest.find('<') else {
            out.push_str(&bl_upnp::xml::unescape(rest));
            return (out, xml.len());
        };
        out.push_str(&bl_upnp::xml::unescape(&rest[..lt]));
        let tag = &rest[lt..];
        if let Some(cdata) = tag.strip_prefix("<![CDATA[") {
            let end = cdata.find("]]>").unwrap_or(cdata.len());
            out.push_str(&cdata[..end]);
            pos += lt + 9 + (end + 3).min(cdata.len());
            continue;
        }
        return (out, pos + lt);
    }
}

/// An XSPF location: `file:` URLs and relative references become paths,
/// other URLs stay.
fn xspf_location(loc: &str) -> Option<String> {
    let loc = loc.trim();
    if strip_prefix_ci(loc, "file:").is_some() {
        return file_url_to_path(loc);
    }
    if is_url(loc) {
        return Some(loc.to_string());
    }
    let path = crate::decode(loc);
    (!path.is_empty()).then_some(path)
}

/// Reads XSPF (`<track>` with `location`, `title`, `creator` and
/// `duration` in milliseconds).
pub fn parse_xspf(xml: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut track: Option<Entry> = None;
    let mut has_location = false;
    let mut pos = 0;
    while out.len() < MAX_ENTRIES {
        let Some(off) = xml.get(pos..).and_then(|r| r.find('<')) else {
            break;
        };
        let start = pos + off;
        let rest = &xml[start..];
        let skip_to = |end: &str| rest.find(end).map_or(xml.len(), |i| start + i + end.len());
        if rest.starts_with("<!--") {
            pos = skip_to("-->");
            continue;
        }
        if rest.starts_with("<![CDATA[") {
            pos = skip_to("]]>");
            continue;
        }
        let Some(gt) = rest.find('>') else {
            break;
        };
        let tag = &rest[1..gt];
        pos = start + gt + 1;
        let closing = tag.starts_with('/');
        let empty = tag.ends_with('/');
        let name = tag
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let local = name.rsplit(':').next().unwrap_or(name);
        match local {
            "track" if closing => {
                if let Some(e) = track.take().filter(|e| !e.location.is_empty()) {
                    out.push(e);
                }
            }
            "track" if !empty => {
                track = Some(Entry::default());
                has_location = false;
            }
            "location" | "title" | "creator" | "duration" if !closing && !empty => {
                let Some(e) = track.as_mut() else {
                    continue;
                };
                let (text, next) = element_text(xml, pos);
                pos = next;
                let text = text.trim();
                match local {
                    // The first location; later ones are alternatives.
                    "location" if !has_location => {
                        has_location = true;
                        e.location = xspf_location(text).unwrap_or_default();
                    }
                    "title" if !text.is_empty() => e.title = Some(text.to_string()),
                    "creator" if !text.is_empty() => e.artist = Some(text.to_string()),
                    "duration" => e.duration_secs = duration(text).map(|ms| ms / 1000.0),
                    _ => {}
                }
            }
            _ => {}
        }
    }
    out
}

/// Writes extended M3U (UTF-8): `#EXTINF` with the duration in seconds
/// (-1 if unknown) and "Artist - Title" where known, then the location.
/// Entries whose location has line breaks or other control characters
/// are left out.
pub fn write_m3u8(entries: &[Entry]) -> String {
    let one_line = |s: &str| -> String {
        s.chars()
            .map(|c| if c.is_control() { ' ' } else { c })
            .collect::<String>()
            .trim()
            .to_string()
    };
    let mut out = String::from("#EXTM3U\n");
    for e in entries {
        let loc = e.location.trim();
        if loc.is_empty() || loc.chars().any(char::is_control) {
            continue;
        }
        let artist = e.artist.as_deref().map(one_line).filter(|s| !s.is_empty());
        let title = e.title.as_deref().map(one_line).filter(|s| !s.is_empty());
        let name = match (artist, title) {
            (Some(a), Some(t)) => format!("{a} - {t}"),
            (a, t) => a.or(t).unwrap_or_default(),
        };
        let secs = e.duration_secs.filter(|d| d.is_finite() && *d > 0.0);
        if !name.is_empty() || secs.is_some() {
            let d = secs.map_or(-1, |d| d.round() as i64);
            out.push_str(&format!("#EXTINF:{d},{name}\n"));
        }
        if loc.starts_with('#') {
            // Would read as a comment.
            out.push_str("./");
        }
        out.push_str(loc);
        out.push('\n');
    }
    out
}

/// Where an entry points.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Location {
    /// An absolute, normalised path (check it with [`within`]).
    Path(PathBuf),
    /// A URL (`scheme://…`); the caller decides which schemes it follows.
    Url(String),
}

/// `scheme://…` (a scheme of two or more characters, so `C:\` is a path).
fn is_url(s: &str) -> bool {
    match s.find("://") {
        Some(i) if i >= 2 => s[..i].bytes().enumerate().all(|(j, c)| {
            c.is_ascii_alphabetic() || (j > 0 && (c.is_ascii_digit() || b"+-.".contains(&c)))
        }),
        _ => false,
    }
}

/// The path of a `file:` URL (`file:///a/b%20c`, `file://localhost/…`,
/// `file:///C:/…` → `C:/…`, `file://server/share/…` → `//server/share/…`).
pub fn file_url_to_path(url: &str) -> Option<String> {
    let rest = strip_prefix_ci(url.trim(), "file:")?;
    let rest = rest.split(['?', '#']).next().unwrap_or("");
    let path = match rest.strip_prefix("//") {
        Some(r) => {
            let (host, path) = r.split_at(r.find('/').unwrap_or(r.len()));
            if host.is_empty() || host.eq_ignore_ascii_case("localhost") {
                crate::decode(path)
            } else {
                format!("//{}{}", crate::decode(host), crate::decode(path))
            }
        }
        None => crate::decode(rest),
    };
    let b = path.as_bytes();
    if b.len() >= 3 && b[0] == b'/' && b[1].is_ascii_alphabetic() && b[2] == b':' {
        return Some(path[1..].to_string());
    }
    (!path.is_empty()).then_some(path)
}

/// `C:\…`, `C:/…` or `C:…`.
fn is_drive_path(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// Resolves an entry's location: URLs stay, `file:` URLs and paths become
/// absolute normalised paths (relative ones against `dir`, the playlist's
/// folder, which should be absolute). `\` separates folders like `/`.
/// Windows drive and UNC paths only resolve on Windows. None if the
/// location cannot be used.
pub fn resolve(location: &str, dir: &Path) -> Option<Location> {
    let loc = location.trim();
    if loc.is_empty() || loc.contains('\0') {
        return None;
    }
    if strip_prefix_ci(loc, "file:").is_some() {
        return resolve_path(&file_url_to_path(loc)?, dir);
    }
    if is_url(loc) {
        return Some(Location::Url(loc.to_string()));
    }
    resolve_path(loc, dir)
}

fn resolve_path(p: &str, dir: &Path) -> Option<Location> {
    if p.contains('\0') {
        return None;
    }
    let path = if cfg!(windows) {
        PathBuf::from(p.replace('/', "\\"))
    } else {
        let unc = p.starts_with("\\\\") || p.starts_with("//");
        if unc || is_drive_path(p) {
            return None;
        }
        PathBuf::from(p.replace('\\', "/"))
    };
    let full = if path.is_absolute() {
        path
    } else {
        dir.join(path)
    };
    Some(Location::Path(normalize(&full)))
}

/// Removes `.` and `..` from a path without touching the file system
/// (`..` never goes above the root; leading `..` of a relative path stay).
pub fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => match out.components().next_back() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) | Some(Component::Prefix(_)) => {}
                _ => out.push(".."),
            },
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// Whether `path` lies inside one of the folders `roots` (or is one of
/// them), after normalising both. Relative paths and roots never match.
/// On Windows the comparison ignores case.
pub fn within<P: AsRef<Path>>(path: &Path, roots: &[P]) -> bool {
    if !path.is_absolute() {
        return false;
    }
    let p = normalize(path);
    roots.iter().any(|r| {
        let r = r.as_ref();
        r.is_absolute() && starts_with(&p, &normalize(r))
    })
}

fn starts_with(p: &Path, root: &Path) -> bool {
    if cfg!(windows) {
        let lower = |c: Component| c.as_os_str().to_string_lossy().to_lowercase();
        let mut pc = p.components();
        root.components()
            .all(|rc| pc.next().is_some_and(|c| lower(c) == lower(rc)))
    } else {
        p.starts_with(root)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(location: &str, artist: Option<&str>, title: Option<&str>, d: Option<f64>) -> Entry {
        Entry {
            location: location.into(),
            title: title.map(Into::into),
            artist: artist.map(Into::into),
            duration_secs: d,
        }
    }

    #[test]
    fn extended_m3u() {
        let data = "\u{feff}#EXTM3U\r\n\
            #PLAYLIST:Mix\r\n\
            #EXTINF:215,Daft Punk - One More Time\r\n\
            Daft Punk/01 One More Time.mp3\r\n\
            \r\n\
            # a comment\r\n\
            /abs/path/song.flac\r\n\
            #EXTINF:-1 tvg-logo=\"a,b.png\" group-title=\"x\",Radio, The Station\r\n\
            http://example.com/stream?x=1\r\n\
            #EXTINF:12.5,  \r\n\
            ..\\Other\\track.ogg\r\n";
        let v = parse(data.as_bytes(), None).unwrap();
        assert_eq!(
            v,
            vec![
                e(
                    "Daft Punk/01 One More Time.mp3",
                    Some("Daft Punk"),
                    Some("One More Time"),
                    Some(215.0)
                ),
                e("/abs/path/song.flac", None, None, None),
                e(
                    "http://example.com/stream?x=1",
                    None,
                    Some("Radio, The Station"),
                    None
                ),
                e("..\\Other\\track.ogg", None, None, Some(12.5)),
            ]
        );
    }

    #[test]
    fn plain_m3u_and_encodings() {
        // No header: the extension or the first entry decides.
        let v = parse(b"song a.mp3\nsong b.mp3\n", None).unwrap();
        assert_eq!(v.len(), 2);
        let v = parse(b"stream\n", Some("m3u")).unwrap();
        assert_eq!(v[0].location, "stream");
        assert!(parse(b"just some text\n", None).is_err());
        let v = parse(b"http://radio.example/live\n", None).unwrap();
        assert_eq!(v[0].location, "http://radio.example/live");
        // Latin-1 (old .m3u).
        let v = parse(
            b"#EXTM3U\n#EXTINF:1,Bj\xf6rk - J\xf3ga\nj\xf3ga.mp3\n",
            None,
        )
        .unwrap();
        assert_eq!(v[0].artist.as_deref(), Some("Björk"));
        assert_eq!(v[0].location, "jóga.mp3");
        // UTF-16 with BOM.
        let mut d = vec![0xff, 0xfe];
        for u in "#EXTM3U\r\nä.mp3\r\n".encode_utf16() {
            d.extend_from_slice(&u.to_le_bytes());
        }
        assert_eq!(parse(&d, None).unwrap()[0].location, "ä.mp3");
        // CR only line ends.
        assert_eq!(parse(b"#EXTM3U\ra.mp3\rb.mp3", None).unwrap().len(), 2);
        // Binary data is not a playlist, whatever the extension.
        assert!(parse(b"\x00\x01\x02ID3", Some("m3u")).is_err());
    }

    #[test]
    fn pls() {
        let data = "[playlist]\n\
            NumberOfEntries=3\n\
            File2=http://b.example/stream\n\
            Title2=Station B\n\
            Length2=-1\n\
            file1=C:\\Music\\a.mp3\n\
            title1=Artist - A\n\
            LENGTH1=180\n\
            Title3=no file\n\
            File10=/x/ten.mp3\n\
            Version=2\n\
            [other]\n\
            File4=ignored\n";
        let v = parse(data.as_bytes(), None).unwrap();
        assert_eq!(
            v,
            vec![
                e("C:\\Music\\a.mp3", None, Some("Artist - A"), Some(180.0)),
                e("http://b.example/stream", None, Some("Station B"), None),
                e("/x/ten.mp3", None, None, None),
            ]
        );
        // Huge indexes do not allocate anything.
        assert_eq!(parse_pls("[playlist]\nFile4294967295=x\n").len(), 1);
        assert!(parse_pls("[playlist]\nFile99999999999=x\n").is_empty());
    }

    #[test]
    fn xspf() {
        let data = r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <title>Not a track</title>
  <trackList>
    <track>
      <location>file:///home/me/Music/My%20Song%20%26%20more.mp3</location>
      <location>http://mirror/ignored.mp3</location>
      <title>Rock &amp; Roll</title>
      <creator>Some &lt;Band&gt;</creator>
      <duration>215500</duration>
    </track>
    <!-- <track><location>commented.mp3</location></track> -->
    <track><location>file:///C:/Music/a.flac</location></track>
    <track><location>Sub%20Dir/b.ogg</location><title><![CDATA[A < B]]></title></track>
    <track><location>https://example.com/s.mp3?a=1&amp;b=2</location></track>
    <track><title>no location</title></track>
    <track/>
  </trackList>
</playlist>"#;
        let v = parse(data.as_bytes(), Some("xml")).unwrap();
        assert_eq!(
            v,
            vec![
                e(
                    "/home/me/Music/My Song & more.mp3",
                    Some("Some <Band>"),
                    Some("Rock & Roll"),
                    Some(215.5)
                ),
                e("C:/Music/a.flac", None, None, None),
                e("Sub Dir/b.ogg", None, Some("A < B"), None),
                e("https://example.com/s.mp3?a=1&b=2", None, None, None),
            ]
        );
        // Degenerate input stays linear and does not panic.
        let nested = "<playlist>".to_string() + &"<track><location>".repeat(50_000);
        assert!(parse_xspf(&nested).is_empty());
        assert!(parse_xspf("<playlist><track><location>x").is_empty());
        assert!(parse_xspf("<playlist><track><location><![CDATA[x").is_empty());
    }

    #[test]
    fn limits() {
        let mut big = String::from("#EXTM3U\n");
        for i in 0..MAX_ENTRIES + 5 {
            big.push_str(&format!("{i}.mp3\n"));
        }
        assert_eq!(parse(big.as_bytes(), None).unwrap().len(), MAX_ENTRIES);
        let huge = vec![b'a'; MAX_SIZE + 1];
        assert!(parse(&huge, Some("m3u")).is_err());
        // Overlong lines are skipped.
        let long = format!("#EXTM3U\n{}\nok.mp3\n", "x".repeat(MAX_LINE + 1));
        assert_eq!(parse(long.as_bytes(), None).unwrap()[0].location, "ok.mp3");
    }

    #[test]
    fn detection() {
        assert_eq!(detect(b"[Playlist]\nFile1=x", None), Some(Format::Pls));
        assert_eq!(detect(b"  #EXTM3U\n", Some("pls")), Some(Format::M3u));
        assert_eq!(
            detect(
                b"<?xml version='1.0'?><playlist xmlns='http://xspf.org/ns/0/'>",
                None
            ),
            Some(Format::Xspf)
        );
        assert_eq!(detect(b"<html><body>", Some("xspf")), None);
        assert_eq!(detect(b"File1=x\n", Some(".PLS")), Some(Format::Pls));
        assert_eq!(detect(b"", None), None);
    }

    #[cfg(unix)]
    #[test]
    fn resolving() {
        let dir = Path::new("/music/lists");
        let p = |s: &str| Some(Location::Path(PathBuf::from(s)));
        assert_eq!(resolve("a.mp3", dir), p("/music/lists/a.mp3"));
        assert_eq!(resolve("../Album/./b.mp3", dir), p("/music/Album/b.mp3"));
        assert_eq!(resolve("..\\Album\\c.mp3", dir), p("/music/Album/c.mp3"));
        assert_eq!(resolve("/other/d.mp3", dir), p("/other/d.mp3"));
        assert_eq!(resolve("../../../../etc/passwd", dir), p("/etc/passwd"));
        assert_eq!(
            resolve("file:///music/My%20Song.mp3", dir),
            p("/music/My Song.mp3")
        );
        assert_eq!(
            resolve("file://localhost/music/x.mp3", dir),
            p("/music/x.mp3")
        );
        assert_eq!(resolve("file:../x.mp3", dir), p("/music/x.mp3"));
        assert_eq!(
            resolve("http://h/s.mp3", dir),
            Some(Location::Url("http://h/s.mp3".into()))
        );
        // Windows-only locations, empty and NUL.
        assert_eq!(resolve("C:\\Music\\a.mp3", dir), None);
        assert_eq!(resolve("file:///C:/Music/a.mp3", dir), None);
        assert_eq!(resolve("\\\\server\\share\\a.mp3", dir), None);
        assert_eq!(resolve("file://server/share/a.mp3", dir), None);
        assert_eq!(resolve("  ", dir), None);
        assert_eq!(resolve("a\0.mp3", dir), None);
        assert_eq!(resolve("file:///a%00b", dir), None);
        // A file whose name has a colon is not a URL.
        assert_eq!(resolve("ab:c.mp3", dir), p("/music/lists/ab:c.mp3"));
    }

    #[cfg(unix)]
    #[test]
    fn within_roots() {
        let roots = [PathBuf::from("/music"), PathBuf::from("/srv/audio/")];
        assert!(within(Path::new("/music/a/b.mp3"), &roots));
        assert!(within(Path::new("/srv/audio/x.flac"), &roots));
        assert!(within(Path::new("/music"), &roots));
        assert!(within(Path::new("/music/a/../b.mp3"), &roots));
        assert!(!within(Path::new("/music/../etc/passwd"), &roots));
        assert!(!within(Path::new("/music2/a.mp3"), &roots));
        assert!(!within(Path::new("/etc/passwd"), &roots));
        assert!(!within(Path::new("music/a.mp3"), &roots));
        assert!(!within(
            Path::new("/music/a.mp3"),
            &[PathBuf::from("music")]
        ));
        assert!(!within::<PathBuf>(Path::new("/music/a.mp3"), &[]));
        // A playlist's entries, resolved and checked.
        let dir = Path::new("/music/lists");
        for (loc, ok) in [
            ("a.mp3", true),
            ("../../etc/shadow", false),
            ("/tmp/x", false),
        ] {
            let Some(Location::Path(p)) = resolve(loc, dir) else {
                panic!("{loc}");
            };
            assert_eq!(within(&p, &roots), ok, "{loc}");
        }
    }

    #[test]
    fn normalizing() {
        assert_eq!(normalize(Path::new("/a/./b/../c")), PathBuf::from("/a/c"));
        assert_eq!(normalize(Path::new("/../..")), PathBuf::from("/"));
        assert_eq!(normalize(Path::new("a/../../b")), PathBuf::from("../b"));
        assert_eq!(normalize(Path::new("./a//b/")), PathBuf::from("a/b"));
    }

    #[test]
    fn writing() {
        let entries = vec![
            e("/m/a.mp3", Some("Artist"), Some("Title"), Some(180.4)),
            e("http://h/s", None, Some("Station"), None),
            e("plain.mp3", None, None, None),
            e("#1.mp3", None, None, Some(3.0)),
            e("evil.mp3\n#EXTINF:1,x", None, None, None),
            e("b.mp3", Some("A\r\nB"), Some("T"), None),
        ];
        let text = write_m3u8(&entries);
        assert_eq!(
            text,
            "#EXTM3U\n\
             #EXTINF:180,Artist - Title\n/m/a.mp3\n\
             #EXTINF:-1,Station\nhttp://h/s\n\
             plain.mp3\n\
             #EXTINF:3,\n./#1.mp3\n\
             #EXTINF:-1,A  B - T\nb.mp3\n"
        );
        let back = parse(text.as_bytes(), Some("m3u8")).unwrap();
        assert_eq!(back.len(), 5);
        assert_eq!(
            back[0],
            e("/m/a.mp3", Some("Artist"), Some("Title"), Some(180.0))
        );
        assert_eq!(back[3].location, "./#1.mp3");
    }
}
