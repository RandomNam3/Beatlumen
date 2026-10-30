//! TLV8, the encoding of pairing messages: one byte type, one byte length,
//! up to 255 bytes of value; longer values are split into consecutive
//! items of the same type.

pub const METHOD: u8 = 0;
pub const IDENTIFIER: u8 = 1;
pub const SALT: u8 = 2;
pub const PUBLIC_KEY: u8 = 3;
pub const PROOF: u8 = 4;
pub const ENCRYPTED_DATA: u8 = 5;
pub const STATE: u8 = 6;
pub const ERROR: u8 = 7;
pub const SIGNATURE: u8 = 10;
pub const FLAGS: u8 = 19;

/// Error value: authentication failed.
pub const ERROR_AUTHENTICATION: u8 = 2;
/// Error value: the accessory cannot accept the request now.
pub const ERROR_UNAVAILABLE: u8 = 6;

/// Most items in one message (guards against oversized input).
const MAX_ITEMS: usize = 64;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Tlv {
    items: Vec<(u8, Vec<u8>)>,
}

impl Tlv {
    pub fn new() -> Tlv {
        Tlv::default()
    }

    pub fn with(mut self, ty: u8, value: &[u8]) -> Tlv {
        self.items.push((ty, value.to_vec()));
        self
    }

    pub fn get(&self, ty: u8) -> Option<&[u8]> {
        self.items
            .iter()
            .find(|(t, _)| *t == ty)
            .map(|(_, v)| v.as_slice())
    }

    /// A one-byte value.
    pub fn byte(&self, ty: u8) -> Option<u8> {
        match self.get(ty)? {
            [b] => Some(*b),
            _ => None,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for (ty, value) in &self.items {
            if value.is_empty() {
                out.extend_from_slice(&[*ty, 0]);
            }
            for chunk in value.chunks(255) {
                out.push(*ty);
                out.push(chunk.len() as u8);
                out.extend_from_slice(chunk);
            }
        }
        out
    }

    /// Parses a message; consecutive fragments of one type are joined.
    pub fn decode(data: &[u8]) -> Option<Tlv> {
        let mut items: Vec<(u8, Vec<u8>)> = Vec::new();
        let mut pos = 0;
        let mut prev_full = false;
        while pos < data.len() {
            let ty = data[pos];
            let len = *data.get(pos + 1)? as usize;
            let value = data.get(pos + 2..pos + 2 + len)?;
            match items.last_mut() {
                Some((t, v)) if *t == ty && prev_full => v.extend_from_slice(value),
                _ => {
                    if items.len() >= MAX_ITEMS {
                        return None;
                    }
                    items.push((ty, value.to_vec()));
                }
            }
            prev_full = len == 255;
            pos += 2 + len;
        }
        Some(Tlv { items })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_with_long_values() {
        let long: Vec<u8> = (0..700u32).map(|i| i as u8).collect();
        let t = Tlv::new()
            .with(STATE, &[1])
            .with(PUBLIC_KEY, &long)
            .with(SALT, &[9; 16])
            .with(SIGNATURE, &[]);
        let bytes = t.encode();
        // 700 bytes in three fragments: 255 + 255 + 190.
        assert_eq!(bytes.len(), 3 + (2 * 3 + 700) + 18 + 2);
        assert_eq!(&bytes[3..5], &[PUBLIC_KEY, 255]);
        let back = Tlv::decode(&bytes).unwrap();
        assert_eq!(back, t);
        assert_eq!(back.byte(STATE), Some(1));
        assert_eq!(back.get(PUBLIC_KEY).unwrap(), &long[..]);
        assert_eq!(back.get(SIGNATURE).unwrap(), &[] as &[u8]);
        assert_eq!(back.get(ERROR), None);
    }

    #[test]
    fn exact_255_values_stay_separate_from_the_next_item() {
        // Two items of one type: the first exactly 255 bytes long is
        // joined with what follows (as the format defines).
        let t = Tlv::new().with(IDENTIFIER, &[1; 255]).with(SALT, &[2]);
        let back = Tlv::decode(&t.encode()).unwrap();
        assert_eq!(back.get(IDENTIFIER).unwrap().len(), 255);
        assert_eq!(back.get(SALT).unwrap(), &[2]);
    }

    #[test]
    fn malformed_input_is_rejected() {
        assert!(Tlv::decode(&[6]).is_none());
        assert!(Tlv::decode(&[6, 3, 1]).is_none());
        let many: Vec<u8> = (0..200u8).flat_map(|i| [i, 0]).collect();
        assert!(Tlv::decode(&many).is_none());
        assert_eq!(Tlv::decode(&[]).unwrap(), Tlv::new());
    }
}
