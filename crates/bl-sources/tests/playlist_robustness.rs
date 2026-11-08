//! Damaged playlists (M3U, PLS, XSPF): reading ends with entries or an
//! error, within the entry limit.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_sources::playlist::{self, parse, write_m3u8, MAX_ENTRIES};

const M3U: &str = "#EXTM3U\n#EXTINF:215,Someone - Song\nmusic/song.flac\n\
#EXTINF:-1 tvg-name=\"a, b\" group-title=\"x\",Radio\nhttp://radio.example/stream\n\
#EXTINF:12.5,\nC:\\Music\\track.mp3\nfile:///home/u/a%20b.ogg\n";

const PLS: &str = "[playlist]\nFile1=http://radio.example/a\nTitle1=Radio A\nLength1=-1\n\
File2=song.mp3\nTitle2=Song\nLength2=215\nNumberOfEntries=2\nVersion=2\n";

const XSPF: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<playlist version="1" xmlns="http://xspf.org/ns/0/">
  <trackList>
    <track><location>file:///music/a%20b.flac</location><title>A &amp; B</title>
      <creator>Someone</creator><duration>215000</duration></track>
    <!-- comment <track> -->
    <track><location><![CDATA[http://example.org/x?a=1&b=2]]></location></track>
    <track><title>no location</title></track>
  </trackList>
</playlist>"#;

#[test]
fn damaged_playlists_end_cleanly() {
    let mut utf16 = vec![0xff, 0xfe];
    utf16.extend(M3U.encode_utf16().flat_map(|u| u.to_le_bytes()));
    let samples = vec![
        M3U.as_bytes().to_vec(),
        PLS.as_bytes().to_vec(),
        XSPF.as_bytes().to_vec(),
        utf16,
    ];
    let tokens: &[&[u8]] = &[
        b"\n",
        b"\r",
        b"#EXTINF:",
        b"\"",
        b",",
        b" - ",
        b"File1=",
        b"File4294967295=",
        b"[playlist]",
        b"<track>",
        b"</track>",
        b"<location>",
        b"</location>",
        b"<![CDATA[",
        b"]]>",
        b"<!--",
        b"&amp;",
        b"&",
        b"<",
        b"file://",
        b"%",
        b"\xff\xfe",
        b"\x00",
    ];
    fuzz::run("playlist", &samples, tokens, 20_000, |d| {
        for hint in [None, Some("m3u"), Some("pls"), Some("xspf")] {
            if let Ok(entries) = parse(d, hint) {
                assert!(entries.len() <= MAX_ENTRIES);
                let m3u = write_m3u8(&entries);
                let back = parse(m3u.as_bytes(), Some("m3u8")).unwrap_or_default();
                assert!(back.len() <= entries.len());
                for e in &entries {
                    let _ = playlist::file_url_to_path(&e.location);
                    let _ = playlist::split_artist_title(&e.location);
                }
            }
        }
    });
}
