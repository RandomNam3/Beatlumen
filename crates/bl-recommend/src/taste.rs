//! A listener's taste from favorites, playlists and plays. Recent plays count
//! more than old ones, songs left early count against themselves.

use crate::catalog::Catalog;
use std::collections::HashMap;

/// Days after which a play counts half.
const HALF_LIFE_DAYS: f32 = 90.0;
/// Plays this close together were one listening session (seconds).
const SESSION_GAP: u64 = 30 * 60;
/// Songs of a session looked at to either side.
const SESSION_REACH: usize = 6;

const FAVORITE: f32 = 4.0;
const PLAYLIST: f32 = 1.0;
const PLAYED: f32 = 1.0;
const SKIPPED: f32 = -0.6;
/// A clear rating in the tags, given by the listener at some time.
const RATED: f32 = 2.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Play {
    pub id: String,
    /// Seconds since the Unix epoch.
    pub t: u64,
    pub skip: bool,
}

/// What is known about a listener.
#[derive(Default)]
pub struct Signals<'a> {
    /// Oldest first.
    pub plays: &'a [Play],
    /// Song ids with the time they were marked.
    pub favorites: &'a [(String, u64)],
    pub playlists: Vec<&'a [String]>,
    /// Seconds since the Unix epoch.
    pub now: u64,
}

pub struct Taste {
    /// How much each song is liked (may be negative).
    pub(crate) song: Vec<f32>,
    pub(crate) artist: Vec<f32>,
    pub(crate) genre: Vec<f32>,
    pub(crate) label: Vec<f32>,
    /// When each song was last played (0: never).
    pub(crate) last: Vec<u64>,
    pub(crate) plays: Vec<u32>,
    pub(crate) skips: Vec<u32>,
    /// Songs often played close to each other.
    pub(crate) together: HashMap<u32, Vec<(u32, f32)>>,
    /// Mean sound of what is liked: (energy, brightness, bpm), if known.
    pub(crate) sound: Option<(f32, f32, f32)>,
    pub(crate) now: u64,
    pub(crate) known: bool,
}

fn decay(now: u64, t: u64) -> f32 {
    let days = now.saturating_sub(t) as f32 / 86_400.0;
    0.5f32.powf(days / HALF_LIFE_DAYS)
}

impl Taste {
    pub fn new(c: &Catalog, s: &Signals) -> Taste {
        let n = c.len();
        let mut song = vec![0f32; n];
        let mut last = vec![0u64; n];
        let mut plays = vec![0u32; n];
        let mut skips = vec![0u32; n];
        let mut known = false;
        for (id, t) in s.favorites {
            if let Some(i) = c.index_of(id) {
                // Marked once and meant for good: fades slower than a play.
                song[i as usize] += FAVORITE * (0.5 + 0.5 * decay(s.now, *t));
                known = true;
            }
        }
        for list in &s.playlists {
            for id in list.iter() {
                if let Some(i) = c.index_of(id) {
                    song[i as usize] += PLAYLIST;
                    known = true;
                }
            }
        }
        for (i, item) in c.items.iter().enumerate() {
            match item.rating {
                Some(r) if r >= 0.8 => {
                    song[i] += RATED;
                    known = true;
                }
                Some(r) if r > 0.0 && r <= 0.2 => song[i] -= RATED,
                _ => {}
            }
        }
        // Sessions, for what is played together.
        let mut together: HashMap<(u32, u32), f32> = HashMap::new();
        let mut session: Vec<u32> = Vec::new();
        let mut session_end = 0u64;
        for p in s.plays {
            let Some(i) = c.index_of(&p.id) else {
                continue;
            };
            let w = decay(s.now, p.t);
            let u = i as usize;
            if p.t > last[u] {
                last[u] = p.t;
            }
            if p.skip {
                song[u] += SKIPPED * w;
                skips[u] += 1;
            } else {
                song[u] += PLAYED * w;
                plays[u] += 1;
                known = true;
                if p.t > session_end + SESSION_GAP {
                    session.clear();
                }
                session_end = p.t;
                for other in session.iter().rev().take(SESSION_REACH) {
                    if *other != i {
                        *together.entry((*other, i)).or_default() += w;
                        *together.entry((i, *other)).or_default() += w;
                    }
                }
                session.push(i);
            }
        }
        let mut near: HashMap<u32, Vec<(u32, f32)>> = HashMap::new();
        for ((a, b), w) in together {
            near.entry(a).or_default().push((b, w));
        }

        let mut artist = vec![0f32; c.artist_count()];
        let mut genre = vec![0f32; c.genre_count()];
        let mut label = vec![0f32; c.label_count()];
        let (mut sound, mut sound_w) = ((0f32, 0f32, 0f32), 0f32);
        for (i, v) in song.iter().enumerate() {
            if *v == 0.0 {
                continue;
            }
            let item = &c.items[i];
            for a in &item.artists {
                artist[*a as usize] += v;
            }
            for g in &item.genres {
                genre[*g as usize] += v;
            }
            for l in &item.labels {
                label[*l as usize] += v;
            }
            if let (Some(f), true) = (&item.features, *v > 0.0) {
                sound.0 += f.energy * v;
                sound.1 += f.brightness * v;
                sound.2 += f.bpm * v;
                sound_w += v;
            }
        }
        // Relative to the strongest, so one song more or less shifts little.
        fn normalize(v: &mut [f32]) {
            let top = v.iter().fold(0f32, |m, x| m.max(*x));
            if top > 0.0 {
                for x in v.iter_mut() {
                    *x = (*x / top).max(-1.0);
                }
            }
        }
        normalize(&mut artist);
        normalize(&mut genre);
        normalize(&mut label);

        Taste {
            song,
            artist,
            genre,
            label,
            last,
            plays,
            skips,
            together: near,
            sound: (sound_w > 0.0)
                .then(|| (sound.0 / sound_w, sound.1 / sound_w, sound.2 / sound_w)),
            now: s.now,
            known,
        }
    }

    /// Whether anything at all is known about this listener.
    pub fn is_known(&self) -> bool {
        self.known
    }

    /// How well a song fits the taste, whether it was heard or not.
    pub(crate) fn fit(&self, c: &Catalog, i: u32) -> f32 {
        let item = &c.items[i as usize];
        let top = |v: &[f32], ids: &[u32]| -> f32 {
            ids.iter().map(|x| v[*x as usize]).fold(
                0f32,
                |m, x| {
                    if x.abs() > m.abs() {
                        x
                    } else {
                        m
                    }
                },
            )
        };
        let mut f = 0.40 * top(&self.artist, &item.artists)
            + 0.25 * top(&self.genre, &item.genres)
            + 0.15 * top(&self.label, &item.labels);
        if let (Some((e, b, bpm)), Some(x)) = (self.sound, &item.features) {
            let tempo = 1.0 - ((x.bpm - bpm).abs() / 60.0).min(1.0);
            f += 0.20
                * (0.5 * (1.0 - (x.energy - e).abs())
                    + 0.2 * (1.0 - (x.brightness - b).abs())
                    + 0.3 * tempo);
        }
        f
    }

    /// Songs played close to `i` by this listener, with how often.
    pub(crate) fn played_with(&self, i: u32) -> &[(u32, f32)] {
        self.together.get(&i).map_or(&[], Vec::as_slice)
    }

    /// 0 for what was just played, towards 1 for what was heard long ago.
    pub(crate) fn freshness(&self, i: u32) -> f32 {
        let last = self.last[i as usize];
        if last == 0 {
            return 1.0;
        }
        let days = self.now.saturating_sub(last) as f32 / 86_400.0;
        1.0 - (-days / 10.0).exp()
    }

    pub(crate) fn days_since(&self, i: u32) -> Option<f32> {
        let last = self.last[i as usize];
        (last != 0).then(|| self.now.saturating_sub(last) as f32 / 86_400.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::song;

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    fn play(id: &str, days_ago: u64, skip: bool) -> Play {
        Play {
            id: id.to_string(),
            t: NOW - days_ago * DAY,
            skip,
        }
    }

    fn catalog() -> Catalog {
        Catalog::new(vec![
            song("a", "One", "Rock", 2000, 0.8, 120.0),
            song("b", "One", "Rock", 2001, 0.8, 120.0),
            song("c", "Two", "Rock", 2002, 0.7, 122.0),
            song("d", "Three", "Ambient", 1980, 0.1, 70.0),
        ])
    }

    #[test]
    fn nothing_known() {
        let c = catalog();
        let t = Taste::new(
            &c,
            &Signals {
                now: NOW,
                ..Signals::default()
            },
        );
        assert!(!t.is_known());
        assert_eq!(t.fit(&c, 0), 0.0);
    }

    #[test]
    fn favorites_and_plays_count_skips_count_against() {
        let c = catalog();
        let plays = [play("a", 1, false), play("d", 1, true), play("d", 2, true)];
        let fav = [("c".to_string(), NOW)];
        let t = Taste::new(
            &c,
            &Signals {
                plays: &plays,
                favorites: &fav,
                now: NOW,
                ..Signals::default()
            },
        );
        assert!(t.is_known());
        assert!(t.song[2] > t.song[0] && t.song[0] > 0.0);
        assert!(t.song[3] < 0.0);
        // Liking a's artist helps b, which was never played.
        assert!(t.fit(&c, 1) > t.fit(&c, 3));
        assert!(t.fit(&c, 3) < 0.2);
    }

    #[test]
    fn recent_plays_count_more() {
        let c = catalog();
        let plays = [play("a", 400, false), play("b", 1, false)];
        let t = Taste::new(
            &c,
            &Signals {
                plays: &plays,
                now: NOW,
                ..Signals::default()
            },
        );
        assert!(t.song[1] > t.song[0] * 3.0);
    }

    #[test]
    fn played_together_within_a_session() {
        let c = catalog();
        let plays = [
            play("a", 100, false),
            Play {
                id: "b".into(),
                t: NOW - 100 * DAY + 240,
                skip: false,
            },
            play("d", 50, false),
        ];
        let t = Taste::new(
            &c,
            &Signals {
                plays: &plays,
                now: NOW,
                ..Signals::default()
            },
        );
        assert_eq!(t.played_with(0).len(), 1);
        assert_eq!(t.played_with(0)[0].0, 1);
        assert!(t.played_with(3).is_empty(), "another session");
    }

    #[test]
    fn just_played_is_not_fresh() {
        let c = catalog();
        let plays = [play("a", 0, false), play("b", 100, false)];
        let t = Taste::new(
            &c,
            &Signals {
                plays: &plays,
                now: NOW,
                ..Signals::default()
            },
        );
        assert!(t.freshness(0) < 0.05);
        assert!(t.freshness(1) > 0.9);
        assert_eq!(t.freshness(2), 1.0);
    }
}
