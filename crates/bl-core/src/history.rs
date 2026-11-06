//! What was played on this device (`history.json`): the source of "recently
//! played", the statistics and the recommendations without a server.
//!
//! A song counts as played after 30 s or half of it, whichever is shorter;
//! less counts as skipped. Live sources (radio stations, AirPlay, DLNA, other
//! apps) are not recorded.
//!
//! With the user's agreement (`Settings::history_sync`) plays of the server's
//! songs made from then on also go to the server. Local and encrypted files
//! never do.

use crate::core::{Core, Req};
use crate::host::{Host, HttpResponse};
use crate::library;
use crate::player::PlayState;
use bl_json::Value;
use bl_net::api::{paths, Track};
use std::collections::HashMap;

const STORAGE: &str = "history.json";
/// Plays kept; the oldest go first. At 25 songs a day more than 5 years.
const MAX_EVENTS: usize = 50_000;
/// Plays sent to the server at once.
const BATCH: usize = 500;
/// Seconds between writes of the file when nothing else asks for one.
const SAVE_DELAY: f64 = 20.0;
/// Seconds to wait after the server could not take plays.
const RETRY: f64 = 120.0;
/// Below this a play is not recorded at all (it never really started).
const MIN_SECONDS: f64 = 1.0;
/// A play counts from here on (or half of the song, if that is shorter).
const COUNTS_AFTER: f64 = 30.0;

/// What is kept of a song, so its plays still make sense when it is gone.
#[derive(Debug, Clone, PartialEq, Default)]
pub(crate) struct Song {
    pub id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub album_id: String,
    pub genre: String,
    pub duration_ms: u64,
}

impl Song {
    fn of(t: &Track) -> Song {
        Song {
            id: t.id.clone(),
            title: t.title.clone(),
            artist: t.artists.join(", "),
            album: t.album.clone(),
            album_id: t.album_id.clone(),
            genre: t.genre.clone(),
            duration_ms: t.duration_ms,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct Play {
    /// Index into `HistoryState::songs`.
    pub song: u32,
    /// When it started (seconds since the Unix epoch).
    pub t: u64,
    /// How long it was heard.
    pub ms: u32,
    pub skip: bool,
}

/// The song playing now.
struct Listening {
    track: Track,
    started: u64,
    played: f64,
    last: f64,
    last_pos: f64,
}

#[derive(Default)]
pub(crate) struct HistoryState {
    pub songs: Vec<Song>,
    index: HashMap<String, u32>,
    /// Oldest first.
    pub plays: Vec<Play>,
    /// Plays ever recorded here; `plays[0]` is number `total - plays.len()`.
    total: u64,
    /// The first `sent` plays ever recorded are done with (sent, or from
    /// before the user agreed).
    sent: u64,
    sending: bool,
    wait_until: f64,
    dirty_at: Option<f64>,
    listening: Option<Listening>,
    /// The server's statistics of all devices, last asked for.
    pub server_stats: Value,
}

impl HistoryState {
    fn song_index(&mut self, t: &Track) -> u32 {
        if let Some(i) = self.index.get(&t.id) {
            // Tags may have been corrected.
            self.songs[*i as usize] = Song::of(t);
            return *i;
        }
        let i = self.songs.len() as u32;
        self.songs.push(Song::of(t));
        self.index.insert(t.id.clone(), i);
        i
    }

    fn add(&mut self, t: &Track, at: u64, seconds: f64) {
        let song = self.song_index(t);
        self.plays.push(Play {
            song,
            t: at,
            ms: (seconds * 1000.0).min(u32::MAX as f64) as u32,
            skip: !counts(seconds, t.duration_ms),
        });
        self.total += 1;
        if self.plays.len() > MAX_EVENTS {
            let drop = self.plays.len() - MAX_EVENTS;
            self.plays.drain(..drop);
            self.prune();
        }
    }

    /// Forgets songs no play refers to any more.
    fn prune(&mut self) {
        let mut used = vec![false; self.songs.len()];
        for p in &self.plays {
            used[p.song as usize] = true;
        }
        let mut map = vec![0u32; self.songs.len()];
        let mut songs = Vec::new();
        for (i, s) in std::mem::take(&mut self.songs).into_iter().enumerate() {
            if used[i] {
                map[i] = songs.len() as u32;
                songs.push(s);
            }
        }
        self.songs = songs;
        for p in &mut self.plays {
            p.song = map[p.song as usize];
        }
        self.index = self
            .songs
            .iter()
            .enumerate()
            .map(|(i, s)| (s.id.clone(), i as u32))
            .collect();
    }

    pub(crate) fn load(data: &[u8]) -> HistoryState {
        let mut s = HistoryState::default();
        let Ok(v) = bl_json::parse_bytes(data) else {
            return s;
        };
        for t in v.get("songs").as_array().into_iter().flatten() {
            s.songs.push(Song {
                id: t.str_or("id", "").to_string(),
                title: t.str_or("title", "").to_string(),
                artist: t.str_or("artist", "").to_string(),
                album: t.str_or("album", "").to_string(),
                album_id: t.str_or("albumId", "").to_string(),
                genre: t.str_or("genre", "").to_string(),
                duration_ms: t.get("durationMs").as_u64().unwrap_or(0),
            });
        }
        s.index = s
            .songs
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.clone(), i as u32))
            .collect();
        for p in v.get("plays").as_array().into_iter().flatten() {
            let song = p.at(0).as_u64().unwrap_or(u64::MAX);
            if (song as usize) < s.songs.len() {
                s.plays.push(Play {
                    song: song as u32,
                    t: p.at(1).as_u64().unwrap_or(0),
                    ms: p.at(2).as_u64().unwrap_or(0) as u32,
                    skip: p.at(3).as_u64().unwrap_or(0) != 0,
                });
            }
        }
        s.total = v
            .get("total")
            .as_u64()
            .unwrap_or(0)
            .max(s.plays.len() as u64);
        s.sent = v.get("sent").as_u64().unwrap_or(0).min(s.total);
        s
    }

    fn to_json(&self) -> Value {
        Value::object()
            .with("total", self.total)
            .with("sent", self.sent)
            .with(
                "songs",
                Value::Array(
                    self.songs
                        .iter()
                        .map(|t| {
                            Value::object()
                                .with("id", t.id.as_str())
                                .with("title", t.title.as_str())
                                .with("artist", t.artist.as_str())
                                .with("album", t.album.as_str())
                                .with("albumId", t.album_id.as_str())
                                .with("genre", t.genre.as_str())
                                .with("durationMs", t.duration_ms)
                        })
                        .collect(),
                ),
            )
            .with(
                "plays",
                Value::Array(
                    self.plays
                        .iter()
                        .map(|p| {
                            Value::Array(vec![
                                Value::from(p.song),
                                Value::from(p.t),
                                Value::from(p.ms),
                                Value::from(u32::from(p.skip)),
                            ])
                        })
                        .collect(),
                ),
            )
    }

    /// Ids of the songs last played (not skipped), newest first.
    pub fn recent_ids(&self) -> impl Iterator<Item = &str> {
        let mut seen = std::collections::HashSet::new();
        self.plays
            .iter()
            .rev()
            .filter(|p| !p.skip)
            .filter_map(move |p| seen.insert(p.song).then_some(p.song))
            .map(|i| self.songs[i as usize].id.as_str())
    }

    /// The next plays to send, and how many plays that finishes.
    fn next_batch(&self) -> (Vec<Value>, u64) {
        let first = self.total - self.plays.len() as u64;
        let start = self.sent.max(first);
        let mut out = Vec::new();
        let mut done = start;
        for p in &self.plays[(start - first) as usize..] {
            if out.len() >= BATCH {
                break;
            }
            done += 1;
            let id = &self.songs[p.song as usize].id;
            if library::is_server_id(id) {
                out.push(
                    Value::object()
                        .with("id", id.as_str())
                        .with("t", p.t)
                        .with("ms", p.ms)
                        .with("skip", p.skip),
                );
            }
        }
        (out, done)
    }
}

/// Whether `seconds` of a song count as having played it.
pub(crate) fn counts(seconds: f64, duration_ms: u64) -> bool {
    let half = duration_ms as f64 / 2000.0;
    let needed = if half > 0.0 {
        COUNTS_AFTER.min(half)
    } else {
        COUNTS_AFTER
    };
    seconds >= needed
}

impl Core {
    pub(crate) fn load_history(&mut self, host: &dyn Host) {
        if let Some(d) = host.storage_read(STORAGE) {
            self.history = HistoryState::load(&d);
        }
    }

    pub(crate) fn save_history(&mut self, host: &mut dyn Host) {
        if self.history.dirty_at.take().is_some() {
            host.storage_write(STORAGE, self.history.to_json().to_json().as_bytes());
        }
    }

    fn history_dirty(&mut self) {
        self.history.dirty_at.get_or_insert(self.time);
    }

    /// Follows the current song while it plays (called every tick).
    pub(crate) fn track_listening(&mut self, host: &mut dyn Host) {
        let track = self
            .current
            .as_ref()
            .filter(|t| !is_live(&t.id) && self.queue.current() == Some(&t.id))
            .filter(|_| !self.playing_airplay() && !self.playing_dlna() && !self.playing_capture())
            .filter(|_| self.player.state() != PlayState::Stopped);
        let same = match (&self.history.listening, track) {
            (Some(l), Some(t)) => l.track.id == t.id,
            (None, None) => true,
            _ => false,
        };
        if !same {
            let track = track.cloned();
            self.finish_listening(host);
            if let Some(t) = track {
                self.history.listening = Some(Listening {
                    track: t,
                    started: host.unix_time() as u64,
                    played: 0.0,
                    last: host.now(),
                    last_pos: 0.0,
                });
            }
        }
        if self.history.listening.is_none() {
            return;
        }
        let now = host.now();
        let playing = self.player.state() == PlayState::Playing;
        let pos = self.player.position(host);
        let restarted = {
            let l = self.history.listening.as_mut().expect("checked above");
            if playing {
                // A stall (or the computer asleep) is not listening.
                l.played += (now - l.last).clamp(0.0, 1.0);
            }
            l.last = now;
            let back = pos + 5.0 < l.last_pos && pos < 2.0;
            l.last_pos = pos;
            back && counts(l.played, l.track.duration_ms)
        };
        // The same song again (repeat, or back to its start).
        if restarted {
            let track = self
                .history
                .listening
                .as_ref()
                .map(|l| l.track.clone())
                .expect("checked above");
            self.finish_listening(host);
            self.history.listening = Some(Listening {
                track,
                started: host.unix_time() as u64,
                played: 0.0,
                last: now,
                last_pos: pos,
            });
        }
    }

    /// Records the song playing now, if it played long enough to tell.
    pub(crate) fn finish_listening(&mut self, _host: &mut dyn Host) {
        let Some(l) = self.history.listening.take() else {
            return;
        };
        if l.played < MIN_SECONDS {
            return;
        }
        self.history.add(&l.track, l.started, l.played);
        self.history_dirty();
        self.revs.history += 1;
    }

    pub(crate) fn poll_history(&mut self, host: &mut dyn Host) {
        if self
            .history
            .dirty_at
            .is_some_and(|t| self.time - t > SAVE_DELAY)
        {
            self.save_history(host);
        }
        if !self.settings.history_sync
            || self.history.sending
            || !self.server.is_connected()
            || self.history.sent >= self.history.total
            || self.time < self.history.wait_until
        {
            return;
        }
        let (events, done) = self.history.next_batch();
        if events.is_empty() {
            self.history.sent = done;
            self.history_dirty();
            return;
        }
        self.history.sending = true;
        let body = Value::object()
            .with("events", Value::Array(events))
            .to_json()
            .into_bytes();
        let rid = self
            .server
            .send(host, "POST", paths::HISTORY, body, "application/json");
        self.requests.insert(rid, Req::HistorySent(done));
    }

    pub(crate) fn history_sent(
        &mut self,
        _host: &mut dyn Host,
        res: &Result<HttpResponse, String>,
        done: u64,
    ) {
        self.history.sending = false;
        match res {
            Ok(r) if (200..300).contains(&r.status) => {
                self.history.sent = done.max(self.history.sent);
                self.history_dirty();
            }
            _ => self.history.wait_until = self.time + RETRY,
        }
    }

    /// The user agreed or changed their mind: either way only plays from
    /// now on are concerned.
    pub(crate) fn history_sync_changed(&mut self) {
        self.history.sent = self.history.total;
        self.history_dirty();
    }

    /// Deletes what was played on this device, and with `server` what the
    /// server knows of this login.
    pub(crate) fn history_clear(&mut self, host: &mut dyn Host, server: bool) {
        let listening = self.history.listening.take();
        let total = self.history.total;
        self.history = HistoryState {
            total,
            sent: total,
            listening,
            ..HistoryState::default()
        };
        host.storage_delete(STORAGE);
        self.revs.history += 1;
        if server && self.server.is_connected() {
            let rid = self.server.send(
                host,
                "DELETE",
                paths::HISTORY,
                Vec::new(),
                "application/json",
            );
            self.requests.insert(rid, Req::HistoryCleared);
        }
    }

    /// `stats.overview`: statistics of this device.
    pub(crate) fn history_stats(&self, host: &dyn Host, range: &str, utc_offset: i64) -> Value {
        use bl_recommend::stats::{stats, Range, StatPlay, StatSong};
        let songs: Vec<StatSong> = self
            .history
            .songs
            .iter()
            .map(|s| StatSong {
                id: s.id.clone(),
                title: s.title.clone(),
                artists: library::split_artists(&s.artist),
                album: s.album.clone(),
                album_id: s.album_id.clone(),
                genre: s.genre.clone(),
            })
            .collect();
        let plays: Vec<StatPlay> = self
            .history
            .plays
            .iter()
            .map(|p| StatPlay {
                song: Some(p.song),
                t: p.t,
                ms: u64::from(p.ms),
                skip: p.skip,
            })
            .collect();
        stats(
            &songs,
            &plays,
            Range::parse(range),
            host.unix_time() as u64,
            utc_offset,
        )
    }

    /// `stats.fetch`: asks the server for its statistics of all devices.
    pub(crate) fn stats_fetch(&mut self, host: &mut dyn Host, range: &str, utc_offset: i64) {
        if !self.server.is_connected() {
            return;
        }
        let path = format!("{}?range={range}&utcOffset={utc_offset}", paths::STATS);
        let rid = self.server.get(host, &path);
        self.requests.insert(rid, Req::Stats);
    }

    pub(crate) fn stats_fetched(&mut self, res: &Result<HttpResponse, String>) {
        self.history.server_stats = res
            .as_ref()
            .ok()
            .filter(|r| r.status == 200)
            .and_then(|r| bl_json::parse_bytes(&r.body).ok())
            .unwrap_or_else(Value::object);
        self.revs.stats += 1;
    }

    /// `stats.library`: the library at a glance.
    pub(crate) fn library_stats(&self) -> Value {
        Value::object()
            .with("tracks", self.library.tracks.len())
            .with("albums", self.library.albums.len())
            .with("artists", self.library.artists.len())
            .with(
                "seconds",
                self.library
                    .tracks
                    .iter()
                    .map(|t| t.duration_ms)
                    .sum::<u64>()
                    / 1000,
            )
            .with("favorites", self.favorites.playlist().items.len())
    }

    /// Songs last played that can be played again, newest first.
    pub(crate) fn history_recent(&self, limit: usize) -> Value {
        Value::Array(
            self.history
                .recent_ids()
                .filter_map(|id| self.library.track(id))
                .take(limit)
                .map(|t| self.track_json(t))
                .collect(),
        )
    }
}

/// Streams that are no song of a library.
fn is_live(id: &str) -> bool {
    id.starts_with(crate::sources::RADIO_PREFIX)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn track(id: &str, secs: u64) -> Track {
        Track {
            id: id.to_string(),
            title: format!("Title {id}"),
            artists: vec!["Artist".to_string()],
            duration_ms: secs * 1000,
            ..Track::default()
        }
    }

    #[test]
    fn what_counts_as_played() {
        // 30 s, or half of a short song.
        assert!(counts(30.0, 300_000));
        assert!(!counts(29.0, 300_000));
        assert!(counts(20.0, 40_000));
        assert!(!counts(19.0, 40_000));
        // Length unknown.
        assert!(counts(30.0, 0));
        assert!(!counts(10.0, 0));
    }

    #[test]
    fn stored_and_loaded() {
        let mut h = HistoryState::default();
        h.add(&track("00000000000000aa", 200), 100, 180.0);
        h.add(&track("file:/m/b.mp3", 200), 300, 2.5);
        h.add(&track("00000000000000aa", 200), 500, 40.0);
        let again = HistoryState::load(h.to_json().to_json().as_bytes());
        assert_eq!(again.plays, h.plays);
        assert_eq!(again.songs, h.songs);
        assert!(again.plays[1].skip && !again.plays[0].skip);
        assert_eq!(again.total, 3);
    }

    #[test]
    fn recent_lists_each_song_once_and_no_skips() {
        let mut h = HistoryState::default();
        h.add(&track("a", 200), 1, 100.0);
        h.add(&track("b", 200), 2, 100.0);
        h.add(&track("c", 200), 3, 2.0);
        h.add(&track("a", 200), 4, 100.0);
        let r: Vec<&str> = h.recent_ids().collect();
        assert_eq!(r, ["a", "b"]);
    }

    #[test]
    fn only_server_songs_are_sent() {
        let mut h = HistoryState::default();
        h.add(&track("00000000000000aa", 200), 1, 100.0);
        h.add(&track("file:/m/b.mp3", 200), 2, 100.0);
        h.add(&track("vault:abc", 200), 3, 100.0);
        h.add(&track("00000000000000cc", 200), 4, 2.0);
        let (ev, done) = h.next_batch();
        assert_eq!(done, 4);
        let ids: Vec<&str> = ev.iter().map(|e| e.str_or("id", "")).collect();
        assert_eq!(ids, ["00000000000000aa", "00000000000000cc"]);
        assert!(ev[1].bool_or("skip", false));
    }

    #[test]
    fn nothing_from_before_the_agreement() {
        let mut h = HistoryState::default();
        h.add(&track("00000000000000aa", 200), 1, 100.0);
        h.sent = h.total;
        h.add(&track("00000000000000bb", 200), 2, 100.0);
        let (ev, done) = h.next_batch();
        assert_eq!(done, 2);
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].str_or("id", ""), "00000000000000bb");
    }

    #[test]
    fn oldest_go_when_full() {
        let mut h = HistoryState::default();
        h.add(&track("old", 200), 0, 100.0);
        for i in 0..MAX_EVENTS as u64 {
            h.add(&track("new", 200), i + 1, 100.0);
        }
        assert_eq!(h.plays.len(), MAX_EVENTS);
        assert_eq!(h.songs.len(), 1);
        assert_eq!(h.songs[0].id, "new");
        assert_eq!(h.plays[0].song, 0);
    }
}
