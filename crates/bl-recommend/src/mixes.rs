//! Rows of recommendations with a reason, and personal playlists ("mixes").
//!
//! Reasons and names have a key and a value; the apps turn them into text in
//! their own language.

use crate::catalog::Catalog;
use crate::radio::{radio, Seed};
use crate::rng::Rng;
use crate::taste::Taste;
use std::collections::{HashMap, HashSet};

/// Songs per row.
const ROW: usize = 24;
/// Songs per mix.
const MIX: usize = 40;
const MAX_MIXES: usize = 6;
/// Added this recently, a song counts as new (seconds).
const NEW_FOR: u64 = 60 * 86_400;
/// Liked but not heard for this long, a song is worth bringing back (days).
const FORGOTTEN_AFTER: f32 = 60.0;

/// Why a song is suggested.
#[derive(Debug, Clone, PartialEq)]
pub enum Reason {
    /// You listen to this artist.
    Artist(String),
    /// You listen to this genre.
    Genre(String),
    /// Played close to a song you like.
    PlayedWith(String),
    /// New in the library.
    New,
    /// Not heard for a while.
    Forgotten,
    /// Nothing known yet, to try out.
    Discover,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub id: String,
    pub reason: Reason,
}

/// A row of the start page; `key` is one of `for-you`, `because` (`arg`: the
/// song's id), `new`, `forgotten`, `discover`.
#[derive(Debug, Clone, PartialEq)]
pub struct Shelf {
    pub key: &'static str,
    pub arg: String,
    pub items: Vec<Suggestion>,
}

/// A personal playlist; `key` is `genre`, `artist` or `label` (`arg`: which
/// one), `discover` or `forgotten`.
#[derive(Debug, Clone, PartialEq)]
pub struct Mix {
    pub id: String,
    pub key: &'static str,
    pub arg: String,
    pub items: Vec<String>,
}

/// Songs by how well they fit, each artist appearing only so often.
fn diverse(c: &Catalog, scored: &mut Vec<(f32, u32)>, count: usize, per_artist: usize) -> Vec<u32> {
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));
    let mut seen: HashMap<u32, usize> = HashMap::new();
    let mut out = Vec::with_capacity(count);
    for (_, i) in scored.iter() {
        if out.len() >= count {
            break;
        }
        let artists = &c.items[*i as usize].artists;
        if artists
            .iter()
            .any(|a| seen.get(a).copied().unwrap_or(0) >= per_artist)
        {
            continue;
        }
        for a in artists {
            *seen.entry(*a).or_default() += 1;
        }
        out.push(*i);
    }
    out
}

fn reason_for(c: &Catalog, t: &Taste, i: u32) -> Reason {
    let item = &c.items[i as usize];
    let top_artist = item
        .artists
        .iter()
        .map(|a| (t.artist[*a as usize], *a))
        .max_by(|x, y| x.0.total_cmp(&y.0));
    let top_genre = item
        .genres
        .iter()
        .map(|g| (t.genre[*g as usize], *g))
        .max_by(|x, y| x.0.total_cmp(&y.0));
    match (top_artist, top_genre) {
        (Some((a, name)), Some((g, _))) if a > 0.05 && a >= g * 0.5 => {
            Reason::Artist(c.artist_name(name).to_string())
        }
        (_, Some((g, name))) if g > 0.05 => Reason::Genre(c.genre_name(name).to_string()),
        (Some((a, name)), None) if a > 0.05 => Reason::Artist(c.artist_name(name).to_string()),
        _ => Reason::Discover,
    }
}

/// The rows of recommendations for a listener. `day` changes the choice from
/// day to day and keeps it for the day.
pub fn shelves(c: &Catalog, t: &Taste, day: u64) -> Vec<Shelf> {
    let mut rng = Rng::new(day ^ 0x5851_F42D);
    let mut out = Vec::new();
    if c.is_empty() {
        return out;
    }
    let mut shown: HashSet<u32> = HashSet::new();

    if !t.is_known() {
        // Nothing known yet: a varied selection to try out.
        let mut scored: Vec<(f32, u32)> = (0..c.len() as u32).map(|i| (rng.f32(), i)).collect();
        let items = diverse(c, &mut scored, ROW, 1);
        out.push(Shelf {
            key: "discover",
            arg: String::new(),
            items: items
                .into_iter()
                .map(|i| Suggestion {
                    id: c.id_of(i).to_string(),
                    reason: Reason::Discover,
                })
                .collect(),
        });
        return out;
    }

    // For you: fits the taste, not heard lately, a little chance.
    let mut scored: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| t.song[*i as usize] > -0.3)
        .map(|i| {
            let fit = t.fit(c, i) * (0.4 + 0.6 * t.freshness(i));
            (fit + 0.08 * rng.f32(), i)
        })
        .collect();
    let items = diverse(c, &mut scored, ROW, 2);
    shown.extend(items.iter().copied());
    out.push(Shelf {
        key: "for-you",
        arg: String::new(),
        items: items
            .iter()
            .map(|i| Suggestion {
                id: c.id_of(*i).to_string(),
                reason: reason_for(c, t, *i),
            })
            .collect(),
    });

    // Because you listened to the song you lately played most.
    if let Some(song) = (0..c.len() as u32)
        .filter(|i| t.days_since(*i).is_some_and(|d| d < 14.0) && t.plays[*i as usize] > 0)
        .max_by(|a, b| t.song[*a as usize].total_cmp(&t.song[*b as usize]))
    {
        let id = c.id_of(song).to_string();
        let items: Vec<Suggestion> = radio(
            c,
            t,
            &Seed::Songs(vec![id.clone()]),
            std::slice::from_ref(&id),
            ROW / 2,
            day,
        )
        .into_iter()
        .map(|s| {
            let with = t
                .played_with(song)
                .iter()
                .any(|(o, _)| Some(*o) == c.index_of(&s));
            Suggestion {
                reason: if with {
                    Reason::PlayedWith(id.clone())
                } else {
                    reason_for(c, t, c.index_of(&s).unwrap_or(song))
                },
                id: s,
            }
        })
        .collect();
        if items.len() >= 3 {
            out.push(Shelf {
                key: "because",
                arg: id,
                items,
            });
        }
    }

    // New, the songs that fit first.
    let mut scored: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| {
            t.plays[*i as usize] == 0 && t.now.saturating_sub(c.items[*i as usize].added) < NEW_FOR
        })
        .map(|i| (t.fit(c, i) + 0.05 * rng.f32(), i))
        .collect();
    let items = diverse(c, &mut scored, ROW, 3);
    if items.len() >= 3 {
        out.push(Shelf {
            key: "new",
            arg: String::new(),
            items: items
                .iter()
                .map(|i| Suggestion {
                    id: c.id_of(*i).to_string(),
                    reason: Reason::New,
                })
                .collect(),
        });
    }

    // Liked once, not heard for a while.
    let mut scored: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| {
            t.song[*i as usize] > 0.0 && t.days_since(*i).is_none_or(|d| d > FORGOTTEN_AFTER)
        })
        .map(|i| (t.song[i as usize] + 0.2 * rng.f32(), i))
        .collect();
    let items = diverse(c, &mut scored, ROW, 4);
    if items.len() >= 3 {
        out.push(Shelf {
            key: "forgotten",
            arg: String::new(),
            items: items
                .iter()
                .map(|i| Suggestion {
                    id: c.id_of(*i).to_string(),
                    reason: Reason::Forgotten,
                })
                .collect(),
        });
    }
    out
}

/// Personal playlists: one per thing the listener likes most (a genre, else
/// an artist), plus a discovery mix of what they never played and one of what
/// they forgot. Stays the same for the day.
pub fn mixes(c: &Catalog, t: &Taste, day: u64) -> Vec<Mix> {
    let mut out = Vec::new();
    if c.is_empty() || !t.is_known() {
        return out;
    }
    let mut groups: Vec<(f32, &'static str, String, Seed)> = Vec::new();
    for (g, v) in t.genre.iter().enumerate() {
        if *v > 0.15 {
            let name = c.genre_name(g as u32).to_string();
            groups.push((
                *v,
                "genre",
                name.clone(),
                Seed::Label(format!("genre:{name}")),
            ));
        }
    }
    if groups.len() < 3 {
        // Few genre tags: the artists listened to most.
        let mut artists: Vec<(f32, u32)> = t
            .artist
            .iter()
            .enumerate()
            .filter(|(_, v)| **v > 0.15)
            .map(|(a, v)| (*v, a as u32))
            .collect();
        artists.sort_by(|a, b| b.0.total_cmp(&a.0));
        for (v, a) in artists.into_iter().take(MAX_MIXES) {
            let name = c.artist_name(a).to_string();
            groups.push((v, "artist", name.clone(), Seed::Artist(name)));
        }
    }
    groups.sort_by(|a, b| b.0.total_cmp(&a.0));
    groups.truncate(MAX_MIXES - 2);
    let recently_heard: Vec<String> = (0..c.len() as u32)
        .filter(|i| t.days_since(*i).is_some_and(|d| d < 1.0))
        .map(|i| c.id_of(i).to_string())
        .collect();
    for (n, (_, key, arg, seed)) in groups.into_iter().enumerate() {
        let items = radio(
            c,
            t,
            &seed,
            &recently_heard,
            MIX,
            day.wrapping_add(n as u64 + 1),
        );
        if items.len() >= 5 {
            out.push(Mix {
                id: format!("mix-{key}-{arg}"),
                key,
                arg,
                items,
            });
        }
    }

    let mut rng = Rng::new(day ^ 0x2545_F491);
    let mut scored: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| t.plays[*i as usize] == 0 && t.skips[*i as usize] == 0)
        .map(|i| (t.fit(c, i) + 0.15 * rng.f32(), i))
        .collect();
    let items = diverse(c, &mut scored, MIX, 3);
    if items.len() >= 5 {
        out.push(Mix {
            id: "mix-discover".to_string(),
            key: "discover",
            arg: String::new(),
            items: items.iter().map(|i| c.id_of(*i).to_string()).collect(),
        });
    }
    let mut scored: Vec<(f32, u32)> = (0..c.len() as u32)
        .filter(|i| {
            t.song[*i as usize] > 0.0 && t.days_since(*i).is_none_or(|d| d > FORGOTTEN_AFTER)
        })
        .map(|i| (t.song[i as usize] + 0.2 * rng.f32(), i))
        .collect();
    let items = diverse(c, &mut scored, MIX, 4);
    if items.len() >= 5 {
        out.push(Mix {
            id: "mix-forgotten".to_string(),
            key: "forgotten",
            arg: String::new(),
            items: items.iter().map(|i| c.id_of(*i).to_string()).collect(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::tests::song;
    use crate::taste::{Play, Signals};

    const NOW: u64 = 1_800_000_000;
    const DAY: u64 = 86_400;

    fn catalog() -> Catalog {
        let mut v = Vec::new();
        for (artist, genre, energy, bpm) in [
            ("Rock 1", "Rock", 0.8, 130.0),
            ("Rock 2", "Rock", 0.85, 125.0),
            ("Rock 3", "Rock", 0.75, 128.0),
            ("Calm 1", "Ambient", 0.1, 70.0),
            ("Calm 2", "Ambient", 0.15, 72.0),
        ] {
            for i in 0..8 {
                let mut s = song(&format!("{artist}-{i}"), artist, genre, 2000, energy, bpm);
                s.added = NOW - 400 * DAY;
                v.push(s);
            }
        }
        Catalog::new(v)
    }

    fn play(id: &str, days_ago: u64) -> Play {
        Play {
            id: id.to_string(),
            t: NOW - days_ago * DAY,
            skip: false,
        }
    }

    fn taste<'a>(c: &Catalog, plays: &'a [Play], fav: &'a [(String, u64)]) -> Taste {
        Taste::new(
            c,
            &Signals {
                plays,
                favorites: fav,
                now: NOW,
                ..Signals::default()
            },
        )
    }

    #[test]
    fn a_new_listener_gets_a_varied_selection() {
        let c = catalog();
        let t = taste(&c, &[], &[]);
        let s = shelves(&c, &t, 1);
        assert_eq!(s.len(), 1);
        assert_eq!(s[0].key, "discover");
        // One song per artist.
        assert_eq!(s[0].items.len(), 5);
        assert!(mixes(&c, &t, 1).is_empty());
    }

    #[test]
    fn suggestions_follow_the_taste_with_a_reason() {
        let c = catalog();
        let plays: Vec<Play> = (0..3).map(|i| play(&format!("Rock 1-{i}"), 20)).collect();
        let t = taste(&c, &plays, &[]);
        let s = shelves(&c, &t, 1);
        let fy = s.iter().find(|x| x.key == "for-you").expect("for you");
        let rock = fy
            .items
            .iter()
            .take(6)
            .filter(|i| i.id.starts_with("Rock"))
            .count();
        assert_eq!(rock, 6, "{fy:?}");
        assert!(matches!(
            fy.items[0].reason,
            Reason::Artist(_) | Reason::Genre(_)
        ));
    }

    #[test]
    fn just_played_songs_come_later() {
        let c = catalog();
        let mut plays: Vec<Play> = (0..8).map(|i| play(&format!("Rock 1-{i}"), 50)).collect();
        plays.push(play("Rock 2-0", 0));
        let t = taste(&c, &plays, &[]);
        let s = shelves(&c, &t, 1);
        let fy = s.iter().find(|x| x.key == "for-you").expect("for you");
        assert!(
            fy.items.iter().take(4).all(|i| i.id != "Rock 2-0"),
            "{fy:?}"
        );
    }

    #[test]
    fn because_you_listened() {
        let c = catalog();
        let plays = [play("Rock 2-1", 2)];
        let t = taste(&c, &plays, &[]);
        let s = shelves(&c, &t, 1);
        let b = s.iter().find(|x| x.key == "because").expect("because");
        assert_eq!(b.arg, "Rock 2-1");
        assert!(b.items.iter().all(|i| i.id != "Rock 2-1"));
        assert!(
            b.items.iter().take(3).all(|i| i.id.starts_with("Rock")),
            "{b:?}"
        );
    }

    #[test]
    fn new_songs_only_within_two_months() {
        let mut songs: Vec<_> = (0..4)
            .map(|i| {
                let mut s = song(
                    &format!("New {i}"),
                    &format!("A{i}"),
                    "Rock",
                    2020,
                    0.8,
                    120.0,
                );
                s.added = NOW - 3 * DAY;
                s
            })
            .collect();
        let mut old = song("Old", "B", "Rock", 2020, 0.8, 120.0);
        old.added = NOW - 300 * DAY;
        songs.push(old);
        let c = Catalog::new(songs);
        let plays = [play("Old", 2)];
        let t = taste(&c, &plays, &[]);
        let s = shelves(&c, &t, 1);
        let new = s.iter().find(|x| x.key == "new").expect("new");
        assert_eq!(new.items.len(), 4);
        assert!(new
            .items
            .iter()
            .all(|i| i.id != "Old" && i.reason == Reason::New));
    }

    #[test]
    fn favorites_not_heard_for_long_come_back() {
        let c = catalog();
        let plays: Vec<Play> = (0..5).map(|i| play(&format!("Calm 1-{i}"), 200)).collect();
        let t = taste(&c, &plays, &[]);
        let s = shelves(&c, &t, 1);
        let f = s.iter().find(|x| x.key == "forgotten").expect("forgotten");
        assert!(f.items.iter().all(|i| i.id.starts_with("Calm 1")), "{f:?}");
    }

    #[test]
    fn mixes_per_genre_and_for_discovery() {
        let c = catalog();
        let plays: Vec<Play> = (0..4)
            .flat_map(|i| {
                [
                    play(&format!("Rock 1-{i}"), 5),
                    play(&format!("Calm 1-{i}"), 6),
                ]
            })
            .collect();
        let t = taste(&c, &plays, &[]);
        let m = mixes(&c, &t, 3);
        let rock = m.iter().find(|x| x.arg == "rock").expect("rock mix");
        assert_eq!(rock.key, "genre");
        assert!(rock.items.iter().all(|i| i.starts_with("Rock")), "{rock:?}");
        assert!(m.iter().any(|x| x.arg == "ambient"));
        let d = m.iter().find(|x| x.key == "discover").expect("discover");
        assert!(d.items.iter().all(|i| !plays.iter().any(|p| &p.id == i)));
        // The same for the day, different the next.
        assert_eq!(mixes(&c, &t, 3), m);
        assert_ne!(mixes(&c, &t, 4), m);
    }

    #[test]
    fn without_genre_tags_mixes_go_by_artist() {
        let mut songs = Vec::new();
        for artist in ["Anna", "Ben"] {
            for i in 0..8 {
                let mut s = song(&format!("{artist}-{i}"), artist, "", 2000, 0.5, 120.0);
                s.added = NOW - 100 * DAY;
                songs.push(s);
            }
        }
        let c = Catalog::new(songs);
        let plays = [play("Anna-0", 3), play("Anna-1", 3)];
        let t = taste(&c, &plays, &[]);
        let m = mixes(&c, &t, 1);
        assert!(
            m.iter().any(|x| x.key == "artist" && x.arg == "anna"),
            "{m:?}"
        );
    }
}
