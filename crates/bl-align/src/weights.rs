//! The model's weights: a PyTorch checkpoint (`pytorch_model.bin`, read
//! by `bl_mt::torch`) or a safetensors file (`model.safetensors`, read
//! here).
//!
//! A safetensors file is an 8-byte little-endian header length, a JSON
//! header naming each tensor's type, shape and byte range, then the raw
//! little-endian values. Everything in the header is checked against the
//! file before a tensor is read.

use bl_asr::model::{f16_to_f32, Store, Tensor};
use bl_mt::torch::{f32_to_f16, Checkpoint, Keep};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// Largest safetensors header read (bytes).
const MAX_HEADER: u64 = 64 << 20;
/// Most dimensions of a tensor.
const MAX_RANK: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    F32,
    F16,
    BF16,
}

impl DType {
    fn size(self) -> usize {
        match self {
            DType::F32 => 4,
            DType::F16 | DType::BF16 => 2,
        }
    }
}

/// A tensor of a safetensors file: its type (`None` for types not
/// supported), shape and absolute byte range in the file.
#[derive(Debug, Clone, PartialEq)]
struct Entry {
    dtype: Option<DType>,
    shape: Vec<usize>,
    start: u64,
    end: u64,
}

/// An opened safetensors file: the description of its tensors.
pub struct SafeTensors {
    path: PathBuf,
    entries: HashMap<String, Entry>,
}

fn io_err(e: std::io::Error) -> String {
    format!("safetensors: {e}")
}

impl SafeTensors {
    pub fn open(path: impl AsRef<Path>) -> Result<SafeTensors, String> {
        let path = path.as_ref().to_path_buf();
        let mut f = File::open(&path).map_err(io_err)?;
        let len = f.metadata().map_err(io_err)?.len();
        let mut n = [0u8; 8];
        f.read_exact(&mut n).map_err(io_err)?;
        let header_len = u64::from_le_bytes(n);
        if header_len > MAX_HEADER || header_len > len - 8 {
            return Err("safetensors: bad header length".into());
        }
        let mut header = vec![0u8; header_len as usize];
        f.read_exact(&mut header).map_err(io_err)?;
        let entries = parse_header(&header, 8 + header_len, len)?;
        Ok(SafeTensors { path, entries })
    }

    pub fn shape(&self, name: &str) -> Option<&[usize]> {
        self.entries.get(name).map(|e| e.shape.as_slice())
    }

    /// Reads the tensors named in `wanted`, kept as asked (shapes as in
    /// `bl_mt::torch::Checkpoint::load`). Fails if one is missing.
    pub fn load(&self, wanted: &[(&str, Keep)]) -> Result<HashMap<String, Tensor>, String> {
        let mut f = File::open(&self.path).map_err(io_err)?;
        let mut out = HashMap::new();
        for &(name, keep) in wanted {
            let e = self
                .entries
                .get(name)
                .ok_or_else(|| format!("safetensors: no tensor {name}"))?;
            let dtype = e
                .dtype
                .ok_or_else(|| format!("safetensors: unsupported type of {name}"))?;
            f.seek(SeekFrom::Start(e.start)).map_err(io_err)?;
            let n = usize::try_from(e.end - e.start).map_err(|_| "safetensors: too large")?;
            let mut raw = vec![0u8; n];
            f.read_exact(&mut raw).map_err(io_err)?;
            let (rows, cols) = match e.shape.split_last() {
                None => (1, 1),
                Some((&cols, rest)) => (rest.iter().product::<usize>(), cols),
            };
            let store = convert(&raw, dtype, keep);
            let t = Tensor::new(cols, rows, store).ok_or("safetensors: bad tensor size")?;
            out.insert(name.to_string(), t);
        }
        Ok(out)
    }
}

/// The entries of a header whose data starts at `data_start` in a file of
/// `len` bytes.
fn parse_header(
    header: &[u8],
    data_start: u64,
    len: u64,
) -> Result<HashMap<String, Entry>, String> {
    let text = std::str::from_utf8(header).map_err(|_| "safetensors: header not UTF-8")?;
    let v = bl_json::parse(text).map_err(|e| format!("safetensors: {e}"))?;
    let map = v.as_object().ok_or("safetensors: header not an object")?;
    let data_len = len - data_start;
    let mut entries = HashMap::new();
    for (name, t) in map.iter() {
        if name == "__metadata__" {
            continue;
        }
        let bad = || format!("safetensors: bad entry {name}");
        let dtype = match t.get("dtype").as_str().ok_or_else(bad)? {
            "F32" => Some(DType::F32),
            "F16" => Some(DType::F16),
            "BF16" => Some(DType::BF16),
            _ => None,
        };
        let shape = t
            .get("shape")
            .as_array()
            .filter(|s| s.len() <= MAX_RANK)
            .ok_or_else(bad)?
            .iter()
            .map(|d| d.as_u64().and_then(|d| usize::try_from(d).ok()))
            .collect::<Option<Vec<usize>>>()
            .ok_or_else(bad)?;
        let offsets = t.get("data_offsets").as_array().ok_or_else(bad)?;
        let (begin, end) = match offsets.as_slice() {
            [a, b] => (a.as_u64().ok_or_else(bad)?, b.as_u64().ok_or_else(bad)?),
            _ => return Err(bad()),
        };
        if begin > end || end > data_len {
            return Err(format!("safetensors: {name} outside the file"));
        }
        if let Some(dt) = dtype {
            let bytes = shape
                .iter()
                .try_fold(dt.size() as u64, |n, &d| n.checked_mul(d as u64));
            if bytes != Some(end - begin) {
                return Err(format!(
                    "safetensors: size of {name} differs from its shape"
                ));
            }
        }
        entries.insert(
            name.to_string(),
            Entry {
                dtype,
                shape,
                start: data_start + begin,
                end: data_start + end,
            },
        );
    }
    Ok(entries)
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
        (DType::F16, Keep::F32) => Store::F32(u16s().map(f16_to_f32).collect()),
        (DType::F16, Keep::F16) => Store::F16(u16s().collect()),
        (DType::BF16, Keep::F32) => Store::F32(u16s().map(bf16).collect()),
        (DType::BF16, Keep::F16) => Store::F16(u16s().map(|h| f32_to_f16(bf16(h))).collect()),
    }
}

/// The weights of a model directory.
pub enum Weights {
    Torch(Checkpoint),
    Safe(SafeTensors),
}

impl Weights {
    /// Opens `pytorch_model.bin` or, if there is none, `model.safetensors`.
    pub fn open(dir: &Path) -> Result<Weights, String> {
        let bin = dir.join("pytorch_model.bin");
        if bin.is_file() {
            return Checkpoint::open(bin).map(Weights::Torch);
        }
        let safe = dir.join("model.safetensors");
        if safe.is_file() {
            return SafeTensors::open(safe).map(Weights::Safe);
        }
        Err("no pytorch_model.bin or model.safetensors".into())
    }

    /// The shape of a tensor, if the file has it.
    pub fn shape(&self, name: &str) -> Option<Vec<usize>> {
        match self {
            Weights::Torch(c) => c.get(name).map(|t| t.shape.clone()),
            Weights::Safe(s) => s.shape(name).map(<[usize]>::to_vec),
        }
    }

    pub fn load(&self, wanted: &[(&str, Keep)]) -> Result<HashMap<String, Tensor>, String> {
        match self {
            Weights::Torch(c) => c.load(wanted),
            Weights::Safe(s) => s.load(wanted),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A safetensors file of the given header and data.
    fn file(header: &str, data: &[u8]) -> Vec<u8> {
        let mut out = (header.len() as u64).to_le_bytes().to_vec();
        out.extend_from_slice(header.as_bytes());
        out.extend_from_slice(data);
        out
    }

    #[test]
    fn reads_safetensors() {
        let mut data = Vec::new();
        for v in [1.0f32, -2.0, 0.5, 3.0, 4.0, 5.0] {
            data.extend_from_slice(&v.to_le_bytes());
        }
        // 1.5 as f16 and as bf16.
        data.extend_from_slice(&0x3e00u16.to_le_bytes());
        data.extend_from_slice(&0x3fc0u16.to_le_bytes());
        let header = r#"{"__metadata__": {"format": "pt"},
            "w": {"dtype": "F32", "shape": [2, 3], "data_offsets": [0, 24]},
            "h": {"dtype": "F16", "shape": [1], "data_offsets": [24, 26]},
            "b": {"dtype": "BF16", "shape": [], "data_offsets": [26, 28]},
            "i": {"dtype": "I64", "shape": [0], "data_offsets": [28, 28]}}"#;
        let dir = std::env::temp_dir().join(format!("bl-align-st-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.safetensors");
        std::fs::write(&path, file(header, &data)).unwrap();
        let st = SafeTensors::open(&path).unwrap();
        assert_eq!(st.shape("w"), Some(&[2, 3][..]));
        let t = st
            .load(&[("w", Keep::F32), ("h", Keep::F32), ("b", Keep::F32)])
            .unwrap();
        assert_eq!((t["w"].rows, t["w"].cols), (2, 3));
        assert_eq!(t["w"].values(), &[1.0, -2.0, 0.5, 3.0, 4.0, 5.0]);
        assert_eq!(t["h"].values(), &[1.5]);
        assert_eq!(t["b"].values(), &[1.5]);
        let half = st.load(&[("w", Keep::F16)]).unwrap();
        assert_eq!(&*half["w"].row(1), &[3.0, 4.0, 5.0]);
        assert!(st.load(&[("i", Keep::F32)]).is_err());
        assert!(st.load(&[("missing", Keep::F32)]).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn rejects_bad_headers() {
        let data_start = 8 + 100;
        let len = data_start + 8;
        let bad = [
            r#"{"w": {"dtype": "F32", "shape": [3], "data_offsets": [0, 8]}}"#,
            r#"{"w": {"dtype": "F32", "shape": [4], "data_offsets": [0, 16]}}"#,
            r#"{"w": {"dtype": "F32", "shape": [1], "data_offsets": [4, 0]}}"#,
            r#"{"w": {"dtype": "F32", "shape": [1], "data_offsets": [0]}}"#,
            r#"{"w": {"dtype": "F32", "shape": [-1], "data_offsets": [0, 4]}}"#,
            r#"{"w": {"shape": [1], "data_offsets": [0, 4]}}"#,
            r#"{"w": {"dtype": "F32", "shape": [4611686018427387904, 4], "data_offsets": [0, 0]}}"#,
            r#"[1, 2]"#,
        ];
        for h in bad {
            assert!(parse_header(h.as_bytes(), data_start, len).is_err(), "{h}");
        }
        let good = r#"{"w": {"dtype": "F32", "shape": [2], "data_offsets": [0, 8]}}"#;
        let e = parse_header(good.as_bytes(), data_start, len).unwrap();
        assert_eq!((e["w"].start, e["w"].end), (data_start, data_start + 8));
    }
}
