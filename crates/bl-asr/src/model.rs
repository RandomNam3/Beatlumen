//! Loading Whisper models in the ggml file format (as published for
//! whisper.cpp): hyperparameters, mel filters, vocabulary and the weights
//! (f32 or f16, converted to f32 on load).

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

/// A weight tensor: `rows` rows of `cols` values (ggml's ne0 = cols).
pub struct Tensor {
    pub cols: usize,
    pub rows: usize,
    pub data: Vec<f32>,
}

impl Tensor {
    pub fn row(&self, r: usize) -> &[f32] {
        &self.data[r * self.cols..(r + 1) * self.cols]
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

/// IEEE half to f32.
pub fn f16_to_f32(h: u16) -> f32 {
    let sign = ((h >> 15) as u32) << 31;
    let exp = ((h >> 10) & 0x1f) as u32;
    let frac = (h & 0x3ff) as u32;
    let bits = match exp {
        0 if frac == 0 => sign,
        0 => {
            // Subnormal: normalise.
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

impl Model {
    pub fn load(r: &mut dyn Read) -> io::Result<Model> {
        if u32le(r)? != MAGIC {
            return Err(bad("not a ggml Whisper model"));
        }
        let params = Params {
            n_vocab: usize_field(r, 1 << 20)?,
            n_audio_ctx: usize_field(r, 1 << 16)?,
            n_audio_state: usize_field(r, 1 << 14)?,
            n_audio_head: usize_field(r, 256)?,
            n_audio_layer: usize_field(r, 256)?,
            n_text_ctx: usize_field(r, 1 << 16)?,
            n_text_state: usize_field(r, 1 << 14)?,
            n_text_head: usize_field(r, 256)?,
            n_text_layer: usize_field(r, 256)?,
            n_mels: usize_field(r, 1024)?,
        };
        let _ftype = i32le(r)?;
        // Mel filters.
        let n_mel = usize_field(r, 1024)?;
        let n_fft = usize_field(r, 1 << 14)?;
        if n_mel != params.n_mels {
            return Err(bad("mel filter count"));
        }
        let mut mel_filters = vec![0f32; n_mel * n_fft];
        read_f32s(r, &mut mel_filters)?;
        // Vocabulary.
        let n = usize_field(r, 1 << 20)?;
        let mut vocab = Vec::with_capacity(n);
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
            let mut data = vec![0f32; count];
            match ttype {
                0 => read_f32s(r, &mut data)?,
                1 => {
                    let mut raw = vec![0u8; count * 2];
                    r.read_exact(&mut raw)?;
                    for (d, c) in data.iter_mut().zip(raw.chunks_exact(2)) {
                        *d = f16_to_f32(u16::from_le_bytes([c[0], c[1]]));
                    }
                }
                _ => {
                    return Err(bad(
                        "unsupported tensor type (quantised models are not supported)",
                    ))
                }
            }
            tensors.insert(
                name,
                Tensor {
                    cols: ne[0],
                    rows: ne[1] * ne[2],
                    data,
                },
            );
        }
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

    /// Whether the model knows several languages (else English only).
    pub fn multilingual(&self) -> bool {
        self.params.n_vocab >= 51865
    }
}

fn read_f32s(r: &mut dyn Read, out: &mut [f32]) -> io::Result<()> {
    let mut raw = vec![0u8; out.len() * 4];
    r.read_exact(&mut raw)?;
    for (d, c) in out.iter_mut().zip(raw.chunks_exact(4)) {
        *d = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

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
