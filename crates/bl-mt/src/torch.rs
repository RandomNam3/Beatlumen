//! Reading PyTorch checkpoints (`pytorch_model.bin`) without Python.
//!
//! Two layouts exist:
//!
//! * The legacy format (torch before 1.6, and what the OPUS-MT models on
//!   Hugging Face use): five pickles one after another (a magic number,
//!   the protocol version, information about the writing system, the
//!   state dict and the list of storage keys in file order), then each
//!   storage as a little-endian u64 element count followed by its raw
//!   little-endian values.
//! * The zip format (torch 1.6 and later): a zip archive with the state
//!   dict pickle in `<name>/data.pkl` and each storage as raw values in
//!   `<name>/data/<key>`. Only stored (uncompressed) entries are read,
//!   which is what torch writes; checksums are not checked.
//!
//! The pickles are read by the safe reader in [`crate::pickle`]. The
//! tensors are only described when the file is opened; their values are
//! read afterwards for the tensors asked for, one storage at a time.

use crate::pickle::{self, DType, TensorRef, Value};
use bl_asr::model::{Store, Tensor};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Largest pickle read (bytes).
const MAX_PICKLE: u64 = 64 << 20;
/// Largest zip central directory read (bytes).
const MAX_DIRECTORY: u64 = 64 << 20;
/// Most entries of a zip archive.
const MAX_ENTRIES: u64 = 1 << 20;
/// Magic number at the start of a legacy checkpoint.
const LEGACY_MAGIC: [u8; 10] = [0x6c, 0xfc, 0x9c, 0x46, 0xf9, 0x20, 0x6a, 0xa8, 0x50, 0x19];

/// How a tensor's values are kept after loading.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Keep {
    F32,
    /// IEEE half floats: half the memory of f32 (for weight matrices).
    F16,
}

enum Layout {
    /// Storage keys in file order; their data starts at `data_start`.
    Legacy { data_start: u64, keys: Vec<String> },
    /// Zip entries by name: (offset of the local header, size).
    Zip {
        prefix: String,
        entries: HashMap<String, (u64, u64)>,
    },
}

/// An opened checkpoint: the names and shapes of its tensors.
pub struct Checkpoint {
    path: PathBuf,
    len: u64,
    layout: Layout,
    tensors: Vec<(String, TensorRef)>,
}

fn io_err(e: std::io::Error) -> String {
    format!("checkpoint: {e}")
}

impl Checkpoint {
    /// Opens a checkpoint and reads the description of its tensors.
    pub fn open(path: impl AsRef<Path>) -> Result<Checkpoint, String> {
        let path = path.as_ref().to_path_buf();
        let mut f = File::open(&path).map_err(io_err)?;
        let len = f.metadata().map_err(io_err)?.len();
        let mut head = [0u8; 4];
        f.read_exact(&mut head).map_err(io_err)?;
        f.seek(SeekFrom::Start(0)).map_err(io_err)?;
        let (layout, dict) = if head == *b"PK\x03\x04" {
            open_zip(&mut f, len)?
        } else {
            open_legacy(f)?
        };
        let tensors = state_dict(dict)?;
        Ok(Checkpoint {
            path,
            len,
            layout,
            tensors,
        })
    }

    /// The tensors in the file (name and description), in file order.
    pub fn tensors(&self) -> &[(String, TensorRef)] {
        &self.tensors
    }

    pub fn get(&self, name: &str) -> Option<&TensorRef> {
        self.tensors.iter().find(|(n, _)| n == name).map(|(_, t)| t)
    }

    /// Reads the tensors named in `wanted`, kept as asked. A tensor of
    /// shape `[a, b, ..., n]` becomes `a * b * ...` rows of `n` values (a
    /// vector one row). Fails if one is missing.
    pub fn load(&self, wanted: &[(&str, Keep)]) -> Result<HashMap<String, Tensor>, String> {
        // Tensors to make, by storage key.
        let mut by_key: HashMap<&str, Vec<(&str, &TensorRef, Keep)>> = HashMap::new();
        for &(name, keep) in wanted {
            let t = self
                .get(name)
                .ok_or_else(|| format!("checkpoint: no tensor {name}"))?;
            by_key
                .entry(t.storage.key.as_str())
                .or_default()
                .push((name, t, keep));
        }
        let mut out = HashMap::new();
        let mut f = BufReader::new(File::open(&self.path).map_err(io_err)?);
        match &self.layout {
            Layout::Legacy { data_start, keys } => {
                f.seek(SeekFrom::Start(*data_start)).map_err(io_err)?;
                let mut pos = *data_start;
                for key in keys {
                    let mut c = [0u8; 8];
                    f.read_exact(&mut c).map_err(io_err)?;
                    let count = u64::from_le_bytes(c);
                    pos += 8;
                    let users = by_key.remove(key.as_str());
                    let dtype = match users.as_ref().and_then(|u| u.first()) {
                        Some((_, t, _)) => t.storage.dtype,
                        // Not needed: its element size is taken from any
                        // tensor using it (f32 if none does).
                        None => self
                            .tensors
                            .iter()
                            .find(|(_, t)| t.storage.key == *key)
                            .map_or(DType::F32, |(_, t)| t.storage.dtype),
                    };
                    let bytes = count
                        .checked_mul(dtype.size() as u64)
                        .filter(|&b| b <= self.len.saturating_sub(pos))
                        .ok_or("checkpoint: storage beyond the end of the file")?;
                    match users {
                        None => {
                            let skip = i64::try_from(bytes).map_err(|_| "checkpoint: too large")?;
                            f.seek_relative(skip).map_err(io_err)?;
                        }
                        Some(users) => {
                            let data = read_exact(&mut f, bytes)?;
                            make(&users, count, &data, &mut out)?;
                        }
                    }
                    pos += bytes;
                }
            }
            Layout::Zip { prefix, entries } => {
                for (key, users) in &by_key {
                    let name = format!("{prefix}data/{key}");
                    let data = zip_entry(&mut f, self.len, entries, &name, u64::MAX)?;
                    let dtype = users[0].1.storage.dtype;
                    if data.len() % dtype.size() != 0 {
                        return Err("checkpoint: storage of a partial element".into());
                    }
                    let count = (data.len() / dtype.size()) as u64;
                    make(users, count, &data, &mut out)?;
                }
                by_key.clear();
            }
        }
        if let Some((_, users)) = by_key.into_iter().next() {
            return Err(format!("checkpoint: no data for {}", users[0].0));
        }
        Ok(out)
    }
}

/// Reads `n` bytes (`n` was checked against the file size).
fn read_exact(f: &mut dyn Read, n: u64) -> Result<Vec<u8>, String> {
    let n = usize::try_from(n).map_err(|_| "checkpoint: storage too large")?;
    let mut data = vec![0u8; n];
    f.read_exact(&mut data).map_err(io_err)?;
    Ok(data)
}

/// Makes the tensors `users` from their storage's raw values `data`
/// (`count` elements).
fn make(
    users: &[(&str, &TensorRef, Keep)],
    count: u64,
    data: &[u8],
    out: &mut HashMap<String, Tensor>,
) -> Result<(), String> {
    for &(name, t, keep) in users {
        if t.storage.count != count {
            return Err(format!("checkpoint: storage size of {name} differs"));
        }
        let dtype = t.storage.dtype;
        if users.iter().any(|(_, u, _)| u.storage.dtype != dtype) {
            return Err("checkpoint: storage used with two types".into());
        }
        let numel = t.numel();
        // Checked against the storage size when the pickle was read.
        let start = (t.offset as usize) * dtype.size();
        let end = start + numel as usize * dtype.size();
        let raw = data
            .get(start..end)
            .ok_or("checkpoint: tensor outside its storage")?;
        let (rows, cols) = match t.shape.split_last() {
            None => (1, 1),
            Some((&cols, rest)) => (rest.iter().product::<usize>(), cols),
        };
        let store = convert(raw, dtype, keep);
        let tensor = Tensor::new(cols, rows, store).ok_or("checkpoint: bad tensor size")?;
        out.insert(name.to_string(), tensor);
    }
    Ok(())
}

/// Raw little-endian values of type `dtype` kept as `keep`.
fn convert(raw: &[u8], dtype: DType, keep: Keep) -> Store {
    let u16s = || {
        raw.chunks_exact(2)
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
    };
    let f32s = || {
        raw.chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let bf16 = |h: u16| f32::from_bits((h as u32) << 16);
    match (dtype, keep) {
        (DType::F32, Keep::F32) => Store::F32(f32s().collect()),
        (DType::F32, Keep::F16) => Store::F16(f32s().map(f32_to_f16).collect()),
        (DType::F16, Keep::F32) => Store::F32(u16s().map(bl_asr::model::f16_to_f32).collect()),
        (DType::F16, Keep::F16) => Store::F16(u16s().collect()),
        (DType::BF16, Keep::F32) => Store::F32(u16s().map(bf16).collect()),
        (DType::BF16, Keep::F16) => Store::F16(u16s().map(|h| f32_to_f16(bf16(h))).collect()),
    }
}

/// f32 to IEEE half, rounding to nearest (ties to even); too large values
/// become infinity.
pub fn f32_to_f16(v: f32) -> u16 {
    let x = v.to_bits();
    let sign = ((x >> 16) & 0x8000) as u16;
    let exp = ((x >> 23) & 0xff) as i32;
    let man = x & 0x7f_ffff;
    if exp == 0xff {
        let nan = if man != 0 {
            0x200 | (man >> 13) as u16
        } else {
            0
        };
        return sign | 0x7c00 | nan;
    }
    let e = exp - 127 + 15;
    if e >= 0x1f {
        return sign | 0x7c00;
    }
    let round = |half: u32, rem: u32, mid: u32| {
        if rem > mid || (rem == mid && half & 1 == 1) {
            half + 1
        } else {
            half
        }
    };
    if e <= 0 {
        // Subnormal half (or zero).
        if e < -10 {
            return sign;
        }
        let m = man | 0x80_0000;
        let shift = (14 - e) as u32;
        let half = m >> shift;
        let r = round(half, m & ((1 << shift) - 1), 1 << (shift - 1));
        return sign | r as u16;
    }
    // A carry out of the fraction raises the exponent (up to infinity).
    let half = ((e as u32) << 10) | (man >> 13);
    sign | round(half, man & 0x1fff, 0x1000) as u16
}

/// The tensors of a state dict pickle (entries that are not tensors are
/// left out).
fn state_dict(v: Value) -> Result<Vec<(String, TensorRef)>, String> {
    let Value::Dict(items) = v else {
        return Err("checkpoint: the state dict is not a dict".into());
    };
    let mut out = Vec::with_capacity(items.len());
    for (k, v) in items {
        if let (Value::Str(name), Value::Tensor(t)) = (k, v) {
            out.push((name, t));
        }
    }
    Ok(out)
}

/// Reads the five pickles of a legacy checkpoint.
fn open_legacy(f: File) -> Result<(Layout, Value), String> {
    let mut r = BufReader::new(f).take(MAX_PICKLE);
    let magic = pickle::read(&mut r)?;
    if magic != Value::BigInt(LEGACY_MAGIC.to_vec()) {
        return Err("not a PyTorch checkpoint".into());
    }
    if pickle::read(&mut r)? != Value::Int(1001) {
        return Err("checkpoint: unknown format version".into());
    }
    let Value::Dict(info) = pickle::read(&mut r)? else {
        return Err("checkpoint: bad system information".into());
    };
    let little = info
        .iter()
        .find(|(k, _)| k.as_str() == Some("little_endian"))
        .map(|(_, v)| v.clone());
    if little != Some(Value::Bool(true)) {
        return Err("checkpoint: not written in little-endian byte order".into());
    }
    let dict = pickle::read(&mut r)?;
    let Value::List(keys) = pickle::read(&mut r)? else {
        return Err("checkpoint: bad storage list".into());
    };
    let keys = keys
        .into_iter()
        .map(|k| match k {
            Value::Str(s) => Ok(s),
            _ => Err("checkpoint: bad storage key".to_string()),
        })
        .collect::<Result<Vec<_>, _>>()?;
    let data_start = MAX_PICKLE - r.limit();
    Ok((Layout::Legacy { data_start, keys }, dict))
}

fn u16_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u16::from_le_bytes(b.get(at..at + 2)?.try_into().ok()?) as u64)
}

fn u32_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u32::from_le_bytes(b.get(at..at + 4)?.try_into().ok()?) as u64)
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

fn read_at(f: &mut File, at: u64, n: u64) -> Result<Vec<u8>, String> {
    f.seek(SeekFrom::Start(at)).map_err(io_err)?;
    read_exact(f, n)
}

/// Reads the central directory of a zip checkpoint and its state dict.
fn open_zip(f: &mut File, len: u64) -> Result<(Layout, Value), String> {
    let bad = || "checkpoint: broken zip archive".to_string();
    // The end of central directory record: within the last 64 KiB + 22
    // bytes (its comment is at most 64 KiB).
    let tail_len = len.min(65_557);
    let tail = read_at(f, len - tail_len, tail_len)?;
    let at = (0..tail.len().saturating_sub(21))
        .rev()
        .find(|&i| tail[i..i + 4] == *b"PK\x05\x06")
        .ok_or_else(bad)?;
    let eocd = &tail[at..];
    let mut entries = u16_at(eocd, 10).ok_or_else(bad)?;
    let mut dir_size = u32_at(eocd, 12).ok_or_else(bad)?;
    let mut dir_at = u32_at(eocd, 16).ok_or_else(bad)?;
    if entries == 0xffff || dir_size == 0xffff_ffff || dir_at == 0xffff_ffff {
        // Zip64: the locator just before, pointing at the zip64 record.
        let loc = at.checked_sub(20).map(|l| &tail[l..at]).ok_or_else(bad)?;
        if loc[..4] != *b"PK\x06\x07" {
            return Err(bad());
        }
        let rec_at = u64_at(loc, 8).ok_or_else(bad)?;
        if rec_at > len.saturating_sub(56) {
            return Err(bad());
        }
        let rec = read_at(f, rec_at, 56)?;
        if rec[..4] != *b"PK\x06\x06" {
            return Err(bad());
        }
        entries = u64_at(&rec, 32).ok_or_else(bad)?;
        dir_size = u64_at(&rec, 40).ok_or_else(bad)?;
        dir_at = u64_at(&rec, 48).ok_or_else(bad)?;
    }
    if entries > MAX_ENTRIES
        || dir_size > MAX_DIRECTORY
        || !matches!(dir_at.checked_add(dir_size), Some(e) if e <= len)
    {
        return Err(bad());
    }
    let dir = read_at(f, dir_at, dir_size)?;
    let mut map = HashMap::new();
    let mut p = 0usize;
    for _ in 0..entries {
        let h = dir.get(p..p + 46).ok_or_else(bad)?;
        if h[..4] != *b"PK\x01\x02" {
            return Err(bad());
        }
        let method = u16_at(h, 10).ok_or_else(bad)?;
        let mut packed = u32_at(h, 20).ok_or_else(bad)?;
        let mut size = u32_at(h, 24).ok_or_else(bad)?;
        let name_len = u16_at(h, 28).ok_or_else(bad)? as usize;
        let extra_len = u16_at(h, 30).ok_or_else(bad)? as usize;
        let comment_len = u16_at(h, 32).ok_or_else(bad)? as usize;
        let mut offset = u32_at(h, 42).ok_or_else(bad)?;
        let name = dir.get(p + 46..p + 46 + name_len).ok_or_else(bad)?;
        let extra = dir
            .get(p + 46 + name_len..p + 46 + name_len + extra_len)
            .ok_or_else(bad)?;
        // Zip64 sizes and offset, for the fields that overflowed.
        let mut e = 0;
        while e + 4 <= extra.len() {
            let id = u16_at(extra, e).ok_or_else(bad)?;
            let n = u16_at(extra, e + 2).ok_or_else(bad)? as usize;
            let body = extra.get(e + 4..e + 4 + n).ok_or_else(bad)?;
            if id == 1 {
                let mut q = 0;
                for field in [&mut size, &mut packed, &mut offset] {
                    if *field == 0xffff_ffff {
                        *field = u64_at(body, q).ok_or_else(bad)?;
                        q += 8;
                    }
                }
            }
            e += 4 + n;
        }
        p += 46 + name_len + extra_len + comment_len;
        if method != 0 || packed != size {
            // Compressed entries are not used by torch; skip them.
            continue;
        }
        if let Ok(name) = std::str::from_utf8(name) {
            map.insert(name.to_string(), (offset, size));
        }
    }
    let prefix = map
        .keys()
        .filter_map(|n| n.strip_suffix("data.pkl"))
        .find(|p| p.ends_with('/') && p.matches('/').count() == 1)
        .ok_or("checkpoint: no data.pkl in the zip archive")?
        .to_string();
    let mut r = BufReader::new(&mut *f);
    if map.contains_key(&format!("{prefix}byteorder")) {
        let order = zip_entry(&mut r, len, &map, &format!("{prefix}byteorder"), 16)?;
        if order != b"little" {
            return Err("checkpoint: not written in little-endian byte order".into());
        }
    }
    let pkl = zip_entry(&mut r, len, &map, &format!("{prefix}data.pkl"), MAX_PICKLE)?;
    let dict = pickle::read_bytes(&pkl)?;
    Ok((
        Layout::Zip {
            prefix,
            entries: map,
        },
        dict,
    ))
}

/// The data of a stored zip entry (at most `max` bytes).
fn zip_entry<R: Read + Seek>(
    f: &mut R,
    len: u64,
    entries: &HashMap<String, (u64, u64)>,
    name: &str,
    max: u64,
) -> Result<Vec<u8>, String> {
    let bad = || format!("checkpoint: broken zip entry {name}");
    let &(offset, size) = entries
        .get(name)
        .ok_or_else(|| format!("checkpoint: no {name}"))?;
    if size > max || offset > len.saturating_sub(30) {
        return Err(bad());
    }
    f.seek(SeekFrom::Start(offset)).map_err(io_err)?;
    let mut h = [0u8; 30];
    f.read_exact(&mut h).map_err(io_err)?;
    if h[..4] != *b"PK\x03\x04" {
        return Err(bad());
    }
    let skip = u16_at(&h, 26).ok_or_else(bad)? + u16_at(&h, 28).ok_or_else(bad)?;
    let start = offset + 30 + skip;
    if !matches!(start.checked_add(size), Some(e) if e <= len) {
        return Err(bad());
    }
    f.seek(SeekFrom::Start(start)).map_err(io_err)?;
    read_exact(f, size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_asr::model::f16_to_f32;

    #[test]
    fn half_conversion_round_trips() {
        for h in 0..=u16::MAX {
            let v = f16_to_f32(h);
            if v.is_nan() {
                assert!(f16_to_f32(f32_to_f16(v)).is_nan());
            } else {
                assert_eq!(f32_to_f16(v), h, "{h:04x}");
            }
        }
        assert_eq!(f32_to_f16(1e6), 0x7c00);
        assert_eq!(f32_to_f16(-1e-10), 0x8000);
        // Ties to even: 1 + 2^-11 lies between 1 and the next half.
        assert_eq!(f32_to_f16(1.0 + 1.0 / 2048.0), 0x3c00);
        assert_eq!(f32_to_f16(1.0 + 3.0 / 2048.0), 0x3c02);
        // Rounding up from the largest subnormal to the smallest normal.
        assert_eq!(
            f32_to_f16(f16_to_f32(0x03ff) + 2f32.powi(-25) * 1.5),
            0x0400
        );
    }

    /// The pickle of a state dict with one 2x2 f32 tensor `w` in storage
    /// "0" and a 3-value f16 tensor `b` (from element 1) in storage "1".
    fn dict_pickle() -> Vec<u8> {
        let mut p = b"\x80\x02ccollections\nOrderedDict\n)R(".to_vec();
        let s = |p: &mut Vec<u8>, s: &str| {
            p.push(b'X');
            p.extend_from_slice(&(s.len() as u32).to_le_bytes());
            p.extend_from_slice(s.as_bytes());
        };
        let tensor = |p: &mut Vec<u8>, ty: &str, key: &str, count: u8, off: u8, size: &[u8]| {
            p.extend_from_slice(b"ctorch._utils\n_rebuild_tensor_v2\n((");
            s(p, "storage");
            p.extend_from_slice(format!("ctorch\n{ty}\n").as_bytes());
            s(p, key);
            s(p, "cpu");
            p.extend_from_slice(&[b'K', count, b'N', b't', b'Q', b'K', off, b'(']);
            for &d in size {
                p.extend_from_slice(&[b'K', d]);
            }
            p.push(b't');
            p.push(b'(');
            let mut stride = 1;
            let mut strides = Vec::new();
            for &d in size.iter().rev() {
                strides.push(stride);
                stride *= d;
            }
            for &st in strides.iter().rev() {
                p.extend_from_slice(&[b'K', st]);
            }
            p.extend_from_slice(b"t\x89ccollections\nOrderedDict\n)RtR");
        };
        s(&mut p, "w");
        tensor(&mut p, "FloatStorage", "0", 4, 0, &[2, 2]);
        s(&mut p, "b");
        tensor(&mut p, "HalfStorage", "1", 4, 1, &[3]);
        p.extend_from_slice(b"u.");
        p
    }

    fn check(c: &Checkpoint) {
        assert_eq!(c.tensors().len(), 2);
        let t = c.load(&[("w", Keep::F32), ("b", Keep::F32)]).unwrap();
        assert_eq!(t["w"].values(), &[1.0, 2.0, 3.0, 4.0]);
        assert_eq!((t["w"].rows, t["w"].cols), (2, 2));
        assert_eq!(t["b"].values(), &[1.0, -2.0, 0.5]);
        let t = c.load(&[("w", Keep::F16)]).unwrap();
        assert_eq!(&*t["w"].row(1), &[3.0, 4.0]);
        assert!(c.load(&[("x", Keep::F32)]).is_err());
    }

    fn storages() -> (Vec<u8>, Vec<u8>) {
        let w: Vec<u8> = [1f32, 2.0, 3.0, 4.0]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        let b: Vec<u8> = [0x7c00u16, 0x3c00, 0xc000, 0x3800]
            .iter()
            .flat_map(|v| v.to_le_bytes())
            .collect();
        (w, b)
    }

    fn legacy_file() -> Vec<u8> {
        let mut f = vec![0x80, 2, 0x8a, 10];
        f.extend_from_slice(&LEGACY_MAGIC);
        f.extend_from_slice(b".\x80\x02M\xe9\x03.");
        f.extend_from_slice(b"\x80\x02}(X\x0d\x00\x00\x00little_endian\x88u.");
        f.extend_from_slice(&dict_pickle());
        f.extend_from_slice(b"\x80\x02](X\x01\x00\x00\x000X\x01\x00\x00\x001e.");
        let (w, b) = storages();
        f.extend_from_slice(&4u64.to_le_bytes());
        f.extend_from_slice(&w);
        f.extend_from_slice(&4u64.to_le_bytes());
        f.extend_from_slice(&b);
        f
    }

    fn temp(name: &str, data: &[u8]) -> PathBuf {
        let p = std::env::temp_dir().join(format!("bl-mt-{}-{name}", std::process::id()));
        std::fs::write(&p, data).unwrap();
        p
    }

    #[test]
    fn reads_the_legacy_format() {
        let f = legacy_file();
        let p = temp("legacy.bin", &f);
        check(&Checkpoint::open(&p).unwrap());
        // Cut short: the description or the data is missing.
        for n in [10, 60, 200, f.len() - 1] {
            std::fs::write(&p, &f[..n]).unwrap();
            let ok = Checkpoint::open(&p).and_then(|c| c.load(&[("b", Keep::F32)]).map(|_| ()));
            assert!(ok.is_err(), "{n}");
        }
        // A storage claiming more data than the file has.
        let mut g = f.clone();
        let at = g.len() - 8 - 8;
        g[at..at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
        std::fs::write(&p, &g).unwrap();
        assert!(Checkpoint::open(&p)
            .unwrap()
            .load(&[("b", Keep::F32)])
            .is_err());
        std::fs::remove_file(&p).unwrap();
    }

    /// A zip archive of stored entries.
    fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        let mut dir = Vec::new();
        for (name, data) in files {
            let offset = out.len() as u32;
            let mut h = b"PK\x03\x04".to_vec();
            h.extend_from_slice(&[20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&3u16.to_le_bytes());
            h.extend_from_slice(name.as_bytes());
            h.extend_from_slice(b"pad");
            out.extend_from_slice(&h);
            out.extend_from_slice(data);
            let mut c = b"PK\x01\x02".to_vec();
            c.extend_from_slice(&[20, 0, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(&(data.len() as u32).to_le_bytes());
            c.extend_from_slice(&(name.len() as u16).to_le_bytes());
            c.extend_from_slice(&[0; 12]);
            c.extend_from_slice(&offset.to_le_bytes());
            c.extend_from_slice(name.as_bytes());
            dir.extend_from_slice(&c);
        }
        let dir_at = out.len() as u32;
        out.extend_from_slice(&dir);
        out.extend_from_slice(b"PK\x05\x06\0\0\0\0");
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(files.len() as u16).to_le_bytes());
        out.extend_from_slice(&(dir.len() as u32).to_le_bytes());
        out.extend_from_slice(&dir_at.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    #[test]
    fn reads_the_zip_format() {
        let (w, b) = storages();
        let pkl = dict_pickle();
        let files: [(&str, &[u8]); 4] = [
            ("archive/data.pkl", &pkl),
            ("archive/byteorder", b"little"),
            ("archive/data/0", &w),
            ("archive/data/1", &b),
        ];
        let f = zip(&files);
        let p = temp("zip.bin", &f);
        check(&Checkpoint::open(&p).unwrap());
        // Storage data missing.
        std::fs::write(&p, zip(&files[..3])).unwrap();
        let c = Checkpoint::open(&p).unwrap();
        assert!(c.load(&[("b", Keep::F32)]).is_err());
        // Big-endian files are refused.
        let files: [(&str, &[u8]); 2] = [("archive/data.pkl", &pkl), ("archive/byteorder", b"big")];
        std::fs::write(&p, zip(&files)).unwrap();
        assert!(Checkpoint::open(&p).is_err());
        // Cut short.
        std::fs::write(&p, &f[..f.len() - 30]).unwrap();
        assert!(Checkpoint::open(&p).is_err());
        std::fs::remove_file(&p).unwrap();
    }
}
