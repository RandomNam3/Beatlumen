//! Finding a song's text: an `.lrc` file next to the audio file, the
//! embedded tag, or a `.txt` file next to it (in this order).

use std::path::Path;

/// Where a song text came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LyricsSource {
    /// `<name>.lrc` next to the file.
    LrcFile,
    /// The file's tags.
    Embedded,
    /// `<name>.txt` next to the file.
    TextFile,
}

impl LyricsSource {
    pub fn name(self) -> &'static str {
        match self {
            LyricsSource::LrcFile => "lrc",
            LyricsSource::Embedded => "embedded",
            LyricsSource::TextFile => "txt",
        }
    }
}

/// The song text of the audio file at `path`, if there is one.
pub fn find_lyrics(path: &Path) -> Option<(String, LyricsSource)> {
    if let Some(t) = read_text_file(&path.with_extension("lrc")) {
        return Some((t, LyricsSource::LrcFile));
    }
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase());
    if let Ok(mut f) = std::fs::File::open(path) {
        if let Ok(info) = crate::read_info(&mut f, ext.as_deref(), false) {
            if let Some(t) = info.tags.lyrics {
                return Some((t, LyricsSource::Embedded));
            }
        }
    }
    read_text_file(&path.with_extension("txt")).map(|t| (t, LyricsSource::TextFile))
}

/// A small text file as a string (UTF-8, UTF-16 with BOM, else Latin-1).
pub fn read_text_file(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > crate::tags::MAX_LYRICS as u64 {
        return None;
    }
    let text = decode_text(&std::fs::read(path).ok()?);
    let t = text.trim();
    (!t.is_empty()).then(|| t.replace("\r\n", "\n"))
}

/// Text bytes as a string (UTF-8, UTF-16 with BOM, else Latin-1).
pub fn decode_text(b: &[u8]) -> String {
    let utf16 = |be: bool| {
        let units = b[2..].chunks_exact(2).map(|c| {
            if be {
                u16::from_be_bytes([c[0], c[1]])
            } else {
                u16::from_le_bytes([c[0], c[1]])
            }
        });
        char::decode_utf16(units)
            .map(|c| c.unwrap_or('\u{fffd}'))
            .collect()
    };
    match b {
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [0xff, 0xfe, ..] => utf16(false),
        [0xfe, 0xff, ..] => utf16(true),
        _ => match std::str::from_utf8(b) {
            Ok(s) => s.to_string(),
            Err(_) => b.iter().map(|&c| c as char).collect(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_encodings() {
        assert_eq!(decode_text(b"\xef\xbb\xbfgr\xc3\xbc\xc3\x9f"), "grüß");
        assert_eq!(decode_text(b"gr\xfc\xdf"), "grüß");
        assert_eq!(decode_text(&[0xff, 0xfe, b'h', 0, b'i', 0]), "hi");
        assert_eq!(decode_text(&[0xfe, 0xff, 0, b'h', 0, b'i']), "hi");
    }

    #[test]
    fn sidecar_files_first() {
        let dir = std::env::temp_dir().join(format!("bl-lyrics-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let song = dir.join("song.mp3");
        std::fs::write(&song, b"not audio").unwrap();
        assert_eq!(find_lyrics(&song), None);
        std::fs::write(dir.join("song.txt"), "plain\r\ntext\n").unwrap();
        assert_eq!(
            find_lyrics(&song),
            Some(("plain\ntext".into(), LyricsSource::TextFile))
        );
        std::fs::write(dir.join("song.lrc"), "[00:01.00]synced").unwrap();
        assert_eq!(
            find_lyrics(&song),
            Some(("[00:01.00]synced".into(), LyricsSource::LrcFile))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
