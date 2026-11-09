//! What public databases know about the library's artists, for artist pages
//! and recommendations.
//!
//! Off until the admin switches it on (`onlineMetadata`): then the names of the
//! library's artists go to MusicBrainz and, for an artist page somebody opened,
//! to Wikimedia and ListenBrainz. Without the switch nothing in here goes out.
//! One artist at a time in the background, at the pace the services ask for.

use crate::State;
use bl_json::Value;
use bl_meta::{Book, Depth, Entry, Lookup, Pacer, Service};
use std::collections::{HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

/// A service that does not answer is left alone after this many tries.
const MOST_TRIES: u32 = 5;

struct Wanted {
    name: String,
    mbid: Option<String>,
    language: String,
}

pub struct ArtistIndex {
    path: PathBuf,
    book: RwLock<Book>,
    enabled: AtomicBool,
    /// Bumped when genres came in, not too often (the recommendations rebuild).
    revision: AtomicU64,
    /// Pages somebody is waiting for.
    pages: Mutex<VecDeque<Wanted>>,
    working: Mutex<Option<String>>,
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl ArtistIndex {
    pub fn new(path: PathBuf, enabled: bool) -> ArtistIndex {
        let book = std::fs::read(&path)
            .ok()
            .and_then(|d| bl_json::parse_bytes(&d).ok())
            .map(|v| Book::from_json(&v))
            .unwrap_or_default();
        ArtistIndex {
            path,
            book: RwLock::new(book),
            enabled: AtomicBool::new(enabled),
            revision: AtomicU64::new(0),
            pages: Mutex::new(VecDeque::new()),
            working: Mutex::new(None),
        }
    }

    pub fn set_enabled(&self, on: bool) {
        if self.enabled.swap(on, Ordering::Relaxed) != on {
            self.revision.fetch_add(1, Ordering::Relaxed);
        }
        if !on {
            self.pages.lock().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    fn is_enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::Relaxed)
    }

    /// An artist's genres for recommendations, while the switch is on.
    pub fn genres(&self, name: &str) -> Vec<String> {
        if !self.is_enabled() {
            return Vec::new();
        }
        self.book
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .genres(name)
            .to_vec()
    }

    /// For the admin page.
    pub fn summary(&self) -> Value {
        Value::object()
            .with("enabled", self.is_enabled())
            .with(
                "known",
                self.book.read().unwrap_or_else(|e| e.into_inner()).len(),
            )
            .with(
                "working",
                self.working
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .is_some(),
            )
    }

    /// What is known about an artist for its page; asks when it is not known.
    pub fn info(&self, name: &str, mbid: Option<String>, language: &str) -> Value {
        let answer = Value::object().with("enabled", self.is_enabled());
        if !self.is_enabled() {
            return answer;
        }
        let name = name.trim();
        if name.is_empty() {
            return answer.with("state", "none");
        }
        let now = unix_now();
        let book = self.book.read().unwrap_or_else(|e| e.into_inner());
        let entry = book.get(name);
        let fresh = entry.is_some_and(|e| e.fresh(now));
        match entry.map(|e| (e.artist.as_ref(), e.fetched)) {
            // A page from long ago is shown all the same while it is asked for
            // anew, with its time, so that the app asks again later too.
            Some((Some(a), fetched)) if a.complete => {
                let ready = answer
                    .with("state", "ready")
                    .with("artist", a.to_json())
                    .with("fetched", fetched);
                drop(book);
                if !fresh {
                    self.want_page(name, mbid, language);
                }
                return ready;
            }
            Some((None, _)) if fresh => return answer.with("state", "none"),
            _ => {}
        }
        drop(book);
        self.want_page(name, mbid, language);
        answer.with("state", "pending")
    }

    /// Queues a page for the worker.
    fn want_page(&self, name: &str, mbid: Option<String>, language: &str) {
        let mut pages = self.pages.lock().unwrap_or_else(|e| e.into_inner());
        let key = Book::key(name);
        let working = self
            .working
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_deref()
            .is_some_and(|w| Book::key(w) == key);
        if pages.len() < 64 && !working && !pages.iter().any(|w| Book::key(&w.name) == key) {
            pages.push_back(Wanted {
                name: name.to_string(),
                mbid,
                language: language.to_string(),
            });
        }
    }

    /// The next artist of the library not known yet. A page is left alone:
    /// it is asked for anew when somebody opens it, in their language, and the
    /// genres alone would take its place.
    fn next_in_library(&self, state: &State, tried: &HashSet<String>) -> Option<Wanted> {
        let now = unix_now();
        let lib = state.library();
        let book = self.book.read().unwrap_or_else(|e| e.into_inner());
        for e in lib.tracks().iter() {
            let t = &e.track;
            for name in &t.artists {
                let key = Book::key(name);
                if key.is_empty()
                    || key == "unknown artist"
                    || tried.contains(&key)
                    || book.get(name).is_some_and(|e| e.fresh(now) || e.has_page())
                {
                    continue;
                }
                let mbid = (t.artists.len() == 1 && !t.meta.mb_artist.is_empty())
                    .then(|| t.meta.mb_artist.clone());
                return Some(Wanted {
                    name: name.clone(),
                    mbid,
                    language: "en".to_string(),
                });
            }
        }
        None
    }

    /// Starts the background worker.
    pub fn attach(self: &Arc<Self>, state: &Arc<State>) {
        let index = self.clone();
        let state = Arc::downgrade(state);
        let _ = crate::util::supervise("artists", move || {
            let mut pacer = Pacer::default();
            let start = Instant::now();
            // Tried without an answer: not again until the next start.
            let mut tried = HashSet::new();
            let mut genres_at = Instant::now();
            let mut genres_changed = false;
            loop {
                let Some(s) = state.upgrade() else {
                    return;
                };
                if !index.is_enabled() {
                    drop(s);
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                }
                let page = index
                    .pages
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .pop_front();
                let (wanted, depth) = match page {
                    Some(w) => (Some(w), Depth::Page),
                    None if s.scanning() => (None, Depth::Genres),
                    None => (index.next_in_library(&s, &tried), Depth::Genres),
                };
                drop(s);
                let Some(wanted) = wanted else {
                    if genres_changed {
                        genres_changed = false;
                        index.revision.fetch_add(1, Ordering::Relaxed);
                    }
                    std::thread::sleep(Duration::from_secs(5));
                    continue;
                };
                tried.insert(Book::key(&wanted.name));
                let clock = || start.elapsed().as_secs_f64();
                genres_changed |= index.look_up(&wanted, depth, &mut pacer, &clock, &fetch, &sleep);
                if genres_changed && genres_at.elapsed() > Duration::from_secs(60) {
                    genres_changed = false;
                    genres_at = Instant::now();
                    index.revision.fetch_add(1, Ordering::Relaxed);
                }
            }
        });
    }

    /// Asks about one artist; true when genres came in.
    fn look_up(
        &self,
        wanted: &Wanted,
        depth: Depth,
        pacer: &mut Pacer,
        clock: &dyn Fn() -> f64,
        fetch: &dyn Fn(&str) -> Result<(u16, Vec<u8>), String>,
        sleep: &dyn Fn(f64),
    ) -> bool {
        *self.working.lock().unwrap_or_else(|e| e.into_inner()) = Some(wanted.name.clone());
        let mut lookup = Lookup::new(
            &wanted.name,
            wanted.mbid.as_deref(),
            &wanted.language,
            depth,
        );
        let mut failed = false;
        let mut tries = 0;
        while let Some(request) = lookup.request() {
            // The switch can be turned off at any moment.
            if !self.is_enabled() {
                failed = true;
                break;
            }
            while !pacer.ready(request.service, clock()) {
                sleep(pace_step(request.service));
            }
            pacer.mark(request.service, clock());
            let (status, body) = fetch(&request.url).unwrap_or_else(|e| {
                crate::debug!("artist lookup: {e}");
                (0, Vec::new())
            });
            if let Err(f) = lookup.answer(status, &body) {
                // Busy for a moment, most of the time: the same again in a while.
                tries += 1;
                pacer.hold(f.service, clock(), f.pause * tries as f64);
                if tries >= MOST_TRIES {
                    failed = true;
                    break;
                }
            }
        }
        if failed {
            *self.working.lock().unwrap_or_else(|e| e.into_inner()) = None;
            return false;
        }
        let artist = lookup.finish();
        let genres = artist.as_ref().is_some_and(|a| !a.genres.is_empty());
        let mut book = self.book.write().unwrap_or_else(|e| e.into_inner());
        // A page known before stays when the services find nobody this time.
        let artist = artist.or_else(|| {
            book.get(&wanted.name)
                .filter(|e| e.has_page())
                .and_then(|e| e.artist.clone())
        });
        book.put(
            &wanted.name,
            Entry {
                artist,
                fetched: unix_now(),
                image_saved: false,
            },
        );
        let json = book.to_json().to_json();
        drop(book);
        // Only now: until the page is in the book, it is not asked for again.
        *self.working.lock().unwrap_or_else(|e| e.into_inner()) = None;
        if let Err(e) = crate::util::write_atomic(&self.path, json.as_bytes()) {
            crate::warn!("cannot write {}: {e}", self.path.display());
        }
        genres
    }
}

fn pace_step(service: Service) -> f64 {
    match service {
        Service::Wikimedia => 0.05,
        _ => 0.2,
    }
}

fn sleep(seconds: f64) {
    std::thread::sleep(Duration::from_secs_f64(seconds));
}

fn fetch(url: &str) -> Result<(u16, Vec<u8>), String> {
    let headers = [
        (
            "User-Agent".to_string(),
            bl_meta::user_agent(env!("CARGO_PKG_VERSION")),
        ),
        ("Accept".to_string(), "application/json".to_string()),
    ];
    let opts = bl_net::client::Options {
        max_body: 8 * 1024 * 1024,
        max_redirects: 3,
        ..Default::default()
    };
    bl_net::client::request("GET", url, &headers, &[], &opts).map(|r| (r.status, r.body))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    const ID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

    /// Each test has a file of its own: a lookup writes the book, and a test
    /// running alongside would otherwise start with that artist already known.
    fn index(name: &str, enabled: bool) -> ArtistIndex {
        let path =
            std::env::temp_dir().join(format!("bl-artists-{}-{name}.json", std::process::id()));
        let _ = std::fs::remove_file(&path);
        ArtistIndex::new(path, enabled)
    }

    fn details() -> Vec<u8> {
        format!(r#"{{"id":"{ID}","name":"The Band","genres":[{{"name":"rock","count":3}}]}}"#)
            .into_bytes()
    }

    #[test]
    fn with_the_switch_off_it_answers_nothing_and_asks_nobody() {
        let i = index("off", false);
        let info = i.info("The Band", None, "de");
        assert!(!info.bool_or("enabled", true));
        assert!(i.pages.lock().unwrap().is_empty(), "nothing was queued");
        assert!(i.genres("The Band").is_empty());
        // A lookup that was under way stops before the next request.
        let asked = RefCell::new(0);
        let wanted = Wanted {
            name: "The Band".into(),
            mbid: None,
            language: "en".into(),
        };
        let found = i.look_up(
            &wanted,
            Depth::Genres,
            &mut Pacer::default(),
            &|| 100.0,
            &|_| {
                *asked.borrow_mut() += 1;
                Ok((200, Vec::new()))
            },
            &|_| {},
        );
        assert!(!found);
        assert_eq!(*asked.borrow(), 0);
        assert_eq!(i.book.read().unwrap().len(), 0);
    }

    #[test]
    fn it_asks_keeps_the_pace_and_remembers() {
        let i = index("asks", true);
        let wanted = Wanted {
            name: "The Band".into(),
            mbid: Some(ID.to_string()),
            language: "en".into(),
        };
        let urls = RefCell::new(Vec::new());
        let time = RefCell::new(100.0);
        let found = i.look_up(
            &wanted,
            Depth::Genres,
            &mut Pacer::default(),
            &|| *time.borrow(),
            &|u| {
                urls.borrow_mut().push(u.to_string());
                Ok((200, details()))
            },
            &|s| *time.borrow_mut() += s,
        );
        assert!(found);
        assert_eq!(urls.borrow().len(), 1);
        assert!(urls.borrow()[0].contains(ID));
        assert_eq!(i.genres("the band"), ["rock"]);
        // It is known now: no page yet, so a page is asked for.
        let info = i.info("The Band", None, "de");
        assert_eq!(info.str_or("state", ""), "pending");
        assert_eq!(i.pages.lock().unwrap().len(), 1);
        let again = i.info("the  band", None, "de");
        assert_eq!(again.str_or("state", ""), "pending");
        assert_eq!(i.pages.lock().unwrap().len(), 1, "asked for once");
        let _ = std::fs::remove_file(&i.path);
    }

    #[test]
    fn a_service_that_is_down_leaves_nothing_behind() {
        let i = index("down", true);
        let wanted = Wanted {
            name: "Nobody Yet".into(),
            mbid: None,
            language: "en".into(),
        };
        let time = RefCell::new(100.0);
        let found = i.look_up(
            &wanted,
            Depth::Genres,
            &mut Pacer::default(),
            &|| *time.borrow(),
            &|_| Err("no route".to_string()),
            &|s| *time.borrow_mut() += s,
        );
        assert!(!found);
        assert!(i.book.read().unwrap().get("nobody yet").is_none());
    }

    #[test]
    fn a_complete_entry_is_served_and_turning_it_off_hides_it() {
        let i = index("complete", true);
        i.book.write().unwrap().put(
            "The Band",
            Entry {
                artist: Some(bl_meta::Artist {
                    name: "The Band".into(),
                    genres: vec!["rock".into()],
                    complete: true,
                    ..Default::default()
                }),
                fetched: unix_now(),
                image_saved: false,
            },
        );
        let info = i.info("the band", None, "de");
        assert_eq!(info.str_or("state", ""), "ready");
        assert_eq!(info.get("artist").str_or("name", ""), "The Band");
        let rev = i.revision();
        i.set_enabled(false);
        assert!(i.revision() > rev);
        assert!(i.genres("the band").is_empty());
        assert!(!i.info("the band", None, "de").bool_or("enabled", true));
    }

    #[test]
    fn a_page_from_long_ago_is_served_at_once_and_asked_for_anew() {
        let i = index("old", true);
        let long_ago = unix_now() - 40 * 86_400;
        i.book.write().unwrap().put(
            "The Band",
            Entry {
                artist: Some(bl_meta::Artist {
                    name: "The Band".into(),
                    biography: "A band.".into(),
                    complete: true,
                    ..Default::default()
                }),
                fetched: long_ago,
                image_saved: false,
            },
        );
        let info = i.info("the band", None, "de");
        assert_eq!(info.str_or("state", ""), "ready");
        assert_eq!(info.get("artist").str_or("biography", ""), "A band.");
        assert_eq!(info.get("fetched").as_u64(), Some(long_ago));
        assert_eq!(i.pages.lock().unwrap().len(), 1, "asked for anew");
        i.info("The Band", None, "de");
        assert_eq!(i.pages.lock().unwrap().len(), 1, "once");

        // The services find nobody this time: the page stays, from now.
        let wanted = i.pages.lock().unwrap().pop_front().unwrap();
        let time = RefCell::new(100.0);
        let found = i.look_up(
            &wanted,
            Depth::Page,
            &mut Pacer::default(),
            &|| *time.borrow(),
            &|_| Ok((200, br#"{"artists":[]}"#.to_vec())),
            &|s| *time.borrow_mut() += s,
        );
        assert!(!found);
        let book = i.book.read().unwrap();
        let e = book.get("the band").unwrap();
        assert!(e.has_page() && e.fresh(unix_now()));
        assert_eq!(e.artist.as_ref().unwrap().biography, "A band.");
        drop(book);
        assert!(i.working.lock().unwrap().is_none());
        let _ = std::fs::remove_file(&i.path);
    }
}
