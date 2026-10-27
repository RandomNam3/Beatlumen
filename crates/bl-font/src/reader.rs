//! Bounds-checked big-endian readers used by all table parsers.
//!
//! Every accessor returns `Option` so malformed or truncated font data turns
//! into `None` (and eventually an error or an empty result) instead of a
//! panic.

/// Sequential big-endian reader over a byte slice.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Reader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    #[inline]
    pub fn new(data: &'a [u8]) -> Self {
        Reader { data, pos: 0 }
    }

    /// A reader starting at `pos`; reads fail if `pos` is out of range.
    #[inline]
    pub fn at(data: &'a [u8], pos: usize) -> Self {
        Reader { data, pos }
    }

    #[inline]
    pub fn skip(&mut self, n: usize) -> Option<()> {
        let end = self.pos.checked_add(n)?;
        if end > self.data.len() {
            return None;
        }
        self.pos = end;
        Some(())
    }

    #[inline]
    pub fn bytes(&mut self, n: usize) -> Option<&'a [u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.data.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    #[inline]
    pub fn u8(&mut self) -> Option<u8> {
        let v = *self.data.get(self.pos)?;
        self.pos += 1;
        Some(v)
    }

    #[inline]
    pub fn i8(&mut self) -> Option<i8> {
        self.u8().map(|v| v as i8)
    }

    #[inline]
    pub fn u16(&mut self) -> Option<u16> {
        let v = u16_at(self.data, self.pos)?;
        self.pos += 2;
        Some(v)
    }

    #[inline]
    pub fn i16(&mut self) -> Option<i16> {
        self.u16().map(|v| v as i16)
    }

    #[inline]
    pub fn u32(&mut self) -> Option<u32> {
        let v = u32_at(self.data, self.pos)?;
        self.pos += 4;
        Some(v)
    }

    #[inline]
    pub fn i32(&mut self) -> Option<i32> {
        self.u32().map(|v| v as i32)
    }

    /// Reads an unsigned big-endian integer of `n` (1..=4) bytes.
    #[inline]
    pub fn uint_n(&mut self, n: usize) -> Option<u32> {
        let b = self.bytes(n)?;
        if n == 0 || n > 4 {
            return None;
        }
        Some(b.iter().fold(0u32, |acc, &x| (acc << 8) | x as u32))
    }

    /// 2.14 fixed point number.
    #[inline]
    pub fn f2dot14(&mut self) -> Option<f32> {
        self.i16().map(|v| v as f32 / 16384.0)
    }

    /// 16.16 fixed point number.
    #[inline]
    pub fn fixed(&mut self) -> Option<f32> {
        self.i32().map(|v| v as f32 / 65536.0)
    }

    /// Four-byte tag.
    #[inline]
    pub fn tag(&mut self) -> Option<[u8; 4]> {
        let b = self.bytes(4)?;
        Some([b[0], b[1], b[2], b[3]])
    }
}

#[inline]
pub(crate) fn u8_at(data: &[u8], off: usize) -> Option<u8> {
    data.get(off).copied()
}

#[inline]
pub(crate) fn u16_at(data: &[u8], off: usize) -> Option<u16> {
    let b = data.get(off..off.checked_add(2)?)?;
    Some(u16::from_be_bytes([b[0], b[1]]))
}

#[inline]
pub(crate) fn i16_at(data: &[u8], off: usize) -> Option<i16> {
    u16_at(data, off).map(|v| v as i16)
}

#[inline]
pub(crate) fn u32_at(data: &[u8], off: usize) -> Option<u32> {
    let b = data.get(off..off.checked_add(4)?)?;
    Some(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
}

/// Binary search over `count` fixed-size records starting at `start`, keyed
/// by a `u16` at the beginning of each record. Returns the record index.
pub(crate) fn bsearch_u16(
    data: &[u8],
    start: usize,
    count: usize,
    record_size: usize,
    key: u16,
) -> Option<usize> {
    let (mut lo, mut hi) = (0usize, count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let v = u16_at(data, start.checked_add(mid.checked_mul(record_size)?)?)?;
        if v < key {
            lo = mid + 1;
        } else if v > key {
            hi = mid;
        } else {
            return Some(mid);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_bounds() {
        let d = [0x12, 0x34, 0x56, 0x78, 0xff];
        let mut r = Reader::new(&d);
        assert_eq!(r.u16(), Some(0x1234));
        assert_eq!(r.u16(), Some(0x5678));
        assert_eq!(r.u16(), None);
        assert_eq!(r.i8(), Some(-1));
        assert_eq!(r.u8(), None);
        assert_eq!(u32_at(&d, 1), Some(0x345678ff));
        assert_eq!(u32_at(&d, 2), None);
        assert_eq!(u16_at(&d, usize::MAX), None);
        let mut r = Reader::at(&d, 100);
        assert_eq!(r.u8(), None);
        assert!(r.skip(usize::MAX).is_none());
        let mut r = Reader::new(&d);
        assert_eq!(r.uint_n(3), Some(0x123456));
    }

    #[test]
    fn binary_search() {
        let d = [0, 1, 0xAA, 0, 5, 0xBB, 0, 9, 0xCC];
        assert_eq!(bsearch_u16(&d, 0, 3, 3, 5), Some(1));
        assert_eq!(bsearch_u16(&d, 0, 3, 3, 9), Some(2));
        assert_eq!(bsearch_u16(&d, 0, 3, 3, 4), None);
        assert_eq!(bsearch_u16(&d, 0, 5, 3, 100), None);
    }
}
