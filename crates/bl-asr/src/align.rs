//! Timing a song text with recognised segments: the recognised words get
//! times within their segment (by position), the song text's words are
//! matched to them by a sequence alignment with fuzzy word comparison,
//! lines take the time of their first matched word (or are interpolated),
//! and long gaps between sung parts become pause lines.

use crate::transcribe::{Segment, Word};

/// A gap without singing of at least this length becomes a pause line.
pub const PAUSE_SECS: f64 = 8.0;

/// Lowercase letters and digits of a word (other characters dropped).
fn norm(w: &str) -> String {
    w.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn similarity(a: &str, b: &str) -> f32 {
    if a == b {
        return 1.0;
    }
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let n = a.len().max(b.len());
    if n == 0 {
        return 0.0;
    }
    // Levenshtein distance.
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    1.0 - prev[b.len()] as f32 / n as f32
}

/// Recognised words with their start times (estimated by position in
/// the segment when the recognition gave none).
fn timed_words(segments: &[Segment]) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for s in segments {
        if !s.words.is_empty() {
            out.extend(
                s.words
                    .iter()
                    .map(|w| (norm(&w.text), w.start))
                    .filter(|(n, _)| !n.is_empty()),
            );
            continue;
        }
        let words: Vec<&str> = s.text.split_whitespace().collect();
        let total: usize = words
            .iter()
            .map(|w| w.chars().count() + 1)
            .sum::<usize>()
            .max(1);
        let mut pos = 0usize;
        for w in words {
            let t = s.start + (s.end - s.start) * pos as f64 / total as f64;
            pos += w.chars().count() + 1;
            let n = norm(w);
            if !n.is_empty() {
                out.push((n, t));
            }
        }
    }
    out
}

/// Word times of a line from `start` to `end`: known times stay (kept in
/// order), the others are spread by character position between them.
fn spread(words: &[&str], known: &[Option<f64>], start: f64, end: f64) -> Vec<f64> {
    let pos: Vec<f64> = words
        .iter()
        .scan(0usize, |p, w| {
            let at = *p;
            *p += w.chars().count() + 1;
            Some(at as f64)
        })
        .collect();
    let total = words
        .iter()
        .map(|w| w.chars().count() + 1)
        .sum::<usize>()
        .max(1) as f64;
    // Anchors: the line's start and end and the known words in order.
    let mut anchors: Vec<(f64, f64)> = vec![(0.0, start)];
    let mut last = start;
    for (i, k) in known.iter().enumerate() {
        if let Some(t) = k {
            if *t >= last && *t < end {
                anchors.push((pos[i], *t));
                last = *t;
            }
        }
    }
    anchors.push((total, end));
    pos.iter()
        .map(|&p| {
            let j = anchors.iter().rposition(|a| a.0 <= p).unwrap_or(0);
            let (p0, t0) = anchors[j];
            let (p1, t1) = anchors.get(j + 1).copied().unwrap_or((total, end));
            if p1 <= p0 {
                t0
            } else {
                t0 + (t1 - t0) * (p - p0) / (p1 - p0)
            }
        })
        .collect()
}

/// Pairs (song text word index, recognised word index) of an alignment.
///
/// A match that continues a run of matches scores more, so a phrase is
/// matched as a whole: song texts repeat their lines (choruses), and
/// without this the words of one line could be matched to scattered
/// words of several repetitions.
fn align_words(text: &[String], rec: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (text.len(), rec.len());
    const GAP: f32 = -0.4;
    const MISMATCH: f32 = -1.0;
    const RUN: f32 = 0.6;
    let w = m + 1;
    let at = |i: usize, j: usize| i * w + j;
    let sim = |i: usize, j: usize| {
        let s = similarity(&text[i - 1], &rec[j - 1]);
        (s >= 0.6).then_some(2.0 * s)
    };
    // best: the best score of the first i and j words; run: the best that
    // ends with text[i - 1] matched to rec[j - 1].
    let mut best = vec![0f32; (n + 1) * w];
    let mut run = vec![f32::NEG_INFINITY; (n + 1) * w];
    for i in 1..=n {
        best[at(i, 0)] = best[at(i - 1, 0)] + GAP;
    }
    for j in 1..=m {
        best[at(0, j)] = best[at(0, j - 1)] + GAP;
    }
    for i in 1..=n {
        for j in 1..=m {
            if let Some(pair) = sim(i, j) {
                run[at(i, j)] = pair + best[at(i - 1, j - 1)].max(run[at(i - 1, j - 1)] + RUN);
            }
            best[at(i, j)] = run[at(i, j)]
                .max(best[at(i - 1, j - 1)] + MISMATCH)
                .max(best[at(i - 1, j)] + GAP)
                .max(best[at(i, j - 1)] + GAP);
        }
    }
    // Back from the end; `in_run`: the path continues a run at (i, j).
    let close = |a: f32, b: f32| (a - b).abs() <= 1e-3 * (1.0 + a.abs());
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    let mut in_run = false;
    while i > 0 && j > 0 {
        let here = if in_run {
            run[at(i, j)]
        } else {
            best[at(i, j)]
        };
        if in_run || close(here, run[at(i, j)]) {
            pairs.push((i - 1, j - 1));
            let pair = sim(i, j).unwrap_or(0.0);
            in_run = run[at(i - 1, j - 1)].is_finite()
                && close(here, pair + run[at(i - 1, j - 1)] + RUN)
                && !close(here, pair + best[at(i - 1, j - 1)]);
            i -= 1;
            j -= 1;
        } else if close(here, best[at(i - 1, j - 1)] + MISMATCH) {
            i -= 1;
            j -= 1;
        } else if close(here, best[at(i - 1, j)] + GAP) {
            i -= 1;
        } else {
            j -= 1;
        }
    }
    pairs.reverse();
    pairs
}

fn lrc_time(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("[{:02}:{:02}.{:02}]", cs / 6000, cs / 100 % 60, cs % 100)
}

/// Pause lines for long gaps between segments (after `from` seconds).
fn pauses(segments: &[Segment]) -> Vec<f64> {
    segments
        .windows(2)
        .filter(|w| w[1].start - w[0].end >= PAUSE_SECS)
        .map(|w| w[0].end)
        .collect()
}

/// Pause lines (their times) for long gaps in the singing before the
/// next of `line_times`.
pub fn pause_lines(segments: &[Segment], line_times: &[f64]) -> Vec<f64> {
    pauses(&speech_segments(segments))
        .into_iter()
        .filter(|p| {
            line_times
                .iter()
                .find(|t| **t > *p)
                .is_some_and(|n| n - p >= PAUSE_SECS)
        })
        .collect()
}

/// A recognised line gets broken at punctuation after this many characters
/// (and anywhere after twice as many).
const LINE_CHARS: usize = 32;
/// A pause between words of at least this long (seconds) ends a line.
const LINE_PAUSE: f64 = 0.45;

/// Where a text is inside what the model writes for sounds (`[applause]`,
/// `(music)`, `*upbeat music*`), carried from word to word.
#[derive(Default)]
struct Annotation {
    depth: usize,
    star: bool,
}

impl Annotation {
    /// The characters of `text` outside annotations.
    fn strip(&mut self, text: &str) -> String {
        let mut out = String::new();
        for c in text.chars() {
            match c {
                '[' | '(' => self.depth += 1,
                ']' | ')' => self.depth = self.depth.saturating_sub(1),
                '*' => self.star = !self.star,
                '♪' | '♫' => {}
                c if self.depth == 0 && !self.star => out.push(c),
                _ => {}
            }
        }
        out
    }
}

/// Text without what the model writes for sounds (`[applause]`,
/// `(music)`, `*upbeat music*`, `♪`, `[BLANK_AUDIO]`).
fn strip_annotations(text: &str) -> String {
    let out = Annotation::default().strip(text);
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The segments with speech (sounds removed). A short segment that only
/// repeats the end of the one before is dropped (models echo the last
/// words into the silence after them).
fn speech_segments(segments: &[Segment]) -> Vec<Segment> {
    let mut out: Vec<Segment> = Vec::new();
    for s in segments {
        let text = strip_annotations(&s.text);
        if text.is_empty() {
            continue;
        }
        let words: Vec<String> = text.split_whitespace().map(norm).collect();
        let echo = out.last().is_some_and(|p| {
            let prev: Vec<String> = p.text.split_whitespace().map(norm).collect();
            words.len() <= 3 && prev.len() > words.len() && prev.ends_with(&words)
        });
        if !echo {
            let mut inside = Annotation::default();
            let words = s
                .words
                .iter()
                .filter(|w| !inside.strip(&w.text).trim().is_empty())
                .cloned()
                .collect();
            out.push(Segment {
                text,
                words,
                ..s.clone()
            });
        }
    }
    out
}

/// A segment's words with times (estimated by character position when
/// the recognition gave none).
fn segment_words(s: &Segment) -> Vec<Word> {
    if !s.words.is_empty() {
        return s.words.clone();
    }
    let words: Vec<&str> = s.text.split_whitespace().collect();
    let total: usize = words
        .iter()
        .map(|w| w.chars().count() + 1)
        .sum::<usize>()
        .max(1);
    let at = |pos: usize| s.start + (s.end - s.start) * pos as f64 / total as f64;
    let mut pos = 0usize;
    words
        .iter()
        .map(|w| {
            let start = at(pos);
            pos += w.chars().count() + 1;
            Word {
                text: w.to_string(),
                start,
                end: at(pos - 1),
                pieces: Vec::new(),
            }
        })
        .collect()
}

/// A segment's words as lines of readable length.
fn split_lines(s: &Segment) -> Vec<Vec<Word>> {
    let words = segment_words(s);
    let mut lines = Vec::new();
    let mut line: Vec<Word> = Vec::new();
    let mut len = 0usize;
    for (i, w) in words.iter().enumerate() {
        len += w.text.chars().count() + usize::from(!line.is_empty());
        line.push(w.clone());
        let rest: usize = words[i + 1..]
            .iter()
            .map(|w| w.text.chars().count() + 1)
            .sum();
        let breaks = w.text.ends_with(['.', ',', '!', '?', ';', ':']);
        // Sung lines often come without punctuation: a pause after the word,
        // or a capital starting the next one, ends a line too.
        let (gap, capital) = words.get(i + 1).map_or((0.0, false), |n| {
            let first = n.text.split('\'').next().unwrap_or_default();
            let capital = n.text.chars().next().is_some_and(char::is_uppercase) && first != "I";
            (n.start - w.end, capital)
        });
        let pause = gap >= LINE_PAUSE && len >= LINE_CHARS / 4;
        // (Over music, words follow each other without gaps.)
        let new_line = capital && len >= LINE_CHARS * 3 / 4;
        if i + 1 < words.len()
            && rest >= LINE_CHARS / 4
            && (pause || new_line || (breaks && len >= LINE_CHARS) || len >= 2 * LINE_CHARS)
        {
            lines.push(std::mem::take(&mut line));
            len = 0;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    // Without punctuation and capitals (some models write so) the rules
    // above find few breaks: long lines are split where the singer
    // breathes, and the lines get capitals.
    let lower = !s.text.chars().any(char::is_uppercase);
    lines
        .into_iter()
        .flat_map(split_long)
        .map(|l| if lower { capitalized(l) } else { l })
        .collect()
}

/// Longest line before it is split at its best break.
const LONG_LINE: usize = 48;

fn line_len(words: &[Word]) -> usize {
    words
        .iter()
        .map(|w| w.text.chars().count() + 1)
        .sum::<usize>()
        .saturating_sub(1)
}

/// A line longer than [`LONG_LINE`] split (again and again) at the break
/// with the longest pause, preferring punctuation and the middle, so that
/// both parts keep a few words.
fn split_long(words: Vec<Word>) -> Vec<Vec<Word>> {
    if line_len(&words) <= LONG_LINE || words.len() < 4 {
        return vec![words];
    }
    let total = line_len(&words);
    let mut best: Option<(f64, usize)> = None;
    let mut left = 0usize;
    for i in 0..words.len() - 1 {
        left += words[i].text.chars().count() + usize::from(i > 0);
        let right = total - left - 1;
        if left < LINE_CHARS / 2 || right < LINE_CHARS / 4 || left > LONG_LINE {
            continue;
        }
        let (w, n) = (&words[i], &words[i + 1]);
        let mut score = (n.start - w.end).max(0.0);
        if w.text.ends_with([',', '.', '!', '?', ';', ':']) {
            score += 0.5;
        }
        // Balanced lines read better.
        score -= (left as f64 - total as f64 / 2.0).abs() / total as f64 * 0.2;
        if best.map_or(true, |(b, _)| score > b) {
            best = Some((score, i + 1));
        }
    }
    let Some((_, at)) = best else {
        return vec![words];
    };
    let mut words = words;
    let rest = words.split_off(at);
    let mut out = split_long(words);
    out.extend(split_long(rest));
    out
}

/// A line of a text written without capitals: the first word and "I"
/// (with "I'm", "I'll", …) capitalised, spelled words ("f-r-i-e-n-d-s")
/// in capitals.
fn capitalized(mut words: Vec<Word>) -> Vec<Word> {
    for (k, w) in words.iter_mut().enumerate() {
        let lower = w.text.to_lowercase();
        let spelled = is_spelled(&w.text);
        let first_person = lower == "i" || lower.starts_with("i'");
        let text = if spelled {
            w.text.to_uppercase()
        } else if k == 0 || first_person {
            let mut c = w.text.chars();
            c.next()
                .map_or_else(String::new, |f| f.to_uppercase().chain(c).collect())
        } else {
            continue;
        };
        set_text(w, text);
    }
    words
}

/// A word written letter by letter ("F-R-I-E-N-D-S", punctuation around it
/// allowed).
fn is_spelled(word: &str) -> bool {
    let core = word.trim_matches(|c: char| !c.is_alphanumeric());
    let parts: Vec<&str> = core.split('-').collect();
    parts.len() >= 3
        && parts
            .iter()
            .all(|p| p.chars().count() == 1 && p.chars().all(char::is_alphanumeric))
}

/// A word's text changed: its pieces follow when they still add up, else
/// the word becomes one piece.
fn set_text(w: &mut Word, text: String) {
    let joined: String = w.pieces.iter().map(|(_, p)| p.as_str()).collect();
    if !w.pieces.is_empty() && joined.to_lowercase() == text.to_lowercase() {
        let mut rest = text.as_str();
        for (_, p) in &mut w.pieces {
            let n = p.len().min(rest.len());
            let (head, tail) = rest.split_at(n);
            *p = head.to_string();
            rest = tail;
        }
    } else if joined != text {
        w.pieces.clear();
    }
    w.text = text;
}

/// Spelled words that are a near miss of a word of the song's title (the
/// model heard "F-R-I-N-D-S" for "F-R-I-E-N-D-S") are written like the
/// title. Returns how many were changed.
pub fn spell_like_title(segments: &mut [Segment], title: &str) -> usize {
    let names: Vec<String> = title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_uppercase)
        .collect();
    let fix = |word: &str| -> Option<String> {
        if !is_spelled(word) {
            return None;
        }
        let letters: String = word
            .chars()
            .filter(|c| c.is_alphanumeric())
            .collect::<String>()
            .to_uppercase();
        // Only a near miss: letters missing or misheard, not added (a
        // longer word, "L-O-V-E-D" for a title "Love", is another word),
        // one letter off for short title words, two for long ones.
        let name = names.iter().find(|n| {
            let len = n.chars().count();
            let allowed = if len >= 7 { 2 } else { 1 };
            let edits = ((1.0 - similarity(n, &letters)) * len.max(letters.chars().count()) as f32)
                .round() as usize;
            len >= 4 && *n != &letters && letters.chars().count() <= len && edits <= allowed
        })?;
        let spelled = name.chars().map(String::from).collect::<Vec<_>>().join("-");
        let lead: String = word.chars().take_while(|c| !c.is_alphanumeric()).collect();
        let trail: String = word
            .chars()
            .rev()
            .take_while(|c| !c.is_alphanumeric())
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        let lower = word.chars().any(char::is_lowercase);
        Some(format!(
            "{lead}{}{trail}",
            if lower {
                spelled.to_lowercase()
            } else {
                spelled
            }
        ))
    };
    let mut changed = 0;
    for s in segments.iter_mut() {
        let text: Vec<String> = s
            .text
            .split(' ')
            .map(|w| {
                fix(w)
                    .inspect(|_| changed += 1)
                    .unwrap_or_else(|| w.to_string())
            })
            .collect();
        s.text = text.join(" ");
        for w in &mut s.words {
            if let Some(t) = fix(&w.text) {
                set_text(w, t);
            }
        }
    }
    changed
}

fn word_tag(t: f64) -> String {
    let cs = (t.max(0.0) * 100.0).round() as u64;
    format!("<{:02}:{:02}.{:02}>", cs / 6000, cs / 100 % 60, cs % 100)
}

/// Words held at least this long are written with the times of their
/// pieces (syllable-like), so a display can follow a long note.
const PIECES_FROM: f64 = 0.5;
/// A pause after a word of at least this long gets the word's end time.
const END_GAP: f64 = 0.25;

/// A line of timed words (enhanced LRC): the start time before each word
/// (and before each piece of a long word), the end time right after a
/// word that is followed by a pause and after the last one. `line_end`:
/// when the next line starts.
pub fn words_line(words: &[Word], line_end: f64) -> String {
    let mut out = String::new();
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        if w.pieces.len() > 1 && w.end - w.start >= PIECES_FROM {
            for (t, p) in &w.pieces {
                out.push_str(&word_tag(*t));
                out.push_str(p);
            }
        } else {
            out.push_str(&word_tag(w.start));
            out.push_str(&w.text);
        }
        let next = words.get(i + 1).map_or(line_end, |n| n.start);
        if i + 1 == words.len() || next - w.end >= END_GAP {
            out.push_str(&word_tag(w.end.max(w.start)));
        }
    }
    out
}

/// A line with a time before each word (enhanced LRC).
fn timed_line(words: &[&str], times: &[f64]) -> String {
    words
        .iter()
        .zip(times)
        .map(|(w, t)| format!("{}{w}", word_tag(*t)))
        .collect::<Vec<_>>()
        .join(" ")
}

/// LRC text of recognised segments (no song text known).
pub fn segments_to_lrc(segments: &[Segment]) -> String {
    segments_lrc(segments, false)
}

/// Like [`segments_to_lrc`] with the estimated time of each word
/// (enhanced LRC, `<mm:ss.xx>` before each word).
pub fn segments_to_lrc_words(segments: &[Segment]) -> String {
    segments_lrc(segments, true)
}

fn segments_lrc(segments: &[Segment], word_times: bool) -> String {
    let speech = speech_segments(segments);
    let split: Vec<Vec<Word>> = speech.iter().flat_map(split_lines).collect();
    let mut lines: Vec<(f64, String)> = split
        .iter()
        .enumerate()
        .map(|(i, words)| {
            let t = words.first().map_or(0.0, |w| w.start);
            if word_times {
                let end = split
                    .get(i + 1)
                    .and_then(|l| l.first())
                    .map_or(t + 10.0, |w| w.start);
                (t, words_line(words, end))
            } else {
                let text: Vec<&str> = words.iter().map(|w| w.text.as_str()).collect();
                (t, text.join(" "))
            }
        })
        .collect();
    lines.extend(pauses(&speech).into_iter().map(|t| (t, String::new())));
    // The end of the singing, when the song goes on.
    if let (Some(last), Some(end)) = (speech.last(), segments.last().map(|s| s.end)) {
        if end - last.end >= PAUSE_SECS {
            lines.push((last.end, String::new()));
        }
    }
    lines.sort_by(|a, b| a.0.total_cmp(&b.0));
    lines
        .into_iter()
        .map(|(t, l)| format!("{}{l}\n", lrc_time(t)))
        .collect()
}

/// Times the lines of a plain song text (`text`, one line per sung line;
/// empty lines are kept as spacing) with the recognised segments. Returns
/// LRC text and the share of words that matched (0..1).
pub fn align_text(text: &str, segments: &[Segment]) -> (String, f32) {
    align(text, segments, false)
}

/// Like [`align_text`] with a time for each word (enhanced LRC): matched
/// words get their recognised time, the others are spread between them.
pub fn align_text_words(text: &str, segments: &[Segment]) -> (String, f32) {
    align(text, segments, true)
}

/// For each word of a line (split at whitespace): whether it is in
/// parentheses.
fn in_parentheses(line: &str) -> Vec<bool> {
    let mut depth = 0usize;
    line.split_whitespace()
        .map(|w| {
            let mut inside = None;
            for c in w.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    c if c.is_alphanumeric() && inside.is_none() => inside = Some(depth > 0),
                    _ => {}
                }
            }
            inside.unwrap_or(depth > 0)
        })
        .collect()
}

/// A line of a song text with the time it starts at, found by matching
/// its words to recognised ones (`matched`), else estimated between the
/// matched lines around it.
#[derive(Debug, Clone, PartialEq)]
pub struct Anchor {
    pub time: f64,
    pub text: String,
    pub matched: bool,
}

/// The lines of a song text (empty ones left out) with their times, and
/// the share of words that matched.
pub fn anchor_lines(text: &str, segments: &[Segment]) -> (Vec<Anchor>, f32) {
    let song_end = segments.last().map_or(0.0, |s| s.end);
    let segments = &speech_segments(segments)[..];
    let t = line_times(text, segments, song_end);
    let anchors = t
        .lines
        .iter()
        .zip(&t.times)
        .zip(&t.found)
        .filter(|((l, _), _)| !l.is_empty())
        .filter_map(|((l, time), found)| {
            time.map(|time| Anchor {
                time,
                text: l.to_string(),
                matched: *found,
            })
        })
        .collect();
    (anchors, t.matched)
}

struct LineTimes<'a> {
    lines: Vec<&'a str>,
    times: Vec<Option<f64>>,
    /// Whether the line's time comes from a matched word.
    found: Vec<bool>,
    /// Recognised time per song text word (index over all words).
    word_time: std::collections::HashMap<usize, f64>,
    matched: f32,
}

fn line_times<'a>(text: &'a str, segments: &[Segment], song_end: f64) -> LineTimes<'a> {
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut words = Vec::new();
    let mut word_line = Vec::new();
    // Backing vocals (in parentheses) are mostly not recognised; matched,
    // their repetitions of the lead's words would take the lead's times.
    let mut lead = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        for (w, backing) in l.split_whitespace().zip(in_parentheses(l)) {
            let n = norm(w);
            if !n.is_empty() {
                if !backing {
                    lead.push(words.len());
                }
                words.push(n);
                word_line.push(li);
            }
        }
    }
    let rec = timed_words(segments);
    let rec_words: Vec<String> = rec.iter().map(|(w, _)| w.clone()).collect();
    let lead_words: Vec<String> = lead.iter().map(|&i| words[i].clone()).collect();
    let pairs: Vec<(usize, usize)> = align_words(&lead_words, &rec_words)
        .into_iter()
        .map(|(wi, ri)| (lead[wi], ri))
        .collect();
    let matched = pairs.len() as f32 / lead.len().max(1) as f32;
    // Per line the time of its first word that matched together with a
    // neighbour (two words in a row on both sides): a single word that
    // matched may well be a common one ("the", "who") of another line.
    let lead_pos: std::collections::HashMap<usize, usize> =
        lead.iter().enumerate().map(|(p, &w)| (w, p)).collect();
    let mut lead_count = vec![0usize; lines.len()];
    for &w in &lead {
        lead_count[word_line[w]] += 1;
    }
    let follows = |a: (usize, usize), b: (usize, usize)| {
        lead_pos[&b.0] == lead_pos[&a.0] + 1 && b.1 == a.1 + 1 && word_line[a.0] == word_line[b.0]
    };
    let mut line_time: Vec<Option<f64>> = vec![None; lines.len()];
    for (k, &(wi, ri)) in pairs.iter().enumerate() {
        let li = word_line[wi];
        let in_run = (k > 0 && follows(pairs[k - 1], pairs[k]))
            || pairs.get(k + 1).is_some_and(|&n| follows(pairs[k], n))
            || lead_count[li] == 1;
        if in_run && line_time[li].is_none() {
            line_time[li] = Some(rec[ri].1);
        }
    }
    // Times must increase.
    let mut last = f64::MIN;
    for t in line_time.iter_mut() {
        if let Some(v) = *t {
            if v < last {
                *t = None;
            } else {
                last = v;
            }
        }
    }
    let found: Vec<bool> = line_time.iter().map(Option::is_some).collect();
    // Interpolate lines without a match (by word count) between neighbours.
    let counts: Vec<usize> = lines
        .iter()
        .map(|l| l.split_whitespace().count().max(1))
        .collect();
    let mut i = 0;
    while i < lines.len() {
        if line_time[i].is_some() || lines[i].is_empty() {
            i += 1;
            continue;
        }
        let prev = (0..i).rev().find_map(|k| line_time[k].map(|t| (k, t)));
        let next = (i + 1..lines.len()).find_map(|k| line_time[k].map(|t| (k, t)));
        let (k0, t0) = prev.unwrap_or((0, segments.first().map_or(0.0, |s| s.start)));
        let (k1, t1) = next.unwrap_or((lines.len(), song_end));
        let total: usize = counts[k0..k1].iter().sum::<usize>().max(1);
        let before: usize = counts[k0..i].iter().sum();
        line_time[i] = Some(t0 + (t1 - t0) * before as f64 / total as f64);
        i += 1;
    }
    LineTimes {
        lines,
        times: line_time,
        found,
        word_time: pairs.iter().map(|&(wi, ri)| (wi, rec[ri].1)).collect(),
        matched,
    }
}

fn align(text: &str, segments: &[Segment], word_times: bool) -> (String, f32) {
    let song_end = segments.last().map_or(0.0, |s| s.end);
    let segments = &speech_segments(segments)[..];
    let LineTimes {
        lines,
        times: line_time,
        word_time: matched_time,
        matched,
        ..
    } = line_times(text, segments, song_end);
    let mut out: Vec<(f64, String)> = Vec::new();
    let mut wi = 0usize;
    for (li, (l, t)) in lines.iter().zip(&line_time).enumerate() {
        let raw: Vec<&str> = l.split_whitespace().collect();
        // The recognised times of this line's words.
        let mut times: Vec<Option<f64>> = Vec::with_capacity(raw.len());
        for w in &raw {
            if norm(w).is_empty() {
                times.push(None);
            } else {
                times.push(matched_time.get(&wi).copied());
                wi += 1;
            }
        }
        if l.is_empty() {
            continue;
        }
        let Some(t) = t else { continue };
        if !word_times {
            out.push((*t, l.to_string()));
            continue;
        }
        let end = line_time[li + 1..]
            .iter()
            .flatten()
            .copied()
            .find(|n| *n > *t)
            .unwrap_or(t + 4.0)
            .min(t + 10.0);
        out.push((*t, timed_line(&raw, &spread(&raw, &times, *t, end))));
    }
    // Pause lines where nothing is sung for a while.
    for p in pauses(segments) {
        let next_line = out.iter().find(|(t, _)| *t > p).map(|(t, _)| *t);
        if next_line.is_some_and(|n| n - p >= PAUSE_SECS) {
            out.push((p, String::new()));
        }
    }
    out.sort_by(|a, b| a.0.total_cmp(&b.0));
    (
        out.into_iter()
            .map(|(t, l)| format!("{}{l}\n", lrc_time(t)))
            .collect(),
        matched,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spelled_title_words_are_mended_only_when_near() {
        let mut segs = vec![seg(1.0, 5.0, "F-R-I-N-D-S, f-r-i-n-e-s L-O-V-E-D A-B-C")];
        assert_eq!(spell_like_title(&mut segs, "FRIENDS (Love)"), 2);
        assert_eq!(segs[0].text, "F-R-I-E-N-D-S, f-r-i-e-n-d-s L-O-V-E-D A-B-C");
        // Lines without capitals get them; long ones are split.
        let w = |t: &str, a: f64, b: f64| Word {
            text: t.into(),
            start: a,
            end: b,
            pieces: Vec::new(),
        };
        let line = vec![
            w("i", 0.0, 0.2),
            w("know", 0.2, 0.5),
            w("you", 0.5, 0.7),
            w("since", 0.7, 1.0),
            w("we", 1.0, 1.2),
            w("were", 1.2, 1.4),
            w("like", 1.4, 1.6),
            w("ten", 1.6, 1.9),
            w("yeah", 2.6, 2.9),
            w("don't", 2.9, 3.1),
            w("mess", 3.1, 3.3),
            w("it", 3.3, 3.4),
            w("up", 3.4, 3.6),
        ];
        let lines: Vec<String> = split_long(line)
            .into_iter()
            .map(capitalized)
            .map(|l| {
                l.iter()
                    .map(|w| w.text.as_str())
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect();
        assert_eq!(
            lines,
            ["I know you since we were like ten", "Yeah don't mess it up"]
        );
    }

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            avg_logprob: -0.3,
            words: Vec::new(),
        }
    }

    #[test]
    fn lines_get_times_of_their_words() {
        let segments = vec![
            seg(10.0, 14.0, "Hello darkness my old friend"),
            seg(14.0, 18.0, "I've come to talk with you again"),
            // A long instrumental gap, misheard words after it.
            seg(40.0, 44.0, "because a vision softly creping"),
        ];
        let text = "Hello darkness, my old friend\nI've come to talk with you again\n\nBecause a vision softly creeping";
        let (lrc, matched) = align_text(text, &segments);
        assert!(matched > 0.9, "{matched}");
        assert_eq!(
            lrc,
            "[00:10.00]Hello darkness, my old friend\n[00:14.00]I've come to talk with you again\n[00:18.00]\n[00:40.00]Because a vision softly creeping\n"
        );
    }

    #[test]
    fn unmatched_lines_are_interpolated() {
        let segments = vec![seg(0.0, 2.0, "one two"), seg(10.0, 12.0, "five six")];
        let (lrc, _) = align_text("one two\nthree four\nfive six", &segments);
        let times: Vec<&str> = lrc.lines().map(|l| &l[..10]).collect();
        assert_eq!(times, vec!["[00:00.00]", "[00:05.00]", "[00:10.00]"]);
    }

    #[test]
    fn sounds_are_dropped_and_long_segments_split() {
        let lrc = segments_to_lrc(&[
            seg(
                0.0,
                10.0,
                "And so my fellow Americans ask not what your country can do for you, ask what you can do for your country.",
            ),
            seg(10.5, 11.5, "[applause]"),
            seg(11.5, 40.0, ""),
            seg(40.5, 61.0, " [BLANK_AUDIO]"),
        ]);
        let lines: Vec<&str> = lrc.lines().collect();
        // Long lines are split near their middle (or at a comma), the
        // words keep their order.
        assert!(lines.iter().all(|l| l.len() <= 10 + LONG_LINE), "{lrc}");
        let text: Vec<&str> = lines
            .iter()
            .map(|l| &l[10..])
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            text.join(" "),
            "And so my fellow Americans ask not what your country can do for you, ask what you can do for your country.",
        );
        assert!(lines.iter().any(|l| l.ends_with("for you,")), "{lrc}");
        assert_eq!(lines.last(), Some(&"[00:10.00]"));
        assert!(!lrc.contains("applause") && !lrc.contains("BLANK"));
        // Sounds do not count as words of the text.
        let (lrc, matched) = align_text(
            "Hello there",
            &[seg(0.0, 2.0, "Hello there"), seg(2.0, 3.0, "(Applause)")],
        );
        assert_eq!(lrc, "[00:00.00]Hello there\n");
        assert_eq!(matched, 1.0);
    }

    #[test]
    fn echoed_endings_are_dropped() {
        let lrc = segments_to_lrc(&[
            seg(0.0, 10.0, "ask what you can do for your country."),
            seg(10.0, 12.0, " country."),
        ]);
        assert_eq!(lrc, "[00:00.00]Ask what you can do for your country.\n");
    }

    #[test]
    fn segments_only() {
        let lrc = segments_to_lrc(&[seg(1.0, 3.0, " Hi there"), seg(20.0, 22.0, "again")]);
        assert_eq!(lrc, "[00:01.00]Hi there\n[00:03.00]\n[00:20.00]Again\n");
    }

    #[test]
    fn word_times() {
        let segments = vec![seg(10.0, 14.0, "Hello darkness my old friend")];
        let (lrc, _) = align_text_words("Hello darkness, my old friend", &segments);
        assert!(
            lrc.starts_with("[00:10.00]<00:10.00>Hello <00:10."),
            "{lrc}"
        );
        let tags: Vec<&str> = lrc.matches('<').collect();
        assert_eq!(tags.len(), 5, "{lrc}");
        // Increasing.
        let times: Vec<f64> = lrc
            .split('<')
            .skip(1)
            .map(|p| {
                let t = &p[..8];
                t[..2].parse::<f64>().unwrap() * 60.0 + t[3..].parse::<f64>().unwrap()
            })
            .collect();
        assert!(times.windows(2).all(|w| w[0] <= w[1]), "{times:?}");
        assert!(*times.last().unwrap() < 14.0);
        let lrc = segments_to_lrc_words(&[seg(1.0, 3.0, "one two three")]);
        // (A line without capitals gets one.)
        assert!(lrc.starts_with("[00:01.00]<00:01.00>One <00:01."), "{lrc}");
    }
}
