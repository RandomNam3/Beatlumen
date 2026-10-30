//! The MarianMT transformer (as in Hugging Face `transformers`):
//! encoder and decoder of post-norm attention blocks, sinusoidal
//! positions, embeddings shared with the output projection.
//!
//! Weight matrices are kept as f16 (or f32) and multiplied with the
//! kernels of `bl_asr::ops`; vectors (biases, layer norms) stay f32.

use crate::torch::{Checkpoint, Keep};
use bl_asr::model::Tensor;
use bl_asr::ops::{axpy, dot, gelu, layer_norm, linear, softmax};
use bl_json::Value;
use std::collections::HashMap;

/// Largest supported sizes (sanity limits for `config.json`).
const MAX_DIM: usize = 8192;
const MAX_LAYERS: usize = 64;
const MAX_VOCAB: usize = 1 << 20;
const MAX_POSITIONS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Activation {
    /// SiLU: `x * sigmoid(x)`.
    Swish,
    Relu,
    /// GELU (computed with the tanh approximation).
    Gelu,
}

/// The model description from `config.json` (and
/// `generation_config.json`).
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    pub d_model: usize,
    pub encoder_layers: usize,
    pub decoder_layers: usize,
    pub encoder_heads: usize,
    pub decoder_heads: usize,
    pub encoder_ffn: usize,
    pub decoder_ffn: usize,
    pub vocab_size: usize,
    pub max_positions: usize,
    pub activation: Activation,
    pub scale_embedding: bool,
    pub decoder_start: u32,
    pub eos: u32,
    pub pad: u32,
    pub num_beams: usize,
    pub max_length: usize,
    /// Tokens never generated (single-token entries of `bad_words_ids`).
    pub bad_words: Vec<u32>,
}

/// A field of a JSON object (`None` if missing or null).
fn field<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    Some(v.get(key)).filter(|v| !v.is_null())
}

fn usize_of(v: &Value, key: &str, max: usize) -> Result<usize, String> {
    field(v, key)
        .and_then(Value::as_u64)
        .and_then(|n| usize::try_from(n).ok())
        .filter(|&n| n >= 1 && n <= max)
        .ok_or_else(|| format!("config: bad or missing {key}"))
}

impl Config {
    /// Reads `config.json`; values in `generation` (from
    /// `generation_config.json`) take precedence for the generation
    /// settings.
    pub fn parse(config: &str, generation: Option<&str>) -> Result<Config, String> {
        let v = bl_json::parse(config).map_err(|e| format!("config.json: {e}"))?;
        let g = match generation {
            Some(s) => bl_json::parse(s).map_err(|e| format!("generation_config.json: {e}"))?,
            None => Value::object(),
        };
        // A generation setting: from generation_config.json, else config.json.
        let gen = |key: &str| field(&g, key).or_else(|| field(&v, key));
        let token = |key: &str, vocab: usize| -> Result<u32, String> {
            gen(key)
                .and_then(Value::as_u64)
                .filter(|&t| t < vocab as u64)
                .map(|t| t as u32)
                .ok_or_else(|| format!("config: bad or missing {key}"))
        };
        if field(&v, "normalize_before").and_then(Value::as_bool) == Some(true) {
            return Err("config: pre-norm models are not supported".into());
        }
        let activation = match field(&v, "activation_function").and_then(Value::as_str) {
            Some("swish" | "silu") => Activation::Swish,
            Some("relu") => Activation::Relu,
            Some("gelu" | "gelu_new" | "gelu_fast" | "gelu_pytorch_tanh") => Activation::Gelu,
            _ => return Err("config: unsupported activation_function".into()),
        };
        let vocab_size = usize_of(&v, "vocab_size", MAX_VOCAB)?;
        let bad_words = match gen("bad_words_ids") {
            Some(Value::Array(lists)) => lists
                .iter()
                .filter_map(|l| match l {
                    Value::Array(t) if t.len() == 1 => t[0].as_u64(),
                    _ => None,
                })
                .filter(|&t| t < vocab_size as u64)
                .map(|t| t as u32)
                .collect(),
            _ => Vec::new(),
        };
        let c = Config {
            d_model: usize_of(&v, "d_model", MAX_DIM)?,
            encoder_layers: usize_of(&v, "encoder_layers", MAX_LAYERS)?,
            decoder_layers: usize_of(&v, "decoder_layers", MAX_LAYERS)?,
            encoder_heads: usize_of(&v, "encoder_attention_heads", MAX_DIM)?,
            decoder_heads: usize_of(&v, "decoder_attention_heads", MAX_DIM)?,
            encoder_ffn: usize_of(&v, "encoder_ffn_dim", 8 * MAX_DIM)?,
            decoder_ffn: usize_of(&v, "decoder_ffn_dim", 8 * MAX_DIM)?,
            vocab_size,
            max_positions: usize_of(&v, "max_position_embeddings", MAX_POSITIONS)?,
            activation,
            scale_embedding: field(&v, "scale_embedding").and_then(Value::as_bool) == Some(true),
            decoder_start: token("decoder_start_token_id", vocab_size)?,
            eos: token("eos_token_id", vocab_size)?,
            pad: token("pad_token_id", vocab_size)?,
            num_beams: gen("num_beams")
                .and_then(Value::as_u64)
                .map_or(1, |n| n.clamp(1, 16) as usize),
            max_length: gen("max_length")
                .and_then(Value::as_u64)
                .map_or(512, |n| n.clamp(2, MAX_POSITIONS as u64) as usize),
            bad_words,
        };
        // (`is_multiple_of` needs Rust 1.87.)
        #[allow(clippy::manual_is_multiple_of)]
        let split = c.d_model % c.encoder_heads == 0 && c.d_model % c.decoder_heads == 0;
        if !split {
            return Err("config: d_model is not a multiple of the heads".into());
        }
        Ok(c)
    }
}

/// Sinusoidal position embeddings as in Marian: for position `p` and
/// `i < dim / 2`, column `i` is `sin(p / 10000^(2i / dim))` and column
/// `dim / 2 + i` the cosine of the same (`positions` rows of `dim`).
pub fn sinusoidal(positions: usize, dim: usize) -> Vec<f32> {
    let half = dim.div_ceil(2);
    let mut out = vec![0f32; positions * dim];
    for p in 0..positions {
        let row = &mut out[p * dim..(p + 1) * dim];
        for i in 0..half {
            let angle = p as f64 / 10000f64.powf((2 * i) as f64 / dim as f64);
            row[i] = angle.sin() as f32;
            if half + i < dim {
                row[half + i] = angle.cos() as f32;
            }
        }
    }
    out
}

struct Linear {
    w: Tensor,
    b: Tensor,
}

impl Linear {
    fn apply(&self, x: &[f32], n: usize) -> Vec<f32> {
        linear(x, n, &self.w, Some(&self.b))
    }
}

struct Norm {
    w: Tensor,
    b: Tensor,
}

impl Norm {
    fn apply(&self, x: &mut [f32]) {
        layer_norm(x, self.w.cols, &self.w, &self.b);
    }
}

struct Attention {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
}

struct EncoderLayer {
    attn: Attention,
    attn_norm: Norm,
    fc1: Linear,
    fc2: Linear,
    norm: Norm,
}

struct DecoderLayer {
    attn: Attention,
    attn_norm: Norm,
    cross: Attention,
    cross_norm: Norm,
    fc1: Linear,
    fc2: Linear,
    norm: Norm,
}

pub struct Model {
    pub config: Config,
    /// Distinct embedding tables (usually one, shared by all three uses).
    tables: Vec<Tensor>,
    enc_embed: usize,
    dec_embed: usize,
    lm_head: usize,
    logits_bias: Option<Tensor>,
    positions: Vec<f32>,
    encoder: Vec<EncoderLayer>,
    decoder: Vec<DecoderLayer>,
}

/// Encoder output prepared for the decoder: keys and values of the cross
/// attention of each layer (`len` rows of `d_model`).
pub struct Encoded {
    pub len: usize,
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
}

/// The decoder's self-attention keys and values of one hypothesis, per
/// layer (one row of `d_model` per generated position).
#[derive(Clone, Default)]
pub struct Cache {
    keys: Vec<Vec<f32>>,
    values: Vec<Vec<f32>>,
}

/// Collects the tensors to load and takes them out after loading.
struct Names {
    wanted: Vec<(String, Keep, usize, usize)>,
}

impl Names {
    fn add(&mut self, name: String, keep: Keep, rows: usize, cols: usize) -> String {
        self.wanted.push((name.clone(), keep, rows, cols));
        name
    }
}

struct Loaded(HashMap<String, Tensor>);

impl Loaded {
    fn take(&mut self, name: &str) -> Result<Tensor, String> {
        self.0
            .remove(name)
            .ok_or_else(|| format!("model: missing tensor {name}"))
    }
}

fn activate(a: Activation, x: &mut [f32]) {
    match a {
        Activation::Swish => x.iter_mut().for_each(|v| *v /= 1.0 + (-*v).exp()),
        Activation::Relu => x.iter_mut().for_each(|v| *v = v.max(0.0)),
        Activation::Gelu => gelu(x),
    }
}

/// Attention of the query rows `q` (`nq` rows, already scaled) to `k` and
/// `v` (`nk` rows), split into `heads` heads of the `dim` columns.
fn attend(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    nq: usize,
    nk: usize,
    dim: usize,
    heads: usize,
) -> Vec<f32> {
    let hd = dim / heads;
    let mut out = vec![0f32; nq * dim];
    let mut scores = vec![0f32; nk];
    for i in 0..nq {
        for h in 0..heads {
            let cols = h * hd..(h + 1) * hd;
            let qi = &q[i * dim..(i + 1) * dim][cols.clone()];
            for (j, s) in scores.iter_mut().enumerate() {
                *s = dot(qi, &k[j * dim..(j + 1) * dim][cols.clone()]);
            }
            softmax(&mut scores);
            let o = &mut out[i * dim..(i + 1) * dim][cols.clone()];
            for (j, &s) in scores.iter().enumerate() {
                axpy(s, &v[j * dim..(j + 1) * dim][cols.clone()], o);
            }
        }
    }
    out
}

fn add(x: &mut [f32], y: &[f32]) {
    for (a, b) in x.iter_mut().zip(y) {
        *a += b;
    }
}

impl Model {
    /// Loads the weights from `ckpt`: matrices kept as `keep`, vectors f32.
    pub fn load(config: Config, ckpt: &Checkpoint, keep: Keep) -> Result<Model, String> {
        let c = &config;
        let d = c.d_model;
        let prefix = if ckpt.get("model.shared.weight").is_some() {
            "model."
        } else {
            ""
        };
        let mut names = Names { wanted: Vec::new() };
        // Embedding tables: tied ones (same storage place and shape) are
        // loaded once.
        let shared = format!("{prefix}shared.weight");
        let mut tables: Vec<String> = Vec::new();
        let mut table = |name: String, fallback: Option<usize>| -> Result<usize, String> {
            let t = ckpt.get(&name);
            if t.is_none() {
                return fallback.ok_or_else(|| format!("model: missing tensor {name}"));
            }
            for (i, other) in tables.iter().enumerate() {
                if ckpt.get(other) == t {
                    return Ok(i);
                }
            }
            tables.push(name);
            Ok(tables.len() - 1)
        };
        let shared_idx = table(shared, None).ok();
        let enc_embed = table(format!("{prefix}encoder.embed_tokens.weight"), shared_idx)?;
        let dec_embed = table(format!("{prefix}decoder.embed_tokens.weight"), shared_idx)?;
        let lm_head = table("lm_head.weight".to_string(), Some(dec_embed))?;
        let table_names: Vec<String> = tables
            .iter()
            .map(|n| names.add(n.clone(), keep, c.vocab_size, d))
            .collect();
        let bias_name = "final_logits_bias".to_string();
        let has_bias = ckpt.get(&bias_name).is_some();
        if has_bias {
            names.add(bias_name.clone(), Keep::F32, 1, c.vocab_size);
        }
        let lin = |names: &mut Names, base: &str, out: usize, inp: usize| {
            names.add(format!("{base}.weight"), keep, out, inp);
            names.add(format!("{base}.bias"), Keep::F32, 1, out);
            base.to_string()
        };
        let norm = |names: &mut Names, base: &str| {
            names.add(format!("{base}.weight"), Keep::F32, 1, d);
            names.add(format!("{base}.bias"), Keep::F32, 1, d);
            base.to_string()
        };
        let plan_attn = |names: &mut Names, base: &str| {
            for p in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                lin(names, &format!("{base}.{p}"), d, d);
            }
        };
        for (kind, layers, ffn) in [
            ("encoder", c.encoder_layers, c.encoder_ffn),
            ("decoder", c.decoder_layers, c.decoder_ffn),
        ] {
            for l in 0..layers {
                let base = format!("{prefix}{kind}.layers.{l}");
                plan_attn(&mut names, &format!("{base}.self_attn"));
                norm(&mut names, &format!("{base}.self_attn_layer_norm"));
                if kind == "decoder" {
                    plan_attn(&mut names, &format!("{base}.encoder_attn"));
                    norm(&mut names, &format!("{base}.encoder_attn_layer_norm"));
                }
                lin(&mut names, &format!("{base}.fc1"), ffn, d);
                lin(&mut names, &format!("{base}.fc2"), d, ffn);
                norm(&mut names, &format!("{base}.final_layer_norm"));
            }
        }
        let wanted: Vec<(&str, Keep)> = names
            .wanted
            .iter()
            .map(|(n, k, _, _)| (n.as_str(), *k))
            .collect();
        let mut loaded = Loaded(ckpt.load(&wanted)?);
        for (name, _, rows, cols) in &names.wanted {
            let t = loaded
                .0
                .get(name)
                .ok_or_else(|| format!("model: missing {name}"))?;
            if (t.rows, t.cols) != (*rows, *cols) {
                return Err(format!(
                    "model: {name} has shape {}x{}, expected {rows}x{cols}",
                    t.rows, t.cols
                ));
            }
        }
        let take_lin = |l: &mut Loaded, base: &str| -> Result<Linear, String> {
            Ok(Linear {
                w: l.take(&format!("{base}.weight"))?,
                b: l.take(&format!("{base}.bias"))?,
            })
        };
        let take_norm = |l: &mut Loaded, base: &str| -> Result<Norm, String> {
            Ok(Norm {
                w: l.take(&format!("{base}.weight"))?,
                b: l.take(&format!("{base}.bias"))?,
            })
        };
        let take_attn = |l: &mut Loaded, base: &str| -> Result<Attention, String> {
            Ok(Attention {
                q: take_lin(l, &format!("{base}.q_proj"))?,
                k: take_lin(l, &format!("{base}.k_proj"))?,
                v: take_lin(l, &format!("{base}.v_proj"))?,
                out: take_lin(l, &format!("{base}.out_proj"))?,
            })
        };
        let mut encoder = Vec::new();
        for l in 0..c.encoder_layers {
            let base = format!("{prefix}encoder.layers.{l}");
            encoder.push(EncoderLayer {
                attn: take_attn(&mut loaded, &format!("{base}.self_attn"))?,
                attn_norm: take_norm(&mut loaded, &format!("{base}.self_attn_layer_norm"))?,
                fc1: take_lin(&mut loaded, &format!("{base}.fc1"))?,
                fc2: take_lin(&mut loaded, &format!("{base}.fc2"))?,
                norm: take_norm(&mut loaded, &format!("{base}.final_layer_norm"))?,
            });
        }
        let mut decoder = Vec::new();
        for l in 0..c.decoder_layers {
            let base = format!("{prefix}decoder.layers.{l}");
            decoder.push(DecoderLayer {
                attn: take_attn(&mut loaded, &format!("{base}.self_attn"))?,
                attn_norm: take_norm(&mut loaded, &format!("{base}.self_attn_layer_norm"))?,
                cross: take_attn(&mut loaded, &format!("{base}.encoder_attn"))?,
                cross_norm: take_norm(&mut loaded, &format!("{base}.encoder_attn_layer_norm"))?,
                fc1: take_lin(&mut loaded, &format!("{base}.fc1"))?,
                fc2: take_lin(&mut loaded, &format!("{base}.fc2"))?,
                norm: take_norm(&mut loaded, &format!("{base}.final_layer_norm"))?,
            });
        }
        let tables = table_names
            .iter()
            .map(|n| loaded.take(n))
            .collect::<Result<Vec<_>, _>>()?;
        let logits_bias = if has_bias {
            Some(loaded.take(&bias_name)?)
        } else {
            None
        };
        let positions = sinusoidal(config.max_positions, d);
        Ok(Model {
            config,
            tables,
            enc_embed,
            dec_embed,
            lm_head,
            logits_bias,
            positions,
            encoder,
            decoder,
        })
    }

    /// Memory taken by the weights (bytes).
    pub fn weight_bytes(&self) -> usize {
        let lin = |l: &Linear| l.w.bytes() + l.b.bytes();
        let norm = |n: &Norm| n.w.bytes() + n.b.bytes();
        let attn = |a: &Attention| lin(&a.q) + lin(&a.k) + lin(&a.v) + lin(&a.out);
        self.tables.iter().map(Tensor::bytes).sum::<usize>()
            + self.logits_bias.as_ref().map_or(0, Tensor::bytes)
            + self.positions.len() * 4
            + self
                .encoder
                .iter()
                .map(|l| {
                    attn(&l.attn) + norm(&l.attn_norm) + lin(&l.fc1) + lin(&l.fc2) + norm(&l.norm)
                })
                .sum::<usize>()
            + self
                .decoder
                .iter()
                .map(|l| {
                    attn(&l.attn)
                        + norm(&l.attn_norm)
                        + attn(&l.cross)
                        + norm(&l.cross_norm)
                        + lin(&l.fc1)
                        + lin(&l.fc2)
                        + norm(&l.norm)
                })
                .sum::<usize>()
    }

    /// Embeddings of `tokens` (scaled as configured) plus the position
    /// embeddings: token `i` at position `at(i)`.
    fn embed(&self, table: usize, tokens: &[u32], at: impl Fn(usize) -> usize) -> Vec<f32> {
        let d = self.config.d_model;
        let scale = if self.config.scale_embedding {
            (d as f32).sqrt()
        } else {
            1.0
        };
        let mut x = vec![0f32; tokens.len() * d];
        for (i, &t) in tokens.iter().enumerate() {
            let row = &mut x[i * d..(i + 1) * d];
            self.tables[table].row_into(t as usize, row);
            let p = at(i);
            for (v, p) in row.iter_mut().zip(&self.positions[p * d..(p + 1) * d]) {
                *v = *v * scale + p;
            }
        }
        x
    }

    /// Longest input or output (tokens).
    pub fn max_positions(&self) -> usize {
        self.config.max_positions
    }

    /// Runs the encoder over `tokens` (at most `max_positions`, each below
    /// the vocabulary size) and prepares the cross attention.
    pub fn encode(&self, tokens: &[u32]) -> Encoded {
        let c = &self.config;
        let (d, n) = (c.d_model, tokens.len());
        assert!(n <= c.max_positions && tokens.iter().all(|&t| (t as usize) < c.vocab_size));
        let scale = 1.0 / ((d / c.encoder_heads) as f32).sqrt();
        let mut x = self.embed(self.enc_embed, tokens, |i| i);
        for l in &self.encoder {
            let mut q = l.attn.q.apply(&x, n);
            q.iter_mut().for_each(|v| *v *= scale);
            let k = l.attn.k.apply(&x, n);
            let v = l.attn.v.apply(&x, n);
            let a = attend(&q, &k, &v, n, n, d, c.encoder_heads);
            add(&mut x, &l.attn.out.apply(&a, n));
            l.attn_norm.apply(&mut x);
            let mut h = l.fc1.apply(&x, n);
            activate(c.activation, &mut h);
            add(&mut x, &l.fc2.apply(&h, n));
            l.norm.apply(&mut x);
        }
        let (keys, values) = self
            .decoder
            .iter()
            .map(|l| (l.cross.k.apply(&x, n), l.cross.v.apply(&x, n)))
            .unzip();
        Encoded {
            len: n,
            keys,
            values,
        }
    }

    /// One decoder step for several hypotheses: `tokens[i]` is the last
    /// token of hypothesis `i` (at position `pos`), whose earlier keys and
    /// values are in `caches[i]` (extended here). Returns the logits
    /// (`tokens.len()` rows of the vocabulary size).
    pub fn step(
        &self,
        enc: &Encoded,
        caches: &mut [Cache],
        tokens: &[u32],
        pos: usize,
    ) -> Vec<f32> {
        let c = &self.config;
        let (d, b) = (c.d_model, tokens.len());
        assert!(caches.len() == b && pos < c.max_positions);
        assert!(tokens.iter().all(|&t| (t as usize) < c.vocab_size));
        let heads = c.decoder_heads;
        let scale = 1.0 / ((d / heads) as f32).sqrt();
        let mut x = self.embed(self.dec_embed, tokens, |_| pos);
        for cache in caches.iter_mut() {
            cache.keys.resize(self.decoder.len(), Vec::new());
            cache.values.resize(self.decoder.len(), Vec::new());
        }
        for (li, l) in self.decoder.iter().enumerate() {
            let mut q = l.attn.q.apply(&x, b);
            q.iter_mut().for_each(|v| *v *= scale);
            let k = l.attn.k.apply(&x, b);
            let v = l.attn.v.apply(&x, b);
            let mut a = vec![0f32; b * d];
            for (i, cache) in caches.iter_mut().enumerate() {
                cache.keys[li].extend_from_slice(&k[i * d..(i + 1) * d]);
                cache.values[li].extend_from_slice(&v[i * d..(i + 1) * d]);
                let nk = cache.keys[li].len() / d;
                let o = attend(
                    &q[i * d..(i + 1) * d],
                    &cache.keys[li],
                    &cache.values[li],
                    1,
                    nk,
                    d,
                    heads,
                );
                a[i * d..(i + 1) * d].copy_from_slice(&o);
            }
            add(&mut x, &l.attn.out.apply(&a, b));
            l.attn_norm.apply(&mut x);
            let mut q = l.cross.q.apply(&x, b);
            q.iter_mut().for_each(|v| *v *= scale);
            let a = attend(&q, &enc.keys[li], &enc.values[li], b, enc.len, d, heads);
            add(&mut x, &l.cross.out.apply(&a, b));
            l.cross_norm.apply(&mut x);
            let mut h = l.fc1.apply(&x, b);
            activate(c.activation, &mut h);
            add(&mut x, &l.fc2.apply(&h, b));
            l.norm.apply(&mut x);
        }
        linear(&x, b, &self.tables[self.lm_head], self.logits_bias.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sinusoidal_positions() {
        let p = sinusoidal(50, 8);
        // Position 0: sines 0, cosines 1.
        assert_eq!(&p[..8], &[0.0, 0.0, 0.0, 0.0, 1.0, 1.0, 1.0, 1.0]);
        // Position 3, i = 1: angle 3 / 10000^(2/8) = 0.3.
        let row = &p[3 * 8..4 * 8];
        assert!((row[1] - 0.3f32.sin()).abs() < 1e-6);
        assert!((row[5] - 0.3f32.cos()).abs() < 1e-6);
        assert!((row[0] - 3f32.sin()).abs() < 1e-6);
        // Odd dimensions: the sines take the extra column.
        let p = sinusoidal(2, 5);
        assert_eq!(&p[..5], &[0.0, 0.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn attention_averages_values() {
        // Equal scores: the mean of the values.
        let q = [0.0, 0.0];
        let k = [1.0, 2.0, 3.0, 4.0];
        let v = [1.0, 10.0, 3.0, 20.0];
        assert_eq!(attend(&q, &k, &v, 1, 2, 2, 2), vec![2.0, 15.0]);
        // A strong match picks one value per head: head 0 scores the keys
        // 100 and 0 (first value), head 1 scores them 0 and -100 (first
        // value again); with the second key matching head 1, its value.
        let k = [1.0, 0.0, 0.0, 1.0];
        let o = attend(&[100.0, -100.0], &k, &v, 1, 2, 2, 2);
        assert!(
            (o[0] - 1.0).abs() < 1e-4 && (o[1] - 10.0).abs() < 1e-4,
            "{o:?}"
        );
        let o = attend(&[100.0, 100.0], &k, &v, 1, 2, 2, 2);
        assert!(
            (o[0] - 1.0).abs() < 1e-4 && (o[1] - 20.0).abs() < 1e-4,
            "{o:?}"
        );
    }

    #[test]
    fn swish() {
        let mut x = [0.0, 1.0, -1.0];
        activate(Activation::Swish, &mut x);
        assert_eq!(x[0], 0.0);
        assert!((x[1] - 0.731_058_6).abs() < 1e-6);
        assert!((x[2] + 0.268_941_4).abs() < 1e-6);
    }

    #[test]
    fn parses_the_config() {
        let cfg = r#"{"d_model": 512, "encoder_layers": 6, "decoder_layers": 6,
            "encoder_attention_heads": 8, "decoder_attention_heads": 8,
            "encoder_ffn_dim": 2048, "decoder_ffn_dim": 2048, "vocab_size": 58101,
            "max_position_embeddings": 512, "activation_function": "swish",
            "scale_embedding": true, "normalize_before": false,
            "decoder_start_token_id": 58100, "eos_token_id": 0, "pad_token_id": 58100,
            "num_beams": 4, "max_length": 512, "bad_words_ids": [[58100]]}"#;
        let c = Config::parse(cfg, Some(r#"{"num_beams": 6}"#)).unwrap();
        assert_eq!(
            (c.d_model, c.vocab_size, c.pad, c.eos),
            (512, 58101, 58100, 0)
        );
        assert_eq!((c.num_beams, c.bad_words.clone()), (6, vec![58100]));
        assert_eq!(c.activation, Activation::Swish);
        assert!(Config::parse(&cfg.replace("\"swish\"", "\"tanh\""), None).is_err());
        assert!(Config::parse(&cfg.replace("58101", "0"), None).is_err());
        assert!(Config::parse(&cfg.replace("\"d_model\": 512", "\"d_model\": 500"), None).is_err());
        assert!(Config::parse("[1]", None).is_err());
    }
}
