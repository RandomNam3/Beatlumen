//! The audio encoder on the GPU (Vulkan compute through `bl-gpu`): the
//! computation of `net::encode`, with the weights kept in GPU memory
//! between windows (uploaded again only for another model). The decoder
//! stays on the CPU.
//!
//! `net::encode` tries this first and computes on the CPU when it returns
//! nothing: no GPU, the GPU turned off (`net::set_gpu`, or `BL_ASR_GPU=0`
//! in the environment), a model it does not handle, or an error (after
//! which the GPU is not tried again until it is turned off and on).

use crate::model::{Model, Packed, Params, QType, Tensor};
use bl_gpu::{Buffer, Commands, Gpu, Kernel, Shader};
use std::io;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Mutex;

/// Values per attention head (the same in all Whisper models; the
/// attention kernel is written for it).
const HEAD: usize = 64;

/// Matrix product flags of the kernel.
const BIAS: u32 = 1;
const GELU: u32 = 2;
const ADD: u32 = 4;

fn gpu_err(e: bl_gpu::Error) -> io::Error {
    io::Error::other(format!("GPU: {e}"))
}

fn unsupported(what: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, format!("GPU encoder: {what}"))
}

/// `v` as a kernel parameter.
fn word(v: usize) -> io::Result<u32> {
    u32::try_from(v).map_err(|_| unsupported("size out of range"))
}

/// Bytes of `count` f32 values.
fn f32_bytes(count: usize) -> io::Result<u64> {
    count
        .checked_mul(4)
        .map(|b| b as u64)
        .ok_or_else(|| unsupported("size out of range"))
}

/// A weight matrix on the GPU: `rows` rows of `cols` values, in the
/// model's format.
struct Mat {
    buf: Buffer,
    shader: Shader,
    rows: usize,
    cols: usize,
}

/// A vector (bias, layer norm weight) on the GPU.
struct Vector {
    buf: Buffer,
    len: usize,
}

struct Layer {
    attn_ln: (Vector, Vector),
    q: (Mat, Vector),
    k: Mat,
    v: (Mat, Vector),
    out: (Mat, Vector),
    mlp_ln: (Vector, Vector),
    mlp0: (Mat, Vector),
    mlp2: (Mat, Vector),
}

/// What identifies the weights a model had when they were uploaded: the
/// shape, where each tensor's data lies and a sample of the values. A
/// model loaded later at the same addresses with other values differs in
/// the sample.
#[derive(PartialEq)]
struct Id {
    params: Params,
    tensors: Vec<(usize, usize, usize, usize, u64)>,
}

/// The encoder's weights on the GPU.
struct Weights {
    id: Id,
    conv1: (Mat, Vector),
    conv2: (Mat, Vector),
    pos: Mat,
    layers: Vec<Layer>,
    ln_post: (Vector, Vector),
}

/// The buffers for the activations of one window.
struct Acts {
    shape: (usize, usize, usize),
    mel: Buffer,
    /// Convolution patches, then the MLP's hidden values.
    scratch: Buffer,
    conv1: Buffer,
    x: Buffer,
    h: Buffer,
    q: Buffer,
    k: Buffer,
    v: Buffer,
    a: Buffer,
}

/// The names of the encoder's tensors of layer `l`.
fn layer_names(l: usize) -> [String; 15] {
    [
        "attn_ln.weight",
        "attn_ln.bias",
        "attn.query.weight",
        "attn.query.bias",
        "attn.key.weight",
        "attn.value.weight",
        "attn.value.bias",
        "attn.out.weight",
        "attn.out.bias",
        "mlp_ln.weight",
        "mlp_ln.bias",
        "mlp.0.weight",
        "mlp.0.bias",
        "mlp.2.weight",
        "mlp.2.bias",
    ]
    .map(|s| format!("encoder.blocks.{l}.{s}"))
}

const GLOBAL_NAMES: [&str; 7] = [
    "encoder.conv1.weight",
    "encoder.conv1.bias",
    "encoder.conv2.weight",
    "encoder.conv2.bias",
    "encoder.positional_embedding",
    "encoder.ln_post.weight",
    "encoder.ln_post.bias",
];

/// Values sampled per tensor for the [`Id`].
const SAMPLES: usize = 256;

/// Address, length and a hash of evenly spaced values (FNV-1a) of `v`.
fn sampled<T: Copy>(v: &[T], bits: impl Fn(T) -> u64) -> (usize, usize, u64) {
    let mut h = 0xcbf2_9ce4_8422_2325u64;
    if let Some(last) = v.len().checked_sub(1) {
        for s in 0..=SAMPLES {
            let i = (s as u64 * last as u64 / SAMPLES as u64) as usize;
            h = (h ^ bits(v[i])).wrapping_mul(0x0000_0100_0000_01b3);
        }
    }
    (v.as_ptr() as usize, v.len(), h)
}

fn identify(m: &Model) -> io::Result<Id> {
    let mut tensors = Vec::new();
    let mut add = |t: &Tensor| {
        let (addr, len, hash) = match t.packed(0, t.rows) {
            Packed::F32(v) => sampled(v, |x| x.to_bits() as u64),
            Packed::F16(v) => sampled(v, |x| x as u64),
            Packed::Quant(_, v) => sampled(v, |x| x as u64),
        };
        tensors.push((addr, len, t.cols, t.rows, hash));
    };
    for name in GLOBAL_NAMES {
        add(m.get(name)?);
    }
    for l in 0..m.params.n_audio_layer {
        for name in layer_names(l) {
            add(m.get(&name)?);
        }
    }
    Ok(Id {
        params: m.params,
        tensors,
    })
}

/// The compute pipelines, one per kernel used.
struct Kernels {
    matmul: Vec<(Shader, Kernel)>,
    layer_norm: Kernel,
    attention: Kernel,
    im2col: Kernel,
}

impl Kernels {
    fn new(gpu: &Gpu) -> bl_gpu::Result<Kernels> {
        let matmul = [
            Shader::MatmulF32,
            Shader::MatmulF16,
            Shader::MatmulQ8_0,
            Shader::MatmulQ5_0,
            Shader::MatmulQ5_1,
            Shader::MatmulQ4_0,
            Shader::MatmulQ4_1,
        ]
        .into_iter()
        .map(|s| Ok((s, gpu.kernel(s)?)))
        .collect::<bl_gpu::Result<_>>()?;
        Ok(Kernels {
            matmul,
            layer_norm: gpu.kernel(Shader::LayerNorm)?,
            attention: gpu.kernel(Shader::Attention)?,
            im2col: gpu.kernel(Shader::Im2col)?,
        })
    }

    fn matmul(&self, s: Shader) -> &Kernel {
        &self
            .matmul
            .iter()
            .find(|(k, _)| *k == s)
            .expect("a kernel for every matrix format")
            .1
    }
}

/// The GPU, its kernels and what is kept between windows.
pub(crate) struct Encoder {
    gpu: Gpu,
    kernels: Kernels,
    weights: Option<Weights>,
    acts: Option<Acts>,
}

/// Command list helpers: each checks the sizes against the buffers before
/// adding the command (the kernels themselves cannot reach outside their
/// buffers, but wrong sizes would give wrong results).
struct Cmds<'a> {
    c: Commands<'a>,
    k: &'a Kernels,
}

impl<'a> Cmds<'a> {
    /// `y (+)= x Wᵀ + b` for `n` rows; `flags` as in the kernel.
    fn matmul(
        &mut self,
        x: &'a Buffer,
        n: usize,
        w: &'a Mat,
        bias: Option<&'a Vector>,
        y: &'a Buffer,
        flags: u32,
    ) -> io::Result<()> {
        let fits = |b: &Buffer, count: Option<usize>| {
            count.is_some_and(|c| f32_bytes(c).is_ok_and(|bytes| bytes <= b.size()))
        };
        if !fits(x, n.checked_mul(w.cols))
            || !fits(y, n.checked_mul(w.rows))
            || bias.is_some_and(|b| b.len < w.rows)
            || !w.rows.checked_mul(w.cols).is_some_and(|c| c < 1 << 31)
        {
            return Err(unsupported("matrix sizes"));
        }
        let flags = flags | if bias.is_some() { BIAS } else { 0 };
        let push = [word(n)?, word(w.cols)?, word(w.rows)?, flags];
        let groups = [word(w.rows.div_ceil(64))?, word(n.div_ceil(64))?, 1];
        let b = bias.map_or(x, |b| &b.buf);
        self.c
            .dispatch(self.k.matmul(w.shader), &[x, &w.buf, b, y], &push, groups)
            .map_err(gpu_err)
    }

    fn layer_norm(
        &mut self,
        x: &'a Buffer,
        n: usize,
        d: usize,
        ln: &'a (Vector, Vector),
        y: &'a Buffer,
    ) -> io::Result<()> {
        let bytes = f32_bytes(n * d)?;
        if x.size() < bytes || y.size() < bytes || ln.0.len < d || ln.1.len < d {
            return Err(unsupported("layer norm sizes"));
        }
        let push = [word(n)?, word(d)?];
        self.c
            .dispatch(
                &self.k.layer_norm,
                &[x, &ln.0.buf, &ln.1.buf, y],
                &push,
                [word(n)?, 1, 1],
            )
            .map_err(gpu_err)
    }

    /// Patches for the convolution of `t` input rows of `cin` channels
    /// (time-major, or channel-major with `channel_major`), `tout` output
    /// rows.
    #[allow(clippy::too_many_arguments)]
    fn im2col(
        &mut self,
        src: &'a Buffer,
        t: usize,
        cin: usize,
        stride: usize,
        channel_major: bool,
        tout: usize,
        dst: &'a Buffer,
    ) -> io::Result<()> {
        let total = tout
            .checked_mul(cin * 3)
            .filter(|&c| c < 1 << 31)
            .ok_or_else(|| unsupported("convolution size"))?;
        if src.size() < f32_bytes(t * cin)? || dst.size() < f32_bytes(total)? {
            return Err(unsupported("convolution sizes"));
        }
        let groups = total.div_ceil(256);
        let gx = groups.min(65_535);
        let push = [
            word(tout)?,
            word(t)?,
            word(cin)?,
            word(stride)?,
            channel_major as u32,
        ];
        self.c
            .dispatch(
                &self.k.im2col,
                &[src, dst],
                &push,
                [word(gx)?, word(groups.div_ceil(gx))?, 1],
            )
            .map_err(gpu_err)
    }

    fn attention(
        &mut self,
        q: &'a Buffer,
        k: &'a Buffer,
        v: &'a Buffer,
        n: usize,
        d: usize,
        out: &'a Buffer,
    ) -> io::Result<()> {
        let bytes = f32_bytes(n * d)?;
        // (`is_multiple_of` needs Rust 1.87.)
        #[allow(clippy::manual_is_multiple_of)]
        let whole_heads = d % HEAD == 0;
        if [q, k, v, out].iter().any(|b| b.size() < bytes) || !whole_heads || n == 0 {
            return Err(unsupported("attention sizes"));
        }
        let push = [word(n)?, word(n)?, word(d)?];
        let groups = [word(n.div_ceil(64))?, word(d / HEAD)?, 1];
        self.c
            .dispatch(&self.k.attention, &[q, k, v, out], &push, groups)
            .map_err(gpu_err)
    }
}

impl Encoder {
    pub(crate) fn new(gpu: Gpu) -> io::Result<Encoder> {
        let kernels = Kernels::new(&gpu).map_err(gpu_err)?;
        Ok(Encoder {
            gpu,
            kernels,
            weights: None,
            acts: None,
        })
    }

    pub(crate) fn device_name(&self) -> &str {
        self.gpu.name()
    }

    fn vector(&self, t: &Tensor, len: usize) -> io::Result<Vector> {
        let v = t.as_f32().ok_or_else(|| unsupported("vector not f32"))?;
        if v.len() != len {
            return Err(unsupported("vector size"));
        }
        Ok(Vector {
            buf: self.gpu.buffer_with(v).map_err(gpu_err)?,
            len,
        })
    }

    /// A weight matrix of `rows` rows of `cols`, as stored in the model.
    fn mat(&self, t: &Tensor, rows: usize, cols: usize) -> io::Result<Mat> {
        if t.rows != rows || t.cols != cols {
            return Err(unsupported("matrix shape"));
        }
        let (buf, shader) = match t.packed(0, t.rows) {
            Packed::F32(v) => (self.gpu.buffer_with(v), Shader::MatmulF32),
            Packed::F16(v) => (self.gpu.buffer_with(v), Shader::MatmulF16),
            Packed::Quant(q, v) => (
                self.gpu.buffer_with(v),
                match q {
                    QType::Q8_0 => Shader::MatmulQ8_0,
                    QType::Q5_0 => Shader::MatmulQ5_0,
                    QType::Q5_1 => Shader::MatmulQ5_1,
                    QType::Q4_0 => Shader::MatmulQ4_0,
                    QType::Q4_1 => Shader::MatmulQ4_1,
                },
            ),
        };
        Ok(Mat {
            buf: buf.map_err(gpu_err)?,
            shader,
            rows,
            cols,
        })
    }

    /// A convolution weight (out, cin, 3) as the matrix of `out` rows of
    /// `cin * 3` values.
    fn conv(&self, m: &Model, name: &str, out: usize, cin: usize) -> io::Result<(Mat, Vector)> {
        let w = m.get(&format!("{name}.weight"))?;
        let v = w
            .as_f32()
            .ok_or_else(|| unsupported("convolution not f32"))?;
        if Some(v.len()) != out.checked_mul(cin * 3) {
            return Err(unsupported("convolution shape"));
        }
        let mat = Mat {
            buf: self.gpu.buffer_with(v).map_err(gpu_err)?,
            shader: Shader::MatmulF32,
            rows: out,
            cols: cin * 3,
        };
        Ok((mat, self.vector(m.get(&format!("{name}.bias"))?, out)?))
    }

    fn upload(&self, m: &Model, id: Id) -> io::Result<Weights> {
        let p = m.params;
        let d = p.n_audio_state;
        let ln = |name: &str| -> io::Result<(Vector, Vector)> {
            Ok((
                self.vector(m.get(&format!("{name}.weight"))?, d)?,
                self.vector(m.get(&format!("{name}.bias"))?, d)?,
            ))
        };
        let mut layers = Vec::with_capacity(p.n_audio_layer);
        for l in 0..p.n_audio_layer {
            let t = |s: &str| m.get(&format!("encoder.blocks.{l}.{s}"));
            let hidden = t("mlp.0.weight")?.rows;
            let lin = |s: &str, rows, cols| -> io::Result<(Mat, Vector)> {
                Ok((
                    self.mat(t(&format!("{s}.weight"))?, rows, cols)?,
                    self.vector(t(&format!("{s}.bias"))?, rows)?,
                ))
            };
            layers.push(Layer {
                attn_ln: ln(&format!("encoder.blocks.{l}.attn_ln"))?,
                q: lin("attn.query", d, d)?,
                k: self.mat(t("attn.key.weight")?, d, d)?,
                v: lin("attn.value", d, d)?,
                out: lin("attn.out", d, d)?,
                mlp_ln: ln(&format!("encoder.blocks.{l}.mlp_ln"))?,
                mlp0: lin("mlp.0", hidden, d)?,
                mlp2: lin("mlp.2", d, hidden)?,
            });
        }
        let pos = m.get("encoder.positional_embedding")?;
        if pos.cols != d || pos.as_f32().is_none() {
            return Err(unsupported("position embedding"));
        }
        Ok(Weights {
            id,
            conv1: self.conv(m, "encoder.conv1", d, p.n_mels)?,
            conv2: self.conv(m, "encoder.conv2", d, d)?,
            pos: self.mat(pos, pos.rows, d)?,
            layers,
            ln_post: ln("encoder.ln_post")?,
        })
    }

    /// Activation buffers for `frames` mel frames and `n` audio rows.
    fn acts(&self, p: &Params, frames: usize, n: usize, hidden: usize) -> io::Result<Acts> {
        let d = p.n_audio_state;
        let scratch = [frames * p.n_mels * 3, n * d * 3, n * hidden]
            .into_iter()
            .max()
            .unwrap_or(0);
        let buf = |count: usize| -> io::Result<Buffer> {
            self.gpu.buffer(f32_bytes(count)?).map_err(gpu_err)
        };
        Ok(Acts {
            shape: (frames, n, hidden),
            mel: buf(p.n_mels * frames)?,
            scratch: buf(scratch)?,
            conv1: buf(frames * d)?,
            x: buf(n * d)?,
            h: buf(n * d)?,
            q: buf(n * d)?,
            k: buf(n * d)?,
            v: buf(n * d)?,
            a: buf(n * d)?,
        })
    }

    /// Encodes a mel window like `net::encode`.
    pub(crate) fn encode(&mut self, m: &Model, mel: &[f32]) -> io::Result<Vec<f32>> {
        let p = m.params;
        let d = p.n_audio_state;
        if d != p.n_audio_head * HEAD {
            return Err(unsupported("attention heads"));
        }
        let frames = mel.len() / p.n_mels;
        if frames == 0 {
            return Err(unsupported("empty window"));
        }
        // Rows after the convolutions (stride 1, then 2), as on the CPU.
        let n = ((frames - 1) / 2 + 1).min(p.n_audio_ctx);
        let id = identify(m)?;
        if !self.weights.as_ref().is_some_and(|w| w.id == id) {
            // The old weights go first, so both never take GPU memory.
            self.weights = None;
            self.weights = Some(self.upload(m, id)?);
        }
        let w = self.weights.as_ref().expect("weights just uploaded");
        if w.pos.rows < n {
            return Err(unsupported("window longer than the audio context"));
        }
        let hidden = w.layers.first().map_or(0, |l| l.mlp0.0.rows);
        if self.acts.as_ref().map(|a| a.shape) != Some((frames, n, hidden)) {
            self.acts = None;
            self.acts = Some(self.acts(&p, frames, n, hidden)?);
        }
        let a = self.acts.as_ref().expect("buffers just made");
        self.gpu
            .write(&a.mel, 0, &mel[..frames * p.n_mels])
            .map_err(gpu_err)?;
        let mut c = Cmds {
            c: Commands::new(),
            k: &self.kernels,
        };
        // Convolutions with GELU; the position embedding is copied into x
        // first and the second convolution added to it.
        c.im2col(&a.mel, frames, p.n_mels, 1, true, frames, &a.scratch)?;
        c.matmul(
            &a.scratch,
            frames,
            &w.conv1.0,
            Some(&w.conv1.1),
            &a.conv1,
            GELU,
        )?;
        c.im2col(&a.conv1, frames, d, 2, false, n, &a.scratch)?;
        c.c.copy(&w.pos.buf, 0, &a.x, 0, f32_bytes(n * d)?)
            .map_err(gpu_err)?;
        c.matmul(
            &a.scratch,
            n,
            &w.conv2.0,
            Some(&w.conv2.1),
            &a.x,
            GELU | ADD,
        )?;
        for l in &w.layers {
            c.layer_norm(&a.x, n, d, &l.attn_ln, &a.h)?;
            c.matmul(&a.h, n, &l.q.0, Some(&l.q.1), &a.q, 0)?;
            c.matmul(&a.h, n, &l.k, None, &a.k, 0)?;
            c.matmul(&a.h, n, &l.v.0, Some(&l.v.1), &a.v, 0)?;
            c.attention(&a.q, &a.k, &a.v, n, d, &a.a)?;
            c.matmul(&a.a, n, &l.out.0, Some(&l.out.1), &a.x, ADD)?;
            c.layer_norm(&a.x, n, d, &l.mlp_ln, &a.h)?;
            c.matmul(&a.h, n, &l.mlp0.0, Some(&l.mlp0.1), &a.scratch, GELU)?;
            c.matmul(&a.scratch, n, &l.mlp2.0, Some(&l.mlp2.1), &a.x, ADD)?;
        }
        c.layer_norm(&a.x, n, d, &w.ln_post, &a.h)?;
        self.gpu.run(&c.c).map_err(gpu_err)?;
        let mut out = vec![0f32; n * d];
        self.gpu.read(&a.h, 0, &mut out).map_err(gpu_err)?;
        Ok(out)
    }
}

/// Whether the GPU encoder is wanted: 0 not decided yet (the
/// environment decides on first use), 1 on, 2 off.
static WANTED: AtomicU8 = AtomicU8::new(0);

enum Slot {
    Untried,
    /// Ready, and whether its first window was checked against the CPU.
    Ready(Box<Encoder>, bool),
    Failed,
}

/// The process's GPU encoder (one GPU, one model's weights at a time).
static SLOT: Mutex<Slot> = Mutex::new(Slot::Untried);

fn wanted() -> bool {
    match WANTED.load(Ordering::Relaxed) {
        0 => {
            let off = std::env::var("BL_ASR_GPU")
                .is_ok_and(|v| matches!(v.trim(), "0" | "off" | "false" | "no"));
            let _ = WANTED.compare_exchange(
                0,
                if off { 2 } else { 1 },
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            WANTED.load(Ordering::Relaxed) == 1
        }
        v => v == 1,
    }
}

/// Turns the GPU encoder on or off; off frees its GPU memory, and on
/// after off tries the GPU again (also after an error).
pub(crate) fn set_wanted(on: bool) {
    WANTED.store(if on { 1 } else { 2 }, Ordering::Relaxed);
    if !on {
        if let Ok(mut slot) = SLOT.lock() {
            *slot = Slot::Untried;
        }
    }
}

/// The name of the GPU in use, once one has encoded.
pub(crate) fn device() -> Option<String> {
    match &*SLOT.lock().ok()? {
        Slot::Ready(e, true) => Some(e.device_name().to_string()),
        _ => None,
    }
}

/// The audio features of a mel window computed on the GPU, or `None` to
/// compute them on the CPU.
pub(crate) fn encode(m: &Model, mel: &[f32]) -> Option<Vec<f32>> {
    if !wanted() {
        return None;
    }
    let mut slot = SLOT.lock().ok()?;
    if matches!(*slot, Slot::Untried) {
        *slot = match Gpu::new().map(Encoder::new) {
            Some(Ok(e)) => Slot::Ready(Box::new(e), false),
            _ => Slot::Failed,
        };
    }
    let Slot::Ready(enc, checked) = &mut *slot else {
        return None;
    };
    let t0 = std::time::Instant::now();
    match enc.encode(m, mel) {
        Ok(x) if *checked => Some(x),
        // The first window also on the CPU: the GPU is kept only when it
        // gives the same result (a driver could compute wrongly) and is
        // faster (a weak GPU or a software driver is not).
        Ok(x) => {
            let gpu_time = t0.elapsed();
            let t1 = std::time::Instant::now();
            let cpu = crate::net::encode_cpu(m, mel).ok()?;
            let cpu_time = t1.elapsed();
            if same(&x, &cpu) && gpu_time < cpu_time {
                *checked = true;
                Some(cpu)
            } else {
                *slot = Slot::Failed;
                Some(cpu)
            }
        }
        // A model this encoder does not handle: the CPU does it, the GPU
        // stays ready for others.
        Err(e) if e.kind() == io::ErrorKind::Unsupported => None,
        Err(_) => {
            *slot = Slot::Failed;
            None
        }
    }
}

/// Whether two encoder outputs agree (relative to their largest value).
fn same(a: &[f32], b: &[f32]) -> bool {
    let scale = b.iter().fold(0f32, |m, v| m.max(v.abs())).max(1e-6);
    a.len() == b.len() && a.iter().zip(b).all(|(x, y)| (x - y).abs() <= 1e-2 * scale)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Store, QK};
    use std::collections::HashMap;

    /// Small deterministic generator (xorshift).
    struct Rng(u64);

    impl Rng {
        fn next(&mut self) -> u64 {
            self.0 ^= self.0 << 13;
            self.0 ^= self.0 >> 7;
            self.0 ^= self.0 << 17;
            self.0
        }

        /// Uniform in [-1, 1).
        fn f32(&mut self) -> f32 {
            (self.next() >> 40) as f32 / (1u64 << 23) as f32 - 1.0
        }

        fn vec(&mut self, n: usize, scale: f32) -> Vec<f32> {
            (0..n).map(|_| self.f32() * scale).collect()
        }
    }

    /// Half float of a value in the normal range (truncated).
    fn to_f16(v: f32) -> u16 {
        let b = v.to_bits();
        let sign = ((b >> 16) & 0x8000) as u16;
        if v.abs() < 6.2e-5 {
            return sign;
        }
        let exp = ((b >> 23) & 0xff) as i32 - 127 + 15;
        sign | ((exp as u16) << 10) | (((b >> 13) & 0x3ff) as u16)
    }

    /// Random weights of `rows` x `cols` in format `f` (0: f32, 1: f16,
    /// then the quantised ones), values about ±`scale`.
    fn matrix(rng: &mut Rng, cols: usize, rows: usize, f: usize, scale: f32) -> Tensor {
        let quant = [
            QType::Q8_0,
            QType::Q5_0,
            QType::Q5_1,
            QType::Q4_0,
            QType::Q4_1,
        ];
        let store = match f {
            0 => Store::F32(rng.vec(cols * rows, scale)),
            1 => Store::F16(
                (0..cols * rows)
                    .map(|_| to_f16(rng.f32() * scale))
                    .collect(),
            ),
            _ => {
                let t = quant[f - 2];
                let bb = t.block_bytes();
                let mut raw: Vec<u8> = (0..cols * rows / QK * bb)
                    .map(|_| rng.next() as u8)
                    .collect();
                // Steps so that the values reach about ±scale.
                let levels = if t == QType::Q8_0 { 128.0 } else { 8.0 };
                for blk in raw.chunks_exact_mut(bb) {
                    let d = to_f16(scale / levels * (0.5 + 0.5 * rng.f32().abs()));
                    blk[..2].copy_from_slice(&d.to_le_bytes());
                    if matches!(t, QType::Q4_1 | QType::Q5_1) {
                        let m = to_f16(-scale * (0.5 + 0.5 * rng.f32().abs()));
                        blk[2..4].copy_from_slice(&m.to_le_bytes());
                    }
                }
                Store::Quant(t, raw)
            }
        };
        Tensor::new(cols, rows, store).expect("tensor size")
    }

    /// A random encoder of `layers` layers whose matrices go through all
    /// formats.
    fn model(rng: &mut Rng, d: usize, layers: usize, ctx: usize) -> Model {
        let n_mels = 80;
        let params = Params {
            n_vocab: 100,
            n_audio_ctx: ctx,
            n_audio_state: d,
            n_audio_head: d / HEAD,
            n_audio_layer: layers,
            n_text_ctx: 10,
            n_text_state: d,
            n_text_head: d / HEAD,
            n_text_layer: 1,
            n_mels,
        };
        let mut t = HashMap::new();
        let mut vector = |rng: &mut Rng, name: String, len: usize, base: f32| {
            let v = rng.vec(len, 0.1).into_iter().map(|x| x + base).collect();
            t.insert(name, Tensor::from_f32(len, 1, v));
        };
        let mut mats = Vec::new();
        for (name, cin) in [("conv1", n_mels), ("conv2", d)] {
            let w = rng.vec(d * cin * 3, 1.0 / (cin as f32 * 3.0).sqrt());
            mats.push((
                format!("encoder.{name}.weight"),
                Tensor::from_f32(3, d * cin, w),
            ));
            vector(rng, format!("encoder.{name}.bias"), d, 0.0);
        }
        let pos = rng.vec(ctx * d, 0.5);
        mats.push((
            "encoder.positional_embedding".into(),
            Tensor::from_f32(d, ctx, pos),
        ));
        vector(rng, "encoder.ln_post.weight".into(), d, 1.0);
        vector(rng, "encoder.ln_post.bias".into(), d, 0.0);
        let mut format = 0;
        for l in 0..layers {
            let p = format!("encoder.blocks.{l}");
            for ln in ["attn_ln", "mlp_ln"] {
                vector(rng, format!("{p}.{ln}.weight"), d, 1.0);
                vector(rng, format!("{p}.{ln}.bias"), d, 0.0);
            }
            for (name, rows, cols, bias) in [
                ("attn.query", d, d, true),
                ("attn.key", d, d, false),
                ("attn.value", d, d, true),
                ("attn.out", d, d, true),
                ("mlp.0", 4 * d, d, true),
                ("mlp.2", d, 4 * d, true),
            ] {
                let scale = 1.5 / (cols as f32).sqrt();
                mats.push((
                    format!("{p}.{name}.weight"),
                    matrix(rng, cols, rows, format % 7, scale),
                ));
                format += 1;
                if bias {
                    vector(rng, format!("{p}.{name}.bias"), rows, 0.0);
                }
            }
        }
        t.extend(mats);
        Model {
            params,
            mel_filters: Vec::new(),
            mel_bins: 0,
            vocab: Vec::new(),
            tensors: t,
        }
    }

    /// The largest difference relative to the largest value of `want`
    /// (infinite if a value is not finite), and the relative difference
    /// of the whole (Euclidean norms).
    fn rel_error(got: &[f32], want: &[f32]) -> (f32, f32) {
        assert_eq!(got.len(), want.len());
        if !got.iter().chain(want).all(|v| v.is_finite()) {
            return (f32::INFINITY, f32::INFINITY);
        }
        let norm =
            |v: &mut dyn Iterator<Item = f32>| v.map(|x| x as f64 * x as f64).sum::<f64>().sqrt();
        let diff = || got.iter().zip(want).map(|(a, b)| a - b);
        let max_diff = diff().fold(0f32, |m, v| m.max(v.abs()));
        let max = want.iter().fold(0f32, |m, v| m.max(v.abs()));
        let l2 = norm(&mut diff()) / norm(&mut want.iter().copied());
        (max_diff / max, l2 as f32)
    }

    /// A GPU (software renderers too) for the tests, if there is one.
    fn encoder() -> Option<Encoder> {
        match Gpu::open(true).map_err(gpu_err).and_then(Encoder::new) {
            Ok(e) => Some(e),
            Err(e) => {
                eprintln!("no Vulkan device, skipped: {e}");
                None
            }
        }
    }

    #[test]
    fn gpu_matches_cpu() {
        let Some(mut enc) = encoder() else { return };
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15);
        // Two layers: all seven weight formats; sizes that are not
        // multiples of the kernels' tiles (151 frames give 76 rows).
        let m = model(&mut rng, 128, 2, 100);
        for frames in [151, 200] {
            let mel = rng.vec(80 * frames, 1.0);
            let cpu = crate::net::encode_cpu(&m, &mel).unwrap();
            let gpu = enc.encode(&m, &mel).unwrap();
            let (err, l2) = rel_error(&gpu, &cpu);
            eprintln!(
                "{frames} frames: difference at most {err:e} of the largest value, {l2:e} in all"
            );
            assert!(err < 1e-3, "{err}");
        }
        // Another model (same shapes, other values) replaces the weights.
        let m2 = model(&mut rng, 128, 2, 100);
        let mel = rng.vec(80 * 200, 1.0);
        let cpu = crate::net::encode_cpu(&m2, &mel).unwrap();
        let gpu = enc.encode(&m2, &mel).unwrap();
        assert!(rel_error(&gpu, &cpu).0 < 1e-3);
        // Heads not of 64 values are left to the CPU.
        let mut m3 = model(&mut rng, 128, 1, 100);
        m3.params.n_audio_head = 4;
        let err = enc.encode(&m3, &mel).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::Unsupported);
    }

    /// GPU against CPU on a real model: `BL_ASR_TEST_MODEL=path cargo
    /// test --release -p bl-asr --features gpu -- --ignored --nocapture
    /// real_model` (with `BL_ASR_TEST_WAV`, 16-bit 16 kHz mono, for
    /// speech instead of noise).
    #[test]
    #[ignore]
    fn real_model() {
        let Some(mut enc) = encoder() else { return };
        let path = std::env::var("BL_ASR_TEST_MODEL").expect("BL_ASR_TEST_MODEL");
        let m = Model::load(&mut io::BufReader::new(std::fs::File::open(path).unwrap())).unwrap();
        let samples: Vec<f32> = match std::env::var("BL_ASR_TEST_WAV") {
            Ok(p) => std::fs::read(p).unwrap()[44..]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect(),
            Err(_) => Rng(5).vec(16_000 * 30, 0.1),
        };
        let mel =
            crate::mel::log_mel(&samples, &m.mel_filters, m.params.n_mels, m.mel_bins).window(0);
        let t = std::time::Instant::now();
        let cpu = crate::net::encode_cpu(&m, &mel).unwrap();
        let cpu_time = t.elapsed();
        let t = std::time::Instant::now();
        let first = enc.encode(&m, &mel).unwrap();
        let first_time = t.elapsed();
        let t = std::time::Instant::now();
        let gpu = enc.encode(&m, &mel).unwrap();
        let gpu_time = t.elapsed();
        assert_eq!(first, gpu);
        let (err, l2) = rel_error(&gpu, &cpu);
        eprintln!(
            "{}: CPU {cpu_time:.2?}, GPU {gpu_time:.2?} (first, with upload, {first_time:.2?}); \
             difference at most {err:e} of the largest value, {l2:e} in all",
            enc.device_name()
        );
        assert!(err < 1e-3, "{err}");
    }
}
