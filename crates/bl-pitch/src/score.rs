//! Points for singing along: the live pitch of a singer against the notes
//! of their voice.
//!
//! - A frame within a pitched note is a hit when the sung pitch is within
//!   the difficulty's tolerance of the note in any octave.
//! - A note earns its share of the note points (by length, golden notes
//!   double) times its hit frames out of the frames it lasts.
//! - Rap notes count a frame as a hit when the singer is heard at all.
//!   Freestyle notes are not scored.
//! - Long notes held on pitch earn the held-note bonus.
//!
//! The points add up to [`NOTE_POINTS`] + [`HELD_POINTS`]; the text points
//! of the karaoke mode come on top (up to 10 000 in all).

use crate::track::{KaraokeTrack, NoteKind};

/// Points for the notes of a song sung perfectly.
pub const NOTE_POINTS: f64 = 8000.0;
/// Points for holding all long notes.
pub const HELD_POINTS: f64 = 1000.0;
/// Notes at least this long (ms) can earn the held-note bonus.
pub const LONG_NOTE_MS: i64 = 800;
/// Share of a long note that must be held on pitch without a break.
pub const HELD_SHARE: f64 = 0.75;

/// How close the pitch must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Difficulty {
    /// Within 2 semitones.
    Easy,
    /// Within 1 semitone.
    #[default]
    Medium,
    /// Within half a semitone.
    Hard,
}

impl Difficulty {
    pub fn tolerance(self) -> f32 {
        match self {
            Difficulty::Easy => 2.0,
            Difficulty::Medium => 1.0,
            Difficulty::Hard => 0.5,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Difficulty::Easy => "easy",
            Difficulty::Medium => "medium",
            Difficulty::Hard => "hard",
        }
    }

    pub fn from_name(s: &str) -> Difficulty {
        match s {
            "easy" => Difficulty::Easy,
            "hard" => Difficulty::Hard,
            _ => Difficulty::Medium,
        }
    }
}

/// Distance in semitones from `sung` to `note` in the nearest octave
/// (-6..=6).
pub fn pitch_class_distance(sung: f32, note: f32) -> f32 {
    (sung - note + 6.0).rem_euclid(12.0) - 6.0
}

/// `sung` moved by octaves to lie next to `note` (for drawing).
pub fn near_octave(sung: f32, note: f32) -> f32 {
    note + pitch_class_distance(sung, note)
}

/// A note of the scored voice.
#[derive(Debug, Clone)]
struct Target {
    start_ms: i64,
    end_ms: i64,
    pitch: i32,
    kind: NoteKind,
    line: usize,
    /// Share of the note points.
    weight: f64,
    /// Share of the held-note bonus (long notes only).
    held_weight: f64,
    frames: u32,
    hits: u32,
    run_ms: i64,
    best_run_ms: i64,
    last_hit_ms: Option<i64>,
}

/// One scored frame, for drawing the singer's pitch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Judged {
    /// Song time (ms).
    pub time_ms: i64,
    /// The sung pitch moved next to the note's octave (or as sung outside
    /// notes); `None` when nobody sings.
    pub midi: Option<f32>,
    /// On pitch within a scored note.
    pub hit: bool,
    /// Index of the line it fell into (in the track's lines).
    pub line: Option<usize>,
}

/// How a line went.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LineResult {
    pub line: usize,
    /// Hit share of its scored frames (0..1), `None` without scored notes.
    pub accuracy: Option<f64>,
}

/// The score of one singer.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Score {
    pub notes: f64,
    pub held: f64,
    /// Long notes held, of those sung through so far.
    pub held_notes: u32,
    pub long_notes: u32,
    /// Hit share of all scored frames so far.
    pub accuracy: f64,
}

impl Score {
    pub fn total(&self) -> f64 {
        self.notes + self.held
    }
}

/// Scores one voice of a track.
pub struct Scorer {
    targets: Vec<Target>,
    tolerance: f32,
    /// Index of the first note that may still get frames.
    cursor: usize,
    /// Expected spacing of frames (ms), for counting a note's frames.
    hop_ms: f64,
}

impl Scorer {
    /// The lines of `voice` (0: all lines; lines without a voice belong to
    /// every voice).
    pub fn new(track: &KaraokeTrack, voice: u8, difficulty: Difficulty, hop_s: f64) -> Scorer {
        let mut targets = Vec::new();
        for (li, l) in track.lines.iter().enumerate() {
            if voice != 0 && l.voice != 0 && l.voice != voice {
                continue;
            }
            for n in &l.notes {
                if n.kind == NoteKind::Freestyle || n.dur_ms <= 0 {
                    continue;
                }
                targets.push(Target {
                    start_ms: n.start_ms,
                    end_ms: n.end_ms(),
                    pitch: n.pitch,
                    kind: n.kind,
                    line: li,
                    weight: 0.0,
                    held_weight: 0.0,
                    frames: 0,
                    hits: 0,
                    run_ms: 0,
                    best_run_ms: 0,
                    last_hit_ms: None,
                });
            }
        }
        targets.sort_by_key(|t| t.start_ms);
        let golden = |k: NoteKind| matches!(k, NoteKind::Golden | NoteKind::RapGolden);
        let total: f64 = targets
            .iter()
            .map(|t| (t.end_ms - t.start_ms) as f64 * if golden(t.kind) { 2.0 } else { 1.0 })
            .sum();
        let long = |t: &Target| t.kind.has_pitch() && t.end_ms - t.start_ms >= LONG_NOTE_MS;
        let long_total: f64 = targets
            .iter()
            .filter(|t| long(t))
            .map(|t| (t.end_ms - t.start_ms) as f64)
            .sum();
        // Without long notes the bonus goes to the notes.
        let note_points = if long_total > 0.0 {
            NOTE_POINTS
        } else {
            NOTE_POINTS + HELD_POINTS
        };
        for t in &mut targets {
            let len = (t.end_ms - t.start_ms) as f64;
            t.weight = if total > 0.0 {
                note_points * len * if golden(t.kind) { 2.0 } else { 1.0 } / total
            } else {
                0.0
            };
            if long(t) {
                t.held_weight = HELD_POINTS * len / long_total;
            }
        }
        Scorer {
            targets,
            tolerance: difficulty.tolerance(),
            cursor: 0,
            hop_ms: (hop_s * 1000.0).max(1.0),
        }
    }

    /// Whether the voice has anything to score.
    pub fn is_empty(&self) -> bool {
        self.targets.is_empty()
    }

    /// Takes a frame of the singer at song time `time_ms`: the pitch as a
    /// (fractional) MIDI note or `None`, and whether anything is heard.
    pub fn push(&mut self, time_ms: i64, midi: Option<f32>, heard: bool) -> Judged {
        // Notes over long ago get no more frames.
        while self.cursor < self.targets.len() && self.targets[self.cursor].end_ms < time_ms - 2000
        {
            self.cursor += 1;
        }
        // Notes are in time order: none after one that starts later.
        let at = self.targets[self.cursor..]
            .iter()
            .take_while(|t| t.start_ms <= time_ms)
            .position(|t| time_ms < t.end_ms)
            .map(|i| i + self.cursor);
        let Some(i) = at else {
            return Judged {
                time_ms,
                midi,
                hit: false,
                line: None,
            };
        };
        let tol = self.tolerance;
        let t = &mut self.targets[i];
        let hit = match t.kind {
            NoteKind::Rap | NoteKind::RapGolden => heard || midi.is_some(),
            _ => midi.is_some_and(|m| pitch_class_distance(m, t.pitch as f32).abs() <= tol),
        };
        t.frames += 1;
        if hit {
            t.hits += 1;
            // A held run goes on over gaps of a few frames (YIN drops some).
            let gap = t.last_hit_ms.map_or(i64::MAX, |l| time_ms - l);
            if gap <= (self.hop_ms * 4.0) as i64 {
                t.run_ms += gap;
            } else {
                t.run_ms = 0;
            }
            t.best_run_ms = t.best_run_ms.max(t.run_ms);
            t.last_hit_ms = Some(time_ms);
        }
        Judged {
            time_ms,
            midi: midi.map(|m| {
                if t.kind.has_pitch() {
                    near_octave(m, t.pitch as f32)
                } else {
                    m
                }
            }),
            hit,
            line: Some(t.line),
        }
    }

    /// Hit share of one target's frames: hits out of the frames it lasts
    /// (missing frames count as missed), for the part sung so far.
    fn share(&self, t: &Target, now_ms: i64) -> f64 {
        let end = t.end_ms.min(now_ms);
        if end <= t.start_ms {
            return 0.0;
        }
        let expected = ((end - t.start_ms) as f64 / self.hop_ms)
            .max(t.frames as f64)
            .max(1.0);
        (t.hits as f64 / expected).min(1.0)
    }

    /// The score up to song time `now_ms`.
    pub fn score(&self, now_ms: i64) -> Score {
        let mut s = Score::default();
        let (mut hit_w, mut all_w) = (0.0, 0.0);
        for t in &self.targets {
            if t.start_ms >= now_ms {
                break;
            }
            let share = self.share(t, now_ms);
            s.notes += t.weight * share;
            let len = (t.end_ms.min(now_ms) - t.start_ms) as f64;
            hit_w += share * len;
            all_w += len;
            if t.held_weight > 0.0 && t.end_ms <= now_ms {
                s.long_notes += 1;
                let len = (t.end_ms - t.start_ms) as f64;
                if t.best_run_ms as f64 + self.hop_ms >= HELD_SHARE * len {
                    s.held_notes += 1;
                    s.held += t.held_weight;
                }
            }
        }
        s.accuracy = if all_w > 0.0 { hit_w / all_w } else { 0.0 };
        s
    }

    /// How each line with scored notes went, up to song time `now_ms`
    /// (lines that have started).
    pub fn lines(&self, now_ms: i64) -> Vec<LineResult> {
        let mut out: Vec<(usize, f64, f64)> = Vec::new();
        for t in &self.targets {
            if t.start_ms >= now_ms {
                break;
            }
            let len = (t.end_ms.min(now_ms) - t.start_ms) as f64;
            let share = self.share(t, now_ms);
            match out.iter_mut().find(|(l, _, _)| *l == t.line) {
                Some(e) => {
                    e.1 += share * len;
                    e.2 += len;
                }
                None => out.push((t.line, share * len, len)),
            }
        }
        out.sort_by_key(|e| e.0);
        out.into_iter()
            .map(|(line, h, a)| LineResult {
                line,
                accuracy: (a > 0.0).then(|| h / a),
            })
            .collect()
    }

    /// Song time (ms) when the voice's last note ends.
    pub fn end_ms(&self) -> i64 {
        self.targets.iter().map(|t| t.end_ms).max().unwrap_or(0)
    }
}

/// A word for how a line or song went (hit share 0..1), as shown.
pub fn rating(accuracy: f64) -> &'static str {
    match accuracy {
        a if a >= 0.9 => "perfect",
        a if a >= 0.75 => "great",
        a if a >= 0.55 => "good",
        a if a >= 0.3 => "ok",
        _ => "weak",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::{KaraokeLine, Note};

    fn note(start_ms: i64, dur_ms: i64, pitch: i32, kind: NoteKind) -> Note {
        Note {
            start_ms,
            dur_ms,
            pitch,
            kind,
            text: "la ".into(),
        }
    }

    fn track() -> KaraokeTrack {
        KaraokeTrack {
            lines: vec![
                KaraokeLine {
                    voice: 1,
                    notes: vec![
                        note(0, 500, 60, NoteKind::Normal),
                        note(500, 1000, 64, NoteKind::Normal),
                    ],
                },
                KaraokeLine {
                    voice: 2,
                    notes: vec![note(2000, 500, 67, NoteKind::Golden)],
                },
                KaraokeLine {
                    voice: 1,
                    notes: vec![
                        note(3000, 500, 62, NoteKind::Freestyle),
                        note(3500, 500, 62, NoteKind::Rap),
                    ],
                },
            ],
            ..Default::default()
        }
    }

    /// Sings `pitch(t)` every 10 ms from 0 to `end_ms`.
    fn sing(s: &mut Scorer, end_ms: i64, pitch: impl Fn(i64) -> Option<f32>) {
        for t in (0..end_ms).step_by(10) {
            s.push(t + 5, pitch(t), pitch(t).is_some());
        }
    }

    #[test]
    fn octaves_do_not_count() {
        assert_eq!(pitch_class_distance(72.0, 60.0), 0.0);
        assert_eq!(pitch_class_distance(49.5, 60.0), 1.5);
        assert!((pitch_class_distance(59.0, 60.0) + 1.0).abs() < 1e-6);
        assert_eq!(near_octave(76.4, 64.0), 64.4);
    }

    #[test]
    fn perfect_singing_gets_all_points() {
        let t = track();
        let mut s = Scorer::new(&t, 1, Difficulty::Hard, 0.01);
        // An octave lower all along.
        sing(&mut s, 4000, |t| match t {
            0..=499 => Some(48.0),
            500..=1499 => Some(52.2),
            3500..=3999 => Some(40.0),
            _ => None,
        });
        let sc = s.score(5000);
        assert!((sc.notes - NOTE_POINTS).abs() < 1.0, "{sc:?}");
        assert_eq!((sc.held_notes, sc.long_notes), (1, 1));
        assert!((sc.total() - NOTE_POINTS - HELD_POINTS).abs() < 1.0);
        assert!(sc.accuracy > 0.99);
        // The other voice's golden note is not this singer's.
        assert_eq!(s.lines(5000).len(), 2);
    }

    #[test]
    fn tolerance_by_difficulty() {
        let t = track();
        let sung = |t: i64| match t {
            0..=1499 => Some(if t < 500 { 61.4 } else { 65.4 }),
            _ => None,
        };
        let mut hard = Scorer::new(&t, 1, Difficulty::Hard, 0.01);
        let mut medium = Scorer::new(&t, 1, Difficulty::Medium, 0.01);
        let mut easy = Scorer::new(&t, 1, Difficulty::Easy, 0.01);
        sing(&mut hard, 2000, sung);
        sing(&mut medium, 2000, sung);
        sing(&mut easy, 2000, sung);
        assert_eq!(hard.score(2000).notes, 0.0);
        assert_eq!(medium.score(2000).notes, 0.0);
        assert!(easy.score(2000).notes > 0.6 * NOTE_POINTS);
    }

    #[test]
    fn a_broken_long_note_gets_no_bonus() {
        let t = track();
        let mut s = Scorer::new(&t, 1, Difficulty::Medium, 0.01);
        // On pitch for the long note but with a 300 ms break in the middle.
        sing(&mut s, 1500, |t| match t {
            500..=850 | 1150..=1499 => Some(64.0),
            _ => None,
        });
        let sc = s.score(1600);
        assert_eq!((sc.held_notes, sc.long_notes), (0, 1));
        assert_eq!(sc.held, 0.0);
        assert!(sc.notes > 0.0);
        // Short gaps (a dropped frame) do not break it.
        let mut s = Scorer::new(&t, 1, Difficulty::Medium, 0.01);
        sing(&mut s, 1500, |t| match t {
            500..=1499 if t % 70 != 0 => Some(64.0),
            _ => None,
        });
        assert_eq!(s.score(1600).held_notes, 1);
    }

    #[test]
    fn rap_counts_sound_and_lines_are_rated() {
        let t = track();
        let mut s = Scorer::new(&t, 0, Difficulty::Medium, 0.01);
        sing(&mut s, 4000, |t| match t {
            3500..=3999 => Some(30.0),
            _ => None,
        });
        let lines = s.lines(4000);
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].accuracy, Some(0.0));
        assert!(lines[2].accuracy.unwrap() > 0.99);
        assert_eq!(rating(0.95), "perfect");
        assert_eq!(rating(0.1), "weak");
        // The golden note counts double: 500 ms of 1500 + 2 * 500 + 500.
        assert!(s.score(4000).notes > 0.0);
        assert_eq!(s.end_ms(), 4000);
    }
}
