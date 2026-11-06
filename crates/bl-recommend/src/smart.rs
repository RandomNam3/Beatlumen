//! Smart lists: rules instead of songs. Nothing but the rules is kept: a list
//! is worked out again whenever somebody looks, so a new song that fits joins
//! by itself and one that stopped fitting leaves by itself.

use crate::catalog::Catalog;
use crate::rng::Rng;
use crate::taste::Taste;
use bl_json::Value;
use std::collections::HashSet;

/// Most songs a list may hold.
pub const MOST_SONGS: usize = 5000;
const DAY: u64 = 86_400;

/// What a song has to be.
#[derive(Debug, Clone, PartialEq)]
pub enum Rule {
    /// Has this label: genres, moods, decades, tempo, keys, languages, record
    /// labels (`genre:rock`) and those given by people.
    Label(String),
    Artist(String),
    Year {
        min: Option<i32>,
        max: Option<i32>,
    },
    /// Tempo in beats per minute, half and double ones not counted.
    Bpm {
        min: Option<f32>,
        max: Option<f32>,
    },
    /// Rated at least this (0..1).
    Rating(f32),
    Favorite,
    /// Played all the way through at least this often.
    Plays(u32),
    PlayedWithin {
        days: u32,
    },
    /// Into the library within this many days.
    AddedWithin {
        days: u32,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct Condition {
    pub rule: Rule,
    /// It must not hold.
    pub not: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// Like a shelf: by artist and album.
    Artist,
    Newest,
    MostPlayed,
    /// What was not heard for the longest time first.
    LongestAgo,
    Random,
}

impl Order {
    fn name(self) -> &'static str {
        match self {
            Order::Artist => "artist",
            Order::Newest => "newest",
            Order::MostPlayed => "mostPlayed",
            Order::LongestAgo => "longestAgo",
            Order::Random => "random",
        }
    }
    fn parse(s: &str) -> Order {
        match s {
            "newest" => Order::Newest,
            "mostPlayed" => Order::MostPlayed,
            "longestAgo" => Order::LongestAgo,
            "random" => Order::Random,
            _ => Order::Artist,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SmartList {
    pub id: String,
    pub name: String,
    pub conditions: Vec<Condition>,
    /// One condition is enough, else all have to hold.
    pub any: bool,
    pub order: Order,
    /// 0: as many as there are.
    pub limit: usize,
}

fn finite_f32(v: &Value, k: &str) -> Option<f32> {
    v.get(k).as_f32().filter(|x| x.is_finite())
}

impl Condition {
    fn to_json(&self) -> Value {
        let v = Value::object().with("not", self.not);
        match &self.rule {
            Rule::Label(l) => v.with("kind", "label").with("value", l.as_str()),
            Rule::Artist(a) => v.with("kind", "artist").with("value", a.as_str()),
            Rule::Year { min, max } => v.with("kind", "year").with("min", *min).with("max", *max),
            Rule::Bpm { min, max } => v.with("kind", "bpm").with("min", *min).with("max", *max),
            Rule::Rating(r) => v.with("kind", "rating").with("min", *r),
            Rule::Favorite => v.with("kind", "favorite"),
            Rule::Plays(n) => v.with("kind", "plays").with("min", *n),
            Rule::PlayedWithin { days } => v.with("kind", "playedWithin").with("days", *days),
            Rule::AddedWithin { days } => v.with("kind", "addedWithin").with("days", *days),
        }
    }

    fn from_json(v: &Value) -> Option<Condition> {
        let text = || Some(v.get("value").as_str()?.trim().to_string()).filter(|t| !t.is_empty());
        let year = |k: &str| v.get(k).as_i64().map(|y| y as i32);
        let days = || v.get("days").as_u64().map(|d| d.min(365 * 100) as u32);
        let rule = match v.str_or("kind", "") {
            "label" => Rule::Label(text()?),
            "artist" => Rule::Artist(text()?),
            "year" => Rule::Year {
                min: year("min"),
                max: year("max"),
            },
            "bpm" => Rule::Bpm {
                min: finite_f32(v, "min"),
                max: finite_f32(v, "max"),
            },
            "rating" => Rule::Rating(finite_f32(v, "min")?.clamp(0.0, 1.0)),
            "favorite" => Rule::Favorite,
            "plays" => Rule::Plays(v.get("min").as_u64()?.min(u32::MAX as u64) as u32),
            "playedWithin" => Rule::PlayedWithin { days: days()? },
            "addedWithin" => Rule::AddedWithin { days: days()? },
            _ => return None,
        };
        Some(Condition {
            rule,
            not: v.bool_or("not", false),
        })
    }
}

impl SmartList {
    pub fn to_json(&self) -> Value {
        Value::object()
            .with("id", self.id.as_str())
            .with("name", self.name.as_str())
            .with(
                "rules",
                Value::Array(self.conditions.iter().map(Condition::to_json).collect()),
            )
            .with("any", self.any)
            .with("order", self.order.name())
            .with("limit", self.limit)
    }

    /// Rules it cannot read (from a newer version) are left out.
    pub fn from_json(v: &Value) -> SmartList {
        SmartList {
            id: v.str_or("id", "").to_string(),
            name: v.str_or("name", "").trim().to_string(),
            conditions: v
                .get("rules")
                .as_array()
                .map(|a| a.iter().filter_map(Condition::from_json).collect())
                .unwrap_or_default(),
            any: v.bool_or("any", false),
            order: Order::parse(v.str_or("order", "")),
            limit: v.get("limit").as_u64().unwrap_or(0).min(MOST_SONGS as u64) as usize,
        }
    }

    fn holds(&self, c: &Catalog, taste: &Taste, favorites: &HashSet<u32>, song: u32) -> bool {
        if self.conditions.is_empty() {
            return false;
        }
        let one = |cond: &Condition| rule_holds(&cond.rule, c, taste, favorites, song) != cond.not;
        if self.any {
            self.conditions.iter().any(one)
        } else {
            self.conditions.iter().all(one)
        }
    }

    /// The ids of the songs that fit, in the list's order. `seed`: for the
    /// random order (another one, another order).
    pub fn songs(&self, c: &Catalog, taste: &Taste, favorites: &[&str], seed: u64) -> Vec<String> {
        let favorites: HashSet<u32> = favorites.iter().filter_map(|id| c.index_of(id)).collect();
        let mut songs: Vec<u32> = (0..c.len() as u32)
            .filter(|i| self.holds(c, taste, &favorites, *i))
            .collect();
        match self.order {
            Order::Artist => {}
            Order::Newest => songs.sort_by_key(|i| std::cmp::Reverse(c.items[*i as usize].added)),
            Order::MostPlayed => {
                songs.sort_by_key(|i| std::cmp::Reverse(taste.plays[*i as usize]));
            }
            Order::LongestAgo => songs.sort_by_key(|i| taste.last[*i as usize]),
            Order::Random => {
                let mut rng = Rng::new(seed);
                for i in (1..songs.len()).rev() {
                    songs.swap(i, (rng.next_u64() % (i as u64 + 1)) as usize);
                }
            }
        }
        let most = if self.limit == 0 {
            MOST_SONGS
        } else {
            self.limit.min(MOST_SONGS)
        };
        songs.truncate(most);
        songs
            .into_iter()
            .map(|i| c.ids[i as usize].clone())
            .collect()
    }
}

fn rule_holds(
    rule: &Rule,
    c: &Catalog,
    taste: &Taste,
    favorites: &HashSet<u32>,
    song: u32,
) -> bool {
    let item = &c.items[song as usize];
    let i = song as usize;
    match rule {
        Rule::Label(name) => c.labels_of(song).any(|l| l == name),
        Rule::Artist(name) => c.has_artist(song, name),
        Rule::Year { min, max } => item
            .year
            .is_some_and(|y| min.is_none_or(|m| y >= m) && max.is_none_or(|m| y <= m)),
        Rule::Bpm { min, max } => item
            .tempo
            .is_some_and(|b| min.is_none_or(|m| b >= m) && max.is_none_or(|m| b <= m)),
        Rule::Rating(at_least) => item.rating.is_some_and(|r| r >= *at_least),
        Rule::Favorite => favorites.contains(&song),
        Rule::Plays(n) => taste.plays[i] >= *n,
        Rule::PlayedWithin { days } => {
            taste.last[i] > 0 && taste.now.saturating_sub(taste.last[i]) <= *days as u64 * DAY
        }
        Rule::AddedWithin { days } => {
            item.added > 0 && taste.now.saturating_sub(item.added) <= *days as u64 * DAY
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{Features, Song, Tagged};
    use crate::taste::{Play, Signals};

    const NOW: u64 = 1_000 * DAY;

    fn song(id: &str, artist: &str, genre: &str, year: i32, added_days_ago: u64, bpm: f32) -> Song {
        Song {
            id: id.to_string(),
            artists: vec![artist.to_string()],
            album_id: format!("{artist}-album"),
            genre: genre.to_string(),
            year: Some(year),
            added: NOW - added_days_ago * DAY,
            features: None,
            labels: Vec::new(),
            tagged: Tagged {
                bpm: Some(bpm),
                ..Tagged::default()
            },
        }
    }

    fn catalog() -> Catalog {
        let mut rated = song("rated", "C", "jazz", 1960, 400, 90.0);
        rated.tagged.rating = Some(0.9);
        let mut labelled = song("labelled", "A", "rock", 1995, 3, 170.0);
        labelled.labels = vec!["Party".to_string()];
        labelled.features = Some(Features {
            bpm: 170.0,
            beat: 0.5,
            key: 0,
            minor: false,
            energy: 0.9,
            brightness: 0.5,
        });
        Catalog::new(vec![
            song("a1", "A", "rock", 1991, 300, 120.0),
            song("b1", "B", "pop", 2004, 10, 128.0),
            rated,
            labelled,
        ])
    }

    fn taste(c: &Catalog) -> Taste {
        let plays = [
            Play {
                id: "b1".into(),
                t: NOW - 2 * DAY,
                skip: false,
            },
            Play {
                id: "b1".into(),
                t: NOW - DAY,
                skip: false,
            },
            Play {
                id: "a1".into(),
                t: NOW - 200 * DAY,
                skip: false,
            },
        ];
        Taste::new(
            c,
            &Signals {
                plays: &plays,
                favorites: &[],
                playlists: Vec::new(),
                now: NOW,
            },
        )
    }

    fn list(conditions: Vec<(Rule, bool)>, any: bool, order: Order, limit: usize) -> SmartList {
        SmartList {
            id: "x".into(),
            name: "Test".into(),
            conditions: conditions
                .into_iter()
                .map(|(rule, not)| Condition { rule, not })
                .collect(),
            any,
            order,
            limit,
        }
    }

    fn run(c: &Catalog, l: &SmartList) -> Vec<String> {
        l.songs(c, &taste(c), &["rated"], 7)
    }

    fn one(rule: Rule) -> SmartList {
        list(vec![(rule, false)], false, Order::Artist, 0)
    }

    #[test]
    fn every_rule() {
        let c = catalog();
        assert_eq!(
            run(&c, &one(Rule::Label("genre:rock".into()))),
            ["a1", "labelled"]
        );
        assert_eq!(run(&c, &one(Rule::Label("Party".into()))), ["labelled"]);
        assert_eq!(
            run(&c, &one(Rule::Label("decade:1990".into()))),
            ["a1", "labelled"]
        );
        assert_eq!(run(&c, &one(Rule::Artist("b".into()))), ["b1"]);
        assert_eq!(
            run(
                &c,
                &one(Rule::Year {
                    min: Some(1990),
                    max: Some(1999)
                })
            ),
            ["a1", "labelled"]
        );
        assert_eq!(
            run(
                &c,
                &one(Rule::Bpm {
                    min: Some(125.0),
                    max: None
                })
            ),
            ["b1", "labelled"]
        );
        assert_eq!(run(&c, &one(Rule::Rating(0.8))), ["rated"]);
        assert_eq!(run(&c, &one(Rule::Favorite)), ["rated"]);
        assert_eq!(run(&c, &one(Rule::Plays(2))), ["b1"]);
        assert_eq!(run(&c, &one(Rule::PlayedWithin { days: 7 })), ["b1"]);
        assert_eq!(
            run(&c, &one(Rule::AddedWithin { days: 30 })),
            ["b1", "labelled"]
        );
    }

    #[test]
    fn all_any_and_not() {
        let c = catalog();
        let rock = (Rule::Label("genre:rock".into()), false);
        let new = (Rule::AddedWithin { days: 30 }, false);
        assert_eq!(
            run(
                &c,
                &list(vec![rock.clone(), new.clone()], false, Order::Artist, 0)
            ),
            ["labelled"]
        );
        assert_eq!(
            run(&c, &list(vec![rock.clone(), new], true, Order::Artist, 0)),
            ["a1", "b1", "labelled"]
        );
        assert_eq!(
            run(
                &c,
                &list(
                    vec![(Rule::Label("genre:rock".into()), true)],
                    false,
                    Order::Artist,
                    0
                )
            ),
            ["b1", "rated"]
        );
        // Never played: no plays at all.
        assert_eq!(
            run(
                &c,
                &list(vec![(Rule::Plays(1), true)], false, Order::Artist, 0)
            ),
            ["rated", "labelled"]
        );
    }

    #[test]
    fn no_rules_no_songs() {
        let c = catalog();
        assert!(run(&c, &list(Vec::new(), false, Order::Artist, 0)).is_empty());
    }

    #[test]
    fn orders_and_limit() {
        let c = catalog();
        let any = (
            Rule::Year {
                min: Some(0),
                max: None,
            },
            false,
        );
        let ordered = |o, limit| run(&c, &list(vec![any.clone()], false, o, limit));
        assert_eq!(ordered(Order::Newest, 2), ["labelled", "b1"]);
        assert_eq!(ordered(Order::MostPlayed, 1), ["b1"]);
        // Never heard first, then what was heard longest ago.
        assert_eq!(ordered(Order::LongestAgo, 0)[2..], ["a1", "b1"]);
        let random = ordered(Order::Random, 0);
        assert_eq!(random.len(), 4);
        let mut sorted = random.clone();
        sorted.sort();
        assert_eq!(sorted, ["a1", "b1", "labelled", "rated"]);
    }

    #[test]
    fn it_survives_saving_and_unknown_rules_are_left_out() {
        let l = list(
            vec![
                (Rule::Label("genre:rock".into()), false),
                (Rule::Artist("A".into()), true),
                (
                    Rule::Year {
                        min: Some(1990),
                        max: None,
                    },
                    false,
                ),
                (
                    Rule::Bpm {
                        min: Some(100.0),
                        max: Some(130.0),
                    },
                    false,
                ),
                (Rule::Rating(0.5), false),
                (Rule::Favorite, false),
                (Rule::Plays(3), false),
                (Rule::PlayedWithin { days: 30 }, true),
                (Rule::AddedWithin { days: 7 }, false),
            ],
            true,
            Order::LongestAgo,
            50,
        );
        let back = SmartList::from_json(&bl_json::parse(&l.to_json().to_json()).unwrap());
        assert_eq!(back, l);
        let future = bl_json::parse(
            r#"{"id":"f","name":"F","rules":[{"kind":"fromTheFuture"},{"kind":"favorite"},{"kind":"label","value":"  "}],"limit":999999}"#,
        )
        .unwrap();
        let read = SmartList::from_json(&future);
        assert_eq!(read.conditions.len(), 1);
        assert_eq!(read.limit, MOST_SONGS);
    }
}
