//! Intra prediction from the edge arrays AboveRow and LeftCol (AV1
//! specification section 7.11.2).

use super::tables::{
    DR_INTRA_DERIVATIVE, INTRA_EDGE_KERNEL, INTRA_FILTER_TAPS, MODE_TO_ANGLE, SM_WEIGHTS_TX_16X16,
    SM_WEIGHTS_TX_32X32, SM_WEIGHTS_TX_4X4, SM_WEIGHTS_TX_64X64, SM_WEIGHTS_TX_8X8,
};

pub(crate) const DC_PRED: u8 = 0;
pub(crate) const V_PRED: u8 = 1;
pub(crate) const H_PRED: u8 = 2;
pub(crate) const D67_PRED: u8 = 8;
pub(crate) const SMOOTH_PRED: u8 = 9;
pub(crate) const SMOOTH_V_PRED: u8 = 10;
pub(crate) const SMOOTH_H_PRED: u8 = 11;
pub(crate) const PAETH_PRED: u8 = 12;
pub(crate) const UV_CFL_PRED: u8 = 13;

/// Offset of index 0 in the edge arrays (index -1 .. -16 are below it).
pub(crate) const EDGE_OFF: usize = 16;
pub(crate) const EDGE_LEN: usize = EDGE_OFF + 2 * 64 + 32;

/// The edges of a transform block: `above[EDGE_OFF + i]` is AboveRow[ i ].
pub(crate) struct Edges {
    pub above: [i32; EDGE_LEN],
    pub left: [i32; EDGE_LEN],
}

pub(crate) fn is_directional(mode: u8) -> bool {
    (V_PRED..=D67_PRED).contains(&mode)
}

#[inline]
fn round2(x: i32, n: u32) -> i32 {
    (x + (1 << (n - 1))) >> n
}

fn sm_weights(log2: u32) -> &'static [u8] {
    match log2 {
        2 => &SM_WEIGHTS_TX_4X4,
        3 => &SM_WEIGHTS_TX_8X8,
        4 => &SM_WEIGHTS_TX_16X16,
        5 => &SM_WEIGHTS_TX_32X32,
        _ => &SM_WEIGHTS_TX_64X64,
    }
}

/// Parameters of a directional prediction besides the edges.
pub(crate) struct Directional {
    pub angle_delta: i32,
    pub enable_edge_filter: bool,
    /// Above or left neighbour uses a smooth mode (intra filter type).
    pub filter_type: bool,
    pub have_left: bool,
    pub have_above: bool,
    /// maxX - x + 1 and maxY - y + 1.
    pub room_x: i32,
    pub room_y: i32,
    pub bit_depth: u32,
}

/// Predicts a w x h block into `pred` (row stride w).
#[allow(clippy::too_many_arguments)]
pub(crate) fn predict(
    e: &mut Edges,
    pred: &mut [i32],
    mode: u8,
    log2w: u32,
    log2h: u32,
    have_left: bool,
    have_above: bool,
    filter_intra: Option<u8>,
    dir: &Directional,
) {
    let (w, h) = (1usize << log2w, 1usize << log2h);
    let a = EDGE_OFF;
    if let Some(fmode) = filter_intra {
        recursive(e, pred, w, h, fmode as usize, dir.bit_depth);
    } else if is_directional(mode) {
        directional(e, pred, mode, w, h, dir);
    } else if mode == SMOOTH_PRED {
        let wx = sm_weights(log2w);
        let wy = sm_weights(log2h);
        let bl = e.left[a + h - 1];
        let tr = e.above[a + w - 1];
        for i in 0..h {
            for j in 0..w {
                let s = wy[i] as i32 * e.above[a + j]
                    + (256 - wy[i] as i32) * bl
                    + wx[j] as i32 * e.left[a + i]
                    + (256 - wx[j] as i32) * tr;
                pred[i * w + j] = round2(s, 9);
            }
        }
    } else if mode == SMOOTH_V_PRED {
        let wy = sm_weights(log2h);
        let bl = e.left[a + h - 1];
        for i in 0..h {
            for j in 0..w {
                let s = wy[i] as i32 * e.above[a + j] + (256 - wy[i] as i32) * bl;
                pred[i * w + j] = round2(s, 8);
            }
        }
    } else if mode == SMOOTH_H_PRED {
        let wx = sm_weights(log2w);
        let tr = e.above[a + w - 1];
        for i in 0..h {
            for j in 0..w {
                let s = wx[j] as i32 * e.left[a + i] + (256 - wx[j] as i32) * tr;
                pred[i * w + j] = round2(s, 8);
            }
        }
    } else if mode == DC_PRED {
        let v = if have_left && have_above {
            let mut sum: i32 = 0;
            for k in 0..h {
                sum += e.left[a + k];
            }
            for k in 0..w {
                sum += e.above[a + k];
            }
            sum += ((w + h) >> 1) as i32;
            sum / (w + h) as i32
        } else if have_left {
            let mut sum: i32 = 0;
            for k in 0..h {
                sum += e.left[a + k];
            }
            ((sum + (h >> 1) as i32) >> log2h).clamp(0, (1 << dir.bit_depth) - 1)
        } else if have_above {
            let mut sum: i32 = 0;
            for k in 0..w {
                sum += e.above[a + k];
            }
            ((sum + (w >> 1) as i32) >> log2w).clamp(0, (1 << dir.bit_depth) - 1)
        } else {
            1 << (dir.bit_depth - 1)
        };
        pred[..w * h].fill(v);
    } else {
        // PAETH_PRED
        let tl = e.above[a - 1];
        for i in 0..h {
            let l = e.left[a + i];
            for j in 0..w {
                let t = e.above[a + j];
                let base = t + l - tl;
                let p_left = (base - l).abs();
                let p_top = (base - t).abs();
                let p_tl = (base - tl).abs();
                pred[i * w + j] = if p_left <= p_top && p_left <= p_tl {
                    l
                } else if p_top <= p_tl {
                    t
                } else {
                    tl
                };
            }
        }
    }
}

fn recursive(e: &Edges, pred: &mut [i32], w: usize, h: usize, mode: usize, bd: u32) {
    let a = EDGE_OFF;
    let max = (1 << bd) - 1;
    for i2 in 0..h >> 1 {
        for j4 in 0..w >> 2 {
            let mut p = [0i32; 7];
            for (i, pv) in p.iter_mut().enumerate() {
                *pv = if i < 5 {
                    if i2 == 0 {
                        e.above[a + (j4 << 2) + i - 1]
                    } else if j4 == 0 && i == 0 {
                        e.left[a + (i2 << 1) - 1]
                    } else {
                        pred[((i2 << 1) - 1) * w + (j4 << 2) + i - 1]
                    }
                } else if j4 == 0 {
                    e.left[a + (i2 << 1) + i - 5]
                } else {
                    pred[((i2 << 1) + i - 5) * w + (j4 << 2) - 1]
                };
            }
            for i1 in 0..2 {
                for j1 in 0..4 {
                    let taps = &INTRA_FILTER_TAPS[mode][(i1 << 2) + j1];
                    let mut pr = 0;
                    for i in 0..7 {
                        pr += taps[i] as i32 * p[i];
                    }
                    // Round2Signed( pr, 4 )
                    let v = if pr >= 0 {
                        (pr + 8) >> 4
                    } else {
                        -((-pr + 8) >> 4)
                    };
                    pred[((i2 << 1) + i1) * w + (j4 << 2) + j1] = v.clamp(0, max);
                }
            }
        }
    }
}

fn edge_strength(w: usize, h: usize, filter_type: bool, delta: i32) -> usize {
    let d = delta.abs();
    let blk = w + h;
    if !filter_type {
        if blk <= 8 {
            (d >= 56) as usize
        } else if blk <= 16 {
            (d >= 40) as usize
        } else if blk <= 24 {
            if d >= 32 {
                3
            } else if d >= 16 {
                2
            } else {
                (d >= 8) as usize
            }
        } else if blk <= 32 {
            if d >= 32 {
                3
            } else if d >= 4 {
                2
            } else {
                1
            }
        } else {
            3
        }
    } else if blk <= 8 {
        if d >= 64 {
            2
        } else {
            (d >= 40) as usize
        }
    } else if blk <= 16 {
        if d >= 48 {
            2
        } else {
            (d >= 20) as usize
        }
    } else if blk <= 24 {
        if d >= 4 {
            3
        } else {
            0
        }
    } else {
        3
    }
}

fn use_upsample(w: usize, h: usize, filter_type: bool, delta: i32) -> bool {
    let d = delta.abs();
    let blk = w + h;
    if d <= 0 || d >= 40 {
        false
    } else if !filter_type {
        blk <= 16
    } else {
        blk <= 8
    }
}

/// Intra edge filter over buf[-1 .. sz-2] (sz values starting at -1).
fn edge_filter(buf: &mut [i32; EDGE_LEN], sz: usize, strength: usize) {
    if strength == 0 {
        return;
    }
    let a = EDGE_OFF;
    let mut edge = [0i32; 160];
    for i in 0..sz {
        edge[i] = buf[a + i - 1];
    }
    let k = &INTRA_EDGE_KERNEL[strength - 1];
    for i in 1..sz {
        let mut s = 0;
        for j in 0..5 {
            let idx = (i as i32 - 2 + j as i32).clamp(0, sz as i32 - 1) as usize;
            s += k[j] as i32 * edge[idx];
        }
        buf[a + i - 1] = (s + 8) >> 4;
    }
}

fn edge_upsample(buf: &mut [i32; EDGE_LEN], num_px: usize, bd: u32) {
    let a = EDGE_OFF;
    let mut dup = [0i32; 40];
    dup[0] = buf[a - 1];
    for i in 0..=num_px {
        // i - 1 = -1 .. num_px - 1
        dup[i + 1] = buf[a + i - 1];
    }
    dup[num_px + 2] = buf[a + num_px - 1];
    let max = (1 << bd) - 1;
    buf[a - 2] = dup[0];
    for i in 0..num_px {
        let s = -dup[i] + 9 * dup[i + 1] + 9 * dup[i + 2] - dup[i + 3];
        let s = ((s + 8) >> 4).clamp(0, max);
        buf[a + 2 * i - 1] = s;
        buf[a + 2 * i] = dup[i + 2];
    }
}

fn directional(e: &mut Edges, pred: &mut [i32], mode: u8, w: usize, h: usize, d: &Directional) {
    let a = EDGE_OFF;
    let p_angle = MODE_TO_ANGLE[mode as usize] as i32 + d.angle_delta * 3;
    let mut up_above = 0u32;
    let mut up_left = 0u32;
    if d.enable_edge_filter {
        if p_angle != 90 && p_angle != 180 {
            if p_angle > 90 && p_angle < 180 && w + h >= 24 {
                let s = e.left[a] * 5 + e.above[a - 1] * 6 + e.above[a] * 5;
                let v = round2(s, 4);
                e.left[a - 1] = v;
                e.above[a - 1] = v;
            }
            if d.have_above {
                let strength = edge_strength(w, h, d.filter_type, p_angle - 90);
                let extra = if p_angle < 90 { h } else { 0 };
                let num_px = (w as i32).min(d.room_x) as usize + extra + 1;
                edge_filter(&mut e.above, num_px, strength);
            }
            if d.have_left {
                let strength = edge_strength(w, h, d.filter_type, p_angle - 180);
                let extra = if p_angle > 180 { w } else { 0 };
                let num_px = (h as i32).min(d.room_y) as usize + extra + 1;
                edge_filter(&mut e.left, num_px, strength);
            }
        }
        if use_upsample(w, h, d.filter_type, p_angle - 90) {
            up_above = 1;
            let extra = if p_angle < 90 { h } else { 0 };
            let num_px = w + extra;
            edge_upsample(&mut e.above, num_px, d.bit_depth);
        }
        if use_upsample(w, h, d.filter_type, p_angle - 180) {
            up_left = 1;
            let extra = if p_angle > 180 { w } else { 0 };
            let num_px = h + extra;
            edge_upsample(&mut e.left, num_px, d.bit_depth);
        }
    }
    let ab = |i: i32| e.above[(a as i32 + i) as usize];
    let lf = |i: i32| e.left[(a as i32 + i) as usize];
    if p_angle < 90 {
        let dx = DR_INTRA_DERIVATIVE[p_angle as usize] as i32;
        let max_base_x = ((w + h - 1) << up_above) as i32;
        for i in 0..h {
            let idx = (i as i32 + 1) * dx;
            let shift = ((idx << up_above) >> 1) & 0x1f;
            for j in 0..w {
                let base = (idx >> (6 - up_above)) + ((j as i32) << up_above);
                pred[i * w + j] = if base < max_base_x {
                    round2(ab(base) * (32 - shift) + ab(base + 1) * shift, 5)
                } else {
                    ab(max_base_x)
                };
            }
        }
    } else if p_angle > 90 && p_angle < 180 {
        let dx = DR_INTRA_DERIVATIVE[(180 - p_angle) as usize] as i32;
        let dy = DR_INTRA_DERIVATIVE[(p_angle - 90) as usize] as i32;
        for i in 0..h {
            for j in 0..w {
                let idx = ((j as i32) << 6) - (i as i32 + 1) * dx;
                let base = idx >> (6 - up_above);
                pred[i * w + j] = if base >= -(1 << up_above) {
                    let shift = ((idx << up_above) >> 1) & 0x1f;
                    round2(ab(base) * (32 - shift) + ab(base + 1) * shift, 5)
                } else {
                    let idx = ((i as i32) << 6) - (j as i32 + 1) * dy;
                    let base = idx >> (6 - up_left);
                    let shift = ((idx << up_left) >> 1) & 0x1f;
                    round2(lf(base) * (32 - shift) + lf(base + 1) * shift, 5)
                };
            }
        }
    } else if p_angle > 180 {
        let dy = DR_INTRA_DERIVATIVE[(270 - p_angle) as usize] as i32;
        for j in 0..w {
            let idx = (j as i32 + 1) * dy;
            let shift = ((idx << up_left) >> 1) & 0x1f;
            for i in 0..h {
                let base = (idx >> (6 - up_left)) + ((i as i32) << up_left);
                pred[i * w + j] = round2(lf(base) * (32 - shift) + lf(base + 1) * shift, 5);
            }
        }
    } else if p_angle == 90 {
        for i in 0..h {
            for j in 0..w {
                pred[i * w + j] = e.above[a + j];
            }
        }
    } else {
        for i in 0..h {
            for j in 0..w {
                pred[i * w + j] = e.left[a + i];
            }
        }
    }
}
