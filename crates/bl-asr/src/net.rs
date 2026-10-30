//! The Whisper transformer: audio encoder (two convolutions, attention
//! blocks) and text decoder (self attention with a cache, cross attention
//! to the audio, MLP), following the published architecture.

use crate::model::{Model, Tensor};
use crate::ops::{axpy, dot, gelu, layer_norm, linear, softmax, threads};
use std::io;

/// Query rows handled together, so each key and value row is read once
/// for all of them.
const ROW_BLOCK: usize = 8;

/// Attention of `q` (`nq` rows) to `k`/`v` (`nk` rows), `heads` heads over
/// `dim` columns; with `causal`, row i sees keys up to `offset + i`. The
/// work is split by head and block of query rows; small jobs (a decoder
/// step) stay on this thread.
#[allow(clippy::too_many_arguments)]
fn attention(
    q: &[f32],
    k: &[f32],
    v: &[f32],
    nq: usize,
    nk: usize,
    dim: usize,
    heads: usize,
    causal: Option<usize>,
) -> Vec<f32> {
    let hd = dim / heads;
    let scale = 1.0 / (hd as f32).sqrt();
    let tasks: Vec<(usize, usize)> = (0..heads)
        .flat_map(|h| (0..nq).step_by(ROW_BLOCK).map(move |r| (h, r)))
        .collect();
    // Many query rows (the encoder): keys and values per head transposed
    // (head dimension x keys), so scores and outputs become long vector
    // operations over all keys.
    let transposed = (nq >= 4 * ROW_BLOCK && causal.is_none()).then(|| {
        let mut kt = vec![0f32; dim * nk];
        let mut vt = vec![0f32; dim * nk];
        for j in 0..nk {
            for c in 0..dim {
                kt[c * nk + j] = k[j * dim + c];
                vt[c * nk + j] = v[j * dim + c];
            }
        }
        (kt, vt)
    });
    let run = |chunk: &[(usize, usize)]| -> Vec<(usize, usize, Vec<f32>)> {
        let mut res = Vec::with_capacity(chunk.len());
        let mut scores = vec![0f32; ROW_BLOCK * nk];
        for &(h, r0) in chunk {
            let rows = ROW_BLOCK.min(nq - r0);
            let limit = |i: usize| causal.map_or(nk, |off| (off + i + 1).min(nk));
            let mut out = vec![0f32; rows * hd];
            if let Some((kt, vt)) = &transposed {
                for r in 0..rows {
                    let i = r0 + r;
                    let qi = &q[i * dim + h * hd..i * dim + (h + 1) * hd];
                    let sc = &mut scores[r * nk..(r + 1) * nk];
                    sc.fill(0.0);
                    for (d, &qd) in qi.iter().enumerate() {
                        let c = h * hd + d;
                        axpy(qd * scale, &kt[c * nk..(c + 1) * nk], sc);
                    }
                    softmax(sc);
                    for d in 0..hd {
                        let c = h * hd + d;
                        out[r * hd + d] = dot(sc, &vt[c * nk..(c + 1) * nk]);
                    }
                }
                res.push((h, r0, out));
                continue;
            }
            for r in 0..rows {
                let i = r0 + r;
                let qi = &q[i * dim + h * hd..i * dim + (h + 1) * hd];
                let sc = &mut scores[r * nk..r * nk + limit(i)];
                for (j, s) in sc.iter_mut().enumerate() {
                    *s = dot(qi, &k[j * dim + h * hd..j * dim + (h + 1) * hd]) * scale;
                }
                softmax(sc);
            }
            // Each value row once for all rows of the block.
            let longest = (0..rows).map(|r| limit(r0 + r)).max().unwrap_or(0);
            for j in 0..longest {
                let vj = &v[j * dim + h * hd..j * dim + (h + 1) * hd];
                for r in 0..rows {
                    if j < limit(r0 + r) {
                        axpy(scores[r * nk + j], vj, &mut out[r * hd..(r + 1) * hd]);
                    }
                }
            }
            res.push((h, r0, out));
        }
        res
    };
    let work = nq * nk * dim;
    let threads = if work < 1 << 20 { 1 } else { threads().min(tasks.len()) };
    let parts: Vec<Vec<(usize, usize, Vec<f32>)>> = if threads <= 1 {
        vec![run(&tasks)]
    } else {
        let per = tasks.len().div_ceil(threads).max(1);
        std::thread::scope(|s| {
            tasks
                .chunks(per)
                .map(|chunk| s.spawn(move || run(chunk)))
                .collect::<Vec<_>>()
                .into_iter()
                .map(|h| h.join().expect("attention thread"))
                .collect()
        })
    };
    let mut out = vec![0f32; nq * dim];
    for (h, r0, o) in parts.into_iter().flatten() {
        for (r, row) in o.chunks_exact(hd).enumerate() {
            let i = r0 + r;
            out[i * dim + h * hd..i * dim + (h + 1) * hd].copy_from_slice(row);
        }
    }
    out
}

struct Block<'a> {
    attn_ln: (&'a Tensor, &'a Tensor),
    q: (&'a Tensor, &'a Tensor),
    k: &'a Tensor,
    v: (&'a Tensor, &'a Tensor),
    out: (&'a Tensor, &'a Tensor),
    cross: Option<CrossAttn<'a>>,
    mlp_ln: (&'a Tensor, &'a Tensor),
    mlp0: (&'a Tensor, &'a Tensor),
    mlp2: (&'a Tensor, &'a Tensor),
}

struct CrossAttn<'a> {
    ln: (&'a Tensor, &'a Tensor),
    q: (&'a Tensor, &'a Tensor),
    k: &'a Tensor,
    v: (&'a Tensor, &'a Tensor),
    out: (&'a Tensor, &'a Tensor),
}

fn block<'a>(m: &'a Model, p: &str, cross: bool) -> io::Result<Block<'a>> {
    let t = |s: &str| m.get(&format!("{p}.{s}"));
    Ok(Block {
        attn_ln: (t("attn_ln.weight")?, t("attn_ln.bias")?),
        q: (t("attn.query.weight")?, t("attn.query.bias")?),
        k: t("attn.key.weight")?,
        v: (t("attn.value.weight")?, t("attn.value.bias")?),
        out: (t("attn.out.weight")?, t("attn.out.bias")?),
        cross: if cross {
            Some(CrossAttn {
                ln: (t("cross_attn_ln.weight")?, t("cross_attn_ln.bias")?),
                q: (t("cross_attn.query.weight")?, t("cross_attn.query.bias")?),
                k: t("cross_attn.key.weight")?,
                v: (t("cross_attn.value.weight")?, t("cross_attn.value.bias")?),
                out: (t("cross_attn.out.weight")?, t("cross_attn.out.bias")?),
            })
        } else {
            None
        },
        mlp_ln: (t("mlp_ln.weight")?, t("mlp_ln.bias")?),
        mlp0: (t("mlp.0.weight")?, t("mlp.0.bias")?),
        mlp2: (t("mlp.2.weight")?, t("mlp.2.bias")?),
    })
}

fn mlp(x: &mut [f32], n: usize, dim: usize, b: &Block) {
    let mut h = x.to_vec();
    layer_norm(&mut h, dim, b.mlp_ln.0, b.mlp_ln.1);
    let mut h = linear(&h, n, b.mlp0.0, Some(b.mlp0.1));
    gelu(&mut h);
    let h = linear(&h, n, b.mlp2.0, Some(b.mlp2.1));
    for (a, b) in x.iter_mut().zip(&h) {
        *a += b;
    }
}

/// 1-D convolution, kernel 3, padding 1: `x` is time-major (`t` rows of
/// `cin`); the weight is (out, cin, 3).
fn conv3(
    x: &[f32],
    t: usize,
    cin: usize,
    w: &Tensor,
    b: &Tensor,
    stride: usize,
) -> (Vec<f32>, usize) {
    let tout = (t + 2 - 3) / stride + 1;
    let mut patches = vec![0f32; tout * cin * 3];
    for o in 0..tout {
        for kk in 0..3 {
            let src = (o * stride + kk) as isize - 1;
            if src < 0 || src as usize >= t {
                continue;
            }
            let row = &x[src as usize * cin..(src as usize + 1) * cin];
            for c in 0..cin {
                patches[o * cin * 3 + c * 3 + kk] = row[c];
            }
        }
    }
    let wt = Tensor::from_f32(cin * 3, w.values().len() / (cin * 3), w.values().to_vec());
    let bias = Tensor::from_f32(b.values().len(), 1, b.values().to_vec());
    (linear(&patches, tout, &wt, Some(&bias)), tout)
}

/// Encodes a 30-second mel window (`n_mels` x 3000, row major): the
/// audio features (`n_audio_ctx` rows of `n_audio_state`).
pub fn encode(m: &Model, mel: &[f32]) -> io::Result<Vec<f32>> {
    let p = m.params;
    let frames = mel.len() / p.n_mels;
    // Time-major input.
    let mut x = vec![0f32; frames * p.n_mels];
    for c in 0..p.n_mels {
        for t in 0..frames {
            x[t * p.n_mels + c] = mel[c * frames + t];
        }
    }
    let (mut x, t1) = conv3(
        &x,
        frames,
        p.n_mels,
        m.get("encoder.conv1.weight")?,
        m.get("encoder.conv1.bias")?,
        1,
    );
    gelu(&mut x);
    let (mut x, t2) = conv3(
        &x,
        t1,
        p.n_audio_state,
        m.get("encoder.conv2.weight")?,
        m.get("encoder.conv2.bias")?,
        2,
    );
    gelu(&mut x);
    let n = t2.min(p.n_audio_ctx);
    x.truncate(n * p.n_audio_state);
    let pos = m.get("encoder.positional_embedding")?;
    for (i, row) in x.chunks_mut(p.n_audio_state).enumerate() {
        for (a, b) in row.iter_mut().zip(pos.row(i).iter()) {
            *a += b;
        }
    }
    let d = p.n_audio_state;
    for l in 0..p.n_audio_layer {
        let b = block(m, &format!("encoder.blocks.{l}"), false)?;
        let mut h = x.clone();
        layer_norm(&mut h, d, b.attn_ln.0, b.attn_ln.1);
        let q = linear(&h, n, b.q.0, Some(b.q.1));
        let k = linear(&h, n, b.k, None);
        let v = linear(&h, n, b.v.0, Some(b.v.1));
        let a = attention(&q, &k, &v, n, n, d, p.n_audio_head, None);
        let o = linear(&a, n, b.out.0, Some(b.out.1));
        for (xa, ob) in x.iter_mut().zip(&o) {
            *xa += ob;
        }
        mlp(&mut x, n, d, &b);
    }
    layer_norm(
        &mut x,
        d,
        m.get("encoder.ln_post.weight")?,
        m.get("encoder.ln_post.bias")?,
    );
    Ok(x)
}

/// Decoder state for one 30-second window: cross-attention keys/values and
/// the self-attention cache.
pub struct Decoder<'a> {
    m: &'a Model,
    blocks: Vec<Block<'a>>,
    cross_kv: Vec<(Vec<f32>, Vec<f32>)>,
    n_audio: usize,
    cache: Vec<(Vec<f32>, Vec<f32>)>,
    len: usize,
}

impl<'a> Decoder<'a> {
    pub fn new(m: &'a Model, audio: &[f32]) -> io::Result<Decoder<'a>> {
        let p = m.params;
        let n_audio = audio.len() / p.n_audio_state;
        let mut blocks = Vec::new();
        let mut cross_kv = Vec::new();
        for l in 0..p.n_text_layer {
            let b = block(m, &format!("decoder.blocks.{l}"), true)?;
            let c = b.cross.as_ref().expect("decoder block");
            cross_kv.push((
                linear(audio, n_audio, c.k, None),
                linear(audio, n_audio, c.v.0, Some(c.v.1)),
            ));
            blocks.push(b);
        }
        Ok(Decoder {
            m,
            cache: vec![(Vec::new(), Vec::new()); blocks.len()],
            blocks,
            cross_kv,
            n_audio,
            len: 0,
        })
    }

    /// Forgets the text so far (same audio).
    pub fn reset(&mut self) {
        self.len = 0;
        for c in &mut self.cache {
            c.0.clear();
            c.1.clear();
        }
    }

    /// Feeds `tokens`; returns the logits after the last one.
    pub fn step(&mut self, tokens: &[u32]) -> io::Result<Vec<f32>> {
        self.step_with(tokens, &[]).map(|(logits, _)| logits)
    }

    /// Heads whose attention to the audio follows the words being said
    /// (Whisper's "alignment heads" lie in the upper decoder layers): the
    /// heads of the upper half of the layers, at most 32, from the top.
    pub fn alignment_heads(&self) -> Vec<(usize, usize)> {
        let p = self.m.params;
        let mut heads = Vec::new();
        for l in (p.n_text_layer / 2..p.n_text_layer).rev() {
            if heads.len() + p.n_text_head > 32 && !heads.is_empty() {
                break;
            }
            heads.extend((0..p.n_text_head).map(|h| (l, h)));
        }
        heads
    }

    /// Like [`step`](Self::step), also returning the attention to the
    /// audio of the given (layer, head) pairs: for each, `tokens.len()`
    /// rows of `n_audio` weights (each row sums to 1).
    pub fn step_with(
        &mut self,
        tokens: &[u32],
        heads: &[(usize, usize)],
    ) -> io::Result<(Vec<f32>, Vec<Vec<f32>>)> {
        let p = self.m.params;
        let d = p.n_text_state;
        let n = tokens.len();
        if self.len + n > p.n_text_ctx {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "text too long"));
        }
        let emb = self.m.get("decoder.token_embedding.weight")?;
        let pos = self.m.get("decoder.positional_embedding")?;
        let mut x = vec![0f32; n * d];
        for (i, &t) in tokens.iter().enumerate() {
            let row = &mut x[i * d..(i + 1) * d];
            for ((a, e), q) in row
                .iter_mut()
                .zip(emb.row(t as usize).iter())
                .zip(pos.row(self.len + i).iter())
            {
                *a = e + q;
            }
        }
        let mut weights = vec![Vec::new(); heads.len()];
        for (l, b) in self.blocks.iter().enumerate() {
            // Self attention with the cache.
            let mut h = x.clone();
            layer_norm(&mut h, d, b.attn_ln.0, b.attn_ln.1);
            let q = linear(&h, n, b.q.0, Some(b.q.1));
            let k = linear(&h, n, b.k, None);
            let v = linear(&h, n, b.v.0, Some(b.v.1));
            self.cache[l].0.extend_from_slice(&k);
            self.cache[l].1.extend_from_slice(&v);
            let total = self.len + n;
            let a = attention(
                &q,
                &self.cache[l].0,
                &self.cache[l].1,
                n,
                total,
                d,
                p.n_text_head,
                Some(self.len),
            );
            let o = linear(&a, n, b.out.0, Some(b.out.1));
            for (xa, ob) in x.iter_mut().zip(&o) {
                *xa += ob;
            }
            // Cross attention to the audio.
            let c = b.cross.as_ref().expect("decoder block");
            let mut h = x.clone();
            layer_norm(&mut h, d, c.ln.0, c.ln.1);
            let q = linear(&h, n, c.q.0, Some(c.q.1));
            let (ck, cv) = &self.cross_kv[l];
            for (slot, &(hl, head)) in heads.iter().enumerate() {
                if hl == l {
                    weights[slot] = cross_weights(&q, ck, n, self.n_audio, d, p.n_text_head, head);
                }
            }
            let a = attention(&q, ck, cv, n, self.n_audio, d, p.n_text_head, None);
            let o = linear(&a, n, c.out.0, Some(c.out.1));
            for (xa, ob) in x.iter_mut().zip(&o) {
                *xa += ob;
            }
            mlp(&mut x, n, d, b);
        }
        self.len += n;
        let last = &mut x[(n - 1) * d..n * d];
        layer_norm(
            last,
            d,
            self.m.get("decoder.ln.weight")?,
            self.m.get("decoder.ln.bias")?,
        );
        let last = last.to_vec();
        Ok((linear(&last, 1, emb, None), weights))
    }
}

/// The attention weights of one head of `q` (`nq` rows) to `k` (`nk`
/// rows): `nq` rows of `nk` weights.
fn cross_weights(
    q: &[f32],
    k: &[f32],
    nq: usize,
    nk: usize,
    dim: usize,
    heads: usize,
    head: usize,
) -> Vec<f32> {
    let hd = dim / heads;
    let scale = 1.0 / (hd as f32).sqrt();
    let mut out = vec![0f32; nq * nk];
    for (i, row) in out.chunks_exact_mut(nk).enumerate() {
        let qi = &q[i * dim + head * hd..i * dim + (head + 1) * hd];
        for (j, w) in row.iter_mut().enumerate() {
            *w = dot(qi, &k[j * dim + head * hd..j * dim + (head + 1) * hd]) * scale;
        }
        softmax(row);
    }
    out
}
