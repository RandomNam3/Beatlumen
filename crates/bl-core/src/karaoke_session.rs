//! A karaoke round: singers on microphones (`mic` streams), each on a
//! voice of the song's karaoke data, their live pitch (`bl_pitch::LiveYin`)
//! scored against the notes (`bl_pitch::score`) on the song's clock. The
//! microphones' audio is read from their ring buffers every tick; nothing
//! of it is kept beyond the last half minute, for scoring the text of the
//! lines just sung (`karaoke_text`).

// The browser build computes nothing on the device (no speech models).
#![cfg_attr(target_arch = "wasm32", allow(dead_code, unused_variables))]

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
/// Rate of the audio kept for the text points.
const TEXT_RATE: u32 = 16000;
/// Audio kept for the text points (seconds of song time).
const TEXT_KEEP_SECS: f64 = 30.0;
/// Room around a line when its text is scored (seconds).
const TEXT_ROOM_SECS: f64 = 0.3;

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
    /// The singing at 16 kHz for the text points, and the song time (s)
    /// of its first sample.
    resampler: Option<bl_media::Resampler>,
    audio: Vec<f32>,
    audio_t0: Option<f64>,
    /// Lines sent for text points, and their scores as they come.
    text_sent: Vec<usize>,
    text_scores: Vec<(usize, Option<f32>)>,
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
            resampler: None,
            audio: Vec::new(),
            audio_t0: None,
            text_sent: Vec::new(),
            text_scores: Vec::new(),
        }
    }

    /// Keeps a block of the singing at 16 kHz, starting again where it does
    /// not follow on from what is kept (a gap, a seek).
    fn keep_audio(&mut self, rate: u32, song_s: f64, samples: &[f32]) {
        let follows = self.audio_t0.is_some_and(|t0| {
            (t0 + self.audio.len() as f64 / TEXT_RATE as f64 - song_s).abs() < 0.15
        });
        if !follows || self.resampler.as_ref().map(|r| r.in_rate()) != Some(rate) {
            self.resampler = Some(bl_media::Resampler::new(rate, TEXT_RATE, 1));
            self.audio.clear();
            self.audio_t0 = Some(song_s);
        }
        if let Some(r) = &mut self.resampler {
            r.process(samples, &mut self.audio);
        }
        let keep = (TEXT_KEEP_SECS * TEXT_RATE as f64) as usize;
        if self.audio.len() > keep + TEXT_RATE as usize * 5 {
            let cut = self.audio.len() - keep;
            self.audio.drain(..cut);
            if let Some(t0) = &mut self.audio_t0 {
                *t0 += cut as f64 / TEXT_RATE as f64;
            }
        }
    }

    /// The kept singing from song time `a` to `b` (s), when it is there.
    fn audio_between(&self, a: f64, b: f64) -> Option<Vec<f32>> {
        let t0 = self.audio_t0?;
        let at = |t: f64| ((t - t0) * TEXT_RATE as f64).round().max(0.0) as usize;
        let (i, j) = (at(a), at(b).min(self.audio.len()));
        // Most of the line must be there.
        (j > i && (j - i) as f64 >= 0.8 * (b - a) * TEXT_RATE as f64)
            .then(|| self.audio[i..j].to_vec())
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
            if let Some(song) = mic::song_time(b.time_us) {
                self.keep_audio(b.rate, song, &b.samples);
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
    /// Points for the text: the scorer (with a speech model that has a
    /// head), whether it is ready, and each line's text and letters.
    #[cfg(not(target_arch = "wasm32"))]
    text: Option<crate::karaoke_text::TextScorer>,
    pub text_state: TextState,
    line_texts: Vec<(String, usize)>,
}

/// Whether a round scores the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TextState {
    /// No speech model with a head here.
    Off,
    Loading,
    Ready,
}

#[derive(Default)]
pub(crate) struct SessionState {
    pub session: Option<Session>,
    pub rev: u64,
}

/// Points for the text in all.
const TEXT_POINTS: f64 = 1000.0;
/// Below this text score (`bl_align::Head::sung_score`) a line's words
/// count as not sung, above `TEXT_FULL` as sung fully (measured: the song's
/// own vocals score 0.27-0.34 in the median, another line's words 0.10).
const TEXT_NONE: f32 = 0.08;
const TEXT_FULL: f32 = 0.28;

/// A line's share of its text points from its score (0..1).
fn text_accuracy(score: f32) -> f64 {
    ((score - TEXT_NONE) / (TEXT_FULL - TEXT_NONE)).clamp(0.0, 1.0) as f64
}

fn text_state_name(s: TextState) -> &'static str {
    match s {
        TextState::Off => "off",
        TextState::Loading => "loading",
        TextState::Ready => "ready",
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Session {
    /// Sends the lines just sung to the text scorer, takes its answers.
    fn score_text(&mut self) {
        use crate::karaoke_text::{Answer, Job};
        let Some(scorer) = &self.text else {
            return;
        };
        for a in scorer.answers.take() {
            match a {
                Answer::Ready => self.text_state = TextState::Ready,
                Answer::Unavailable => self.text_state = TextState::Off,
                Answer::Line {
                    singer,
                    line,
                    score,
                } => {
                    if let Some(s) = self.singers.get_mut(singer) {
                        s.text_scores.push((line, score));
                    }
                }
            }
        }
        if self.text_state == TextState::Off {
            self.text = None;
            return;
        }
        let lang = if self.track.language.is_empty() {
            "en".to_string()
        } else {
            self.track.language.clone()
        };
        for si in 0..self.singers.len() {
            for (line, a, b) in self.lines_to_score(&self.singers[si]) {
                let singer = &mut self.singers[si];
                singer.text_sent.push(line);
                match singer.audio_between(a - TEXT_ROOM_SECS, b + TEXT_ROOM_SECS) {
                    Some(audio) => scorer.send(Job {
                        singer: si,
                        line,
                        audio,
                        text: self.line_texts[line].0.clone(),
                        lang: lang.clone(),
                    }),
                    // Not heard (paused, a gap): no score either way.
                    None => singer.text_scores.push((line, None)),
                }
            }
        }
    }
}

impl Session {
    /// Points for the text of a singer so far: the scored lines' shares by
    /// their letters (lines not scored, e.g. skipped on a slow device, do
    /// not count either way).
    fn text_points(&self, s: &Singer) -> Option<f64> {
        if self.text_state == TextState::Off {
            return None;
        }
        let (mut got, mut all) = (0.0, 0.0);
        for (line, score) in &s.text_scores {
            let Some(score) = score else { continue };
            let letters = self.line_texts.get(*line).map_or(0, |l| l.1) as f64;
            got += text_accuracy(*score) * letters;
            all += letters;
        }
        Some(if all > 0.0 {
            TEXT_POINTS * got / all
        } else {
            0.0
        })
    }

    /// Points out of 10 000: notes, held notes and the text; without text
    /// points the others count for all.
    fn points(&self, s: &Singer, sc: &score::Score) -> f64 {
        let max = score::NOTE_POINTS + score::HELD_POINTS;
        match self.text_points(s) {
            Some(t) => (sc.total() + t).round(),
            None => (sc.total() * 10_000.0 / max).round(),
        }
    }

    /// The singer's lines that have ended and are not scored for their text
    /// yet: (line, start s, end s).
    fn lines_to_score(&self, s: &Singer) -> Vec<(usize, f64, f64)> {
        self.track
            .lines
            .iter()
            .enumerate()
            .filter(|(i, l)| {
                (s.voice == 0 || l.voice == 0 || l.voice == s.voice)
                    && !s.text_sent.contains(i)
                    && self.line_texts.get(*i).is_some_and(|t| t.1 > 0)
            })
            .filter_map(|(i, l)| {
                let (a, b) = (l.start_ms()?, l.end_ms()?);
                (b + 400 <= self.now_ms).then_some((i, a as f64 / 1000.0, b as f64 / 1000.0))
            })
            .collect()
    }

    fn singer_json(&self, s: &Singer, since_ms: Option<i64>, full: bool) -> Value {
        let sc = s.scorer.score(self.now_ms);
        let mut v = Value::object()
            .with("name", s.name.as_str())
            .with("stream", s.stream as u64)
            .with("voice", s.voice as u64)
            .with("points", self.points(s, &sc))
            .with("text", self.text_points(s).map(f64::round))
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
                            let text = s
                                .text_scores
                                .iter()
                                .find(|(i, _)| *i == l.line)
                                .and_then(|(_, sc)| *sc)
                                .map(|sc| (text_accuracy(sc) * 1000.0).round() / 1000.0);
                            Value::object()
                                .with("line", l.line as u64)
                                .with(
                                    "accuracy",
                                    l.accuracy.map(|a| (a * 1000.0).round() / 1000.0),
                                )
                                .with("text", text)
                        })
                        .collect(),
                ),
            );
        }
        v
    }
}

impl Core {
    /// The text scorer for a round, with a speech model that has a head.
    #[cfg(not(target_arch = "wasm32"))]
    fn karaoke_text_scorer(&self, host: &dyn Host) -> Option<crate::karaoke_text::TextScorer> {
        if !host.has_threads() {
            return None;
        }
        let dir = host.storage_path("models/.keep").and_then(|p| {
            std::path::PathBuf::from(p)
                .parent()
                .map(std::path::PathBuf::from)
        })?;
        let model = crate::karaoke_text::model_with_head(&dir, &self.settings.speech_model)?;
        Some(crate::karaoke_text::TextScorer::start(host, model))
    }

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
        let line_texts: Vec<(String, usize)> = track
            .lines
            .iter()
            .map(|l| {
                let text: String = l
                    .notes
                    .iter()
                    .map(|n| n.text.trim_start_matches('~'))
                    .collect();
                let letters = text.chars().filter(|c| c.is_alphabetic()).count();
                (text.trim().to_string(), letters)
            })
            .collect();
        #[cfg(not(target_arch = "wasm32"))]
        let text = self.karaoke_text_scorer(host);
        #[cfg(target_arch = "wasm32")]
        let text: Option<()> = None;
        let text_state = if text.is_some() {
            TextState::Loading
        } else {
            TextState::Off
        };
        self.karaoke_session.session = Some(Session {
            track_id: id,
            track,
            difficulty,
            singers,
            finished: false,
            end_ms,
            now_ms: 0,
            #[cfg(not(target_arch = "wasm32"))]
            text,
            text_state,
            line_texts,
        });
        self.karaoke_session.rev += 1;
        Value::object()
            .with("ok", true)
            .with("text", text_state != TextState::Off)
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
        #[cfg(not(target_arch = "wasm32"))]
        s.score_text();
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
            .with("textScoring", text_state_name(s.text_state))
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
            .with("textScoring", text_state_name(s.text_state))
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_scores_to_shares() {
        assert_eq!(text_accuracy(0.05), 0.0);
        assert_eq!(text_accuracy(0.5), 1.0);
        assert!((text_accuracy(0.18) - 0.5).abs() < 1e-6);
    }
}
