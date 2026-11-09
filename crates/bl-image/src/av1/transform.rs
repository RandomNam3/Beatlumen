//! Inverse transforms (AV1 specification section 7.13): DCT 4..64, ADST
//! 4/8/16, identity 4..32 and the Walsh-Hadamard transform of lossless
//! blocks, as butterfly networks exactly as specified.

use super::tables::{COS128_LOOKUP, TRANSFORM_ROW_SHIFT, TX_HEIGHT_LOG2, TX_WIDTH_LOG2};

// Transform types.
pub(crate) const DCT_DCT: u8 = 0;
pub(crate) const ADST_DCT: u8 = 1;
pub(crate) const DCT_ADST: u8 = 2;
pub(crate) const ADST_ADST: u8 = 3;
pub(crate) const FLIPADST_DCT: u8 = 4;
pub(crate) const DCT_FLIPADST: u8 = 5;
pub(crate) const FLIPADST_FLIPADST: u8 = 6;
pub(crate) const ADST_FLIPADST: u8 = 7;
pub(crate) const FLIPADST_ADST: u8 = 8;
pub(crate) const IDTX: u8 = 9;
pub(crate) const V_DCT: u8 = 10;
pub(crate) const H_DCT: u8 = 11;
pub(crate) const V_ADST: u8 = 12;
pub(crate) const H_ADST: u8 = 13;
pub(crate) const V_FLIPADST: u8 = 14;
pub(crate) const H_FLIPADST: u8 = 15;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Dct,
    Adst,
    Identity,
}

/// (row kind, column kind) of a transform type.
fn kinds(tx_type: u8) -> (Kind, Kind) {
    use Kind::*;
    match tx_type {
        DCT_DCT => (Dct, Dct),
        ADST_DCT => (Dct, Adst),
        DCT_ADST => (Adst, Dct),
        ADST_ADST => (Adst, Adst),
        FLIPADST_DCT => (Dct, Adst),
        DCT_FLIPADST => (Adst, Dct),
        FLIPADST_FLIPADST => (Adst, Adst),
        ADST_FLIPADST => (Adst, Adst),
        FLIPADST_ADST => (Adst, Adst),
        IDTX => (Identity, Identity),
        V_DCT => (Identity, Dct),
        H_DCT => (Dct, Identity),
        V_ADST => (Identity, Adst),
        H_ADST => (Adst, Identity),
        V_FLIPADST => (Identity, Adst),
        _ => (Adst, Identity), // H_FLIPADST
    }
}

pub(crate) fn flips(tx_type: u8) -> (bool, bool) {
    let ud = matches!(
        tx_type,
        FLIPADST_DCT | FLIPADST_ADST | V_FLIPADST | FLIPADST_FLIPADST
    );
    let lr = matches!(
        tx_type,
        DCT_FLIPADST | ADST_FLIPADST | H_FLIPADST | FLIPADST_FLIPADST
    );
    (ud, lr)
}

#[inline]
fn round2(x: i64, n: u32) -> i64 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

#[inline]
fn cos128(angle: i32) -> i64 {
    let a = angle & 255;
    (match a {
        0..=64 => COS128_LOOKUP[a as usize] as i32,
        65..=128 => -(COS128_LOOKUP[(128 - a) as usize] as i32),
        129..=192 => -(COS128_LOOKUP[(a - 128) as usize] as i32),
        _ => COS128_LOOKUP[(256 - a) as usize] as i32,
    }) as i64
}

#[inline]
fn sin128(angle: i32) -> i64 {
    cos128(angle - 64)
}

#[inline]
fn brev(bits: u32, x: usize) -> usize {
    (x.reverse_bits() >> (usize::BITS - bits)) & ((1 << bits) - 1)
}

/// B( a, b, angle, flip )
#[inline]
fn bf(t: &mut [i32], a: usize, b: usize, angle: i32, flip: bool) {
    let (ta, tb) = (t[a] as i64, t[b] as i64);
    let x = ta * cos128(angle) - tb * sin128(angle);
    let y = ta * sin128(angle) + tb * cos128(angle);
    let (x, y) = (round2(x, 12) as i32, round2(y, 12) as i32);
    if flip {
        t[a] = y;
        t[b] = x;
    } else {
        t[a] = x;
        t[b] = y;
    }
}

/// H( a, b, flip, r )
#[inline]
fn hd(t: &mut [i32], a: usize, b: usize, flip: bool, r: u32) {
    let (a, b) = if flip { (b, a) } else { (a, b) };
    let (x, y) = (t[a] as i64, t[b] as i64);
    let lo = -(1i64 << (r - 1));
    let hi = (1i64 << (r - 1)) - 1;
    t[a] = (x + y).clamp(lo, hi) as i32;
    t[b] = (x - y).clamp(lo, hi) as i32;
}

fn idct(t: &mut [i32], n: u32, r: u32) {
    let len = 1usize << n;
    let mut copy = [0i32; 64];
    copy[..len].copy_from_slice(&t[..len]);
    for i in 0..len {
        t[i] = copy[brev(n, i)];
    }
    if n == 6 {
        for i in 0..16 {
            bf(t, 32 + i, 63 - i, 63 - 4 * brev(4, i) as i32, false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            bf(t, 16 + i, 31 - i, 6 + ((brev(3, 7 - i) as i32) << 3), false);
        }
    }
    if n == 6 {
        for i in 0..16 {
            hd(t, 32 + i * 2, 33 + i * 2, i & 1 == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            bf(t, 8 + i, 15 - i, 12 + ((brev(2, 3 - i) as i32) << 4), false);
        }
    }
    if n >= 5 {
        for i in 0..8 {
            hd(t, 16 + 2 * i, 17 + 2 * i, i & 1 == 1, r);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..2 {
                bf(
                    t,
                    62 - i * 4 - j,
                    33 + i * 4 + j,
                    60 - 16 * brev(2, i) as i32 + 64 * j as i32,
                    true,
                );
            }
        }
    }
    if n >= 3 {
        for i in 0..2 {
            bf(t, 4 + i, 7 - i, 56 - 32 * i as i32, false);
        }
    }
    if n >= 4 {
        for i in 0..4 {
            hd(t, 8 + 2 * i, 9 + 2 * i, i & 1 == 1, r);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..2 {
                bf(
                    t,
                    30 - 4 * i - j,
                    17 + 4 * i + j,
                    24 + ((j as i32) << 6) + ((1 - i as i32) << 5),
                    true,
                );
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            for j in 0..2 {
                hd(t, 32 + i * 4 + j, 35 + i * 4 - j, i & 1 == 1, r);
            }
        }
    }
    for i in 0..2 {
        bf(t, 2 * i, 2 * i + 1, 32 + 16 * i as i32, i == 0);
    }
    if n >= 3 {
        for i in 0..2 {
            hd(t, 4 + 2 * i, 5 + 2 * i, i == 1, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(t, 14 - i, 9 + i, 48 + 64 * i as i32, true);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            for j in 0..2 {
                hd(t, 16 + 4 * i + j, 19 + 4 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..2 {
            for j in 0..4 {
                bf(
                    t,
                    61 - i * 8 - j,
                    34 + i * 8 + j,
                    56 - i as i32 * 32 + (j as i32 >> 1) * 64,
                    true,
                );
            }
        }
    }
    for i in 0..2 {
        hd(t, i, 3 - i, false, r);
    }
    if n >= 3 {
        bf(t, 6, 5, 32, true);
    }
    if n >= 4 {
        for i in 0..2 {
            for j in 0..2 {
                hd(t, 8 + 4 * i + j, 11 + 4 * i - j, i == 1, r);
            }
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(t, 29 - i, 18 + i, 48 + (i as i32 >> 1) * 64, true);
        }
    }
    if n == 6 {
        for i in 0..4 {
            for j in 0..4 {
                hd(t, 32 + 8 * i + j, 39 + 8 * i - j, i & 1 == 1, r);
            }
        }
    }
    if n >= 3 {
        for i in 0..4 {
            hd(t, i, 7 - i, false, r);
        }
    }
    if n >= 4 {
        for i in 0..2 {
            bf(t, 13 - i, 10 + i, 32, true);
        }
    }
    if n >= 5 {
        for i in 0..2 {
            for j in 0..4 {
                hd(t, 16 + i * 8 + j, 23 + i * 8 - j, i == 1, r);
            }
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(t, 59 - i, 36 + i, if i < 4 { 48 } else { 112 }, true);
        }
    }
    if n >= 4 {
        for i in 0..8 {
            hd(t, i, 15 - i, false, r);
        }
    }
    if n >= 5 {
        for i in 0..4 {
            bf(t, 27 - i, 20 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..8 {
            hd(t, 32 + i, 47 - i, false, r);
            hd(t, 48 + i, 63 - i, true, r);
        }
    }
    if n >= 5 {
        for i in 0..16 {
            hd(t, i, 31 - i, false, r);
        }
    }
    if n == 6 {
        for i in 0..8 {
            bf(t, 55 - i, 40 + i, 32, true);
        }
    }
    if n == 6 {
        for i in 0..32 {
            hd(t, i, 63 - i, false, r);
        }
    }
}

fn adst_in_perm(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let idx = if i & 1 == 1 { i - 1 } else { n0 - i - 1 };
        t[i] = copy[idx];
    }
}

fn adst_out_perm(t: &mut [i32], n: u32) {
    let n0 = 1usize << n;
    let mut copy = [0i32; 16];
    copy[..n0].copy_from_slice(&t[..n0]);
    for i in 0..n0 {
        let a = (i >> 3) & 1;
        let b = ((i >> 2) & 1) ^ ((i >> 3) & 1);
        let c = ((i >> 1) & 1) ^ ((i >> 2) & 1);
        let d = (i & 1) ^ ((i >> 1) & 1);
        let idx = ((d << 3) | (c << 2) | (b << 1) | a) >> (4 - n);
        t[i] = if i & 1 == 1 { -copy[idx] } else { copy[idx] };
    }
}

fn adst4(t: &mut [i32]) {
    const S1: i64 = 1321;
    const S2: i64 = 2482;
    const S3: i64 = 3344;
    const S4: i64 = 3803;
    let (t0, t1, t2, t3) = (t[0] as i64, t[1] as i64, t[2] as i64, t[3] as i64);
    let mut s0 = S1 * t0;
    let mut s1 = S2 * t0;
    let s2 = S3 * t1;
    let s3 = S4 * t2;
    let s4 = S1 * t2;
    let s5 = S2 * t3;
    let s6 = S4 * t3;
    let a7 = t0 - t2;
    let b7 = a7 + t3;
    s0 += s3;
    s1 -= s4;
    let s3 = s2;
    let s2 = S3 * b7;
    s0 += s5;
    s1 -= s6;
    let x0 = s0 + s3;
    let x1 = s1 + s3;
    let x2 = s2;
    let x3 = s0 + s1 - s3;
    t[0] = round2(x0, 12) as i32;
    t[1] = round2(x1, 12) as i32;
    t[2] = round2(x2, 12) as i32;
    t[3] = round2(x3, 12) as i32;
}

fn adst8(t: &mut [i32], r: u32) {
    adst_in_perm(t, 3);
    for i in 0..4 {
        bf(t, 2 * i, 2 * i + 1, 60 - 16 * i as i32, true);
    }
    for i in 0..4 {
        hd(t, i, 4 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 4 + 3 * i, 5 + i, 48 - 32 * i as i32, true);
    }
    for i in 0..2 {
        for j in 0..2 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..2 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_perm(t, 3);
}

fn adst16(t: &mut [i32], r: u32) {
    adst_in_perm(t, 4);
    for i in 0..8 {
        bf(t, 2 * i, 2 * i + 1, 62 - 8 * i as i32, true);
    }
    for i in 0..8 {
        hd(t, i, 8 + i, false, r);
    }
    for i in 0..2 {
        bf(t, 8 + 2 * i, 9 + 2 * i, 56 - 32 * i as i32, true);
        bf(t, 13 + 2 * i, 12 + 2 * i, 8 + 32 * i as i32, true);
    }
    for i in 0..4 {
        for j in 0..2 {
            hd(t, 8 * j + i, 4 + 8 * j + i, false, r);
        }
    }
    for i in 0..2 {
        for j in 0..2 {
            bf(
                t,
                4 + 8 * j + 3 * i,
                5 + 8 * j + i,
                48 - 32 * i as i32,
                true,
            );
        }
    }
    for i in 0..2 {
        for j in 0..4 {
            hd(t, 4 * j + i, 2 + 4 * j + i, false, r);
        }
    }
    for i in 0..4 {
        bf(t, 2 + 4 * i, 3 + 4 * i, 32, true);
    }
    adst_out_perm(t, 4);
}

fn identity(t: &mut [i32], n: u32) {
    let len = 1usize << n;
    for v in t[..len].iter_mut() {
        let x = *v as i64;
        *v = match n {
            2 => round2(x * 5793, 12),
            3 => x * 2,
            4 => round2(x * 11586, 12),
            _ => x * 4,
        } as i32;
    }
}

fn wht(t: &mut [i32], shift: u32) {
    let mut a = t[0] >> shift;
    let mut c = t[1] >> shift;
    let mut d = t[2] >> shift;
    let mut b = t[3] >> shift;
    a += c;
    d -= b;
    let e = (a - d) >> 1;
    b = e - b;
    c = e - c;
    a -= b;
    d += c;
    t[0] = a;
    t[1] = b;
    t[2] = c;
    t[3] = d;
}

fn transform_1d(t: &mut [i32], kind: Kind, n: u32, r: u32) {
    match kind {
        Kind::Dct => idct(t, n, r),
        Kind::Adst => match n {
            2 => adst4(t),
            3 => adst8(t, r),
            _ => adst16(t, r),
        },
        Kind::Identity => identity(t, n),
    }
}

/// 2D inverse transform of `coef` (rows of min(w, 32) dequantised
/// coefficients) into `residual` (w * h). `rows_nz` is the number of
/// leading coefficient rows that can be non-zero.
pub(crate) fn inverse_2d(
    coef: &[i32],
    residual: &mut [i32],
    tx_size: usize,
    tx_type: u8,
    lossless: bool,
    bit_depth: u32,
    rows_nz: usize,
) {
    let log2w = TX_WIDTH_LOG2[tx_size] as u32;
    let log2h = TX_HEIGHT_LOG2[tx_size] as u32;
    let (w, h) = (1usize << log2w, 1usize << log2h);
    let tw = w.min(32);
    let row_shift = if lossless {
        0
    } else {
        TRANSFORM_ROW_SHIFT[tx_size] as u32
    };
    let col_shift = if lossless { 0 } else { 4 };
    let row_clamp = bit_depth + 8;
    let col_clamp = (bit_depth + 6).max(16);
    let (row_kind, col_kind) = kinds(tx_type);
    let rect = log2w.abs_diff(log2h) == 1;
    let col_lo = -(1i32 << (col_clamp - 1));
    let col_hi = (1i32 << (col_clamp - 1)) - 1;
    let mut t = [0i32; 64];
    for i in 0..h {
        let out = &mut residual[i * w..(i + 1) * w];
        if i >= rows_nz.min(32) {
            // Rows of zeros stay zero in every transform.
            out.fill(0);
            continue;
        }
        t[..w].fill(0);
        t[..tw].copy_from_slice(&coef[i * tw..i * tw + tw]);
        if rect {
            for v in t[..w].iter_mut() {
                *v = round2(*v as i64 * 2896, 12) as i32;
            }
        }
        if lossless {
            wht(&mut t, 2);
        } else {
            transform_1d(&mut t, row_kind, log2w, row_clamp);
        }
        for j in 0..w {
            out[j] = (round2(t[j] as i64, row_shift) as i32).clamp(col_lo, col_hi);
        }
    }
    for j in 0..w {
        for i in 0..h {
            t[i] = residual[i * w + j];
        }
        if lossless {
            wht(&mut t, 0);
        } else {
            transform_1d(&mut t, col_kind, log2h, col_clamp);
        }
        for i in 0..h {
            residual[i * w + j] = round2(t[i] as i64, col_shift) as i32;
        }
    }
}
