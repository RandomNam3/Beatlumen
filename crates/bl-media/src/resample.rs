//! Streaming polyphase windowed-sinc sample rate converter.
//!
//! The interpolation filter is a Kaiser-windowed sinc (β ≈ 9, ~90 dB
//! stopband) whose cutoff sits in the middle of a transition band that ends
//! exactly at the lower Nyquist frequency, so aliasing/imaging components
//! are attenuated by ≥ 80 dB while the passband extends to ~91 % of the
//! lower Nyquist frequency. Rational ratios (after GCD reduction) with a
//! moderate number of phases use an exact phase table; other ratios use a
//! 1024-phase table with linear interpolation between phases.
//!
//! The output is delayed by [`Resampler::latency`] frames (the filter is
//! primed with zeros instead of waiting for look-ahead input).

/// Stopband attenuation target in dB used to derive β and the length.
const ATTENUATION_DB: f64 = 90.0;
/// Passband edge relative to the lower Nyquist frequency.
const PASSBAND: f64 = 0.91;
/// Maximum table size (phases × taps) for the exact rational path.
const MAX_TABLE: usize = 1 << 20;
const MAX_TAPS: usize = 4096;
const INTERP_PHASES: usize = 1024;

/// High quality streaming sample rate converter for interleaved audio.
pub struct Resampler {
    channels: usize,
    in_rate: u32,
    out_rate: u32,
    passthrough: bool,
    /// Time step per output frame is `m / l` input samples.
    l: u64,
    m: u64,
    taps: usize,
    /// Number of table phases; rows `0..=phases` (row `phases` is row 0
    /// shifted by one tap, used for interpolation).
    phases: usize,
    interpolate: bool,
    table: Vec<f32>,
    /// Planar input history per channel.
    buf: Vec<Vec<f32>>,
    /// Index of the first tap of the next output frame in `buf`.
    pos: usize,
    /// Fractional position in units of `1 / l` input samples.
    frac: u64,
    latency: usize,
    /// Initial state (zero prefill length, start position, start phase).
    init: (usize, usize, u64),
    scratch: Vec<f32>,
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a
}

/// Zeroth order modified Bessel function of the first kind.
fn bessel_i0(x: f64) -> f64 {
    let mut sum = 1.0;
    let mut term = 1.0;
    let q = x * x / 4.0;
    for k in 1..200 {
        term *= q / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32, channels: u16) -> Resampler {
        let channels = channels as usize;
        let passthrough = in_rate == out_rate || in_rate == 0 || out_rate == 0;
        let g = gcd(in_rate.max(1) as u64, out_rate.max(1) as u64);
        let l = out_rate.max(1) as u64 / g;
        let m = in_rate.max(1) as u64 / g;
        let mut r = Resampler {
            channels,
            in_rate,
            out_rate,
            passthrough,
            l,
            m,
            taps: 0,
            phases: 1,
            interpolate: false,
            table: Vec::new(),
            buf: vec![Vec::new(); channels],
            pos: 0,
            frac: 0,
            latency: 0,
            init: (0, 0, 0),
            scratch: Vec::new(),
        };
        if !passthrough {
            r.design();
            r.reset();
        }
        r
    }

    fn design(&mut self) {
        // Ratio of the lower Nyquist frequency to the input Nyquist frequency.
        let ratio = (self.l as f64 / self.m as f64).min(1.0);
        let transition = 0.5 * (1.0 - PASSBAND) * ratio; // cycles per input sample
        let fc = 0.5 * ratio * (1.0 + PASSBAND) / 2.0;
        let beta = 0.1102 * (ATTENUATION_DB - 8.7);
        let n = ((ATTENUATION_DB - 7.95) / (14.36 * transition)).ceil() as usize + 1;
        let taps = (n.div_ceil(2) * 2).clamp(8, MAX_TAPS);
        let half = taps / 2;
        let (phases, interpolate) = if (self.l as usize) <= 4096 && self.l as usize * taps <= MAX_TABLE {
            (self.l as usize, false)
        } else {
            (INTERP_PHASES, true)
        };
        let i0b = bessel_i0(beta);
        let mut table = vec![0f32; (phases + 1) * taps];
        for p in 0..=phases {
            let phi = p as f64 / phases as f64;
            let row = &mut table[p * taps..(p + 1) * taps];
            let mut sum = 0.0;
            let mut vals = vec![0f64; taps];
            for (j, v) in vals.iter_mut().enumerate() {
                let tau = phi + half as f64 - 1.0 - j as f64;
                let x = tau / half as f64;
                if x.abs() >= 1.0 {
                    continue;
                }
                let arg = 2.0 * fc * tau;
                let sinc = if arg.abs() < 1e-12 { 1.0 } else { (std::f64::consts::PI * arg).sin() / (std::f64::consts::PI * arg) };
                let w = bessel_i0(beta * (1.0 - x * x).sqrt()) / i0b;
                *v = 2.0 * fc * sinc * w;
                sum += *v;
            }
            // Normalise every phase to unity DC gain.
            for (o, v) in row.iter_mut().zip(&vals) {
                *o = (v / sum) as f32;
            }
        }
        self.taps = taps;
        self.phases = phases;
        self.interpolate = interpolate;
        self.table = table;
        // Delay of `latency` output frames = latency * m / l input samples,
        // at least `half` so that every output is computable immediately.
        let latency = (half as u64 * self.l).div_ceil(self.m) as usize;
        self.latency = latency;
        let delay_num = latency as u64 * self.m; // in 1/l input samples
        let zeros = (delay_num.div_ceil(self.l)) as usize + half;
        // Time of output 0 in buffer coordinates: zeros - delay.
        let t = zeros as u64 * self.l - delay_num;
        let start = (t / self.l) as usize + 1 - half;
        self.init = (zeros, start, t % self.l);
    }

    /// Converts `input`, appending to `out`. Keeps internal history between
    /// calls so chunk boundaries are seamless.
    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        let ch = self.channels;
        if ch == 0 {
            return;
        }
        let input = &input[..input.len() / ch * ch];
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        for (c, b) in self.buf.iter_mut().enumerate() {
            b.extend(input.iter().skip(c).step_by(ch));
        }
        let taps = self.taps;
        let avail = self.buf[0].len();
        let est = ((avail.saturating_sub(self.pos)) as u64 * self.l / self.m) as usize + 1;
        out.reserve(est * ch);
        while self.pos + taps <= avail {
            let row: &[f32] = if self.interpolate {
                let x = self.frac as u128 * self.phases as u128;
                let p = (x / self.l as u128) as usize;
                let w = ((x % self.l as u128) as f64 / self.l as f64) as f32;
                let a = &self.table[p * taps..(p + 1) * taps];
                let b = &self.table[(p + 1) * taps..(p + 2) * taps];
                self.scratch.clear();
                self.scratch.extend(a.iter().zip(b).map(|(&a, &b)| a + (b - a) * w));
                &self.scratch
            } else {
                let p = self.frac as usize;
                &self.table[p * taps..(p + 1) * taps]
            };
            for b in &self.buf {
                out.push(dot(&b[self.pos..self.pos + taps], row));
            }
            self.frac += self.m;
            self.pos += (self.frac / self.l) as usize;
            self.frac %= self.l;
        }
        // Drop history that is no longer needed.
        let consumed = self.pos.min(avail);
        if consumed > 0 {
            for b in &mut self.buf {
                b.drain(..consumed);
            }
            self.pos -= consumed;
        }
    }

    /// Clears the history (after seeking).
    pub fn reset(&mut self) {
        if self.passthrough {
            return;
        }
        let (zeros, start, frac) = self.init;
        for b in &mut self.buf {
            b.clear();
            b.resize(zeros, 0.0);
        }
        self.pos = start;
        self.frac = frac;
    }

    /// Output latency in output frames introduced by the filter.
    pub fn latency(&self) -> usize {
        self.latency
    }

    /// Input sample rate.
    pub fn in_rate(&self) -> u32 {
        self.in_rate
    }

    /// Output sample rate.
    pub fn out_rate(&self) -> u32 {
        self.out_rate
    }
}

#[inline]
fn dot(a: &[f32], b: &[f32]) -> f32 {
    let mut acc = [0f32; 8];
    let ca = a.chunks_exact(8);
    let cb = b.chunks_exact(8);
    let (ra, rb) = (ca.remainder(), cb.remainder());
    for (x, y) in ca.zip(cb) {
        for i in 0..8 {
            acc[i] += x[i] * y[i];
        }
    }
    let mut s = (acc[0] + acc[4]) + (acc[1] + acc[5]) + (acc[2] + acc[6]) + (acc[3] + acc[7]);
    for (x, y) in ra.iter().zip(rb) {
        s += x * y;
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(rate: u32, freq: f64, n: usize) -> Vec<f32> {
        (0..n).map(|i| (0.5 * (2.0 * std::f64::consts::PI * freq * i as f64 / rate as f64).sin()) as f32).collect()
    }

    /// SNR of the resampled sine against the ideal sine (after latency).
    fn snr(in_rate: u32, out_rate: u32, freq: f64, chunk: usize) -> f64 {
        let n = in_rate as usize;
        let x = sine(in_rate, freq, n);
        let mut r = Resampler::new(in_rate, out_rate, 1);
        let mut y = Vec::new();
        for c in x.chunks(chunk) {
            r.process(c, &mut y);
        }
        let lat = r.latency();
        let expected_len = (n as u64 * out_rate as u64 / in_rate as u64) as i64;
        assert!((y.len() as i64 - expected_len).abs() <= 2, "{} vs {}", y.len(), expected_len);
        let (mut sig, mut err) = (0.0f64, 0.0f64);
        // Skip filter warm-up / tail regions.
        let (a, b) = (lat + out_rate as usize / 20, y.len() - out_rate as usize / 20);
        for (i, &v) in y.iter().enumerate().take(b).skip(a) {
            let t = (i - lat) as f64 / out_rate as f64;
            let ideal = 0.5 * (2.0 * std::f64::consts::PI * freq * t).sin();
            sig += ideal * ideal;
            err += (v as f64 - ideal).powi(2);
        }
        10.0 * (sig / err).log10()
    }

    #[test]
    fn sine_snr() {
        for &(a, b) in &[(44100, 48000), (48000, 44100), (22050, 48000), (96000, 44100), (44100, 44101), (8000, 48000)] {
            for &f in &[100.0, 1000.0, 3500.0, 7000.0, 18000.0] {
                if f > 0.9 * 0.5 * a.min(b) as f64 {
                    continue;
                }
                let s = snr(a, b, f, 1000);
                println!("resampler {a} -> {b} Hz, {f} Hz sine: SNR {s:.1} dB");
                assert!(s > 80.0, "{a}->{b} {f} Hz: SNR {s:.1} dB");
            }
        }
    }

    #[test]
    fn aliasing_rejected() {
        // A 23 kHz tone in 48 kHz input must vanish when converting to 44.1 kHz.
        let x = sine(48000, 23000.0, 48000);
        let mut r = Resampler::new(48000, 44100, 1);
        let mut y = Vec::new();
        r.process(&x, &mut y);
        let lat = r.latency() + 2000;
        let rms = (y[lat..y.len() - 2000].iter().map(|v| (*v as f64).powi(2)).sum::<f64>() / (y.len() - lat - 2000) as f64).sqrt();
        let db = 20.0 * (rms / (0.5 / 2f64.sqrt())).log10();
        println!("resampler 48000 -> 44100 Hz, 23 kHz alias level {db:.1} dB");
        assert!(db < -80.0, "alias level {db:.1} dB");
    }

    #[test]
    fn chunking_is_seamless_and_reset_works() {
        let x: Vec<f32> = sine(44100, 440.0, 20000).iter().flat_map(|&v| [v, -v]).collect();
        let mut a = Resampler::new(44100, 48000, 2);
        let mut ya = Vec::new();
        a.process(&x, &mut ya);
        let mut b = Resampler::new(44100, 48000, 2);
        let mut yb = Vec::new();
        for c in x.chunks(2 * 37) {
            b.process(c, &mut yb);
        }
        assert_eq!(ya, yb);
        b.reset();
        let mut yc = Vec::new();
        b.process(&x, &mut yc);
        assert_eq!(ya, yc);
        let mut p = Resampler::new(48000, 48000, 2);
        let mut yp = Vec::new();
        p.process(&x, &mut yp);
        assert_eq!(yp, x);
        assert_eq!(p.latency(), 0);
    }
}
