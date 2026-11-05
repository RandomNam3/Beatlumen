//! The songs to choose from and how alike two of them are.

use crate::labels::auto_labels;
use std::collections::HashMap;

/// How a song sounds (see `bl_analysis::features`); only a server knows.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Features {
    pub bpm: f32,
    /// How clear the beat is (0..1).
    pub beat: f32,
    /// Pitch class of the key (0 = C).
    pub key: u8,
    pub minor: bool,
    /// (0..1)
    pub energy: f32,
    /// (0..1)
    pub brightness: f32,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Song {
    pub id: String,
    pub artists: Vec<String>,
    pub album_id: String,
    pub genre: String,
    pub year: Option<i32>,
    /// When it came into the library (seconds since the Unix epoch).
    pub added: u64,
    pub features: Option<Features>,
    /// Labels given by people.
    pub labels: Vec<String>,
}

pub(crate) struct Item {
    pub artists: Vec<u32>,
    pub album: u32,
    pub genres: Vec<u32>,
    pub year: Option<i32>,
    pub added: u64,
    pub features: Option<Features>,
    /// Both automatic ones and those given by people.
    pub labels: Vec<u32>,
}

#[derive(Default)]
struct Names {
    ids: HashMap<String, u32>,
    names: Vec<String>,
}

impl Names {
    fn id(&mut self, name: &str) -> u32 {
        if let Some(i) = self.ids.get(name) {
            return *i;
        }
        let i = self.names.len() as u32;
        self.names.push(name.to_string());
        self.ids.insert(name.to_string(), i);
        i
    }
}

/// All songs, with names replaced by numbers.
pub struct Catalog {
    pub(crate) ids: Vec<String>,
    pub(crate) items: Vec<Item>,
    pub(crate) by_id: HashMap<String, u32>,
    artists: Names,
    genres: Names,
    labels: Names,
    albums: Names,
    pub(crate) by_artist: Vec<Vec<u32>>,
    pub(crate) by_album: Vec<Vec<u32>>,
    pub(crate) by_label: Vec<Vec<u32>>,
}

/// A genre tag may hold several ("Rock; Pop", "Indie/Folk").
pub(crate) fn split_genres(genre: &str) -> Vec<String> {
    genre
        .split([';', ',', '/', '|'])
        .map(|g| g.trim().to_lowercase())
        .filter(|g| !g.is_empty() && g != "unknown" && g != "other")
        .collect()
}

impl Catalog {
    pub fn new(songs: Vec<Song>) -> Catalog {
        let mut c = Catalog {
            ids: Vec::with_capacity(songs.len()),
            items: Vec::with_capacity(songs.len()),
            by_id: HashMap::with_capacity(songs.len()),
            artists: Names::default(),
            genres: Names::default(),
            labels: Names::default(),
            albums: Names::default(),
            by_artist: Vec::new(),
            by_album: Vec::new(),
            by_label: Vec::new(),
        };
        for s in songs {
            let i = c.ids.len() as u32;
            let mut labels: Vec<u32> = auto_labels(&s)
                .iter()
                .chain(s.labels.iter())
                .map(|l| c.labels.id(l))
                .collect();
            labels.sort_unstable();
            labels.dedup();
            let artists: Vec<u32> = s
                .artists
                .iter()
                .map(|a| a.trim().to_lowercase())
                .filter(|a| !a.is_empty() && a != "unknown artist")
                .map(|a| c.artists.id(&a))
                .collect();
            let album = c.albums.id(&s.album_id);
            let genres = split_genres(&s.genre)
                .iter()
                .map(|g| c.genres.id(g))
                .collect();
            for a in &artists {
                if c.by_artist.len() <= *a as usize {
                    c.by_artist.resize(*a as usize + 1, Vec::new());
                }
                c.by_artist[*a as usize].push(i);
            }
            if c.by_album.len() <= album as usize {
                c.by_album.resize(album as usize + 1, Vec::new());
            }
            c.by_album[album as usize].push(i);
            for l in &labels {
                if c.by_label.len() <= *l as usize {
                    c.by_label.resize(*l as usize + 1, Vec::new());
                }
                c.by_label[*l as usize].push(i);
            }
            c.by_id.insert(s.id.clone(), i);
            c.ids.push(s.id);
            c.items.push(Item {
                artists,
                album,
                genres,
                year: s.year,
                added: s.added,
                features: s.features,
                labels,
            });
        }
        c
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn index_of(&self, id: &str) -> Option<u32> {
        self.by_id.get(id).copied()
    }

    pub fn id_of(&self, i: u32) -> &str {
        &self.ids[i as usize]
    }

    pub fn artist_songs(&self, name: &str) -> Vec<u32> {
        self.artists
            .ids
            .get(&name.trim().to_lowercase())
            .map(|a| self.by_artist[*a as usize].clone())
            .unwrap_or_default()
    }

    pub fn album_songs(&self, album_id: &str) -> Vec<u32> {
        self.albums
            .ids
            .get(album_id)
            .map(|a| self.by_album[*a as usize].clone())
            .unwrap_or_default()
    }

    pub fn label_songs(&self, label: &str) -> Vec<u32> {
        self.labels
            .ids
            .get(label)
            .map(|l| self.by_label[*l as usize].clone())
            .unwrap_or_default()
    }

    /// Every label with the number of its songs, the fullest first.
    pub fn label_counts(&self) -> Vec<(&str, usize)> {
        let mut v: Vec<(&str, usize)> = self
            .labels
            .names
            .iter()
            .enumerate()
            .map(|(i, n)| (n.as_str(), self.by_label[i].len()))
            .filter(|(_, n)| *n > 0)
            .collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        v
    }

    /// The labels of a song.
    pub fn labels_of(&self, i: u32) -> impl Iterator<Item = &str> {
        self.items[i as usize]
            .labels
            .iter()
            .map(|l| self.labels.names[*l as usize].as_str())
    }

    pub(crate) fn genre_name(&self, g: u32) -> &str {
        &self.genres.names[g as usize]
    }

    pub(crate) fn artist_name(&self, a: u32) -> &str {
        &self.artists.names[a as usize]
    }

    pub(crate) fn genre_count(&self) -> usize {
        self.genres.names.len()
    }

    pub(crate) fn artist_count(&self) -> usize {
        self.artists.names.len()
    }

    pub(crate) fn label_count(&self) -> usize {
        self.labels.names.len()
    }

    /// How alike two songs are (0..1).
    pub fn similarity(&self, a: u32, b: u32) -> f32 {
        if a == b {
            return 1.0;
        }
        let (x, y) = (&self.items[a as usize], &self.items[b as usize]);
        let mut score = 0.0;
        let mut weight = 0.0;

        // What is not known about a song does not count against it.
        if !x.artists.is_empty() && !y.artists.is_empty() {
            weight += 0.25;
            if x.artists.iter().any(|t| y.artists.contains(t)) {
                score += 0.25;
            }
        }
        weight += 0.05;
        if x.album == y.album {
            score += 0.05;
        }
        if !x.genres.is_empty() && !y.genres.is_empty() {
            weight += 0.25;
            let shared = x.genres.iter().filter(|g| y.genres.contains(g)).count();
            score += 0.25 * shared as f32 / x.genres.len().max(y.genres.len()) as f32;
        }
        if let (Some(p), Some(q)) = (x.year, y.year) {
            weight += 0.08;
            score += 0.08 * (1.0 - ((p - q).abs() as f32 / 25.0).min(1.0));
        }
        if !x.labels.is_empty() && !y.labels.is_empty() {
            let shared = x.labels.iter().filter(|l| y.labels.contains(l)).count();
            let all = x.labels.len() + y.labels.len() - shared;
            weight += 0.10;
            score += 0.10 * shared as f32 / all as f32;
        }
        if let (Some(p), Some(q)) = (x.features, y.features) {
            weight += 0.37;
            score += 0.12 * tempo_closeness(p.bpm, q.bpm)
                + 0.13 * (1.0 - (p.energy - q.energy).abs())
                + 0.05 * (1.0 - (p.brightness - q.brightness).abs())
                + 0.04 * (1.0 - (p.beat - q.beat).abs())
                + 0.03 * key_closeness(p, q);
        }
        if weight <= 0.0 {
            0.0
        } else {
            score / weight
        }
    }
}

/// 1 for the same tempo, also at half or double speed; 0 from a third off.
fn tempo_closeness(a: f32, b: f32) -> f32 {
    if a <= 0.0 || b <= 0.0 {
        return 0.5;
    }
    let octaves = (a / b).log2().abs();
    let off = (octaves - octaves.round()).abs();
    (1.0 - off / 0.4).clamp(0.0, 1.0)
}

/// Neighbours on the circle of fifths go well together.
fn key_closeness(a: Features, b: Features) -> f32 {
    // Relative major of a minor key: same place on the circle.
    let place = |f: Features| -> i32 {
        let pc = if f.minor { (f.key + 3) % 12 } else { f.key };
        (pc as i32 * 7) % 12
    };
    let d = (place(a) - place(b)).rem_euclid(12);
    let steps = d.min(12 - d);
    (1.0 - steps as f32 / 6.0).clamp(0.0, 1.0)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn song(id: &str, artist: &str, genre: &str, year: i32, energy: f32, bpm: f32) -> Song {
        Song {
            id: id.to_string(),
            artists: vec![artist.to_string()],
            album_id: format!("{artist}-album"),
            genre: genre.to_string(),
            year: Some(year),
            added: 1000,
            features: Some(Features {
                bpm,
                beat: 0.5,
                key: 0,
                minor: false,
                energy,
                brightness: 0.5,
            }),
            labels: Vec::new(),
        }
    }

    #[test]
    fn same_artist_and_sound_are_closest() {
        let c = Catalog::new(vec![
            song("a", "One", "Rock", 2000, 0.8, 120.0),
            song("b", "One", "Rock", 2001, 0.8, 120.0),
            song("c", "Two", "Rock", 2002, 0.7, 122.0),
            song("d", "Three", "Ambient", 1980, 0.1, 70.0),
        ]);
        let ab = c.similarity(0, 1);
        let ac = c.similarity(0, 2);
        let ad = c.similarity(0, 3);
        assert!(ab > ac && ac > ad, "{ab} {ac} {ad}");
        assert!((c.similarity(2, 2) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn songs_without_sound_or_tags_can_still_be_compared() {
        let mut a = song("a", "One", "Rock", 2000, 0.8, 120.0);
        let mut b = song("b", "Two", "Rock", 2000, 0.8, 120.0);
        a.features = None;
        b.features = None;
        let bare = Song {
            id: "c".into(),
            ..Song::default()
        };
        let c = Catalog::new(vec![a, b, bare]);
        assert!(c.similarity(0, 1) > 0.3);
        assert!(c.similarity(0, 2) < 0.2);
    }

    #[test]
    fn half_tempo_is_the_same_tempo() {
        assert!(tempo_closeness(120.0, 60.0) > 0.99);
        assert!(tempo_closeness(120.0, 121.0) > 0.9);
        assert!(tempo_closeness(120.0, 170.0) < 0.2);
    }

    #[test]
    fn neighbour_keys_are_closer() {
        let f = |key, minor| Features {
            bpm: 120.0,
            beat: 0.5,
            key,
            minor,
            energy: 0.5,
            brightness: 0.5,
        };
        // C major: G major is next to it, A minor the same place, F# far away.
        assert!(key_closeness(f(0, false), f(9, true)) > 0.99);
        assert!(key_closeness(f(0, false), f(7, false)) > key_closeness(f(0, false), f(6, false)));
    }

    #[test]
    fn genres_split_and_labels_counted() {
        assert_eq!(split_genres("Rock; Pop/Indie"), ["rock", "pop", "indie"]);
        let mut a = song("a", "One", "Rock", 2000, 0.9, 150.0);
        a.labels = vec!["Party".to_string()];
        let c = Catalog::new(vec![a, song("b", "Two", "Pop", 1995, 0.1, 70.0)]);
        assert_eq!(c.label_songs("Party"), [0]);
        assert_eq!(c.label_songs("genre:pop"), [1]);
        assert_eq!(c.label_songs("decade:1990"), [1]);
        assert_eq!(c.label_songs("tempo:fast"), [0]);
        assert!(c
            .label_counts()
            .iter()
            .any(|(n, k)| *n == "Party" && *k == 1));
        assert!(c.labels_of(0).any(|l| l == "energy:high"));
    }
}
