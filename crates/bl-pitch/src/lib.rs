//! Pitch of the singing voice, for the karaoke mode.
//!
//! - [`pyin::track`]: the melody of a whole song offline (probabilistic
//!   YIN: several period candidates per frame, then the most likely path
//!   of pitch and voicing through an HMM), from separated vocals.
//! - [`live::LiveYin`]: the pitch of a microphone while someone sings,
//!   about 30 ms behind.
//! - [`notes`]: one note per syllable from the melody and timed
//!   syllables (an extra "~" note where the pitch moves within one).
//! - [`track::KaraokeTrack`]: lines of notes with voices and the key, as
//!   JSON and as UltraStar TXT ([`ultrastar`]).
//!
//! Own implementation of the methods in: A. de Cheveigné, H. Kawahara,
//! "YIN, a fundamental frequency estimator for speech and music" (2002);
//! M. Mauch, S. Dixon, "pYIN: A fundamental frequency estimator using
//! probabilistic threshold distributions" (2014).

#![forbid(unsafe_code)]

mod dsp;
pub mod live;
pub mod notes;
pub mod pyin;
pub mod syllables;
pub mod track;
pub mod ultrastar;

pub use live::{LiveOptions, LivePitch, LiveYin};
pub use notes::{build_track, BeatGrid, LineIn, NoteOptions, Syllable};
pub use pyin::{Frame, PyinOptions};
pub use track::{Grid, KaraokeLine, KaraokeTrack, Key, Note, NoteKind};

/// MIDI note number (69 = A4 = 440 Hz, fractional) of a frequency.
pub fn hz_to_midi(hz: f32) -> f32 {
    69.0 + 12.0 * (hz / 440.0).log2()
}

/// Frequency of a (fractional) MIDI note number.
pub fn midi_to_hz(midi: f32) -> f32 {
    440.0 * ((midi - 69.0) / 12.0).exp2()
}

/// Name of a MIDI note, e.g. "A4", "C#3".
pub fn midi_name(midi: i32) -> String {
    const NAMES: [&str; 12] = [
        "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
    ];
    format!(
        "{}{}",
        NAMES[midi.rem_euclid(12) as usize],
        midi.div_euclid(12) - 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn midi_conversions() {
        assert!((hz_to_midi(440.0) - 69.0).abs() < 1e-4);
        assert!((midi_to_hz(60.0) - 261.6256).abs() < 1e-2);
        assert_eq!(midi_name(60), "C4");
        assert_eq!(midi_name(69), "A4");
        assert_eq!(midi_name(-1), "B-2");
    }
}
