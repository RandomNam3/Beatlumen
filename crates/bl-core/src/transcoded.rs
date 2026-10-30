//! Server tracks converted to Opus by the server while they are sent
//! (smaller streams, e.g. over mobile data). Such a stream has no length,
//! so seeking asks the server for a new stream starting at that point.

use bl_media::{AudioSpec, Decoder, Error};
use std::io::{self, Read, Seek, SeekFrom};

/// Bytes of a converted stream kept for reading its start again.
const HEAD: usize = 256 * 1024;

pub(crate) struct Transcoded {
    /// The stream URL without a start position.
    url: String,
    headers: Vec<(String, String)>,
    inner: Box<dyn Decoder>,
    spec: AudioSpec,
    /// The song's length in frames, from the library.
    total: Option<u64>,
    /// The server sent the original file (it cannot convert): seeking goes
    /// to the decoder as usual.
    direct: bool,
}

impl Transcoded {
    /// `url` ends in a query (`…?format=opus&bitrate=…`); `codec` is the
    /// original's, used when the server sends that instead.
    pub(crate) fn open(
        url: String,
        headers: Vec<(String, String)>,
        duration_ms: u64,
        codec: String,
    ) -> Result<Transcoded, String> {
        let stream = bl_net::client::HttpStream::open(&url, &headers)?;
        // A converted stream has no length up front.
        let direct = stream.len() != u64::MAX;
        let hint = if direct { Some(codec.as_str()) } else { None };
        let src: Box<dyn bl_media::Source> = if direct {
            Box::new(io::BufReader::with_capacity(64 * 1024, stream))
        } else {
            Box::new(HeadCached::new(stream))
        };
        let inner =
            bl_media::open(src, hint.filter(|c| !c.is_empty())).map_err(|e| e.to_string())?;
        let spec = inner.spec();
        let total = (duration_ms > 0)
            .then(|| duration_ms * spec.sample_rate as u64 / 1000)
            .filter(|_| !direct);
        Ok(Transcoded {
            url,
            headers,
            inner,
            spec,
            total,
            direct,
        })
    }
}

impl Decoder for Transcoded {
    fn spec(&self) -> AudioSpec {
        self.spec
    }

    fn total_frames(&self) -> Option<u64> {
        if self.direct {
            self.inner.total_frames()
        } else {
            self.total
        }
    }

    fn next_chunk(&mut self, out: &mut Vec<f32>) -> Result<bool, Error> {
        self.inner.next_chunk(out)
    }

    fn seek(&mut self, frame: u64) -> Result<u64, Error> {
        if self.direct {
            return self.inner.seek(frame);
        }
        let rate = self.spec.sample_rate.max(1) as u64;
        let ms = frame * 1000 / rate;
        let url = format!("{}&start={ms}", self.url);
        let stream = bl_net::client::HttpStream::open(&url, &self.headers)
            .map_err(|_| Error::Io(io::Error::other("cannot reach the server")))?;
        self.inner = bl_media::open(Box::new(HeadCached::new(stream)), Some("opus"))?;
        Ok(ms * rate / 1000)
    }

    fn codec_name(&self) -> &'static str {
        self.inner.codec_name()
    }
}

/// Keeps the start of a converted stream: finding out its format reads the
/// start again, and each new request would convert the song once more.
struct HeadCached<R> {
    inner: R,
    head: Vec<u8>,
    /// Read position.
    pos: u64,
    /// Position of `inner`.
    inner_pos: u64,
}

impl<R: Read + Seek> HeadCached<R> {
    fn new(inner: R) -> HeadCached<R> {
        HeadCached {
            inner,
            head: Vec::new(),
            pos: 0,
            inner_pos: 0,
        }
    }
}

impl<R: Read + Seek> Read for HeadCached<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if (self.pos as usize) < self.head.len() {
            let from = &self.head[self.pos as usize..];
            let n = from.len().min(buf.len());
            buf[..n].copy_from_slice(&from[..n]);
            self.pos += n as u64;
            return Ok(n);
        }
        if self.inner_pos != self.pos {
            self.inner_pos = self.inner.seek(SeekFrom::Start(self.pos))?;
        }
        let n = self.inner.read(buf)?;
        if self.inner_pos as usize == self.head.len() && self.head.len() < HEAD {
            let keep = n.min(HEAD - self.head.len());
            self.head.extend_from_slice(&buf[..keep]);
        }
        self.inner_pos += n as u64;
        self.pos += n as u64;
        Ok(n)
    }
}

impl<R: Read + Seek> Seek for HeadCached<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let new = match to {
            SeekFrom::Start(p) => p,
            // The length is not known (the stream reports why).
            SeekFrom::End(d) => {
                self.inner_pos = self.inner.seek(SeekFrom::End(d))?;
                self.inner_pos
            }
            SeekFrom::Current(d) => self
                .pos
                .checked_add_signed(d)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "seek before start"))?,
        };
        self.pos = new;
        Ok(new)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Counts how often the inner stream is repositioned.
    struct Counted(io::Cursor<Vec<u8>>, usize);

    impl Read for Counted {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.0.read(buf)
        }
    }

    impl Seek for Counted {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.1 += 1;
            self.0.seek(to)
        }
    }

    #[test]
    fn start_is_read_again_from_memory() {
        let data: Vec<u8> = (0..400_000u32).map(|i| (i % 251) as u8).collect();
        let mut h = HeadCached::new(Counted(io::Cursor::new(data.clone()), 0));
        let mut a = vec![0u8; 20_000];
        h.read_exact(&mut a).unwrap();
        h.seek(SeekFrom::Start(100)).unwrap();
        let mut b = vec![0u8; 30_000];
        h.read_exact(&mut b).unwrap();
        assert_eq!(&b[..], &data[100..30_100]);
        assert_eq!(h.inner.1, 0, "no new request for the start");
        // Past the kept part: read on from the stream.
        h.seek(SeekFrom::Start(300_000)).unwrap();
        let mut c = vec![0u8; 10];
        h.read_exact(&mut c).unwrap();
        assert_eq!(&c[..], &data[300_000..300_010]);
    }
}
