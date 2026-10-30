//! Timing a song's text in one go, as the server and the apps do it:
//!
//! - an LRC text with line times: each line's words are timed against
//!   the audio (the line times stay);
//! - a plain text: the singing is recognised first (steered by the text)
//!   to find when each line is sung, then the words are timed;
//! - no text: what is sung is written down, with word times.
//!
//! The result is enhanced LRC: a time before each word (and before the
//! pieces of long notes) and after a word that is followed by a pause.
//! Its `[re:]` tag names this timing's version and the model, so that a
//! run over a whole library skips what the same or a better model did.

use crate::align::{align_text, segments_to_lrc_words, words_line};
use crate::model::Model;
use crate::transcribe::{Line, Options, Transcriber};

/// Share of a plain text's words that must be recognised.
pub const MIN_MATCH: f32 = 0.25;
/// Mean token log probability below which written-down words are not
/// trusted.
const MIN_CONFIDENCE: f32 = -1.2;
/// A line runs at most this long (seconds) when nothing follows it.
const LAST_LINE: f64 = 12.0;
/// Version of the timing (raised when it gets better, so older results
/// are made again).
pub const VERSION: u32 = 2;
const CREATOR: &str = "Beatlumen speech timing";

/// The `[re:]` line naming the timing and the model (`model_id`).
pub fn creator_tag(model_id: &str) -> String {
    format!("[re:{CREATOR} {VERSION}, model {model_id}]\n")
}

/// The timing version and model id a text was made with (from its
/// `[re:]` tag), when it was made by this timing.
pub fn made_with(lrc: &str) -> Option<(u32, String)> {
    lrc.lines().find_map(|l| {
        let rest = l.trim().strip_prefix("[re:")?.strip_suffix(']')?;
        let rest = rest.strip_prefix(CREATOR)?.trim_start();
        let (version, model) = rest.split_once(',')?;
        let model = model.trim().strip_prefix("model")?.trim();
        Some((version.trim().parse().ok()?, model.to_string()))
    })
}

/// Whether an LRC text has word times (`<mm:ss.xx>`).
pub fn has_word_times(lrc: &str) -> bool {
    lrc.lines().any(|l| {
        l.match_indices('<').any(|(i, _)| {
            l[i + 1..]
                .split_once('>')
                .is_some_and(|(t, _)| parse_time(t).is_some())
        })
    })
}

/// A timed song text.
pub struct Timed {
    /// Enhanced LRC.
    pub lrc: String,
    /// The words were written down from the singing (no text was given).
    pub written_down: bool,
}

/// Times `text` (or writes down what is sung) for `samples` (mono,
/// 16 kHz) with `model` (`model_id` is written into the result).
/// `progress` gets 0..1 and cancels with false.
pub fn time_song(
    model: &Model,
    model_id: &str,
    samples: &[f32],
    text: Option<&str>,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<Timed, String> {
    let t = Transcriber::new(model);
    let song_end = samples.len() as f64 / crate::mel::SAMPLE_RATE as f64;
    let text = text.map(str::trim).filter(|t| !t.is_empty());
    if let Some(lrc) = text.and_then(lrc_lines) {
        let lrc_text = text.unwrap_or_default();
        let head = creator_tag(model_id) + &header(lrc_text);
        return time_lines(&t, samples, &lrc, head, song_end, progress).map(|lrc| Timed {
            lrc,
            written_down: false,
        });
    }
    let prompt = text.map(|t| {
        let words: Vec<&str> = t.split_whitespace().collect();
        words[words.len().saturating_sub(120)..].join(" ")
    });
    let share = 0.6;
    let mut first = |p: f32| progress(p * share);
    let segments = t
        .transcribe(
            samples,
            Options {
                language: None,
                prompt: prompt.as_deref(),
                progress: &mut first,
            },
        )
        .map_err(|e| e.to_string())?;
    if segments.is_empty() {
        return Err("no singing recognised".into());
    }
    match text {
        Some(text) => {
            let (lines, matched) = align_text(text, &segments);
            if matched < MIN_MATCH {
                return Err(format!(
                    "the song text was not recognised ({:.0} % of the words)",
                    matched * 100.0
                ));
            }
            let lines = lrc_lines(&lines).unwrap_or_default();
            let mut rest = |p: f32| progress(share + p * (1.0 - share));
            time_lines(
                &t,
                samples,
                &lines,
                creator_tag(model_id),
                song_end,
                &mut rest,
            )
            .map(|lrc| Timed {
                lrc,
                written_down: false,
            })
        }
        None => {
            let conf = segments.iter().map(|s| s.avg_logprob).sum::<f32>() / segments.len() as f32;
            if conf < MIN_CONFIDENCE {
                return Err("the words could not be recognised reliably".into());
            }
            // The written-down lines are timed like a given text (word
            // times and voices).
            let lines = lrc_lines(&segments_to_lrc_words(&segments)).unwrap_or_default();
            let mut rest = |p: f32| progress(share + p * (1.0 - share));
            time_lines(
                &t,
                samples,
                &lines,
                creator_tag(model_id),
                song_end,
                &mut rest,
            )
            .map(|lrc| Timed {
                lrc,
                written_down: true,
            })
        }
    }
}

/// Times the words of LRC lines and writes them back (line times kept).
fn time_lines(
    t: &Transcriber,
    samples: &[f32],
    lines: &[(f64, String)],
    header: String,
    song_end: f64,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<String, String> {
    let timed: Vec<Line> = lines
        .iter()
        .enumerate()
        .map(|(i, (start, text))| {
            let next = lines[i + 1..]
                .iter()
                .map(|(t, _)| *t)
                .find(|t| *t > *start)
                .unwrap_or(song_end.max(*start + 1.0));
            Line {
                start: *start,
                end: next.min(start + LAST_LINE).max(*start + 0.2),
                text: text.clone(),
            }
        })
        .collect();
    let words = t
        .time_lines(samples, &timed, None, progress)
        .map_err(|e| e.to_string())?;
    let mut out = header;
    for (l, w) in timed.iter().zip(words) {
        out.push_str(&lrc_time(l.start));
        if w.is_empty() {
            out.push_str(l.text.trim());
        } else {
            out.push_str(&words_line(&w, l.end));
        }
        out.push('\n');
    }
    Ok(out)
}

fn lrc_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("[{:02}:{:02}.{:02}]", cs / 6000, cs / 100 % 60, cs % 100)
}

/// "mm:ss.xx" (or "mm:ss") in seconds.
fn parse_time(s: &str) -> Option<f64> {
    let (m, rest) = s.split_once(':')?;
    let m: f64 = m.trim().parse().ok()?;
    let sec: f64 = rest.trim().parse().ok()?;
    (m >= 0.0 && (0.0..60.0).contains(&sec)).then_some(m * 60.0 + sec)
}

/// The lines of an LRC text with their times (word times and other tags
/// removed, a line with several times repeated, `[offset:]` applied),
/// sorted by time; None when fewer than two lines have times.
pub fn lrc_lines(text: &str) -> Option<Vec<(f64, String)>> {
    let mut offset = 0.0;
    let mut out: Vec<(f64, String)> = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim();
        let mut times = Vec::new();
        while let Some(r) = rest.strip_prefix('[') {
            let Some(end) = r.find(']') else { break };
            let tag = &r[..end];
            if let Some(t) = parse_time(tag) {
                times.push(t);
            } else if let Some(o) = tag.strip_prefix("offset:") {
                offset = o.trim().parse::<f64>().unwrap_or(0.0) / 1000.0;
            }
            rest = &r[end + 1..];
        }
        let words = strip_word_times(rest);
        out.extend(times.into_iter().map(|t| (t, words.clone())));
    }
    if out.len() < 2 {
        return None;
    }
    for l in &mut out {
        l.0 = (l.0 - offset).max(0.0);
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    Some(out)
}

/// An enhanced LRC line's text without its `<mm:ss.xx>` word times.
fn strip_word_times(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('<') {
        out.push_str(&rest[..i]);
        let r = &rest[i + 1..];
        match r.find('>') {
            Some(end) if parse_time(&r[..end]).is_some() => rest = &r[end + 1..],
            _ => {
                out.push('<');
                rest = r;
            }
        }
    }
    out.push_str(rest);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The tag lines of an LRC text (title, artist, …) to keep at its top;
/// `[offset:]` is left out (its shift is applied to the times).
fn header(text: &str) -> String {
    text.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with('[')
                && l.ends_with(']')
                && l[1..].split(':').next().is_some_and(|k| {
                    !k.is_empty() && k.chars().all(|c| c.is_ascii_alphabetic()) && k != "offset"
                })
                && made_with(l).is_none()
        })
        .map(|l| format!("{l}\n"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lrc_lines_are_read() {
        let text = "[ti:Friends]\n[offset:+500]\n[00:10.50]<00:10.50>You <00:11.00>say\n\
                    [00:20.00][01:00.00]Haven't I made it obvious?\n[00:15.00]\n";
        let l = lrc_lines(text).unwrap();
        assert_eq!(
            l,
            vec![
                (10.0, "You say".to_string()),
                (14.5, String::new()),
                (19.5, "Haven't I made it obvious?".to_string()),
                (59.5, "Haven't I made it obvious?".to_string()),
            ]
        );
        assert_eq!(header(text), "[ti:Friends]\n");
        assert!(lrc_lines("just a text\nwith lines").is_none());
        assert_eq!(strip_word_times("a <b> c"), "a <b> c");
        let tag = creator_tag("small-q5_1");
        assert_eq!(made_with(&tag), Some((VERSION, "small-q5_1".to_string())));
        assert_eq!(
            header(&(tag + "[ar:X]\n")),
            "[ar:X]\n",
            "an old tag is replaced"
        );
        assert!(has_word_times("[00:01.00]<00:01.00>a <00:01.50>b"));
        assert!(!has_word_times("[00:01.00]a <b>"));
    }
}
