//! SBR signal processing for one channel and frame: LPC-based HF
//! generation (4.6.18.6), envelope adjustment with limiter, noise and
//! sinusoid insertion (4.6.18.7), and assembly of the QMF matrix fed to the
//! synthesis filterbank.

use super::freq::FreqTables;
use super::tables::NOISE_TABLE;
use super::{SbrChannel, SbrHeader, Slot, MAX_BANDS, MAX_ENV, SLOTS, T_HFADJ, T_HFGEN};
use crate::fft::Complex;

const LIM_GAINS: [f32; 4] = [0.70795, 1.0, 1.41254, 1e10];
const EPS0: f32 = 1e-12;
/// Smoothing filter, h_smooth[0] applies to the current slot.
const H_SMOOTH: [f32; 5] = [0.333_333_33, 0.301_502_83, 0.218_169_5, 0.115_163_83, 0.031_830_5];

fn chirp(invf: u8, prev_invf: u8, bw_prev: f32) -> f32 {
    let new = match invf {
        1 => {
            if prev_invf == 0 {
                0.6
            } else {
                0.75
            }
        }
        2 => 0.9,
        3 => 0.98,
        _ => {
            if prev_invf == 1 {
                0.6
            } else {
                0.0
            }
        }
    };
    let bw = if new < bw_prev { 0.75 * new + 0.25 * bw_prev } else { 0.90625 * new + 0.09375 * bw_prev };
    if bw < 0.015625 {
        0.0
    } else {
        bw.min(0.996_093_75)
    }
}

/// Complex LPC coefficients (alpha0, alpha1) of low band `k`.
fn lpc(x_low: &[[Complex; 32]], k: usize) -> ((f64, f64), (f64, f64)) {
    // phi(i, j) = sum_n x[n + 2 - i] * conj(x[n + 2 - j]), n in 0..38.
    let (mut r01, mut r02, mut r12) = ((0.0, 0.0), (0.0, 0.0), (0.0, 0.0));
    let (mut r11, mut r22) = (0.0f64, 0.0f64);
    for n in 0..(SLOTS + 6) {
        let x0 = x_low[n + 2][k];
        let x1 = x_low[n + 1][k];
        let x2 = x_low[n][k];
        let (a0r, a0i) = (x0.re as f64, x0.im as f64);
        let (a1r, a1i) = (x1.re as f64, x1.im as f64);
        let (a2r, a2i) = (x2.re as f64, x2.im as f64);
        r01.0 += a0r * a1r + a0i * a1i;
        r01.1 += a0i * a1r - a0r * a1i;
        r02.0 += a0r * a2r + a0i * a2i;
        r02.1 += a0i * a2r - a0r * a2i;
        r12.0 += a1r * a2r + a1i * a2i;
        r12.1 += a1i * a2r - a1r * a2i;
        r11 += a1r * a1r + a1i * a1i;
        r22 += a2r * a2r + a2i * a2i;
    }
    let d = r22 * r11 - (r12.0 * r12.0 + r12.1 * r12.1) / (1.0 + 1e-6);
    let mut a1 = (0.0, 0.0);
    if d != 0.0 {
        // (phi01 * phi12 - phi02 * phi11) / d
        a1.0 = (r01.0 * r12.0 - r01.1 * r12.1 - r02.0 * r11) / d;
        a1.1 = (r01.0 * r12.1 + r01.1 * r12.0 - r02.1 * r11) / d;
    }
    let mut a0 = (0.0, 0.0);
    if r11 != 0.0 {
        // -(phi01 + a1 * conj(phi12)) / phi11
        a0.0 = -(r01.0 + a1.0 * r12.0 + a1.1 * r12.1) / r11;
        a0.1 = -(r01.1 + a1.1 * r12.0 - a1.0 * r12.1) / r11;
    }
    if a0.0 * a0.0 + a0.1 * a0.1 >= 16.0 || a1.0 * a1.0 + a1.1 * a1.1 >= 16.0 {
        return ((0.0, 0.0), (0.0, 0.0));
    }
    (a0, a1)
}

/// Processes one frame of one channel. `ft` is `None` when no SBR data is
/// available (the low band is then only resampled). Writes 32 slots of the
/// combined QMF signal into `x`.
pub(super) fn process(ch: &mut SbrChannel, ft: Option<&FreqTables>, h: &SbrHeader, reset: bool, x: &mut [Slot]) {
    // X_low only carries the bands below the crossover of the frame it
    // belongs to (history slots: previous kx, new slots: current kx).
    let kx_now = ft.map_or(32, |f| f.kx.min(32));
    for slot in &mut ch.x_low[T_HFGEN..] {
        for v in &mut slot[kx_now..] {
            *v = Complex::default();
        }
    }
    let (kx, m, t_e0) = match ft {
        Some(ft) => {
            adjust(ch, ft, h, reset);
            (ft.kx, ft.m, ch.frame.grid.t_e[0])
        }
        None => (32, 0, 0),
    };
    // Assemble the synthesis input.
    for (j, slot) in x.iter_mut().enumerate().take(SLOTS) {
        let (kxj, mj) = if j < 2 * t_e0 { (ch.kx_prev, ch.m_prev) } else { (kx, m) };
        let low = &ch.x_low[j + T_HFADJ];
        let kxj = kxj.min(32);
        slot[..kxj].copy_from_slice(&low[..kxj]);
        let hi_end = (kxj + mj).min(64);
        slot[kxj..hi_end].copy_from_slice(&ch.y[j][kxj..hi_end]);
        for v in &mut slot[hi_end..] {
            *v = Complex::default();
        }
    }
    // Lookahead slots (low band only) for the parametric stereo hybrid
    // analysis.
    for j in SLOTS..x.len().min(SLOTS + 6) {
        let low = &ch.x_low[j + T_HFADJ];
        let slot = &mut x[j];
        let kxj = kx.min(32);
        slot[..kxj].copy_from_slice(&low[..kxj]);
        for v in &mut slot[kxj..] {
            *v = Complex::default();
        }
    }
    // Shift histories.
    ch.x_low.copy_within(SLOTS..SLOTS + T_HFGEN, 0);
    ch.y.copy_within(SLOTS..SLOTS + 6, 0);
    for s in &mut ch.y[6..] {
        *s = [Complex::default(); 64];
    }
    ch.kx_prev = kx;
    ch.m_prev = m;
}

/// HF generation and envelope adjustment into `ch.y`.
fn adjust(ch: &mut SbrChannel, ft: &FreqTables, h: &SbrHeader, reset: bool) {
    let grid = ch.frame.grid;
    let kx = ft.kx;
    let m = ft.m;
    let l_start = 2 * grid.t_e[0];
    let l_end = 2 * grid.t_e[grid.num_env];
    let nq = ft.n_q();

    // Chirp factors per noise band.
    // (Entries beyond the current number of noise bands keep their old
    // values, which matters when the band count changes.)
    let mut bw = [0f32; 5];
    for i in 0..nq {
        bw[i] = chirp(ch.frame.invf[i], ch.invf_prev[i], ch.bw_prev[i]);
        ch.bw_prev[i] = bw[i];
        ch.invf_prev[i] = ch.frame.invf[i];
    }

    // HF generation (X_high indexed by HF slot l; X_low slot = l + 2).
    let mut xh = vec![[Complex::default(); 64]; SLOTS + 8];
    let mut target = kx;
    let mut alphas: [Option<((f32, f32), (f32, f32))>; 32] = [None; 32];
    for &(start, num) in &ft.patches {
        for xb in 0..num {
            let k = target + xb;
            let p = start + xb;
            if k >= 64 || p >= 32 {
                continue;
            }
            let g = (0..nq).rev().find(|&g| ft.f_noise[g] <= k).unwrap_or(0);
            let b = bw[g];
            if b == 0.0 {
                for l in l_start..l_end {
                    xh[l][k] = ch.x_low[l + T_HFADJ][p];
                }
                continue;
            }
            let (a0, a1) = *alphas[p].get_or_insert_with(|| {
                let (a0, a1) = lpc(&ch.x_low, p);
                ((a0.0 as f32, a0.1 as f32), (a1.0 as f32, a1.1 as f32))
            });
            let c0 = Complex::new(a0.0 * b, a0.1 * b);
            let c1 = Complex::new(a1.0 * b * b, a1.1 * b * b);
            for l in l_start..l_end {
                let x0 = ch.x_low[l + T_HFADJ][p];
                let x1 = ch.x_low[l + T_HFADJ - 1][p];
                let x2 = ch.x_low[l + T_HFADJ - 2][p];
                let t1 = c0.mul(x1);
                let t2 = c1.mul(x2);
                xh[l][k] = Complex::new(x0.re + t1.re + t2.re, x0.im + t1.im + t2.im);
            }
        }
        target += num;
    }

    // Sinusoid mapping.
    let l_a = grid.l_a;
    let l_a_prev: i32 = if ch.prev_l_a_last { 0 } else { -1 };
    let mut s_index = [[false; MAX_BANDS]; MAX_ENV];
    if ch.frame.add_harmonic_flag {
        for e in 0..grid.num_env {
            for i in 0..ft.n_high() {
                if ch.frame.add_harmonic[i] {
                    let mid = (ft.f_high[i] + ft.f_high[i + 1]) / 2 - kx;
                    if e as i32 >= l_a || ch.s_index_prev[mid] {
                        s_index[e][mid] = true;
                    }
                }
            }
        }
    }

    // Gains per envelope.
    let mut g_env = [[0f32; MAX_BANDS]; MAX_ENV];
    let mut q_env = [[0f32; MAX_BANDS]; MAX_ENV];
    let mut s_env = [[0f32; MAX_BANDS]; MAX_ENV];
    for e in 0..grid.num_env {
        let table = ft.table(grid.freq_res[e]);
        let l0 = 2 * grid.t_e[e];
        let l1 = 2 * grid.t_e[e + 1];
        let q = if grid.num_noise == 2 && grid.t_e[e] >= grid.t_q[1] { 1 } else { 0 };
        let delta = !(e as i32 == l_a || e as i32 == l_a_prev);
        let mut e_orig = [0f32; MAX_BANDS];
        let mut q_map = [0f32; MAX_BANDS];
        let mut s_mapped = [false; MAX_BANDS];
        let mut e_curr = [0f32; MAX_BANDS];
        for b in 0..table.len() - 1 {
            let (lo, hi) = (table[b] - kx, table[b + 1] - kx);
            let has_sine = s_index[e][lo..hi].iter().any(|&s| s);
            for k in lo..hi {
                e_orig[k] = ch.e_orig[e][b];
                s_mapped[k] = has_sine;
            }
        }
        for b in 0..nq {
            for k in ft.f_noise[b] - kx..ft.f_noise[b + 1] - kx {
                q_map[k] = ch.q_orig[q][b];
            }
        }
        let inv_len = 1.0 / (l1 - l0) as f32;
        if h.interpol_freq {
            for k in 0..m {
                let mut acc = 0.0f32;
                for l in l0..l1 {
                    let v = xh[l][kx + k];
                    acc += v.re * v.re + v.im * v.im;
                }
                e_curr[k] = acc * inv_len;
            }
        } else {
            for b in 0..table.len() - 1 {
                let (lo, hi) = (table[b] - kx, table[b + 1] - kx);
                let mut acc = 0.0f32;
                for k in lo..hi {
                    for l in l0..l1 {
                        let v = xh[l][kx + k];
                        acc += v.re * v.re + v.im * v.im;
                    }
                }
                let avg = acc * inv_len / (hi - lo) as f32;
                e_curr[lo..hi].iter_mut().for_each(|v| *v = avg);
            }
        }
        let mut g = [0f32; MAX_BANDS];
        let mut qm = [0f32; MAX_BANDS];
        let mut sm = [0f32; MAX_BANDS];
        for k in 0..m {
            let qv = q_map[k];
            qm[k] = (e_orig[k] * qv / (1.0 + qv)).sqrt();
            if s_index[e][k] {
                sm[k] = (e_orig[k] / (1.0 + qv)).sqrt();
            }
            g[k] = if !s_mapped[k] {
                let d = if delta { qv } else { 0.0 };
                (e_orig[k] / ((1.0 + e_curr[k]) * (1.0 + d))).sqrt()
            } else {
                (e_orig[k] * qv / ((1.0 + e_curr[k]) * (1.0 + qv))).sqrt()
            };
        }
        // Limiter and boost.
        for lb in 0..ft.f_lim.len() - 1 {
            let lo = ft.f_lim[lb] - kx;
            let hi = (ft.f_lim[lb + 1] - kx).min(m);
            if lo >= hi {
                continue;
            }
            let sum_orig: f32 = e_orig[lo..hi].iter().sum();
            let sum_curr: f32 = e_curr[lo..hi].iter().sum();
            let g_max = (LIM_GAINS[h.limiter_gains as usize & 3] * ((EPS0 + sum_orig) / (EPS0 + sum_curr)).sqrt()).min(1e5);
            let mut denom = EPS0;
            for k in lo..hi {
                if g[k] > g_max {
                    qm[k] *= g_max / g[k];
                    g[k] = g_max;
                }
                denom += e_curr[k] * g[k] * g[k] + sm[k] * sm[k];
                if delta && sm[k] == 0.0 {
                    denom += qm[k] * qm[k];
                }
            }
            let boost = ((EPS0 + sum_orig) / denom).sqrt().min(1.584_893_2);
            for k in lo..hi {
                g[k] *= boost;
                qm[k] *= boost;
                sm[k] *= boost;
            }
        }
        g_env[e] = g;
        q_env[e] = qm;
        s_env[e] = sm;
    }

    // Assembly with gain smoothing, noise and sinusoids.
    let smoothing = !h.smoothing_mode;
    if reset || !ch.hist_valid {
        for i in 0..4 {
            ch.g_hist[i] = g_env[0];
            ch.q_hist[i] = q_env[0];
        }
        ch.hist_valid = true;
    }
    let mut noise_idx = ch.index_noise;
    let mut sine_idx = ch.index_sine;
    for e in 0..grid.num_env {
        let transient = e as i32 == l_a || e as i32 == l_a_prev;
        let (g, qm, sm) = (&g_env[e], &q_env[e], &s_env[e]);
        for l in 2 * grid.t_e[e]..2 * grid.t_e[e + 1] {
            let y = &mut ch.y[l];
            for k in 0..m {
                let (gf, qf) = if smoothing && !transient {
                    let mut gs = g[k] * H_SMOOTH[0];
                    let mut qs = qm[k] * H_SMOOTH[0];
                    for j in 0..4 {
                        gs += ch.g_hist[j][k] * H_SMOOTH[j + 1];
                        qs += ch.q_hist[j][k] * H_SMOOTH[j + 1];
                    }
                    (gs, qs)
                } else {
                    (g[k], qm[k])
                };
                let src = xh[l][kx + k];
                let mut re = src.re * gf;
                let mut im = src.im * gf;
                noise_idx = (noise_idx + 1) & 511;
                if sm[k] != 0.0 {
                    let s = sm[k];
                    match sine_idx {
                        0 => re += s,
                        2 => re -= s,
                        1 => im += if (k + kx) & 1 == 0 { s } else { -s },
                        _ => im += if (k + kx) & 1 == 0 { -s } else { s },
                    }
                } else if !transient {
                    let v = NOISE_TABLE[noise_idx];
                    re += qf * v[0];
                    im += qf * v[1];
                }
                y[kx + k] = Complex::new(re, im);
            }
            sine_idx = (sine_idx + 1) & 3;
            // Push the current (unsmoothed) gains into the history.
            ch.g_hist.copy_within(0..3, 1);
            ch.q_hist.copy_within(0..3, 1);
            ch.g_hist[0] = *g;
            ch.q_hist[0] = *qm;
        }
    }
    ch.index_noise = noise_idx;
    ch.index_sine = sine_idx;
    ch.s_index_prev = s_index[grid.num_env - 1];
    ch.prev_l_a_last = l_a == grid.num_env as i32;
}
