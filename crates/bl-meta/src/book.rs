//! What has been looked up, so that nobody is asked twice.

use crate::Artist;
use bl_json::Value;
use std::collections::HashMap;

/// How long an answer counts (seconds).
const KNOWN_FOR: u64 = 30 * 86_400;
/// Not found: the databases grow, but not by the day.
const UNKNOWN_FOR: u64 = 7 * 86_400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// `None`: nothing to be sure of was found.
    pub artist: Option<Artist>,
    /// When it was asked (seconds since the Unix epoch).
    pub fetched: u64,
    /// The picture was saved on this device.
    pub image_saved: bool,
}

impl Entry {
    pub fn fresh(&self, now: u64) -> bool {
        let lasts = if self.artist.is_some() {
            KNOWN_FOR
        } else {
            UNKNOWN_FOR
        };
        now.saturating_sub(self.fetched) < lasts
    }
}

/// Artists by name, in the spelling that makes the same name meet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Book {
    entries: HashMap<String, Entry>,
}

impl Book {
    pub fn key(name: &str) -> String {
        name.split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    }

    pub fn get(&self, name: &str) -> Option<&Entry> {
        self.entries.get(&Book::key(name))
    }

    pub fn get_mut(&mut self, name: &str) -> Option<&mut Entry> {
        self.entries.get_mut(&Book::key(name))
    }

    pub fn put(&mut self, name: &str, entry: Entry) {
        self.entries.insert(Book::key(name), entry);
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The genres of an artist for recommendations; one of several of that
    /// name says nothing.
    pub fn genres(&self, name: &str) -> &[String] {
        self.get(name)
            .and_then(|e| e.artist.as_ref())
            .filter(|a| !a.ambiguous)
            .map_or(&[], |a| a.genres.as_slice())
    }

    pub fn to_json(&self) -> Value {
        let mut keys: Vec<&String> = self.entries.keys().collect();
        keys.sort();
        Value::Array(
            keys.into_iter()
                .map(|k| {
                    let e = &self.entries[k];
                    let mut v = Value::object()
                        .with("key", k.as_str())
                        .with("fetched", e.fetched);
                    if e.image_saved {
                        v = v.with("imageSaved", true);
                    }
                    if let Some(a) = &e.artist {
                        v = v.with("artist", a.to_json());
                    }
                    v
                })
                .collect(),
        )
    }

    pub fn from_json(v: &Value) -> Book {
        let mut entries = HashMap::new();
        for e in v.as_array().into_iter().flatten() {
            if let Some(k) = e.get("key").as_str() {
                let artist = e.get("artist");
                entries.insert(
                    k.to_string(),
                    Entry {
                        artist: artist.as_object().map(|_| Artist::from_json(artist)),
                        fetched: e.get("fetched").as_u64().unwrap_or(0),
                        image_saved: e.bool_or("imageSaved", false),
                    },
                );
            }
        }
        Book { entries }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn artist(genres: &[&str], ambiguous: bool) -> Artist {
        Artist {
            name: "A".into(),
            genres: genres.iter().map(|g| g.to_string()).collect(),
            ambiguous,
            ..Artist::default()
        }
    }

    #[test]
    fn answers_keep_for_a_while_and_not_knowing_for_less() {
        let known = Entry {
            artist: Some(artist(&[], false)),
            fetched: 1000,
            image_saved: false,
        };
        assert!(known.fresh(1000 + 29 * 86_400));
        assert!(!known.fresh(1000 + 31 * 86_400));
        let unknown = Entry {
            artist: None,
            ..known
        };
        assert!(unknown.fresh(1000 + 6 * 86_400));
        assert!(!unknown.fresh(1000 + 8 * 86_400));
    }

    #[test]
    fn names_meet_and_it_survives_saving() {
        let mut b = Book::default();
        b.put(
            "The  Band",
            Entry {
                artist: Some(artist(&["rock"], false)),
                fetched: 5,
                image_saved: true,
            },
        );
        b.put(
            "Nobody",
            Entry {
                artist: None,
                fetched: 6,
                image_saved: false,
            },
        );
        assert_eq!(b.genres("the band"), ["rock"]);
        let again = Book::from_json(&bl_json::parse(&b.to_json().to_json()).unwrap());
        assert_eq!(again, b);
        assert!(again.get("nobody").unwrap().artist.is_none());
    }

    #[test]
    fn one_of_several_of_that_name_says_nothing_about_genres() {
        let mut b = Book::default();
        b.put(
            "Twin",
            Entry {
                artist: Some(artist(&["rock"], true)),
                fetched: 1,
                image_saved: false,
            },
        );
        assert!(b.genres("twin").is_empty());
    }
}
