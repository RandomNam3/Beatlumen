//! Binary property lists ("bplist00"), the body format of AirPlay 2
//! requests and responses: a reader with limits for untrusted input and a
//! writer.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i128),
    Real(f64),
    /// Seconds since 2001-01-01.
    Date(f64),
    Data(Vec<u8>),
    String(String),
    Uid(u64),
    Array(Vec<Value>),
    Dict(Vec<(String, Value)>),
}

/// Deepest nesting accepted.
const MAX_DEPTH: usize = 32;
/// Most objects decoded from one plist (shared references count again,
/// so a small file cannot expand into a huge tree).
const MAX_NODES: usize = 100_000;

impl Value {
    /// A dictionary from key/value pairs.
    pub fn dict<const N: usize>(entries: [(&str, Value); N]) -> Value {
        Value::Dict(
            entries
                .into_iter()
                .map(|(k, v)| (k.to_string(), v))
                .collect(),
        )
    }

    /// Looks up a dictionary key.
    pub fn get(&self, key: &str) -> Option<&Value> {
        match self {
            Value::Dict(d) => d.iter().find(|(k, _)| k == key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn as_u64(&self) -> Option<u64> {
        match *self {
            Value::Int(v) if (0..=u64::MAX as i128).contains(&v) => Some(v as u64),
            // Unsigned values written as negative 64-bit integers.
            Value::Int(v) if v >= i64::MIN as i128 && v < 0 => Some(v as i64 as u64),
            Value::Real(f) if f >= 0.0 && f < u64::MAX as f64 => Some(f as u64),
            _ => None,
        }
    }

    pub fn as_f64(&self) -> Option<f64> {
        match *self {
            Value::Int(v) => Some(v as f64),
            Value::Real(f) => Some(f),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match *self {
            Value::Bool(b) => Some(b),
            Value::Int(v) => Some(v != 0),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_data(&self) -> Option<&[u8]> {
        match self {
            Value::Data(d) => Some(d),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Value::Array(a) => Some(a),
            _ => None,
        }
    }
}

// ---- reading -----------------------------------------------------------------

struct Reader<'a> {
    data: &'a [u8],
    offset_size: usize,
    ref_size: usize,
    count: usize,
    table: usize,
    nodes: usize,
}

fn be(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |acc, &b| acc << 8 | b as u64)
}

impl Reader<'_> {
    fn offset(&self, index: usize) -> Result<usize, &'static str> {
        if index >= self.count {
            return Err("object reference out of range");
        }
        let at = self.table + index * self.offset_size;
        let off = be(&self.data[at..at + self.offset_size]) as usize;
        if off < 8 || off >= self.table {
            return Err("object offset out of range");
        }
        Ok(off)
    }

    fn bytes(&self, at: usize, len: usize) -> Result<&[u8], &'static str> {
        at.checked_add(len)
            .filter(|&end| end <= self.table)
            .map(|end| &self.data[at..end])
            .ok_or("object extends past the data")
    }

    /// Count of a container/string: the marker's low nibble, or an int
    /// object after it. Returns (count, position of the payload).
    fn count_at(&self, pos: usize, nibble: u8) -> Result<(usize, usize), &'static str> {
        if nibble != 0x0f {
            return Ok((nibble as usize, pos + 1));
        }
        let marker = *self.bytes(pos + 1, 1)?.first().expect("1 byte");
        if marker >> 4 != 0x1 || marker & 0x0f > 3 {
            return Err("bad length");
        }
        let n = 1usize << (marker & 0x0f);
        let v = be(self.bytes(pos + 2, n)?);
        if v > self.data.len() as u64 {
            return Err("length too large");
        }
        Ok((v as usize, pos + 2 + n))
    }

    fn object(&mut self, index: usize, depth: usize) -> Result<Value, &'static str> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err("too many objects");
        }
        if depth > MAX_DEPTH {
            return Err("nested too deeply");
        }
        let pos = self.offset(index)?;
        let marker = self.data[pos];
        let nibble = marker & 0x0f;
        Ok(match marker >> 4 {
            0x0 => match marker {
                0x00 => Value::Null,
                0x08 => Value::Bool(false),
                0x09 => Value::Bool(true),
                _ => return Err("unknown marker"),
            },
            0x1 => {
                if nibble > 4 {
                    return Err("bad integer size");
                }
                let n = 1usize << nibble;
                let b = self.bytes(pos + 1, n)?;
                Value::Int(match n {
                    1 | 2 | 4 => be(b) as i128,
                    8 => be(b) as i64 as i128,
                    _ => i128::from_be_bytes(b.try_into().expect("16 bytes")),
                })
            }
            0x2 => match nibble {
                2 => Value::Real(
                    f32::from_be_bytes(self.bytes(pos + 1, 4)?.try_into().expect("4")) as f64,
                ),
                3 => Value::Real(f64::from_be_bytes(
                    self.bytes(pos + 1, 8)?.try_into().expect("8"),
                )),
                _ => return Err("bad real size"),
            },
            0x3 if marker == 0x33 => Value::Date(f64::from_be_bytes(
                self.bytes(pos + 1, 8)?.try_into().expect("8"),
            )),
            0x4 => {
                let (n, at) = self.count_at(pos, nibble)?;
                Value::Data(self.bytes(at, n)?.to_vec())
            }
            0x5 => {
                let (n, at) = self.count_at(pos, nibble)?;
                Value::String(String::from_utf8_lossy(self.bytes(at, n)?).into_owned())
            }
            0x6 => {
                let (n, at) = self.count_at(pos, nibble)?;
                let b = self.bytes(at, n.checked_mul(2).ok_or("length too large")?)?;
                let units: Vec<u16> = b
                    .chunks_exact(2)
                    .map(|c| u16::from_be_bytes([c[0], c[1]]))
                    .collect();
                Value::String(String::from_utf16_lossy(&units))
            }
            0x8 => Value::Uid(be(self.bytes(pos + 1, nibble as usize + 1)?)),
            0xa => {
                let (n, at) = self.count_at(pos, nibble)?;
                let refs = self.refs(at, n)?;
                let mut out = Vec::with_capacity(n.min(1024));
                for r in refs {
                    out.push(self.object(r, depth + 1)?);
                }
                Value::Array(out)
            }
            0xd => {
                let (n, at) = self.count_at(pos, nibble)?;
                let refs = self.refs(at, n.checked_mul(2).ok_or("length too large")?)?;
                let mut out = Vec::with_capacity(n.min(1024));
                for i in 0..n {
                    let Value::String(key) = self.object(refs[i], depth + 1)? else {
                        return Err("dictionary key is not a string");
                    };
                    out.push((key, self.object(refs[n + i], depth + 1)?));
                }
                Value::Dict(out)
            }
            _ => return Err("unknown marker"),
        })
    }

    fn refs(&self, at: usize, n: usize) -> Result<Vec<usize>, &'static str> {
        let b = self.bytes(at, n.checked_mul(self.ref_size).ok_or("too many refs")?)?;
        Ok(b.chunks_exact(self.ref_size)
            .map(|c| be(c) as usize)
            .collect())
    }
}

/// Parses a binary plist.
pub fn parse(data: &[u8]) -> Result<Value, &'static str> {
    if data.len() < 8 + 32 || !data.starts_with(b"bplist0") {
        return Err("not a binary plist");
    }
    let t = &data[data.len() - 32..];
    let offset_size = t[6] as usize;
    let ref_size = t[7] as usize;
    let count = be(&t[8..16]);
    let top = be(&t[16..24]);
    let table = be(&t[24..32]);
    if !(1..=8).contains(&offset_size) || !(1..=8).contains(&ref_size) {
        return Err("bad trailer");
    }
    let end = (data.len() - 32) as u64;
    if count == 0
        || top >= count
        || table < 9
        || table > end
        || count > (end - table) / offset_size as u64
    {
        return Err("bad offset table");
    }
    let mut r = Reader {
        data,
        offset_size,
        ref_size,
        count: count as usize,
        table: table as usize,
        nodes: 0,
    };
    r.object(top as usize, 0)
}

// ---- writing -----------------------------------------------------------------

fn count(v: &Value) -> usize {
    match v {
        Value::Array(a) => 1 + a.iter().map(count).sum::<usize>(),
        Value::Dict(d) => 1 + d.iter().map(|(_, v)| 1 + count(v)).sum::<usize>(),
        _ => 1,
    }
}

struct Writer {
    objects: Vec<Vec<u8>>,
    ref_size: usize,
}

fn header(out: &mut Vec<u8>, kind: u8, n: usize) {
    if n < 15 {
        out.push(kind << 4 | n as u8);
    } else {
        out.push(kind << 4 | 0x0f);
        int(out, n as i128);
    }
}

fn int(out: &mut Vec<u8>, v: i128) {
    if (0..=0xff).contains(&v) {
        out.push(0x10);
        out.push(v as u8);
    } else if (0..=0xffff).contains(&v) {
        out.push(0x11);
        out.extend_from_slice(&(v as u16).to_be_bytes());
    } else if (0..=0xffff_ffff).contains(&v) {
        out.push(0x12);
        out.extend_from_slice(&(v as u32).to_be_bytes());
    } else if (i64::MIN as i128..=i64::MAX as i128).contains(&v) {
        out.push(0x13);
        out.extend_from_slice(&(v as i64).to_be_bytes());
    } else {
        out.push(0x14);
        out.extend_from_slice(&v.to_be_bytes());
    }
}

impl Writer {
    fn add(&mut self, v: &Value) -> usize {
        let index = self.objects.len();
        self.objects.push(Vec::new());
        let mut out = Vec::new();
        match v {
            Value::Null => out.push(0x00),
            Value::Bool(b) => out.push(if *b { 0x09 } else { 0x08 }),
            Value::Int(i) => int(&mut out, *i),
            Value::Real(f) => {
                out.push(0x23);
                out.extend_from_slice(&f.to_be_bytes());
            }
            Value::Date(f) => {
                out.push(0x33);
                out.extend_from_slice(&f.to_be_bytes());
            }
            Value::Data(d) => {
                header(&mut out, 0x4, d.len());
                out.extend_from_slice(d);
            }
            Value::String(s) if s.is_ascii() => {
                header(&mut out, 0x5, s.len());
                out.extend_from_slice(s.as_bytes());
            }
            Value::String(s) => {
                let units: Vec<u16> = s.encode_utf16().collect();
                header(&mut out, 0x6, units.len());
                for u in units {
                    out.extend_from_slice(&u.to_be_bytes());
                }
            }
            Value::Uid(u) => {
                let b = u.to_be_bytes();
                let skip = (b.iter().take_while(|&&x| x == 0).count()).min(7);
                out.push(0x80 | (7 - skip) as u8);
                out.extend_from_slice(&b[skip..]);
            }
            Value::Array(a) => {
                let refs: Vec<usize> = a.iter().map(|v| self.add(v)).collect();
                header(&mut out, 0xa, refs.len());
                self.put_refs(&mut out, &refs);
            }
            Value::Dict(d) => {
                let mut keys = Vec::with_capacity(d.len());
                let mut values = Vec::with_capacity(d.len());
                for (k, v) in d {
                    keys.push(self.add(&Value::String(k.clone())));
                    values.push(self.add(v));
                }
                header(&mut out, 0xd, d.len());
                self.put_refs(&mut out, &keys);
                self.put_refs(&mut out, &values);
            }
        }
        self.objects[index] = out;
        index
    }

    fn put_refs(&self, out: &mut Vec<u8>, refs: &[usize]) {
        for &r in refs {
            out.extend_from_slice(&(r as u64).to_be_bytes()[8 - self.ref_size..]);
        }
    }
}

fn int_size(max: u64) -> usize {
    match max {
        0..=0xff => 1,
        0x100..=0xffff => 2,
        0x1_0000..=0xffff_ffff => 4,
        _ => 8,
    }
}

/// Serializes a value as a binary plist.
pub fn write(v: &Value) -> Vec<u8> {
    let n = count(v);
    let mut w = Writer {
        objects: Vec::with_capacity(n),
        ref_size: int_size(n as u64),
    };
    w.add(v);
    let mut out = b"bplist00".to_vec();
    let mut offsets = Vec::with_capacity(n);
    for o in &w.objects {
        offsets.push(out.len() as u64);
        out.extend_from_slice(o);
    }
    let table = out.len() as u64;
    let offset_size = int_size(table);
    for o in offsets {
        out.extend_from_slice(&o.to_be_bytes()[8 - offset_size..]);
    }
    out.extend_from_slice(&[0; 6]);
    out.push(offset_size as u8);
    out.push(w.ref_size as u8);
    out.extend_from_slice(&(n as u64).to_be_bytes());
    out.extend_from_slice(&0u64.to_be_bytes());
    out.extend_from_slice(&table.to_be_bytes());
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Value {
        Value::dict([
            ("name", Value::String("Beatlumen".into())),
            ("unicode", Value::String("Wohnzimmer – Küche".into())),
            ("features", Value::Int(0x1C340405F4A00)),
            ("negative", Value::Int(-5)),
            ("big", Value::Int(u64::MAX as i128)),
            ("small", Value::Int(7)),
            ("volume", Value::Real(-15.5)),
            ("flag", Value::Bool(true)),
            ("none", Value::Null),
            ("when", Value::Date(1.5)),
            ("uid", Value::Uid(300)),
            ("key", Value::Data((0..40).collect())),
            (
                "streams",
                Value::Array(vec![
                    Value::dict([("type", Value::Int(103)), ("ct", Value::Int(4))]),
                    Value::Array(vec![]),
                ]),
            ),
        ])
    }

    #[test]
    fn round_trip() {
        let v = sample();
        let bytes = write(&v);
        assert!(bytes.starts_with(b"bplist00"));
        assert_eq!(parse(&bytes).unwrap(), v);
        let streams = v.get("streams").unwrap().as_array().unwrap();
        assert_eq!(streams[0].get("type").unwrap().as_u64(), Some(103));
        assert_eq!(v.get("big").unwrap().as_u64(), Some(u64::MAX));
        assert_eq!(v.get("volume").unwrap().as_f64(), Some(-15.5));
    }

    #[test]
    fn many_objects_use_wider_references() {
        let v = Value::Array((0..300).map(|i| Value::Int(i * 1000)).collect());
        let bytes = write(&v);
        assert_eq!(bytes[bytes.len() - 25], 2);
        assert_eq!(parse(&bytes).unwrap(), v);
    }

    #[test]
    fn reads_plists_written_by_other_tools() {
        // {"qualifier": ["txtAirPlay"]} as Python's plistlib writes it.
        let bytes = [
            0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd1, 0x01, 0x02, 0x59, 0x71, 0x75,
            0x61, 0x6c, 0x69, 0x66, 0x69, 0x65, 0x72, 0xa1, 0x03, 0x5a, 0x74, 0x78, 0x74, 0x41,
            0x69, 0x72, 0x50, 0x6c, 0x61, 0x79, 0x08, 0x0b, 0x15, 0x17, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x04, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x22,
        ];
        let v = parse(&bytes).unwrap();
        let q = v.get("qualifier").unwrap().as_array().unwrap();
        assert_eq!(q[0].as_str(), Some("txtAirPlay"));
        // {"list": [1.5, b"ab", True], "networkTimeFrac": 0xF000000000000000,
        //  "networkTimeSecs": 1700000000, "rate": 1, "rtpTime": 123456}
        let bytes = [
            0x62, 0x70, 0x6c, 0x69, 0x73, 0x74, 0x30, 0x30, 0xd5, 0x01, 0x02, 0x03, 0x04, 0x05,
            0x06, 0x0a, 0x0b, 0x0c, 0x0d, 0x54, 0x6c, 0x69, 0x73, 0x74, 0x5f, 0x10, 0x0f, 0x6e,
            0x65, 0x74, 0x77, 0x6f, 0x72, 0x6b, 0x54, 0x69, 0x6d, 0x65, 0x46, 0x72, 0x61, 0x63,
            0x5f, 0x10, 0x0f, 0x6e, 0x65, 0x74, 0x77, 0x6f, 0x72, 0x6b, 0x54, 0x69, 0x6d, 0x65,
            0x53, 0x65, 0x63, 0x73, 0x54, 0x72, 0x61, 0x74, 0x65, 0x57, 0x72, 0x74, 0x70, 0x54,
            0x69, 0x6d, 0x65, 0xa3, 0x07, 0x08, 0x09, 0x23, 0x3f, 0xf8, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x42, 0x61, 0x62, 0x09, 0x14, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0xf0, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x12, 0x65, 0x53, 0xf1, 0x00,
            0x10, 0x01, 0x12, 0x00, 0x01, 0xe2, 0x40, 0x08, 0x13, 0x18, 0x2a, 0x3c, 0x41, 0x49,
            0x4d, 0x56, 0x59, 0x5a, 0x6b, 0x70, 0x72, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01,
            0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x0e, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x77,
        ];
        let v = parse(&bytes).unwrap();
        assert_eq!(v.get("rate").unwrap().as_u64(), Some(1));
        assert_eq!(v.get("rtpTime").unwrap().as_u64(), Some(123456));
        assert_eq!(
            v.get("networkTimeFrac").unwrap().as_u64(),
            Some(0xF000000000000000)
        );
        assert_eq!(
            v.get("list").unwrap(),
            &Value::Array(vec![
                Value::Real(1.5),
                Value::Data(b"ab".to_vec()),
                Value::Bool(true)
            ])
        );
    }

    #[test]
    fn malformed_input_is_rejected() {
        let good = write(&sample());
        // Every truncation fails cleanly.
        for len in 0..good.len() {
            let _ = parse(&good[..len]);
        }
        // Every single-byte corruption fails cleanly (or parses).
        for i in 0..good.len() {
            let mut bad = good.clone();
            bad[i] ^= 0xff;
            let _ = parse(&bad);
        }
        assert!(parse(b"bplist00").is_err());
        // An array containing itself.
        let mut cyclic = b"bplist00".to_vec();
        cyclic.extend_from_slice(&[0xa1, 0x00]);
        cyclic.push(8); // offset table: object 0 at 8
        cyclic.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        cyclic.extend_from_slice(&1u64.to_be_bytes());
        cyclic.extend_from_slice(&0u64.to_be_bytes());
        cyclic.extend_from_slice(&10u64.to_be_bytes());
        assert_eq!(parse(&cyclic), Err("nested too deeply"));
        // Many references to one big array: limited by the node budget.
        let mut wide = b"bplist00".to_vec();
        wide.push(0xaf);
        wide.extend_from_slice(&[0x11, 0x01, 0x00]); // 256 entries
        wide.extend(std::iter::repeat(0u8).take(256)); // all refer to object 0
        let table = wide.len() as u64;
        wide.push(8);
        wide.extend_from_slice(&[0, 0, 0, 0, 0, 0, 1, 1]);
        wide.extend_from_slice(&1u64.to_be_bytes());
        wide.extend_from_slice(&0u64.to_be_bytes());
        wide.extend_from_slice(&table.to_be_bytes());
        assert!(parse(&wide).is_err());
    }
}
