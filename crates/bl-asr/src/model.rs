//! Loading Whisper models in the ggml file format (as published for
//! whisper.cpp): hyperparameters, mel filters, vocabulary and the weights.
//! Weight matrices stay in the file's format (f32, f16 or the quantised
//! q4_0, q4_1, q5_0, q5_1 and q8_0) and are expanded row by row while
//! computing; vectors, convolutions and position embeddings become f32.

use std::borrow::Cow;
use std::collections::HashMap;
use std::io::{self, Read};

const MAGIC: u32 = 0x6767_6d6c; // "ggml"

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    pub n_vocab: usize,
    pub n_audio_ctx: usize,
    pub n_audio_state: usize,
    pub n_audio_head: usize,
    pub n_audio_layer: usize,
    pub n_text_ctx: usize,
    pub n_text_state: usize,
    pub n_text_head: usize,
    pub n_text_layer: usize,
    pub n_mels: usize,
}

/// Quantised block formats of ggml (32 values per block).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QType {
    Q4_0,
    Q4_1,
    Q5_0,
    Q5_1,
    Q8_0,
}

/// Values per quantised block.
pub const QK: usize = 32;

impl QType {
    fn from_ggml(t: i32) -> Option<QType> {
        Some(match t {
            2 => QType::Q4_0,
            3 => QType::Q4_1,
            6 => QType::Q5_0,
            7 => QType::Q5_1,
            8 => QType::Q8_0,
            _ => return None,
        })
    }

    /// Bytes per block of 32 values.
    pub fn block_bytes(self) -> usize {
        match self {
            QType::Q4_0 => 18,
            QType::Q4_1 => 20,
            QType::Q5_0 => 22,
            QType::Q5_1 => 24,
            QType::Q8_0 => 34,
        }
    }

    /// Expands one block into 32 values (the plain version; `ops` has
    /// vectorised ones).
    pub(crate) fn dequantize(self, b: &[u8], out: &mut [f32]) {
        let h = |i: usize| f16_to_f32(u16::from_le_bytes([b[i], b[i + 1]]));
        let d = h(0);
        match self {
            QType::Q8_0 => {
                for (o, &q) in out.iter_mut().zip(&b[2..34]) {
                    *o = q as i8 as f32 * d;
                }
            }
            QType::Q4_0 | QType::Q4_1 => {
                let (min, qs) = match self {
                    QType::Q4_0 => (-8.0 * d, &b[2..18]),
                    _ => (h(2), &b[4..20]),
                };
                for (j, &q) in qs.iter().enumerate() {
                    out[j] = (q & 0x0f) as f32 * d + min;
                    out[j + 16] = (q >> 4) as f32 * d + min;
                }
            }
            QType::Q5_0 | QType::Q5_1 => {
                let (min, at) = match self {
                    QType::Q5_0 => (-16.0 * d, 2),
                    _ => (h(2), 4),
                };
                let qh = u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]]);
                for (j, &q) in b[at + 4..at + 20].iter().enumerate() {
                    let hi0 = ((qh >> j) << 4) & 0x10;
                    let hi1 = (qh >> (j + 12)) & 0x10;
                    out[j] = ((q as u32 & 0x0f) | hi0) as f32 * d + min;
                    out[j + 16] = ((q as u32 >> 4) | hi1) as f32 * d + min;
                }
            }
        }
    }
}

/// How a tensor's values are kept in memory.
pub enum Store {
    F32(Vec<f32>),
    F16(Vec<u16>),
    Quant(QType, Vec<u8>),
}

/// Rows of a tensor as they are stored, for the kernels in `ops`.
#[derive(Clone, Copy)]
pub(crate) enum Packed<'a> {
    F32(&'a [f32]),
    F16(&'a [u16]),
    /// Whole blocks of the given type.
    Quant(QType, &'a [u8]),
}

/// A weight tensor: `rows` rows of `cols` values (ggml's ne0 = cols).
pub struct Tensor {
    pub cols: usize,
    pub rows: usize,
    store: Store,
}

impl Tensor {
    pub fn from_f32(cols: usize, rows: usize, data: Vec<f32>) -> Tensor {
        Tensor {
            cols,
            rows,
            store: Store::F32(data),
        }
    }

    /// A tensor of `rows` rows of `cols` values kept as `store`; `None` if
    /// the stored data does not have exactly that size (quantised rows
    /// must be whole blocks).
    pub fn new(cols: usize, rows: usize, store: Store) -> Option<Tensor> {
        let count = cols.checked_mul(rows)?;
        let fits = match &store {
            Store::F32(v) => v.len() == count,
            Store::F16(v) => v.len() == count,
            Store::Quant(t, v) => {
                // (`is_multiple_of` needs Rust 1.87.)
                #[allow(clippy::manual_is_multiple_of)]
                let whole_blocks = cols % QK == 0;
                whole_blocks && Some(v.len()) == (count / QK).checked_mul(t.block_bytes())
            }
        };
        fits.then_some(Tensor { cols, rows, store })
    }

    /// All values (tensors kept as f32: vectors, convolutions, position
    /// embeddings).
    pub fn values(&self) -> &[f32] {
        match &self.store {
            Store::F32(v) => v,
            _ => panic!("tensor is not kept as f32"),
        }
    }

    /// The f32 values of a whole-f32 tensor, if it is one.
    pub fn as_f32(&self) -> Option<&[f32]> {
        match &self.store {
            Store::F32(v) => Some(v),
            _ => None,
        }
    }

    /// Row `r` as f32 values.
    pub fn row(&self, r: usize) -> Cow<'_, [f32]> {
        match &self.store {
            Store::F32(v) => Cow::Borrowed(&v[r * self.cols..(r + 1) * self.cols]),
            _ => {
                let mut out = vec![0f32; self.cols];
                self.row_into(r, &mut out);
                Cow::Owned(out)
            }
        }
    }

    /// Writes row `r` as f32 values into `out` (`cols` long).
    pub fn row_into(&self, r: usize, out: &mut [f32]) {
        crate::ops::expand(self.packed(r, r + 1), out);
    }

    /// Rows `r0..r1` as stored (panics if they are not all there).
    pub(crate) fn packed(&self, r0: usize, r1: usize) -> Packed<'_> {
        let c = self.cols;
        match &self.store {
            Store::F32(v) => Packed::F32(&v[r0 * c..r1 * c]),
            Store::F16(v) => Packed::F16(&v[r0 * c..r1 * c]),
            Store::Quant(t, v) => {
                let row_bytes = c / QK * t.block_bytes();
                Packed::Quant(*t, &v[r0 * row_bytes..r1 * row_bytes])
            }
        }
    }

    /// Memory the values take (bytes).
    pub fn bytes(&self) -> usize {
        match &self.store {
            Store::F32(v) => v.len() * 4,
            Store::F16(v) => v.len() * 2,
            Store::Quant(_, v) => v.len(),
        }
    }

    fn into_f32(self) -> Tensor {
        if matches!(self.store, Store::F32(_)) {
            return self;
        }
        let mut data = vec![0f32; self.cols * self.rows];
        crate::ops::expand(self.packed(0, self.rows), &mut data);
        Tensor::from_f32(self.cols, self.rows, data)
    }
}

pub struct Model {
    pub params: Params,
    /// Mel filter bank: `n_mels` rows of `n_fft / 2 + 1` weights.
    pub mel_filters: Vec<f32>,
    pub mel_bins: usize,
    /// Token texts as bytes (UTF-8 pieces).
    pub vocab: Vec<Vec<u8>>,
    pub tensors: HashMap<String, Tensor>,
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Reads `n` bytes. The buffer grows (doubling, from 1 MiB) only as the
/// data arrives, so a size a file claims allocates at most about twice
/// what the file really holds.
fn read_bytes(r: &mut dyn Read, n: usize) -> io::Result<Vec<u8>> {
    let mut v: Vec<u8> = Vec::new();
    while v.len() < n {
        let start = v.len();
        let end = start.saturating_mul(2).max(1 << 20).min(n);
        v.reserve_exact(end - start);
        v.resize(end, 0);
        r.read_exact(&mut v[start..])?;
    }
    Ok(v)
}

/// What the network expects of a tensor it uses.
#[derive(Debug, Clone, Copy)]
enum Want {
    /// `cols` x `rows`.
    Dims(usize, usize),
    /// This many values, in any shape (vectors, convolutions).
    Count(usize),
    /// `cols` columns and at most this many rows.
    ColsUpTo(usize, usize),
    /// `rows` rows and at most this many columns.
    RowsUpTo(usize, usize),
    /// At most this many values, in any shape.
    CountUpTo(usize),
}

impl Want {
    fn fits(self, cols: usize, rows: usize) -> bool {
        let Some(count) = cols.checked_mul(rows) else {
            return false;
        };
        match self {
            Want::Dims(c, r) => cols == c && rows == r,
            Want::Count(n) => count == n,
            Want::ColsUpTo(c, r) => cols == c && rows <= r,
            Want::RowsUpTo(c, r) => rows == r && cols <= c,
            Want::CountUpTo(n) => count <= n,
        }
    }
}

/// Widest hidden layer of the MLPs, in multiples of the state (Whisper
/// uses 4).
const MLP_WIDTH: usize = 16;

/// The shape the network expects of the tensor `name`, and whether it is
/// used as f32 (vectors, convolutions, position embeddings; the rest is
/// read row by row as stored). `None` for tensors it does not use.
fn expected(p: &Params, name: &str) -> Option<(Want, bool)> {
    use Want::*;
    let (a, t) = (p.n_audio_state, p.n_text_state);
    Some(match name {
        "encoder.conv1.weight" => (Count(3 * p.n_mels * a), true),
        "encoder.conv2.weight" => (Count(3 * a * a), true),
        "encoder.conv1.bias"
        | "encoder.conv2.bias"
        | "encoder.ln_post.weight"
        | "encoder.ln_post.bias" => (Count(a), true),
        "encoder.positional_embedding" => (Dims(a, p.n_audio_ctx), true),
        "decoder.positional_embedding" => (Dims(t, p.n_text_ctx), true),
        "decoder.token_embedding.weight" => (Dims(t, p.n_vocab), false),
        "decoder.ln.weight" | "decoder.ln.bias" => (Count(t), true),
        _ => {
            let (d, layers, rest, decoder) = match name.strip_prefix("encoder.blocks.") {
                Some(r) => (a, p.n_audio_layer, r, false),
                None => (
                    t,
                    p.n_text_layer,
                    name.strip_prefix("decoder.blocks.")?,
                    true,
                ),
            };
            let (layer, part) = rest.split_once('.')?;
            if layer.parse::<usize>().ok()? >= layers {
                return None;
            }
            match part {
                "attn_ln.weight" | "attn_ln.bias" | "mlp_ln.weight" | "mlp_ln.bias"
                | "attn.query.bias" | "attn.value.bias" | "attn.out.bias" | "mlp.2.bias" => {
                    (Count(d), true)
                }
                "attn.query.weight" | "attn.key.weight" | "attn.value.weight"
                | "attn.out.weight" => (Dims(d, d), false),
                "mlp.0.weight" => (ColsUpTo(d, MLP_WIDTH * d), false),
                "mlp.0.bias" => (CountUpTo(MLP_WIDTH * d), true),
                "mlp.2.weight" => (RowsUpTo(MLP_WIDTH * d, d), false),
                "cross_attn_ln.weight"
                | "cross_attn_ln.bias"
                | "cross_attn.query.bias"
                | "cross_attn.value.bias"
                | "cross_attn.out.bias"
                    if decoder =>
                {
                    (Count(d), true)
                }
                "cross_attn.query.weight" | "cross_attn.out.weight" if decoder => {
                    (Dims(d, d), false)
                }
                // Keys and values of the audio features.
                "cross_attn.key.weight" | "cross_attn.value.weight" if decoder => {
                    (Dims(a, d), false)
                }
                _ => return None,
            }
        }
    })
}

/// The MLP layers of each block fit each other (their width is free).
fn check_mlps(p: &Params, tensors: &HashMap<String, Tensor>) -> io::Result<()> {
    let blocks = (0..p.n_audio_layer)
        .map(|l| format!("encoder.blocks.{l}"))
        .chain((0..p.n_text_layer).map(|l| format!("decoder.blocks.{l}")));
    for b in blocks {
        let Some(w0) = tensors.get(&format!("{b}.mlp.0.weight")) else {
            continue;
        };
        let width = w0.rows;
        let bias_ok = tensors
            .get(&format!("{b}.mlp.0.bias"))
            .is_none_or(|t| t.cols * t.rows == width);
        let w2_ok = tensors
            .get(&format!("{b}.mlp.2.weight"))
            .is_none_or(|t| t.cols == width);
        if !bias_ok || !w2_ok {
            return Err(bad(&format!("{b}: MLP layers do not fit")));
        }
    }
    Ok(())
}

fn u32le(r: &mut dyn Read) -> io::Result<u32> {
    let mut b = [0u8; 4];
    r.read_exact(&mut b)?;
    Ok(u32::from_le_bytes(b))
}

fn i32le(r: &mut dyn Read) -> io::Result<i32> {
    Ok(u32le(r)? as i32)
}

fn usize_field(r: &mut dyn Read, max: usize) -> io::Result<usize> {
    let v = i32le(r)?;
    if v <= 0 || v as usize > max {
        return Err(bad("model parameter out of range"));
    }
    Ok(v as usize)
}

/// IEEE half to f32 (exact for every input). Branch-free, so loops
/// over it vectorise.
#[inline]
pub fn f16_to_f32(h: u16) -> f32 {
    let h = h as u32;
    let sign = (h & 0x8000) << 16;
    let exp = h & 0x7c00;
    // Exponent and fraction moved to their f32 places.
    let shifted = (h & 0x7fff) << 13;
    // Normal numbers: rebias the exponent from 15 to 127.
    let normal = shifted + ((127 - 15) << 23);
    // Infinity and NaN keep their fraction.
    let special = shifted | 0x7f80_0000;
    // Subnormals are fraction * 2^-24: an exact integer conversion and
    // scaling (no subnormal f32 arithmetic involved).
    let sub = ((h & 0x3ff) as f32 * (1.0 / 16_777_216.0)).to_bits();
    let bits = if exp == 0 {
        sub
    } else if exp == 0x7c00 {
        special
    } else {
        normal
    };
    f32::from_bits(sign | bits)
}

impl Model {
    pub fn load(r: &mut dyn Read) -> io::Result<Model> {
        if u32le(r)? != MAGIC {
            return Err(bad("not a ggml Whisper model"));
        }
        // Limits well above every Whisper model (large: 1280 wide, 1500
        // audio and 448 text positions, 32 layers, 128 mel bands).
        let params = Params {
            n_vocab: usize_field(r, 1 << 20)?,
            n_audio_ctx: usize_field(r, 1 << 13)?,
            n_audio_state: usize_field(r, 1 << 12)?,
            n_audio_head: usize_field(r, 256)?,
            n_audio_layer: usize_field(r, 256)?,
            n_text_ctx: usize_field(r, 1 << 13)?,
            n_text_state: usize_field(r, 1 << 12)?,
            n_text_head: usize_field(r, 256)?,
            n_text_layer: usize_field(r, 256)?,
            n_mels: usize_field(r, 1024)?,
        };
        if params.n_audio_state % params.n_audio_head != 0
            || params.n_text_state % params.n_text_head != 0
        {
            return Err(bad("attention heads do not divide the state"));
        }
        let _ftype = i32le(r)?;
        // Mel filters: one row of weights for each bin of the 400-point
        // transform the spectrogram uses.
        let n_mel = usize_field(r, 1024)?;
        let n_fft = usize_field(r, 1 << 14)?;
        if n_mel != params.n_mels {
            return Err(bad("mel filter count"));
        }
        if n_fft != crate::mel::N_FFT / 2 + 1 {
            return Err(bad("mel filter width"));
        }
        let mel_filters = read_f32s(r, n_mel * n_fft)?;
        // Vocabulary.
        let n = usize_field(r, 1 << 20)?;
        let mut vocab = Vec::with_capacity(n.min(1 << 16));
        for _ in 0..n {
            let len = u32le(r)? as usize;
            if len > 1 << 16 {
                return Err(bad("vocabulary entry too long"));
            }
            let mut w = vec![0u8; len];
            r.read_exact(&mut w)?;
            vocab.push(w);
        }
        // Tensors until the end of the file.
        let mut tensors = HashMap::new();
        loop {
            let mut first = [0u8; 4];
            match r.read(&mut first[..1])? {
                0 => break,
                _ => r.read_exact(&mut first[1..])?,
            }
            let n_dims = i32::from_le_bytes(first);
            let name_len = i32le(r)?;
            let ttype = i32le(r)?;
            if !(1..=3).contains(&n_dims) || !(1..=256).contains(&name_len) {
                return Err(bad("tensor header"));
            }
            let mut ne = [1usize; 3];
            for d in ne.iter_mut().take(n_dims as usize) {
                *d = usize_field(r, 1 << 20)?;
            }
            let mut name = vec![0u8; name_len as usize];
            r.read_exact(&mut name)?;
            let name = String::from_utf8(name).map_err(|_| bad("tensor name"))?;
            let count = ne[0]
                .checked_mul(ne[1])
                .and_then(|c| c.checked_mul(ne[2]))
                .filter(|&c| c <= 1 << 30)
                .ok_or_else(|| bad("tensor too large"))?;
            let (cols, rows) = (ne[0], ne[1] * ne[2]);
            // A tensor the network uses must have the shape the
            // hyperparameters give it: nothing sized from them then reaches
            // beyond the weights.
            let want = expected(&params, &name);
            if want.is_some_and(|(w, _)| !w.fits(cols, rows)) {
                return Err(bad(&format!("tensor {name} has the wrong shape")));
            }
            let too_large = || bad("tensor too large");
            let store = match ttype {
                0 => Store::F32(read_f32s(r, count)?),
                1 => {
                    let raw = read_bytes(r, count.checked_mul(2).ok_or_else(too_large)?)?;
                    Store::F16(
                        raw.chunks_exact(2)
                            .map(|c| u16::from_le_bytes([c[0], c[1]]))
                            .collect(),
                    )
                }
                t => {
                    let q = QType::from_ggml(t).ok_or_else(|| bad("unsupported tensor type"))?;
                    if ne[0] % QK != 0 {
                        return Err(bad("quantised row length"));
                    }
                    let n = (count / QK)
                        .checked_mul(q.block_bytes())
                        .ok_or_else(too_large)?;
                    Store::Quant(q, read_bytes(r, n)?)
                }
            };
            let t = Tensor { cols, rows, store };
            // Matrices stay compact; what the network uses as f32 (vectors,
            // convolutions, position embeddings) is expanded. Tensors it
            // does not use stay as stored.
            let f32 = want.is_some_and(|(_, f32)| f32);
            tensors.insert(name, if f32 { t.into_f32() } else { t });
        }
        check_mlps(&params, &tensors)?;
        Ok(Model {
            params,
            mel_filters,
            mel_bins: n_fft,
            vocab,
            tensors,
        })
    }

    pub fn get(&self, name: &str) -> io::Result<&Tensor> {
        self.tensors
            .get(name)
            .ok_or_else(|| bad(&format!("missing tensor {name}")))
    }

    /// Memory the weights take (bytes).
    pub fn weight_bytes(&self) -> usize {
        self.tensors.values().map(Tensor::bytes).sum()
    }

    /// Whether the model knows several languages (else English only).
    pub fn multilingual(&self) -> bool {
        self.params.n_vocab >= 51865
    }
}

/// Reads `n` little-endian f32 values (see [`read_bytes`]).
fn read_f32s(r: &mut dyn Read, n: usize) -> io::Result<Vec<f32>> {
    let bytes = n.checked_mul(4).ok_or_else(|| bad("tensor too large"))?;
    Ok(read_bytes(r, bytes)?
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantised_blocks() {
        let one = 0x3c00u16.to_le_bytes(); // 1.0
        let half = 0x3800u16.to_le_bytes(); // 0.5
        let mut out = [0f32; 32];
        // q8_0: d, 32 signed bytes.
        let mut b = one.to_vec();
        b.extend((0..32).map(|i| (i as i8 - 16) as u8));
        QType::Q8_0.dequantize(&b, &mut out);
        assert_eq!((out[0], out[31]), (-16.0, 15.0));
        // q4_0: d, nibbles (low: first half, high: second half), minus 8.
        let mut b = one.to_vec();
        b.extend([0x9fu8; 16]);
        QType::Q4_0.dequantize(&b, &mut out);
        assert_eq!((out[0], out[16]), (7.0, 1.0));
        // q4_1: d, m, nibbles.
        let mut b = half.to_vec();
        b.extend(one);
        b.extend([0x21u8; 16]);
        QType::Q4_1.dequantize(&b, &mut out);
        assert_eq!((out[0], out[16]), (1.5, 2.0));
        // q5_0: d, high bits, nibbles, minus 16.
        let mut b = one.to_vec();
        b.extend(1u32.to_le_bytes()); // high bit of value 0
        b.extend([0x00u8; 16]);
        QType::Q5_0.dequantize(&b, &mut out);
        assert_eq!((out[0], out[1], out[16]), (0.0, -16.0, -16.0));
        // q5_1: d, m, high bits (bit 16 = value 16), nibbles.
        let mut b = one.to_vec();
        b.extend(half);
        b.extend((1u32 << 16).to_le_bytes());
        b.extend([0x10u8; 16]);
        QType::Q5_1.dequantize(&b, &mut out);
        assert_eq!((out[0], out[16]), (0.5, 17.5));
    }

    /// The straightforward conversion `f16_to_f32` replaced.
    fn f16_reference(h: u16) -> f32 {
        let sign = ((h >> 15) as u32) << 31;
        let exp = ((h >> 10) & 0x1f) as u32;
        let frac = (h & 0x3ff) as u32;
        let bits = match exp {
            0 if frac == 0 => sign,
            0 => {
                let mut e = 127 - 15 + 1;
                let mut f = frac;
                while f & 0x400 == 0 {
                    f <<= 1;
                    e -= 1;
                }
                sign | ((e as u32) << 23) | ((f & 0x3ff) << 13)
            }
            0x1f => sign | 0x7f80_0000 | (frac << 13),
            _ => sign | ((exp + 127 - 15) << 23) | (frac << 13),
        };
        f32::from_bits(bits)
    }

    #[test]
    fn half_floats_all() {
        for h in 0..=u16::MAX {
            assert_eq!(
                f16_to_f32(h).to_bits(),
                f16_reference(h).to_bits(),
                "{h:#06x}"
            );
        }
    }

    #[test]
    fn tensor_sizes() {
        let q8 = |len| Store::Quant(QType::Q8_0, vec![0; len]);
        assert!(Tensor::new(32, 2, q8(68)).is_some());
        assert!(Tensor::new(32, 2, q8(67)).is_none());
        assert!(Tensor::new(16, 2, q8(34)).is_none());
        assert!(Tensor::new(3, 2, Store::F16(vec![0; 6])).is_some());
        assert!(Tensor::new(3, 2, Store::F32(vec![0.0; 5])).is_none());
        assert!(Tensor::new(usize::MAX, 2, Store::F32(Vec::new())).is_none());
    }

    #[test]
    fn half_floats() {
        assert_eq!(f16_to_f32(0x3c00), 1.0);
        assert_eq!(f16_to_f32(0xc000), -2.0);
        assert_eq!(f16_to_f32(0x7bff), 65504.0);
        assert_eq!(f16_to_f32(0x0001), 5.960_464_5e-8);
        assert_eq!(f16_to_f32(0x0000), 0.0);
        assert!(f16_to_f32(0x7c00).is_infinite());
    }
}
