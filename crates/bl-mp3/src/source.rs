//! Buffered byte source over a `Read + Seek` stream with look-ahead.

use std::io::{self, Read, Seek, SeekFrom};

const READ_CHUNK: usize = 32 * 1024;

pub(crate) struct Source<R> {
    inner: R,
    buf: Vec<u8>,
    /// Cursor into `buf`.
    pos: usize,
    /// Absolute stream offset of `buf[0]`.
    base: u64,
    /// The underlying reader returned EOF after the last byte in `buf`.
    eof: bool,
    /// Absolute offset where audio data ends (trailing tags start).
    end: u64,
}

impl<R: Read + Seek> Source<R> {
    pub fn new(inner: R, start: u64, end: u64) -> Self {
        Source {
            inner,
            buf: Vec::with_capacity(2 * READ_CHUNK),
            pos: 0,
            base: start,
            eof: false,
            end,
        }
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Absolute offset of the cursor.
    #[inline]
    pub fn offset(&self) -> u64 {
        self.base + self.pos as u64
    }

    #[inline]
    pub fn end(&self) -> u64 {
        self.end
    }

    /// Bytes available at the cursor (bounded by the audio data end).
    #[inline]
    fn available(&self) -> usize {
        let in_buf = self.buf.len() - self.pos;
        let to_end = self.end.saturating_sub(self.offset());
        if to_end < in_buf as u64 {
            to_end as usize
        } else {
            in_buf
        }
    }

    /// Tries to make at least `n` bytes available; returns the number of
    /// bytes available (fewer than `n` only at the end of data).
    pub fn fill(&mut self, n: usize) -> io::Result<usize> {
        if self.available() >= n || self.eof || self.offset() >= self.end {
            return Ok(self.available());
        }
        if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.base += self.pos as u64;
            self.pos = 0;
        }
        while self.buf.len() < n && !self.eof {
            let old = self.buf.len();
            let want = (n - old).max(READ_CHUNK);
            self.buf.resize(old + want, 0);
            let got = loop {
                match self.inner.read(&mut self.buf[old..]) {
                    Ok(k) => break k,
                    Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                    Err(e) => {
                        self.buf.truncate(old);
                        return Err(e);
                    }
                }
            };
            self.buf.truncate(old + got);
            if got == 0 {
                self.eof = true;
            }
            if self.base + self.buf.len() as u64 >= self.end {
                break;
            }
        }
        Ok(self.available())
    }

    /// The bytes currently available at the cursor.
    #[inline]
    pub fn peek(&self) -> &[u8] {
        &self.buf[self.pos..self.pos + self.available()]
    }

    /// Advances the cursor by `n` bytes (may go past buffered data).
    pub fn consume(&mut self, n: usize) -> io::Result<()> {
        let target = self.offset().saturating_add(n as u64);
        if n <= self.buf.len() - self.pos {
            self.pos += n;
            Ok(())
        } else {
            self.seek_to(target)
        }
    }

    /// Repositions the cursor at absolute offset `abs`.
    pub fn seek_to(&mut self, abs: u64) -> io::Result<()> {
        if abs >= self.base && abs <= self.base + self.buf.len() as u64 {
            self.pos = (abs - self.base) as usize;
            return Ok(());
        }
        self.buf.clear();
        self.pos = 0;
        self.base = abs;
        self.eof = false;
        if abs >= self.end {
            self.eof = true;
            return Ok(());
        }
        self.inner.seek(SeekFrom::Start(abs))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fill_peek_consume_seek() {
        let data: Vec<u8> = (0..100_000u32).map(|i| i as u8).collect();
        let mut s = Source::new(Cursor::new(data.clone()), 0, 90_000);
        assert_eq!(s.fill(10).unwrap(), READ_CHUNK);
        assert_eq!(s.peek()[5], 5);
        s.consume(40_000).unwrap();
        assert_eq!(s.offset(), 40_000);
        s.fill(4).unwrap();
        assert_eq!(s.peek()[0], data[40_000]);
        s.seek_to(89_998).unwrap();
        assert_eq!(s.fill(10).unwrap(), 2);
        s.seek_to(10).unwrap();
        assert_eq!(s.fill(70_000).unwrap(), 70_000);
        assert_eq!(s.peek()[69_999], data[70_009]);
    }
}
