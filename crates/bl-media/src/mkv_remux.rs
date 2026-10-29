//! Opus from WebM (Matroska) into Ogg Opus without decoding: time ranges
//! can be left out (whole packets), tags and a cover are written. The input
//! is read strictly forward, so it may still be downloading; pages are
//! written as they fill, so the output can be played while it grows.

use super::*;
use std::io::Write;

/// Tags of the written file.
#[derive(Clone, Debug, Default)]
pub struct RemuxTags {
    pub title: String,
    pub artist: String,
    pub album: String,
    /// Year or date ("2021", "2021-05-03").
    pub date: String,
    /// More Vorbis comments (key, value).
    pub comments: Vec<(String, String)>,
    /// Front cover (MIME type, image data).
    pub cover: Option<(String, Vec<u8>)>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RemuxResult {
    /// Length of the written audio.
    pub duration_ms: u64,
    /// Audio left out.
    pub removed_ms: u64,
}

/// Retained window of a forward-only reader: the demuxer may seek back a
/// little (its buffered reader) and skips forward by reading.
struct Forward {
    inner: Box<dyn Read + Send>,
    buf: Vec<u8>,
    base: u64,
    pos: u64,
    eof: bool,
}

const KEEP: usize = 1 << 20;

impl Forward {
    fn fill_to(&mut self, end: u64) -> io::Result<()> {
        let mut chunk = [0u8; 64 * 1024];
        while !self.eof && self.base + (self.buf.len() as u64) < end {
            let n = match self.inner.read(&mut chunk) {
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            if n == 0 {
                self.eof = true;
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
        Ok(())
    }

    fn trim(&mut self) {
        let consumed = (self.pos.saturating_sub(self.base)) as usize;
        if consumed > 2 * KEEP {
            let drop = (consumed - KEEP).min(self.buf.len());
            self.buf.drain(..drop);
            self.base += drop as u64;
        }
    }
}

impl Read for Forward {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if out.is_empty() {
            return Ok(0);
        }
        self.fill_to(self.pos + 1)?;
        let end = self.base + self.buf.len() as u64;
        if self.pos >= end {
            return Ok(0);
        }
        let i = (self.pos - self.base) as usize;
        let n = out.len().min(self.buf.len() - i);
        out[..n].copy_from_slice(&self.buf[i..i + n]);
        self.pos += n as u64;
        self.trim();
        Ok(n)
    }
}

impl Seek for Forward {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let t = match to {
            SeekFrom::Start(n) => n as i128,
            SeekFrom::Current(k) => self.pos as i128 + k as i128,
            SeekFrom::End(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    "stream length unknown",
                ))
            }
        };
        if t < self.base as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before retained data",
            ));
        }
        let t = t.min(u64::MAX as i128) as u64;
        // Skipping forward reads (and drops) what lies between.
        while t > self.base + self.buf.len() as u64 && !self.eof {
            let want = (self.base + self.buf.len() as u64 + (KEEP as u64)).min(t);
            self.fill_to(want)?;
            self.pos = (self.base + self.buf.len() as u64).min(t);
            self.trim();
        }
        self.pos = t;
        Ok(t)
    }
}

fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len().div_ceil(3) * 4);
    for ch in data.chunks(3) {
        let b = [ch[0], *ch.get(1).unwrap_or(&0), *ch.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        for i in 0..4 {
            if i <= ch.len() {
                s.push(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// The OpusTags packet.
fn opus_tags(tags: &RemuxTags) -> Vec<u8> {
    let mut entries: Vec<String> = Vec::new();
    for (k, v) in [
        ("TITLE", &tags.title),
        ("ARTIST", &tags.artist),
        ("ALBUM", &tags.album),
        ("DATE", &tags.date),
    ] {
        if !v.is_empty() {
            entries.push(format!("{k}={v}"));
        }
    }
    for (k, v) in &tags.comments {
        let k: String = k
            .chars()
            .filter(|c| (' '..='}').contains(c) && *c != '=')
            .collect();
        if !k.is_empty() && !v.is_empty() {
            entries.push(format!("{k}={v}"));
        }
    }
    if let Some((mime, data)) = &tags.cover {
        let mut pic = Vec::with_capacity(data.len() + 64);
        pic.extend_from_slice(&3u32.to_be_bytes()); // front cover
        pic.extend_from_slice(&(mime.len() as u32).to_be_bytes());
        pic.extend_from_slice(mime.as_bytes());
        pic.extend_from_slice(&0u32.to_be_bytes()); // description
        pic.extend_from_slice(&[0u8; 16]); // size, depth, colours: unknown
        pic.extend_from_slice(&(data.len() as u32).to_be_bytes());
        pic.extend_from_slice(data);
        entries.push(format!("METADATA_BLOCK_PICTURE={}", base64(&pic)));
    }
    let vendor = b"Beatlumen";
    let mut p = Vec::new();
    p.extend_from_slice(b"OpusTags");
    p.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    p.extend_from_slice(vendor);
    p.extend_from_slice(&(entries.len() as u32).to_le_bytes());
    for e in &entries {
        p.extend_from_slice(&(e.len() as u32).to_le_bytes());
        p.extend_from_slice(e.as_bytes());
    }
    p
}

/// Remuxes the Opus track of a WebM stream into Ogg Opus, leaving out the
/// packets that start inside one of `cuts` (seconds of the input, sorted
/// or not). Returns `Error::Unsupported` for other codecs.
pub fn webm_opus_to_ogg(
    input: Box<dyn Read + Send>,
    out: &mut dyn Write,
    cuts: &[(f64, f64)],
    tags: &RemuxTags,
) -> Result<RemuxResult, Error> {
    let fwd = Forward {
        inner: input,
        buf: Vec::new(),
        base: 0,
        pos: 0,
        eof: false,
    };
    let mut r = BufReader::with_capacity(64 * 1024, Box::new(fwd) as Box<dyn Source>);

    // EBML header, then the segment's elements up to the first cluster.
    let seg_end = loop {
        let Some(h) = read_header(&mut r)? else {
            return invalid("not a WebM file");
        };
        match h.id {
            ID_EBML => {
                let size = h.size.ok_or(Error::InvalidData("bad EBML header".into()))?;
                r.seek(SeekFrom::Start(h.data + size))?;
            }
            ID_SEGMENT => break h.size.map_or(u64::MAX, |s| h.data.saturating_add(s)),
            _ => return invalid("not a WebM file"),
        }
    };
    let mut timecode_scale = 1_000_000u64;
    let mut tracks: Vec<Track> = Vec::new();
    loop {
        let Some(h) = read_header(&mut r)? else {
            return invalid("no audio in the WebM file");
        };
        match (h.id, h.size) {
            (ID_CLUSTER, _) => {
                r.seek(SeekFrom::Start(h.pos))?;
                break;
            }
            (ID_INFO, Some(size)) => {
                let d = read_vec(&mut r, size, MAX_ELEMENT)?;
                for (id, v) in children(&d) {
                    if id == ID_TIMECODESCALE {
                        timecode_scale = uint(v).max(1);
                    }
                }
            }
            (ID_TRACKS, Some(size)) => {
                let d = read_vec(&mut r, size, MAX_ELEMENT)?;
                tracks = children(&d)
                    .filter(|(id, _)| *id == ID_TRACKENTRY)
                    .map(|(_, b)| parse_track(b))
                    .collect();
            }
            (_, Some(size)) => {
                r.seek(SeekFrom::Start(h.data.saturating_add(size)))?;
            }
            (_, None) => return invalid("unknown-sized element before the clusters"),
        }
    }
    let Some(t) = tracks.iter().find(|t| t.codec_id == "A_OPUS") else {
        return unsupported("the WebM file has no Opus track");
    };
    if t.unsupported_encoding {
        return unsupported("compressed or encrypted Matroska track");
    }
    let head = bl_opus::OpusHead::parse(&t.private)?;
    let pre_skip = if t.codec_delay_ns > 0 {
        ((t.codec_delay_ns as u128 * 48_000 + 500_000_000) / 1_000_000_000) as u64
    } else {
        head.pre_skip as u64
    };
    let mut head_packet = t.private.clone();
    head_packet[10..12].copy_from_slice(&(pre_skip.min(u16::MAX as u64) as u16).to_le_bytes());

    let mut pages = Vec::new();
    let mut w = bl_ogg::PageWriter::new(0x426c_4f70); // "BlOp"
    w.packet(&mut pages, &head_packet, 0);
    w.flush(&mut pages, false);
    w.packet(&mut pages, &opus_tags(tags), 0);
    w.flush(&mut pages, false);
    out.write_all(&pages)?;
    pages.clear();

    let mut dmx = Demuxer {
        r,
        seg_end,
        track: t.number,
        strip: t.strip.clone(),
        timecode_scale,
        cluster_tc: 0,
        queue: VecDeque::new(),
    };
    let in_cut = |s: f64| cuts.iter().any(|&(a, b)| s >= a && s < b);
    let mut in_pos = 0u64; // samples of the input so far (incl. pre-skip)
    let mut out_pos = 0u64; // granule of the output so far
    let mut removed = 0u64;
    let mut held: Option<(Vec<u8>, u64, i64)> = None; // packet, samples, discard ns
    let mut since_flush = 0u64;
    loop {
        let frame = dmx.next_frame()?;
        // Packets inside a cut are dropped; the next kept packet (or the
        // end) decides about the one held back, which is written once it
        // is known whether it is the last (whose end may be trimmed).
        let next = match frame {
            Some(f) => {
                let samples = bl_opus::packet_samples_48k(&f.data).unwrap_or(960) as u64;
                let start = in_pos.saturating_sub(pre_skip) as f64 / 48_000.0;
                in_pos += samples;
                if in_cut(start + samples as f64 / 96_000.0) {
                    removed += samples;
                    continue;
                }
                Some((f.data, samples, f.discard_ns))
            }
            None => None,
        };
        let last = next.is_none();
        let mut granule = out_pos;
        if let Some((data, samples, discard_ns)) = held.take() {
            out_pos += samples;
            granule = out_pos;
            if last && discard_ns > 0 {
                let trim = (discard_ns as u128 * 48_000 / 1_000_000_000) as u64;
                granule = granule.saturating_sub(trim.min(samples)).max(pre_skip);
            }
            w.packet(&mut pages, &data, granule as i64);
            since_flush += samples;
        }
        if last {
            w.flush(&mut pages, true);
        } else if since_flush >= 48_000 {
            // About a second per page: playable while it grows.
            w.flush(&mut pages, false);
            since_flush = 0;
        }
        if !pages.is_empty() {
            out.write_all(&pages)?;
            out.flush()?;
            pages.clear();
        }
        if last {
            return Ok(RemuxResult {
                duration_ms: granule.saturating_sub(pre_skip) * 1000 / 48_000,
                removed_ms: removed * 1000 / 48_000,
            });
        }
        held = next;
    }
}
