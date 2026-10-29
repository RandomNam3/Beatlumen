//! The CastV2 message (a small protobuf) and its framing on the wire: a
//! 4-byte big-endian length, then the message.
//!
//! ```text
//! message CastMessage {
//!   required ProtocolVersion protocol_version = 1;  // 0 = CASTV2_1_0
//!   required string source_id = 2;
//!   required string destination_id = 3;
//!   required string namespace = 4;
//!   required PayloadType payload_type = 5;           // 0 = STRING
//!   optional string payload_utf8 = 6;
//!   optional bytes payload_binary = 7;
//! }
//! ```

/// Largest accepted message.
pub const MAX_MESSAGE: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Message {
    pub source: String,
    pub destination: String,
    pub namespace: String,
    pub payload: String,
}

fn varint(mut v: u64, out: &mut Vec<u8>) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn field_str(field: u64, s: &str, out: &mut Vec<u8>) {
    varint(field << 3 | 2, out);
    varint(s.len() as u64, out);
    out.extend_from_slice(s.as_bytes());
}

impl Message {
    /// The message with its length prefix.
    pub fn encode(&self) -> Vec<u8> {
        let mut m = Vec::with_capacity(64 + self.payload.len());
        varint(1 << 3, &mut m);
        varint(0, &mut m);
        field_str(2, &self.source, &mut m);
        field_str(3, &self.destination, &mut m);
        field_str(4, &self.namespace, &mut m);
        varint(5 << 3, &mut m);
        varint(0, &mut m);
        field_str(6, &self.payload, &mut m);
        let mut out = (m.len() as u32).to_be_bytes().to_vec();
        out.extend(m);
        out
    }

    /// Decodes one message (without the length prefix).
    pub fn decode(b: &[u8]) -> Option<Message> {
        let mut m = Message::default();
        let mut i = 0;
        let read_varint = |i: &mut usize| -> Option<u64> {
            let mut v = 0u64;
            for shift in (0..64).step_by(7) {
                let byte = *b.get(*i)?;
                *i += 1;
                v |= ((byte & 0x7f) as u64) << shift;
                if byte & 0x80 == 0 {
                    return Some(v);
                }
            }
            None
        };
        while i < b.len() {
            let key = read_varint(&mut i)?;
            let (field, wire) = (key >> 3, key & 7);
            match wire {
                0 => {
                    read_varint(&mut i)?;
                }
                2 => {
                    let len = read_varint(&mut i)? as usize;
                    let data = b.get(i..i.checked_add(len)?)?;
                    i += len;
                    let s = || String::from_utf8_lossy(data).into_owned();
                    match field {
                        2 => m.source = s(),
                        3 => m.destination = s(),
                        4 => m.namespace = s(),
                        6 => m.payload = s(),
                        _ => {}
                    }
                }
                // Fixed 64/32-bit fields do not occur; skip them anyway.
                1 => i += 8,
                5 => i += 4,
                _ => return None,
            }
        }
        Some(m)
    }
}

/// Takes complete messages from the front of `buf`.
pub fn take_messages(buf: &mut Vec<u8>) -> Result<Vec<Message>, &'static str> {
    let mut out = Vec::new();
    loop {
        if buf.len() < 4 {
            return Ok(out);
        }
        let len = u32::from_be_bytes([buf[0], buf[1], buf[2], buf[3]]) as usize;
        if len > MAX_MESSAGE {
            return Err("message too large");
        }
        if buf.len() < 4 + len {
            return Ok(out);
        }
        let m = Message::decode(&buf[4..4 + len]).ok_or("invalid message")?;
        buf.drain(..4 + len);
        out.push(m);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_framing() {
        let m = Message {
            source: "sender-0".into(),
            destination: "receiver-0".into(),
            namespace: "urn:x-cast:com.google.cast.tp.connection".into(),
            payload: "{\"type\":\"CONNECT\"}".into(),
        };
        let mut wire = m.encode();
        wire.extend(m.encode());
        wire.extend_from_slice(&[0, 0]); // start of a third
        let got = take_messages(&mut wire).unwrap();
        assert_eq!(got, vec![m.clone(), m]);
        assert_eq!(wire, vec![0, 0]);
    }
}
