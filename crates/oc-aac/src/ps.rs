//! Parametric stereo (HE-AAC v2, ISO/IEC 14496-3 8.6.4), baseline decoder:
//! hybrid analysis (20-band configuration; 34-band parameters are mapped
//! to 20 bands), transient-ducked all-pass decorrelation, mixing
//! procedures R_a and R_b with parameter interpolation, hybrid synthesis.
//! IPD/OPD extension data is skipped (as allowed for baseline decoders).

use crate::bits::BitReader;
use crate::fft::Complex;
use crate::huffman::HuffTable;
use crate::sbr::Slot;
use crate::sbr_huffman_tables::*;
use crate::Error;
use std::f64::consts::PI;
use std::sync::OnceLock;

const SLOTS: usize = 32;
/// Bands of the hybrid domain: 10 hybrid sub-bands + QMF bands 3..63.
const NB: usize = 71;
const NBINS: usize = 20;
const MAX_ENV: usize = 5;
/// Hybrid-domain bands that use all-pass decorrelation (QMF bands < 23).
const NR_ALLPASS: usize = 30;
/// First hybrid-domain band with the short (1 slot) delay (QMF band 35).
const SHORT_DELAY_BAND: usize = 42;
const LONG_DELAY: usize = 14;

const G0: [f32; 13] = [
    0.00746082949812,
    0.02270420949825,
    0.04546865930473,
    0.07266113929591,
    0.09885108575264,
    0.11793710567217,
    0.125,
    0.11793710567217,
    0.09885108575264,
    0.07266113929591,
    0.04546865930473,
    0.02270420949825,
    0.00746082949812,
];
const G1: [f32; 13] = [
    0.0,
    0.01899487526049,
    0.0,
    -0.07293139167538,
    0.0,
    0.30596630545168,
    0.5,
    0.30596630545168,
    0.0,
    -0.07293139167538,
    0.0,
    0.01899487526049,
    0.0,
];

/// Centre frequencies (in QMF band units) of the 10 hybrid sub-bands, in
/// the order produced by [`Ps::hybrid_analysis`].
const F_CENTER_HYBRID: [f64; 10] = [0.125, 0.375, 0.625, 0.875, -0.375, -0.125, 1.75, 1.25, 2.25, 2.75];
/// Parameter bin of each hybrid sub-band.
const BIN_HYBRID: [usize; 10] = [0, 1, 2, 3, 1, 0, 5, 4, 6, 7];

const IID_COARSE_DB: [f64; 15] = [-25.0, -18.0, -14.0, -10.0, -7.0, -4.0, -2.0, 0.0, 2.0, 4.0, 7.0, 10.0, 14.0, 18.0, 25.0];
const IID_FINE_DB: [f64; 31] = [
    -50.0, -45.0, -40.0, -35.0, -30.0, -25.0, -22.0, -19.0, -16.0, -13.0, -10.0, -8.0, -6.0, -4.0, -2.0, 0.0, 2.0, 4.0, 6.0,
    8.0, 10.0, 13.0, 16.0, 19.0, 22.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0,
];
const ICC_RHO: [f64; 8] = [1.0, 0.937, 0.84118, 0.60092, 0.36764, 0.0, -0.589, -1.0];

const PEAK_DECAY: f32 = 0.765_928_34;
const ALPHA_SMOOTH: f32 = 0.25;
const TRANSIENT_IMPACT: f32 = 1.5;
const AP_DELAYS: [usize; 3] = [3, 4, 5];
const AP_A: [f64; 3] = [0.65143905753106, 0.56471812200776, 0.48954165955695];
const AP_Q: [f64; 3] = [0.43, 0.75, 0.347];
const Q_PHI: f64 = 0.39;

fn qmf_bin(k: usize) -> usize {
    match k {
        3..=8 => k + 5,
        9..=10 => 14,
        11..=13 => 15,
        14..=17 => 16,
        18..=22 => 17,
        23..=34 => 18,
        _ => 19,
    }
}

struct Tables {
    /// Parameter bin per hybrid-domain band.
    bin: [usize; NB],
    /// Hybrid filters for QMF band 0 (8 complex) and bands 1-2 (2 real).
    g8: [[Complex; 13]; 8],
    g2: [[f32; 13]; 2],
    /// All-pass constants per band: phi_fract, Q_fract[3], a*g[3].
    phi: [Complex; NR_ALLPASS],
    qf: [[Complex; 3]; NR_ALLPASS],
    ag: [[f32; 3]; NR_ALLPASS],
    iid_huff: [HuffTable; 4],
    icc_huff: [HuffTable; 2],
}

fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut bin = [0usize; NB];
        for (b, v) in bin.iter_mut().enumerate() {
            *v = if b < 10 { BIN_HYBRID[b] } else { qmf_bin(b - 7) };
        }
        let mut g8 = [[Complex::default(); 13]; 8];
        for (q, g) in g8.iter_mut().enumerate() {
            for n in 0..13 {
                let ph = PI / 4.0 * (q as f64 + 0.5) * (n as f64 - 6.0);
                let c = Complex::expi(ph);
                g[n] = Complex::new(c.re * G0[n], c.im * G0[n]);
            }
        }
        let mut g2 = [[0f32; 13]; 2];
        for (q, g) in g2.iter_mut().enumerate() {
            for n in 0..13 {
                g[n] = G1[n] * (PI * q as f64 * (n as f64 - 6.0)).cos() as f32;
            }
        }
        let mut phi = [Complex::default(); NR_ALLPASS];
        let mut qf = [[Complex::default(); 3]; NR_ALLPASS];
        let mut ag = [[0f32; 3]; NR_ALLPASS];
        for k in 0..NR_ALLPASS {
            let (fc, qmf_band) = if k < 10 { (F_CENTER_HYBRID[k], 0usize) } else { ((k - 7) as f64 + 0.5, k - 7) };
            phi[k] = Complex::expi(-PI * Q_PHI * fc);
            let slope = if qmf_band <= 3 { 1.0 } else { (1.0 - 0.05 * (qmf_band as f64 - 3.0)).max(0.0) };
            for m in 0..3 {
                qf[k][m] = Complex::expi(-PI * AP_Q[m] * fc);
                ag[k][m] = (AP_A[m] * slope) as f32;
            }
        }
        Tables {
            bin,
            g8,
            g2,
            phi,
            qf,
            ag,
            iid_huff: [
                HuffTable::new(&PS_IID_F, 9),
                HuffTable::new(&PS_IID_T, 9),
                HuffTable::new(&PS_IID_FINE_F, 9),
                HuffTable::new(&PS_IID_FINE_T, 9),
            ],
            icc_huff: [HuffTable::new(&PS_ICC_F, 9), HuffTable::new(&PS_ICC_T, 9)],
        }
    })
}

/// Mixing matrix (h11, h12, h21, h22) for IID/ICC indices.
fn mixing_matrix(iid: i32, icc: i32, fine: bool, type_b: bool) -> [f32; 4] {
    let db = if fine { IID_FINE_DB[(iid + 15).clamp(0, 30) as usize] } else { IID_COARSE_DB[(iid + 7).clamp(0, 14) as usize] };
    let c = 10f64.powf(db / 20.0);
    let rho = ICC_RHO[icc.clamp(0, 7) as usize];
    if !type_b {
        let c1 = (2.0 / (1.0 + c * c)).sqrt();
        let c2 = c * c1;
        let alpha = 0.5 * rho.acos();
        let beta = alpha * (c1 - c2) / std::f64::consts::SQRT_2;
        [
            (c2 * (beta + alpha).cos()) as f32,
            (c1 * (beta - alpha).cos()) as f32,
            (c2 * (beta + alpha).sin()) as f32,
            (c1 * (beta - alpha).sin()) as f32,
        ]
    } else {
        let rho = rho.max(0.05);
        let mut alpha = 0.5 * (2.0 * c * rho).atan2(c * c - 1.0);
        if alpha < 0.0 {
            alpha += PI / 2.0;
        }
        let m = c + 1.0 / c;
        let mu = (1.0 + (4.0 * rho * rho - 4.0) / (m * m)).max(0.0).sqrt();
        let gamma = ((1.0 - mu) / (1.0 + mu)).sqrt().atan();
        let s2 = std::f64::consts::SQRT_2;
        [
            (s2 * alpha.cos() * gamma.cos()) as f32,
            (s2 * alpha.sin() * gamma.cos()) as f32,
            (-s2 * alpha.sin() * gamma.sin()) as f32,
            (s2 * alpha.cos() * gamma.sin()) as f32,
        ]
    }
}

fn map34_to_20(a: &mut [i32; 34]) {
    let v = *a;
    a[0] = (2 * v[0] + v[1]) / 3;
    a[1] = (v[1] + 2 * v[2]) / 3;
    a[2] = (2 * v[3] + v[4]) / 3;
    a[3] = (v[4] + 2 * v[5]) / 3;
    a[4] = (v[6] + v[7]) / 2;
    a[5] = (v[8] + v[9]) / 2;
    a[6] = v[10];
    a[7] = v[11];
    a[8] = (v[12] + v[13]) / 2;
    a[9] = (v[14] + v[15]) / 2;
    a[10] = v[16];
    a[11] = v[17];
    a[12] = v[18];
    a[13] = v[19];
    a[14] = (v[20] + v[21]) / 2;
    a[15] = (v[22] + v[23]) / 2;
    a[16] = (v[24] + v[25]) / 2;
    a[17] = (v[26] + v[27]) / 2;
    a[18] = (v[28] + v[29] + v[30] + v[31]) / 4;
    a[19] = (v[32] + v[33]) / 2;
}

pub(crate) struct Ps {
    header_seen: bool,
    enable_iid: bool,
    iid_mode: u8,
    enable_icc: bool,
    icc_mode: u8,
    enable_ext: bool,
    /// Parameters of the current frame (20-band resolution).
    num_env: usize,
    borders: [usize; MAX_ENV + 1],
    iid: [[i32; 34]; MAX_ENV],
    icc: [[i32; 34]; MAX_ENV],
    /// Last decoded parameters (parse resolution) for time-delta coding.
    iid_prev: [i32; 34],
    icc_prev: [i32; 34],
    data_valid: bool,
    // Processing state.
    h_prev: [[f32; 4]; NBINS],
    hyb_hist: [[Complex; 13]; 3],
    peak_decay: [f32; NBINS],
    smooth_nrg: [f32; NBINS],
    smooth_diff: [f32; NBINS],
    /// z^-2 pre-delay of the all-pass bands.
    ap_pre: [[Complex; 2]; NR_ALLPASS],
    /// All-pass section delay lines (length 5 rings).
    ap_buf: [[[Complex; 5]; 3]; NR_ALLPASS],
    ap_pos: [usize; 3],
    /// Plain delay lines for the remaining bands.
    delay: Vec<[Complex; LONG_DELAY]>,
    delay_pos: usize,
}

impl Ps {
    pub fn new() -> Self {
        Ps {
            header_seen: false,
            enable_iid: false,
            iid_mode: 0,
            enable_icc: false,
            icc_mode: 0,
            enable_ext: false,
            num_env: 0,
            borders: [0; MAX_ENV + 1],
            iid: [[0; 34]; MAX_ENV],
            icc: [[0; 34]; MAX_ENV],
            iid_prev: [0; 34],
            icc_prev: [0; 34],
            data_valid: false,
            // The first envelope fades in from a zero mixing matrix.
            h_prev: [[0.0; 4]; NBINS],
            hyb_hist: [[Complex::default(); 13]; 3],
            peak_decay: [0.0; NBINS],
            smooth_nrg: [0.0; NBINS],
            smooth_diff: [0.0; NBINS],
            ap_pre: [[Complex::default(); 2]; NR_ALLPASS],
            ap_buf: [[[Complex::default(); 5]; 3]; NR_ALLPASS],
            ap_pos: [0; 3],
            delay: vec![[Complex::default(); LONG_DELAY]; NB - NR_ALLPASS],
            delay_pos: 0,
        }
    }

    pub fn reset(&mut self) {
        let header = (self.header_seen, self.enable_iid, self.iid_mode, self.enable_icc, self.icc_mode, self.enable_ext);
        *self = Ps::new();
        (self.header_seen, self.enable_iid, self.iid_mode, self.enable_icc, self.icc_mode, self.enable_ext) = header;
    }

    /// Whether a PS header has been received (processing has started).
    pub fn started(&self) -> bool {
        self.header_seen
    }

    /// Discards the parameters of the current frame (parse error).
    pub fn invalidate(&mut self) {
        self.data_valid = false;
    }

    /// Parses ps_data().
    pub fn parse(&mut self, r: &mut BitReader) -> Result<(), Error> {
        self.data_valid = false;
        let t = tables();
        if r.read_bit() {
            self.enable_iid = r.read_bit();
            if self.enable_iid {
                self.iid_mode = r.read(3) as u8;
            }
            self.enable_icc = r.read_bit();
            if self.enable_icc {
                self.icc_mode = r.read(3) as u8;
            }
            self.enable_ext = r.read_bit();
            self.header_seen = true;
        }
        if !self.header_seen || self.iid_mode > 5 || self.icc_mode > 5 {
            return Err(Error::InvalidData("PS: no valid header"));
        }
        let frame_class = r.read_bit();
        let idx = r.read(2) as usize;
        let num_env = if frame_class { idx + 1 } else { [0, 1, 2, 4][idx] };
        let mut borders = [0usize; MAX_ENV + 1];
        if frame_class {
            for e in 1..=num_env {
                borders[e] = r.read(5) as usize + 1;
            }
        }
        let fine = self.iid_mode >= 3;
        let nbins = |mode: u8| [10usize, 20, 34][(mode % 3) as usize];
        let n_iid = nbins(self.iid_mode);
        let n_icc = nbins(self.icc_mode);
        let mut raw_iid = [[0i32; 34]; MAX_ENV];
        let mut raw_icc = [[0i32; 34]; MAX_ENV];
        let mut dt_iid = [false; MAX_ENV];
        let mut dt_icc = [false; MAX_ENV];
        if self.enable_iid {
            for e in 0..num_env {
                dt_iid[e] = r.read_bit();
                let (book, off) = match (fine, dt_iid[e]) {
                    (false, false) => (&t.iid_huff[0], PS_IID_F_OFFSET),
                    (false, true) => (&t.iid_huff[1], PS_IID_T_OFFSET),
                    (true, false) => (&t.iid_huff[2], PS_IID_FINE_F_OFFSET),
                    (true, true) => (&t.iid_huff[3], PS_IID_FINE_T_OFFSET),
                };
                for b in 0..n_iid {
                    raw_iid[e][b] = book.decode(r).ok_or(Error::InvalidData("PS: bad IID code"))? as i32 - off;
                }
            }
        }
        if self.enable_icc {
            for e in 0..num_env {
                dt_icc[e] = r.read_bit();
                let (book, off) = if dt_icc[e] { (&t.icc_huff[1], PS_ICC_T_OFFSET) } else { (&t.icc_huff[0], PS_ICC_F_OFFSET) };
                for b in 0..n_icc {
                    raw_icc[e][b] = book.decode(r).ok_or(Error::InvalidData("PS: bad ICC code"))? as i32 - off;
                }
            }
        }
        if self.enable_ext {
            let mut cnt = r.read(4) as usize;
            if cnt == 15 {
                cnt += r.read(8) as usize;
            }
            r.skip(8 * cnt); // IPD/OPD data: not used by baseline decoders
        }
        r.check()?;

        // Differential decoding.
        let max_iid = if fine { 15 } else { 7 };
        for e in 0..num_env {
            decode_par(self.enable_iid, &raw_iid[e], dt_iid[e], n_iid, &mut self.iid_prev, -max_iid, max_iid);
            self.iid[e] = self.iid_prev;
            decode_par(self.enable_icc, &raw_icc[e], dt_icc[e], n_icc, &mut self.icc_prev, 0, 7);
            self.icc[e] = self.icc_prev;
        }
        let mut num_env = num_env;
        if num_env == 0 {
            // Keep the previous parameters for the whole frame.
            num_env = 1;
            self.iid[0] = if self.enable_iid { self.iid_prev } else { [0; 34] };
            self.icc[0] = if self.enable_icc { self.icc_prev } else { [0; 34] };
        }
        // Envelope borders.
        if !frame_class {
            for e in 0..=num_env {
                borders[e] = e * SLOTS / num_env;
            }
        } else {
            borders[0] = 0;
            if borders[num_env] < SLOTS {
                if num_env < MAX_ENV {
                    self.iid[num_env] = self.iid[num_env - 1];
                    self.icc[num_env] = self.icc[num_env - 1];
                    num_env += 1;
                }
                borders[num_env] = SLOTS;
            }
            borders[num_env] = borders[num_env].min(SLOTS);
            for e in 1..num_env {
                let hi = SLOTS - num_env + e;
                let lo = borders[e - 1] + 1;
                borders[e] = borders[e].clamp(lo.min(hi), hi);
            }
        }
        for e in 0..num_env {
            if self.iid_mode % 3 == 2 {
                map34_to_20(&mut self.iid[e]);
            }
            if self.icc_mode % 3 == 2 {
                map34_to_20(&mut self.icc[e]);
            }
        }
        self.num_env = num_env;
        self.borders = borders;
        self.data_valid = true;
        Ok(())
    }

    /// Hybrid analysis of QMF bands 0..2 for one slot; `look` holds the
    /// lookahead samples (slot n + 6) of the three bands.
    fn hybrid_analysis(&mut self, t: &Tables, look: [Complex; 3], out: &mut [Complex; NB]) {
        for b in 0..3 {
            let h = &mut self.hyb_hist[b];
            h.copy_within(1..13, 0);
            h[12] = look[b];
        }
        // QMF band 0: 8 complex sub-bands (y_q = sum_j G_q(j) x(m - j)).
        let mut q8 = [Complex::default(); 8];
        let h0 = &self.hyb_hist[0];
        for (q, g) in t.g8.iter().enumerate() {
            let mut acc = Complex::default();
            for j in 0..13 {
                let x = h0[12 - j];
                let p = x.mul(g[j]);
                acc.re += p.re;
                acc.im += p.im;
            }
            q8[q] = acc;
        }
        let add = |a: Complex, b: Complex| Complex::new(a.re + b.re, a.im + b.im);
        out[0] = q8[0];
        out[1] = q8[1];
        out[2] = add(q8[2], q8[5]);
        out[3] = add(q8[3], q8[4]);
        out[4] = q8[6];
        out[5] = q8[7];
        // QMF bands 1 and 2: 2 real-filtered sub-bands each.
        for b in 1..3 {
            let h = &self.hyb_hist[b];
            for q in 0..2 {
                let mut acc = Complex::default();
                for j in 0..13 {
                    let g = t.g2[q][j];
                    if g != 0.0 {
                        acc.re += h[12 - j].re * g;
                        acc.im += h[12 - j].im * g;
                    }
                }
                out[6 + 2 * (b - 1) + q] = acc;
            }
        }
    }

    /// Applies parametric stereo to a mono QMF signal `x` (38 slots: the
    /// frame plus 6 lookahead slots for the lowest bands), producing the
    /// left and right QMF signals (32 slots).
    pub fn process(&mut self, x: &[Slot], left: &mut [Slot], right: &mut [Slot], usb: usize) {
        let t = tables();
        if !self.data_valid {
            // No new parameters: hold the last ones.
            self.num_env = 1;
            self.borders[0] = 0;
            self.borders[1] = SLOTS;
            self.iid[0] = self.iid_prev;
            self.icc[0] = self.icc_prev;
            if self.iid_mode % 3 == 2 {
                map34_to_20(&mut self.iid[0]);
            }
            if self.icc_mode % 3 == 2 {
                map34_to_20(&mut self.icc[0]);
            }
            if !self.header_seen {
                self.iid[0] = [0; 34];
                self.icc[0] = [0; 34];
            }
        }
        self.data_valid = false;
        let fine = self.iid_mode >= 3;
        let type_b = self.icc_mode >= 3;
        let mut s = [Complex::default(); NB];
        let mut d = [Complex::default(); NB];
        for e in 0..self.num_env {
            let (b0, b1) = (self.borders[e], self.borders[e + 1]);
            if b1 <= b0 {
                continue;
            }
            let mut target = [[0f32; 4]; NBINS];
            for (b, tg) in target.iter_mut().enumerate() {
                let iid = if self.enable_iid { self.iid[e][b] } else { 0 };
                let icc = if self.enable_icc { self.icc[e][b] } else { 0 };
                *tg = mixing_matrix(iid, icc, fine, type_b);
            }
            let len = (b1 - b0) as f32;
            for n in b0..b1 {
                let look = [
                    x.get(n + 6).map_or(Complex::default(), |sl| sl[0]),
                    x.get(n + 6).map_or(Complex::default(), |sl| sl[1]),
                    x.get(n + 6).map_or(Complex::default(), |sl| sl[2]),
                ];
                self.hybrid_analysis(t, look, &mut s);
                s[10..].copy_from_slice(&x[n][3..64]);
                self.decorrelate(t, &s, &mut d);
                // Interpolated mixing.
                let w = (n - b0 + 1) as f32 / len;
                let mut h = [[0f32; 4]; NBINS];
                for b in 0..NBINS {
                    for i in 0..4 {
                        h[b][i] = self.h_prev[b][i] + w * (target[b][i] - self.h_prev[b][i]);
                    }
                }
                let mut l = [Complex::default(); NB];
                let mut r = [Complex::default(); NB];
                for k in 0..NB {
                    let hb = &h[t.bin[k]];
                    l[k] = Complex::new(hb[0] * s[k].re + hb[2] * d[k].re, hb[0] * s[k].im + hb[2] * d[k].im);
                    r[k] = Complex::new(hb[1] * s[k].re + hb[3] * d[k].re, hb[1] * s[k].im + hb[3] * d[k].im);
                }
                // Hybrid synthesis.
                for (out, src) in [(&mut left[n], &l), (&mut right[n], &r)] {
                    let mut b0 = Complex::default();
                    for v in &src[0..6] {
                        b0.re += v.re;
                        b0.im += v.im;
                    }
                    out[0] = b0;
                    out[1] = Complex::new(src[6].re + src[7].re, src[6].im + src[7].im);
                    out[2] = Complex::new(src[8].re + src[9].re, src[8].im + src[9].im);
                    out[3..64].copy_from_slice(&src[10..]);
                    for v in &mut out[usb.clamp(3, 64)..] {
                        *v = Complex::default();
                    }
                }
            }
            self.h_prev = target;
        }
    }

    /// Transient-ducked decorrelation of one slot.
    fn decorrelate(&mut self, t: &Tables, s: &[Complex; NB], d: &mut [Complex; NB]) {
        // Transient detection per parameter bin.
        let mut p = [0f32; NBINS];
        for k in 0..NB {
            p[t.bin[k]] += s[k].re * s[k].re + s[k].im * s[k].im;
        }
        let mut gain = [1f32; NBINS];
        for b in 0..NBINS {
            self.peak_decay[b] = (self.peak_decay[b] * PEAK_DECAY).max(p[b]);
            self.smooth_nrg[b] += ALPHA_SMOOTH * (p[b] - self.smooth_nrg[b]);
            self.smooth_diff[b] += ALPHA_SMOOTH * (self.peak_decay[b] - p[b] - self.smooth_diff[b]);
            let thr = TRANSIENT_IMPACT * self.smooth_diff[b];
            if thr > self.smooth_nrg[b] {
                gain[b] = self.smooth_nrg[b] / thr;
            }
        }
        // All-pass filtered bands.
        let pos = self.ap_pos;
        for k in 0..NR_ALLPASS {
            let pre = &mut self.ap_pre[k];
            let delayed = pre[0];
            pre[0] = pre[1];
            pre[1] = s[k];
            let mut v = delayed.mul(t.phi[k]);
            for m in 0..3 {
                let len = AP_DELAYS[m];
                let buf = &mut self.ap_buf[k][m];
                let w_old = buf[pos[m]];
                let q = t.qf[k][m];
                let a = t.ag[k][m];
                // w(n) = x(n) + a Q w(n - d); y(n) = Q w(n - d) - a w(n)
                let qw = q.mul(w_old);
                let w = Complex::new(v.re + a * qw.re, v.im + a * qw.im);
                v = Complex::new(qw.re - a * w.re, qw.im - a * w.im);
                buf[pos[m]] = w;
                let _ = len;
            }
            let g = gain[t.bin[k]];
            d[k] = Complex::new(v.re * g, v.im * g);
        }
        for m in 0..3 {
            self.ap_pos[m] = (self.ap_pos[m] + 1) % AP_DELAYS[m];
        }
        // Delayed bands.
        let dp = self.delay_pos;
        for k in NR_ALLPASS..NB {
            let line = &mut self.delay[k - NR_ALLPASS];
            let len = if k < SHORT_DELAY_BAND { LONG_DELAY } else { 1 };
            let idx = if len == LONG_DELAY { dp } else { 0 };
            let out = line[idx];
            line[idx] = s[k];
            let g = gain[t.bin[k]];
            d[k] = Complex::new(out.re * g, out.im * g);
        }
        self.delay_pos = (self.delay_pos + 1) % LONG_DELAY;
    }
}

/// Delta decoding of one envelope of IID or ICC indices into `prev` (which
/// then holds the absolute values in 34-band capable resolution).
fn decode_par(enable: bool, raw: &[i32; 34], dt: bool, n: usize, prev: &mut [i32; 34], min: i32, max: i32) {
    if !enable {
        *prev = [0; 34];
        return;
    }
    let stride = if n == 10 { 2 } else { 1 };
    let mut cur = [0i32; 34];
    if !dt {
        let mut acc = 0;
        for b in 0..n {
            acc += raw[b];
            acc = acc.clamp(min, max);
            cur[b] = acc;
        }
    } else {
        for b in 0..n {
            cur[b] = (prev[b * stride] + raw[b]).clamp(min, max);
        }
    }
    if stride == 2 {
        for b in (0..20).rev() {
            cur[b] = cur[b / 2];
        }
    }
    *prev = cur;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hybrid_filters_reconstruct() {
        // Sum of all sub-band filters is a pure delay of 6 samples.
        let t = tables();
        for n in 0..13 {
            let s: f32 = t.g8.iter().map(|g| g[n].re).sum();
            let si: f32 = t.g8.iter().map(|g| g[n].im).sum();
            let expect = if n == 6 { 1.0 } else { 0.0 };
            assert!((s - expect).abs() < 1e-6 && si.abs() < 1e-6);
            let s2 = t.g2[0][n] + t.g2[1][n];
            assert!((s2 - expect).abs() < 1e-6);
        }
    }

    #[test]
    fn neutral_parameters_give_identity() {
        let h = mixing_matrix(0, 0, false, false);
        assert!((h[0] - 1.0).abs() < 1e-6 && (h[1] - 1.0).abs() < 1e-6);
        assert!(h[2].abs() < 1e-6 && h[3].abs() < 1e-6);
    }
}
