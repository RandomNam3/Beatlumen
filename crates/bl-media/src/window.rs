//! A `Read + Seek` view of a byte range of another stream (used to hide
//! trailing tags such as ID3v1/APEv2 from stream-level codec decoders).

use std::io::{self, Read, Seek, SeekFrom};

pub(crate) struct Window<R> {
    inner: R,
    start: u64,
    end: u64,
    pos: u64,
}

impl<R: Read + Seek> Window<R> {
    /// Creates a view of `[start, end)` of `inner`, positioned at `start`.
    pub fn new(mut inner: R, start: u64, end: u64) -> io::Result<Self> {
        let end = end.max(start);
        inner.seek(SeekFrom::Start(start))?;
        Ok(Window {
            inner,
            start,
            end,
            pos: start,
        })
    }
}

impl<R: Read + Seek> Read for Window<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.end.saturating_sub(self.pos);
        if left == 0 || buf.is_empty() {
            return Ok(0);
        }
        let n = (buf.len() as u64).min(left) as usize;
        let got = self.inner.read(&mut buf[..n])?;
        self.pos += got as u64;
        Ok(got)
    }
}

impl<R: Read + Seek> Seek for Window<R> {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target: i128 = match to {
            SeekFrom::Start(n) => self.start as i128 + n as i128,
            SeekFrom::End(k) => self.end as i128 + k as i128,
            SeekFrom::Current(k) => self.pos as i128 + k as i128,
        };
        if target < self.start as i128 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start of window",
            ));
        }
        let target = target.min(u64::MAX as i128) as u64;
        if target != self.pos {
            self.inner.seek(SeekFrom::Start(target))?;
            self.pos = target;
        }
        Ok(self.pos - self.start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn window_bounds() {
        let data: Vec<u8> = (0..100).collect();
        let mut w = Window::new(Cursor::new(data), 10, 20).unwrap();
        let mut buf = Vec::new();
        w.read_to_end(&mut buf).unwrap();
        assert_eq!(buf, (10..20).collect::<Vec<u8>>());
        assert_eq!(w.seek(SeekFrom::End(-2)).unwrap(), 8);
        let mut b = [0u8; 4];
        assert_eq!(w.read(&mut b).unwrap(), 2);
        assert_eq!(&b[..2], &[18, 19]);
        assert!(w.seek(SeekFrom::Current(-20)).is_err());
    }
}
