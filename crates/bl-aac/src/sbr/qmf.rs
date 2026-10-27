//! SBR QMF banks (ISO/IEC 14496-3 4.6.18.4): 32-band complex analysis of
//! the core signal and 64-band complex synthesis at twice the core rate,
//! both computed with FFTs.

use super::tables::QMF_C_Q31;
use crate::fft::{Complex, Fft};
use std::f64::consts::PI;
use std::sync::OnceLock;

/// Input scaling: the SBR envelope data refer to 16-bit PCM scale.
const PCM_SCALE: f32 = 32768.0;

pub(crate) struct QmfTables {
    c: Vec<f32>,
    fft64: Fft,
    fft128: Fft,
    ana_pre: Vec<Complex>,
    ana_post: Vec<Complex>,
    syn_pre: Vec<Complex>,
    syn_post: Vec<Complex>,
    /// Downsampled (32-band) synthesis twiddles.
    ds_pre: Vec<Complex>,
    ds_post: Vec<Complex>,
}

pub(crate) fn tables() -> &'static QmfTables {
    static T: OnceLock<QmfTables> = OnceLock::new();
    T.get_or_init(|| QmfTables {
        c: QMF_C_Q31
            .iter()
            .map(|&v| (v as f64 / 2147483648.0) as f32)
            .collect(),
        fft64: Fft::new(64),
        fft128: Fft::new(128),
        ana_pre: (0..64)
            .map(|n| Complex::expi(-PI * n as f64 / 64.0))
            .collect(),
        ana_post: (0..32)
            .map(|k| {
                let c = Complex::expi(-PI * (k as f64 + 0.5) / 128.0);
                Complex::new(2.0 * c.re, 2.0 * c.im)
            })
            .collect(),
        syn_pre: (0..64)
            .map(|k| {
                // conj(e^{-i pi 255 (k + 1/2) / 128}) / 64, with output scaling.
                let c = Complex::expi(PI * 255.0 * (k as f64 + 0.5) / 128.0);
                let s = 1.0 / (64.0 * PCM_SCALE as f64);
                Complex::new((c.re as f64 * s) as f32, (c.im as f64 * s) as f32)
            })
            .collect(),
        syn_post: (0..128)
            .map(|n| Complex::expi(PI * n as f64 / 128.0))
            .collect(),
        ds_pre: (0..32)
            .map(|k| {
                let c = Complex::expi(PI * 127.5 * (k as f64 + 0.5) / 64.0);
                let s = 1.0 / (64.0 * PCM_SCALE as f64);
                Complex::new((c.re as f64 * s) as f32, (c.im as f64 * s) as f32)
            })
            .collect(),
        ds_post: (0..64)
            .map(|n| Complex::expi(PI * n as f64 / 64.0))
            .collect(),
    })
}

/// 32-band analysis filterbank state.
#[derive(Clone)]
pub(crate) struct Analysis {
    x: Vec<f32>,
    buf: Vec<Complex>,
}

impl Analysis {
    pub fn new() -> Self {
        Analysis {
            x: vec![0.0; 320],
            buf: vec![Complex::default(); 64],
        }
    }

    pub fn reset(&mut self) {
        self.x.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Processes 32 input samples into 32 complex subband samples.
    pub fn process(&mut self, t: &QmfTables, input: &[f32], out: &mut [Complex]) {
        self.x.copy_within(0..288, 32);
        for n in 0..32 {
            self.x[n] = input[31 - n] * PCM_SCALE;
        }
        for n in 0..64 {
            let mut u = 0.0f32;
            for j in 0..5 {
                let i = n + 64 * j;
                u += self.x[i] * t.c[2 * i];
            }
            let p = t.ana_pre[n];
            self.buf[n] = Complex::new(u * p.re, u * p.im);
        }
        t.fft64.forward(&mut self.buf);
        for k in 0..32 {
            let b = self.buf[k];
            out[k] = t.ana_post[k].mul(Complex::new(b.re, -b.im));
        }
    }
}

/// 64-band synthesis filterbank state.
#[derive(Clone)]
pub(crate) struct Synthesis {
    v: Vec<f32>,
    buf: Vec<Complex>,
}

impl Synthesis {
    pub fn new() -> Self {
        Synthesis {
            v: vec![0.0; 1280],
            buf: vec![Complex::default(); 128],
        }
    }

    pub fn reset(&mut self) {
        self.v.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Synthesises 64 output samples from 64 complex subband samples.
    pub fn process(&mut self, t: &QmfTables, x: &[Complex], out: &mut [f32]) {
        self.v.copy_within(0..1152, 128);
        for k in 0..64 {
            // conj(X[k] * pre[k]) = conj(X) * conj(pre); syn_pre holds conj(pre).
            let a = Complex::new(x[k].re, -x[k].im);
            self.buf[k] = a.mul(t.syn_pre[k]);
        }
        for b in &mut self.buf[64..] {
            *b = Complex::default();
        }
        t.fft128.forward(&mut self.buf);
        for n in 0..128 {
            let d = self.buf[n];
            let p = t.syn_post[n];
            self.v[n] = p.re * d.re + p.im * d.im;
        }
        let c = &t.c;
        for n in 0..64 {
            let mut acc = 0.0f32;
            for i in 0..5 {
                acc += self.v[256 * i + n] * c[128 * i + n];
                acc += self.v[256 * i + 192 + n] * c[128 * i + 64 + n];
            }
            out[n] = acc;
        }
    }
}

/// Downsampled 32-band synthesis filterbank state (output at the core
/// rate, used when the SBR output rate equals the core rate).
#[derive(Clone)]
pub(crate) struct SynthesisDs {
    v: Vec<f32>,
    buf: Vec<Complex>,
}

impl SynthesisDs {
    pub fn new() -> Self {
        SynthesisDs {
            v: vec![0.0; 640],
            buf: vec![Complex::default(); 64],
        }
    }

    pub fn reset(&mut self) {
        self.v.iter_mut().for_each(|v| *v = 0.0);
    }

    /// Synthesises 32 output samples from the lower 32 subband samples.
    pub fn process(&mut self, t: &QmfTables, x: &[Complex], out: &mut [f32]) {
        self.v.copy_within(0..576, 64);
        for k in 0..32 {
            let a = Complex::new(x[k].re, -x[k].im);
            self.buf[k] = a.mul(t.ds_pre[k]);
        }
        for b in &mut self.buf[32..] {
            *b = Complex::default();
        }
        t.fft64.forward(&mut self.buf);
        for n in 0..64 {
            let d = self.buf[n];
            let p = t.ds_post[n];
            self.v[n] = p.re * d.re + p.im * d.im;
        }
        let c = &t.c;
        for n in 0..32 {
            let mut acc = 0.0f32;
            for i in 0..5 {
                acc += self.v[128 * i + n] * c[2 * (64 * i + n)];
                acc += self.v[128 * i + 96 + n] * c[2 * (64 * i + 32 + n)];
            }
            out[n] = acc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Analysis followed by synthesis (no high band) must reproduce the
    /// input at twice the sampling rate.
    #[test]
    fn qmf_roundtrip_upsamples() {
        let t = tables();
        let mut ana = Analysis::new();
        let mut syn = Synthesis::new();
        let n = 32 * 300;
        let f = 0.013; // cycles per input sample
        let input: Vec<f32> = (0..n)
            .map(|i| (0.5 * (2.0 * PI * f * i as f64).sin()) as f32)
            .collect();
        let mut out = Vec::new();
        let mut sub = vec![Complex::default(); 32];
        let mut full = vec![Complex::default(); 64];
        let mut o = vec![0.0; 64];
        for s in 0..n / 32 {
            ana.process(t, &input[s * 32..s * 32 + 32], &mut sub);
            full[..32].copy_from_slice(&sub);
            syn.process(t, &full, &mut o);
            out.extend_from_slice(&o);
        }
        // Find the delay and compare against the ideal upsampled sine.
        let mut best = (f64::MAX, 0);
        for d in 0..1300 {
            let mut e = 0.0;
            for i in 4000..8000 {
                let r = 0.5 * (2.0 * PI * f / 2.0 * (i as f64 - d as f64)).sin();
                e += (out[i] as f64 - r).powi(2);
            }
            if e < best.0 {
                best = (e, d);
            }
        }
        let snr = 10.0 * (4000.0 * 0.125 / best.0).log10();
        assert!(snr > 50.0, "roundtrip SNR {snr} dB (delay {})", best.1);
    }

    /// Analysis followed by downsampled synthesis reproduces the input
    /// (unity gain) at the same rate.
    #[test]
    fn qmf_roundtrip_downsampled() {
        let t = tables();
        let mut ana = Analysis::new();
        let mut syn = SynthesisDs::new();
        let n = 32 * 300;
        let f = 0.0227;
        let input: Vec<f32> = (0..n)
            .map(|i| (0.5 * (2.0 * PI * f * i as f64).sin()) as f32)
            .collect();
        let mut out = Vec::new();
        let mut sub = vec![Complex::default(); 32];
        let mut o = vec![0.0; 32];
        for s in 0..n / 32 {
            ana.process(t, &input[s * 32..s * 32 + 32], &mut sub);
            syn.process(t, &sub, &mut o);
            out.extend_from_slice(&o);
        }
        // Least-squares fit of a sine at the input frequency.
        let (mut ss, mut sc, mut cc, mut ys, mut yc) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for i in 2000..8000 {
            let (s_, c_) = (
                (2.0 * PI * f * i as f64).sin(),
                (2.0 * PI * f * i as f64).cos(),
            );
            ss += s_ * s_;
            sc += s_ * c_;
            cc += c_ * c_;
            ys += out[i] as f64 * s_;
            yc += out[i] as f64 * c_;
        }
        let det = ss * cc - sc * sc;
        let a = (ys * cc - yc * sc) / det;
        let b = (yc * ss - ys * sc) / det;
        let amp = (a * a + b * b).sqrt();
        assert!((amp - 0.5).abs() < 0.005, "amplitude {amp}");
    }
}
