//! Mixed-radix FFT (radix 2, 3, 4, 5) and inverse MDCT for the CELT mode.
//!
//! Ported from libopus `celt/kiss_fft.c` and `celt/mdct.c`:
//!
//! Copyright (c) 2003-2004, Mark Borgerding
//! Lots of modifications by Jean-Marc Valin
//! Copyright (c) 2005-2007, Xiph.Org Foundation
//! Copyright (c) 2008, Xiph.Org Foundation, CSIRO
//! Copyright (c) 2007-2008 CSIRO, Copyright (c) 2007-2008 Xiph.Org Foundation
//! (mdct.c, written by Jean-Marc Valin)
//!
//! All rights reserved.
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the following conditions are met:
//! - Redistributions of source code must retain the above copyright notice,
//!   this list of conditions and the following disclaimer.
//! - Redistributions in binary form must reproduce the above copyright notice,
//!   this list of conditions and the following disclaimer in the documentation
//!   and/or other materials provided with the distribution.
//!
//! THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS"
//! AND ANY EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED (see NOTICE).

use super::static_modes::{
    FFT_BITREV120, FFT_BITREV240, FFT_BITREV480, FFT_BITREV60, FFT_TWIDDLES48000_960,
    MDCT_TWIDDLES960,
};

#[derive(Clone, Copy, Default)]
pub struct Cpx {
    pub r: f32,
    pub i: f32,
}

struct FftState {
    nfft: usize,
    shift: usize,
    factors: [usize; 16],
    bitrev: &'static [u16],
}

static FFT_STATES: [FftState; 4] = [
    FftState {
        nfft: 480,
        shift: 0,
        factors: [5, 96, 3, 32, 4, 8, 2, 4, 4, 1, 0, 0, 0, 0, 0, 0],
        bitrev: &FFT_BITREV480,
    },
    FftState {
        nfft: 240,
        shift: 1,
        factors: [5, 48, 3, 16, 4, 4, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        bitrev: &FFT_BITREV240,
    },
    FftState {
        nfft: 120,
        shift: 2,
        factors: [5, 24, 3, 8, 2, 4, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0],
        bitrev: &FFT_BITREV120,
    },
    FftState {
        nfft: 60,
        shift: 3,
        factors: [5, 12, 3, 4, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        bitrev: &FFT_BITREV60,
    },
];

#[inline(always)]
fn tw(idx: usize) -> Cpx {
    let (r, i) = FFT_TWIDDLES48000_960[idx];
    Cpx { r, i }
}

#[inline(always)]
fn cmul(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r * b.r - a.i * b.i,
        i: a.r * b.i + a.i * b.r,
    }
}

#[inline(always)]
fn cadd(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r + b.r,
        i: a.i + b.i,
    }
}

#[inline(always)]
fn csub(a: Cpx, b: Cpx) -> Cpx {
    Cpx {
        r: a.r - b.r,
        i: a.i - b.i,
    }
}

fn kf_bfly2(fout: &mut [Cpx], n: usize) {
    let tw = 0.707_106_781_2_f32;
    for blk in 0..n {
        let f = &mut fout[blk * 8..blk * 8 + 8];
        let t = f[4];
        f[4] = csub(f[0], t);
        f[0] = cadd(f[0], t);

        let t = Cpx {
            r: (f[5].r + f[5].i) * tw,
            i: (f[5].i - f[5].r) * tw,
        };
        f[5] = csub(f[1], t);
        f[1] = cadd(f[1], t);

        let t = Cpx {
            r: f[6].i,
            i: -f[6].r,
        };
        f[6] = csub(f[2], t);
        f[2] = cadd(f[2], t);

        let t = Cpx {
            r: (f[7].i - f[7].r) * tw,
            i: -(f[7].i + f[7].r) * tw,
        };
        f[7] = csub(f[3], t);
        f[3] = cadd(f[3], t);
    }
}

fn kf_bfly4(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    if m == 1 {
        for f in fout[..4 * n].chunks_exact_mut(4) {
            let scratch0 = csub(f[0], f[2]);
            f[0] = cadd(f[0], f[2]);
            let mut scratch1 = cadd(f[1], f[3]);
            f[2] = csub(f[0], scratch1);
            f[0] = cadd(f[0], scratch1);
            scratch1 = csub(f[1], f[3]);
            f[1] = Cpx {
                r: scratch0.r + scratch1.i,
                i: scratch0.i - scratch1.r,
            };
            f[3] = Cpx {
                r: scratch0.r - scratch1.i,
                i: scratch0.i + scratch1.r,
            };
        }
    } else {
        let tw = &FFT_TWIDDLES48000_960[..];
        for i in 0..n {
            let base = i * mm;
            let f = &mut fout[base..base + 4 * m];
            let (f0, rest) = f.split_at_mut(m);
            let (f1, rest) = rest.split_at_mut(m);
            let (f2, f3) = rest.split_at_mut(m);
            let t1 = tw.iter().step_by(fstride);
            let t2 = tw.iter().step_by(2 * fstride);
            let t3 = tw.iter().step_by(3 * fstride);
            for (((((a, b), c), d), w1), (w2, w3)) in f0
                .iter_mut()
                .zip(f1.iter_mut())
                .zip(f2.iter_mut())
                .zip(f3.iter_mut())
                .zip(t1)
                .zip(t2.zip(t3))
            {
                let s0 = cmul(*b, Cpx { r: w1.0, i: w1.1 });
                let s1 = cmul(*c, Cpx { r: w2.0, i: w2.1 });
                let s2 = cmul(*d, Cpx { r: w3.0, i: w3.1 });
                let s5 = csub(*a, s1);
                *a = cadd(*a, s1);
                let s3 = cadd(s0, s2);
                let s4 = csub(s0, s2);
                *c = csub(*a, s3);
                *a = cadd(*a, s3);
                *b = Cpx {
                    r: s5.r + s4.i,
                    i: s5.i - s4.r,
                };
                *d = Cpx {
                    r: s5.r - s4.i,
                    i: s5.i + s4.r,
                };
            }
        }
    }
}

fn kf_bfly3(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    let epi3 = tw(fstride * m);
    let tw = &FFT_TWIDDLES48000_960[..];
    for i in 0..n {
        let base = i * mm;
        let f = &mut fout[base..base + 3 * m];
        let (f0, rest) = f.split_at_mut(m);
        let (f1, f2) = rest.split_at_mut(m);
        let t1 = tw.iter().step_by(fstride);
        let t2 = tw.iter().step_by(2 * fstride);
        for ((((a, b), c), w1), w2) in f0
            .iter_mut()
            .zip(f1.iter_mut())
            .zip(f2.iter_mut())
            .zip(t1)
            .zip(t2)
        {
            let s1 = cmul(*b, Cpx { r: w1.0, i: w1.1 });
            let s2 = cmul(*c, Cpx { r: w2.0, i: w2.1 });
            let s3 = cadd(s1, s2);
            let mut s0 = csub(s1, s2);
            let mut fm = Cpx {
                r: a.r - s3.r * 0.5,
                i: a.i - s3.i * 0.5,
            };
            s0.r *= epi3.i;
            s0.i *= epi3.i;
            *a = cadd(*a, s3);
            *c = Cpx {
                r: fm.r + s0.i,
                i: fm.i - s0.r,
            };
            fm = Cpx {
                r: fm.r - s0.i,
                i: fm.i + s0.r,
            };
            *b = fm;
        }
    }
}

fn kf_bfly5(fout: &mut [Cpx], fstride: usize, m: usize, n: usize, mm: usize) {
    let ya = tw(fstride * m);
    let yb = tw(fstride * 2 * m);
    let tw = &FFT_TWIDDLES48000_960[..];
    for i in 0..n {
        let base = i * mm;
        let f = &mut fout[base..base + 5 * m];
        let (f0, rest) = f.split_at_mut(m);
        let (f1, rest) = rest.split_at_mut(m);
        let (f2, rest) = rest.split_at_mut(m);
        let (f3, f4) = rest.split_at_mut(m);
        let t1 = tw.iter().step_by(fstride);
        let t2 = tw.iter().step_by(2 * fstride);
        let t3 = tw.iter().step_by(3 * fstride);
        let t4 = tw.iter().step_by(4 * fstride);
        for (((((a, b), c), d), e), ((w1, w2), (w3, w4))) in f0
            .iter_mut()
            .zip(f1.iter_mut())
            .zip(f2.iter_mut())
            .zip(f3.iter_mut())
            .zip(f4.iter_mut())
            .zip(t1.zip(t2).zip(t3.zip(t4)))
        {
            let s0 = *a;
            let s1 = cmul(*b, Cpx { r: w1.0, i: w1.1 });
            let s2 = cmul(*c, Cpx { r: w2.0, i: w2.1 });
            let s3 = cmul(*d, Cpx { r: w3.0, i: w3.1 });
            let s4 = cmul(*e, Cpx { r: w4.0, i: w4.1 });
            let s7 = cadd(s1, s4);
            let s10 = csub(s1, s4);
            let s8 = cadd(s2, s3);
            let s9 = csub(s2, s3);
            *a = Cpx {
                r: s0.r + (s7.r + s8.r),
                i: s0.i + (s7.i + s8.i),
            };
            let s5 = Cpx {
                r: s0.r + (s7.r * ya.r + s8.r * yb.r),
                i: s0.i + (s7.i * ya.r + s8.i * yb.r),
            };
            let s6 = Cpx {
                r: s10.i * ya.i + s9.i * yb.i,
                i: -(s10.r * ya.i + s9.r * yb.i),
            };
            *b = csub(s5, s6);
            *e = cadd(s5, s6);
            let s11 = Cpx {
                r: s0.r + (s7.r * yb.r + s8.r * ya.r),
                i: s0.i + (s7.i * yb.r + s8.i * ya.r),
            };
            let s12 = Cpx {
                r: s9.i * ya.i - s10.i * yb.i,
                i: s10.r * yb.i - s9.r * ya.i,
            };
            *c = cadd(s11, s12);
            *d = csub(s11, s12);
        }
    }
}

/// `opus_fft_impl`: in-place FFT on already bit-reversed data.
fn fft_impl(st: &FftState, fout: &mut [Cpx]) {
    let shift = st.shift;
    let mut fstride = [0usize; 8];
    fstride[0] = 1;
    let mut l = 0usize;
    loop {
        let p = st.factors[2 * l];
        let m = st.factors[2 * l + 1];
        fstride[l + 1] = fstride[l] * p;
        l += 1;
        if m == 1 {
            break;
        }
    }
    let mut m = st.factors[2 * l - 1];
    for i in (0..l).rev() {
        let m2 = if i != 0 { st.factors[2 * i - 1] } else { 1 };
        match st.factors[2 * i] {
            2 => kf_bfly2(fout, fstride[i]),
            4 => kf_bfly4(fout, fstride[i] << shift, m, fstride[i], m2),
            3 => kf_bfly3(fout, fstride[i] << shift, m, fstride[i], m2),
            5 => kf_bfly5(fout, fstride[i] << shift, m, fstride[i], m2),
            _ => {}
        }
        m = m2;
    }
}

/// Inverse MDCT (`clt_mdct_backward`) with TDAC windowing.
///
/// `input` holds the frequency-domain coefficients read with stride
/// `stride`; `out` receives `N/2 + overlap` samples (the first `overlap`
/// samples are overlap-added with what's already there in the C code's
/// in-place layout; here `out` is written exactly as libopus does).
/// `scratch` must hold at least N/4 complex values.
pub fn mdct_backward(
    input: &[f32],
    out: &mut [f32],
    window: &[f32],
    overlap: usize,
    shift: usize,
    stride: usize,
    scratch: &mut Vec<Cpx>,
) {
    let mut n = 1920usize;
    let mut trig_off = 0usize;
    for _ in 0..shift {
        n >>= 1;
        trig_off += n;
    }
    let trig = &MDCT_TWIDDLES960[trig_off..];
    let n2 = n >> 1;
    let n4 = n >> 2;
    let st = &FFT_STATES[shift];
    debug_assert_eq!(st.nfft, n4);

    if scratch.len() < n4 {
        scratch.resize(n4, Cpx::default());
    }
    let scratch = &mut scratch[..n4];
    // Pre-rotate, storing directly in bit-reversed order.
    {
        let bitrev = st.bitrev;
        let mut xp1 = 0usize;
        let mut xp2 = stride * (n2 - 1);
        for i in 0..n4 {
            let rev = bitrev[i] as usize;
            let x1 = input[xp1];
            let x2 = input[xp2];
            let yr = x2 * trig[i] + x1 * trig[n4 + i];
            let yi = x1 * trig[i] - x2 * trig[n4 + i];
            // Swap real and imag because we use an FFT instead of an IFFT.
            scratch[rev] = Cpx { r: yi, i: yr };
            xp1 += 2 * stride;
            xp2 = xp2.wrapping_sub(2 * stride);
        }
    }

    fft_impl(st, scratch);

    // Post-rotate and de-shuffle from both ends.
    {
        let half = overlap >> 1;
        let yp = &mut out[half..half + n2];
        let mut i0 = 0usize; // complex index from the start
        let mut i1 = n4 - 1; // complex index from the end
        for i in 0..(n4 + 1) >> 1 {
            let re = scratch[i0].i;
            let im = scratch[i0].r;
            let t0 = trig[i];
            let t1 = trig[n4 + i];
            let yr = re * t0 + im * t1;
            let yi = re * t1 - im * t0;
            let re1 = scratch[i1].i;
            let im1 = scratch[i1].r;
            yp[2 * i0] = yr;
            yp[2 * i1 + 1] = yi;
            let t0 = trig[n4 - i - 1];
            let t1 = trig[n2 - i - 1];
            let yr = re1 * t0 + im1 * t1;
            let yi = re1 * t1 - im1 * t0;
            yp[2 * i1] = yr;
            yp[2 * i0 + 1] = yi;
            i0 += 1;
            i1 = i1.wrapping_sub(1);
        }
    }

    // Mirror on both sides for TDAC.
    {
        let mut xp1 = overlap - 1;
        let mut yp1 = 0usize;
        for i in 0..overlap / 2 {
            let wp1 = window[i];
            let wp2 = window[overlap - 1 - i];
            let x1 = out[xp1];
            let x2 = out[yp1];
            out[yp1] = wp2 * x2 - wp1 * x1;
            out[xp1] = wp1 * x2 + wp2 * x1;
            yp1 += 1;
            xp1 -= 1;
        }
    }
}
