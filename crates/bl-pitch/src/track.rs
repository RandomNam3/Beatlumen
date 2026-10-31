//! A karaoke track: lines of notes, each line sung by a voice, with the
//! song's key; stored as JSON.

use bl_json::{Map, Value};

/// How a note is sung and scored (the UltraStar note types).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NoteKind {
    /// `:`
    #[default]
    Normal,
    /// `*`: counts double.
    Golden,
    /// `F`: not scored (no clear pitch).
    Freestyle,
    /// `R`: only the timing is scored.
    Rap,
    /// `G`: rap, counts double.
    RapGolden,
}

impl NoteKind {
    pub fn symbol(self) -> char {
        match self {
            NoteKind::Normal => ':',
            NoteKind::Golden => '*',
            NoteKind::Freestyle => 'F',
            NoteKind::Rap => 'R',
            NoteKind::RapGolden => 'G',
        }
    }

    pub fn from_symbol(c: char) -> Option<NoteKind> {
        Some(match c {
            ':' => NoteKind::Normal,
            '*' => NoteKind::Golden,
            'F' => NoteKind::Freestyle,
            'R' => NoteKind::Rap,
            'G' => NoteKind::RapGolden,
            _ => return None,
        })
    }

    /// Whether the pitch is scored.
    pub fn has_pitch(self) -> bool {
        matches!(self, NoteKind::Normal | NoteKind::Golden)
    }
}

/// One sung note: a syllable, or "~" where a syllable goes on at another
/// pitch.
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub start_ms: i64,
    pub dur_ms: i64,
    /// MIDI note number (60 = C4).
    pub pitch: i32,
    pub kind: NoteKind,
    /// The syllable as shown; a word's last syllable keeps the space after
    /// it (or, as imported, a word's first one the space before it).
    pub text: String,
}

impl Note {
    pub fn end_ms(&self) -> i64 {
        self.start_ms + self.dur_ms
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct KaraokeLine {
    /// Who sings it (1, 2, …; 0 when not said), as in the song text.
    pub voice: u8,
    pub notes: Vec<Note>,
}

impl KaraokeLine {
    pub fn start_ms(&self) -> Option<i64> {
        self.notes.first().map(|n| n.start_ms)
    }

    pub fn end_ms(&self) -> Option<i64> {
        self.notes.iter().map(Note::end_ms).max()
    }

    pub fn text(&self) -> String {
        self.notes
            .iter()
            .map(|n| n.text.as_str())
            .filter(|t| t.trim() != "~")
            .collect::<String>()
            .trim()
            .to_string()
    }
}

/// The song's key.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Key {
    /// Pitch class of the tonic (0 = C … 11 = B).
    pub tonic: u8,
    pub minor: bool,
    /// How well the notes fit (correlation with the key profile, −1…1).
    pub strength: f32,
}

impl Key {
    pub fn name(&self) -> String {
        const NAMES: [&str; 12] = [
            "C", "C#", "D", "Eb", "E", "F", "F#", "G", "Ab", "A", "Bb", "B",
        ];
        format!(
            "{} {}",
            NAMES[self.tonic as usize % 12],
            if self.minor { "minor" } else { "major" }
        )
    }

    /// The key that fits the notes best (Krumhansl-Schmuckler: the
    /// duration of each pitch class against the Krumhansl-Kessler key
    /// profiles). `None` without pitched notes.
    pub fn estimate<'a>(notes: impl IntoIterator<Item = &'a Note>) -> Option<Key> {
        const MAJOR: [f32; 12] = [
            6.35, 2.23, 3.48, 2.33, 4.38, 4.09, 2.52, 5.19, 2.39, 3.66, 2.29, 2.88,
        ];
        const MINOR: [f32; 12] = [
            6.33, 2.68, 3.52, 5.38, 2.60, 3.53, 2.54, 4.75, 3.98, 2.69, 3.34, 3.17,
        ];
        let mut hist = [0f32; 12];
        for n in notes {
            if n.kind.has_pitch() && n.dur_ms > 0 {
                hist[n.pitch.rem_euclid(12) as usize] += n.dur_ms as f32;
            }
        }
        if hist.iter().all(|&h| h == 0.0) {
            return None;
        }
        let corr = |profile: &[f32; 12], tonic: usize| {
            let mx = hist.iter().sum::<f32>() / 12.0;
            let my = profile.iter().sum::<f32>() / 12.0;
            let (mut sxy, mut sxx, mut syy) = (0f32, 0f32, 0f32);
            for (pc, &h) in hist.iter().enumerate() {
                let x = h - mx;
                let y = profile[(pc + 12 - tonic) % 12] - my;
                sxy += x * y;
                sxx += x * x;
                syy += y * y;
            }
            sxy / (sxx * syy).sqrt().max(1e-12)
        };
        let mut best = Key {
            tonic: 0,
            minor: false,
            strength: f32::NEG_INFINITY,
        };
        for tonic in 0..12 {
            for (minor, profile) in [(false, &MAJOR), (true, &MINOR)] {
                let r = corr(profile, tonic);
                if r > best.strength {
                    best = Key {
                        tonic: tonic as u8,
                        minor,
                        strength: r,
                    };
                }
            }
        }
        Some(best)
    }
}

/// The UltraStar beat grid: a step is 15 / `bpm` seconds (a quarter of a
/// beat at `bpm`), step 0 is at `gap_ms`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    pub bpm: f64,
    pub gap_ms: f64,
}

impl Grid {
    pub fn step_ms(&self) -> f64 {
        15_000.0 / self.bpm
    }

    pub fn to_step(&self, ms: i64) -> i64 {
        ((ms as f64 - self.gap_ms) / self.step_ms()).round() as i64
    }

    pub fn to_ms(&self, step: i64) -> i64 {
        (self.gap_ms + step as f64 * self.step_ms()).round() as i64
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct KaraokeTrack {
    pub title: String,
    pub artist: String,
    /// Language of the text ("en", "de", … or a name as in UltraStar
    /// files), empty when not known.
    pub language: String,
    /// Singer names by voice: `singers[0]` sings voice 1.
    pub singers: Vec<String>,
    /// The grid the notes were placed on, if any (UltraStar files have
    /// one).
    pub grid: Option<Grid>,
    pub key: Option<Key>,
    /// Lines in time order.
    pub lines: Vec<KaraokeLine>,
    /// Other UltraStar header fields (MP3, COVER, GENRE, …), kept as read.
    pub extra: Vec<(String, String)>,
}

impl KaraokeTrack {
    pub fn notes(&self) -> impl Iterator<Item = &Note> {
        self.lines.iter().flat_map(|l| l.notes.iter())
    }

    /// Whether more than one voice sings.
    pub fn is_duet(&self) -> bool {
        let mut voices = self.lines.iter().map(|l| l.voice.max(1));
        let first = voices.next();
        voices.any(|v| Some(v) != first)
    }

    /// Lowest and highest pitched note.
    pub fn range(&self) -> Option<(i32, i32)> {
        let mut pitched = self.notes().filter(|n| n.kind.has_pitch()).map(|n| n.pitch);
        let first = pitched.next()?;
        Some(pitched.fold((first, first), |(lo, hi), p| (lo.min(p), hi.max(p))))
    }

    /// Sets `key` from the notes.
    pub fn estimate_key(&mut self) {
        self.key = Key::estimate(self.notes());
    }

    /// Sorts the lines by start time (stable, so voices keep their order).
    pub fn sort_lines(&mut self) {
        self.lines.sort_by_key(|l| l.start_ms().unwrap_or(i64::MAX));
    }

    /// The track as JSON: times in seconds, as the song text's JSON.
    ///
    /// `{"format": "beatlumen-karaoke", "version": 1, "title", "artist",
    /// "lang", "singers": [..], "grid": {"bpm", "gap"}, "key": {"tonic",
    /// "minor", "name", "strength"}, "extra": {..}, "lines": [{"v",
    /// "notes": [{"t", "d", "p", "s", "k"}]}]}` with `k` (the UltraStar
    /// note type) left out for normal notes.
    pub fn to_json(&self) -> Value {
        let mut v = Value::object()
            .with("format", "beatlumen-karaoke")
            .with("version", 1u32)
            .with("title", self.title.as_str())
            .with("artist", self.artist.as_str());
        if !self.language.is_empty() {
            v.set("lang", self.language.as_str());
        }
        if !self.singers.is_empty() {
            v.set("singers", self.singers.clone());
        }
        if let Some(g) = self.grid {
            v.set(
                "grid",
                Value::object().with("bpm", g.bpm).with("gap", g.gap_ms),
            );
        }
        if let Some(k) = self.key {
            v.set(
                "key",
                Value::object()
                    .with("tonic", k.tonic as u32)
                    .with("minor", k.minor)
                    .with("name", k.name())
                    .with("strength", (k.strength * 1000.0).round() / 1000.0),
            );
        }
        if !self.extra.is_empty() {
            let mut m = Map::new();
            for (k, val) in &self.extra {
                m.insert(k.as_str(), val.as_str());
            }
            v.set("extra", m);
        }
        let lines: Vec<Value> = self
            .lines
            .iter()
            .map(|l| {
                let notes: Vec<Value> = l
                    .notes
                    .iter()
                    .map(|n| {
                        let mut o = Value::object()
                            .with("t", n.start_ms as f64 / 1000.0)
                            .with("d", n.dur_ms as f64 / 1000.0)
                            .with("p", n.pitch)
                            .with("s", n.text.as_str());
                        if n.kind != NoteKind::Normal {
                            o.set("k", n.kind.symbol().to_string());
                        }
                        o
                    })
                    .collect();
                let mut o = Value::object();
                if l.voice > 0 {
                    o.set("v", l.voice as u32);
                }
                o.with("notes", notes)
            })
            .collect();
        v.with("lines", lines)
    }

    /// Reads [`to_json`](Self::to_json)'s format.
    pub fn from_json(v: &Value) -> Result<KaraokeTrack, String> {
        if v.str_or("format", "") != "beatlumen-karaoke" {
            return Err("not a karaoke track".into());
        }
        let ms = |x: &Value| x.as_f64().map(|s| (s * 1000.0).round() as i64);
        let mut t = KaraokeTrack {
            title: v.str_or("title", "").to_string(),
            artist: v.str_or("artist", "").to_string(),
            language: v.str_or("lang", "").to_string(),
            ..Default::default()
        };
        if let Some(s) = v.get("singers").as_array() {
            t.singers = s
                .iter()
                .filter_map(|x| x.as_str().map(str::to_string))
                .collect();
        }
        let g = v.get("grid");
        if let (Some(bpm), Some(gap)) = (g.get("bpm").as_f64(), g.get("gap").as_f64()) {
            if bpm > 0.0 {
                t.grid = Some(Grid { bpm, gap_ms: gap });
            }
        }
        let k = v.get("key");
        if let Some(tonic) = k.get("tonic").as_u64() {
            t.key = Some(Key {
                tonic: (tonic % 12) as u8,
                minor: k.bool_or("minor", false),
                strength: k.f32_or("strength", 0.0),
            });
        }
        if let Some(m) = v.get("extra").as_object() {
            t.extra = m
                .iter()
                .filter_map(|(k, x)| x.as_str().map(|s| (k.to_string(), s.to_string())))
                .collect();
        }
        for l in v.get("lines").as_array().map_or(&[][..], |a| a.as_slice()) {
            let mut line = KaraokeLine {
                voice: l.get("v").as_u64().unwrap_or(0).min(255) as u8,
                notes: Vec::new(),
            };
            for n in l.get("notes").as_array().map_or(&[][..], |a| a.as_slice()) {
                let (Some(start_ms), Some(dur_ms)) = (ms(n.get("t")), ms(n.get("d"))) else {
                    return Err("note without time".into());
                };
                let kind = n
                    .get("k")
                    .as_str()
                    .and_then(|s| s.chars().next())
                    .and_then(NoteKind::from_symbol)
                    .unwrap_or_default();
                line.notes.push(Note {
                    start_ms,
                    dur_ms,
                    pitch: n.get("p").as_i64().unwrap_or(60) as i32,
                    kind,
                    text: n.str_or("s", "").to_string(),
                });
            }
            t.lines.push(line);
        }
        Ok(t)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note(start_ms: i64, dur_ms: i64, pitch: i32, text: &str) -> Note {
        Note {
            start_ms,
            dur_ms,
            pitch,
            kind: NoteKind::Normal,
            text: text.into(),
        }
    }

    #[test]
    fn key_of_a_scale() {
        // A minor: A B C D E F G with A, C, E longest.
        let notes: Vec<Note> = [
            (57, 900),
            (59, 300),
            (60, 700),
            (62, 300),
            (64, 800),
            (65, 300),
            (67, 300),
        ]
        .iter()
        .map(|&(p, d)| note(0, d, p, "a"))
        .collect();
        let k = Key::estimate(&notes).unwrap();
        assert_eq!(k.name(), "A minor");
        // G major: G B D longest, F#.
        let notes: Vec<Note> = [
            (55, 900),
            (57, 300),
            (59, 700),
            (60, 300),
            (62, 800),
            (64, 300),
            (66, 400),
        ]
        .iter()
        .map(|&(p, d)| note(0, d, p, "a"))
        .collect();
        assert_eq!(Key::estimate(&notes).unwrap().name(), "G major");
        assert!(Key::estimate(&[]).is_none());
    }

    #[test]
    fn json_round_trip() {
        let mut t = KaraokeTrack {
            title: "Song".into(),
            artist: "Someone".into(),
            language: "en".into(),
            singers: vec!["Anne".into(), "Bob".into()],
            grid: Some(Grid {
                bpm: 300.5,
                gap_ms: 1234.5,
            }),
            extra: vec![("MP3".into(), "song.mp3".into())],
            lines: vec![
                KaraokeLine {
                    voice: 1,
                    notes: vec![note(1234, 250, 62, "Hel"), note(1484, 333, 64, "lo ")],
                },
                KaraokeLine {
                    voice: 2,
                    notes: vec![Note {
                        kind: NoteKind::Freestyle,
                        ..note(2001, 1000, 55, "yeah")
                    }],
                },
            ],
            ..Default::default()
        };
        t.estimate_key();
        let text = t.to_json().to_json();
        let back = KaraokeTrack::from_json(&bl_json::parse(&text).unwrap()).unwrap();
        assert_eq!(back.lines, t.lines);
        assert_eq!(back.grid, t.grid);
        assert_eq!(back.singers, t.singers);
        assert_eq!(back.extra, t.extra);
        assert_eq!(back.key.map(|k| k.name()), t.key.map(|k| k.name()));
        assert_eq!(back.lines[0].text(), "Hello");
        assert!(back.is_duet());
        assert_eq!(back.range(), Some((62, 64)));
    }
}
