//! Song texts: LRC parsing (several time tags per line, `[offset:]`,
//! enhanced LRC word timing) and loading the current track's text from the
//! file, the server or an import.

use crate::core::{Core, Req};
use crate::host::Host;
use crate::library;
use crate::server::ServerState;
use bl_json::Value;

/// At most this many lines are kept.
const MAX_LINES: usize = 5000;

#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    /// Start in milliseconds (synced text only).
    pub time_ms: Option<i64>,
    pub text: String,
    /// Word start times and words (enhanced LRC), joined they give `text`.
    pub words: Vec<(i64, String)>,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
}

/// Parses a time like "01:23.45", "1:23", "01:23:45" (hundredths) or
/// "1:02:03.5" into milliseconds.
fn parse_time(s: &str) -> Option<i64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || !parts.iter().all(|p| !p.is_empty()) {
        return None;
    }
    let num = |p: &str| -> Option<i64> {
        p.bytes()
            .all(|b| b.is_ascii_digit())
            .then(|| p.parse().ok())?
    };
    let secs_frac = |p: &str| -> Option<i64> {
        let (sec, frac) = match p.split_once('.') {
            Some((a, b)) => (a, b),
            None => (p, ""),
        };
        let sec = num(sec)?;
        let frac_ms = if frac.is_empty() {
            0
        } else {
            let f = num(&frac[..frac.len().min(3)])?;
            f * 10i64.pow(3 - frac.len().min(3) as u32)
        };
        (sec < 60).then_some(sec * 1000 + frac_ms)
    };
    let min = num(parts[0])?;
    match parts.len() {
        // [mm:ss.xx]
        2 => Some(min * 60_000 + secs_frac(parts[1])?),
        // [mm:ss:xx] (hundredths) or [h:mm:ss.xx]
        _ if parts[2].contains('.') => {
            let m = num(parts[1])?;
            (m < 60).then_some(min * 3_600_000 + m * 60_000 + secs_frac(parts[2])?)
        }
        _ => {
            let sec = num(parts[1])?;
            let hund = num(parts[2])?;
            (sec < 60 && parts[2].len() <= 2).then_some(min * 60_000 + sec * 1000 + hund * 10)
        }
    }
}

/// Splits the leading `[...]` tags off a line.
fn leading_tags(line: &str) -> (Vec<&str>, &str) {
    let mut tags = Vec::new();
    let mut rest = line.trim_start();
    while let Some(r) = rest.strip_prefix('[') {
        let Some(end) = r.find(']') else { break };
        tags.push(&r[..end]);
        rest = &r[end + 1..];
    }
    (tags, rest)
}

/// Text and word times of an enhanced LRC line ("<00:01.00>word ...").
fn words(text: &str, line_start: i64, offset: i64) -> (String, Vec<(i64, String)>) {
    if !text.contains('<') {
        return (text.trim().to_string(), Vec::new());
    }
    let mut out = Vec::new();
    let mut plain = String::new();
    let mut time = line_start;
    let mut rest = text;
    let mut word = String::new();
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('<') {
            if let Some(end) = r.find('>') {
                if let Some(t) = parse_time(&r[..end]) {
                    if !word.is_empty() {
                        out.push((time, std::mem::take(&mut word)));
                    }
                    time = t - offset;
                    rest = &r[end + 1..];
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        word.push(c);
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    if !word.is_empty() {
        out.push((time, word));
    }
    // Leading spaces stay with their words; trim the line's ends.
    let trimmed = plain.trim().to_string();
    if let Some(first) = out.first_mut() {
        first.1 = first.1.trim_start().to_string();
    }
    if let Some(last) = out.last_mut() {
        last.1 = last.1.trim_end().to_string();
    }
    out.retain(|(_, w)| !w.is_empty());
    (trimmed, out)
}

pub fn parse(text: &str) -> Lyrics {
    let mut offset = 0i64;
    let mut synced: Vec<Line> = Vec::new();
    let mut plain: Vec<Line> = Vec::new();
    for raw in text.lines() {
        let (tags, rest) = leading_tags(raw);
        let mut times = Vec::new();
        let mut meta_only = !tags.is_empty();
        for t in &tags {
            if let Some(ms) = parse_time(t) {
                times.push(ms);
                meta_only = false;
            } else if let Some(v) = t.strip_prefix("offset:") {
                offset = v.trim().parse().unwrap_or(0);
            }
        }
        if times.is_empty() {
            if meta_only && rest.trim().is_empty() {
                continue; // [ar:...], [ti:...], ...
            }
            plain.push(Line {
                time_ms: None,
                text: raw.trim().to_string(),
                words: Vec::new(),
            });
            continue;
        }
        for t in times {
            let start = t - offset;
            let (text, words) = words(rest, start, offset);
            synced.push(Line {
                time_ms: Some(start.max(0)),
                text,
                words,
            });
        }
        if synced.len() > MAX_LINES {
            break;
        }
    }
    if !synced.is_empty() {
        synced.sort_by_key(|l| l.time_ms);
        synced.truncate(MAX_LINES);
        return Lyrics {
            synced: true,
            lines: synced,
        };
    }
    // Plain text: without leading and trailing empty lines.
    while plain.first().is_some_and(|l| l.text.is_empty()) {
        plain.remove(0);
    }
    while plain.last().is_some_and(|l| l.text.is_empty()) {
        plain.pop();
    }
    plain.truncate(MAX_LINES);
    Lyrics {
        synced: false,
        lines: plain,
    }
}

impl Lyrics {
    pub fn to_json(&self) -> Value {
        Value::object().with("synced", self.synced).with(
            "lines",
            Value::Array(
                self.lines
                    .iter()
                    .map(|l| {
                        let mut v = Value::object().with("text", l.text.as_str());
                        if let Some(t) = l.time_ms {
                            v = v.with("t", t as f64 / 1000.0);
                        }
                        if !l.words.is_empty() {
                            v = v.with(
                                "words",
                                Value::Array(
                                    l.words
                                        .iter()
                                        .map(|(t, w)| {
                                            Value::object()
                                                .with("t", *t as f64 / 1000.0)
                                                .with("w", w.as_str())
                                        })
                                        .collect(),
                                ),
                            );
                        }
                        v
                    })
                    .collect(),
            ),
        )
    }
}

/// Loading state of the current track's song text.
#[derive(Debug, Clone, Default)]
pub(crate) struct LyricsState {
    /// Track the state is for.
    pub id: String,
    pub loading: bool,
    /// The text and where it came from (`None`: the track has none).
    pub found: Option<(Lyrics, String)>,
    /// The next picked text file is a song text for the current track.
    pub expect_import: bool,
    /// The server is timing the text by speech recognition.
    pub sync: Option<SyncState>,
}

/// A running `lyrics.sync` on the server.
#[derive(Debug, Clone, Default)]
pub(crate) struct SyncState {
    /// `queued` or `running`.
    pub state: String,
    pub progress: f64,
    /// When to ask for the status next (host time).
    pub next_poll: f64,
    /// A status request is under way.
    pub asking: bool,
}

/// Seconds between status requests of a running `lyrics.sync`.
const SYNC_POLL_SECS: f64 = 2.0;

/// Storage key of an imported song text for a track of this device.
fn import_key(id: &str) -> String {
    format!("lyrics/{:016x}.lrc", library::fnv(id))
}

impl Core {
    /// `lyrics.get`: the current track's song text; starts loading it.
    pub(crate) fn lyrics_get(&mut self, host: &mut dyn Host) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("status", "none");
        };
        if self.lyrics.id != id {
            self.lyrics_load(host, &id);
        }
        let l = &self.lyrics;
        let v = match (&l.found, l.loading) {
            (_, true) => Value::object()
                .with("status", "loading")
                .with("id", id.as_str()),
            (None, false) => Value::object()
                .with("status", "none")
                .with("id", id.as_str()),
            (Some((lyrics, source)), false) => lyrics
                .to_json()
                .with("status", "ready")
                .with("id", id.as_str())
                .with("source", source.as_str())
                .with("canRemove", source == "imported"),
        };
        let can_sync = l.sync.is_none()
            && !l.loading
            && library::is_server_id(&id)
            && matches!(&self.server.state, ServerState::Connected(i) if i.speech)
            && !l.found.as_ref().is_some_and(|(lyrics, _)| lyrics.synced);
        let v = v.with("canSync", can_sync);
        match &l.sync {
            Some(s) => v.with(
                "sync",
                Value::object()
                    .with("state", s.state.as_str())
                    .with("progress", s.progress),
            ),
            None => v,
        }
    }

    fn lyrics_load(&mut self, host: &mut dyn Host, id: &str) {
        let sync = (self.lyrics.id == id)
            .then(|| self.lyrics.sync.take())
            .flatten();
        self.lyrics = LyricsState {
            id: id.to_string(),
            loading: true,
            sync,
            ..Default::default()
        };
        let mailbox = self.lyrics_box.clone();
        let key = id.to_string();
        let done = |text: Option<(String, String)>| (key.clone(), text);
        if let Some(path) = library::local_path(id).map(str::to_string) {
            if let Some(b) = host.storage_read(&import_key(id)) {
                let t = bl_media::lyrics::decode_text(&b);
                mailbox.post(done(Some((t, "imported".into()))));
            } else if host.has_threads() {
                let key = key.clone();
                host.spawn(Box::new(move || {
                    let found = bl_media::lyrics::find_lyrics(std::path::Path::new(&path))
                        .map(|(t, s)| (t, s.name().to_string()));
                    mailbox.post((key, found));
                }));
            } else {
                let found = host.fs_read(&path).ok().and_then(|d| {
                    let mut c = std::io::Cursor::new(d);
                    let ext = crate::jobs::extension(&path);
                    bl_media::read_info(&mut c, ext.as_deref(), false)
                        .ok()?
                        .tags
                        .lyrics
                        .map(|t| (t, "embedded".to_string()))
                });
                mailbox.post(done(found));
            }
        } else if let Some(name) = id.strip_prefix("mem:") {
            let found = self.memory_files.get(name).and_then(|d| {
                let mut c = std::io::Cursor::new(d.as_slice());
                bl_media::read_info(&mut c, crate::jobs::extension(name).as_deref(), false)
                    .ok()?
                    .tags
                    .lyrics
                    .map(|t| (t, "embedded".to_string()))
            });
            mailbox.post(done(found));
        } else if id.starts_with(crate::vault::ID_PREFIX) {
            match self.vault_opener(host, id) {
                Some(opener) => {
                    let job = move || {
                        let found = opener().ok().and_then(|(mut src, ext)| {
                            bl_media::read_info(&mut src, ext.as_deref(), false)
                                .ok()?
                                .tags
                                .lyrics
                                .map(|t| (t, "embedded".to_string()))
                        });
                        mailbox.post((key, found));
                    };
                    if host.has_threads() {
                        host.spawn(Box::new(job));
                    } else {
                        job();
                    }
                }
                None => mailbox.post(done(None)),
            }
        } else if let (false, Some(l)) = (self.server.is_connected(), self.offline_lyrics(id)) {
            mailbox.post(done(Some(l)));
        } else if self.server.is_connected() && library::is_server_id(id) {
            let rid = self.server.get(host, &bl_net::api::paths::track_lyrics(id));
            self.requests.insert(rid, Req::Lyrics(id.to_string()));
        } else {
            mailbox.post(done(None));
        }
    }

    /// A server answer to `GET /api/tracks/<id>/lyrics`.
    pub(crate) fn lyrics_response(
        &mut self,
        host: &mut dyn Host,
        id: String,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        let found = match res {
            Ok(r) if r.status == 200 => bl_json::parse_bytes(&r.body).ok().and_then(|v| {
                let text = v.get("text").as_str()?.to_string();
                Some((
                    text,
                    v.get("source").as_str().unwrap_or("server").to_string(),
                ))
            }),
            _ => None,
        };
        // Offline copies follow the server's text.
        match res {
            Ok(r) if r.status == 200 || r.status == 404 => {
                self.offline_set_lyrics(host, &id, found.clone())
            }
            _ => {}
        }
        self.lyrics_box.post((id, found));
    }

    /// Takes loaded song texts (from `tick`).
    pub(crate) fn poll_lyrics(&mut self) {
        for (id, found) in self.lyrics_box.take() {
            if id != self.lyrics.id {
                continue;
            }
            self.lyrics.loading = false;
            self.lyrics.found = found
                .map(|(t, s)| (parse(&t), s))
                .filter(|(l, _)| !l.lines.is_empty());
            self.revs.lyrics += 1;
        }
    }

    /// `lyrics.import`: a song text for the current track (`text`), stored
    /// on the server for its tracks, else on this device.
    pub(crate) fn lyrics_import(&mut self, host: &mut dyn Host, text: &str) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        let text = text.trim();
        if text.is_empty() || text.len() > 256 * 1024 {
            return Value::object().with("error", self.tr("This file has no usable song text"));
        }
        if library::is_server_id(&id) && self.server.is_connected() {
            let rid = self.server.send(
                host,
                "PUT",
                &bl_net::api::paths::track_lyrics(&id),
                text.as_bytes().to_vec(),
                "text/plain; charset=utf-8",
            );
            self.requests.insert(rid, Req::LyricsSaved);
        } else if library::local_path(&id).is_some() {
            host.storage_write(&import_key(&id), text.as_bytes());
        } else {
            return Value::object().with(
                "error",
                self.tr("Song texts can only be added to library songs"),
            );
        }
        self.lyrics = LyricsState {
            id: id.clone(),
            found: Some((parse(text), "imported".into())),
            ..Default::default()
        };
        self.revs.lyrics += 1;
        Value::object().with("ok", true)
    }

    /// Imports a picked file as the current track's song text.
    pub(crate) fn lyrics_import_bytes(&mut self, host: &mut dyn Host, data: &[u8]) {
        let r = if data.len() > 256 * 1024 {
            Value::object().with("error", self.tr("This file has no usable song text"))
        } else {
            self.lyrics_import(host, &bl_media::lyrics::decode_text(data))
        };
        match r.get("error").as_str() {
            Some(e) => self.toast(e.to_string(), true),
            None => self.toast(self.tr("Song text added"), false),
        }
    }

    /// `lyrics.sync`: lets the server time the current track's song text by
    /// speech recognition (or recognize it when there is none).
    pub(crate) fn lyrics_sync(&mut self, host: &mut dyn Host) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        if !library::is_server_id(&id)
            || !matches!(&self.server.state, ServerState::Connected(i) if i.speech)
        {
            return Value::object().with("error", self.tr("The server cannot time song texts"));
        }
        if self.lyrics.id != id {
            self.lyrics_load(host, &id);
        }
        let rid = self.server.send(
            host,
            "POST",
            &bl_net::api::paths::track_lyrics_sync(&id),
            Vec::new(),
            "text/plain",
        );
        self.requests.insert(rid, Req::LyricsSync(id));
        self.lyrics.sync = Some(SyncState {
            state: "queued".into(),
            asking: true,
            ..Default::default()
        });
        self.revs.lyrics += 1;
        Value::object().with("ok", true)
    }

    /// A server answer about a `lyrics.sync`.
    pub(crate) fn lyrics_sync_response(
        &mut self,
        host: &mut dyn Host,
        id: String,
        res: &Result<crate::host::HttpResponse, String>,
    ) {
        if id != self.lyrics.id || self.lyrics.sync.is_none() {
            return;
        }
        let status = match res {
            Ok(r) if (200..300).contains(&r.status) => bl_json::parse_bytes(&r.body).ok(),
            _ => None,
        };
        let state = status
            .as_ref()
            .map(|v| v.str_or("state", "idle").to_string())
            .unwrap_or_else(|| "failed".into());
        match state.as_str() {
            "queued" | "running" => {
                let s = self.lyrics.sync.as_mut().expect("checked above");
                s.state = state;
                s.progress = status.as_ref().map_or(0.0, |v| v.f64_or("progress", 0.0));
                s.asking = false;
                s.next_poll = host.now() + SYNC_POLL_SECS;
            }
            _ => {
                self.lyrics.sync = None;
                if state == "done" {
                    self.toast(self.tr("Song text timed"), false);
                } else {
                    self.toast(self.tr("Timing the song text failed"), true);
                }
                self.lyrics_load(host, &id);
            }
        }
        self.revs.lyrics += 1;
    }

    /// Asks the server how a running `lyrics.sync` is doing (from `tick`).
    pub(crate) fn poll_lyrics_sync(&mut self, host: &mut dyn Host) {
        let Some(s) = &mut self.lyrics.sync else {
            return;
        };
        if s.asking || host.now() < s.next_poll {
            return;
        }
        if !self.server.is_connected() {
            self.lyrics.sync = None;
            self.revs.lyrics += 1;
            return;
        }
        s.asking = true;
        let id = self.lyrics.id.clone();
        let rid = self
            .server
            .get(host, &bl_net::api::paths::track_lyrics_sync(&id));
        self.requests.insert(rid, Req::LyricsSync(id));
    }

    /// `lyrics.remove`: drops the imported song text of the current track.
    pub(crate) fn lyrics_remove(&mut self, host: &mut dyn Host) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        if library::is_server_id(&id) && self.server.is_connected() {
            let rid = self.server.send(
                host,
                "DELETE",
                &bl_net::api::paths::track_lyrics(&id),
                Vec::new(),
                "text/plain",
            );
            self.requests.insert(rid, Req::LyricsSaved);
        } else {
            host.storage_delete(&import_key(&id));
        }
        self.lyrics_load(host, &id);
        Value::object().with("ok", true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn times(l: &Lyrics) -> Vec<(Option<i64>, &str)> {
        l.lines
            .iter()
            .map(|l| (l.time_ms, l.text.as_str()))
            .collect()
    }

    #[test]
    fn times_and_formats() {
        assert_eq!(parse_time("01:23.45"), Some(83_450));
        assert_eq!(parse_time("1:23"), Some(83_000));
        assert_eq!(parse_time("01:23.4"), Some(83_400));
        assert_eq!(parse_time("01:23.456"), Some(83_456));
        assert_eq!(parse_time("01:23:45"), Some(83_450));
        assert_eq!(parse_time("1:02:03.5"), Some(3_723_500));
        for bad in ["", "ar:Someone", "1:60", "x:10", "1:2:3:4", "01:-1"] {
            assert_eq!(parse_time(bad), None, "{bad}");
        }
    }

    #[test]
    fn lrc_with_metadata_offset_and_repeats() {
        let l = parse(
            "[ti:Song]\n[ar:Someone]\n[offset:500]\n\
             [00:10.00][00:30.00]Chorus\n[00:05.00]Intro\n[00:20.00]\nNot timed\n",
        );
        assert!(l.synced);
        assert_eq!(
            times(&l),
            vec![
                (Some(4_500), "Intro"),
                (Some(9_500), "Chorus"),
                (Some(19_500), ""),
                (Some(29_500), "Chorus"),
            ]
        );
    }

    #[test]
    fn enhanced_word_timing() {
        let l = parse("[00:01.00]<00:01.00>Hello <00:01.50>big <00:02.00>world");
        let line = &l.lines[0];
        assert_eq!(line.text, "Hello big world");
        assert_eq!(
            line.words,
            vec![
                (1000, "Hello ".into()),
                (1500, "big ".into()),
                (2000, "world".into())
            ]
        );
    }

    #[test]
    fn plain_text() {
        let l = parse("\n\nFirst line\n\nSecond [not a tag\n\n");
        assert!(!l.synced);
        assert_eq!(
            times(&l),
            vec![
                (None, "First line"),
                (None, ""),
                (None, "Second [not a tag")
            ]
        );
        let j = l.to_json();
        assert_eq!(j.get("lines").as_array().unwrap().len(), 3);
    }
}
