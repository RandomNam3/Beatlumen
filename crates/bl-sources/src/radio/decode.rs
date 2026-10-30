//! Decoding live radio streams.
//!
//! [`bl_media::open`] is made for files: its MP3, ADTS and FLAC readers
//! need the length of the stream (ADTS even scans all frames when it
//! opens), which a live stream does not have. So:
//!
//! * MP3 (and MP2): [`bl_mp3::Mp3Decoder`], which takes a failing
//!   `SeekFrom::End` as a stream of unknown length, over a [`LiveSource`].
//! * AAC: ADTS frames are read one after the other and decoded with
//!   [`bl_aac::AacDecoder`], resynchronising after garbage.
//! * Ogg (Vorbis, Opus, FLAC in Ogg): bl-media's Ogg decoder over a
//!   [`LiveSource`] (it also accepts an unknown length). Icecast sends a
//!   new chained Ogg stream for each song; each chain gets a new decoder
//!   and its Vorbis/Opus comments give the title.
//! * Native FLAC streams are not supported.
//!
//! Damaged frames are skipped (the player ends playback on an error), up
//! to 50 in a row. Live streams cannot seek.

use super::{set_title, sniff_codec, RadioStream, Titles};
use bl_aac::{AacDecoder, AdtsHeader};
use bl_media::{AudioSpec, Decoder, Error};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::sync::{Arc, Mutex};

/// Damaged frames in a row before decoding fails.
const MAX_ERRORS: u32 = 50;
/// Garbage skipped while looking for an ADTS frame.
const MAX_JUNK: usize = 1 << 20;

/// A live stream as a [`bl_media::Source`] for decoders that probe the
/// format first: the first [`LiveSource::KEEP`] bytes stay in memory, so
/// the decoder can seek back within them until it has read further;
/// seeking forward skips data (at most 64 MiB); other seeks fail. The
/// length is unknown: `SeekFrom::End` fails with
/// [`io::ErrorKind::Unsupported`], which bl-mp3 and bl-media's Ogg reader
/// take as a stream of unknown length.
pub struct LiveSource {
    inner: Box<dyn Read + Send>,
    /// The start of the stream while `whole`.
    head: Vec<u8>,
    /// `head` holds every byte read so far.
    whole: bool,
    pos: u64,
    /// Bytes read from `inner`.
    read: u64,
}

impl LiveSource {
    /// Bytes kept for probing.
    pub const KEEP: usize = 1 << 20;
    const MAX_SKIP: u64 = 64 << 20;

    pub fn new(inner: Box<dyn Read + Send>) -> LiveSource {
        LiveSource {
            inner,
            head: Vec::new(),
            whole: true,
            pos: 0,
            read: 0,
        }
    }

    fn keep(&mut self, data: &[u8]) {
        if self.whole {
            if self.head.len() + data.len() <= Self::KEEP {
                self.head.extend_from_slice(data);
            } else {
                self.whole = false;
                self.head = Vec::new();
            }
        }
        self.read += data.len() as u64;
    }
}

impl Read for LiveSource {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.pos < self.read {
            // Within the kept start (seeking back is only possible then).
            let p = self.pos as usize;
            let n = buf.len().min(self.head.len().saturating_sub(p));
            buf[..n].copy_from_slice(&self.head[p..p + n]);
            self.pos += n as u64;
            return Ok(n);
        }
        let n = self.inner.read(buf)?;
        self.keep(&buf[..n]);
        self.pos = self.read;
        Ok(n)
    }
}

impl Seek for LiveSource {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => Some(p),
            SeekFrom::Current(d) => self.pos.checked_add_signed(d),
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "live stream: the length is unknown",
                ))
            }
        }
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before the start"))?;
        if target <= self.read {
            if target < self.read && !self.whole {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "live stream: cannot seek back",
                ));
            }
            self.pos = target;
            return Ok(target);
        }
        if target - self.read > Self::MAX_SKIP {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "live stream: seek too far ahead",
            ));
        }
        let mut tmp = [0u8; 16 * 1024];
        while self.read < target {
            let want = (target - self.read).min(tmp.len() as u64) as usize;
            let n = match self.inner.read(&mut tmp[..want]) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            self.keep(&tmp[..n]);
        }
        self.pos = self.read;
        Ok(self.pos)
    }
}

/// A decoder for an open radio stream (see the module documentation).
pub fn open_decoder(stream: RadioStream) -> Result<Box<dyn Decoder>, String> {
    let RadioStream {
        mut reader,
        mut codec,
        titles,
        ..
    } = stream;
    if codec.is_empty() {
        let mut head = Vec::new();
        (&mut reader)
            .take(8192)
            .read_to_end(&mut head)
            .map_err(|e| format!("receive: {e}"))?;
        codec = sniff_codec(&head).to_string();
        reader = Box::new(Cursor::new(head).chain(reader));
    }
    match codec.as_str() {
        "mp3" => Mp3Live::open(LiveSource::new(reader)),
        "aac" => AdtsLive::open(reader),
        "ogg" | "opus" => OggLive::open(reader, titles),
        "flac" => return Err("native FLAC radio streams are not supported".into()),
        "" => return Err("unknown radio stream format".into()),
        other => return Err(format!("unsupported radio stream format: {other}")),
    }
    .map_err(|e| e.to_string())
}

/// Calls `f` again after damaged frames (up to [`MAX_ERRORS`] in a row);
/// I/O errors end decoding.
fn skip_damaged(
    errors: &mut u32,
    mut f: impl FnMut() -> Result<bool, Error>,
) -> Result<bool, Error> {
    loop {
        match f() {
            Ok(more) => {
                *errors = 0;
                return Ok(more);
            }
            Err(e @ Error::Io(_)) => return Err(e),
            Err(e) => {
                *errors += 1;
                if *errors > MAX_ERRORS {
                    return Err(e);
                }
            }
        }
    }
}

fn unseekable() -> Result<u64, Error> {
    Err(Error::Unsupported("a live stream cannot seek".into()))
}

struct Mp3Live {
    dec: bl_mp3::Mp3Decoder<LiveSource>,
    spec: AudioSpec,
    codec: &'static str,
    errors: u32,
}

impl Mp3Live {
    fn open(src: LiveSource) -> Result<Box<dyn Decoder>, Error> {
        let dec = bl_mp3::Mp3Decoder::new(src)?;
        let spec = AudioSpec {
            sample_rate: dec.sample_rate(),
            channels: dec.channels(),
        };
        if spec.sample_rate == 0 || spec.channels == 0 {
            return Err(Error::InvalidData("no MPEG audio format".into()));
        }
        let codec = match dec.layer() {
            1 => "mp1",
            2 => "mp2",
            _ => "mp3",
        };
        Ok(Box::new(Mp3Live {
            dec,
            spec,
            codec,
            errors: 0,
        }))
    }
}

impl Decoder for Mp3Live {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        None
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let dec = &mut self.dec;
        skip_damaged(&mut self.errors, || {
            dec.next_chunk(out).map_err(Error::from)
        })
    }
    fn seek(&mut self, _frame: u64) -> Result<u64, Error> {
        unseekable()
    }
    fn codec_name(&self) -> &'static str {
        self.codec
    }
}

/// ADTS frames read one after the other from a stream.
struct AdtsFrames {
    r: Box<dyn Read + Send>,
    buf: Vec<u8>,
    pos: usize,
    eof: bool,
}

/// What must stay the same from frame to frame: object type, sample rate
/// index, channel configuration.
type AdtsKey = (u8, u8, u8);

fn adts_key(h: &AdtsHeader) -> AdtsKey {
    (h.object_type, h.sample_rate_index, h.channel_config)
}

/// A plausible ADTS header (layer 0, some payload).
fn adts_header(d: &[u8]) -> Option<AdtsHeader> {
    AdtsHeader::parse(d).filter(|h| d[1] & 0x06 == 0 && h.frame_length > h.header_length)
}

impl AdtsFrames {
    /// Makes `n` bytes available at `pos`; false if the stream ends first.
    fn fill(&mut self, n: usize) -> io::Result<bool> {
        if self.buf.len() - self.pos >= n {
            return Ok(true);
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
        while self.buf.len() < n && !self.eof {
            let old = self.buf.len();
            self.buf.resize(old + (n - old).max(16 * 1024), 0);
            let got = loop {
                match self.r.read(&mut self.buf[old..]) {
                    Ok(k) => break k,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        self.buf.truncate(old);
                        return Err(e);
                    }
                }
            };
            self.buf.truncate(old + got);
            self.eof = got == 0;
        }
        Ok(self.buf.len() >= n)
    }

    /// The next frame (as a range of `buf`) matching `key` (any if None);
    /// after garbage, or for the first frame, the following header must be
    /// valid too. None at the end of the stream.
    fn next(&mut self, key: Option<AdtsKey>) -> Result<Option<(usize, usize)>, Error> {
        let mut skipped = 0;
        loop {
            if !self.fill(9)? {
                return Ok(None);
            }
            let h = adts_header(&self.buf[self.pos..])
                .filter(|h| key.map_or(true, |k| k == adts_key(h)));
            if let Some(h) = h {
                let len = h.frame_length;
                let confirmed = (skipped == 0 && key.is_some())
                    || !self.fill(len + 9)?
                    || adts_header(&self.buf[self.pos + len..])
                        .is_some_and(|n| adts_key(&n) == adts_key(&h));
                if confirmed {
                    if !self.fill(len)? {
                        return Ok(None); // truncated at the end
                    }
                    let start = self.pos;
                    self.pos += len;
                    return Ok(Some((start, len)));
                }
            }
            self.pos += 1;
            skipped += 1;
            if skipped > MAX_JUNK {
                return Err(Error::InvalidData("no ADTS frames in the stream".into()));
            }
        }
    }
}

struct AdtsLive {
    frames: AdtsFrames,
    key: AdtsKey,
    dec: AacDecoder,
    spec: AudioSpec,
    /// Samples of the first frame (decoded to learn the format).
    pending: Vec<f32>,
    errors: u32,
}

impl AdtsLive {
    fn open(r: Box<dyn Read + Send>) -> Result<Box<dyn Decoder>, Error> {
        let mut frames = AdtsFrames {
            r,
            buf: Vec::new(),
            pos: 0,
            eof: false,
        };
        // The first frame that decodes tells the output format (implicit
        // SBR/PS show only then).
        for _ in 0..MAX_ERRORS {
            let (start, len) = frames
                .next(None)?
                .ok_or_else(|| Error::InvalidData("no ADTS frames in the stream".into()))?;
            let frame = &frames.buf[start..start + len];
            let Some(h) = adts_header(frame) else {
                continue;
            };
            let mut dec = AacDecoder::new(&h.audio_specific_config())?;
            let mut pending = Vec::new();
            match dec.decode_adts_frame(frame, &mut pending) {
                Ok(_) if dec.sample_rate() > 0 && dec.channels() > 0 => {
                    let spec = AudioSpec {
                        sample_rate: dec.sample_rate(),
                        channels: dec.channels(),
                    };
                    return Ok(Box::new(AdtsLive {
                        frames,
                        key: adts_key(&h),
                        dec,
                        spec,
                        pending,
                        errors: 0,
                    }));
                }
                Err(e @ bl_aac::Error::Unsupported(_)) => return Err(e.into()),
                _ => {}
            }
        }
        Err(Error::InvalidData("ADTS frames do not decode".into()))
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            let Some((start, len)) = self.frames.next(Some(self.key))? else {
                return Ok(false);
            };
            let before = out.len();
            match self
                .dec
                .decode_adts_frame(&self.frames.buf[start..start + len], out)
            {
                Ok(n) if n > 0 => {
                    let ch = self.spec.channels as usize;
                    out.truncate(before + (out.len() - before) / ch * ch);
                    return Ok(true);
                }
                Ok(_) => continue,
                Err(e) => {
                    out.truncate(before);
                    return Err(e.into());
                }
            }
        }
    }
}

impl Decoder for AdtsLive {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        None
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        if !self.pending.is_empty() {
            out.append(&mut self.pending);
            return Ok(true);
        }
        let mut errors = self.errors;
        let r = skip_damaged(&mut errors, || self.decode_next(out));
        self.errors = errors;
        r
    }
    fn seek(&mut self, _frame: u64) -> Result<u64, Error> {
        unseekable()
    }
    fn codec_name(&self) -> &'static str {
        "aac"
    }
}

/// Ogg pages of a stream, stopping before each new chain (a page that
/// begins a logical stream after pages with audio) until it is released.
struct OggPages {
    inner: Box<dyn Read + Send>,
    page: Vec<u8>,
    pos: usize,
    /// Only beginning-of-stream pages so far in this chain.
    starting: bool,
    /// `page` begins the next chain.
    held: bool,
    titles: Titles,
}

/// Fills `buf`; false on a clean end before the first byte.
fn read_full(r: &mut dyn Read, buf: &mut [u8]) -> io::Result<bool> {
    let mut got = 0;
    while got < buf.len() {
        match r.read(&mut buf[got..]) {
            Ok(0) if got == 0 => return Ok(false),
            Ok(0) => return Err(io::ErrorKind::UnexpectedEof.into()),
            Ok(n) => got += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

impl OggPages {
    /// Reads the next page into `page` (resynchronising on "OggS").
    fn read_page(&mut self) -> io::Result<bool> {
        let mut head = [0u8; 27];
        if !read_full(&mut self.inner, &mut head)? {
            return Ok(false);
        }
        let mut skipped = 0;
        while &head[..4] != b"OggS" {
            head.copy_within(1.., 0);
            if !read_full(&mut self.inner, &mut head[26..])? {
                return Ok(false);
            }
            skipped += 1;
            if skipped > MAX_JUNK {
                return Err(io::Error::new(io::ErrorKind::InvalidData, "lost Ogg sync"));
            }
        }
        let mut table = vec![0u8; head[26] as usize];
        if !read_full(&mut self.inner, &mut table)? {
            return Ok(false);
        }
        let body: usize = table.iter().map(|&l| l as usize).sum();
        self.page.clear();
        self.page.extend_from_slice(&head);
        self.page.extend_from_slice(&table);
        self.page.resize(27 + table.len() + body, 0);
        if !read_full(&mut self.inner, &mut self.page[27 + table.len()..])? {
            return Ok(false);
        }
        self.pos = 0;
        let bos = head[5] & 0x02 != 0;
        if bos && !self.starting {
            self.held = true;
            self.starting = true;
        } else if !bos {
            if self.starting {
                // The comment header follows the identification header.
                if let Some(t) = comment_title(&self.page[27 + table.len()..]) {
                    set_title(&self.titles, Some(t));
                }
            }
            self.starting = false;
        }
        Ok(true)
    }

    /// Lets the held chain be read; false if there is none.
    fn release(&mut self) -> bool {
        std::mem::take(&mut self.held)
    }
}

impl Read for OggPages {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.held {
                return Ok(0);
            }
            if self.pos < self.page.len() {
                break;
            }
            if !self.read_page()? {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.page.len() - self.pos);
        buf[..n].copy_from_slice(&self.page[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// "Artist - Title" from a Vorbis or Opus comment header (as far as it is
/// in this page).
fn comment_title(p: &[u8]) -> Option<String> {
    fn u32le(r: &mut &[u8]) -> Option<usize> {
        let b = r.get(..4)?;
        *r = &r[4..];
        Some(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
    }
    let mut r = p
        .strip_prefix(b"\x03vorbis")
        .or_else(|| p.strip_prefix(b"OpusTags"))?;
    let vendor = u32le(&mut r)?;
    r = r.get(vendor..)?;
    let count = u32le(&mut r)?;
    let (mut title, mut artist) = (None, None);
    for _ in 0..count.min(1000) {
        let Some(len) = u32le(&mut r) else {
            break;
        };
        let Some(c) = r.get(..len) else {
            break;
        };
        r = &r[len..];
        let c = String::from_utf8_lossy(c);
        if let Some((k, v)) = c.split_once('=') {
            let v = v.trim();
            if v.is_empty() {
                continue;
            }
            if k.eq_ignore_ascii_case("TITLE") && title.is_none() {
                title = Some(v.to_string());
            } else if k.eq_ignore_ascii_case("ARTIST") && artist.is_none() {
                artist = Some(v.to_string());
            }
        }
        if title.is_some() && artist.is_some() {
            break;
        }
    }
    match (artist, title) {
        (Some(a), Some(t)) => Some(format!("{a} - {t}")),
        (a, t) => t.or(a),
    }
}

/// Reads the shared pages (each chain's decoder owns one of these).
struct SharedPages(Arc<Mutex<OggPages>>);

impl Read for SharedPages {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).read(buf)
    }
}

struct OggLive {
    pages: Arc<Mutex<OggPages>>,
    dec: Box<dyn Decoder>,
    spec: AudioSpec,
    errors: u32,
}

impl OggLive {
    fn open(r: Box<dyn Read + Send>, titles: Titles) -> Result<Box<dyn Decoder>, Error> {
        let pages = Arc::new(Mutex::new(OggPages {
            inner: r,
            page: Vec::new(),
            pos: 0,
            starting: true,
            held: false,
            titles,
        }));
        let dec = Self::chain(&pages)?;
        Ok(Box::new(OggLive {
            spec: dec.spec(),
            pages,
            dec,
            errors: 0,
        }))
    }

    fn chain(pages: &Arc<Mutex<OggPages>>) -> Result<Box<dyn Decoder>, Error> {
        let src = LiveSource::new(Box::new(SharedPages(pages.clone())));
        bl_media::open(Box::new(src), Some("ogg"))
    }

    fn decode_next(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        loop {
            if self.dec.next_chunk(out)? {
                return Ok(true);
            }
            let next_chain = self
                .pages
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .release();
            if !next_chain {
                return Ok(false);
            }
            let dec = Self::chain(&self.pages)?;
            if dec.spec() != self.spec {
                return Err(Error::Io(io::Error::other(
                    "the stream's audio format changed",
                )));
            }
            self.dec = dec;
        }
    }
}

impl Decoder for OggLive {
    fn spec(&self) -> AudioSpec {
        self.spec
    }
    fn total_frames(&self) -> Option<u64> {
        None
    }
    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        let mut errors = self.errors;
        let r = skip_damaged(&mut errors, || self.decode_next(out));
        self.errors = errors;
        r
    }
    fn seek(&mut self, _frame: u64) -> Result<u64, Error> {
        unseekable()
    }
    fn codec_name(&self) -> &'static str {
        self.dec.codec_name()
    }
}
