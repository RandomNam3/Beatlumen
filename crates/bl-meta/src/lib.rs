//! What public databases know about an artist, for artist pages and
//! recommendations: MusicBrainz, Wikidata, Wikipedia, Wikimedia Commons and
//! ListenBrainz.
//!
//! This crate has no network. It builds the addresses, reads the answers and
//! knows how often each service may be asked; the request is made by whoever
//! uses it, and only when the listener switched this on. So every test runs
//! without going out.

#![forbid(unsafe_code)]

mod book;
mod lookup;

pub use book::{Book, Entry};
pub use lookup::{Depth, Failure, Lookup, Pacer, Request, Service};

use bl_json::Value;

/// Who is asking: MusicBrainz requires a way to reach the program's authors.
pub fn user_agent(version: &str) -> String {
    format!("Beatlumen/{version} ( https://github.com/RandomNam3/Beatlumen )")
}

/// An artist related to another one.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Related {
    pub name: String,
    pub mbid: String,
    pub begin: String,
    pub end: String,
    pub ended: bool,
}

impl Related {
    fn to_json(&self) -> Value {
        Value::object()
            .with("name", self.name.as_str())
            .with("mbid", self.mbid.as_str())
            .with("begin", self.begin.as_str())
            .with("end", self.end.as_str())
            .with("ended", self.ended)
    }
    fn from_json(v: &Value) -> Related {
        Related {
            name: v.str_or("name", "").to_string(),
            mbid: v.str_or("mbid", "").to_string(),
            begin: v.str_or("begin", "").to_string(),
            end: v.str_or("end", "").to_string(),
            ended: v.bool_or("ended", false),
        }
    }
}

/// A page about an artist elsewhere.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Link {
    /// "homepage", "bandcamp", "soundcloud", "youtube", "wikipedia", ...
    pub kind: String,
    pub url: String,
}

/// What is known about an artist.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Artist {
    pub mbid: String,
    pub name: String,
    /// "person", "group", "orchestra", "choir", "character", "other".
    pub kind: String,
    /// There are others of this name: it may be the wrong one.
    pub ambiguous: bool,
    /// ISO country code, where it comes from and where it began.
    pub country: String,
    pub area: String,
    pub begin_area: String,
    pub begin: String,
    pub end: String,
    pub ended: bool,
    /// Genres people agree on most, first.
    pub genres: Vec<String>,
    pub members: Vec<Related>,
    pub groups: Vec<Related>,
    pub similar: Vec<Related>,
    pub links: Vec<Link>,
    /// A few sentences from Wikipedia and the page they are from.
    pub biography: String,
    pub biography_url: String,
    /// A picture from Wikimedia Commons, its page and who made it.
    pub image_url: String,
    pub image_page: String,
    pub image_credit: String,
    /// The whole page was asked for and not only the genres.
    pub complete: bool,
}

impl Artist {
    pub fn to_json(&self) -> Value {
        let list = |l: &[Related]| Value::Array(l.iter().map(Related::to_json).collect());
        Value::object()
            .with("mbid", self.mbid.as_str())
            .with("name", self.name.as_str())
            .with("kind", self.kind.as_str())
            .with("ambiguous", self.ambiguous)
            .with("country", self.country.as_str())
            .with("area", self.area.as_str())
            .with("beginArea", self.begin_area.as_str())
            .with("begin", self.begin.as_str())
            .with("end", self.end.as_str())
            .with("ended", self.ended)
            .with("genres", self.genres.clone())
            .with("members", list(&self.members))
            .with("groups", list(&self.groups))
            .with("similar", list(&self.similar))
            .with(
                "links",
                Value::Array(
                    self.links
                        .iter()
                        .map(|l| {
                            Value::object()
                                .with("kind", l.kind.as_str())
                                .with("url", l.url.as_str())
                        })
                        .collect(),
                ),
            )
            .with("biography", self.biography.as_str())
            .with("biographyUrl", self.biography_url.as_str())
            .with("imageUrl", self.image_url.as_str())
            .with("imagePage", self.image_page.as_str())
            .with("imageCredit", self.image_credit.as_str())
            .with("complete", self.complete)
    }

    pub fn from_json(v: &Value) -> Artist {
        let list = |k: &str| -> Vec<Related> {
            v.get(k)
                .as_array()
                .map(|a| a.iter().map(Related::from_json).collect())
                .unwrap_or_default()
        };
        Artist {
            mbid: v.str_or("mbid", "").to_string(),
            name: v.str_or("name", "").to_string(),
            kind: v.str_or("kind", "").to_string(),
            ambiguous: v.bool_or("ambiguous", false),
            country: v.str_or("country", "").to_string(),
            area: v.str_or("area", "").to_string(),
            begin_area: v.str_or("beginArea", "").to_string(),
            begin: v.str_or("begin", "").to_string(),
            end: v.str_or("end", "").to_string(),
            ended: v.bool_or("ended", false),
            genres: v
                .get("genres")
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(|g| g.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default(),
            members: list("members"),
            groups: list("groups"),
            similar: list("similar"),
            links: v
                .get("links")
                .as_array()
                .map(|a| {
                    a.iter()
                        .map(|l| Link {
                            kind: l.str_or("kind", "").to_string(),
                            url: l.str_or("url", "").to_string(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            biography: v.str_or("biography", "").to_string(),
            biography_url: v.str_or("biographyUrl", "").to_string(),
            image_url: v.str_or("imageUrl", "").to_string(),
            image_page: v.str_or("imagePage", "").to_string(),
            image_credit: v.str_or("imageCredit", "").to_string(),
            complete: v.bool_or("complete", false),
        }
    }
}
