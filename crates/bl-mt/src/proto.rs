//! A minimal reader for protocol buffer messages (the SentencePiece model
//! files): fields one after another, each with its number and wire type.
//! Lengths are checked against the message, so broken input is an error.

/// A field's value as it is encoded.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Field<'a> {
    Varint(u64),
    Fixed64(u64),
    /// Strings, bytes, nested messages and packed arrays.
    Bytes(&'a [u8]),
    Fixed32(u32),
}

impl Field<'_> {
    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Field::Fixed32(v) => Some(f32::from_bits(v)),
            _ => None,
        }
    }
}

/// Iterates over the fields of a message.
pub struct Reader<'a> {
    buf: &'a [u8],
    pos: usize,
}

impl<'a> Reader<'a> {
    pub fn new(buf: &'a [u8]) -> Reader<'a> {
        Reader { buf, pos: 0 }
    }

    fn varint(&mut self) -> Result<u64, String> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = *self.buf.get(self.pos).ok_or("protobuf: truncated")?;
            self.pos += 1;
            v |= ((b & 0x7f) as u64) << (7 * i);
            if b & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("protobuf: varint too long".into())
    }

    fn take(&mut self, n: usize) -> Result<&'a [u8], String> {
        let end = self
            .pos
            .checked_add(n)
            .filter(|&e| e <= self.buf.len())
            .ok_or("protobuf: truncated")?;
        let s = &self.buf[self.pos..end];
        self.pos = end;
        Ok(s)
    }

    /// The next field (number and value), or `None` at the end.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Result<Option<(u32, Field<'a>)>, String> {
        if self.pos >= self.buf.len() {
            return Ok(None);
        }
        let key = self.varint()?;
        let number = u32::try_from(key >> 3)
            .ok()
            .filter(|&n| n != 0)
            .ok_or("protobuf: bad field number")?;
        let field = match key & 7 {
            0 => Field::Varint(self.varint()?),
            1 => Field::Fixed64(u64::from_le_bytes(
                self.take(8)?.try_into().unwrap_or([0; 8]),
            )),
            2 => {
                let n = usize::try_from(self.varint()?).map_err(|_| "protobuf: truncated")?;
                Field::Bytes(self.take(n)?)
            }
            5 => Field::Fixed32(u32::from_le_bytes(
                self.take(4)?.try_into().unwrap_or([0; 4]),
            )),
            _ => return Err("protobuf: unsupported wire type".into()),
        };
        Ok(Some((number, field)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_fields() {
        // 1: varint 150; 2: "hi"; 3: fixed32 1.5; 4: fixed64 7.
        let mut b = vec![0x08, 0x96, 0x01, 0x12, 2, b'h', b'i', 0x1d];
        b.extend_from_slice(&1.5f32.to_le_bytes());
        b.push(0x21);
        b.extend_from_slice(&7u64.to_le_bytes());
        let mut r = Reader::new(&b);
        assert_eq!(r.next().unwrap(), Some((1, Field::Varint(150))));
        assert_eq!(r.next().unwrap(), Some((2, Field::Bytes(b"hi"))));
        let (n, f) = r.next().unwrap().unwrap();
        assert_eq!((n, f.as_f32()), (3, Some(1.5)));
        assert_eq!(r.next().unwrap(), Some((4, Field::Fixed64(7))));
        assert_eq!(r.next().unwrap(), None);
    }

    #[test]
    fn rejects_broken_messages() {
        let bad: &[&[u8]] = &[
            &[0x08],          // varint missing
            &[0x08, 0x80],    // varint cut
            &[0x12, 5, b'a'], // length beyond the message
            &[
                0x12, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x01,
            ], // huge length
            &[0x1d, 1, 2],    // fixed32 cut
            &[0x0b],          // group (wire type 3)
            &[0x00, 0x00],    // field number 0
            &[
                0x08, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x80, 0x01,
            ], // varint too long
        ];
        for b in bad {
            let mut r = Reader::new(b);
            let mut res = Ok(None);
            for _ in 0..4 {
                res = r.next();
                if !matches!(res, Ok(Some(_))) {
                    break;
                }
            }
            assert!(res.is_err(), "{b:?}");
        }
    }
}
