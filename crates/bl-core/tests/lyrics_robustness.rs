//! Damaged song texts (LRC, enhanced LRC, TTML): reading ends with lines,
//! bounded in number and length, in time linear in the text.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_core::lyrics::parse;

const LRC: &str = "\
[ti:Song]
[ar:Someone]
[length:03:15.20]
[offset:250]
[re:speech timing (base)]
[00:01.00][00:31.00]First line, sung twice
[00:05.50]Anne-Marie: <00:05.50>You <00:05.90>say <00:06.40>so<00:06.80>
[v:2]
[00:09.00]<00:09.00>(Back<00:09.40>ing) <00:09.80>voice
[01:02:03.5]late
[00:12:34]hundredths
";

const PLAIN: &str = "\n\nVerse one\nAnne-Marie: line two\n\nAnne-Marie: chorus\n\n\n";

const TTML: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<tt xmlns="http://www.w3.org/ns/ttml" xmlns:ttm="http://www.w3.org/ns/ttml#metadata">
<head><metadata><ttm:agent xml:id="v1"/><ttm:agent xml:id="v2"/></metadata></head>
<body><div>
<p begin="00:01.000" end="00:04.000" ttm:agent="v1"><span begin="00:01.000" end="00:01.500">One</span> <span begin="00:01.600" end="00:02.000">two</span><span ttm:role="x-bg"><span begin="2.1s" end="2.5s">(echo)</span></span></p>
<p begin="5s" ttm:agent="v2"><span begin="5s" end="5.5s">Three &amp; <br/>four</span><span ttm:role="x-translation">drei</span></p>
<!-- a comment --><p begin="00:00:07:15">frames</p>
</div></body></tt>"#;

fn exercise(d: &[u8]) {
    let text = bl_media::lyrics::decode_text(d);
    let text = bl_media::lyrics::normalize_text(&text);
    let l = parse(&text);
    assert!(l.lines.len() <= 5001);
    for line in &l.lines {
        assert!(line.text.len() <= 4096);
        if let Some(t) = line.time_ms {
            assert!(t >= 0);
        }
    }
    let _ = l.to_json();
    let _ = parse(&l.to_lrc());
    let _ = parse(&l.to_enhanced_lrc());
}

#[test]
fn damaged_song_texts_end_cleanly() {
    let samples = vec![
        LRC.as_bytes().to_vec(),
        PLAIN.as_bytes().to_vec(),
        TTML.as_bytes().to_vec(),
    ];
    let tokens: &[&[u8]] = &[
        b"[",
        b"]",
        b"[00:01.00]",
        b"<00:01.00>",
        b"[offset:-9223372036854775808]",
        b"[99999999999:59.99]",
        b"[v:",
        b"<",
        b">",
        b":",
        b".",
        b"\n",
        b"\xc3\xa4",
        b"&",
        b"&amp;",
        b"&#x",
        b"<span begin=\"1s\">",
        b"</span>",
        b"<p begin=\"9e300s\">",
        b"</p>",
        b"<br/>",
        b"ttm:agent=\"",
        b"<!--",
        b"(",
        b")",
    ];
    fuzz::run("lyrics", &samples, tokens, 20_000, exercise);
}
