//! Labels that follow from what is known about a song. They have the form
//! `kind:value`; labels given by people have no such form (a ":" is taken
//! out of their names). The apps translate the known ones.

use crate::catalog::{split_genres, Features, Song};

const KEYS: [&str; 12] = [
    "C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B",
];

/// `C`, `F#m`.
pub fn key_name(key: u8, minor: bool) -> String {
    format!(
        "{}{}",
        KEYS[key as usize % 12],
        if minor { "m" } else { "" }
    )
}

fn tempo_class(bpm: f32) -> &'static str {
    // Half and double tempo are hard to tell apart: the ends are judged
    // as they feel.
    if bpm < 90.0 {
        "slow"
    } else if bpm < 130.0 {
        "medium"
    } else {
        "fast"
    }
}

fn energy_class(energy: f32) -> &'static str {
    // Mastered music is loud: most of it lies between 0.5 and 0.95.
    if energy < 0.5 {
        "low"
    } else if energy < 0.8 {
        "medium"
    } else {
        "high"
    }
}

/// A rough guess from loudness and major or minor: the labels are shown as
/// an estimate.
fn mood(f: &Features) -> Option<&'static str> {
    match (energy_class(f.energy), f.minor) {
        ("high", false) => Some("upbeat"),
        ("high", true) => Some("intense"),
        ("low", false) => Some("relaxed"),
        ("low", true) => Some("melancholic"),
        _ => None,
    }
}

fn danceable(f: &Features) -> bool {
    f.beat >= 0.35 && (90.0..=140.0).contains(&f.bpm) && f.energy >= 0.45
}

/// The song's labels from its tags and features.
pub fn auto_labels(s: &Song) -> Vec<String> {
    let mut out = Vec::new();
    for g in split_genres(&s.genre) {
        out.push(format!("genre:{g}"));
    }
    if let Some(y) = s.year.filter(|y| *y >= 1000) {
        out.push(format!("decade:{}", y / 10 * 10));
    }
    if let Some(f) = &s.features {
        out.push(format!("tempo:{}", tempo_class(f.bpm)));
        out.push(format!("energy:{}", energy_class(f.energy)));
        out.push(format!("key:{}", key_name(f.key, f.minor)));
        if let Some(m) = mood(f) {
            out.push(format!("mood:{m}"));
        }
        if danceable(f) {
            out.push("mood:danceable".to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn features(bpm: f32, energy: f32, minor: bool) -> Features {
        Features {
            bpm,
            beat: 0.6,
            key: 9,
            minor,
            energy,
            brightness: 0.4,
        }
    }

    #[test]
    fn from_tags_alone() {
        let s = Song {
            genre: "Hard Rock; Metal".into(),
            year: Some(1987),
            ..Song::default()
        };
        assert_eq!(
            auto_labels(&s),
            ["genre:hard rock", "genre:metal", "decade:1980"]
        );
    }

    #[test]
    fn from_the_sound() {
        let s = Song {
            features: Some(features(128.0, 0.8, false)),
            ..Song::default()
        };
        let l = auto_labels(&s);
        for want in [
            "tempo:medium",
            "energy:high",
            "key:A",
            "mood:upbeat",
            "mood:danceable",
        ] {
            assert!(l.iter().any(|x| x == want), "{want}: {l:?}");
        }
        let s = Song {
            features: Some(features(70.0, 0.2, true)),
            ..Song::default()
        };
        let l = auto_labels(&s);
        for want in ["tempo:slow", "energy:low", "key:Am", "mood:melancholic"] {
            assert!(l.iter().any(|x| x == want), "{want}: {l:?}");
        }
        assert!(!l.iter().any(|x| x == "mood:danceable"));
    }
}
