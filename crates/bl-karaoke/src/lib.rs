//! The karaoke data of a song, as the server and the apps compute it:
//!
//! 1. the song decoded to stereo at the separation's rate,
//! 2. its vocals separated from the music (`bl-sep`),
//! 3. their melody (`bl_pitch::pyin`),
//! 4. one note per syllable of the timed song text (`bl_pitch::notes`),
//!    with the key.
//!
//! The result is a [`bl_pitch::KaraokeTrack`]; [`to_stored`] adds where
//! it came from and which song text it was made from, so that a changed
//! text makes it stale ([`is_current`]).

#![forbid(unsafe_code)]

use bl_pitch::{lrc, notes, pyin, syllables, KaraokeTrack};

/// Songs longer than this are taken for their first part only (minutes).
pub const MAX_MINUTES: u32 = 12;
pub use bl_pitch::stored::{import, is_current, text_hash, to_stored, Source, VERSION};

/// The language of a song text by its common words, among the languages
/// with hyphenation patterns ("en" when unclear).
pub fn guess_language(text: &str) -> &'static str {
    const WORDS: &[(&str, &[&str])] = &[
        (
            "en",
            &[
                "the", "you", "and", "i", "to", "it", "me", "my", "is", "that", "in", "your", "we",
                "don't", "i'm", "on", "of", "be",
            ],
        ),
        (
            "de",
            &[
                "ich", "du", "und", "die", "der", "das", "nicht", "ist", "mich", "mir", "ein",
                "es", "wir", "zu", "dich", "sie", "auf",
            ],
        ),
        (
            "es",
            &[
                "que", "de", "el", "la", "y", "en", "no", "me", "te", "mi", "tu", "lo", "por",
                "con", "una", "es", "yo",
            ],
        ),
        (
            "fr",
            &[
                "je", "tu", "le", "la", "et", "de", "les", "pas", "est", "que", "moi", "toi", "un",
                "une", "dans", "c'est", "mon",
            ],
        ),
        (
            "it",
            &[
                "che", "di", "il", "la", "e", "non", "mi", "ti", "sono", "per", "un", "una", "io",
                "tu", "con", "è", "ho",
            ],
        ),
        (
            "nl",
            &[
                "ik", "je", "de", "het", "een", "en", "niet", "is", "van", "dat", "mij", "jij",
                "we", "op", "zijn", "maar", "wat",
            ],
        ),
        (
            "pt",
            &[
                "que", "eu", "não", "você", "de", "o", "a", "e", "um", "uma", "meu", "minha",
                "com", "pra", "se", "te", "é",
            ],
        ),
    ];
    let words: Vec<String> = text
        .split(|c: char| !(c.is_alphanumeric() || c == '\''))
        .filter(|w| !w.is_empty())
        .map(|w| w.to_lowercase())
        .collect();
    let mut scores: Vec<(&str, usize)> = WORDS
        .iter()
        .map(|(lang, list)| {
            (
                *lang,
                words.iter().filter(|w| list.contains(&w.as_str())).count(),
            )
        })
        .collect();
    scores.sort_by(|a, b| b.1.cmp(&a.1));
    let (best, second) = (scores[0], scores[1]);
    if best.1 >= 3 && best.1 * 2 > second.1 * 3 {
        best.0
    } else {
        "en"
    }
}

/// A song decoded for the separation: one or two channels at its rate.
pub struct Audio {
    pub channels: Vec<Vec<f32>>,
    pub rate: u32,
}

impl Audio {
    pub fn secs(&self) -> f64 {
        self.channels.first().map_or(0, Vec::len) as f64 / self.rate.max(1) as f64
    }
}

/// Decodes a song to stereo (mono stays mono) at the separation's rate,
/// at most [`MAX_MINUTES`]. `keep_going` is asked between chunks.
pub fn decode(
    src: Box<dyn bl_media::Source>,
    ext_hint: Option<&str>,
    keep_going: &mut dyn FnMut() -> bool,
) -> Result<Audio, String> {
    let mut dec = bl_media::open(src, ext_hint).map_err(|e| e.to_string())?;
    let spec = dec.spec();
    let ch_in = spec.channels.max(1);
    let ch = ch_in.min(2) as usize;
    let rate = bl_sep::SAMPLE_RATE;
    let mut rs = bl_media::Resampler::new(spec.sample_rate, rate, ch as u16);
    let limit = rate as usize * MAX_MINUTES as usize * 60;
    let mut buf = Vec::new();
    let mut stereo = Vec::new();
    let mut out = Vec::new();
    let mut channels: Vec<Vec<f32>> = vec![Vec::new(); ch];
    // The converter's delay is dropped, so that times stay the same.
    let mut skip = rs.latency();
    loop {
        buf.clear();
        let more = dec.next_chunk(&mut buf).map_err(|e| e.to_string())?;
        let input: &[f32] = if ch_in > 2 {
            stereo.clear();
            bl_media::downmix_to_stereo(&buf, ch_in, &mut stereo);
            &stereo
        } else {
            &buf
        };
        out.clear();
        rs.process(input, &mut out);
        let drop = skip.min(out.len() / ch);
        skip -= drop;
        for f in out[drop * ch..].chunks_exact(ch) {
            for (c, s) in channels.iter_mut().zip(f) {
                c.push(*s);
            }
        }
        if !more || channels[0].len() >= limit {
            break;
        }
        if !keep_going() {
            return Err("cancelled".into());
        }
    }
    for c in &mut channels {
        c.truncate(limit);
    }
    if channels[0].is_empty() {
        return Err("no audio".into());
    }
    Ok(Audio { channels, rate })
}

/// What [`compute`] found, besides the track.
#[derive(Debug, Clone, Default)]
pub struct Report {
    pub notes: notes::Report,
    pub language: String,
    /// Seconds spent separating and tracking the pitch.
    pub separation_secs: f64,
    pub pitch_secs: f64,
}

/// The karaoke track of a song from its audio and its timed text
/// (enhanced LRC; lines without word times get their words spread).
/// `progress` gets 0..1 and may return false to cancel.
pub fn compute(
    sep: &bl_sep::Separator,
    audio: &Audio,
    lyrics: &str,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Result<(KaraokeTrack, Report), String> {
    let song = lrc::parse(lyrics);
    if song.lines.is_empty() {
        return Err("the song text has no times".into());
    }
    let t = std::time::Instant::now();
    let refs: Vec<&[f32]> = audio.channels.iter().map(Vec::as_slice).collect();
    let mut sep_progress = |p: f32| progress(p * 0.9);
    let vocals = sep.separate(
        &refs,
        audio.rate,
        bl_sep::Options {
            wiener: false,
            progress: &mut sep_progress,
        },
    )?;
    let separation_secs = t.elapsed().as_secs_f64();
    let mono = vocals.mono(16000);
    drop(vocals);
    if !progress(0.9) {
        return Err("cancelled".into());
    }
    let t = std::time::Instant::now();
    let frames = pyin::track(&mono, 16000, &pyin::PyinOptions::default());
    let pitch_secs = t.elapsed().as_secs_f64();
    let (track, report) = from_frames(&frames, &song);
    progress(1.0);
    let language = track.language.clone();
    Ok((
        track,
        Report {
            notes: report,
            language,
            separation_secs,
            pitch_secs,
        },
    ))
}

/// The track from a melody already tracked and the parsed song text.
pub fn from_frames(frames: &[pyin::Frame], song: &lrc::Song) -> (KaraokeTrack, notes::Report) {
    let lang = guess_language(&song.text());
    let lines: Vec<notes::LineIn> = song
        .lines
        .iter()
        .map(|l| notes::LineIn {
            voice: l.voice,
            syllables: syllables::syllabify(&l.syllables, lang),
        })
        .collect();
    let (mut track, report) = notes::build_track(frames, &lines, &notes::NoteOptions::default());
    track.title = song.title.clone();
    track.artist = song.artist.clone();
    track.language = lang.to_string();
    if track.key.is_none() {
        track.estimate_key();
    }
    (track, report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_pitch::Frame;

    #[test]
    fn languages() {
        assert_eq!(
            guess_language("ich liebe dich und du liebst mich nicht, das ist es"),
            "de"
        );
        assert_eq!(
            guess_language("you and me, it is the end of my world"),
            "en"
        );
        assert_eq!(guess_language("la la la"), "en");
    }

    #[test]
    fn notes_from_a_melody() {
        // A4 for the first second, C5 for the second.
        let frames: Vec<Frame> = (0..200)
            .map(|i| Frame {
                time: i as f64 * 0.01,
                f0: Some(if i < 100 { 440.0 } else { 523.25 }),
                voiced_prob: 0.9,
            })
            .collect();
        let song = lrc::parse("[00:00.00]<00:00.00>la <00:01.00>lu<00:01.90>\n");
        let (t, report) = from_frames(&frames, &song);
        assert_eq!(report.notes, 2);
        let pitches: Vec<i32> = t.notes().map(|n| n.pitch).collect();
        assert_eq!(pitches, [69, 72]);
        assert_eq!(t.language, "en");
    }
}
