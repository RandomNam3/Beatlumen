//! A karaoke round: singers on microphones (`mic` streams), each on a
//! voice of the song's karaoke data, their live pitch (`bl_pitch::LiveYin`)
//! scored against the notes (`bl_pitch::score`) on the song's clock. The
//! microphones' audio is read from their ring buffers every tick; nothing
//! of it is kept beyond the current line.

use crate::core::Core;
use crate::host::Host;
use crate::mic;
use bl_json::Value;
use bl_pitch::score::{self, Difficulty, Judged, Scorer};
use bl_pitch::{KaraokeTrack, LiveOptions, LivePitch, LiveYin};
use std::collections::VecDeque;

/// Scored frames kept for drawing (ms of song time).
const TRAIL_MS: i64 = 8000;
/// Frames louder than this (RMS) count as heard (rap notes).
const HEARD_RMS: f32 = 0.01;
/// The round ends this long after the last note (ms).
const END_AFTER_MS: i64 = 1500;
/// Most singers in a round.
pub(crate) const MAX_SINGERS: usize = 8;

pub(crate) struct Singer {
    pub name: String,
    pub stream: u32,
    /// Voice of the song (0: all lines).
    pub voice: u8,
    yin: Option<LiveYin>,
    rate: u32,
    /// Next sample of the stream to read.
    next: u64,
    /// Stream sample of the pitch tracker's time 0.
    base: u64,
    /// (sample, clock µs) of recent blocks, to time the pitch frames.
    marks: VecDeque<(u64, f64)>,
    pub scorer: Scorer,
    trail: VecDeque<Judged>,
    found: Vec<LivePitch>,
    /// Level of the newest frame (RMS).
    level: f32,
}

impl Singer {
    fn new(name: String, stream: u32, voice: u8, scorer: Scorer) -> Singer {
        Singer {
            name,
            stream,
            voice,
            yin: None,
            rate: 0,
            next: 0,
            base: 0,
            marks: VecDeque::new(),
            scorer,
            trail: VecDeque::new(),
            found: Vec::new(),
            level: 0.0,
        }
    }

    /// The clock time (µs) of stream sample `idx`.
    fn time_of(&self, idx: u64) -> Option<f64> {
        let (i, t) = self
            .marks
            .iter()
            .rev()
            .find(|(i, _)| *i <= idx)
            .or(self.marks.front())?;
        Some(t + (idx as f64 - *i as f64) * 1e6 / self.rate.max(1) as f64)
    }

    /// Reads what the microphone recorded since the last tick; scores it
    /// when the song plays.
    fn take_audio(&mut self, playing: bool) {
        loop {
            let Some(b) = mic::read(self.stream, self.next, 8192) else {
                return;
            };
            if b.samples.is_empty() {
                return;
            }
            let restart = b.rate != self.rate || b.index != self.next || self.yin.is_none();
            if restart || !playing {
                // A gap (or paused): the tracker starts again here.
                if b.rate != self.rate || self.yin.is_none() {
                    self.rate = b.rate;
                    self.yin = Some(LiveYin::new(b.rate, LiveOptions::default()));
                } else if let Some(y) = &mut self.yin {
                    y.reset();
                }
                self.base = b.index;
                self.marks.clear();
            }
            self.next = b.index + b.samples.len() as u64;
            if !playing {
                continue;
            }
            self.marks.push_back((b.index, b.time_us));
            while self.marks.len() > 64 {
                self.marks.pop_front();
            }
            let mut found = std::mem::take(&mut self.found);
            found.clear();
            if let Some(y) = &mut self.yin {
                y.push(&b.samples, &mut found);
            }
            for p in &found {
                let idx = self.base + (p.time * self.rate as f64).round() as u64;
                let Some(at) = self.time_of(idx) else {
                    continue;
                };
                let Some(song) = mic::song_time(at) else {
                    continue;
                };
                let midi = p.f0.map(bl_pitch::hz_to_midi);
                let j = self
                    .scorer
                    .push((song * 1000.0).round() as i64, midi, p.rms >= HEARD_RMS);
                self.level = p.rms;
                self.trail.push_back(j);
            }
            self.found = found;
            if let Some(last) = self.trail.back().map(|j| j.time_ms) {
                while self
                    .trail
                    .front()
                    .is_some_and(|j| j.time_ms < last - TRAIL_MS || j.time_ms > last + 1000)
                {
                    self.trail.pop_front();
                }
            }
        }
    }
}

pub(crate) struct Session {
    pub track_id: String,
    pub track: KaraokeTrack,
    pub difficulty: Difficulty,
    pub singers: Vec<Singer>,
    pub finished: bool,
    /// Song time (ms) the round ends at.
    end_ms: i64,
    /// Newest song time seen (ms).
    now_ms: i64,
}

#[derive(Default)]
pub(crate) struct SessionState {
    pub session: Option<Session>,
    pub rev: u64,
}

impl Session {
    /// Points out of 10 000 (notes and held notes; the text's share when
    /// it is scored).
    fn points(s: &score::Score) -> f64 {
        (s.total() * 10_000.0 / (score::NOTE_POINTS + score::HELD_POINTS)).round()
    }

    fn singer_json(&self, s: &Singer, since_ms: Option<i64>, full: bool) -> Value {
        let sc = s.scorer.score(self.now_ms);
        let mut v = Value::object()
            .with("name", s.name.as_str())
            .with("stream", s.stream as u64)
            .with("voice", s.voice as u64)
            .with("points", Self::points(&sc))
            .with("notes", sc.notes.round())
            .with("held", sc.held.round())
            .with("heldNotes", sc.held_notes as u64)
            .with("longNotes", sc.long_notes as u64)
            .with("accuracy", (sc.accuracy * 1000.0).round() / 1000.0)
            .with("rating", score::rating(sc.accuracy))
            .with("level", s.level as f64);
        if let Some(since) = since_ms {
            // Flat [time ms, pitch or null, hit] triples after `since`.
            let mut t = Vec::new();
            for j in s.trail.iter().filter(|j| j.time_ms > since) {
                t.push(Value::from(j.time_ms));
                t.push(match j.midi {
                    Some(m) => Value::from((m as f64 * 100.0).round() / 100.0),
                    None => Value::Null,
                });
                t.push(Value::from(j.hit as u64));
            }
            v.set("trail", Value::Array(t));
        }
        let lines = s.scorer.lines(self.now_ms);
        let last = lines
            .iter()
            .rev()
            .find(|l| {
                self.track.lines[l.line]
                    .end_ms()
                    .is_some_and(|e| e <= self.now_ms)
            })
            .and_then(|l| Some((l.line, l.accuracy?)));
        if let Some((line, acc)) = last {
            v.set(
                "lastLine",
                Value::object()
                    .with("line", line as u64)
                    .with("rating", score::rating(acc)),
            );
        }
        if full {
            v.set(
                "lines",
                Value::Array(
                    lines
                        .iter()
                        .map(|l| {
                            Value::object().with("line", l.line as u64).with(
                                "accuracy",
                                l.accuracy.map(|a| (a * 1000.0).round() / 1000.0),
                            )
                        })
                        .collect(),
                ),
            );
        }
        v
    }
}

impl Core {
    /// `karaoke.start {singers: [{stream, voice, name}], difficulty}`: a
    /// round on the current song with its loaded karaoke data.
    pub(crate) fn karaoke_start(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        let Some(found) = self
            .karaoke
            .found
            .as_ref()
            .filter(|_| self.karaoke.id == id && !self.karaoke.loading)
        else {
            return Value::object().with("error", self.tr("This song has no karaoke data"));
        };
        let track = found.track.clone();
        let difficulty = Difficulty::from_name(c.str_or("difficulty", "medium"));
        let open = mic::stream_ids();
        let mut singers = Vec::new();
        for (i, s) in c
            .get("singers")
            .as_array()
            .map_or(&[][..], |a| a.as_slice())
            .iter()
            .take(MAX_SINGERS)
            .enumerate()
        {
            let Some(stream) = s.get("stream").as_u64().map(|v| v as u32) else {
                continue;
            };
            if !open.contains(&stream) || singers.iter().any(|x: &Singer| x.stream == stream) {
                continue;
            }
            let voice = s.get("voice").as_u64().unwrap_or(0).min(9) as u8;
            let name = match s.str_or("name", "").trim() {
                "" => format!("{} {}", self.tr("Singer"), i + 1),
                n => n.chars().take(40).collect(),
            };
            let scorer = Scorer::new(&track, voice, difficulty, LiveOptions::default().hop);
            let mut singer = Singer::new(name, stream, voice, scorer);
            // From now on: what the microphone heard before does not count.
            singer.next = mic::latest(stream, 0).map_or(0, |b| b.index);
            singers.push(singer);
        }
        if singers.is_empty() {
            return Value::object().with("error", self.tr("Choose a microphone for each singer"));
        }
        let end_ms = singers.iter().map(|s| s.scorer.end_ms()).max().unwrap_or(0) + END_AFTER_MS;
        self.karaoke_session.session = Some(Session {
            track_id: id,
            track,
            difficulty,
            singers,
            finished: false,
            end_ms,
            now_ms: 0,
        });
        self.karaoke_session.rev += 1;
        let _ = host;
        Value::object().with("ok", true)
    }

    /// Reads the microphones and scores them (from `tick`).
    pub(crate) fn poll_karaoke_session(&mut self, host: &mut dyn Host) {
        let playing = self.is_playing();
        let current = self.queue.current().cloned();
        let position_ms = (self.position(host) * 1000.0).round() as i64;
        let Some(s) = self.karaoke_session.session.as_mut() else {
            return;
        };
        if s.finished {
            return;
        }
        if current.as_deref() != Some(s.track_id.as_str()) {
            s.finished = true;
            self.karaoke_session.rev += 1;
            return;
        }
        for singer in &mut s.singers {
            singer.take_audio(playing);
        }
        s.now_ms = s.now_ms.max(position_ms);
        // Over: after the last note, or the song stopped after it.
        let last_note = s.end_ms - END_AFTER_MS;
        if position_ms >= s.end_ms || (!playing && s.now_ms >= last_note) {
            s.finished = true;
            self.karaoke_session.rev += 1;
        }
    }

    /// `karaoke.live {since}`: the round now: points, the newest scored
    /// frames of each singer (after song time `since`, ms).
    pub(crate) fn karaoke_live(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let Some(s) = &self.karaoke_session.session else {
            return Value::object().with("active", false);
        };
        let since = c.get("since").as_i64().unwrap_or(i64::MIN);
        let now = mic::song_time(mic::clock_us() as f64)
            .map(|t| (t * 1000.0).round() as i64)
            .unwrap_or_else(|| (self.position(host) * 1000.0).round() as i64);
        Value::object()
            .with("active", !s.finished)
            .with("finished", s.finished)
            .with("id", s.track_id.as_str())
            .with("songMs", now)
            .with("difficulty", s.difficulty.name())
            .with(
                "singers",
                Value::Array(
                    s.singers
                        .iter()
                        .map(|x| s.singer_json(x, Some(since), false))
                        .collect(),
                ),
            )
            .with("rev", self.karaoke_session.rev)
    }

    /// `karaoke.results`: points per singer with each line's hit share.
    pub(crate) fn karaoke_results(&self) -> Value {
        let Some(s) = &self.karaoke_session.session else {
            return Value::object().with("active", false);
        };
        let mut singers: Vec<Value> = s
            .singers
            .iter()
            .map(|x| s.singer_json(x, None, true))
            .collect();
        singers.sort_by(|a, b| b.f64_or("points", 0.0).total_cmp(&a.f64_or("points", 0.0)));
        Value::object()
            .with("finished", s.finished)
            .with("id", s.track_id.as_str())
            .with("title", s.track.title.as_str())
            .with("artist", s.track.artist.as_str())
            .with("difficulty", s.difficulty.name())
            .with("singers", Value::Array(singers))
    }

    /// `karaoke.stop`: ends the round (the results stay until the next).
    pub(crate) fn karaoke_stop(&mut self) -> Value {
        if let Some(s) = &mut self.karaoke_session.session {
            s.finished = true;
        }
        self.karaoke_session.rev += 1;
        self.karaoke_results()
    }
}
