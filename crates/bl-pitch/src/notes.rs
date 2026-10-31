//! One note per syllable from the melody.
//!
//! A syllable's note has the median pitch of its voiced frames. Where the
//! pitch moves away by more than a semitone and stays there for a while
//! (a melisma), the syllable goes on as an extra "~" note, as UltraStar
//! files write it. Syllables with too few voiced frames (spoken, rapped,
//! whispered) become freestyle notes. Single notes an octave away from
//! their neighbours (an octave error of the tracker) move back.

use crate::pyin::Frame;
use crate::track::{Grid, KaraokeLine, KaraokeTrack, Key, Note, NoteKind};

/// A timed syllable (seconds). A word's last syllable keeps the space
/// after it.
#[derive(Debug, Clone, PartialEq)]
pub struct Syllable {
    pub start: f64,
    pub end: f64,
    pub text: String,
}

/// A line of the song text: its syllables and who sings it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct LineIn {
    pub voice: u8,
    pub syllables: Vec<Syllable>,
}

/// The song's beats, to put notes on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BeatGrid {
    pub bpm: f64,
    /// Time of one beat (seconds).
    pub offset: f64,
    /// Grid steps per beat (4: sixteenth notes in 4/4).
    pub division: u32,
}

#[derive(Debug, Clone)]
pub struct NoteOptions {
    /// A pitch this far (semitones) from the note's…
    pub split_semitones: f32,
    /// …for this long (seconds) starts an extra note.
    pub split_min: f64,
    /// Share of a syllable's frames that must be voiced for a pitched
    /// note (else freestyle).
    pub min_voiced: f32,
    /// Silence longer than this (seconds) at a syllable's end is cut off
    /// its note.
    pub tighten: f64,
    /// Move single notes an octave away from their neighbours back.
    pub octave_fix: bool,
    /// Put the notes on this grid.
    pub grid: Option<BeatGrid>,
}

impl Default for NoteOptions {
    fn default() -> Self {
        NoteOptions {
            split_semitones: 1.0,
            split_min: 0.12,
            min_voiced: 0.3,
            tighten: 0.15,
            octave_fix: true,
            grid: None,
        }
    }
}

/// What [`build_track`] found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Report {
    /// Share of the frames within syllables that are voiced.
    pub voiced_share: f32,
    pub notes: usize,
    /// Extra "~" notes of melismas.
    pub extra_notes: usize,
    pub freestyle: usize,
    /// Notes moved by an octave.
    pub octave_fixes: usize,
}

/// A note while it is worked on: times in seconds, fractional pitch (NaN
/// when not known).
#[derive(Debug, Clone)]
struct Est {
    start: f64,
    end: f64,
    pitch: f32,
    kind: NoteKind,
    text: String,
    /// The first note of its syllable.
    first: bool,
}

fn median(v: &mut [f32]) -> f32 {
    if v.is_empty() {
        return f32::NAN;
    }
    v.sort_by(f32::total_cmp);
    let m = v.len() / 2;
    if v.len() % 2 == 1 {
        v[m]
    } else {
        0.5 * (v[m - 1] + v[m])
    }
}

/// Median of five around each value.
fn median5(x: &[f32]) -> Vec<f32> {
    (0..x.len())
        .map(|i| {
            let lo = i.saturating_sub(2);
            let hi = (i + 3).min(x.len());
            let mut w: Vec<f32> = x[lo..hi].to_vec();
            median(&mut w)
        })
        .collect()
}

/// The notes of one syllable; counts its frames into `frames_in` and
/// `voiced_in`.
fn syllable_notes(
    frames: &[Frame],
    hop: f64,
    s: &Syllable,
    opts: &NoteOptions,
    counts: &mut (usize, usize),
    out: &mut Vec<Est>,
) {
    let i0 = frames.partition_point(|f| f.time < s.start);
    let i1 = frames.partition_point(|f| f.time < s.end).max(i0);
    let sel = &frames[i0..i1];
    let voiced: Vec<(f64, f32)> = sel
        .iter()
        .filter_map(|f| f.midi().map(|m| (f.time, m)))
        .collect();
    counts.0 += sel.len();
    counts.1 += voiced.len();
    let share = voiced.len() as f32 / sel.len().max(1) as f32;
    if voiced.len() < 3 || share < opts.min_voiced {
        let mut m: Vec<f32> = voiced.iter().map(|v| v.1).collect();
        out.push(Est {
            start: s.start,
            end: s.end,
            pitch: median(&mut m),
            kind: NoteKind::Freestyle,
            text: s.text.clone(),
            first: true,
        });
        return;
    }
    let m = median5(&voiced.iter().map(|v| v.1).collect::<Vec<f32>>());
    let t = |k: usize| voiced[k].0;
    // Segments of steady pitch, as ranges of voiced frames.
    let mut segs: Vec<(usize, usize)> = Vec::new();
    let mut seg_start = 0;
    let mut run: Option<usize> = None;
    for k in 0..m.len() {
        let mut steady: Vec<f32> = m[seg_start..run.unwrap_or(k).max(seg_start + 1)].to_vec();
        let reference = median(&mut steady);
        if (m[k] - reference).abs() > opts.split_semitones {
            let r = match run {
                Some(r) if (m[k] - m[r]).abs() <= opts.split_semitones => r,
                _ => k,
            };
            run = Some(r);
            if t(k) - t(r) + hop >= opts.split_min {
                segs.push((seg_start, r));
                seg_start = r;
                run = None;
            }
        } else {
            run = None;
        }
    }
    segs.push((seg_start, m.len()));
    // Parts shorter than a split join the one before (the first one the
    // one after).
    let short = |&(a, b): &(usize, usize)| t(b - 1) - t(a) + hop < opts.split_min;
    let mut k = 0;
    while segs.len() > 1 && k < segs.len() {
        if short(&segs[k]) {
            if k == 0 {
                segs[1].0 = segs[0].0;
            } else {
                segs[k - 1].1 = segs[k].1;
            }
            segs.remove(k);
        } else {
            k += 1;
        }
    }
    // Pitch per segment; neighbours on the same semitone join.
    let mut parts: Vec<(usize, usize, f32)> = Vec::new();
    for (a, b) in segs {
        let mut v = m[a..b].to_vec();
        let p = median(&mut v);
        match parts.last_mut() {
            Some(last) if last.2.round() == p.round() => {
                last.1 = b;
                let mut v = m[last.0..b].to_vec();
                last.2 = median(&mut v);
            }
            _ => parts.push((a, b, p)),
        }
    }
    // Times: the syllable's start, then where each part's pitch begins;
    // silence at the end is cut off.
    let last_voiced = t(voiced.len() - 1) + hop;
    let end = if s.end - last_voiced > opts.tighten {
        last_voiced
    } else {
        s.end
    };
    let n = parts.len();
    let word = s.text.trim_end();
    let space = &s.text[word.len()..];
    for (i, &(a, _, p)) in parts.iter().enumerate() {
        let start = if i == 0 { s.start } else { t(a) - hop / 2.0 };
        let stop = parts.get(i + 1).map_or(end, |next| t(next.0) - hop / 2.0);
        let text = match (n, i) {
            (1, _) => s.text.clone(),
            (_, 0) => word.to_string(),
            (_, i) if i + 1 == n => format!("~{space}"),
            _ => "~".to_string(),
        };
        out.push(Est {
            start,
            end: stop.max(start),
            pitch: p,
            kind: NoteKind::Normal,
            text,
            first: i == 0,
        });
    }
}

/// Word times are rough: a syllable's last part on the next syllable's
/// pitch most likely is the next syllable started early, and a short
/// first part on the previous syllable's pitch the previous one going on.
/// Such parts move to the neighbour instead of being "~" notes.
fn fix_edges(est: &mut Vec<Est>, opts: &NoteOptions) {
    let same = |a: &Est, b: &Est| {
        a.kind.has_pitch() && b.kind.has_pitch() && (a.pitch - b.pitch).abs() < 0.75
    };
    let mut i = 1;
    while i < est.len() {
        // est[i] starts a syllable; est[i - 1] ends the one before.
        if !est[i].first || est[i].start - est[i - 1].end > 0.05 {
            i += 1;
            continue;
        }
        let (a, b) = (&est[i - 1], &est[i]);
        let trailing = !a.first && same(a, b) && a.end - a.start <= 2.5 * opts.split_min;
        let leading = !trailing
            && est.get(i + 1).is_some_and(|n| !n.first)
            && same(a, b)
            && b.end - b.start <= 1.5 * opts.split_min;
        if trailing {
            // The syllable before loses its last part (and keeps its space).
            let gone = est.remove(i - 1);
            let space = &gone.text[gone.text.trim_end().len()..];
            est[i - 1].start = gone.start;
            est[i - 2].text.push_str(space);
            continue;
        }
        if leading {
            let gone = est.remove(i);
            est[i - 1].end = est[i].start;
            let space = est[i].text[est[i].text.trim_end().len()..].to_string();
            est[i].text = gone.text.trim_end().to_string() + &space;
            est[i].first = true;
            continue;
        }
        i += 1;
    }
}

/// Moves pitched notes that sit one or two octaves away from the notes around them
/// (three before, three after) back; returns how many moved.
fn fix_octaves(notes: &mut [&mut Est]) -> usize {
    let pitched: Vec<usize> = (0..notes.len())
        .filter(|&i| notes[i].kind.has_pitch())
        .collect();
    let orig: Vec<f32> = pitched.iter().map(|&i| notes[i].pitch).collect();
    let mut moved = 0;
    for (k, &i) in pitched.iter().enumerate() {
        let lo = k.saturating_sub(3);
        let hi = (k + 4).min(pitched.len());
        let mut ctx: Vec<f32> = (lo..hi).filter(|&j| j != k).map(|j| orig[j]).collect();
        if ctx.len() < 2 {
            continue;
        }
        let c = median(&mut ctx);
        let p = orig[k];
        // One or two octaves (YIN on the second or fourth harmonic).
        if (p - c).abs() > 9.0 {
            let shift = ((c - p) / 12.0).round().clamp(-2.0, 2.0) * 12.0;
            if shift != 0.0 && (p + shift - c).abs() < 4.0 {
                notes[i].pitch = p + shift;
                moved += 1;
            }
        }
    }
    moved
}

/// The notes of one line (no octave fix, no grid).
pub fn line_notes(frames: &[Frame], syllables: &[Syllable], opts: &NoteOptions) -> Vec<Note> {
    let hop = hop_of(frames);
    let mut est = Vec::new();
    let mut counts = (0, 0);
    for s in syllables {
        syllable_notes(frames, hop, s, opts, &mut counts, &mut est);
    }
    fix_edges(&mut est, opts);
    fill_unknown(&mut est, f32::NAN);
    est.into_iter()
        .map(|e| to_note(e.start, e.end, &e))
        .collect()
}

fn hop_of(frames: &[Frame]) -> f64 {
    match frames {
        [a, b, ..] => b.time - a.time,
        _ => 0.01,
    }
}

/// Unknown pitches (freestyle notes without voiced frames) from the
/// nearest note with one, else `fallback`.
fn fill_unknown(est: &mut [Est], fallback: f32) {
    let known: Vec<(usize, f32)> = est
        .iter()
        .enumerate()
        .filter(|(_, e)| !e.pitch.is_nan())
        .map(|(i, e)| (i, e.pitch))
        .collect();
    for (i, e) in est.iter_mut().enumerate() {
        if e.pitch.is_nan() {
            e.pitch = known
                .iter()
                .min_by_key(|(j, _)| j.abs_diff(i))
                .map_or(fallback, |k| k.1);
        }
    }
}

fn to_note(start: f64, end: f64, e: &Est) -> Note {
    let start_ms = (start * 1000.0).round() as i64;
    Note {
        start_ms,
        dur_ms: ((end * 1000.0).round() as i64 - start_ms).max(1),
        pitch: if e.pitch.is_nan() {
            60
        } else {
            e.pitch.round() as i32
        },
        kind: e.kind,
        text: e.text.clone(),
    }
}

/// The karaoke track of a song from its melody and timed syllables (lines
/// in time order). Title, artist and language are left for the caller.
pub fn build_track(
    frames: &[Frame],
    lines: &[LineIn],
    opts: &NoteOptions,
) -> (KaraokeTrack, Report) {
    let hop = hop_of(frames);
    let mut counts = (0, 0);
    let mut est: Vec<Vec<Est>> = lines
        .iter()
        .map(|l| {
            let mut v = Vec::new();
            for s in &l.syllables {
                syllable_notes(frames, hop, s, opts, &mut counts, &mut v);
            }
            fix_edges(&mut v, opts);
            v
        })
        .collect();
    let mut report = Report {
        voiced_share: counts.1 as f32 / counts.0.max(1) as f32,
        ..Default::default()
    };
    // Unknown pitches: from the line, else the song.
    let mut all: Vec<f32> = est
        .iter()
        .flatten()
        .map(|e| e.pitch)
        .filter(|p| !p.is_nan())
        .collect();
    let song = if all.is_empty() {
        60.0
    } else {
        median(&mut all)
    };
    for l in &mut est {
        fill_unknown(l, song);
    }
    if opts.octave_fix {
        let mut voices: Vec<u8> = lines.iter().map(|l| l.voice).collect();
        voices.sort_unstable();
        voices.dedup();
        for v in voices {
            let mut of_voice: Vec<&mut Est> = est
                .iter_mut()
                .zip(lines)
                .filter(|(_, l)| l.voice == v)
                .flat_map(|(e, _)| e.iter_mut())
                .collect();
            report.octave_fixes += fix_octaves(&mut of_voice);
        }
    }
    let mut track = KaraokeTrack::default();
    for (l, e) in lines.iter().zip(&est) {
        let mut notes: Vec<Note> = Vec::with_capacity(e.len());
        let mut prev_end = f64::NEG_INFINITY;
        for n in e {
            let (mut start, mut end) = (n.start, n.end);
            if let Some(g) = opts.grid {
                let unit = 60.0 / (g.bpm * g.division.max(1) as f64);
                let snap = |x: f64| g.offset + ((x - g.offset) / unit).round() * unit;
                start = snap(start).max(prev_end);
                end = snap(end).max(start + unit);
            }
            prev_end = end;
            report.notes += 1;
            report.extra_notes += usize::from(n.text.trim() == "~");
            report.freestyle += usize::from(n.kind == NoteKind::Freestyle);
            notes.push(to_note(start, end, n));
        }
        if !notes.is_empty() {
            track.lines.push(KaraokeLine {
                voice: l.voice,
                notes,
            });
        }
    }
    if let Some(g) = opts.grid {
        let unit = 60.0 / (g.bpm * g.division.max(1) as f64);
        let first = track.notes().map(|n| n.start_ms).min().unwrap_or(0) as f64 / 1000.0;
        let mut gap = g.offset + ((first - g.offset) / unit).floor() * unit;
        while gap < 0.0 {
            gap += unit;
        }
        track.grid = Some(Grid {
            bpm: g.bpm * g.division.max(1) as f64 / 4.0,
            gap_ms: (gap * 1_000_000.0).round() / 1000.0,
        });
    }
    track.sort_lines();
    track.key = Key::estimate(track.notes());
    (track, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::midi_to_hz;

    /// Frames every 10 ms for `secs`, pitch from `midi(t)` (None: unvoiced).
    fn frames(secs: f64, midi: impl Fn(f64) -> Option<f32>) -> Vec<Frame> {
        (0..(secs * 100.0) as usize)
            .map(|i| {
                let time = i as f64 / 100.0;
                let m = midi(time);
                Frame {
                    time,
                    f0: m.map(midi_to_hz),
                    voiced_prob: if m.is_some() { 0.9 } else { 0.0 },
                }
            })
            .collect()
    }

    fn syl(start: f64, end: f64, text: &str) -> Syllable {
        Syllable {
            start,
            end,
            text: text.into(),
        }
    }

    fn summary(notes: &[Note]) -> Vec<(i32, &str, NoteKind)> {
        notes
            .iter()
            .map(|n| (n.pitch, n.text.as_str(), n.kind))
            .collect()
    }

    #[test]
    fn one_note_per_syllable_with_vibrato() {
        let fr = frames(2.0, |t| {
            let base = if t < 1.0 { 62.0 } else { 57.0 };
            Some(base + 0.8 * (2.0 * std::f64::consts::PI * 5.5 * t).sin() as f32)
        });
        let notes = line_notes(
            &fr,
            &[syl(0.0, 1.0, "Hel"), syl(1.0, 2.0, "lo ")],
            &NoteOptions::default(),
        );
        assert_eq!(
            summary(&notes),
            [(62, "Hel", NoteKind::Normal), (57, "lo ", NoteKind::Normal)]
        );
        assert_eq!((notes[1].start_ms, notes[1].dur_ms), (1000, 1000));
    }

    #[test]
    fn melisma_makes_extra_notes_but_ornaments_do_not() {
        // 60 for 0.4 s, a 60 ms flick to 63, back to 60, then 64 for 0.4 s.
        let fr = frames(1.2, |t| {
            Some(if t < 0.4 {
                60.0
            } else if t < 0.46 {
                63.0
            } else if t < 0.8 {
                60.0
            } else {
                64.0
            })
        });
        let notes = line_notes(&fr, &[syl(0.0, 1.2, "love ")], &NoteOptions::default());
        assert_eq!(
            summary(&notes),
            [(60, "love", NoteKind::Normal), (64, "~ ", NoteKind::Normal)]
        );
        assert!(
            (notes[1].start_ms - 795).abs() <= 10,
            "{}",
            notes[1].start_ms
        );
        assert_eq!(notes[1].end_ms(), 1200);
        // Three levels: "~" in the middle has no space.
        let fr = frames(0.9, |t| {
            Some(if t < 0.3 {
                60.0
            } else if t < 0.6 {
                62.0
            } else {
                64.0
            })
        });
        let notes = line_notes(&fr, &[syl(0.0, 0.9, "oh ")], &NoteOptions::default());
        let texts: Vec<&str> = notes.iter().map(|n| n.text.as_str()).collect();
        assert_eq!(texts, ["oh", "~", "~ "]);
    }

    #[test]
    fn rough_word_times_move_to_the_neighbour() {
        // "love" sung on 60 until 0.3 s, then "me" on 64, but the word
        // times say "me" starts at 0.5 s; "you" (0.85 s) on 67 really
        // starts at 1.0 s, so its first 0.15 s are still "me".
        let fr = frames(1.6, |t| {
            Some(if t < 0.3 {
                60.0
            } else if t < 1.0 {
                64.0
            } else {
                67.0
            })
        });
        let notes = line_notes(
            &fr,
            &[
                syl(0.0, 0.5, "love "),
                syl(0.5, 0.85, "me "),
                syl(0.85, 1.6, "you"),
            ],
            &NoteOptions::default(),
        );
        assert_eq!(
            summary(&notes),
            [
                (60, "love ", NoteKind::Normal),
                (64, "me ", NoteKind::Normal),
                (67, "you", NoteKind::Normal)
            ]
        );
        assert!((notes[1].start_ms - 300).abs() <= 10, "{:?}", notes[1]);
        assert!((notes[2].start_ms - 1000).abs() <= 10, "{:?}", notes[2]);
        assert_eq!(notes[1].end_ms(), notes[2].start_ms);
    }

    #[test]
    fn unvoiced_syllables_are_freestyle_and_silence_is_cut() {
        let fr = frames(2.0, |t| {
            if (0.5..0.8).contains(&t) {
                Some(67.0)
            } else {
                None
            }
        });
        let notes = line_notes(
            &fr,
            &[syl(0.0, 0.4, "shh "), syl(0.5, 1.5, "yeah ")],
            &NoteOptions::default(),
        );
        assert_eq!(notes[0].kind, NoteKind::Freestyle);
        // Its pitch comes from the neighbour.
        assert_eq!(notes[0].pitch, 67);
        assert_eq!(notes[1].kind, NoteKind::Normal);
        assert_eq!(notes[1].end_ms(), 800);
    }

    #[test]
    fn octave_errors_move_back_and_grid_snaps() {
        // Five syllables around 60, one tracked an octave up.
        let pitches = [60.0, 62.0, 74.0, 60.0, 59.0, 62.0];
        let fr = frames(3.0, |t| Some(pitches[((t / 0.5) as usize).min(5)]));
        let line = LineIn {
            voice: 1,
            syllables: (0..6)
                .map(|i| syl(i as f64 * 0.5 + 0.013, i as f64 * 0.5 + 0.49, "la "))
                .collect(),
        };
        let opts = NoteOptions {
            grid: Some(BeatGrid {
                bpm: 120.0,
                offset: 0.0,
                division: 4,
            }),
            ..Default::default()
        };
        let (track, report) = build_track(&fr, &[line], &opts);
        let p: Vec<i32> = track.notes().map(|n| n.pitch).collect();
        assert_eq!(p, [60, 62, 62, 60, 59, 62]);
        assert_eq!(report.octave_fixes, 1);
        assert_eq!(report.notes, 6);
        assert!(report.voiced_share > 0.95);
        // 120 BPM in sixteenths: 125 ms steps, UltraStar BPM 120.
        let g = track.grid.unwrap();
        assert_eq!((g.bpm, g.gap_ms), (120.0, 0.0));
        assert!(track
            .notes()
            .all(|n| n.start_ms % 125 == 0 && n.dur_ms % 125 == 0));
        assert_eq!(track.lines[0].voice, 1);
        assert!(track.key.is_some());
    }

    #[test]
    fn two_octave_errors_move_back() {
        let mut est: Vec<Est> = [57.0, 59.0, 83.0, 57.0, 36.0, 60.0, 59.0]
            .iter()
            .map(|&pitch| Est {
                start: 0.0,
                end: 0.1,
                pitch,
                kind: NoteKind::Normal,
                text: "la".into(),
                first: true,
            })
            .collect();
        let mut refs: Vec<&mut Est> = est.iter_mut().collect();
        assert_eq!(fix_octaves(&mut refs), 2);
        let p: Vec<f32> = est.iter().map(|e| e.pitch).collect();
        assert_eq!(p, [57.0, 59.0, 59.0, 57.0, 60.0, 60.0, 59.0]);
    }

    #[test]
    fn from_audio_to_ultrastar() {
        // A sung-like saw melody C4 E4 G4 (the last one gliding into a
        // held A4), through pYIN, notes and UltraStar.
        let rate = 22050u32;
        let melody = |t: f32| -> f32 {
            match t {
                t if t < 0.5 => 60.0,
                t if t < 1.0 => 64.0,
                t if t < 1.4 => 67.0,
                t if t < 1.45 => 67.0 + (t - 1.4) / 0.05 * 2.0,
                _ => 69.0,
            }
        };
        let mut ph = 0f32;
        let x: Vec<f32> = (0..(2.0 * rate as f32) as usize)
            .map(|i| {
                let t = i as f32 / rate as f32;
                ph += 2.0 * std::f32::consts::PI * midi_to_hz(melody(t)) / rate as f32;
                (1..8)
                    .map(|h| (h as f32 * ph).sin() / h as f32)
                    .sum::<f32>()
                    * 0.2
            })
            .collect();
        let fr = crate::pyin::track(&x, rate, &crate::PyinOptions::default());
        let line = LineIn {
            voice: 0,
            syllables: vec![
                syl(0.0, 0.5, "do "),
                syl(0.5, 1.0, "re "),
                syl(1.0, 2.0, "mi"),
            ],
        };
        let (mut track, report) = build_track(&fr, &[line], &NoteOptions::default());
        track.title = "Scale".into();
        let p: Vec<(i32, &str)> = track.notes().map(|n| (n.pitch, n.text.as_str())).collect();
        assert_eq!(p, [(60, "do "), (64, "re "), (67, "mi"), (69, "~")]);
        assert_eq!(report.extra_notes, 1);
        let txt = crate::ultrastar::to_ultrastar(&track);
        assert!(txt.contains(": 0 20 0 do \n"), "{txt}");
        assert_eq!(
            crate::ultrastar::from_ultrastar(&txt)
                .unwrap()
                .notes()
                .count(),
            4
        );
    }
}
