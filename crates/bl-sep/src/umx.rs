//! The Open-Unmix network (`openunmix.model.OpenUnmix`) for one source:
//! the magnitude spectrogram of a stereo mixture (bins up to `max_bin`)
//! is normalised, encoded per frame (linear, batch norm, tanh), passed
//! through a 3-layer bidirectional LSTM, decoded with a skip connection
//! (linear, batch norm, ReLU, linear, batch norm) and scaled into a mask
//! for all bins; the source's magnitude is the mask times the mixture's.

use bl_asr::model::Tensor;
use bl_asr::ops::linear;
use bl_mt::torch::{Checkpoint, Keep};
use std::collections::HashMap;
use std::path::Path;

/// Channels of the model.
pub const CHANNELS: usize = 2;
/// Bins of a frame (n_fft 4096).
pub const BINS: usize = 2049;
/// Bins the network looks at (up to 16 kHz).
pub const MAX_BIN: usize = 1487;
/// Width of the encoded frames.
pub const HIDDEN: usize = 512;
/// LSTM layers (each forward and backward with `HIDDEN / 2` units).
pub const LAYERS: usize = 3;
const UNITS: usize = HIDDEN / 2;
/// Epsilon of the batch norms.
const BN_EPS: f32 = 1e-5;
/// Frames whose LSTM input projections are computed together.
const LSTM_BLOCK: usize = 128;

/// Batch norm in evaluation mode as `x * scale + shift`.
struct Norm {
    scale: Vec<f32>,
    shift: Vec<f32>,
}

impl Norm {
    fn apply(&self, x: &mut [f32]) {
        for row in x.chunks_exact_mut(self.scale.len()) {
            for ((v, s), b) in row.iter_mut().zip(&self.scale).zip(&self.shift) {
                *v = *v * s + b;
            }
        }
    }
}

/// One direction of an LSTM layer (PyTorch's gate order i, f, g, o).
struct Direction {
    /// Input weights (4 units x input).
    w_ih: Tensor,
    /// Recurrent weights (4 units x units).
    w_hh: Tensor,
    /// Both biases added.
    bias: Tensor,
}

impl Direction {
    /// Runs over the `frames` rows of `x` (`HIDDEN` wide), forward or
    /// backward, writing the hidden states to `out` (`UNITS` per frame).
    fn run(&self, x: &[f32], frames: usize, backward: bool, out: &mut [f32]) {
        let mut h = vec![0f32; UNITS];
        let mut c = vec![0f32; UNITS];
        let blocks = frames.div_ceil(LSTM_BLOCK);
        for b in 0..blocks {
            let b = if backward { blocks - 1 - b } else { b };
            let (t0, t1) = (b * LSTM_BLOCK, ((b + 1) * LSTM_BLOCK).min(frames));
            let pre = linear(
                &x[t0 * HIDDEN..t1 * HIDDEN],
                t1 - t0,
                &self.w_ih,
                Some(&self.bias),
            );
            for i in 0..t1 - t0 {
                let i = if backward { t1 - t0 - 1 - i } else { i };
                let mut g = linear(&h, 1, &self.w_hh, None);
                for (g, p) in g.iter_mut().zip(&pre[i * 4 * UNITS..(i + 1) * 4 * UNITS]) {
                    *g += p;
                }
                let (gi, rest) = g.split_at(UNITS);
                let (gf, rest) = rest.split_at(UNITS);
                let (gg, go) = rest.split_at(UNITS);
                for u in 0..UNITS {
                    c[u] = sigmoid(gf[u]) * c[u] + sigmoid(gi[u]) * gg[u].tanh();
                    h[u] = sigmoid(go[u]) * c[u].tanh();
                }
                let t = t0 + i;
                out[t * UNITS..(t + 1) * UNITS].copy_from_slice(&h);
            }
        }
    }
}

/// `v`, or zero if it is subnormal.
fn flush(v: f32) -> f32 {
    if v.is_subnormal() {
        0.0
    } else {
        v
    }
}

fn sigmoid(x: f32) -> f32 {
    1.0 / (1.0 + (-x).exp())
}

/// The weights of the network.
pub struct Umx {
    /// Added to the input magnitudes (the negated mean), then multiplied
    /// by `input_scale` (the inverse deviation); `MAX_BIN` each.
    input_mean: Vec<f32>,
    input_scale: Vec<f32>,
    /// The mask is `x * output_scale + output_mean` (`BINS` each).
    output_scale: Vec<f32>,
    output_mean: Vec<f32>,
    fc1: Tensor,
    bn1: Norm,
    lstm: Vec<[Direction; 2]>,
    fc2: Tensor,
    bn2: Norm,
    fc3: Tensor,
    bn3: Norm,
}

impl Umx {
    /// Reads the weights from a PyTorch checkpoint of the state dict.
    pub fn load(path: impl AsRef<Path>) -> Result<Umx, String> {
        let ckpt = Checkpoint::open(path)?;
        let mut names = vec![
            "input_mean".to_string(),
            "input_scale".into(),
            "output_scale".into(),
            "output_mean".into(),
            "fc1.weight".into(),
            "fc2.weight".into(),
            "fc3.weight".into(),
        ];
        for bn in ["bn1", "bn2", "bn3"] {
            for p in ["weight", "bias", "running_mean", "running_var"] {
                names.push(format!("{bn}.{p}"));
            }
        }
        for l in 0..LAYERS {
            for dir in ["", "_reverse"] {
                for p in ["weight_ih", "weight_hh", "bias_ih", "bias_hh"] {
                    names.push(format!("lstm.{p}_l{l}{dir}"));
                }
            }
        }
        let wanted: Vec<(&str, Keep)> = names.iter().map(|n| (n.as_str(), Keep::F32)).collect();
        let mut t = ckpt.load(&wanted)?;
        Umx::from_tensors(&mut t)
    }

    fn from_tensors(t: &mut HashMap<String, Tensor>) -> Result<Umx, String> {
        let mut take = |name: &str, rows: usize, cols: usize| -> Result<Tensor, String> {
            let x = t
                .remove(name)
                .ok_or_else(|| format!("model: no tensor {name}"))?;
            if (x.rows, x.cols) != (rows, cols) {
                return Err(format!(
                    "model: {name} is {}x{}, not {rows}x{cols}",
                    x.rows, x.cols
                ));
            }
            // Subnormal values (many in the trained weights) make the
            // products many times slower on some CPUs; zero is as good.
            let values = x.values().iter().map(|&v| flush(v)).collect();
            Ok(Tensor::from_f32(cols, rows, values))
        };
        let mut vector = |name: &str, len: usize| -> Result<Vec<f32>, String> {
            Ok(take(name, 1, len)?.values().to_vec())
        };
        let input_mean = vector("input_mean", MAX_BIN)?;
        let input_scale = vector("input_scale", MAX_BIN)?;
        let output_scale = vector("output_scale", BINS)?;
        let output_mean = vector("output_mean", BINS)?;
        let mut norm = |name: &str, len: usize| -> Result<Norm, String> {
            let w = vector(&format!("{name}.weight"), len)?;
            let b = vector(&format!("{name}.bias"), len)?;
            let m = vector(&format!("{name}.running_mean"), len)?;
            let v = vector(&format!("{name}.running_var"), len)?;
            let scale: Vec<f32> = w
                .iter()
                .zip(&v)
                .map(|(w, v)| flush(w / (v + BN_EPS).sqrt()))
                .collect();
            let shift = b
                .iter()
                .zip(&m)
                .zip(&scale)
                .map(|((b, m), s)| flush(b - m * s))
                .collect();
            Ok(Norm { scale, shift })
        };
        let bn1 = norm("bn1", HIDDEN)?;
        let bn2 = norm("bn2", HIDDEN)?;
        let bn3 = norm("bn3", CHANNELS * BINS)?;
        let mut lstm = Vec::new();
        for l in 0..LAYERS {
            let mut dir = |suffix: &str| -> Result<Direction, String> {
                let w_ih = take(&format!("lstm.weight_ih_l{l}{suffix}"), 4 * UNITS, HIDDEN)?;
                let w_hh = take(&format!("lstm.weight_hh_l{l}{suffix}"), 4 * UNITS, UNITS)?;
                let b_ih = take(&format!("lstm.bias_ih_l{l}{suffix}"), 1, 4 * UNITS)?;
                let b_hh = take(&format!("lstm.bias_hh_l{l}{suffix}"), 1, 4 * UNITS)?;
                let bias = b_ih
                    .values()
                    .iter()
                    .zip(b_hh.values())
                    .map(|(a, b)| a + b)
                    .collect();
                Ok(Direction {
                    w_ih,
                    w_hh,
                    bias: Tensor::from_f32(4 * UNITS, 1, bias),
                })
            };
            lstm.push([dir("")?, dir("_reverse")?]);
        }
        Ok(Umx {
            input_mean,
            input_scale,
            output_scale,
            output_mean,
            fc1: take("fc1.weight", HIDDEN, CHANNELS * MAX_BIN)?,
            bn1,
            lstm,
            fc2: take("fc2.weight", HIDDEN, 2 * HIDDEN)?,
            bn2,
            fc3: take("fc3.weight", CHANNELS * BINS, HIDDEN)?,
            bn3,
        })
    }

    /// Encodes `frames` frames of mixture magnitudes (`CHANNELS * MAX_BIN`
    /// per frame, channel after channel): `HIDDEN` values per frame.
    pub fn encode(&self, mag: &[f32], frames: usize) -> Vec<f32> {
        let mut x = mag.to_vec();
        for row in x.chunks_exact_mut(MAX_BIN) {
            for ((v, m), s) in row.iter_mut().zip(&self.input_mean).zip(&self.input_scale) {
                *v = (*v + m) * s;
            }
        }
        let mut h = linear(&x, frames, &self.fc1, None);
        self.bn1.apply(&mut h);
        h.iter_mut().for_each(|v| *v = v.tanh());
        h
    }

    /// The LSTM over all encoded frames: `HIDDEN` values per frame (the
    /// forward then the backward states). `keep_going` is asked after
    /// each layer.
    pub fn lstm(
        &self,
        encoded: &[f32],
        frames: usize,
        keep_going: &mut dyn FnMut(usize) -> bool,
    ) -> Option<Vec<f32>> {
        let mut x = encoded.to_vec();
        for (l, [fwd, bwd]) in self.lstm.iter().enumerate() {
            let mut a = vec![0f32; frames * UNITS];
            let mut b = vec![0f32; frames * UNITS];
            std::thread::scope(|s| {
                let x = &x;
                let back = s.spawn(|| bwd.run(x, frames, true, &mut b));
                fwd.run(x, frames, false, &mut a);
                back.join().expect("LSTM thread");
            });
            for (row, (a, b)) in x
                .chunks_exact_mut(HIDDEN)
                .zip(a.chunks_exact(UNITS).zip(b.chunks_exact(UNITS)))
            {
                row[..UNITS].copy_from_slice(a);
                row[UNITS..].copy_from_slice(b);
            }
            if !keep_going(l) {
                return None;
            }
        }
        Some(x)
    }

    /// The masks of `frames` frames (`CHANNELS * BINS` per frame) from the
    /// encoded frames and the LSTM's output (`HIDDEN` per frame each).
    pub fn decode(&self, encoded: &[f32], lstm: &[f32], frames: usize) -> Vec<f32> {
        let mut x = Vec::with_capacity(frames * 2 * HIDDEN);
        for (e, l) in encoded.chunks_exact(HIDDEN).zip(lstm.chunks_exact(HIDDEN)) {
            x.extend_from_slice(e);
            x.extend_from_slice(l);
        }
        let mut h = linear(&x, frames, &self.fc2, None);
        self.bn2.apply(&mut h);
        h.iter_mut().for_each(|v| *v = v.max(0.0));
        let mut m = linear(&h, frames, &self.fc3, None);
        self.bn3.apply(&mut m);
        for row in m.chunks_exact_mut(BINS) {
            for ((v, s), b) in row
                .iter_mut()
                .zip(&self.output_scale)
                .zip(&self.output_mean)
            {
                *v = (*v * s + b).max(0.0);
            }
        }
        m
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lstm_step_by_hand() {
        // One unit is enough to check the gate order and the recurrence:
        // run a direction with weights where only the first unit matters.
        let mut w_ih = vec![0f32; 4 * UNITS * HIDDEN];
        let mut w_hh = vec![0f32; 4 * UNITS * UNITS];
        let mut bias = vec![0f32; 4 * UNITS];
        // Gates i, f, g, o of unit 0: input weight on x[0], recurrent
        // weight on h[0], bias.
        let gates = [
            (0.5, 0.25, 0.1),
            (-0.3, 0.5, 0.2),
            (0.8, -0.4, 0.0),
            (0.2, 0.3, -0.1),
        ];
        for (k, &(wi, wh, b)) in gates.iter().enumerate() {
            w_ih[k * UNITS * HIDDEN] = wi;
            w_hh[k * UNITS * UNITS] = wh;
            bias[k * UNITS] = b;
        }
        let d = Direction {
            w_ih: Tensor::from_f32(HIDDEN, 4 * UNITS, w_ih),
            w_hh: Tensor::from_f32(UNITS, 4 * UNITS, w_hh),
            bias: Tensor::from_f32(4 * UNITS, 1, bias),
        };
        let xs = [1.0f32, -2.0, 0.5];
        let mut x = vec![0f32; xs.len() * HIDDEN];
        for (t, &v) in xs.iter().enumerate() {
            x[t * HIDDEN] = v;
        }
        let reference = |order: &mut dyn Iterator<Item = usize>| {
            let (mut h, mut c) = (0f64, 0f64);
            let mut out = [0f64; 3];
            let sig = |v: f64| 1.0 / (1.0 + (-v).exp());
            for t in order {
                let g = |k: usize| {
                    let (wi, wh, b) = gates[k];
                    wi as f64 * xs[t] as f64 + wh as f64 * h + b as f64
                };
                c = sig(g(1)) * c + sig(g(0)) * g(2).tanh();
                h = sig(g(3)) * c.tanh();
                out[t] = h;
            }
            out
        };
        for backward in [false, true] {
            let mut out = vec![0f32; xs.len() * UNITS];
            d.run(&x, xs.len(), backward, &mut out);
            let want = if backward {
                reference(&mut (0..3).rev())
            } else {
                reference(&mut (0..3))
            };
            for t in 0..3 {
                assert!(
                    (out[t * UNITS] as f64 - want[t]).abs() < 1e-6,
                    "{backward} {t}"
                );
                // Units without weights stay at zero (gate g = 0).
                assert!(out[t * UNITS + 1..(t + 1) * UNITS]
                    .iter()
                    .all(|&v| v == 0.0));
            }
        }
    }
}
