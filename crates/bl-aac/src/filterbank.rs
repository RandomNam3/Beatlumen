//! AAC synthesis filterbank: IMDCT (2048 / 8 x 256), sine and KBD windows,
//! block switching and overlap-add (ISO/IEC 14496-3 4.6.11).

use crate::mdct::Imdct;

#[allow(dead_code)]
pub(crate) const ONLY_LONG: u8 = 0;
pub(crate) const LONG_START: u8 = 1;
pub(crate) const EIGHT_SHORT: u8 = 2;
pub(crate) const LONG_STOP: u8 = 3;

/// Output scaling: the standard's IMDCT (2/N normalisation) produces 16-bit
/// PCM scale; we output [-1, 1].
const OUTPUT_SCALE: f64 = 1.0 / 32768.0;

fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..60 {
        term *= q / (k * k) as f64;
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

/// Rising half (length n/2) of a Kaiser-Bessel-derived window of length n.
fn kbd_window(n: usize, alpha: f64) -> Vec<f32> {
    let half = n / 2;
    let kaiser: Vec<f64> = (0..=half)
        .map(|i| {
            let r = (i as f64 - n as f64 / 4.0) / (n as f64 / 4.0);
            bessel_i0(std::f64::consts::PI * alpha * (1.0 - r * r).max(0.0).sqrt())
        })
        .collect();
    let total: f64 = kaiser.iter().sum();
    let mut acc = 0.0;
    (0..half)
        .map(|i| {
            acc += kaiser[i];
            (acc / total).sqrt() as f32
        })
        .collect()
}

/// Rising half (length n/2) of a sine window of length n.
fn sine_window(n: usize) -> Vec<f32> {
    (0..n / 2)
        .map(|i| (std::f64::consts::PI / n as f64 * (i as f64 + 0.5)).sin() as f32)
        .collect()
}

/// Per-channel filterbank state.
#[derive(Clone)]
pub(crate) struct ChannelState {
    pub overlap: Vec<f32>,
    pub prev_shape: u8,
}

impl ChannelState {
    pub fn new() -> Self {
        ChannelState {
            overlap: vec![0.0; 1024],
            prev_shape: 0,
        }
    }
    pub fn reset(&mut self) {
        self.overlap.iter_mut().for_each(|v| *v = 0.0);
        self.prev_shape = 0;
    }
}

pub(crate) struct Filterbank {
    long: Imdct,
    short: Imdct,
    /// [sine, kbd] rising halves.
    long_win: [Vec<f32>; 2],
    short_win: [Vec<f32>; 2],
    buf: Vec<f32>,
    tmp: Vec<f32>,
}

impl Filterbank {
    pub fn new() -> Filterbank {
        Filterbank {
            long: Imdct::new(1024, OUTPUT_SCALE / 1024.0),
            short: Imdct::new(128, OUTPUT_SCALE / 128.0),
            long_win: [sine_window(2048), kbd_window(2048, 4.0)],
            short_win: [sine_window(256), kbd_window(256, 6.0)],
            buf: vec![0.0; 2048],
            tmp: vec![0.0; 2048],
        }
    }

    /// Inverse transform of one frame of 1024 spectral coefficients (or
    /// 8 x 128 for short windows), producing 1024 output samples.
    pub fn synthesize(
        &mut self,
        spec: &[f32],
        seq: u8,
        shape: u8,
        st: &mut ChannelState,
        out: &mut [f32],
    ) {
        let prev = (st.prev_shape & 1) as usize;
        let cur = (shape & 1) as usize;
        let out = &mut out[..1024];
        if seq == EIGHT_SHORT {
            let tmp = &mut self.tmp;
            tmp.iter_mut().for_each(|v| *v = 0.0);
            for w in 0..8 {
                let buf = &mut self.buf[..256];
                self.short.transform(&spec[w * 128..w * 128 + 128], buf);
                let wl = if w == 0 {
                    &self.short_win[prev]
                } else {
                    &self.short_win[cur]
                };
                let wr = &self.short_win[cur];
                let base = 448 + 128 * w;
                for n in 0..128 {
                    tmp[base + n] += buf[n] * wl[n];
                    tmp[base + 128 + n] += buf[128 + n] * wr[127 - n];
                }
            }
            for n in 0..1024 {
                out[n] = st.overlap[n] + tmp[n];
                st.overlap[n] = tmp[1024 + n];
            }
        } else {
            let buf = &mut self.buf;
            self.long.transform(&spec[..1024], buf);
            // Left half.
            if seq == LONG_STOP {
                let ws = &self.short_win[prev];
                for n in 0..448 {
                    out[n] = st.overlap[n];
                }
                for n in 0..128 {
                    out[448 + n] = st.overlap[448 + n] + buf[448 + n] * ws[n];
                }
                for n in 576..1024 {
                    out[n] = st.overlap[n] + buf[n];
                }
            } else {
                let wl = &self.long_win[prev];
                for n in 0..1024 {
                    out[n] = st.overlap[n] + buf[n] * wl[n];
                }
            }
            // Right half -> overlap.
            if seq == LONG_START {
                let ws = &self.short_win[cur];
                st.overlap[..448].copy_from_slice(&buf[1024..1472]);
                for n in 0..128 {
                    st.overlap[448 + n] = buf[1472 + n] * ws[127 - n];
                }
                st.overlap[576..].iter_mut().for_each(|v| *v = 0.0);
            } else {
                let wr = &self.long_win[cur];
                for n in 0..1024 {
                    st.overlap[n] = buf[1024 + n] * wr[1023 - n];
                }
            }
        }
        st.prev_shape = shape;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_power_complementary() {
        for w in [
            sine_window(2048),
            kbd_window(2048, 4.0),
            sine_window(256),
            kbd_window(256, 6.0),
        ] {
            let h = w.len();
            for n in 0..h {
                let s = w[n] * w[n] + w[h - 1 - n] * w[h - 1 - n];
                assert!((s - 1.0).abs() < 1e-5, "{s}");
            }
        }
    }
}
