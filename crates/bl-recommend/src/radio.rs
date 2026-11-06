//! A radio: starts from songs, an artist, an album or a label and walks from
//! song to song, close to where it started and to what plays right now, with
//! the listener's taste and some chance mixed in.

use crate::catalog::Catalog;
use crate::rng::Rng;
use crate::taste::Taste;
use std::collections::{HashSet, VecDeque};

/// Songs compared closely (out of the whole library).
const POOL: usize = 1500;
/// Songs a seed list is cut to.
const MAX_SEEDS: usize = 40;
/// So many songs pass before an artist comes again, if possible.
const ARTIST_SPACING: usize = 6;
/// A song by another artist comes first if it is this much like the last one.
const STILL_ALIKE: f32 = 0.45;

#[derive(Debug, Clone, PartialEq)]
pub enum Seed {
    Songs(Vec<String>),
    Artist(String),
    Album(String),
    Label(String),
    /// What the listener likes.
    Taste,
}

fn seed_songs(c: &Catalog, t: &Taste, seed: &Seed, rng: &mut Rng) -> Vec<u32> {
    let mut v = match seed {
        Seed::Songs(ids) => ids.iter().filter_map(|id| c.index_of(id)).collect(),
        Seed::Artist(a) => c.artist_songs(a),
        Seed::Album(a) => c.album_songs(a),
        Seed::Label(l) => c.label_songs(l),
        Seed::Taste => {
            let mut liked: Vec<u32> = (0..c.len() as u32)
                .filter(|i| t.song[*i as usize] > 0.0)
                .collect();
            liked.sort_by(|a, b| t.song[*b as usize].total_cmp(&t.song[*a as usize]));
            liked.truncate(MAX_SEEDS * 4);
            liked
        }
    };
    // A sample keeps long lists fast.
    while v.len() > MAX_SEEDS {
        let i = rng.below(v.len());
        v.swap_remove(i);
    }
    v
}

/// The next `count` songs of a radio. `recent`: ids already played or in the
/// queue, which are left out as long as there is enough else. `random` makes
/// each radio different.
pub fn radio(
    c: &Catalog,
    t: &Taste,
    seed: &Seed,
    recent: &[String],
    count: usize,
    random: u64,
) -> Vec<String> {
    let mut rng = Rng::new(random);
    if c.is_empty() || count == 0 {
        return Vec::new();
    }
    let seeds = seed_songs(c, t, seed, &mut rng);
    let excluded: HashSet<u32> = recent.iter().filter_map(|id| c.index_of(id)).collect();
    // A label radio stays within its label as long as it can.
    let only: Option<HashSet<u32>> = match seed {
        Seed::Label(l) => {
            let songs = c.label_songs(l);
            (songs.iter().filter(|s| !excluded.contains(s)).count() >= count.min(5))
                .then(|| songs.into_iter().collect())
        }
        _ => None,
    };

    // First a rough look at everything.
    let mut pool: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| only.as_ref().is_none_or(|o| o.contains(i)))
        .map(|i| {
            let close = if seeds.is_empty() {
                0.0
            } else {
                seeds.iter().map(|s| c.similarity(*s, i)).sum::<f32>() / seeds.len() as f32
            };
            let score = close + 0.3 * t.fit(c, i) + 0.25 * rng.f32();
            (score, i)
        })
        .collect();
    pool.sort_by(|a, b| b.0.total_cmp(&a.0));
    pool.truncate(POOL.max(count * 4));

    let mut out = Vec::with_capacity(count);
    let mut used: HashSet<u32> = HashSet::new();
    let mut recent_artists: VecDeque<u32> = VecDeque::new();
    let mut current = seeds.first().copied();
    for round in 0..2 {
        // The second round takes what was left out, if the library is small.
        while out.len() < count {
            let mut best: Option<(f32, u32)> = None;
            // The best by another artist than the one that just played.
            let mut other: Option<(f32, u32)> = None;
            for (base, i) in &pool {
                if used.contains(i) || (round == 0 && excluded.contains(i)) {
                    continue;
                }
                let item = &c.items[*i as usize];
                let mut score = *base;
                if let Some(cur) = current {
                    score += 0.6 * c.similarity(cur, *i);
                }
                // Played together before is a good sign, as is not having
                // heard it for a while.
                if let Some(cur) = current {
                    if t.played_with(cur).iter().any(|(o, _)| o == i) {
                        score += 0.25;
                    }
                }
                score *= 0.5 + 0.5 * t.freshness(*i);
                score -= 0.5 * (t.skips[*i as usize].min(5) as f32 / 5.0);
                // Not too soon again.
                let last = item
                    .artists
                    .iter()
                    .any(|a| recent_artists.back() == Some(a));
                if item.artists.iter().any(|a| recent_artists.contains(a)) {
                    score -= 0.25;
                }
                score += 0.18 * rng.f32();
                if best.is_none_or(|(b, _)| score > b) {
                    best = Some((score, *i));
                }
                if !last && other.is_none_or(|(b, _)| score > b) {
                    other = Some((score, *i));
                }
            }
            // Another artist, unless that means leaving the music behind (a
            // small library with little of this kind).
            if let Some((_, o)) = other {
                if current.is_none_or(|cur| c.similarity(cur, o) >= STILL_ALIKE) {
                    best = other;
                }
            }
            let Some((_, pick)) = best else {
                break;
            };
            used.insert(pick);
            current = Some(pick);
            for a in &c.items[pick as usize].artists {
                recent_artists.push_back(*a);
            }
            while recent_artists.len() > ARTIST_SPACING {
                recent_artists.pop_front();
            }
            out.push(c.id_of(pick).to_string());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::song;
    use crate::taste::{Play, Signals};

    const NOW: u64 = 1_800_000_000;

    /// Three rock artists and three ambient ones, four songs each.
    fn catalog() -> Catalog {
        let mut v = Vec::new();
        for (artist, genre, energy, bpm) in [
            ("Rock 1", "Rock", 0.8, 130.0),
            ("Rock 2", "Rock", 0.85, 125.0),
            ("Rock 3", "Rock", 0.75, 128.0),
            ("Calm 1", "Ambient", 0.1, 70.0),
            ("Calm 2", "Ambient", 0.15, 72.0),
            ("Calm 3", "Ambient", 0.2, 68.0),
        ] {
            for i in 0..4 {
                v.push(song(
                    &format!("{artist}-{i}"),
                    artist,
                    genre,
                    2000,
                    energy,
                    bpm,
                ));
            }
        }
        Catalog::new(v)
    }

    fn nobody(c: &Catalog) -> Taste {
        Taste::new(
            c,
            &Signals {
                now: NOW,
                ..Signals::default()
            },
        )
    }

    #[test]
    fn stays_close_to_the_seed() {
        let c = catalog();
        let t = nobody(&c);
        let list = radio(
            &c,
            &t,
            &Seed::Songs(vec!["Rock 1-0".to_string()]),
            &[],
            6,
            1,
        );
        assert_eq!(list.len(), 6);
        assert!(list.iter().all(|id| id.starts_with("Rock")), "{list:?}");
    }

    #[test]
    fn no_song_twice_and_artists_take_turns() {
        let c = catalog();
        let t = nobody(&c);
        let list = radio(&c, &t, &Seed::Artist("Rock 2".into()), &[], 9, 2);
        let unique: HashSet<_> = list.iter().collect();
        assert_eq!(unique.len(), list.len());
        for w in list.windows(2) {
            let artist = |s: &str| s.rsplit_once('-').unwrap().0.to_string();
            assert_ne!(artist(&w[0]), artist(&w[1]), "{list:?}");
        }
    }

    #[test]
    fn leaves_out_what_was_just_played_unless_nothing_else_is_left() {
        let c = catalog();
        let t = nobody(&c);
        let recent: Vec<String> = (0..4).map(|i| format!("Rock 1-{i}")).collect();
        let list = radio(&c, &t, &Seed::Artist("Rock 1".into()), &recent, 8, 3);
        assert!(list.iter().all(|id| !recent.contains(id)), "{list:?}");
        // All 24 asked for: the rest fills up.
        let all = radio(&c, &t, &Seed::Artist("Rock 1".into()), &recent, 24, 3);
        assert_eq!(all.len(), 24);
    }

    #[test]
    fn a_label_radio_stays_in_its_label() {
        let c = catalog();
        let t = nobody(&c);
        let list = radio(&c, &t, &Seed::Label("genre:ambient".into()), &[], 8, 4);
        assert_eq!(list.len(), 8);
        assert!(list.iter().all(|id| id.starts_with("Calm")), "{list:?}");
    }

    #[test]
    fn the_listeners_taste_decides_without_a_seed() {
        let c = catalog();
        let plays: Vec<Play> = (0..4)
            .map(|i| Play {
                id: format!("Calm 1-{i}"),
                t: NOW - 86_400 * 30,
                skip: false,
            })
            .collect();
        let t = Taste::new(
            &c,
            &Signals {
                plays: &plays,
                now: NOW,
                ..Signals::default()
            },
        );
        let list = radio(&c, &t, &Seed::Taste, &[], 6, 5);
        let calm = list.iter().filter(|id| id.starts_with("Calm")).count();
        assert!(calm >= 5, "{list:?}");
    }

    #[test]
    fn same_seed_same_list_other_seed_other_list() {
        let c = catalog();
        let t = nobody(&c);
        let seed = Seed::Songs(vec!["Rock 1-0".to_string()]);
        let a = radio(&c, &t, &seed, &[], 8, 7);
        let b = radio(&c, &t, &seed, &[], 8, 7);
        let d = radio(&c, &t, &seed, &[], 8, 8);
        assert_eq!(a, b);
        assert_ne!(a, d);
    }

    #[test]
    fn empty_library() {
        let c = Catalog::new(Vec::new());
        let t = nobody(&c);
        assert!(radio(&c, &t, &Seed::Taste, &[], 5, 1).is_empty());
    }
}
