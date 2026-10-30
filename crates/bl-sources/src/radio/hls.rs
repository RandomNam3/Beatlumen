//! HTTP Live Streaming (RFC 8216) for radio. A master playlist picks the
//! audio-only variant with the highest bandwidth (else an audio rendition,
//! else the first variant); the media playlist's segments are fetched in
//! order and read as one stream. A live playlist starts three segments
//! from its end and is reloaded about every target duration (half of it
//! while it does not change); one with `#EXT-X-ENDLIST` plays from its
//! start to its end.
//!
//! Segments: MPEG-TS (demuxed to ADTS AAC or MPEG audio by
//! [`TsDemuxer`]) and packed audio (`.aac` ADTS, `.mp3`, whose leading ID3
//! timestamp tags are removed). Not supported, with a clear error: fMP4 /
//! CMAF (`#EXT-X-MAP`), encryption (`#EXT-X-KEY` other than NONE) and byte
//! ranges. `#EXTINF` titles ("Artist - Title" or `title="…",artist="…"`)
//! are published like ICY titles when their segment is fetched.
//!
//! Limits: playlists 1 MiB and 20000 segments, segments 16 MiB. A segment
//! that fails twice is skipped; three failures in a row end the stream,
//! as does a live playlist that stops changing.

use super::{fetch, join, set_title, sniff_codec, Reach, Titles, TsDemuxer};
use crate::playlist::decode_text;
use std::collections::VecDeque;
use std::io::{self, Read};
use std::time::{Duration, Instant};

const MAX_PLAYLIST: usize = 1 << 20;
const MAX_SEGMENTS: usize = 20_000;
const MAX_SEGMENT: usize = 16 << 20;
const MAX_TITLE: usize = 512;
/// Reloads without new segments before a live stream counts as ended.
const MAX_UNCHANGED: u32 = 12;
/// Failed segments or reloads in a row before the stream fails.
const MAX_FAILURES: u32 = 3;

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Segment {
    pub seq: u64,
    pub url: String,
    pub duration: f64,
    pub title: Option<String>,
    /// Starts after `#EXT-X-DISCONTINUITY` (or a jump in the sequence).
    pub discontinuity: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct MediaPlaylist {
    /// Seconds, 1 to 60.
    pub target: f64,
    pub segments: Vec<Segment>,
    pub ended: bool,
}

/// An HLS playlist (it has `#EXT-X-` tags), not a list of stream URLs.
pub(crate) fn is_hls(text: &str) -> bool {
    text.contains("#EXT-X-")
}

fn is_master(text: &str) -> bool {
    text.contains("#EXT-X-STREAM-INF")
}

/// An attribute list `KEY=value,KEY="quoted, value"` (keys upper case).
fn attrs(s: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut rest = s.trim();
    while out.len() < 64 {
        let Some(eq) = rest.find('=') else {
            break;
        };
        let key = rest[..eq].trim().to_ascii_uppercase();
        let after = &rest[eq + 1..];
        let (value, next) = match after.strip_prefix('"') {
            Some(q) => match q.find('"') {
                Some(i) => (&q[..i], &q[i + 1..]),
                None => (q, ""),
            },
            None => after.split_at(after.find(',').unwrap_or(after.len())),
        };
        out.push((key, value.trim().to_string()));
        rest = next.trim_start().trim_start_matches(',').trim_start();
    }
    out
}

fn attr<'a>(a: &'a [(String, String)], key: &str) -> Option<&'a str> {
    a.iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .filter(|v| !v.is_empty())
}

/// One line of text, at most [`MAX_TITLE`] characters.
fn clip(s: &str) -> String {
    s.chars()
        .take(MAX_TITLE)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect::<String>()
        .trim()
        .to_string()
}

/// `#EXTINF:` value: duration and title.
fn extinf(v: &str) -> (f64, Option<String>) {
    let (d, rest) = v.split_once(',').unwrap_or((v, ""));
    let d = d
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|d| d.is_finite() && *d >= 0.0)
        .unwrap_or(0.0);
    let rest = rest.trim();
    let title = if rest.contains("=\"") {
        let a = attrs(rest);
        match (attr(&a, "ARTIST"), attr(&a, "TITLE")) {
            (Some(ar), Some(t)) => Some(format!("{ar} - {t}")),
            (None, Some(t)) => Some(t.to_string()),
            _ => None,
        }
    } else {
        Some(rest.to_string())
    };
    (d, title.map(|t| clip(&t)).filter(|t| !t.is_empty()))
}

/// Reads a media playlist; segment URLs are resolved against `base`.
pub(crate) fn parse_media(text: &str, base: &str) -> Result<MediaPlaylist, String> {
    let mut pl = MediaPlaylist::default();
    let mut first_seq = 0u64;
    let mut pending: Option<(f64, Option<String>)> = None;
    let mut discontinuity = false;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.len() > 8192 {
            continue;
        }
        let Some(tag) = line.strip_prefix('#') else {
            if pl.segments.len() < MAX_SEGMENTS {
                let (duration, title) = pending.take().unwrap_or((0.0, None));
                pl.segments.push(Segment {
                    seq: 0,
                    url: join(base, line),
                    duration,
                    title,
                    discontinuity: std::mem::take(&mut discontinuity),
                });
            }
            continue;
        };
        if let Some(v) = tag.strip_prefix("EXTINF:") {
            pending = Some(extinf(v));
        } else if let Some(v) = tag.strip_prefix("EXT-X-TARGETDURATION:") {
            pl.target = v.trim().parse().unwrap_or(0.0);
        } else if let Some(v) = tag.strip_prefix("EXT-X-MEDIA-SEQUENCE:") {
            first_seq = v
                .trim()
                .parse()
                .map_err(|_| "HLS: invalid media sequence")?;
        } else if tag == "EXT-X-ENDLIST" {
            pl.ended = true;
        } else if tag == "EXT-X-DISCONTINUITY" {
            discontinuity = true;
        } else if let Some(v) = tag.strip_prefix("EXT-X-KEY:") {
            let a = attrs(v);
            if !attr(&a, "METHOD").is_some_and(|m| m.eq_ignore_ascii_case("NONE")) {
                return Err("encrypted HLS streams are not supported".into());
            }
        } else if tag.starts_with("EXT-X-MAP") {
            return Err("HLS with fMP4/CMAF segments is not supported".into());
        } else if tag.starts_with("EXT-X-BYTERANGE") {
            return Err("HLS byte-range segments are not supported".into());
        } else if tag.starts_with("EXT-X-STREAM-INF") {
            return Err("HLS: a master playlist where a media playlist was expected".into());
        }
    }
    for (i, s) in pl.segments.iter_mut().enumerate() {
        s.seq = first_seq.saturating_add(i as u64);
    }
    if !(pl.target.is_finite() && pl.target > 0.0) {
        pl.target = pl.segments.iter().map(|s| s.duration).fold(0.0, f64::max);
    }
    pl.target = if pl.target > 0.0 {
        pl.target.clamp(1.0, 60.0)
    } else {
        10.0
    };
    Ok(pl)
}

/// Only codecs the decoders read (AAC and MPEG audio).
fn audio_only(codecs: &str) -> bool {
    codecs.split(',').all(|c| {
        let c = c.trim().to_ascii_lowercase();
        c.starts_with("mp4a") || c == "mp3"
    })
}

/// The media playlist to play from a master playlist.
pub(crate) fn pick_variant(text: &str, base: &str) -> Option<String> {
    // (bandwidth, audio only, URL)
    let mut variants: Vec<(u64, bool, String)> = Vec::new();
    // (default, URL)
    let mut renditions: Vec<(bool, String)> = Vec::new();
    let mut pending: Option<(u64, bool)> = None;
    for line in text.lines().map(str::trim) {
        if variants.len() + renditions.len() >= 1000 {
            break;
        }
        if let Some(v) = line.strip_prefix("#EXT-X-STREAM-INF:") {
            let a = attrs(v);
            let bandwidth = attr(&a, "BANDWIDTH")
                .and_then(|b| b.parse().ok())
                .unwrap_or(0);
            pending = Some((bandwidth, attr(&a, "CODECS").is_some_and(audio_only)));
        } else if let Some(v) = line.strip_prefix("#EXT-X-MEDIA:") {
            let a = attrs(v);
            if let (Some("AUDIO"), Some(uri)) = (attr(&a, "TYPE"), attr(&a, "URI")) {
                renditions.push((attr(&a, "DEFAULT") == Some("YES"), join(base, uri)));
            }
        } else if !line.is_empty() && !line.starts_with('#') {
            if let Some((bandwidth, audio)) = pending.take() {
                variants.push((bandwidth, audio, join(base, line)));
            }
        }
    }
    variants
        .iter()
        .filter(|v| v.1)
        .max_by_key(|v| v.0)
        .map(|v| v.2.clone())
        .or_else(|| {
            renditions
                .iter()
                .find(|r| r.0)
                .or(renditions.first())
                .map(|r| r.1.clone())
        })
        .or_else(|| variants.first().map(|v| v.2.clone()))
}

/// Which segments to fetch, across reloads of a live playlist.
#[derive(Debug, Default)]
pub(crate) struct Tracker {
    /// Sequence number of the next segment to queue (None before the
    /// first playlist).
    next: Option<u64>,
    pub queue: VecDeque<Segment>,
}

impl Tracker {
    /// Queues the playlist's segments that were not queued before; returns
    /// how many.
    pub fn merge(&mut self, pl: &MediaPlaylist) -> usize {
        let (Some(first), Some(last)) = (pl.segments.first(), pl.segments.last()) else {
            return 0;
        };
        // A live stream starts three segments from its end (RFC 8216 6.3.3).
        let start = if pl.ended {
            first.seq
        } else {
            pl.segments[pl.segments.len().saturating_sub(3)].seq
        };
        let window = pl.segments.len() as u64;
        let next = match self.next {
            None => start,
            // The numbering went back by more than the playlist's length:
            // the stream was restarted.
            Some(n) if last.seq.saturating_add(window + 1) < n => start,
            // Fell behind: the wanted segments are gone.
            Some(n) if n < first.seq => first.seq,
            Some(n) => n,
        };
        let jumped = self.next.is_some_and(|n| n != next);
        let mut added = 0;
        for s in pl.segments.iter().filter(|s| s.seq >= next) {
            if self.queue.len() >= MAX_SEGMENTS {
                break;
            }
            let mut s = s.clone();
            s.discontinuity |= jumped && added == 0;
            self.next = Some(s.seq + 1);
            self.queue.push_back(s);
            added += 1;
        }
        if added == 0 && self.next.is_none() {
            self.next = Some(next);
        }
        added
    }
}

/// An HLS stream as one continuous reader of elementary audio (ADTS AAC or
/// MPEG audio frames).
pub struct HlsStream {
    /// The media playlist (reloaded while live).
    url: String,
    reach: Reach,
    target: Duration,
    ended: bool,
    tracker: Tracker,
    last_load: Instant,
    unchanged: u32,
    failures: u32,
    /// Segments loaded without finding the codec.
    probes: u32,
    demux: TsDemuxer,
    codec: &'static str,
    titles: Titles,
    out: Vec<u8>,
    pos: usize,
}

impl HlsStream {
    /// Opens a master or media playlist and fetches the first segment (to
    /// know the codec).
    pub fn open(url: &str) -> Result<HlsStream, String> {
        let reach = Reach::of(url);
        let (url, body) = fetch(url, reach, MAX_PLAYLIST)?;
        HlsStream::from_playlist(&url, &decode_text(&body), reach)
    }

    pub(crate) fn from_playlist(url: &str, text: &str, reach: Reach) -> Result<HlsStream, String> {
        if !text.trim_start().starts_with("#EXTM3U") {
            return Err("not an HLS playlist".into());
        }
        let (url, text) = if is_master(text) {
            let variant = pick_variant(text, url).ok_or("HLS: the master playlist is empty")?;
            let (u, body) = fetch(&variant, reach, MAX_PLAYLIST)?;
            (u, decode_text(&body))
        } else {
            (url.to_string(), text.to_string())
        };
        let pl = parse_media(&text, &url)?;
        let mut s = HlsStream {
            url,
            reach,
            target: Duration::from_secs_f64(pl.target),
            ended: pl.ended,
            tracker: Tracker::default(),
            last_load: Instant::now(),
            unchanged: 0,
            failures: 0,
            probes: 0,
            demux: TsDemuxer::new(),
            codec: "",
            titles: Titles::default(),
            out: Vec::new(),
            pos: 0,
        };
        if s.tracker.merge(&pl) == 0 {
            return Err("HLS: the playlist has no segments".into());
        }
        s.fill().map_err(|e| e.to_string())?;
        if s.codec.is_empty() {
            return Err("HLS: no AAC or MPEG audio in the stream".into());
        }
        Ok(s)
    }

    /// "aac" or "mp3".
    pub fn codec(&self) -> &'static str {
        self.codec
    }

    /// The current `#EXTINF` title.
    pub fn titles(&self) -> Titles {
        self.titles.clone()
    }

    /// The next segment, reloading a live playlist when all are taken.
    fn next_segment(&mut self) -> Result<Option<Segment>, String> {
        loop {
            if let Some(s) = self.tracker.queue.pop_front() {
                return Ok(Some(s));
            }
            if self.ended {
                return Ok(None);
            }
            let wait = if self.unchanged > 0 {
                self.target / 2
            } else {
                self.target
            };
            if let Some(rest) = wait.checked_sub(self.last_load.elapsed()) {
                std::thread::sleep(rest);
            }
            self.last_load = Instant::now();
            let loaded = fetch(&self.url, self.reach, MAX_PLAYLIST)
                .and_then(|(_, body)| parse_media(&decode_text(&body), &self.url));
            let pl = match loaded {
                Ok(pl) => pl,
                Err(e) => {
                    self.failures += 1;
                    if self.failures >= MAX_FAILURES {
                        return Err(e);
                    }
                    self.unchanged += 1;
                    continue;
                }
            };
            self.target = Duration::from_secs_f64(pl.target);
            self.ended = pl.ended;
            if self.tracker.merge(&pl) > 0 {
                self.unchanged = 0;
                self.failures = 0;
            } else {
                self.unchanged += 1;
                if self.unchanged > MAX_UNCHANGED {
                    return Err("HLS: the live playlist stopped updating".into());
                }
            }
        }
    }

    /// Fetches a segment and appends its audio.
    fn load(&mut self, seg: &Segment) -> Result<(), String> {
        let (_, data) = fetch(&seg.url, self.reach, MAX_SEGMENT)?;
        if seg.discontinuity {
            self.demux.flush(&mut self.out);
            self.demux.reset();
        }
        if data.len() >= 8 && matches!(&data[4..8], b"ftyp" | b"styp" | b"moof" | b"sidx") {
            return Err("HLS with fMP4/CMAF segments is not supported".into());
        }
        if data.first() == Some(&0x47) {
            self.demux.push(&data, &mut self.out);
            if self.demux.has_pmt() && self.demux.codec().is_none() {
                return Err("HLS: no AAC or MPEG audio in the stream".into());
            }
            if self.codec.is_empty() {
                self.codec = self.demux.codec().unwrap_or("");
            }
        } else {
            let audio = skip_id3(&data);
            if self.codec.is_empty() {
                self.codec = sniff_codec(audio);
            }
            self.out.extend_from_slice(audio);
        }
        if self.codec.is_empty() {
            // Transport streams without a PMT would be fetched forever.
            self.probes += 1;
            if self.probes >= MAX_FAILURES {
                return Err("HLS: no AAC or MPEG audio in the stream".into());
            }
        }
        if let Some(t) = &seg.title {
            set_title(&self.titles, Some(t.clone()));
        }
        Ok(())
    }

    /// Fetches segments until there is audio to read; false at the end.
    fn fill(&mut self) -> io::Result<bool> {
        while self.pos >= self.out.len() {
            self.out.clear();
            self.pos = 0;
            let Some(seg) = self.next_segment().map_err(io::Error::other)? else {
                self.demux.flush(&mut self.out);
                return Ok(!self.out.is_empty());
            };
            match self.load(&seg).or_else(|_| self.load(&seg)) {
                Ok(()) => self.failures = 0,
                Err(e) => {
                    self.failures += 1;
                    if self.failures >= MAX_FAILURES {
                        return Err(io::Error::other(e));
                    }
                }
            }
        }
        Ok(true)
    }
}

impl Read for HlsStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() || !self.fill()? {
            return Ok(0);
        }
        let n = buf.len().min(self.out.len() - self.pos);
        buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The data after leading ID3v2 tags (packed audio segments start with a
/// timestamp tag).
fn skip_id3(mut d: &[u8]) -> &[u8] {
    while d.len() >= 10 && d.starts_with(b"ID3") {
        let size = d[6..10]
            .iter()
            .fold(0usize, |a, &b| (a << 7) | (b & 0x7f) as usize);
        let footer = if d[5] & 0x10 != 0 { 10 } else { 0 };
        d = d.get(10 + size + footer..).unwrap_or(&[]);
    }
    d
}

#[cfg(test)]
mod tests {
    use super::*;

    const BASE: &str = "http://cdn.example/radio/live/index.m3u8?token=1";

    fn seqs(t: &Tracker) -> Vec<u64> {
        t.queue.iter().map(|s| s.seq).collect()
    }

    fn live(first: u64, n: u64) -> MediaPlaylist {
        let text = format!(
            "#EXTM3U\n#EXT-X-TARGETDURATION:6\n#EXT-X-MEDIA-SEQUENCE:{first}\n{}",
            (first..first + n)
                .map(|i| format!("#EXTINF:6.0,\nseg{i}.ts\n"))
                .collect::<String>()
        );
        parse_media(&text, BASE).unwrap()
    }

    #[test]
    fn media_playlist() {
        let text = "#EXTM3U\r\n\
            #EXT-X-VERSION:3\r\n\
            #EXT-X-TARGETDURATION:10\r\n\
            #EXT-X-MEDIA-SEQUENCE:2680\r\n\
            #EXT-X-KEY:METHOD=NONE\r\n\
            #EXTINF:9.98,Daft Punk - Around the World\r\n\
            seg2680.aac\r\n\
            #EXTINF:10.0,title=\"Song, with comma\",artist=\"Band\",url=\"x\"\r\n\
            /abs/seg2681.ts\r\n\
            #EXT-X-DISCONTINUITY\r\n\
            #EXTINF:10,\r\n\
            ../other/seg2682.ts?x=1\r\n\
            #EXTINF:4.5\r\n\
            https://other.example/seg2683.ts\r\n\
            #EXT-X-ENDLIST\r\n";
        let pl = parse_media(text, BASE).unwrap();
        assert_eq!(pl.target, 10.0);
        assert!(pl.ended);
        let s = &pl.segments;
        assert_eq!(s.len(), 4);
        assert_eq!(
            s.iter().map(|s| s.seq).collect::<Vec<_>>(),
            [2680, 2681, 2682, 2683]
        );
        assert_eq!(s[0].url, "http://cdn.example/radio/live/seg2680.aac");
        assert_eq!(s[0].title.as_deref(), Some("Daft Punk - Around the World"));
        assert_eq!(s[0].duration, 9.98);
        assert_eq!(s[1].url, "http://cdn.example/abs/seg2681.ts");
        assert_eq!(s[1].title.as_deref(), Some("Band - Song, with comma"));
        assert_eq!(s[2].url, "http://cdn.example/radio/other/seg2682.ts?x=1");
        assert!(s[2].discontinuity && !s[1].discontinuity);
        assert_eq!(s[2].title, None);
        assert_eq!(s[3].url, "https://other.example/seg2683.ts");

        // Without a target duration: the longest segment, at least 1 s.
        let pl = parse_media("#EXTM3U\n#EXTINF:0.2,\na.ts\n", BASE).unwrap();
        assert_eq!(pl.target, 1.0);
        assert!(!pl.ended);
    }

    #[test]
    fn unsupported_playlists() {
        let err = |t: &str| parse_media(t, BASE).unwrap_err();
        assert!(err("#EXTM3U\n#EXT-X-MAP:URI=\"init.mp4\"\n#EXTINF:6,\na.m4s\n").contains("fMP4"));
        assert!(err("#EXTM3U\n#EXT-X-KEY:METHOD=AES-128,URI=\"k\"\n").contains("encrypted"));
        assert!(err("#EXTM3U\n#EXT-X-KEY:METHOD=SAMPLE-AES,URI=\"k\"\n").contains("encrypted"));
        assert!(err("#EXTM3U\n#EXT-X-BYTERANGE:100@0\n").contains("byte-range"));
        assert!(err("#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\na.m3u8\n").contains("master"));
        assert!(err("#EXTM3U\n#EXT-X-MEDIA-SEQUENCE:x\n").contains("sequence"));
    }

    #[test]
    fn master_playlist() {
        let text = "#EXTM3U\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"avc1.4d401f,mp4a.40.2\"\n\
            video.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=64000,CODECS=\"mp4a.40.5\"\n\
            low.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=128000,CODECS=\"mp4a.40.2\"\n\
            high/index.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=96000,CODECS=\"ec-3\"\n\
            surround.m3u8\n";
        assert_eq!(
            pick_variant(text, BASE).as_deref(),
            Some("http://cdn.example/radio/live/high/index.m3u8")
        );
        // Video variants with an audio rendition.
        let text = "#EXTM3U\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"en\",URI=\"audio/en.m3u8\"\n\
            #EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a\",NAME=\"de\",DEFAULT=YES,URI=\"audio/de.m3u8\"\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2000000,CODECS=\"avc1.4d401f,mp4a.40.2\",AUDIO=\"a\"\n\
            video.m3u8\n";
        assert_eq!(
            pick_variant(text, BASE).as_deref(),
            Some("http://cdn.example/radio/live/audio/de.m3u8")
        );
        // Nothing known about the codecs: the first variant.
        let text = "#EXTM3U\n#EXT-X-STREAM-INF:BANDWIDTH=1\nfirst.m3u8\n\
            #EXT-X-STREAM-INF:BANDWIDTH=2\nsecond.m3u8\n";
        assert_eq!(
            pick_variant(text, BASE).as_deref(),
            Some("http://cdn.example/radio/live/first.m3u8")
        );
        assert_eq!(pick_variant("#EXTM3U\n", BASE), None);
    }

    #[test]
    fn live_refresh() {
        let mut t = Tracker::default();
        // Starts three segments from the live edge.
        assert_eq!(t.merge(&live(100, 6)), 3);
        assert_eq!(seqs(&t), [103, 104, 105]);
        t.queue.clear();
        // Unchanged playlist: nothing new.
        assert_eq!(t.merge(&live(100, 6)), 0);
        // The window moves on by two.
        assert_eq!(t.merge(&live(102, 6)), 2);
        assert_eq!(seqs(&t), [106, 107]);
        assert!(!t.queue[0].discontinuity);
        t.queue.clear();
        // A stale (older) copy from a cache adds nothing.
        assert_eq!(t.merge(&live(100, 6)), 0);
        // Fell behind: continue with the oldest segment still listed.
        assert_eq!(t.merge(&live(120, 6)), 6);
        assert_eq!(seqs(&t), [120, 121, 122, 123, 124, 125]);
        assert!(t.queue[0].discontinuity && !t.queue[1].discontinuity);
        t.queue.clear();
        // The stream restarted its numbering: from its live edge again.
        assert_eq!(t.merge(&live(0, 5)), 3);
        assert_eq!(seqs(&t), [2, 3, 4]);
        assert!(t.queue[0].discontinuity);
        // An empty playlist changes nothing.
        assert_eq!(t.merge(&MediaPlaylist::default()), 0);

        // A finished playlist plays from its start.
        let mut t = Tracker::default();
        let mut pl = live(7, 5);
        pl.ended = true;
        assert_eq!(t.merge(&pl), 5);
        assert_eq!(seqs(&t), [7, 8, 9, 10, 11]);
    }

    #[test]
    fn helpers() {
        let mut tagged = b"ID3\x04\x00\x00\x00\x00\x00\x05abcde".to_vec();
        tagged.extend_from_slice(b"ID3\x04\x00\x10\x00\x00\x00\x00xxxxxxxxxx\xff\xf1");
        assert_eq!(skip_id3(&tagged), b"\xff\xf1");
        assert_eq!(skip_id3(b"ID3\x04\x00\x00\x7f\x7f\x7f\x7f"), b"");
        assert_eq!(skip_id3(b"\xff\xfb"), b"\xff\xfb");
        assert_eq!(
            attrs("A=1,B=\"x,y\", C=z,D=\"open"),
            [
                ("A".to_string(), "1".to_string()),
                ("B".into(), "x,y".into()),
                ("C".into(), "z".into()),
                ("D".into(), "open".into())
            ]
        );
        assert_eq!(extinf("-1,"), (0.0, None));
        assert_eq!(extinf("5,A\u{7}B"), (5.0, Some("A B".into())));
    }
}
