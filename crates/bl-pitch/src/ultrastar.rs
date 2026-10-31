//! UltraStar TXT: header fields (`#TITLE:`, `#ARTIST:`, `#BPM:`, `#GAP:`,
//! …), then notes `: start length pitch text` in grid steps (`*` golden,
//! `F` freestyle, `R`/`G` rap), `- step` line breaks, `P1`/`P2` before
//! each singer's part of a duet, and `E` at the end. Pitch 0 is C4.

use crate::track::{Grid, KaraokeLine, KaraokeTrack, Note, NoteKind};

/// Grid used when the track has none: steps of 25 ms.
const DEFAULT_BPM: f64 = 600.0;

/// Header fields this module writes itself (others in `extra` are kept).
const OWN_FIELDS: &[&str] = &[
    "TITLE",
    "ARTIST",
    "LANGUAGE",
    "BPM",
    "GAP",
    "RELATIVE",
    "ENCODING",
    "DUETSINGERP1",
    "DUETSINGERP2",
    "P1",
    "P2",
];

fn number(v: f64) -> String {
    if v.fract() == 0.0 {
        format!("{}", v as i64)
    } else {
        // Comma decimals, as the classic format writes them.
        format!("{}", (v * 1000.0).round() / 1000.0).replace('.', ",")
    }
}

/// Which `P` section a voice goes to: voices 0 and 1 sing P1, 2 sings P2,
/// higher voices (both singers) P3.
fn part_of(voice: u8) -> u8 {
    voice.clamp(1, 3)
}

/// The track as UltraStar TXT (UTF-8). Without a grid in the track, notes
/// go on 25 ms steps from the first note.
pub fn to_ultrastar(t: &KaraokeTrack) -> String {
    let grid = t.grid.unwrap_or_else(|| Grid {
        bpm: DEFAULT_BPM,
        gap_ms: t.notes().map(|n| n.start_ms).min().unwrap_or(0) as f64,
    });
    let duet = t.is_duet();
    let mut out = String::new();
    let mut field = |k: &str, v: &str| {
        if !v.is_empty() {
            out += &format!("#{k}:{v}\n");
        }
    };
    field("TITLE", &t.title);
    field("ARTIST", &t.artist);
    field("LANGUAGE", &t.language);
    if duet {
        for (i, s) in t.singers.iter().take(2).enumerate() {
            field(&format!("DUETSINGERP{}", i + 1), s);
        }
    }
    for (k, v) in &t.extra {
        if !OWN_FIELDS.contains(&k.to_ascii_uppercase().as_str()) {
            field(k, v);
        }
    }
    field("BPM", &number(grid.bpm));
    field("GAP", &number(grid.gap_ms));

    let parts: Vec<u8> = if duet {
        let mut p: Vec<u8> = t.lines.iter().map(|l| part_of(l.voice)).collect();
        p.sort_unstable();
        p.dedup();
        p
    } else {
        vec![0]
    };
    for part in parts {
        if duet {
            out += &format!("P{part}\n");
        }
        let lines: Vec<&KaraokeLine> = t
            .lines
            .iter()
            .filter(|l| !l.notes.is_empty() && (!duet || part_of(l.voice) == part))
            .collect();
        let mut prev_end: Option<i64> = None;
        for l in lines {
            let first = grid.to_step(l.notes[0].start_ms);
            if let Some(end) = prev_end {
                out += &format!("- {}\n", end.min(first));
            }
            for n in &l.notes {
                let start = grid.to_step(n.start_ms);
                let len = (grid.to_step(n.end_ms()) - start).max(1);
                out += &format!(
                    "{} {} {} {} {}\n",
                    n.kind.symbol(),
                    start,
                    len,
                    n.pitch - 60,
                    n.text
                );
                prev_end = Some(prev_end.map_or(start + len, |e| e.max(start + len)));
            }
        }
    }
    out += "E\n";
    out
}

/// Reads UltraStar TXT from bytes: UTF-8 (with or without BOM), else
/// Windows-1252/Latin-1 as older files are.
pub fn from_ultrastar_bytes(b: &[u8]) -> Result<KaraokeTrack, String> {
    match std::str::from_utf8(b) {
        Ok(s) => from_ultrastar(s),
        Err(_) => from_ultrastar(&b.iter().map(|&c| cp1252(c)).collect::<String>()),
    }
}

fn cp1252(c: u8) -> char {
    const HIGH: [char; 32] = [
        '€', '\u{81}', '‚', 'ƒ', '„', '…', '†', '‡', 'ˆ', '‰', 'Š', '‹', 'Œ', '\u{8d}', 'Ž',
        '\u{8f}', '\u{90}', '‘', '’', '“', '”', '•', '–', '—', '˜', '™', 'š', '›', 'œ', '\u{9d}',
        'ž', 'Ÿ',
    ];
    match c {
        0x80..=0x9f => HIGH[(c - 0x80) as usize],
        _ => c as char,
    }
}

fn parse_num(s: &str) -> Option<f64> {
    s.trim().replace(',', ".").parse().ok()
}

/// Splits off the next whitespace separated token.
fn token(s: &str) -> Option<(&str, &str)> {
    let s = s.trim_start_matches([' ', '\t']);
    if s.is_empty() {
        return None;
    }
    let end = s.find([' ', '\t']).unwrap_or(s.len());
    Some((&s[..end], &s[end..]))
}

/// Reads UltraStar TXT. Notes before any `P` marker have voice 0; `P1`,
/// `P2` (and `P3`, both) set the voice of the lines after them. The
/// syllables' spaces are kept as written.
pub fn from_ultrastar(text: &str) -> Result<KaraokeTrack, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut t = KaraokeTrack::default();
    let mut bpm = None;
    let mut gap = 0.0;
    let mut relative = false;
    // Notes as (voice, line number in that voice, step, length, kind,
    // pitch, text); turned into times once the header is known.
    struct Raw {
        voice: u8,
        line: usize,
        step: i64,
        len: i64,
        kind: NoteKind,
        pitch: i32,
        text: String,
    }
    let mut raw: Vec<Raw> = Vec::new();
    let mut voice = 0u8;
    let mut line = 0usize;
    let mut offset = 0i64;
    let mut singers: [String; 2] = Default::default();
    for (no, l) in text.lines().enumerate() {
        let l = l.strip_suffix('\r').unwrap_or(l);
        let err = |what: &str| format!("line {}: {what}: {l:?}", no + 1);
        if let Some(h) = l.strip_prefix('#') {
            let Some((k, v)) = h.split_once(':') else {
                continue;
            };
            let v = v.trim();
            match k.trim().to_ascii_uppercase().as_str() {
                "TITLE" => t.title = v.into(),
                "ARTIST" => t.artist = v.into(),
                "LANGUAGE" => t.language = v.into(),
                "BPM" => bpm = Some(parse_num(v).ok_or_else(|| err("bad BPM"))?),
                "GAP" => gap = parse_num(v).ok_or_else(|| err("bad GAP"))?,
                "RELATIVE" => relative = v.eq_ignore_ascii_case("yes"),
                "ENCODING" => {}
                "DUETSINGERP1" | "P1" => singers[0] = v.into(),
                "DUETSINGERP2" | "P2" => singers[1] = v.into(),
                _ => t.extra.push((k.trim().to_string(), v.to_string())),
            }
            continue;
        }
        let trimmed = l.trim_start();
        let Some(c) = trimmed.chars().next() else {
            continue;
        };
        let rest = &trimmed[c.len_utf8()..];
        match c {
            'E' if rest.trim().is_empty() => break,
            'P' if rest.trim().parse::<u8>().is_ok() => {
                voice = rest.trim().parse::<u8>().unwrap_or(1).min(9);
                line += 1;
                offset = 0;
            }
            '-' => {
                let mut nums = rest
                    .split_whitespace()
                    .filter_map(|s| s.parse::<i64>().ok());
                let a = nums
                    .next()
                    .ok_or_else(|| err("line break without a step"))?;
                if relative {
                    offset += nums.next().unwrap_or(a);
                }
                line += 1;
            }
            'B' => {} // tempo changes: not supported, ignored
            _ => {
                let Some(kind) = NoteKind::from_symbol(c) else {
                    return Err(err("unknown line"));
                };
                let mut fields = [0i64; 3];
                let mut r = rest;
                for f in &mut fields {
                    let (tok, after) = token(r).ok_or_else(|| err("short note"))?;
                    *f = tok.parse().map_err(|_| err("bad number"))?;
                    r = after;
                }
                // One separator, then the syllable as written.
                let text = r
                    .strip_prefix(' ')
                    .or_else(|| r.strip_prefix('\t'))
                    .unwrap_or(r);
                raw.push(Raw {
                    voice,
                    line,
                    step: fields[0] + offset,
                    len: fields[1],
                    kind,
                    pitch: fields[2] as i32 + 60,
                    text: text.to_string(),
                });
            }
        }
    }
    let bpm = bpm.filter(|b| *b > 0.0).ok_or("no #BPM")?;
    let grid = Grid { bpm, gap_ms: gap };
    t.grid = Some(grid);
    t.singers = singers.into_iter().collect();
    while t.singers.last().is_some_and(|s| s.is_empty()) {
        t.singers.pop();
    }
    let mut current: Option<usize> = None;
    for r in raw {
        let start = grid.to_ms(r.step);
        let note = Note {
            start_ms: start,
            dur_ms: grid.to_ms(r.step + r.len) - start,
            pitch: r.pitch,
            kind: r.kind,
            text: r.text,
        };
        if current != Some(r.line) {
            t.lines.push(KaraokeLine {
                voice: r.voice,
                notes: Vec::new(),
            });
            current = Some(r.line);
        }
        if let Some(l) = t.lines.last_mut() {
            l.notes.push(note);
        }
    }
    t.sort_lines();
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    const SONG: &str = "\
#TITLE:A Song
#ARTIST:Two People
#LANGUAGE:English
#DUETSINGERP1:Anne
#DUETSINGERP2:Bob
#MP3:a song.mp3
#BPM:300,5
#GAP:1200
P1
: 0 4 5 Hel
: 4 4 7 lo\u{20}
* 8 8 9 world
- 16
F 20 4 0 (spo
R 24 4 0 ken)
P2
: 10 6 -3 Ja
: 16 2 -1 ~
- 18
G 30 4 2  oui
E
";

    #[test]
    fn round_trip() {
        let t = from_ultrastar(SONG).unwrap();
        assert_eq!(t.title, "A Song");
        assert_eq!(t.singers, ["Anne", "Bob"]);
        assert_eq!(
            t.grid,
            Some(Grid {
                bpm: 300.5,
                gap_ms: 1200.0
            })
        );
        assert_eq!(t.extra, [("MP3".to_string(), "a song.mp3".to_string())]);
        // Lines in time order: P1 line, P2 line, P1 line, P2 line.
        let voices: Vec<u8> = t.lines.iter().map(|l| l.voice).collect();
        assert_eq!(voices, [1, 2, 1, 2]);
        assert_eq!(t.lines[0].text(), "Hello world");
        assert_eq!(t.lines[0].notes[0].pitch, 65);
        assert_eq!(t.lines[0].notes[2].kind, NoteKind::Golden);
        assert_eq!(t.lines[3].notes[0].text, " oui");
        let step = 15000.0f64 / 300.5;
        assert_eq!(
            t.lines[0].notes[1].start_ms,
            (1200.0 + 4.0 * step).round() as i64
        );
        // Out again: the same text.
        assert_eq!(to_ultrastar(&t), SONG);
        // And through JSON.
        let j = crate::KaraokeTrack::from_json(&t.to_json()).unwrap();
        assert_eq!(to_ultrastar(&j), SONG);
    }

    #[test]
    fn relative_latin1_and_crlf() {
        let txt = "#TITLE:Caf\u{e9}\r\n#BPM:200\r\n#GAP:0\r\n#RELATIVE:yes\r\n: 0 2 0 a\r\n- 4 6\r\n: 0 2 1 b\r\n: 2 2 2 c\r\nE\r\n";
        let bytes: Vec<u8> = txt.chars().map(|c| c as u32 as u8).collect();
        let t = from_ultrastar_bytes(&bytes).unwrap();
        assert_eq!(t.title, "Café");
        assert_eq!(t.lines.len(), 2);
        let step = 15000 / 200;
        assert_eq!(t.lines[1].notes[0].start_ms, 6 * step);
        assert_eq!(t.lines[1].notes[1].start_ms, 8 * step);
        assert_eq!(t.lines[1].notes[1].text, "c");
        assert!(from_ultrastar(": 0 1 0 a\nE\n").is_err());
    }

    #[test]
    fn export_without_grid() {
        let t = KaraokeTrack {
            title: "T".into(),
            lines: vec![KaraokeLine {
                voice: 0,
                notes: vec![
                    Note {
                        start_ms: 1000,
                        dur_ms: 260,
                        pitch: 64,
                        kind: NoteKind::Normal,
                        text: "la ".into(),
                    },
                    Note {
                        start_ms: 1300,
                        dur_ms: 10,
                        pitch: 62,
                        kind: NoteKind::Normal,
                        text: "la".into(),
                    },
                ],
            }],
            ..Default::default()
        };
        let s = to_ultrastar(&t);
        assert_eq!(
            s,
            "#TITLE:T\n#BPM:600\n#GAP:1000\n: 0 10 4 la \n: 12 1 2 la\nE\n"
        );
        let back = from_ultrastar(&s).unwrap();
        assert_eq!(back.lines[0].notes[0].start_ms, 1000);
        assert_eq!(back.lines[0].notes[1].start_ms, 1300);
    }
}
