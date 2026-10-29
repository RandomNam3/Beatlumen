//! Timing a song text with recognised segments: the recognised words get
//! times within their segment (by position), the song text's words are
//! matched to them by a sequence alignment with fuzzy word comparison,
//! lines take the time of their first matched word (or are interpolated),
//! and long gaps between sung parts become pause lines.

use crate::transcribe::Segment;

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

/// Recognised words with estimated start times.
fn timed_words(segments: &[Segment]) -> Vec<(String, f64)> {
    let mut out = Vec::new();
    for s in segments {
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

/// Pairs (song text word index, recognised word index) of an alignment.
fn align_words(text: &[String], rec: &[String]) -> Vec<(usize, usize)> {
    let (n, m) = (text.len(), rec.len());
    const GAP: f32 = -0.4;
    let mut score = vec![0f32; (n + 1) * (m + 1)];
    let at = |i: usize, j: usize| i * (m + 1) + j;
    for i in 1..=n {
        score[at(i, 0)] = score[at(i - 1, 0)] + GAP;
    }
    for j in 1..=m {
        score[at(0, j)] = score[at(0, j - 1)] + GAP;
    }
    for i in 1..=n {
        for j in 1..=m {
            let sim = similarity(&text[i - 1], &rec[j - 1]);
            let pair = if sim >= 0.6 { 2.0 * sim } else { -1.0 };
            score[at(i, j)] = (score[at(i - 1, j - 1)] + pair)
                .max(score[at(i - 1, j)] + GAP)
                .max(score[at(i, j - 1)] + GAP);
        }
    }
    let mut pairs = Vec::new();
    let (mut i, mut j) = (n, m);
    while i > 0 && j > 0 {
        let sim = similarity(&text[i - 1], &rec[j - 1]);
        let pair = if sim >= 0.6 { 2.0 * sim } else { -1.0 };
        if (score[at(i, j)] - (score[at(i - 1, j - 1)] + pair)).abs() < 1e-4 {
            if sim >= 0.6 {
                pairs.push((i - 1, j - 1));
            }
            i -= 1;
            j -= 1;
        } else if (score[at(i, j)] - (score[at(i - 1, j)] + GAP)).abs() < 1e-4 {
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

/// LRC text of recognised segments (no song text known).
pub fn segments_to_lrc(segments: &[Segment]) -> String {
    let mut lines: Vec<(f64, String)> = segments
        .iter()
        .map(|s| (s.start, s.text.trim().to_string()))
        .collect();
    lines.extend(pauses(segments).into_iter().map(|t| (t, String::new())));
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
    let lines: Vec<&str> = text.lines().map(str::trim).collect();
    let mut words = Vec::new();
    let mut word_line = Vec::new();
    for (li, l) in lines.iter().enumerate() {
        for w in l.split_whitespace() {
            let n = norm(w);
            if !n.is_empty() {
                words.push(n);
                word_line.push(li);
            }
        }
    }
    let rec = timed_words(segments);
    let rec_words: Vec<String> = rec.iter().map(|(w, _)| w.clone()).collect();
    let pairs = align_words(&words, &rec_words);
    let matched = pairs.len() as f32 / words.len().max(1) as f32;
    // First matched time per line.
    let mut line_time: Vec<Option<f64>> = vec![None; lines.len()];
    for &(wi, ri) in &pairs {
        let li = word_line[wi];
        if line_time[li].is_none() {
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
    // Interpolate lines without a match (by word count) between neighbours.
    let counts: Vec<usize> = lines
        .iter()
        .map(|l| l.split_whitespace().count().max(1))
        .collect();
    let song_end = segments.last().map_or(0.0, |s| s.end);
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
    let mut out: Vec<(f64, String)> = Vec::new();
    for (l, t) in lines.iter().zip(&line_time) {
        if l.is_empty() {
            continue;
        }
        if let Some(t) = t {
            out.push((*t, l.to_string()));
        }
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

    fn seg(start: f64, end: f64, text: &str) -> Segment {
        Segment {
            start,
            end,
            text: text.into(),
            avg_logprob: -0.3,
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
    fn segments_only() {
        let lrc = segments_to_lrc(&[seg(1.0, 3.0, " Hi there"), seg(20.0, 22.0, "again")]);
        assert_eq!(lrc, "[00:01.00]Hi there\n[00:03.00]\n[00:20.00]again\n");
    }
}
