//! Small byte-level helpers shared by the container parsers and tag readers.

use std::io::{self, Read, Seek, SeekFrom};

use crate::Error;

/// Shorthand for an `InvalidData` error result.
pub(crate) fn invalid<T>(msg: &str) -> Result<T, Error> {
    Err(Error::InvalidData(msg.to_string()))
}

/// Shorthand for an `Unsupported` error result.
pub(crate) fn unsupported<T>(msg: impl Into<String>) -> Result<T, Error> {
    Err(Error::Unsupported(msg.into()))
}

/// Reads as many bytes as possible into `buf`. Returns the number of bytes
/// read, which is less than `buf.len()` only at the end of the stream.
pub(crate) fn read_up_to<R: Read + ?Sized>(r: &mut R, buf: &mut [u8]) -> io::Result<usize> {
    let mut pos = 0;
    while pos < buf.len() {
        match r.read(&mut buf[pos..]) {
            Ok(0) => break,
            Ok(n) => pos += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(pos)
}

/// Reads `len` bytes into a new vector without trusting `len` for the
/// allocation (the vector grows as data arrives), so corrupted size fields
/// cannot trigger huge allocations. Fails if `len > limit` or on EOF.
pub(crate) fn read_vec<R: Read + ?Sized>(r: &mut R, len: u64, limit: u64) -> Result<Vec<u8>, Error> {
    if len > limit {
        return invalid("element too large");
    }
    let mut v = Vec::with_capacity(len.min(1 << 20) as usize);
    let got = r.take(len).read_to_end(&mut v)?;
    if got as u64 != len {
        return invalid("unexpected end of file");
    }
    Ok(v)
}

/// Total length of a seekable stream; the current position is preserved.
pub(crate) fn stream_len<R: Seek + ?Sized>(r: &mut R) -> io::Result<u64> {
    let pos = r.stream_position()?;
    let len = r.seek(SeekFrom::End(0))?;
    if pos != len {
        r.seek(SeekFrom::Start(pos))?;
    }
    Ok(len)
}

/// Positions a buffered reader at `target` without discarding its buffer
/// when the target is close to the current position (`cur`).
pub(crate) fn seek_buffered<R: Read + Seek>(r: &mut std::io::BufReader<R>, cur: &mut u64, target: u64) -> io::Result<()> {
    if *cur != target {
        let delta = target as i128 - *cur as i128;
        if delta.abs() < (1 << 20) {
            r.seek_relative(delta as i64)?;
        } else {
            r.seek(SeekFrom::Start(target))?;
        }
        *cur = target;
    }
    Ok(())
}

/// Bounds-checked big/little endian reader over a byte slice.
#[derive(Clone)]
pub(crate) struct ByteCursor<'a> {
    pub data: &'a [u8],
    pub pos: usize,
}

#[allow(dead_code)]
impl<'a> ByteCursor<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        ByteCursor { data, pos: 0 }
    }
    pub fn remaining(&self) -> usize {
        self.data.len().saturating_sub(self.pos)
    }
    pub fn rest(&self) -> &'a [u8] {
        &self.data[self.pos.min(self.data.len())..]
    }
    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        if self.remaining() < n {
            return invalid("truncated data");
        }
        let s = &self.data[self.pos..self.pos + n];
        self.pos += n;
        Ok(s)
    }
    pub fn skip(&mut self, n: usize) -> Result<(), Error> {
        self.bytes(n).map(|_| ())
    }
    pub fn array<const N: usize>(&mut self) -> Result<[u8; N], Error> {
        let mut a = [0u8; N];
        a.copy_from_slice(self.bytes(N)?);
        Ok(a)
    }
    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }
    pub fn be16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_be_bytes(self.array()?))
    }
    pub fn be24(&mut self) -> Result<u32, Error> {
        let b = self.bytes(3)?;
        Ok(((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32)
    }
    pub fn be32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_be_bytes(self.array()?))
    }
    pub fn be64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_be_bytes(self.array()?))
    }
    pub fn le16(&mut self) -> Result<u16, Error> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    pub fn le32(&mut self) -> Result<u32, Error> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    pub fn le64(&mut self) -> Result<u64, Error> {
        Ok(u64::from_le_bytes(self.array()?))
    }
}

pub(crate) fn be16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_be_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
pub(crate) fn be32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_be_bytes(d.get(at..at + 4)?.try_into().ok()?))
}
pub(crate) fn be64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_be_bytes(d.get(at..at + 8)?.try_into().ok()?))
}
pub(crate) fn le16(d: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(d.get(at..at + 2)?.try_into().ok()?))
}
pub(crate) fn le32(d: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(d.get(at..at + 4)?.try_into().ok()?))
}
pub(crate) fn le64(d: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(d.get(at..at + 8)?.try_into().ok()?))
}

/// Decodes ISO-8859-1 text.
pub(crate) fn latin1(data: &[u8]) -> String {
    data.iter().map(|&b| b as char).collect()
}

/// Decodes text of unknown encoding: UTF-8 if valid, ISO-8859-1 otherwise.
/// Trailing NULs are removed.
pub(crate) fn utf8_or_latin1(data: &[u8]) -> String {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |p| p + 1);
    let data = &data[..end];
    match std::str::from_utf8(data) {
        Ok(s) => s.to_string(),
        Err(_) => latin1(data),
    }
}

/// `a * b / c` without intermediate overflow (saturating on overflow).
pub(crate) fn mul_div(a: u64, b: u64, c: u64) -> u64 {
    if c == 0 {
        return 0;
    }
    let r = (a as u128 * b as u128) / c as u128;
    r.min(u64::MAX as u128) as u64
}

/// Converts a frame count to milliseconds.
pub(crate) fn frames_to_ms(frames: u64, rate: u32) -> u64 {
    mul_div(frames, 1000, rate as u64)
}

/// Bitrate in kbit/s from a byte count and a duration in frames.
pub(crate) fn bitrate_kbps(bytes: u64, frames: u64, rate: u32) -> Option<u32> {
    if frames == 0 || rate == 0 {
        return None;
    }
    let kbps = mul_div(bytes.saturating_mul(8), rate as u64, frames.saturating_mul(1000));
    u32::try_from(kbps).ok()
}

/// Appends the part of a decoded chunk that lies inside the output window
/// `[start, end)` (PCM frame positions). `chunk_pos` is the position of the
/// first frame of `chunk`. Returns the number of frames appended.
pub(crate) fn emit_window(
    chunk: &[f32],
    channels: usize,
    chunk_pos: i64,
    start: u64,
    end: Option<u64>,
    out: &mut Vec<f32>,
) -> usize {
    if channels == 0 {
        return 0;
    }
    let n = (chunk.len() / channels) as i64;
    let lo = (start as i64).max(chunk_pos);
    let hi = match end {
        Some(e) => (e.min(i64::MAX as u64) as i64).min(chunk_pos + n),
        None => chunk_pos + n,
    };
    if hi <= lo {
        return 0;
    }
    let a = (lo - chunk_pos) as usize * channels;
    let b = (hi - chunk_pos) as usize * channels;
    out.extend_from_slice(&chunk[a..b]);
    (hi - lo) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window() {
        let chunk: Vec<f32> = (0..10).map(|v| v as f32).collect();
        let mut out = Vec::new();
        // mono, chunk at position 5..15, window [8, 12)
        assert_eq!(emit_window(&chunk, 1, 5, 8, Some(12), &mut out), 4);
        assert_eq!(out, vec![3.0, 4.0, 5.0, 6.0]);
        out.clear();
        assert_eq!(emit_window(&chunk, 2, -2, 0, None, &mut out), 3);
        assert_eq!(out, vec![4.0, 5.0, 6.0, 7.0, 8.0, 9.0]);
        out.clear();
        assert_eq!(emit_window(&chunk, 1, 0, 20, None, &mut out), 0);
    }

    #[test]
    fn cursor_bounds() {
        let mut c = ByteCursor::new(&[1, 2, 3]);
        assert_eq!(c.be16().unwrap(), 0x0102);
        assert!(c.be16().is_err());
        assert_eq!(c.u8().unwrap(), 3);
    }
}
