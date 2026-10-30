//! Ogg Opus stream writer (RFC 7845): identification and comment headers,
//! audio pages with granule positions, end trimming on the last page.

use bl_ogg::PageWriter;
use std::io::{self, Write};

/// Vendor string written to `OpusTags`.
const VENDOR: &str = "Beatlumen bl-opus-enc";
/// A page is ended once it holds this many bytes ...
const PAGE_BYTES: usize = 4000;
/// ... or this many samples (48 kHz), whichever comes first.
const PAGE_SAMPLES: u64 = 24000;

/// Parameters of the `OpusHead` packet (channel mapping family 0).
#[derive(Debug, Clone, Copy)]
pub struct OpusHeader {
    /// 1 or 2.
    pub channels: u8,
    /// Samples (at 48 kHz) to discard at the start of the decoded stream.
    pub pre_skip: u16,
    /// Sample rate of the original input (informational).
    pub input_sample_rate: u32,
    /// Output gain in Q7.8 dB.
    pub output_gain: i16,
}

impl OpusHeader {
    fn packet(&self) -> Vec<u8> {
        let mut h = Vec::with_capacity(19);
        h.extend_from_slice(b"OpusHead");
        h.push(1);
        h.push(self.channels);
        h.extend_from_slice(&self.pre_skip.to_le_bytes());
        h.extend_from_slice(&self.input_sample_rate.to_le_bytes());
        h.extend_from_slice(&self.output_gain.to_le_bytes());
        h.push(0);
        h
    }
}

fn tags_packet(tags: &[(&str, &str)]) -> Vec<u8> {
    let mut t = Vec::new();
    t.extend_from_slice(b"OpusTags");
    t.extend_from_slice(&(VENDOR.len() as u32).to_le_bytes());
    t.extend_from_slice(VENDOR.as_bytes());
    t.extend_from_slice(&(tags.len() as u32).to_le_bytes());
    for (k, v) in tags {
        // Field names are ASCII without '='; keep them valid.
        let key: String = k
            .chars()
            .filter(|c| (' '..='}').contains(c) && *c != '=')
            .collect();
        let len = key.len() + 1 + v.len();
        t.extend_from_slice(&(len as u32).to_le_bytes());
        t.extend_from_slice(key.as_bytes());
        t.push(b'=');
        t.extend_from_slice(v.as_bytes());
    }
    t
}

/// A serial number that differs between streams.
fn new_serial() -> u32 {
    use std::sync::atomic::{AtomicU32, Ordering};
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let x =
        t ^ (COUNTER.fetch_add(1, Ordering::Relaxed) as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    let x = (x ^ (x >> 33)).wrapping_mul(0xff51_afd7_ed55_8ccd);
    (x ^ (x >> 29)) as u32
}

/// Writes Opus packets as an Ogg Opus stream.
///
/// The last packet is held back until the next one (or [`finish`]) so that
/// the final page can carry the end-trimmed granule position and the
/// end-of-stream flag.
///
/// [`finish`]: OggOpusWriter::finish
pub struct OggOpusWriter<W: Write> {
    inner: W,
    pages: PageWriter,
    buf: Vec<u8>,
    pre_skip: u16,
    /// Granule position after all packets passed so far (incl. held).
    granule: u64,
    /// Held packet with its end granule and duration.
    held: Option<(Vec<u8>, u64, u64)>,
    /// The tags page is still open (no audio written yet).
    headers_open: bool,
    page_start_granule: u64,
}

impl<W: Write> OggOpusWriter<W> {
    /// Starts a stream for `channels` (1 or 2) with the given pre-skip
    /// (typically [`crate::OpusEncoder::LOOKAHEAD`]) and no tags.
    pub fn new(w: W, channels: u8, pre_skip: u16) -> io::Result<Self> {
        let head = OpusHeader {
            channels,
            pre_skip,
            input_sample_rate: 48000,
            output_gain: 0,
        };
        Self::with_header(w, head, &[], None)
    }

    /// Starts a stream with explicit header fields, `OpusTags` comments
    /// (`(field, value)`, e.g. `("TITLE", "...")`) and an optional serial
    /// number (random if `None`).
    pub fn with_header(
        w: W,
        head: OpusHeader,
        tags: &[(&str, &str)],
        serial: Option<u32>,
    ) -> io::Result<Self> {
        if !(1..=2).contains(&head.channels) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Ogg Opus writer: 1 or 2 channels",
            ));
        }
        let mut s = OggOpusWriter {
            inner: w,
            pages: PageWriter::new(serial.unwrap_or_else(new_serial)),
            buf: Vec::new(),
            pre_skip: head.pre_skip,
            granule: 0,
            held: None,
            headers_open: true,
            page_start_granule: 0,
        };
        // The identification header has a page of its own, the comment
        // header ends its page(s) (flushed with the first audio packet).
        s.pages.packet(&mut s.buf, &head.packet(), 0);
        s.pages.flush(&mut s.buf, false);
        s.pages.packet(&mut s.buf, &tags_packet(tags), 0);
        s.write_out()?;
        Ok(s)
    }

    fn write_out(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            self.inner.write_all(&self.buf)?;
            self.buf.clear();
        }
        Ok(())
    }

    fn push(&mut self, packet: &[u8], granule: u64) {
        if self.headers_open {
            self.pages.flush(&mut self.buf, false);
            self.headers_open = false;
            self.page_start_granule = 0;
        }
        self.pages.packet(&mut self.buf, packet, granule as i64);
        if self.pages.pending() >= PAGE_BYTES || granule - self.page_start_granule >= PAGE_SAMPLES {
            self.pages.flush(&mut self.buf, false);
            self.page_start_granule = granule;
        }
    }

    /// Adds an Opus packet decoding to `samples` samples (at 48 kHz, e.g.
    /// 960 for a 20 ms packet). Completed pages are written to the output.
    pub fn write_packet(&mut self, packet: &[u8], samples: u32) -> io::Result<()> {
        if let Some((p, g, _)) = self.held.take() {
            self.push(&p, g);
        }
        self.granule += samples as u64;
        self.held = Some((packet.to_vec(), self.granule, samples as u64));
        self.write_out()
    }

    /// Ends the current page early (to bound latency when streaming).
    pub fn flush_page(&mut self) -> io::Result<()> {
        if let Some((p, g, _)) = self.held.take() {
            self.push(&p, g);
        }
        if !self.headers_open {
            self.pages.flush(&mut self.buf, false);
            self.page_start_granule = self.granule;
        }
        self.write_out()?;
        self.inner.flush()
    }

    /// Writes the last page and returns the inner writer. `total_samples`
    /// is the length of the original audio in samples at 48 kHz (without
    /// pre-skip); the decoded output is trimmed to it (at most the samples
    /// of the last packet can be trimmed). `None` keeps all decoded samples.
    pub fn finish(mut self, total_samples: Option<u64>) -> io::Result<W> {
        match self.held.take() {
            Some((p, g, n)) => {
                let end = match total_samples {
                    Some(t) => g.min(self.pre_skip as u64 + t).max(g - n),
                    None => g,
                };
                self.push(&p, end);
                self.pages.flush(&mut self.buf, true);
            }
            None => {
                // No audio: the comment header page ends the stream.
                self.pages.flush(&mut self.buf, true);
            }
        }
        self.write_out()?;
        self.inner.flush()?;
        Ok(self.inner)
    }

    /// Granule position after the packets written so far.
    pub fn granule(&self) -> u64 {
        self.granule
    }

    pub fn get_ref(&self) -> &W {
        &self.inner
    }

    pub fn get_mut(&mut self) -> &mut W {
        &mut self.inner
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_ogg::PacketReader;
    use std::io::Cursor;

    fn read_all(data: &[u8]) -> Vec<bl_ogg::Packet> {
        let mut r = PacketReader::new(Cursor::new(data));
        let mut v = Vec::new();
        while let Some(p) = r.next_packet().unwrap() {
            v.push(p);
        }
        v
    }

    #[test]
    fn headers_granules_and_trimming() {
        let mut w = OggOpusWriter::with_header(
            Vec::new(),
            OpusHeader {
                channels: 2,
                pre_skip: 120,
                input_sample_rate: 44100,
                output_gain: 0,
            },
            &[("TITLE", "Test"), ("ARTIST", "Nobody")],
            Some(7),
        )
        .unwrap();
        for i in 0..100u32 {
            w.write_packet(&[0xfc, i as u8, 1, 2, 3], 960).unwrap();
        }
        let data = w.finish(Some(95_000)).unwrap();
        let pk = read_all(&data);
        assert_eq!(pk.len(), 102);
        let head = bl_opus::OpusHead::parse(&pk[0].data).unwrap();
        assert_eq!(
            (head.channels, head.pre_skip, head.input_sample_rate),
            (2, 120, 44100)
        );
        assert!(pk[0].bos && pk[0].granule == Some(0));
        assert!(pk[1].data.starts_with(b"OpusTags"));
        let s = String::from_utf8_lossy(&pk[1].data);
        assert!(s.contains("TITLE=Test") && s.contains("ARTIST=Nobody"));
        assert_eq!(pk[1].granule, Some(0));
        let last = pk.last().unwrap();
        assert!(last.eos);
        assert_eq!(last.granule, Some(120 + 95_000));
        // Intermediate granules count all decoded samples.
        let mut prev = 0;
        for (i, p) in pk[2..].iter().enumerate() {
            assert_eq!(p.data[1], i as u8);
            if let Some(g) = p.granule {
                if !p.eos {
                    assert_eq!(g, (i as u64 + 1) * 960);
                }
                assert!(g >= prev);
                prev = g;
            }
        }
        // Pages are bounded (about half a second each).
        let pages = data.windows(4).filter(|w| w == b"OggS").count();
        assert!((5..=12).contains(&pages), "{pages} pages");
    }

    #[test]
    fn empty_stream_has_eos() {
        let w = OggOpusWriter::new(Vec::new(), 1, 120).unwrap();
        let data = w.finish(Some(0)).unwrap();
        let pk = read_all(&data);
        assert_eq!(pk.len(), 2);
        assert!(pk[1].eos);
        assert!(OggOpusWriter::new(Vec::new(), 3, 120).is_err());
    }
}
