//! The wav2vec2 network with its CTC head (as `Wav2Vec2ForCTC` in Hugging
//! Face `transformers`, in eval mode): a stack of 1-D convolutions turns
//! 16 kHz samples into one feature vector per 20 ms, a convolution adds
//! relative positions, a transformer encoder follows and a linear layer
//! gives the logits of the characters.
//!
//! Weight matrices are kept as f16 (or f32) and multiplied with the
//! kernels of `bl_asr::ops`; vectors (biases, norms) stay f32. The
//! convolutions are computed as matrix products of their input windows.
//! The feature encoder runs over the audio in pieces (each output frame
//! only sees 400 samples), so its memory stays small for long inputs.

use crate::config::{Config, FeatNorm};
use crate::weights::Weights;
use bl_asr::model::{Store, Tensor};
use bl_asr::ops::{layer_norm, linear, softmax, threads};
use bl_mt::torch::{f32_to_f16, Keep};
use std::collections::HashMap;

/// Output frames of the feature encoder computed at a time.
const CHUNK_FRAMES: usize = 128;
/// Input windows put side by side for one convolution product (values).
const WINDOW_VALUES: usize = 1 << 22;
/// Epsilon of the input scaling and of the group norm.
const INPUT_EPS: f64 = 1e-7;
const GROUP_EPS: f64 = 1e-5;

struct Linear {
    w: Tensor,
    b: Option<Tensor>,
}

impl Linear {
    fn apply(&self, x: &[f32], n: usize) -> Vec<f32> {
        linear(x, n, &self.w, self.b.as_ref())
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

/// A convolution of the feature encoder. Its weights are one row per
/// output channel of `kernel * in` values (kernel position major), so an
/// input window of time-major frames multiplies it directly.
struct ConvLayer {
    kernel: usize,
    stride: usize,
    inputs: usize,
    conv: Linear,
    /// Group norm (first layer of the base models) or layer norm.
    norm: Option<Norm>,
}

/// The positional convolution: one weight matrix per group, laid out
/// like those of [`ConvLayer`].
struct PosConv {
    kernel: usize,
    groups: Vec<Tensor>,
    b: Tensor,
}

struct Layer {
    q: Linear,
    k: Linear,
    v: Linear,
    out: Linear,
    attn_norm: Norm,
    ff1: Linear,
    ff2: Linear,
    final_norm: Norm,
}

pub struct Model {
    pub config: Config,
    convs: Vec<ConvLayer>,
    proj_norm: Norm,
    proj: Linear,
    pos: PosConv,
    enc_norm: Norm,
    layers: Vec<Layer>,
    lm_head: Linear,
}

/// Collects the tensors to load (with their expected shape) and takes
/// them out after loading.
struct Plan {
    wanted: Vec<(String, Keep, usize, usize)>,
}

impl Plan {
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

/// `w` (`out` rows of `inputs * kernel` values, input channel major as
/// torch stores convolutions) with each row reordered to kernel position
/// major, kept as `keep`.
fn conv_matrix(w: &[f32], out: usize, inputs: usize, kernel: usize, keep: Keep) -> Tensor {
    let mut m = vec![0f32; out * inputs * kernel];
    for o in 0..out {
        let src = &w[o * inputs * kernel..(o + 1) * inputs * kernel];
        let dst = &mut m[o * inputs * kernel..(o + 1) * inputs * kernel];
        for i in 0..inputs {
            for k in 0..kernel {
                dst[k * inputs + i] = src[i * kernel + k];
            }
        }
    }
    keep_as(m, inputs * kernel, out, keep)
}

fn keep_as(data: Vec<f32>, cols: usize, rows: usize, keep: Keep) -> Tensor {
    match keep {
        Keep::F32 => Tensor::from_f32(cols, rows, data),
        Keep::F16 => Tensor::new(
            cols,
            rows,
            Store::F16(data.into_iter().map(f32_to_f16).collect()),
        )
        .expect("matrix size"),
    }
}

/// The weight of a weight-normed convolution: `g * v / |v|`, the norm
/// taken over all dimensions but `dim` (the one where `g` is not 1).
fn weight_norm(
    g: &[f32],
    g_shape: &[usize],
    v: &[f32],
    v_shape: &[usize],
) -> Result<Vec<f32>, String> {
    let bad = || "model: unsupported weight norm of the positional convolution".to_string();
    if g_shape.len() != v_shape.len() {
        return Err(bad());
    }
    let dims: Vec<usize> = (0..g_shape.len()).filter(|&d| g_shape[d] != 1).collect();
    let dim = match dims.as_slice() {
        [d] if g_shape[*d] == v_shape[*d] => *d,
        _ => return Err(bad()),
    };
    // Elements of v: index along `dim` is (flat / inner) % size.
    let inner: usize = v_shape[dim + 1..].iter().product();
    let size = v_shape[dim];
    let mut sums = vec![0f64; size];
    for (i, &x) in v.iter().enumerate() {
        sums[(i / inner) % size] += x as f64 * x as f64;
    }
    let scale: Vec<f32> = sums
        .iter()
        .zip(g)
        .map(|(s, &g)| {
            if *s > 0.0 {
                (g as f64 / s.sqrt()) as f32
            } else {
                0.0
            }
        })
        .collect();
    Ok(v.iter()
        .enumerate()
        .map(|(i, &x)| x * scale[(i / inner) % size])
        .collect())
}

/// Splits `x` into pieces for the threads (when large) and runs `f` on
/// each.
fn par_chunks(x: &mut [f32], f: impl Fn(&mut [f32]) + Sync) {
    let n = threads();
    if n == 1 || x.len() < 1 << 16 {
        f(x);
        return;
    }
    let size = x.len().div_ceil(n);
    std::thread::scope(|s| {
        for c in x.chunks_mut(size) {
            let f = &f;
            s.spawn(move || f(c));
        }
    });
}

/// erf(x) within 1.5e-7 (Abramowitz and Stegun 7.1.26).
fn erf(x: f32) -> f32 {
    let a = x.abs() as f64;
    let t = 1.0 / (1.0 + 0.327_591_1 * a);
    let poly = t
        * (0.254_829_592
            + t * (-0.284_496_736
                + t * (1.421_413_741 + t * (-1.453_152_027 + t * 1.061_405_429))));
    let r = (1.0 - poly * (-a * a).exp()) as f32;
    r.copysign(x)
}

/// The exact GELU, `x * (1 + erf(x / sqrt 2)) / 2` (as these models are
/// trained with; the tanh approximation differs by up to 1e-3).
pub fn gelu_exact(x: &mut [f32]) {
    par_chunks(x, |c| {
        for v in c {
            *v *= 0.5 * (1.0 + erf(*v * std::f32::consts::FRAC_1_SQRT_2));
        }
    });
}

fn add(x: &mut [f32], y: &[f32]) {
    for (a, b) in x.iter_mut().zip(y) {
        *a += b;
    }
}

/// `n` output frames of a convolution (no bias, no padding) over
/// time-major `x` frames of `inputs` channels.
fn convolve(
    x: &[f32],
    inputs: usize,
    n: usize,
    kernel: usize,
    stride: usize,
    w: &Linear,
) -> Vec<f32> {
    let width = kernel * inputs;
    let block = (WINDOW_VALUES / width).max(1);
    let mut out = Vec::with_capacity(n * w.w.rows);
    let mut windows = Vec::new();
    for t0 in (0..n).step_by(block) {
        let t1 = (t0 + block).min(n);
        windows.clear();
        for t in t0..t1 {
            windows.extend_from_slice(&x[t * stride * inputs..][..width]);
        }
        out.extend(w.apply(&windows, t1 - t0));
    }
    out
}

/// Per-channel mean and 1 / standard deviation for the group norm.
struct GroupStats {
    mean: Vec<f32>,
    inv_std: Vec<f32>,
}

impl Model {
    /// Loads the weights: matrices kept as `keep`, vectors f32.
    pub fn load(config: Config, weights: &Weights, keep: Keep) -> Result<Model, String> {
        let c = &config;
        let d = c.hidden;
        let p = if weights
            .shape("wav2vec2.feature_projection.projection.weight")
            .is_some()
        {
            "wav2vec2."
        } else {
            ""
        };
        let mut plan = Plan { wanted: Vec::new() };
        let lin = |plan: &mut Plan, base: &str, out: usize, inp: usize, keep: Keep| {
            plan.add(format!("{base}.weight"), keep, out, inp);
            plan.add(format!("{base}.bias"), Keep::F32, 1, out);
        };
        let norm = |plan: &mut Plan, base: &str, dim: usize| {
            plan.add(format!("{base}.weight"), Keep::F32, 1, dim);
            plan.add(format!("{base}.bias"), Keep::F32, 1, dim);
        };
        let mut inputs = 1;
        for (i, conv) in c.conv.iter().enumerate() {
            let base = format!("{p}feature_extractor.conv_layers.{i}");
            plan.add(
                format!("{base}.conv.weight"),
                Keep::F32,
                conv.dim * inputs,
                conv.kernel,
            );
            if c.conv_bias {
                plan.add(format!("{base}.conv.bias"), Keep::F32, 1, conv.dim);
            }
            if c.feat_norm == FeatNorm::Layer || i == 0 {
                norm(&mut plan, &format!("{base}.layer_norm"), conv.dim);
            }
            inputs = conv.dim;
        }
        let last = inputs;
        norm(
            &mut plan,
            &format!("{p}feature_projection.layer_norm"),
            last,
        );
        lin(
            &mut plan,
            &format!("{p}feature_projection.projection"),
            d,
            last,
            keep,
        );
        let gs = d / c.pos_groups;
        let pos = format!("{p}encoder.pos_conv_embed.conv");
        let (g_name, v_name) = if weights.shape(&format!("{pos}.weight_g")).is_some() {
            (format!("{pos}.weight_g"), format!("{pos}.weight_v"))
        } else {
            (
                format!("{pos}.parametrizations.weight.original0"),
                format!("{pos}.parametrizations.weight.original1"),
            )
        };
        let plain = weights.shape(&g_name).is_none();
        let v_name = if plain {
            format!("{pos}.weight")
        } else {
            v_name
        };
        let v_shape = weights
            .shape(&v_name)
            .ok_or_else(|| format!("model: missing tensor {v_name}"))?;
        if v_shape != [d, gs, c.pos_kernel] {
            return Err(format!("model: {v_name} has shape {v_shape:?}"));
        }
        plan.add(v_name.clone(), Keep::F32, d * gs, c.pos_kernel);
        let g_shape = if plain {
            Vec::new()
        } else {
            let s = weights.shape(&g_name).unwrap_or_default();
            // Loaded as rows of the last dimension.
            let (rows, cols) = match s.split_last() {
                None => (1, 1),
                Some((&cols, rest)) => (rest.iter().product(), cols),
            };
            plan.add(g_name.clone(), Keep::F32, rows, cols);
            s
        };
        plan.add(format!("{pos}.bias"), Keep::F32, 1, d);
        norm(&mut plan, &format!("{p}encoder.layer_norm"), d);
        for l in 0..c.layers {
            let base = format!("{p}encoder.layers.{l}");
            for proj in ["q_proj", "k_proj", "v_proj", "out_proj"] {
                lin(&mut plan, &format!("{base}.attention.{proj}"), d, d, keep);
            }
            norm(&mut plan, &format!("{base}.layer_norm"), d);
            lin(
                &mut plan,
                &format!("{base}.feed_forward.intermediate_dense"),
                c.ffn,
                d,
                keep,
            );
            lin(
                &mut plan,
                &format!("{base}.feed_forward.output_dense"),
                d,
                c.ffn,
                keep,
            );
            norm(&mut plan, &format!("{base}.final_layer_norm"), d);
        }
        // The head is small: kept as f32.
        lin(&mut plan, "lm_head", c.vocab_size, d, Keep::F32);

        let wanted: Vec<(&str, Keep)> = plan
            .wanted
            .iter()
            .map(|(n, k, _, _)| (n.as_str(), *k))
            .collect();
        let mut loaded = Loaded(weights.load(&wanted)?);
        for (name, _, rows, cols) in &plan.wanted {
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
                b: Some(l.take(&format!("{base}.bias"))?),
            })
        };
        let take_norm = |l: &mut Loaded, base: &str| -> Result<Norm, String> {
            Ok(Norm {
                w: l.take(&format!("{base}.weight"))?,
                b: l.take(&format!("{base}.bias"))?,
            })
        };
        let mut convs = Vec::new();
        let mut inputs = 1;
        for (i, conv) in c.conv.iter().enumerate() {
            let base = format!("{p}feature_extractor.conv_layers.{i}");
            let w = loaded.take(&format!("{base}.conv.weight"))?;
            let w = conv_matrix(w.values(), conv.dim, inputs, conv.kernel, keep);
            let b = if c.conv_bias {
                Some(loaded.take(&format!("{base}.conv.bias"))?)
            } else {
                None
            };
            let norm = if c.feat_norm == FeatNorm::Layer || i == 0 {
                Some(take_norm(&mut loaded, &format!("{base}.layer_norm"))?)
            } else {
                None
            };
            convs.push(ConvLayer {
                kernel: conv.kernel,
                stride: conv.stride,
                inputs,
                conv: Linear { w, b },
                norm,
            });
            inputs = conv.dim;
        }
        let proj_norm = take_norm(&mut loaded, &format!("{p}feature_projection.layer_norm"))?;
        let proj = take_lin(&mut loaded, &format!("{p}feature_projection.projection"))?;
        let v = loaded.take(&v_name)?;
        let weight = if plain {
            v.values().to_vec()
        } else {
            let g = loaded.take(&g_name)?;
            weight_norm(g.values(), &g_shape, v.values(), &v_shape)?
        };
        let k = c.pos_kernel;
        let groups = (0..c.pos_groups)
            .map(|g| {
                conv_matrix(
                    &weight[g * gs * gs * k..(g + 1) * gs * gs * k],
                    gs,
                    gs,
                    k,
                    keep,
                )
            })
            .collect();
        let pos = PosConv {
            kernel: k,
            groups,
            b: loaded.take(&format!("{pos}.bias"))?,
        };
        let enc_norm = take_norm(&mut loaded, &format!("{p}encoder.layer_norm"))?;
        let mut layers = Vec::with_capacity(c.layers);
        for l in 0..c.layers {
            let base = format!("{p}encoder.layers.{l}");
            let a = format!("{base}.attention");
            layers.push(Layer {
                q: take_lin(&mut loaded, &format!("{a}.q_proj"))?,
                k: take_lin(&mut loaded, &format!("{a}.k_proj"))?,
                v: take_lin(&mut loaded, &format!("{a}.v_proj"))?,
                out: take_lin(&mut loaded, &format!("{a}.out_proj"))?,
                attn_norm: take_norm(&mut loaded, &format!("{base}.layer_norm"))?,
                ff1: take_lin(
                    &mut loaded,
                    &format!("{base}.feed_forward.intermediate_dense"),
                )?,
                ff2: take_lin(&mut loaded, &format!("{base}.feed_forward.output_dense"))?,
                final_norm: take_norm(&mut loaded, &format!("{base}.final_layer_norm"))?,
            });
        }
        let lm_head = take_lin(&mut loaded, "lm_head")?;
        Ok(Model {
            config,
            convs,
            proj_norm,
            proj,
            pos,
            enc_norm,
            layers,
            lm_head,
        })
    }

    /// Memory taken by the weights (bytes).
    pub fn weight_bytes(&self) -> usize {
        let lin = |l: &Linear| l.w.bytes() + l.b.as_ref().map_or(0, Tensor::bytes);
        let norm = |n: &Norm| n.w.bytes() + n.b.bytes();
        let convs: usize = self
            .convs
            .iter()
            .map(|c| lin(&c.conv) + c.norm.as_ref().map_or(0, norm))
            .sum();
        let layers: usize = self
            .layers
            .iter()
            .map(|l| {
                lin(&l.q)
                    + lin(&l.k)
                    + lin(&l.v)
                    + lin(&l.out)
                    + lin(&l.ff1)
                    + lin(&l.ff2)
                    + norm(&l.attn_norm)
                    + norm(&l.final_norm)
            })
            .sum();
        let pos: usize =
            self.pos.groups.iter().map(Tensor::bytes).sum::<usize>() + self.pos.b.bytes();
        convs
            + layers
            + pos
            + norm(&self.proj_norm)
            + lin(&self.proj)
            + norm(&self.enc_norm)
            + lin(&self.lm_head)
    }

    /// The logits of 16 kHz `samples`: one row of `vocab_size` per output
    /// frame (none if the input is shorter than one frame's window).
    pub fn logits(&self, samples: &[f32]) -> Vec<f32> {
        let frames = self.config.frames(samples.len());
        if frames == 0 {
            return Vec::new();
        }
        let x = self.prepare(samples);
        let mut h = self.features(&x, frames);
        self.encode(&mut h, frames);
        self.lm_head.apply(&h, frames)
    }

    /// The input scaled to zero mean and unit variance (if the model
    /// wants it).
    fn prepare(&self, samples: &[f32]) -> Vec<f32> {
        if !self.config.normalize {
            return samples.to_vec();
        }
        let n = samples.len() as f64;
        let mean = samples.iter().map(|&v| v as f64).sum::<f64>() / n;
        let var = samples
            .iter()
            .map(|&v| (v as f64 - mean) * (v as f64 - mean))
            .sum::<f64>()
            / n;
        let inv = 1.0 / (var + INPUT_EPS).sqrt();
        samples
            .iter()
            .map(|&v| ((v as f64 - mean) * inv) as f32)
            .collect()
    }

    /// The projected features (`frames` rows of `hidden`), computed piece
    /// by piece.
    fn features(&self, x: &[f32], frames: usize) -> Vec<f32> {
        let stats = match self.config.feat_norm {
            FeatNorm::Group => Some(self.group_stats(x)),
            FeatNorm::Layer => None,
        };
        let hop = self.config.hop();
        let field = self.config.receptive_field();
        let mut out = Vec::with_capacity(frames * self.config.hidden);
        for j0 in (0..frames).step_by(CHUNK_FRAMES) {
            let j1 = (j0 + CHUNK_FRAMES).min(frames);
            let piece = &x[j0 * hop..(j1 - 1) * hop + field];
            let mut h = self.conv_stack(piece, stats.as_ref());
            debug_assert_eq!(
                h.len(),
                (j1 - j0) * self.convs.last().map_or(1, |c| c.conv.w.rows)
            );
            self.proj_norm.apply(&mut h);
            out.extend(self.proj.apply(&h, j1 - j0));
        }
        out
    }

    /// The feature encoder over the samples `x`: time-major frames.
    fn conv_stack(&self, x: &[f32], stats: Option<&GroupStats>) -> Vec<f32> {
        let mut h = x.to_vec();
        let mut len = x.len();
        for (i, l) in self.convs.iter().enumerate() {
            let n = (len - l.kernel) / l.stride + 1;
            let dim = l.conv.w.rows;
            let mut out = convolve(&h, l.inputs, n, l.kernel, l.stride, &l.conv);
            match (&l.norm, stats) {
                (Some(norm), Some(s)) if i == 0 => {
                    let (w, b) = (norm.w.values(), norm.b.values());
                    for row in out.chunks_mut(dim) {
                        for (c, v) in row.iter_mut().enumerate() {
                            *v = (*v - s.mean[c]) * s.inv_std[c] * w[c] + b[c];
                        }
                    }
                }
                (Some(norm), None) => norm.apply(&mut out),
                _ => {}
            }
            gelu_exact(&mut out);
            h = out;
            len = n;
        }
        h
    }

    /// Mean and spread of each channel of the first convolution over the
    /// whole input (the group norm of the base models).
    fn group_stats(&self, x: &[f32]) -> GroupStats {
        let l = &self.convs[0];
        let dim = l.conv.w.rows;
        let n = (x.len() - l.kernel) / l.stride + 1;
        let mut sum = vec![0f64; dim];
        let mut sq = vec![0f64; dim];
        let block = 8192;
        for t0 in (0..n).step_by(block) {
            let t1 = (t0 + block).min(n);
            let part = &x[t0 * l.stride..(t1 - 1) * l.stride + l.kernel];
            let out = convolve(part, 1, t1 - t0, l.kernel, l.stride, &l.conv);
            for row in out.chunks(dim) {
                for (c, &v) in row.iter().enumerate() {
                    sum[c] += v as f64;
                    sq[c] += v as f64 * v as f64;
                }
            }
        }
        let mut mean = Vec::with_capacity(dim);
        let mut inv_std = Vec::with_capacity(dim);
        for c in 0..dim {
            let m = sum[c] / n as f64;
            let var = (sq[c] / n as f64 - m * m).max(0.0);
            mean.push(m as f32);
            inv_std.push((1.0 / (var + GROUP_EPS).sqrt()) as f32);
        }
        GroupStats { mean, inv_std }
    }

    /// The positional convolution of `h` (`n` frames): grouped, padded by
    /// half the kernel on both sides, the last frame dropped for an even
    /// kernel, then GELU.
    fn positions(&self, h: &[f32], n: usize) -> Vec<f32> {
        let d = self.config.hidden;
        let k = self.pos.kernel;
        let pad = k / 2;
        let gs = d / self.pos.groups.len();
        let mut out = vec![0f32; n * d];
        let block = (WINDOW_VALUES / (k * gs)).max(1);
        let mut windows = Vec::new();
        for (g, w) in self.pos.groups.iter().enumerate() {
            for t0 in (0..n).step_by(block) {
                let t1 = (t0 + block).min(n);
                windows.clear();
                for t in t0..t1 {
                    for j in 0..k {
                        // Input frame t - pad + j (zero outside).
                        match (t + j).checked_sub(pad).filter(|&s| s < n) {
                            Some(s) => windows.extend_from_slice(&h[s * d + g * gs..][..gs]),
                            None => windows.resize(windows.len() + gs, 0.0),
                        }
                    }
                }
                let r = linear(&windows, t1 - t0, w, None);
                for (t, row) in (t0..t1).zip(r.chunks(gs)) {
                    out[t * d + g * gs..][..gs].copy_from_slice(row);
                }
            }
        }
        for row in out.chunks_mut(d) {
            add(row, self.pos.b.values());
        }
        gelu_exact(&mut out);
        out
    }

    /// The transformer encoder over `h` (`n` frames, in place).
    fn encode(&self, h: &mut [f32], n: usize) {
        let pos = self.positions(h, n);
        add(h, &pos);
        let stable = self.config.stable_layer_norm;
        if !stable {
            self.enc_norm.apply(h);
        }
        for l in &self.layers {
            if stable {
                let mut x = h.to_vec();
                l.attn_norm.apply(&mut x);
                let a = self.attention(l, &x, n);
                add(h, &a);
                let mut x = h.to_vec();
                l.final_norm.apply(&mut x);
                let f = self.feed_forward(l, &x, n);
                add(h, &f);
            } else {
                let a = self.attention(l, h, n);
                add(h, &a);
                l.attn_norm.apply(h);
                let f = self.feed_forward(l, h, n);
                add(h, &f);
                l.final_norm.apply(h);
            }
        }
        if stable {
            self.enc_norm.apply(h);
        }
    }

    fn feed_forward(&self, l: &Layer, x: &[f32], n: usize) -> Vec<f32> {
        let mut f = l.ff1.apply(x, n);
        gelu_exact(&mut f);
        l.ff2.apply(&f, n)
    }

    /// Self-attention of the `n` frames `x` to each other.
    fn attention(&self, l: &Layer, x: &[f32], n: usize) -> Vec<f32> {
        let d = self.config.hidden;
        let heads = self.config.heads;
        let hd = d / heads;
        let scale = 1.0 / (hd as f32).sqrt();
        let q = l.q.apply(x, n);
        let k = l.k.apply(x, n);
        let v = l.v.apply(x, n);
        let mut o = vec![0f32; n * d];
        let column = |m: &[f32], h: usize| -> Vec<f32> {
            m.chunks(d)
                .flat_map(|row| row[h * hd..(h + 1) * hd].iter().copied())
                .collect()
        };
        for h in 0..heads {
            let mut qh = column(&q, h);
            qh.iter_mut().for_each(|v| *v *= scale);
            let kh = Tensor::from_f32(hd, n, column(&k, h));
            let mut scores = linear(&qh, n, &kh, None);
            par_chunks_rows(&mut scores, n, softmax);
            // The values transposed: one row per head dimension.
            let mut vt = vec![0f32; hd * n];
            for (t, row) in v.chunks(d).enumerate() {
                for (j, &val) in row[h * hd..(h + 1) * hd].iter().enumerate() {
                    vt[j * n + t] = val;
                }
            }
            let vt = Tensor::from_f32(n, hd, vt);
            let oh = linear(&scores, n, &vt, None);
            for (t, row) in oh.chunks(hd).enumerate() {
                o[t * d + h * hd..][..hd].copy_from_slice(row);
            }
        }
        l.out.apply(&o, n)
    }
}

/// Runs `f` on each row of `width` values of `x`, rows spread over the
/// threads.
fn par_chunks_rows(x: &mut [f32], width: usize, f: fn(&mut [f32])) {
    let n = threads();
    let rows = x.len() / width.max(1);
    if n == 1 || x.len() < 1 << 16 || rows < 2 {
        x.chunks_mut(width).for_each(f);
        return;
    }
    let per = rows.div_ceil(n) * width;
    std::thread::scope(|s| {
        for c in x.chunks_mut(per) {
            s.spawn(move || c.chunks_mut(width).for_each(f));
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erf_and_gelu_match_known_values() {
        for (x, e) in [
            (0.0f32, 0.0f32),
            (0.5, 0.520_499_9),
            (1.0, 0.842_700_8),
            (2.0, 0.995_322_3),
            (-1.0, -0.842_700_8),
        ] {
            assert!((erf(x) - e).abs() < 2e-7, "erf({x}) = {}", erf(x));
        }
        let mut g = [-3.0f32, -1.0, 0.0, 1.0, 3.0];
        gelu_exact(&mut g);
        let want = [-0.004_049_7f32, -0.158_655_3, 0.0, 0.841_344_7, 2.995_950_3];
        for (a, b) in g.iter().zip(want) {
            assert!((a - b).abs() < 1e-6, "{a} vs {b}");
        }
    }

    #[test]
    fn weight_norm_over_the_last_dimension() {
        // v: [2, 1, 2]; g: [1, 1, 2] (norm over dims 0 and 1).
        let v = [3.0, 1.0, 4.0, 0.0];
        let w = weight_norm(&[10.0, 2.0], &[1, 1, 2], &v, &[2, 1, 2]).unwrap();
        assert_eq!(w, vec![6.0, 2.0, 8.0, 0.0]);
        // g: [2, 1, 1] (norm per output channel).
        let w = weight_norm(&[5.0, 1.0], &[2, 1, 1], &v, &[2, 1, 2]).unwrap();
        let r = 10f32.sqrt();
        assert!((w[0] - 15.0 / r).abs() < 1e-6 && (w[1] - 5.0 / r).abs() < 1e-6);
        assert_eq!(&w[2..], &[1.0, 0.0]);
        assert!(weight_norm(&[1.0], &[1, 1], &v, &[2, 1, 2]).is_err());
    }

    #[test]
    fn convolution_matches_the_direct_sum() {
        // 2 input channels, 3 output channels, kernel 3, stride 2.
        let (inputs, outs, k, s) = (2, 3, 3, 2);
        let w: Vec<f32> = (0..outs * inputs * k)
            .map(|i| (i as f32 * 0.37).sin())
            .collect();
        let x: Vec<f32> = (0..9 * inputs).map(|i| (i as f32 * 0.11).cos()).collect();
        let lin = Linear {
            w: conv_matrix(&w, outs, inputs, k, Keep::F32),
            b: None,
        };
        let n = (9 - k) / s + 1;
        let got = convolve(&x, inputs, n, k, s, &lin);
        for t in 0..n {
            for o in 0..outs {
                let mut want = 0.0;
                for i in 0..inputs {
                    for j in 0..k {
                        want += w[(o * inputs + i) * k + j] * x[(t * s + j) * inputs + i];
                    }
                }
                assert!((got[t * outs + o] - want).abs() < 1e-5);
            }
        }
    }
}
