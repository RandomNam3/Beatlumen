//! Timed song texts (enhanced LRC) as lines of timed pieces, the input of
//! [`crate::notes::build_track`] after [`crate::syllables::syllabify`].

use crate::notes::{LineIn, Syllable};

/// A song text read for the karaoke track.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Song {
    pub title: String,
    pub artist: String,
    /// Lines in time order; pieces are words (with the space after them)
    /// or pieces of words, as in the text.
    pub lines: Vec<LineIn>,
    /// Whether any line has word times (else words are spread over the
    /// lines).
    pub word_timed: bool,
}

impl Song {
    /// All of the sung text, for guessing its language.
    pub fn text(&self) -> String {
        let mut s = String::new();
        for l in &self.lines {
            for p in &l.syllables {
                s.push_str(&p.text);
            }
            s.push('\n');
        }
        s
    }
}

/// "mm:ss.xx" (or "h:mm:ss.xx") in seconds.
fn time(s: &str) -> Option<f64> {
    let s = s.trim();
    let mut parts = s.rsplitn(3, ':');
    let sec: f64 = parts.next()?.parse().ok()?;
    let min: f64 = parts.next()?.parse().ok()?;
    let hours: f64 = match parts.next() {
        Some(h) => h.parse().ok()?,
        None => 0.0,
    };
    (sec.is_finite() && sec >= 0.0 && min >= 0.0).then_some(hours * 3600.0 + min * 60.0 + sec)
}

/// A timed piece as read: start (NaN when the line has no word times),
/// its own end if given, text.
type RawPiece = (f64, Option<f64>, String);

/// Strips a singer's name before a line ("Anne: …", short and without
/// sentence marks), as song texts write duets.
fn strip_name(body: &str) -> &str {
    let plain = body.trim_start();
    if plain.starts_with('<') {
        return body;
    }
    match plain.split_once(": ") {
        Some((name, rest))
            if !name.is_empty()
                && name.chars().count() <= 24
                && !name.contains(['.', ',', '!', '?', '<'])
                && name.split_whitespace().count() <= 3 =>
        {
            rest
        }
        _ => body,
    }
}

/// Reads enhanced LRC: `[mm:ss.xx]` line times, `<mm:ss.xx>` before each
/// word or piece (and after a word that ends before the next starts),
/// `[v:N]` voices, `[ti:]`/`[ar:]`, `[offset:]`. Lines without word times
/// get their words spread from the line's start (at most 0.4 s a word).
/// Text without line times gives no lines.
pub fn parse(text: &str) -> Song {
    let mut song = Song::default();
    let mut voice = 0u8;
    let mut offset = 0.0f64;
    let mut raw: Vec<(f64, u8, Vec<RawPiece>)> = Vec::new();
    for l in text.lines() {
        let mut rest = l.trim();
        let mut starts = Vec::new();
        while let Some((tag, after)) = rest.strip_prefix('[').and_then(|r| r.split_once(']')) {
            if let Some(v) = tag.strip_prefix("ti:") {
                song.title = v.trim().into();
            } else if let Some(v) = tag.strip_prefix("ar:") {
                song.artist = v.trim().into();
            } else if let Some(v) = tag.strip_prefix("v:") {
                voice = v.trim().parse::<u8>().unwrap_or(0).min(9);
            } else if let Some(v) = tag.strip_prefix("offset:") {
                offset = v.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            } else if let Some(t) = time(tag) {
                starts.push(t);
            }
            rest = after;
        }
        if starts.is_empty() {
            continue;
        }
        let body = strip_name(rest);
        for start in starts {
            let start = start - offset;
            let mut pieces = if body.contains('<') {
                song.word_timed = true;
                timed_pieces(body, start, offset)
            } else {
                body.split_whitespace()
                    .map(|w| (f64::NAN, None, format!("{w} ")))
                    .collect()
            };
            if let Some(last) = pieces.last_mut() {
                last.2 = last.2.trim_end().to_string();
            }
            pieces.retain(|p| !p.2.trim().is_empty() && p.2.trim() != "♪");
            if !pieces.is_empty() {
                raw.push((start.max(0.0), voice, pieces));
            }
        }
    }
    raw.sort_by(|a, b| a.0.total_cmp(&b.0));
    for i in 0..raw.len() {
        let next = raw.get(i + 1).map_or(raw[i].0 + 5.0, |n| n.0);
        let (start, voice, pieces) = (raw[i].0, raw[i].1, &raw[i].2);
        let n = pieces.len();
        let untimed = pieces.iter().all(|p| p.0.is_nan());
        let step = ((next - start) / n as f64).min(0.4);
        let starts: Vec<f64> = (0..n)
            .map(|k| {
                if untimed || pieces[k].0.is_nan() {
                    start + k as f64 * step
                } else {
                    pieces[k].0.max(0.0)
                }
            })
            .collect();
        let line_end = next.min(starts[n - 1] + 3.0);
        let syllables = (0..n)
            .map(|k| {
                // An end at (or before) the start says nothing.
                let given = pieces[k].1.filter(|e| *e > starts[k] + 0.005);
                let end = given.unwrap_or(if untimed {
                    starts[k] + step
                } else {
                    starts.get(k + 1).copied().unwrap_or(line_end)
                });
                Syllable {
                    start: starts[k],
                    end: end.max(starts[k]),
                    text: pieces[k].2.clone(),
                }
            })
            .collect();
        song.lines.push(LineIn { voice, syllables });
    }
    song
}

/// The pieces of a line with word times.
fn timed_pieces(body: &str, start: f64, offset: f64) -> Vec<RawPiece> {
    let mut pieces: Vec<RawPiece> = Vec::new();
    let mut t = start;
    let mut cur = String::new();
    let mut rest = body;
    let flush = |pieces: &mut Vec<RawPiece>, cur: &mut String, t: f64, end: Option<f64>| {
        if !cur.trim().is_empty() {
            pieces.push((t, end, std::mem::take(cur)));
        } else if let Some(last) = pieces.last_mut() {
            // Spaces belong to the word before.
            last.2.push_str(cur);
            cur.clear();
        } else {
            cur.clear();
        }
    };
    while !rest.is_empty() {
        if let Some((tt, after)) = rest.strip_prefix('<').and_then(|r| r.split_once('>')) {
            if let Some(nt) = time(tt) {
                let nt = nt - offset;
                // A time right after a word, before a space, is its end.
                let ends = !cur.ends_with(' ') && (after.is_empty() || after.starts_with(' '));
                flush(&mut pieces, &mut cur, t, ends.then_some(nt));
                t = nt;
                rest = after;
                continue;
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        cur.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut pieces, &mut cur, t, None);
    pieces
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_times_and_voices() {
        let s = parse(
            "[ti:Song]\n[ar:Someone]\n[v:1]\n[00:01.00]<00:01.00>Hel<00:01.40>lo <00:02.00>world<00:02.80>\n\
             [v:2]\n[00:04.00]Anne: <00:04.00>yes <00:04.50>we <00:05.00>can\n",
        );
        assert_eq!(s.title, "Song");
        assert_eq!(s.artist, "Someone");
        assert!(s.word_timed);
        assert_eq!(s.lines.len(), 2);
        let l = &s.lines[0];
        assert_eq!(l.voice, 1);
        let texts: Vec<&str> = l.syllables.iter().map(|p| p.text.as_str()).collect();
        assert_eq!(texts, ["Hel", "lo ", "world"]);
        assert!((l.syllables[1].start - 1.4).abs() < 1e-9);
        assert!((l.syllables[2].end - 2.8).abs() < 1e-9);
        let l = &s.lines[1];
        assert_eq!(l.voice, 2);
        assert_eq!(l.syllables[0].text, "yes ");
        assert!((l.syllables[0].start - 4.0).abs() < 1e-9);
    }

    #[test]
    fn line_times_spread_words() {
        let s = parse("[00:10.00]one two three\n[00:11.00]four\n");
        assert!(!s.word_timed);
        let l = &s.lines[0];
        assert_eq!(l.syllables.len(), 3);
        // A third of a second each until the next line.
        assert!((l.syllables[1].start - 10.333).abs() < 0.01);
        assert!(l.syllables[2].end <= 11.0 + 1e-9);
        assert_eq!(s.text(), "one two three\nfour\n");
    }

    #[test]
    fn empty_end_times_are_ignored() {
        let s = parse("[00:01.00]<00:01.00>go <00:01.50>show<00:01.50>\n[00:05.00]next\n");
        let l = &s.lines[0];
        // To the line's end: three seconds after its last word at most.
        assert!((l.syllables[1].end - 4.5).abs() < 1e-9, "{:?}", l.syllables[1]);
    }

    #[test]
    fn plain_text_has_no_lines() {
        assert!(parse("just a text\nwithout times\n").lines.is_empty());
    }

    #[test]
    fn offset_and_repeated_times() {
        let s = parse("[offset:500]\n[00:02.00][00:12.00]la la\n");
        assert_eq!(s.lines.len(), 2);
        assert!((s.lines[0].syllables[0].start - 1.5).abs() < 1e-9);
        assert!((s.lines[1].syllables[0].start - 11.5).abs() < 1e-9);
    }
}
