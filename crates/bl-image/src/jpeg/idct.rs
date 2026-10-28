//! Inverse DCTs for the JPEG decoder.
//!
//! * [`idct8`]: full-size separable float IDCT using the Arai-Agui-Nakajima
//!   (AAN) factorization (5 multiplications per 1-D transform) with the
//!   dequantization folded into prescaled quantization tables,
//! * [`idct_reduced`]: 4x4, 2x2 and 1x1 outputs computed directly from the
//!   low-frequency coefficients (the 8-point basis evaluated at the centres of
//!   2x2 / 4x4 / 8x8 pixel groups), used for DCT-domain downscaling.

use std::f64::consts::PI;

/// AAN scale factors: 1 for k = 0, cos(k*pi/16) * sqrt(2) otherwise.
fn aan_factor(k: usize) -> f64 {
    if k == 0 {
        1.0
    } else {
        (k as f64 * PI / 16.0).cos() * std::f64::consts::SQRT_2
    }
}

/// Dequantization tables of one component in the forms the IDCTs need.
#[derive(Clone)]
pub struct Dequant {
    /// q * aan(row) * aan(col) / 8, natural order.
    pub aan: [f32; 64],
    /// Plain q, natural order.
    pub plain: [f32; 64],
}

impl Dequant {
    /// `q` in natural order.
    pub fn new(q: &[u16; 64]) -> Dequant {
        let mut aan = [0f32; 64];
        let mut plain = [0f32; 64];
        for r in 0..8 {
            for c in 0..8 {
                let i = r * 8 + c;
                aan[i] = (q[i] as f64 * aan_factor(r) * aan_factor(c) / 8.0) as f32;
                plain[i] = q[i] as f32;
            }
        }
        Dequant { aan, plain }
    }
}

#[inline(always)]
fn to_u8(v: f32) -> u8 {
    // `as` saturates (NaN -> 0); +128.5 then truncation rounds to nearest for
    // the non-negative range that matters.
    (v + 128.5).clamp(0.0, 255.0) as u8
}

/// Full 8x8 IDCT of `coef` (quantized, natural order) into `out` at `off`
/// with row stride `stride`.
pub fn idct8(coef: &[i16; 64], dq: &Dequant, out: &mut [u8], off: usize, stride: usize) {
    let q = &dq.aan;
    if coef[1..].iter().all(|&c| c == 0) {
        let v = to_u8(coef[0] as f32 * q[0]);
        for y in 0..8 {
            out[off + y * stride..off + y * stride + 8].fill(v);
        }
        return;
    }
    let mut ws = [0f32; 64];
    // Columns.
    for c in 0..8 {
        let d = |r: usize| coef[r * 8 + c] as f32 * q[r * 8 + c];
        if (1..8).all(|r| coef[r * 8 + c] == 0) {
            let dc = d(0);
            for r in 0..8 {
                ws[r * 8 + c] = dc;
            }
            continue;
        }
        let o = aan_1d([d(0), d(1), d(2), d(3), d(4), d(5), d(6), d(7)]);
        for r in 0..8 {
            ws[r * 8 + c] = o[r];
        }
    }
    // Rows.
    for r in 0..8 {
        let row = &ws[r * 8..r * 8 + 8];
        let o = aan_1d([
            row[0], row[1], row[2], row[3], row[4], row[5], row[6], row[7],
        ]);
        let dst = &mut out[off + r * stride..off + r * stride + 8];
        for x in 0..8 {
            dst[x] = to_u8(o[x]);
        }
    }
}

#[inline(always)]
fn aan_1d(i: [f32; 8]) -> [f32; 8] {
    // Even part.
    let tmp10 = i[0] + i[4];
    let tmp11 = i[0] - i[4];
    let tmp13 = i[2] + i[6];
    let tmp12 = (i[2] - i[6]) * std::f32::consts::SQRT_2 - tmp13;
    let e0 = tmp10 + tmp13;
    let e3 = tmp10 - tmp13;
    let e1 = tmp11 + tmp12;
    let e2 = tmp11 - tmp12;
    // Odd part.
    let z13 = i[5] + i[3];
    let z10 = i[5] - i[3];
    let z11 = i[1] + i[7];
    let z12 = i[1] - i[7];
    let o7 = z11 + z13;
    let t11 = (z11 - z13) * std::f32::consts::SQRT_2;
    let z5 = (z10 + z12) * 1.847_759;
    let t10 = z5 - z12 * 1.082_392_2;
    let t12 = z5 - z10 * 2.613_126;
    let o6 = t12 - o7;
    let o5 = t11 - o6;
    let o4 = t10 - o5;
    [
        e0 + o7,
        e1 + o6,
        e2 + o5,
        e3 + o4,
        e3 - o4,
        e2 - o5,
        e1 - o6,
        e0 - o7,
    ]
}

/// Basis tables for the reduced IDCTs: `b[n][x][u]` = C(u)/2 *
/// cos((2x+1) u pi / (2n)) for n = 4 and 2.
pub struct ReducedBasis {
    b4: [[f32; 4]; 4],
    b2: [[f32; 2]; 2],
}

impl ReducedBasis {
    pub fn new() -> Self {
        let c = |u: usize| {
            if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        let mut b4 = [[0f32; 4]; 4];
        for (x, row) in b4.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                *v = (0.5 * c(u) * ((2 * x + 1) as f64 * u as f64 * PI / 8.0).cos()) as f32;
            }
        }
        let mut b2 = [[0f32; 2]; 2];
        for (x, row) in b2.iter_mut().enumerate() {
            for (u, v) in row.iter_mut().enumerate() {
                *v = (0.5 * c(u) * ((2 * x + 1) as f64 * u as f64 * PI / 4.0).cos()) as f32;
            }
        }
        ReducedBasis { b4, b2 }
    }
}

/// Reduced IDCT producing an `n`x`n` block (n = 4, 2 or 1).
pub fn idct_reduced(
    n: usize,
    coef: &[i16; 64],
    dq: &Dequant,
    basis: &ReducedBasis,
    out: &mut [u8],
    off: usize,
    stride: usize,
) {
    let q = &dq.plain;
    match n {
        1 => out[off] = to_u8(coef[0] as f32 * q[0] * 0.125),
        2 => {
            let f = |i: usize| coef[i] as f32 * q[i];
            let b = &basis.b2;
            for y in 0..2 {
                for x in 0..2 {
                    let mut s = 0.0;
                    for v in 0..2 {
                        for u in 0..2 {
                            s += b[y][v] * b[x][u] * f(v * 8 + u);
                        }
                    }
                    out[off + y * stride + x] = to_u8(s);
                }
            }
        }
        _ => {
            let b = &basis.b4;
            if coef[1..].iter().all(|&c| c == 0) {
                let v = to_u8(coef[0] as f32 * q[0] * 0.125);
                for y in 0..4 {
                    out[off + y * stride..off + y * stride + 4].fill(v);
                }
                return;
            }
            // Rows of frequencies -> horizontal positions.
            let mut t = [[0f32; 4]; 4];
            for v in 0..4 {
                let f = [
                    coef[v * 8] as f32 * q[v * 8],
                    coef[v * 8 + 1] as f32 * q[v * 8 + 1],
                    coef[v * 8 + 2] as f32 * q[v * 8 + 2],
                    coef[v * 8 + 3] as f32 * q[v * 8 + 3],
                ];
                for x in 0..4 {
                    t[v][x] = b[x][0] * f[0] + b[x][1] * f[1] + b[x][2] * f[2] + b[x][3] * f[3];
                }
            }
            for y in 0..4 {
                for x in 0..4 {
                    let s = b[y][0] * t[0][x]
                        + b[y][1] * t[1][x]
                        + b[y][2] * t[2][x]
                        + b[y][3] * t[3][x];
                    out[off + y * stride + x] = to_u8(s);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reference(coef: &[i16; 64], q: &[u16; 64]) -> [f64; 64] {
        let mut out = [0f64; 64];
        let c = |u: usize| {
            if u == 0 {
                std::f64::consts::FRAC_1_SQRT_2
            } else {
                1.0
            }
        };
        for y in 0..8 {
            for x in 0..8 {
                let mut s = 0.0;
                for v in 0..8 {
                    for u in 0..8 {
                        let f = coef[v * 8 + u] as f64 * q[v * 8 + u] as f64;
                        s += c(u)
                            * c(v)
                            * f
                            * (((2 * x + 1) * u) as f64 * PI / 16.0).cos()
                            * (((2 * y + 1) * v) as f64 * PI / 16.0).cos();
                    }
                }
                out[y * 8 + x] = s / 4.0 + 128.0;
            }
        }
        out
    }

    #[test]
    fn matches_reference_idct() {
        let mut seed = 12345u32;
        let mut rnd = || {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            (seed >> 16) as i32
        };
        let mut q = [0u16; 64];
        for (i, v) in q.iter_mut().enumerate() {
            *v = 1 + (i as u16 % 7);
        }
        let dq = Dequant::new(&q);
        for _ in 0..200 {
            let mut coef = [0i16; 64];
            for (i, c) in coef.iter_mut().enumerate() {
                let range = if i == 0 { 120 } else { 30 / (1 + i as i32 / 8) };
                *c = (rnd() % (2 * range + 1) - range) as i16;
            }
            let r = reference(&coef, &q);
            let mut out = [0u8; 64];
            idct8(&coef, &dq, &mut out, 0, 8);
            for i in 0..64 {
                let expect = r[i].round().clamp(0.0, 255.0);
                assert!(
                    (out[i] as f64 - expect).abs() <= 1.0,
                    "idct mismatch {} vs {}",
                    out[i],
                    expect
                );
            }
            // Reduced versions approximate block averages.
            let basis = ReducedBasis::new();
            let mut o4 = [0u8; 16];
            idct_reduced(4, &coef, &dq, &basis, &mut o4, 0, 4);
            let mut o1 = [0u8; 1];
            idct_reduced(1, &coef, &dq, &basis, &mut o1, 0, 1);
            let mean: f64 = r.iter().sum::<f64>() / 64.0;
            assert!((o1[0] as f64 - mean.clamp(0.0, 255.0)).abs() <= 1.0);
            let _ = o4;
        }
    }
}
