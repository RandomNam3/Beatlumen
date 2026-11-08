//! Song texts: LRC parsing (several time tags per line, `[offset:]`,
//! enhanced LRC word timing) and loading the current track's text from the
//! file, the server or an import.

use crate::core::{Core, Req};
use crate::host::Host;
use crate::library;
use crate::server::ServerState;
use bl_json::Value;

/// At most this many lines are kept (timed lines, or plain ones).
const MAX_LINES: usize = 5000;
/// Longer lines are cut (bytes of the line as written, tags included).
const MAX_LINE_BYTES: usize = 4096;
/// Times beyond this (ms, 100 hours either way) are no song's.
const MAX_TIME_MS: i64 = 100 * 3_600_000;

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Line {
    /// Start in milliseconds (synced text only).
    pub time_ms: Option<i64>,
    pub text: String,
    /// Timed words, or pieces of words (enhanced LRC); joined they give
    /// `text`.
    pub words: Vec<Piece>,
    /// Who sings it (1, 2, …; 0 when the text does not say): from `[v:N]`
    /// tag lines before it or a name before the line ("Anne-Marie: …").
    pub voice: u8,
    /// The singer's name, when the text gives one.
    pub who: String,
}

/// A timed word, or a piece of one (a syllable of a long note), in
/// milliseconds. A word's text keeps the space after it; a piece within
/// a word has none.
#[derive(Debug, Clone, PartialEq)]
pub struct Piece {
    pub t: i64,
    /// When it is over: its own end time, else when the next one starts.
    pub end: i64,
    pub w: String,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Lyrics {
    pub synced: bool,
    pub lines: Vec<Line>,
    /// What made it (`[re:]`), e.g. the speech timing and its model.
    pub creator: Option<String>,
}

/// Whether a timed text can belong to a song of this length.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Fit {
    Fine,
    /// It goes on well after the song has ended.
    Overruns,
    /// It says it was timed for another length (`[length:]`).
    OtherLength,
}

/// How long after a song's end a text may still be sung (ms).
const OVERRUN_MS: i64 = 10_000;
/// How far the length a text was timed for may differ (ms): timings drift
/// noticeably on another edit of a song.
const LENGTH_TOLERANCE_MS: i64 = 2_000;

pub(crate) fn fit(text: &str, lyrics: &Lyrics, duration_ms: Option<i64>) -> Fit {
    let Some(duration) = duration_ms.filter(|d| *d > 0) else {
        return Fit::Fine;
    };
    if !lyrics.synced {
        return Fit::Fine;
    }
    let last = lyrics.lines.iter().filter_map(|l| l.time_ms).max();
    if last.is_some_and(|t| t > duration.saturating_add(OVERRUN_MS)) {
        return Fit::Overruns;
    }
    let stated = text.lines().find_map(|l| {
        let tag = l.trim().strip_prefix("[length:")?.strip_suffix(']')?;
        parse_time(tag)
    });
    if stated.is_some_and(|s| s.abs_diff(duration) > LENGTH_TOLERANCE_MS as u64) {
        return Fit::OtherLength;
    }
    Fit::Fine
}

/// Parses a time like "01:23.45", "1:23", "01:23:45" (hundredths) or
/// "1:02:03.5" into milliseconds; `None` beyond [`MAX_TIME_MS`].
fn parse_time(s: &str) -> Option<i64> {
    parse_time_unchecked(s).filter(|t| (0..=MAX_TIME_MS).contains(t))
}

fn parse_time_unchecked(s: &str) -> Option<i64> {
    let s = s.trim();
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() < 2 || parts.len() > 3 || !parts.iter().all(|p| !p.is_empty()) {
        return None;
    }
    // Digits only, and few enough that the sums below cannot overflow.
    let num = |p: &str| -> Option<i64> {
        (p.len() <= 12 && p.bytes().all(|b| b.is_ascii_digit())).then(|| p.parse().ok())?
    };
    let secs_frac = |p: &str| -> Option<i64> {
        let (sec, frac) = match p.split_once('.') {
            Some((a, b)) => (a, b),
            None => (p, ""),
        };
        let sec = num(sec)?;
        // Digits checked before cutting: a cut inside a character panics.
        if !frac.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
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

/// The line cut to at most `max` bytes (at a character boundary).
fn cut(line: &str, max: usize) -> &str {
    if line.len() <= max {
        return line;
    }
    let mut end = max;
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    &line[..end]
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

/// Text and timed pieces of an enhanced LRC line ("<00:01.00>word ..."):
/// a time before a word or piece starts it; a time right after a word that
/// is followed by a space (or the end) ends it. Ends not given are filled
/// in by [`fill_ends`].
fn words(text: &str, line_start: i64, offset: i64) -> (String, Vec<Piece>) {
    if !text.contains('<') {
        return (text.trim().to_string(), Vec::new());
    }
    let mut out: Vec<(i64, Option<i64>, String)> = Vec::new();
    let mut plain = String::new();
    let mut time = line_start;
    let mut rest = text;
    let mut word = String::new();
    let flush = |out: &mut Vec<(i64, Option<i64>, String)>,
                 time: i64,
                 word: &mut String,
                 end: Option<i64>| {
        if word.is_empty() {
            return;
        }
        // Spaces only: they belong to the word before.
        if word.trim().is_empty() {
            if let Some(last) = out.last_mut() {
                last.2.push_str(word);
            }
            word.clear();
            return;
        }
        out.push((time, end, std::mem::take(word)));
    };
    while !rest.is_empty() {
        if let Some(r) = rest.strip_prefix('<') {
            if let Some(close) = r.find('>') {
                if let Some(t) = parse_time(&r[..close]) {
                    let after = &r[close + 1..];
                    let ends_word = !word.is_empty()
                        && !word.ends_with(char::is_whitespace)
                        && !after.chars().next().is_some_and(|c| !c.is_whitespace());
                    if ends_word {
                        flush(&mut out, time, &mut word, Some(t.saturating_sub(offset)));
                    } else {
                        flush(&mut out, time, &mut word, None);
                    }
                    time = t.saturating_sub(offset);
                    rest = after;
                    continue;
                }
            }
        }
        let c = rest.chars().next().unwrap_or(' ');
        word.push(c);
        plain.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut out, time, &mut word, None);
    // Leading spaces stay with their words; trim the line's ends.
    let trimmed = plain.trim().to_string();
    if let Some(first) = out.first_mut() {
        first.2 = first.2.trim_start().to_string();
    }
    if let Some(last) = out.last_mut() {
        last.2 = last.2.trim_end().to_string();
    }
    out.retain(|(_, _, w)| !w.is_empty());
    let pieces = out
        .into_iter()
        .map(|(t, end, w)| Piece {
            t,
            end: end.unwrap_or(i64::MIN),
            w,
        })
        .collect();
    (trimmed, pieces)
}

/// Fills in the ends of pieces that have none: when the next piece starts,
/// for a line's last one when the next line starts (at most 3 s on).
fn fill_ends(lines: &mut [Line]) {
    let starts: Vec<Option<i64>> = lines.iter().map(|l| l.time_ms).collect();
    for (i, line) in lines.iter_mut().enumerate() {
        let next_line = starts[i + 1..].iter().flatten().copied().next();
        let n = line.words.len();
        for k in 0..n {
            if line.words[k].end != i64::MIN {
                continue;
            }
            let t = line.words[k].t;
            let next = if k + 1 < n {
                line.words[k + 1].t
            } else {
                let most = t.saturating_add(3000);
                next_line.unwrap_or(most).min(most)
            };
            line.words[k].end = next.max(t);
        }
    }
}

/// The parts of a line in and outside parentheses (in parentheses: a
/// backing voice), as `[{"s": text, "b": in parentheses}]`.
fn backing_parts(text: &str) -> Value {
    let mut parts: Vec<(String, bool)> = Vec::new();
    let mut depth = 0usize;
    for c in text.chars() {
        if c == '(' {
            depth += 1;
        }
        let b = depth > 0;
        match parts.last_mut() {
            Some(p) if p.1 == b => p.0.push(c),
            _ => parts.push((c.to_string(), b)),
        }
        if c == ')' {
            depth = depth.saturating_sub(1);
        }
    }
    Value::Array(
        parts
            .into_iter()
            .map(|(s, b)| Value::object().with("s", s).with("b", b))
            .collect(),
    )
}

/// Which pieces of a line are sung by a backing voice (in parentheses).
fn backing_pieces(words: &[Piece]) -> Vec<bool> {
    let mut depth = 0usize;
    words
        .iter()
        .map(|p| {
            let b = depth > 0 || p.w.trim_start().starts_with('(');
            for c in p.w.chars() {
                match c {
                    '(' => depth += 1,
                    ')' => depth = depth.saturating_sub(1),
                    _ => {}
                }
            }
            b
        })
        .collect()
}

/// Names before lines ("Anne-Marie: You say…", or the duet marks "M:",
/// "F:", "D:") become voices: a name counts when it starts at least two
/// lines (so a sung "Baby: …" once is left alone). The name leaves the
/// line's text (and its first timed words).
fn voices_from_names(lines: &mut [Line]) {
    fn name_of(text: &str) -> Option<&str> {
        let (name, rest) = text.split_once(':')?;
        let name = name.trim();
        let ok = !name.is_empty()
            && name.chars().count() <= 24
            && name.split_whitespace().count() <= 3
            && name.chars().next().is_some_and(char::is_uppercase)
            && name
                .chars()
                .all(|c| c.is_alphanumeric() || " &-'.".contains(c))
            && !rest.trim().is_empty()
            && rest.starts_with(' ');
        ok.then_some(name)
    }
    let mut counts: Vec<(String, usize)> = Vec::new();
    for l in lines.iter() {
        if let Some(n) = name_of(&l.text) {
            match counts.iter_mut().find(|(c, _)| c == n) {
                Some(c) => c.1 += 1,
                None => counts.push((n.to_string(), 1)),
            }
        }
    }
    let duet = |n: &str| matches!(n, "M" | "F" | "D");
    let names: Vec<String> = counts
        .into_iter()
        .filter(|(n, c)| *c >= 2 || duet(n))
        .map(|(n, _)| n)
        .collect();
    if names.is_empty() {
        return;
    }
    for l in lines.iter_mut() {
        let Some(n) = name_of(&l.text).map(str::to_string) else {
            continue;
        };
        let Some(i) = names.iter().position(|x| *x == n) else {
            continue;
        };
        let prefix = l.text.find(':').map_or(0, |p| p + 1);
        l.text = l.text[prefix..].trim_start().to_string();
        // The name's pieces leave the timed words too.
        let mut drop = prefix;
        while drop > 0 && !l.words.is_empty() {
            let len = l.words[0].w.trim_end().len();
            if len > drop {
                break;
            }
            drop = drop.saturating_sub(l.words[0].w.len());
            l.words.remove(0);
        }
        l.voice = (i + 1).min(9) as u8;
        l.who = if duet(&n) { String::new() } else { n };
    }
}

pub fn parse(text: &str) -> Lyrics {
    let mut offset = 0i64;
    let mut creator = None;
    let mut voice = 0u8;
    let mut synced: Vec<Line> = Vec::new();
    let mut plain: Vec<Line> = Vec::new();
    'lines: for raw in text.lines() {
        let raw = cut(raw, MAX_LINE_BYTES);
        let (tags, rest) = leading_tags(raw);
        let mut times = Vec::new();
        let mut meta_only = !tags.is_empty();
        for t in &tags {
            if let Some(ms) = parse_time(t) {
                times.push(ms);
                meta_only = false;
            } else if let Some(v) = t.strip_prefix("offset:") {
                offset = v
                    .trim()
                    .parse::<i64>()
                    .unwrap_or(0)
                    .clamp(-MAX_TIME_MS, MAX_TIME_MS);
            }
        }
        if times.is_empty() {
            if meta_only && rest.trim().is_empty() {
                if let Some(c) = tags.iter().find_map(|t| t.strip_prefix("re:")) {
                    creator = Some(c.trim().to_string());
                }
                // [v:N]: voice N sings the lines that follow.
                if let Some(v) = tags.iter().find_map(|t| t.strip_prefix("v:")) {
                    voice = v.trim().parse::<u8>().unwrap_or(0).min(9);
                }
                continue; // [ar:...], [ti:...], ...
            }
            // Leading empty lines are left out; enough lines are enough
            // (timed lines further on still count).
            if (plain.is_empty() && raw.trim().is_empty()) || plain.len() >= MAX_LINES {
                continue;
            }
            plain.push(Line {
                time_ms: None,
                text: raw.trim().to_string(),
                words: Vec::new(),
                voice,
                who: String::new(),
            });
            continue;
        }
        for t in times {
            if synced.len() > MAX_LINES {
                break 'lines;
            }
            let start = t.saturating_sub(offset);
            let (text, words) = words(rest, start, offset);
            synced.push(Line {
                time_ms: Some(start.clamp(0, MAX_TIME_MS)),
                text,
                words,
                voice,
                who: String::new(),
            });
        }
    }
    if !synced.is_empty() {
        synced.sort_by_key(|l| l.time_ms);
        synced.truncate(MAX_LINES);
        fill_ends(&mut synced);
        voices_from_names(&mut synced);
        return Lyrics {
            synced: true,
            lines: synced,
            creator,
        };
    }
    // Plain text: without leading (left out above) and trailing empty lines.
    while plain.last().is_some_and(|l| l.text.is_empty()) {
        plain.pop();
    }
    voices_from_names(&mut plain);
    Lyrics {
        synced: false,
        lines: plain,
        creator,
    }
}

impl Lyrics {
    /// Whether it has word times.
    pub fn word_timed(&self) -> bool {
        self.lines.iter().any(|l| !l.words.is_empty())
    }

    /// The text again: LRC lines (without word times) when it has times,
    /// else its lines.
    pub fn to_lrc(&self) -> String {
        self.lines
            .iter()
            .map(|l| match l.time_ms {
                Some(t) if self.synced => {
                    let cs = t.max(0) / 10;
                    format!(
                        "[{:02}:{:02}.{:02}]{}\n",
                        cs / 6000,
                        cs / 100 % 60,
                        cs % 100,
                        l.text
                    )
                }
                _ => format!("{}\n", l.text),
            })
            .collect()
    }

    /// The text as enhanced LRC with what it holds now: line and word
    /// times (word end times where a pause follows), singers' names before
    /// their lines and `[v:N]` for voices without a name.
    pub fn to_enhanced_lrc(&self) -> String {
        fn time(ms: i64) -> String {
            let cs = ms.max(0) / 10;
            format!("{:02}:{:02}.{:02}", cs / 6000, cs / 100 % 60, cs % 100)
        }
        let mut out = String::new();
        if let Some(c) = &self.creator {
            out += &format!("[re:{c}]\n");
        }
        let mut voice = 0u8;
        for l in &self.lines {
            if l.who.is_empty() && l.voice != voice && l.voice > 0 {
                out += &format!("[v:{}]\n", l.voice);
            }
            voice = l.voice;
            if let (Some(t), true) = (l.time_ms, self.synced) {
                out += &format!("[{}]", time(t));
            }
            if !l.who.is_empty() {
                out += &format!("{}: ", l.who);
            }
            if l.words.is_empty() || !self.synced {
                out += &l.text;
            } else {
                for (i, p) in l.words.iter().enumerate() {
                    out += &format!("<{}>{}", time(p.t), p.w);
                    let next = l.words.get(i + 1).map(|n| n.t);
                    if next.map_or(true, |n| p.end < n) && p.end > p.t {
                        out += &format!("<{}>", time(p.end));
                    }
                }
            }
            out.push('\n');
        }
        out
    }

    pub fn to_json(&self) -> Value {
        Value::object().with("synced", self.synced).with(
            "lines",
            Value::Array(
                self.lines
                    .iter()
                    .map(|l| {
                        let mut v = Value::object()
                            .with("text", l.text.as_str())
                            .with("parts", backing_parts(&l.text));
                        if let Some(t) = l.time_ms {
                            v = v.with("t", t as f64 / 1000.0);
                        }
                        if l.voice > 0 {
                            v = v.with("v", l.voice as u64);
                        }
                        if !l.who.is_empty() {
                            v = v.with("who", l.who.as_str());
                        }
                        if !l.words.is_empty() {
                            v = v.with(
                                "words",
                                Value::Array(
                                    l.words
                                        .iter()
                                        .zip(backing_pieces(&l.words))
                                        .map(|(p, b)| {
                                            let mut o = Value::object()
                                                .with("t", p.t as f64 / 1000.0)
                                                .with("e", p.end as f64 / 1000.0)
                                                .with("w", p.w.as_str());
                                            if b {
                                                o = o.with("b", true);
                                            }
                                            o
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

/// The speech timing's version and model from an LRC's creator
/// (`Beatlumen speech timing 2, model base`, as `bl_asr::sync` writes it;
/// read here too because the browser build has no speech recognition).
fn speech_creator(c: &str) -> Option<(u32, String)> {
    let rest = c
        .trim()
        .strip_prefix("Beatlumen speech timing")?
        .trim_start();
    let (version, model) = rest.split_once(',')?;
    let model = model.trim().strip_prefix("model")?.trim();
    Some((version.trim().parse().ok()?, model.to_string()))
}

/// Storage key of an imported song text for a track of this device.
pub(crate) fn import_key(id: &str) -> String {
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
                .with("wordTimed", lyrics.word_timed())
                .with("origin", self.lyrics_origin(&id, lyrics, source))
                .with("canRemove", source == "imported"),
        };
        // Timing is offered while the text has no word times.
        let can_sync = l.sync.is_none()
            && !l.loading
            && library::is_server_id(&id)
            && matches!(&self.server.state, ServerState::Connected(i) if i.speech)
            && !l
                .found
                .as_ref()
                .is_some_and(|(lyrics, _)| lyrics.word_timed());
        let mut v = v
            .with("canSync", can_sync)
            .with("translation", self.translate_json());
        // The translation under each line.
        if let Some((lyrics, _)) = l.found.as_ref().filter(|_| !l.loading) {
            if let (Some(tr), Some(lines)) = (
                self.translated_lines(&id, lyrics.lines.len()),
                v.as_object_mut()
                    .and_then(|o| o.get_mut("lines"))
                    .and_then(Value::as_array_mut),
            ) {
                for (line, t) in lines.iter_mut().zip(tr) {
                    if !t.is_empty() && t.trim() != line.get("text").as_str().unwrap_or("").trim() {
                        line.set("tr", t.as_str());
                    }
                }
            }
        }
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

    /// Where a song text comes from and where it is kept, for the user.
    fn lyrics_origin(&self, id: &str, lyrics: &Lyrics, source: &str) -> String {
        let on_server = library::is_server_id(id);
        let speech = lyrics.creator.as_deref().and_then(speech_creator);
        let what = match (source, &speech) {
            ("lrc", _) => self.tr("From the song's LRC file"),
            ("ttml", _) => self.tr("From the song's TTML file"),
            ("embedded", _) => self.tr("From the music file"),
            ("txt", _) => self.tr("From the song's text file"),
            ("recognized", _) => self.tr("Written down by speech recognition, saved on the server"),
            ("synced", _) => self.tr("Timed by speech recognition, saved on the server"),
            (_, Some(_)) if on_server => {
                self.tr("Timed by speech recognition, saved on the server")
            }
            (_, Some(_)) => self.tr("Timed by speech recognition, saved on this device"),
            (_, None) if on_server => self.tr("Added by you, saved on the server"),
            (_, None) => self.tr("Added by you, saved on this device"),
        };
        match speech {
            Some((_, model)) => format!("{what} · {} {model}", self.tr("speech model")),
            None => what.to_string(),
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
        } else if id.starts_with(crate::sources::ID_PREFIX) {
            // Songs of the app's sources: texts added here (or computed).
            let found = host
                .storage_read(&import_key(id))
                .map(|b| (bl_media::lyrics::decode_text(&b), "imported".to_string()));
            mailbox.post(done(found));
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

    /// `lyrics.edit`: the current song text changed by hand, then stored
    /// like an import. `lines`: `[{t, text, from}]`, `t` the start in
    /// seconds (none for a plain text), `from` the index of the line it was
    /// (-1 for a new one). A line keeps its words' times, moved with it,
    /// while its text is unchanged.
    pub(crate) fn lyrics_edit(&mut self, host: &mut dyn Host, c: &Value) -> Value {
        let err = |m: &str| Value::object().with("error", m);
        let Some(id) = self.queue.current().cloned() else {
            return err("nothing playing");
        };
        let Some(lines) = c.get("lines").as_array().filter(|l| l.len() <= 4000) else {
            return err("no lines");
        };
        let old = (self.lyrics.id == id)
            .then(|| self.lyrics.found.as_ref().map(|(l, _)| l.clone()))
            .flatten()
            .unwrap_or_default();
        let mut out = Lyrics {
            synced: false,
            lines: Vec::with_capacity(lines.len()),
            creator: None,
        };
        let mut words_kept = true;
        for v in lines {
            let text: String = v
                .str_or("text", "")
                .chars()
                .filter(|c| *c != '\n' && *c != '\r')
                .take(1000)
                .collect();
            let t = v
                .get("t")
                .as_f64()
                .filter(|t| t.is_finite() && *t >= 0.0 && *t < 24.0 * 3600.0);
            let time_ms = t.map(|t| (t * 1000.0).round() as i64);
            out.synced |= time_ms.is_some();
            let from = v.get("from").as_i64().and_then(|i| usize::try_from(i).ok());
            let line = match from.and_then(|i| old.lines.get(i)) {
                Some(o) if o.text == text => {
                    let shift = match (time_ms, o.time_ms) {
                        (Some(a), Some(b)) => a - b,
                        _ => 0,
                    };
                    let words = o
                        .words
                        .iter()
                        .map(|p| Piece {
                            t: p.t + shift,
                            end: p.end + shift,
                            w: p.w.clone(),
                        })
                        .collect();
                    Line {
                        time_ms,
                        words,
                        ..o.clone()
                    }
                }
                Some(o) => {
                    words_kept &= o.words.is_empty();
                    Line {
                        time_ms,
                        text,
                        words: Vec::new(),
                        voice: o.voice,
                        who: o.who.clone(),
                    }
                }
                None => Line {
                    time_ms,
                    text,
                    ..Default::default()
                },
            };
            out.lines.push(line);
        }
        // Times by speech recognition stay named only while all its word
        // times are kept.
        if words_kept && out.lines.iter().any(|l| !l.words.is_empty()) {
            out.creator = old.creator.clone();
        }
        self.lyrics_import(host, &out.to_enhanced_lrc())
    }

    /// `lyrics.import`: a song text for the current track (`text`), stored
    /// on the server for its tracks, else on this device.
    pub(crate) fn lyrics_import(&mut self, host: &mut dyn Host, text: &str) -> Value {
        let Some(id) = self.queue.current().cloned() else {
            return Value::object().with("error", "nothing playing");
        };
        let text = bl_media::lyrics::normalize_text(text);
        let text = text.trim();
        if text.is_empty() || text.len() > 256 * 1024 {
            return Value::object().with("error", self.tr("This file has no usable song text"));
        }
        let duration = self.player.duration().map(|d| (d * 1000.0) as i64);
        let warning = match fit(text, &parse(text), duration) {
            Fit::Fine => None,
            Fit::Overruns | Fit::OtherLength => {
                Some(self.tr("This song text probably belongs to another version of the song: its times do not fit the length."))
            }
        };
        if library::is_server_id(&id) && self.server.is_connected() {
            let rid = self.server.send(
                host,
                "PUT",
                &bl_net::api::paths::track_lyrics(&id),
                text.as_bytes().to_vec(),
                "text/plain; charset=utf-8",
            );
            self.requests.insert(rid, Req::LyricsSaved);
        } else if library::local_path(&id).is_some() || id.starts_with(crate::sources::ID_PREFIX) {
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
        let answer = Value::object().with("ok", true);
        match warning {
            Some(w) => answer.with("warning", w),
            None => answer,
        }
    }

    /// Imports a picked file as the current track's song text.
    pub(crate) fn lyrics_import_bytes(&mut self, host: &mut dyn Host, data: &[u8]) {
        let r = if data.len() > 256 * 1024 {
            Value::object().with("error", self.tr("This file has no usable song text"))
        } else {
            self.lyrics_import(host, &bl_media::lyrics::decode_text(data))
        };
        match (r.get("error").as_str(), r.get("warning").as_str()) {
            (Some(e), _) => self.toast(e.to_string(), true),
            (None, Some(w)) => self.toast(w.to_string(), true),
            _ => self.toast(self.tr("Song text added"), false),
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

    #[test]
    fn a_ttml_file_ends_up_with_its_words_voices_and_pauses() {
        let lrc = bl_media::ttml_to_lrc(
            r#"<tt xmlns:ttm="x"><body><div>
              <p begin="1s" ttm:agent="v1"><span begin="1s" end="1.5s">Hel</span><span begin="1.5s" end="2s">lo</span> <span begin="3s" end="4s">world</span></p>
              <p begin="5s" ttm:agent="v2"><span begin="5s" end="6s">Other</span> <span ttm:role="x-bg"><span begin="6s" end="7s">oh</span></span></p>
            </div></body></tt>"#,
        )
        .unwrap();
        let l = parse(&lrc);
        assert!(l.synced);
        assert!(l.word_timed());
        assert_eq!(l.lines.len(), 2);
        let first = &l.lines[0];
        assert_eq!(first.text, "Hello world");
        assert_eq!(first.voice, 1);
        assert_eq!(first.time_ms, Some(1000));
        assert_eq!(first.words[0].w, "Hel");
        assert_eq!(first.words[1].w, "lo ");
        // The pause before "world" is kept.
        assert_eq!(first.words[1].end, 2000);
        assert_eq!(first.words[2].t, 3000);
        assert_eq!(first.words[2].end, 4000);
        let second = &l.lines[1];
        assert_eq!(second.voice, 2);
        assert_eq!(second.text, "Other (oh)");
    }

    #[test]
    fn a_text_that_cannot_belong_to_the_song_is_told_apart() {
        let timed = "[00:10.00]A\n[03:00.00]B\n";
        let l = parse(timed);
        assert_eq!(fit(timed, &l, Some(200_000)), Fit::Fine);
        // A few seconds past the end: a fade-out, not another song.
        assert_eq!(fit(timed, &l, Some(175_000)), Fit::Fine);
        assert_eq!(fit(timed, &l, Some(120_000)), Fit::Overruns);
        // Nothing to compare with.
        assert_eq!(fit(timed, &l, None), Fit::Fine);
        // Without times there is nothing that could not fit.
        let plain = "A\nB\n";
        assert_eq!(fit(plain, &parse(plain), Some(1000)), Fit::Fine);
        // Its own statement of the length.
        let stated = "[length:03:30]\n[00:10.00]A\n";
        let l = parse(stated);
        assert_eq!(fit(stated, &l, Some(211_000)), Fit::Fine);
        assert_eq!(fit(stated, &l, Some(240_000)), Fit::OtherLength);
    }

    #[test]
    fn edited_texts_are_written_back_unchanged() {
        let text = "[re:Beatlumen speech timing 2, model base]\n\
            [00:01.00]Anne-Marie: <00:01.00>You <00:01.40>say<00:02.00>\n\
            [00:03.00]Marshmello: <00:03.00>Hey\n\
            [00:05.00]Anne-Marie: <00:05.00>Fri<00:05.30>ends <00:06.00>(oh)<00:06.50>\n\
            [v:2]\n[00:08.00]Plain line\n";
        let l = parse(text);
        let back = parse(&l.to_enhanced_lrc());
        assert_eq!(back, l, "{}", l.to_enhanced_lrc());
        assert_eq!(back.lines[0].who, "Anne-Marie");
        assert_eq!(back.lines[2].words.len(), 3);
    }
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
    fn crafted_texts_end_cleanly() {
        // Times and offsets at the ends of the number range.
        for text in [
            "[offset:-9223372036854775808]\n[00:01.00]a",
            "[offset:9223372036854775807]\n[00:01.00]<00:02.00>a <00:03.00>b",
            "[99999999999999999999:00.00]x",
            "[999999999999:59.99]x\n[00:01]y",
            "[00:01.abé]x\n[00:01.1é]y",
            "[00:01]<99999999999:00>a<00:01.é>b",
        ] {
            let l = parse(text);
            for line in &l.lines {
                assert!(line.time_ms.is_none_or(|t| (0..=MAX_TIME_MS).contains(&t)));
            }
            let _ = fit(text, &l, Some(i64::MAX));
            let _ = fit(text, &l, Some(1));
            let _ = l.to_enhanced_lrc();
        }
        assert_eq!(parse_time("00:01.abé"), None);
        // Plain lines: capped, and many leading empty lines are cheap.
        let mut text = "\n".repeat(2_000_000);
        text += &"a line\n".repeat(20_000);
        let t = std::time::Instant::now();
        let l = parse(&text);
        assert!(!l.synced);
        assert_eq!(l.lines.len(), MAX_LINES);
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
        // Many time tags on one long line: lines and their length stay bounded.
        let line = "[00:01.00]".repeat(50_000) + &"x".repeat(1_000_000);
        let l = parse(&line.repeat(3));
        assert!(l.lines.len() <= MAX_LINES);
        assert!(l.lines.iter().all(|x| x.text.len() <= MAX_LINE_BYTES));
        // A long line is cut at a character boundary.
        let l = parse(&"ä".repeat(10_000));
        assert_eq!(l.lines.len(), 1);
        assert!(l.lines[0].text.len() <= MAX_LINE_BYTES);
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
        let l = parse("[00:01.00]<00:01.00>Hello <00:01.50>big <00:02.00>world\n[00:04.00]x");
        let line = &l.lines[0];
        assert_eq!(line.text, "Hello big world");
        let p = |t, end, w: &str| Piece {
            t,
            end,
            w: w.into(),
        };
        assert_eq!(
            line.words,
            vec![
                p(1000, 1500, "Hello "),
                p(1500, 2000, "big "),
                p(2000, 4000, "world")
            ]
        );
        // Word ends before pauses, pieces of a long word, the creator tag.
        let l = parse(
            "[re:Beatlumen speech timing 2, model base]\n\
             [00:01.00]<00:01.00>Ooh<00:01.40> <00:02.00>frie<00:02.50>nds<00:03.10>",
        );
        assert_eq!(
            l.creator.as_deref(),
            Some("Beatlumen speech timing 2, model base")
        );
        assert_eq!(l.lines[0].text, "Ooh frie".to_string() + "nds");
        assert_eq!(
            l.lines[0].words,
            vec![
                p(1000, 1400, "Ooh "),
                p(2000, 2500, "frie"),
                p(2500, 3100, "nds")
            ]
        );
    }

    #[test]
    fn voices_and_backing() {
        let l = parse(
            "[00:01.00]Anne-Marie: You say you love me\n[00:03.00]Marshmello: (Oh)\n\
             [00:05.00]Anne-Marie: Here we go\n[00:07.00]Marshmello: again\n\
             [v:3]\n[00:09.00]<00:09.00>Haven't <00:09.50>I <00:10.00>(made <00:10.50>it)\n",
        );
        let v: Vec<(u8, &str, &str)> = l
            .lines
            .iter()
            .map(|x| (x.voice, x.who.as_str(), x.text.as_str()))
            .collect();
        assert_eq!(
            v,
            vec![
                (1, "Anne-Marie", "You say you love me"),
                (2, "Marshmello", "(Oh)"),
                (1, "Anne-Marie", "Here we go"),
                (2, "Marshmello", "again"),
                (3, "", "Haven't I (made it)"),
            ]
        );
        assert_eq!(
            backing_pieces(&l.lines[4].words),
            vec![false, false, true, true]
        );
        let j = backing_parts("Haven't I made it? (Haven't I?)");
        assert_eq!(j.at(1).str_or("s", ""), "(Haven't I?)");
        assert!(j.at(1).bool_or("b", false));
        // A name once is part of the song.
        let l = parse("Baby: come back\nother line");
        assert_eq!(l.lines[0].text, "Baby: come back");
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
