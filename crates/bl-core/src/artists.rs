//! What public databases know about artists, for artist pages and
//! recommendations. Off until the listener switches it on: then artists' names
//! go to MusicBrainz, Wikimedia and ListenBrainz. Without the switch nothing in
//! here makes a request, and what was found earlier is not shown either.
//!
//! One thing at a time and in the background; playback never waits for it.
//! A server of the listener's that has this switched on is asked first.

use crate::core::{Core, Req};
use crate::host::{Host, HttpRequest, HttpResponse};
use crate::server::ServerState;
use bl_json::Value;
use bl_meta::{Book, Depth, Entry, Lookup, Pacer};
use std::collections::{HashSet, VecDeque};

const STORAGE_KEY: &str = "artists.json";
const IMAGE_PREFIX: &str = "artist-images/";
/// Largest picture kept (bytes).
const MAX_IMAGE: usize = 4 * 1024 * 1024;
/// A pending answer from the server is asked for again after this (seconds).
const SERVER_RETRY: f64 = 3.0;
/// A service that does not answer is left alone after this many tries.
const MOST_TRIES: u32 = 5;

struct Wanted {
    name: String,
    mbid: Option<String>,
    depth: Depth,
    /// Our server was already asked.
    asked_server: bool,
}

struct Running {
    name: String,
    lookup: Lookup,
    waiting: bool,
    /// Asked in vain so far.
    tries: u32,
}

#[derive(Default)]
pub(crate) struct Artists {
    book: Book,
    loaded: bool,
    wanted: VecDeque<Wanted>,
    running: Option<Running>,
    /// A question to our server is on its way.
    asking_server: bool,
    /// Names waiting for the server's answer, and when to ask again.
    server_again: Option<(Wanted, f64)>,
    pacer: Pacer,
    /// A picture on its way: the artist's name.
    image: Option<String>,
    images_wanted: VecDeque<(String, String)>,
    save_at: Option<f64>,
    /// Bumped when something new is known (the artist page looks again).
    pub rev: u64,
    /// Bumped when genres came in, not too often (recommendations rebuild).
    pub genres_rev: u64,
    genres_changed: bool,
    genres_at: f64,
    /// The library as last looked through.
    library_seen: u64,
}

impl Artists {
    fn wanted_contains(&self, name: &str, depth: Depth) -> bool {
        let key = Book::key(name);
        self.wanted
            .iter()
            .any(|w| Book::key(&w.name) == key && w.depth == depth)
            || self
                .running
                .as_ref()
                .is_some_and(|r| Book::key(&r.name) == key)
    }

    /// Nothing more is to be asked.
    fn stop(&mut self) {
        self.wanted.clear();
        self.running = None;
        self.server_again = None;
        self.images_wanted.clear();
        self.image = None;
        self.asking_server = false;
    }
}

fn image_key(name: &str) -> String {
    format!(
        "{IMAGE_PREFIX}{:016x}",
        crate::library::fnv(&Book::key(name))
    )
}

fn user_agent() -> String {
    bl_meta::user_agent(env!("CARGO_PKG_VERSION"))
}

fn get(url: &str) -> HttpRequest {
    HttpRequest {
        method: "GET".into(),
        url: url.to_string(),
        headers: vec![
            ("User-Agent".to_string(), user_agent()),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body: Vec::new(),
    }
}

impl Core {
    fn artist_language(&self) -> String {
        self.settings
            .language
            .split(['-', '_'])
            .next()
            .filter(|l| l.len() == 2)
            .unwrap_or("en")
            .to_lowercase()
    }

    fn artists_load(&mut self, host: &mut dyn Host) {
        if !self.artists.loaded {
            self.artists.loaded = true;
            if let Some(data) = host.storage_read(STORAGE_KEY) {
                if let Ok(v) = bl_json::parse_bytes(&data) {
                    self.artists.book = Book::from_json(&v);
                }
            }
            self.artists.genres_rev += 1;
        }
    }

    /// The artist's id, when the tags of one of their songs name it.
    fn artist_mbid(&self, name: &str) -> Option<String> {
        self.library
            .artist_tracks(name)
            .into_iter()
            .filter(|t| t.artists.len() == 1)
            .find(|t| !t.meta.mb_artist.is_empty())
            .map(|t| t.meta.mb_artist.clone())
    }

    /// Looks through the library for artists not known yet. Songs of a server
    /// are left to the server, which has its own switch. A page is left alone:
    /// it is asked for anew when it is opened, and the genres alone would take
    /// its place.
    fn artists_look_through_library(&mut self, now: u64) {
        if self.artists.library_seen == self.library.revision {
            return;
        }
        self.artists.library_seen = self.library.revision;
        let mut seen = HashSet::new();
        for t in &self.library.tracks {
            if crate::library::is_server_id(&t.id) {
                continue;
            }
            for (i, name) in t.artists.iter().enumerate() {
                let key = Book::key(name);
                if key.is_empty() || key == "unknown artist" || !seen.insert(key) {
                    continue;
                }
                if self
                    .artists
                    .book
                    .get(name)
                    .is_some_and(|e| e.fresh(now) || e.has_page())
                    || self.artists.wanted_contains(name, Depth::Genres)
                {
                    continue;
                }
                let mbid = (i == 0 && t.artists.len() == 1 && !t.meta.mb_artist.is_empty())
                    .then(|| t.meta.mb_artist.clone());
                self.artists.wanted.push_back(Wanted {
                    name: name.clone(),
                    mbid,
                    depth: Depth::Genres,
                    asked_server: true,
                });
            }
        }
    }

    /// One step per tick, at the pace the services ask for.
    pub(crate) fn poll_artists(&mut self, host: &mut dyn Host) {
        if !self.settings.online_metadata {
            return;
        }
        self.artists_load(host);
        let now = host.now();
        let unix = host.unix_time() as u64;
        self.artists_look_through_library(unix);
        if let Some((_, at)) = &self.artists.server_again {
            if *at <= now && !self.artists.asking_server {
                if let Some((w, _)) = self.artists.server_again.take() {
                    self.artists.wanted.push_front(w);
                }
            }
        }
        if self.artists.running.is_none() && !self.artists.asking_server {
            self.artists_start_next(host, unix);
        }
        self.artists_step(host, now, unix);
        self.artists_image_step(host, now);
        if self.artists.genres_changed
            && (self.artists.wanted.is_empty() || now - self.artists.genres_at > 60.0)
        {
            self.artists.genres_changed = false;
            self.artists.genres_at = now;
            self.artists.genres_rev += 1;
        }
        if self.artists.save_at.is_some_and(|t| t <= now) {
            self.artists.save_at = None;
            let json = self.artists.book.to_json().to_json();
            host.storage_write(STORAGE_KEY, json.as_bytes());
        }
    }

    fn artists_start_next(&mut self, host: &mut dyn Host, unix: u64) {
        while let Some(w) = self.artists.wanted.pop_front() {
            let done = self.artists.book.get(&w.name).is_some_and(|e| {
                e.fresh(unix)
                    && (w.depth == Depth::Genres || e.artist.as_ref().is_none_or(|a| a.complete))
            });
            if done {
                continue;
            }
            if !w.asked_server && matches!(self.server.state, ServerState::Connected(_)) {
                let path = format!(
                    "{}?name={}&lang={}",
                    bl_net::api::paths::ARTIST_INFO,
                    bl_net::http::percent_encode(&w.name),
                    self.artist_language()
                );
                let id = host.http_request(HttpRequest {
                    method: "GET".into(),
                    url: self.server.url(&path),
                    headers: self.server.headers(),
                    body: Vec::new(),
                });
                self.requests.insert(id, Req::ArtistServer);
                self.artists.asking_server = true;
                self.artists.server_again = Some((w, f64::MAX));
                return;
            }
            let language = self.artist_language();
            self.artists.running = Some(Running {
                lookup: Lookup::new(&w.name, w.mbid.as_deref(), &language, w.depth),
                name: w.name,
                waiting: false,
                tries: 0,
            });
            return;
        }
    }

    fn artists_step(&mut self, host: &mut dyn Host, now: f64, unix: u64) {
        let Some(running) = &mut self.artists.running else {
            return;
        };
        if running.waiting {
            return;
        }
        let Some(request) = running.lookup.request() else {
            let Some(done) = self.artists.running.take() else {
                return;
            };
            let artist = done.lookup.finish();
            self.artists_remember(host, &done.name, artist, unix, now);
            return;
        };
        if !self.artists.pacer.ready(request.service, now) {
            return;
        }
        self.artists.pacer.mark(request.service, now);
        running.waiting = true;
        let id = host.http_request(get(&request.url));
        self.requests.insert(id, Req::Artist);
    }

    /// Keeps what was found (`fetched`: when it was asked). The page only looks
    /// again when something changed, and the picture is fetched only when it
    /// is a new one.
    fn artists_remember(
        &mut self,
        host: &dyn Host,
        name: &str,
        artist: Option<bl_meta::Artist>,
        fetched: u64,
        now: f64,
    ) {
        let old = self.artists.book.get(name);
        // A page known before stays when the services find nobody this time.
        let artist = artist.or_else(|| old.filter(|e| e.has_page()).and_then(|e| e.artist.clone()));
        let image_url = artist
            .as_ref()
            .map(|a| a.image_url.clone())
            .unwrap_or_default();
        let image_saved = !image_url.is_empty()
            && old.is_some_and(|e| {
                e.image_saved && e.artist.as_ref().is_some_and(|a| a.image_url == image_url)
            })
            && !host.storage_list(&image_key(name)).is_empty();
        let unchanged = old.is_some_and(|e| e.artist == artist && e.image_saved == image_saved);
        if !unchanged && artist.as_ref().is_some_and(|a| !a.genres.is_empty()) {
            self.artists.genres_changed = true;
        }
        if !image_saved && !image_url.is_empty() {
            self.artists
                .images_wanted
                .push_back((name.to_string(), image_url));
        }
        self.artists.book.put(
            name,
            Entry {
                artist,
                fetched,
                image_saved,
            },
        );
        if !unchanged {
            self.artists.rev += 1;
        }
        self.artists.save_at.get_or_insert(now + 5.0);
    }

    fn artists_image_step(&mut self, host: &mut dyn Host, now: f64) {
        if self.artists.image.is_some()
            || !self.artists.pacer.ready(bl_meta::Service::Wikimedia, now)
        {
            return;
        }
        let Some((name, url)) = self.artists.images_wanted.pop_front() else {
            return;
        };
        // Where a file cannot be kept the page loads it from there itself.
        if host.storage_path(&image_key(&name)).is_none() {
            return;
        }
        self.artists.pacer.mark(bl_meta::Service::Wikimedia, now);
        self.artists.image = Some(name);
        let mut request = get(&url);
        request.headers.retain(|(k, _)| k != "Accept");
        let id = host.http_request(request);
        self.requests.insert(id, Req::ArtistImage);
    }

    pub(crate) fn artist_http(&mut self, host: &mut dyn Host, res: Result<HttpResponse, String>) {
        let Some(running) = &mut self.artists.running else {
            return;
        };
        running.waiting = false;
        if !self.settings.online_metadata {
            return;
        }
        let (status, body) = match &res {
            Ok(r) => (r.status, r.body.as_slice()),
            Err(_) => (0, &[][..]),
        };
        if let Err(failed) = running.lookup.answer(status, body) {
            let now = host.now();
            running.tries += 1;
            self.artists
                .pacer
                .hold(failed.service, now, failed.pause * running.tries as f64);
            if running.tries >= MOST_TRIES {
                // Not remembered: asked again when the library or the page
                // look the next time.
                host.log(&format!(
                    "artist lookup of {}: no answer ({status})",
                    running.name
                ));
                self.artists.running = None;
                self.artists.library_seen = u64::MAX;
                self.artists.rev += 1;
            }
        }
    }

    pub(crate) fn artist_image_http(
        &mut self,
        host: &mut dyn Host,
        res: Result<HttpResponse, String>,
    ) {
        let Some(name) = self.artists.image.take() else {
            return;
        };
        let Ok(r) = res else { return };
        if !(200..300).contains(&r.status) || r.body.is_empty() || r.body.len() > MAX_IMAGE {
            return;
        }
        if !self.settings.online_metadata || !host.storage_write(&image_key(&name), &r.body) {
            return;
        }
        if let Some(e) = self.artists.book.get_mut(&name) {
            e.image_saved = true;
            self.artists.rev += 1;
            self.artists.save_at.get_or_insert(host.now() + 5.0);
        }
    }

    /// The answer of our own server. It has its own switch; when that is off
    /// or the server does not know this at all, this device asks by itself.
    pub(crate) fn artist_server_http(
        &mut self,
        host: &mut dyn Host,
        res: Result<HttpResponse, String>,
    ) {
        self.artists.asking_server = false;
        let Some((mut w, _)) = self.artists.server_again.take() else {
            return;
        };
        if !self.settings.online_metadata {
            return;
        }
        let answer = res
            .ok()
            .filter(|r| (200..300).contains(&r.status))
            .and_then(|r| bl_json::parse_bytes(&r.body).ok());
        let unix = host.unix_time() as u64;
        let now = host.now();
        match answer
            .as_ref()
            .filter(|a| a.bool_or("enabled", false))
            .map(|a| (a.str_or("state", ""), a))
        {
            Some(("ready", a)) => {
                let artist = bl_meta::Artist::from_json(a.get("artist"));
                // A page from long ago, which the server is asking for anew:
                // its own time, so that this device asks again later.
                let fetched = a.get("fetched").as_u64().map_or(unix, |f| f.min(unix));
                self.artists_remember(host, &w.name, Some(artist), fetched, now);
            }
            Some(("none", _)) => self.artists_remember(host, &w.name, None, unix, now),
            Some(("pending", _)) => {
                self.artists.server_again = Some((w, now + SERVER_RETRY));
            }
            _ => {
                w.asked_server = true;
                self.artists.wanted.push_front(w);
            }
        }
    }

    /// The switch was turned off: nothing goes out from here on.
    pub(crate) fn artists_switched(&mut self) {
        self.artists.stop();
        self.artists.library_seen = u64::MAX;
        self.artists.genres_rev += 1;
        self.artists.rev += 1;
    }

    /// Forgets everything that was looked up.
    pub(crate) fn artists_clear(&mut self, host: &mut dyn Host) {
        self.artists_load(host);
        self.artists.stop();
        self.artists.book.clear();
        for key in host.storage_list(IMAGE_PREFIX) {
            host.storage_delete(&key);
        }
        host.storage_delete(STORAGE_KEY);
        self.artists.save_at = None;
        self.artists.library_seen = u64::MAX;
        self.artists.genres_rev += 1;
        self.artists.rev += 1;
    }

    /// What is known about an artist for its page; asks when it is not known.
    /// `state`: "off" (the switch), "pending", "none" or "ready".
    pub(crate) fn artist_info(&mut self, host: &mut dyn Host, name: &str) -> Value {
        let result = |state: &str| Value::object().with("state", state);
        if !self.settings.online_metadata {
            return result("off");
        }
        self.artists_load(host);
        let unix = host.unix_time() as u64;
        let entry = self.artists.book.get(name);
        let fresh = entry.is_some_and(|e| e.fresh(unix));
        let complete = entry
            .and_then(|e| e.artist.as_ref())
            .is_some_and(|a| a.complete);
        let shown = entry
            .filter(|e| e.artist.is_some())
            .map(|e| (e.artist.clone().unwrap_or_default(), e.image_saved));
        let none = fresh && entry.is_some_and(|e| e.artist.is_none());
        if !none && !(fresh && complete) && !self.artists.wanted_contains(name, Depth::Page) {
            let mbid = self.artist_mbid(name);
            self.artists.wanted.push_front(Wanted {
                name: name.to_string(),
                mbid,
                depth: Depth::Page,
                asked_server: false,
            });
        }
        match shown {
            Some((artist, saved)) => {
                let image = if saved {
                    host.storage_path(&image_key(name))
                        .map(|p| format!("file://{p}"))
                } else {
                    None
                };
                let mut json = artist.to_json();
                json.set(
                    "image",
                    Value::from(image.unwrap_or_else(|| artist.image_url.clone())),
                );
                result(if fresh && complete {
                    "ready"
                } else {
                    "pending"
                })
                .with("artist", json)
            }
            None if none => result("none"),
            None => result("pending"),
        }
    }

    pub(crate) fn artists_json(&self) -> Value {
        Value::object()
            .with("rev", self.artists.rev)
            .with("known", self.artists.book.len())
            .with("waiting", self.artists.wanted.len())
            .with("source", "MusicBrainz, Wikimedia, ListenBrainz")
    }

    /// An artist's genres for recommendations, when the switch is on.
    pub(crate) fn artist_genres(&self, name: &str) -> &[String] {
        if self.settings.online_metadata {
            self.artists.book.genres(name)
        } else {
            &[]
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::core::LaunchArgs;
    use crate::host::{AudioDeviceInfo, AudioSource, DirEntry};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::sync::Arc;

    /// Everything goes nowhere; requests are counted and answered from a list.
    #[derive(Default)]
    pub(crate) struct TestHost {
        time: f64,
        storage: RefCell<HashMap<String, Vec<u8>>>,
        requests: Vec<HttpRequest>,
        next_id: u64,
        answers: Vec<(u64, Result<HttpResponse, String>)>,
    }

    impl Host for TestHost {
        fn now(&self) -> f64 {
            self.time
        }
        fn unix_time(&self) -> f64 {
            1_800_000_000.0 + self.time
        }
        fn random_seed(&self) -> u64 {
            1
        }
        fn platform(&self) -> &'static str {
            "linux"
        }
        fn storage_read(&self, key: &str) -> Option<Vec<u8>> {
            self.storage.borrow().get(key).cloned()
        }
        fn storage_write(&self, key: &str, data: &[u8]) -> bool {
            self.storage
                .borrow_mut()
                .insert(key.to_string(), data.to_vec());
            true
        }
        fn storage_delete(&self, key: &str) {
            self.storage.borrow_mut().remove(key);
        }
        fn storage_list(&self, prefix: &str) -> Vec<String> {
            self.storage
                .borrow()
                .keys()
                .filter(|k| k.starts_with(prefix))
                .cloned()
                .collect()
        }
        fn storage_path(&self, key: &str) -> Option<String> {
            Some(format!("/test/{key}"))
        }
        fn has_filesystem(&self) -> bool {
            false
        }
        fn fs_roots(&self) -> Vec<(String, String)> {
            Vec::new()
        }
        fn fs_list(&self, _path: &str) -> Result<Vec<DirEntry>, String> {
            Err("no".into())
        }
        fn fs_open(&self, _path: &str) -> Result<Box<dyn bl_media::Source>, String> {
            Err("no".into())
        }
        fn fs_read(&self, _path: &str) -> Result<Vec<u8>, String> {
            Err("no".into())
        }
        fn fs_metadata(&self, _path: &str) -> Option<DirEntry> {
            None
        }
        fn http_request(&mut self, req: HttpRequest) -> u64 {
            self.next_id += 1;
            self.requests.push(req);
            self.next_id
        }
        fn http_poll(&mut self) -> Vec<(u64, Result<HttpResponse, String>)> {
            std::mem::take(&mut self.answers)
        }
        fn has_threads(&self) -> bool {
            false
        }
        fn spawn(&self, job: Box<dyn FnOnce() + Send>) {
            job();
        }
        fn audio_start(
            &mut self,
            _source: Arc<dyn AudioSource>,
        ) -> Result<AudioDeviceInfo, String> {
            Err("no".into())
        }
        fn log(&self, _msg: &str) {}
    }

    const ID: &str = "a74b1b7f-71a5-4011-9441-d0b5e4122711";

    fn command(core: &mut Core, host: &mut TestHost, json: String) -> Value {
        core.command(host, &bl_json::parse(&json).unwrap())
    }

    fn run(core: &mut Core, host: &mut TestHost, seconds: f64) {
        let end = host.time + seconds;
        while host.time < end {
            host.time += 0.1;
            core.tick(host);
        }
    }

    fn song(name: &str) -> bl_net::api::Track {
        bl_net::api::Track {
            id: format!("{}{name}", crate::library::LOCAL_PREFIX),
            title: "Song".into(),
            artists: vec![name.to_string()],
            ..Default::default()
        }
    }

    fn outward(host: &TestHost) -> usize {
        host.requests
            .iter()
            .filter(|r| {
                [
                    "musicbrainz.org",
                    "wikidata.org",
                    "wikipedia.org",
                    "wikimedia.org",
                    "listenbrainz.org",
                ]
                .iter()
                .any(|d| r.url.contains(d))
            })
            .count()
    }

    #[test]
    fn with_the_switch_off_nothing_goes_out() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        assert!(!core.settings.online_metadata, "off from the start");
        core.library.local.push(song("The Band"));
        core.library.rebuild();
        let info = command(
            &mut core,
            &mut host,
            r#"{"cmd":"artist.info","name":"The Band"}"#.to_string(),
        );
        assert_eq!(info.str_or("state", ""), "off");
        run(&mut core, &mut host, 30.0);
        assert_eq!(outward(&host), 0, "{:?}", host.requests);
    }

    #[test]
    fn with_the_switch_on_it_asks_says_who_it_is_and_keeps_the_pace() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.library.local.push(song("The Band"));
        core.library.local.push(song("The Other Band"));
        core.library.rebuild();
        core.settings.online_metadata = true;
        run(&mut core, &mut host, 0.5);
        assert_eq!(outward(&host), 1);
        let first = &host.requests[0];
        assert!(first
            .url
            .starts_with("https://musicbrainz.org/ws/2/artist?query="));
        assert!(first
            .headers
            .iter()
            .any(|(k, v)| k == "User-Agent" && v.starts_with("Beatlumen/")));
        // The answer comes quickly: nobody of that name. The next one still
        // has to wait for the pace.
        let answer = |host: &mut TestHost| {
            let id = host.next_id;
            host.answers.push((
                id,
                Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"artists":[]}"#.to_vec(),
                }),
            ));
        };
        answer(&mut host);
        run(&mut core, &mut host, 0.3);
        assert_eq!(outward(&host), 1, "too early");
        run(&mut core, &mut host, 1.2);
        assert_eq!(outward(&host), 2);
        // Nobody is asked twice while an answer is on its way.
        run(&mut core, &mut host, 5.0);
        assert_eq!(outward(&host), 2);
        // Both are known now: nothing more to ask, also in a long time.
        answer(&mut host);
        run(&mut core, &mut host, 30.0);
        assert_eq!(outward(&host), 2);
        assert_eq!(core.artists.book.len(), 2);
    }

    #[test]
    fn turning_it_off_stops_what_is_under_way() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.library.local.push(song("A"));
        core.library.local.push(song("B"));
        core.library.rebuild();
        core.settings.online_metadata = true;
        run(&mut core, &mut host, 0.5);
        let before = outward(&host);
        command(
            &mut core,
            &mut host,
            r#"{"cmd":"settings.set","key":"onlineMetadata","value":false}"#.to_string(),
        );
        run(&mut core, &mut host, 30.0);
        assert_eq!(outward(&host), before);
    }

    #[test]
    fn known_artists_show_only_with_the_switch_and_can_be_forgotten() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.settings.online_metadata = true;
        core.artists.loaded = true;
        core.artists.book.put(
            "The Band",
            Entry {
                artist: Some(bl_meta::Artist {
                    mbid: ID.to_string(),
                    name: "The Band".into(),
                    genres: vec!["rock".to_string()],
                    complete: true,
                    ..Default::default()
                }),
                fetched: host.unix_time() as u64,
                image_saved: false,
            },
        );
        let info = core.artist_info(&mut host, "the band");
        assert_eq!(info.str_or("state", ""), "ready");
        assert_eq!(info.get("artist").str_or("name", ""), "The Band");
        assert_eq!(core.artist_genres("The Band"), ["rock"]);

        core.settings.online_metadata = false;
        assert_eq!(
            core.artist_info(&mut host, "the band").str_or("state", ""),
            "off"
        );
        assert!(core.artist_genres("The Band").is_empty());

        core.settings.online_metadata = true;
        core.artists_clear(&mut host);
        assert_eq!(core.artists.book.len(), 0);
    }

    #[test]
    fn what_was_asked_survives_a_restart() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.settings.online_metadata = true;
        core.artists.loaded = true;
        core.artists_remember(
            &host,
            "The Band",
            Some(bl_meta::Artist {
                name: "The Band".into(),
                ..Default::default()
            }),
            1_800_000_000,
            0.0,
        );
        run(&mut core, &mut host, 6.0);
        assert!(host.storage.borrow().contains_key(STORAGE_KEY));
        let mut again = Core::new(&mut host, LaunchArgs::default(), "de");
        again.settings.online_metadata = true;
        run(&mut again, &mut host, 0.2);
        assert_eq!(again.artists.book.len(), 1);
    }

    fn page(image: &str) -> bl_meta::Artist {
        bl_meta::Artist {
            mbid: ID.to_string(),
            name: "The Band".into(),
            genres: vec!["rock".to_string()],
            biography: "A band.".into(),
            image_url: image.to_string(),
            complete: true,
            ..Default::default()
        }
    }

    /// A page with its picture on this device, asked for `days` ago.
    fn known_page(core: &mut Core, host: &TestHost, days: u64) {
        core.artists.loaded = true;
        core.artists.book.put(
            "The Band",
            Entry {
                artist: Some(page("https://upload.wikimedia.org/a.jpg")),
                fetched: host.unix_time() as u64 - days * 86_400,
                image_saved: true,
            },
        );
        host.storage_write(&image_key("The Band"), b"picture");
    }

    #[test]
    fn a_page_from_long_ago_is_shown_at_once_and_asked_for_anew_when_opened() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.library.local.push(song("The Band"));
        core.library.rebuild();
        core.settings.online_metadata = true;
        known_page(&mut core, &host, 40);
        // Looking through the library leaves the page alone: the genres alone
        // would take its place.
        run(&mut core, &mut host, 5.0);
        assert_eq!(outward(&host), 0, "{:?}", host.requests);

        let info = core.artist_info(&mut host, "The Band");
        assert_eq!(info.get("artist").str_or("biography", ""), "A band.");
        assert_eq!(
            info.get("artist").str_or("image", ""),
            format!("file:///test/{}", image_key("The Band"))
        );
        run(&mut core, &mut host, 0.5);
        assert_eq!(outward(&host), 1, "asked for anew");
        // Nobody is found this time: the page stays, with its picture.
        let rev = core.artists.rev;
        let unix = host.unix_time() as u64;
        core.artists_remember(&host, "The Band", None, unix, host.now());
        let e = core.artists.book.get("The Band").unwrap();
        assert!(e.has_page() && e.image_saved && e.fresh(unix));
        assert_eq!(core.artists.rev, rev, "nothing changed on the page");
    }

    #[test]
    fn the_same_picture_is_not_fetched_again_and_a_new_one_is() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.settings.online_metadata = true;
        known_page(&mut core, &host, 40);
        let unix = host.unix_time() as u64;
        core.artists_remember(
            &host,
            "The Band",
            Some(page("https://upload.wikimedia.org/a.jpg")),
            unix,
            0.0,
        );
        assert!(core.artists.book.get("The Band").unwrap().image_saved);
        assert!(core.artists.images_wanted.is_empty());

        let rev = core.artists.rev;
        core.artists_remember(
            &host,
            "The Band",
            Some(page("https://upload.wikimedia.org/b.jpg")),
            unix,
            0.0,
        );
        assert!(!core.artists.book.get("The Band").unwrap().image_saved);
        assert_eq!(core.artists.images_wanted.len(), 1);
        assert!(core.artists.rev > rev);
    }

    #[test]
    fn a_page_the_server_has_from_long_ago_keeps_its_time() {
        let mut host = TestHost::default();
        let mut core = Core::new(&mut host, LaunchArgs::default(), "de");
        core.settings.online_metadata = true;
        core.artists.loaded = true;
        let long_ago = host.unix_time() as u64 - 40 * 86_400;
        let answer = |core: &mut Core, host: &mut TestHost| {
            core.artists.server_again = Some((
                Wanted {
                    name: "The Band".into(),
                    mbid: None,
                    depth: Depth::Page,
                    asked_server: false,
                },
                f64::MAX,
            ));
            let body = Value::object()
                .with("enabled", true)
                .with("state", "ready")
                .with("fetched", long_ago)
                .with("artist", page("").to_json())
                .to_json();
            core.artist_server_http(
                host,
                Ok(HttpResponse {
                    status: 200,
                    headers: Vec::new(),
                    body: body.into_bytes(),
                }),
            );
        };
        answer(&mut core, &mut host);
        let e = core.artists.book.get("The Band").unwrap();
        assert_eq!(e.fetched, long_ago);
        assert!(e.has_page());
        // Shown, and asked for again while it is open, but the same answer
        // again does not make the page look again (no back and forth).
        let info = core.artist_info(&mut host, "The Band");
        assert_eq!(info.get("artist").str_or("name", ""), "The Band");
        let rev = core.artists.rev;
        answer(&mut core, &mut host);
        assert_eq!(core.artists.rev, rev);
    }
}
