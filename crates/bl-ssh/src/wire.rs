//! SSH wire encoding (RFC 4251 section 5): bytes, booleans, integers,
//! strings, mpints and name-lists. Reading checks every length against the
//! data actually there.

use crate::Error;

/// Appends SSH data types to a buffer.
pub(crate) trait Put {
    fn u8(&mut self, v: u8);
    fn bool(&mut self, v: bool);
    fn u32(&mut self, v: u32);
    fn u64(&mut self, v: u64);
    fn string(&mut self, v: &[u8]);
    /// A non-negative integer given as big-endian bytes.
    fn mpint(&mut self, v: &[u8]);
}

impl Put for Vec<u8> {
    fn u8(&mut self, v: u8) {
        self.push(v);
    }

    fn bool(&mut self, v: bool) {
        self.push(v as u8);
    }

    fn u32(&mut self, v: u32) {
        self.extend_from_slice(&v.to_be_bytes());
    }

    fn u64(&mut self, v: u64) {
        self.extend_from_slice(&v.to_be_bytes());
    }

    fn string(&mut self, v: &[u8]) {
        // Callers only pass data far below 4 GiB (packets are bounded).
        self.u32(v.len() as u32);
        self.extend_from_slice(v);
    }

    fn mpint(&mut self, v: &[u8]) {
        let start = v.iter().position(|&b| b != 0).unwrap_or(v.len());
        let v = &v[start..];
        let pad = v.first().is_some_and(|&b| b & 0x80 != 0);
        self.u32((v.len() + pad as usize) as u32);
        if pad {
            self.push(0);
        }
        self.extend_from_slice(v);
    }
}

/// Reads SSH data types from a message.
pub(crate) struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

fn truncated() -> Error {
    Error::Protocol("truncated message".into())
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    pub fn bytes(&mut self, n: usize) -> Result<&'a [u8], Error> {
        let end = self.pos.checked_add(n).ok_or_else(truncated)?;
        let s = self.buf.get(self.pos..end).ok_or_else(truncated)?;
        self.pos = end;
        Ok(s)
    }

    pub fn u8(&mut self) -> Result<u8, Error> {
        Ok(self.bytes(1)?[0])
    }

    pub fn bool(&mut self) -> Result<bool, Error> {
        Ok(self.u8()? != 0)
    }

    pub fn u32(&mut self) -> Result<u32, Error> {
        let b = self.bytes(4)?;
        Ok(u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
    }

    pub fn u64(&mut self) -> Result<u64, Error> {
        let b = self.bytes(8)?;
        let mut a = [0u8; 8];
        a.copy_from_slice(b);
        Ok(u64::from_be_bytes(a))
    }

    pub fn string(&mut self) -> Result<&'a [u8], Error> {
        let n = self.u32()? as usize;
        self.bytes(n)
    }

    /// A string that must be UTF-8 (algorithm names, messages).
    pub fn str(&mut self) -> Result<&'a str, Error> {
        std::str::from_utf8(self.string()?).map_err(|_| Error::Protocol("invalid text".into()))
    }

    pub fn name_list(&mut self) -> Result<Vec<&'a str>, Error> {
        let s = self.str()?;
        Ok(if s.is_empty() {
            Vec::new()
        } else {
            s.split(',').collect()
        })
    }

    /// A non-negative mpint as big-endian bytes without leading zeros.
    pub fn mpint(&mut self) -> Result<&'a [u8], Error> {
        let v = self.string()?;
        if v.first().is_some_and(|&b| b & 0x80 != 0) {
            return Err(Error::Protocol("negative number".into()));
        }
        let start = v.iter().position(|&b| b != 0).unwrap_or(v.len());
        Ok(&v[start..])
    }

    pub fn rest(&mut self) -> &'a [u8] {
        let s = &self.buf[self.pos..];
        self.pos = self.buf.len();
        s
    }

    pub fn is_empty(&self) -> bool {
        self.pos >= self.buf.len()
    }

    /// Fails when anything is left (strictly formed messages).
    pub fn end(&self) -> Result<(), Error> {
        if self.is_empty() {
            Ok(())
        } else {
            Err(Error::Protocol(
                "unexpected data at the end of a message".into(),
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mp(v: &[u8]) -> Vec<u8> {
        let mut b = Vec::new();
        b.mpint(v);
        b
    }

    #[test]
    fn mpint_examples_from_rfc4251() {
        assert_eq!(mp(&[]), [0, 0, 0, 0]);
        assert_eq!(mp(&[0, 0]), [0, 0, 0, 0]);
        assert_eq!(
            mp(&[0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]),
            [0, 0, 0, 8, 0x09, 0xa3, 0x78, 0xf9, 0xb2, 0xe3, 0x32, 0xa7]
        );
        assert_eq!(mp(&[0x80]), [0, 0, 0, 2, 0, 0x80]);
        assert_eq!(mp(&[0, 0, 0x7f]), [0, 0, 0, 1, 0x7f]);
        let enc = mp(&[0x80, 1]);
        assert_eq!(Reader::new(&enc).mpint().unwrap(), [0x80, 1]);
        assert!(Reader::new(&[0, 0, 0, 1, 0xff]).mpint().is_err());
    }

    #[test]
    fn strings_and_bounds() {
        let mut b = Vec::new();
        b.string(b"ssh-ed25519");
        b.bool(true);
        b.u32(7);
        b.u64(1 << 40);
        b.string(b"a,b");
        let mut r = Reader::new(&b);
        assert_eq!(r.str().unwrap(), "ssh-ed25519");
        assert!(r.bool().unwrap());
        assert_eq!(r.u32().unwrap(), 7);
        assert_eq!(r.u64().unwrap(), 1 << 40);
        assert_eq!(r.name_list().unwrap(), ["a", "b"]);
        r.end().unwrap();
        assert!(r.u8().is_err());
        // A length beyond the data and one that overflows.
        assert!(Reader::new(&[0, 0, 0, 5, 1, 2]).string().is_err());
        assert!(Reader::new(&[0xff, 0xff, 0xff, 0xff]).string().is_err());
        assert_eq!(Reader::new(&[0, 0, 0, 0]).name_list().unwrap().len(), 0);
    }
}
