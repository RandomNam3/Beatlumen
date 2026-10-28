//! VP8 key frame decoder (lossy WebP), written from RFC 6386: boolean
//! entropy decoder, segmentation, token partitions, intra prediction
//! (16x16, 4x4 subblock and chroma modes), WHT / DCT inversion and the
//! normal and simple loop filters. Inter frames are not needed for WebP.

use super::vp8_tables::{
    AC_QLOOKUP, COEFF_UPDATE_PROBS, DC_QLOOKUP, DEFAULT_COEFF_PROBS, KF_BMODE_PROB,
};
use crate::Error;

/// Decoded 4:2:0 frame; planes are padded to whole macroblocks.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub y_stride: usize,
    pub uv_stride: usize,
}

/// Boolean entropy decoder (RFC 6386 section 7) with a 64-bit window.
struct BoolDecoder<'a> {
    data: &'a [u8],
    pos: usize,
    value: u64,
    bits: i32,
    range: u32,
}

impl<'a> BoolDecoder<'a> {
    fn new(data: &'a [u8]) -> Self {
        let mut d = BoolDecoder {
            data,
            pos: 0,
            value: 0,
            bits: 0,
            range: 255,
        };
        d.fill();
        d
    }

    fn fill(&mut self) {
        while self.bits <= 56 {
            let b = self.data.get(self.pos).copied().unwrap_or(0);
            self.pos += 1;
            self.value |= (b as u64) << (56 - self.bits);
            self.bits += 8;
        }
    }

    #[inline(always)]
    fn read(&mut self, prob: u8) -> bool {
        if self.bits < 8 {
            self.fill();
        }
        let split = 1 + (((self.range - 1) * prob as u32) >> 8);
        let big = (split as u64) << 56;
        let bit = if self.value >= big {
            self.range -= split;
            self.value -= big;
            true
        } else {
            self.range = split;
            false
        };
        let shift = self.range.leading_zeros().saturating_sub(24);
        self.range <<= shift;
        self.value <<= shift;
        self.bits -= shift as i32;
        bit
    }

    fn flag(&mut self) -> bool {
        self.read(128)
    }

    fn literal(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.flag() as u32;
        }
        v
    }

    /// Optional signed value: flag, magnitude, sign.
    fn maybe_signed(&mut self, n: u32) -> i32 {
        if !self.flag() {
            return 0;
        }
        let v = self.literal(n) as i32;
        if self.flag() {
            -v
        } else {
            v
        }
    }

    fn tree(&mut self, tree: &[i8], probs: &[u8]) -> usize {
        let mut i = 0usize;
        loop {
            let n = tree[i + self.read(probs[i >> 1]) as usize];
            if n <= 0 {
                return (-n) as usize;
            }
            i = n as usize;
        }
    }

    /// True once more than a few bytes past the end were consumed.
    fn exhausted(&self) -> bool {
        self.pos > self.data.len() + 16
    }
}

// Macroblock luma modes.
const DC_PRED: usize = 0;
const V_PRED: usize = 1;
const H_PRED: usize = 2;
const TM_PRED: usize = 3;
const B_PRED: usize = 4;
// Subblock modes.
const B_DC_PRED: usize = 0;
const B_TM_PRED: usize = 1;
const B_VE_PRED: usize = 2;
const B_HE_PRED: usize = 3;
const B_LD_PRED: usize = 4;
const B_RD_PRED: usize = 5;
const B_VR_PRED: usize = 6;
const B_VL_PRED: usize = 7;
const B_HD_PRED: usize = 8;
const B_HU_PRED: usize = 9;

const KF_YMODE_TREE: [i8; 8] = [
    -(B_PRED as i8),
    2,
    4,
    6,
    -(DC_PRED as i8),
    -(V_PRED as i8),
    -(H_PRED as i8),
    -(TM_PRED as i8),
];
const KF_YMODE_PROB: [u8; 4] = [145, 156, 163, 128];
const UV_MODE_TREE: [i8; 6] = [
    -(DC_PRED as i8),
    2,
    -(V_PRED as i8),
    4,
    -(H_PRED as i8),
    -(TM_PRED as i8),
];
const KF_UV_MODE_PROB: [u8; 3] = [142, 114, 183];
const BMODE_TREE: [i8; 18] = [
    -(B_DC_PRED as i8),
    2,
    -(B_TM_PRED as i8),
    4,
    -(B_VE_PRED as i8),
    6,
    8,
    12,
    -(B_HE_PRED as i8),
    10,
    -(B_RD_PRED as i8),
    -(B_VR_PRED as i8),
    -(B_LD_PRED as i8),
    14,
    -(B_VL_PRED as i8),
    16,
    -(B_HD_PRED as i8),
    -(B_HU_PRED as i8),
];
const SEGMENT_TREE: [i8; 6] = [2, 4, -0, -1, -2, -3];

const COEFF_BANDS: [usize; 17] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7, 0];
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const PCAT3: [u8; 3] = [173, 148, 140];
const PCAT4: [u8; 4] = [176, 155, 140, 135];
const PCAT5: [u8; 5] = [180, 157, 141, 134, 130];
const PCAT6: [u8; 11] = [254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129];

type CoeffProbs = [[[[u8; 11]; 3]; 8]; 4];

/// Dequantization factors of one segment: [y1, y2, uv] x [dc, ac].
#[derive(Clone, Copy, Default)]
struct Dequant {
    y1: [i32; 2],
    y2: [i32; 2],
    uv: [i32; 2],
}

#[derive(Clone, Copy, Default)]
struct MbInfo {
    filter_level: u8,
    /// Inner edges are filtered (B_PRED or non-zero coefficients).
    inner: bool,
}

struct Header {
    segmentation: bool,
    update_map: bool,
    seg_abs: bool,
    seg_quant: [i32; 4],
    seg_lf: [i32; 4],
    seg_probs: [u8; 3],
    simple_filter: bool,
    filter_level: i32,
    sharpness: i32,
    lf_delta: bool,
    ref_delta: [i32; 4],
    mode_delta: [i32; 4],
    skip_enabled: bool,
    skip_prob: u8,
}

pub fn decode(data: &[u8]) -> Result<Frame, Error> {
    if data.len() < 10 {
        return Err(Error::InvalidData("vp8: truncated frame"));
    }
    let tag = data[0] as u32 | (data[1] as u32) << 8 | (data[2] as u32) << 16;
    if tag & 1 != 0 {
        return Err(Error::Unsupported("vp8: not a key frame"));
    }
    if (tag >> 1) & 7 > 3 {
        return Err(Error::Unsupported("vp8: unknown version"));
    }
    let first_part = (tag >> 5) as usize;
    if data[3..6] != [0x9d, 0x01, 0x2a] {
        return Err(Error::InvalidData("vp8: bad start code"));
    }
    let width = (u16::from_le_bytes([data[6], data[7]]) & 0x3fff) as usize;
    let height = (u16::from_le_bytes([data[8], data[9]]) & 0x3fff) as usize;
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("vp8: zero dimension"));
    }
    let rest = &data[10..];
    let part0 = &rest[..first_part.min(rest.len())];
    let mut bd = BoolDecoder::new(part0);
    let _color_space = bd.flag();
    let _clamping = bd.flag();

    let mut h = Header {
        segmentation: false,
        update_map: false,
        seg_abs: false,
        seg_quant: [0; 4],
        seg_lf: [0; 4],
        seg_probs: [255; 3],
        simple_filter: false,
        filter_level: 0,
        sharpness: 0,
        lf_delta: false,
        ref_delta: [0; 4],
        mode_delta: [0; 4],
        skip_enabled: false,
        skip_prob: 0,
    };
    h.segmentation = bd.flag();
    if h.segmentation {
        h.update_map = bd.flag();
        let update_data = bd.flag();
        if update_data {
            h.seg_abs = bd.flag();
            for q in h.seg_quant.iter_mut() {
                *q = bd.maybe_signed(7);
            }
            for l in h.seg_lf.iter_mut() {
                *l = bd.maybe_signed(6);
            }
        }
        if h.update_map {
            for p in h.seg_probs.iter_mut() {
                *p = if bd.flag() { bd.literal(8) as u8 } else { 255 };
            }
        }
    }
    h.simple_filter = bd.flag();
    h.filter_level = bd.literal(6) as i32;
    h.sharpness = bd.literal(3) as i32;
    h.lf_delta = bd.flag();
    if h.lf_delta && bd.flag() {
        for d in h.ref_delta.iter_mut() {
            *d = bd.maybe_signed(6);
        }
        for d in h.mode_delta.iter_mut() {
            *d = bd.maybe_signed(6);
        }
    }
    let num_parts = 1usize << bd.literal(2);
    // Token partitions follow the first partition.
    let after = &rest[first_part.min(rest.len())..];
    let sizes_len = 3 * (num_parts - 1);
    if after.len() < sizes_len {
        return Err(Error::InvalidData("vp8: truncated partition table"));
    }
    let mut parts: Vec<BoolDecoder> = Vec::with_capacity(num_parts);
    let mut off = sizes_len;
    for i in 0..num_parts {
        let size = if i + 1 < num_parts {
            let s = &after[3 * i..3 * i + 3];
            s[0] as usize | (s[1] as usize) << 8 | (s[2] as usize) << 16
        } else {
            after.len().saturating_sub(off)
        };
        let start = off.min(after.len());
        let end = off.saturating_add(size).min(after.len());
        parts.push(BoolDecoder::new(&after[start..end]));
        off = off.saturating_add(size);
    }

    // Quantizer indices.
    let q_index = bd.literal(7) as i32;
    let y1_dc = bd.maybe_signed(4);
    let y2_dc = bd.maybe_signed(4);
    let y2_ac = bd.maybe_signed(4);
    let uv_dc = bd.maybe_signed(4);
    let uv_ac = bd.maybe_signed(4);
    let _refresh_entropy = bd.flag();
    let mut probs: CoeffProbs = DEFAULT_COEFF_PROBS;
    for (i, a) in probs.iter_mut().enumerate() {
        for (j, b) in a.iter_mut().enumerate() {
            for (k, c) in b.iter_mut().enumerate() {
                for (l, p) in c.iter_mut().enumerate() {
                    if bd.read(COEFF_UPDATE_PROBS[i][j][k][l]) {
                        *p = bd.literal(8) as u8;
                    }
                }
            }
        }
    }
    h.skip_enabled = bd.flag();
    if h.skip_enabled {
        h.skip_prob = bd.literal(8) as u8;
    }

    let dq_lookup = |tab: &[u16; 128], q: i32| tab[q.clamp(0, 127) as usize] as i32;
    let mut dequant = [Dequant::default(); 4];
    for (s, d) in dequant.iter_mut().enumerate() {
        let mut q = q_index;
        if h.segmentation {
            q = if h.seg_abs {
                h.seg_quant[s]
            } else {
                q + h.seg_quant[s]
            };
        }
        d.y1 = [dq_lookup(&DC_QLOOKUP, q + y1_dc), dq_lookup(&AC_QLOOKUP, q)];
        d.y2 = [
            dq_lookup(&DC_QLOOKUP, q + y2_dc) * 2,
            (dq_lookup(&AC_QLOOKUP, q + y2_ac) * 155 / 100).max(8),
        ];
        d.uv = [
            dq_lookup(&DC_QLOOKUP, q + uv_dc).min(132),
            dq_lookup(&AC_QLOOKUP, q + uv_ac),
        ];
    }

    let mb_cols = width.div_ceil(16);
    let mb_rows = height.div_ceil(16);
    if (mb_cols * 16) as u64 * (mb_rows * 16) as u64 > crate::MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    let y_stride = mb_cols * 16;
    let uv_stride = mb_cols * 8;
    let mut f = Frame {
        width,
        height,
        y: vec![0; y_stride * mb_rows * 16],
        u: vec![0; uv_stride * mb_rows * 8],
        v: vec![0; uv_stride * mb_rows * 8],
        y_stride,
        uv_stride,
    };

    let mut mb_info = vec![MbInfo::default(); mb_cols * mb_rows];
    // Above contexts: token non-zero flags (9 per MB) and subblock modes.
    let mut above_nz = vec![[0u8; 9]; mb_cols];
    let mut above_modes = vec![[B_DC_PRED; 4]; mb_cols];
    for my in 0..mb_rows {
        let mut left_nz = [0u8; 9];
        let mut left_modes = [B_DC_PRED; 4];
        let tp = &mut parts[my % num_parts];
        for mx in 0..mb_cols {
            // --- Modes (first partition).
            let segment = if h.update_map {
                bd.tree(&SEGMENT_TREE, &h.seg_probs)
            } else {
                0
            };
            let skip = h.skip_enabled && bd.read(h.skip_prob);
            let ymode = bd.tree(&KF_YMODE_TREE, &KF_YMODE_PROB);
            let mut bmodes = [B_DC_PRED; 16];
            if ymode == B_PRED {
                for i in 0..16 {
                    let a = if i < 4 {
                        above_modes[mx][i]
                    } else {
                        bmodes[i - 4]
                    };
                    let l = if i % 4 == 0 {
                        left_modes[i / 4]
                    } else {
                        bmodes[i - 1]
                    };
                    bmodes[i] = bd.tree(&BMODE_TREE, &KF_BMODE_PROB[a][l]);
                }
            } else {
                let m = match ymode {
                    DC_PRED => B_DC_PRED,
                    V_PRED => B_VE_PRED,
                    H_PRED => B_HE_PRED,
                    _ => B_TM_PRED,
                };
                bmodes = [m; 16];
            }
            for i in 0..4 {
                above_modes[mx][i] = bmodes[12 + i];
                left_modes[i] = bmodes[i * 4 + 3];
            }
            let uvmode = bd.tree(&UV_MODE_TREE, &KF_UV_MODE_PROB);

            // --- Residual tokens.
            let mut coeffs = [[0i16; 16]; 25];
            let mut any_nz = false;
            let anz = &mut above_nz[mx];
            if skip {
                let keep_y2 = ymode == B_PRED;
                for k in 0..8 {
                    left_nz[k] = 0;
                    anz[k] = 0;
                }
                if !keep_y2 {
                    left_nz[8] = 0;
                    anz[8] = 0;
                }
            } else {
                let dq = &dequant[segment];
                let has_y2 = ymode != B_PRED;
                if has_y2 {
                    let ctx = (left_nz[8] + anz[8]) as usize;
                    let nz = read_block(tp, &probs[1], ctx, 0, dq.y2, &mut coeffs[24]);
                    left_nz[8] = nz as u8;
                    anz[8] = nz as u8;
                    any_nz |= nz;
                }
                let (ytype, first) = if has_y2 { (0, 1) } else { (3, 0) };
                for i in 0..16 {
                    let (l, a) = (i / 4, i % 4);
                    let ctx = (left_nz[l] + anz[a]) as usize;
                    let nz = read_block(tp, &probs[ytype], ctx, first, dq.y1, &mut coeffs[i]);
                    left_nz[l] = nz as u8;
                    anz[a] = nz as u8;
                    any_nz |= nz;
                }
                for i in 16..24 {
                    let j = i - 16;
                    let base = if j < 4 { 4 } else { 6 };
                    let (l, a) = (base + (j % 4) / 2, base + j % 2);
                    let ctx = (left_nz[l] + anz[a]) as usize;
                    let nz = read_block(tp, &probs[2], ctx, 0, dq.uv, &mut coeffs[i]);
                    left_nz[l] = nz as u8;
                    anz[a] = nz as u8;
                    any_nz |= nz;
                }
                if has_y2 {
                    let dc = inverse_wht(&coeffs[24]);
                    for i in 0..16 {
                        coeffs[i][0] = dc[i];
                    }
                }
            }

            // --- Reconstruction.
            reconstruct_mb(&mut f, mb_cols, mx, my, ymode, &bmodes, uvmode, &coeffs);

            // Loop filter level for this macroblock.
            let mut level = h.filter_level;
            if h.segmentation {
                level = if h.seg_abs {
                    h.seg_lf[segment]
                } else {
                    level + h.seg_lf[segment]
                };
            }
            level = level.clamp(0, 63);
            if h.lf_delta {
                level += h.ref_delta[0];
                if ymode == B_PRED {
                    level += h.mode_delta[0];
                }
                level = level.clamp(0, 63);
            }
            mb_info[my * mb_cols + mx] = MbInfo {
                filter_level: level as u8,
                inner: any_nz || ymode == B_PRED,
            };
        }
        if bd.exhausted() {
            break; // truncated mode partition: keep what was decoded
        }
    }
    if h.filter_level > 0 {
        loop_filter(&mut f, &h, &mb_info, mb_cols, mb_rows);
    }
    Ok(f)
}

/// Reads the tokens of one 4x4 block; returns whether any coefficient was
/// coded (the context for neighbouring blocks).
fn read_block(
    bd: &mut BoolDecoder,
    probs: &[[[u8; 11]; 3]; 8],
    ctx: usize,
    first: usize,
    dq: [i32; 2],
    out: &mut [i16; 16],
) -> bool {
    let mut c = first;
    let mut p = &probs[COEFF_BANDS[c]][ctx];
    if !bd.read(p[0]) {
        return false;
    }
    loop {
        // EOB was ruled out at this position; read zero tokens.
        while !bd.read(p[1]) {
            if c >= 15 {
                return true;
            }
            c += 1;
            p = &probs[COEFF_BANDS[c]][0];
        }
        let v: i32 = if !bd.read(p[2]) {
            1
        } else if !bd.read(p[3]) {
            if !bd.read(p[4]) {
                2
            } else if !bd.read(p[5]) {
                3
            } else {
                4
            }
        } else if !bd.read(p[6]) {
            if !bd.read(p[7]) {
                5 + bd.read(159) as i32
            } else {
                7 + 2 * bd.read(165) as i32 + bd.read(145) as i32
            }
        } else {
            let (base, cat): (i32, &[u8]) = if !bd.read(p[8]) {
                if !bd.read(p[9]) {
                    (11, &PCAT3)
                } else {
                    (19, &PCAT4)
                }
            } else if !bd.read(p[10]) {
                (35, &PCAT5)
            } else {
                (67, &PCAT6)
            };
            let mut e = 0;
            for &pr in cat {
                e = (e << 1) | bd.read(pr) as i32;
            }
            base + e
        };
        let v = if bd.flag() { -v } else { v };
        out[ZIGZAG[c]] = (v * dq[(c > 0) as usize]) as i16;
        if c >= 15 {
            return true;
        }
        c += 1;
        p = &probs[COEFF_BANDS[c]][if v.abs() == 1 { 1 } else { 2 }];
        if !bd.read(p[0]) {
            return true;
        }
    }
}

fn inverse_wht(input: &[i16; 16]) -> [i16; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| input[i + k] as i32;
        let a1 = ip(0) + ip(12);
        let b1 = ip(4) + ip(8);
        let c1 = ip(4) - ip(8);
        let d1 = ip(0) - ip(12);
        t[i] = a1 + b1;
        t[4 + i] = c1 + d1;
        t[8 + i] = a1 - b1;
        t[12 + i] = d1 - c1;
    }
    let mut out = [0i16; 16];
    for i in 0..4 {
        let r = &t[4 * i..4 * i + 4];
        let a1 = r[0] + r[3];
        let b1 = r[1] + r[2];
        let c1 = r[1] - r[2];
        let d1 = r[0] - r[3];
        out[4 * i] = ((a1 + b1 + 3) >> 3) as i16;
        out[4 * i + 1] = ((c1 + d1 + 3) >> 3) as i16;
        out[4 * i + 2] = ((a1 - b1 + 3) >> 3) as i16;
        out[4 * i + 3] = ((d1 - c1 + 3) >> 3) as i16;
    }
    out
}

/// Inverse DCT of one block, added to the prediction at `off`.
fn idct_add(coef: &[i16; 16], dst: &mut [u8], off: usize, stride: usize) {
    const C1: i32 = 20091; // cos(pi/8) * sqrt(2) - 1, Q16
    const S1: i32 = 35468; // sin(pi/8) * sqrt(2), Q16
    if coef[1..].iter().all(|&c| c == 0) {
        let dc = (coef[0] as i32 + 4) >> 3;
        if dc == 0 {
            return;
        }
        for y in 0..4 {
            for x in 0..4 {
                let p = &mut dst[off + y * stride + x];
                *p = (*p as i32 + dc).clamp(0, 255) as u8;
            }
        }
        return;
    }
    let mut t = [0i32; 16];
    for i in 0..4 {
        let ip = |k: usize| coef[i + k] as i32;
        let a1 = ip(0) + ip(8);
        let b1 = ip(0) - ip(8);
        let c1 = ((ip(4) * S1) >> 16) - (ip(12) + ((ip(12) * C1) >> 16));
        let d1 = (ip(4) + ((ip(4) * C1) >> 16)) + ((ip(12) * S1) >> 16);
        t[i] = (a1 + d1) as i16 as i32;
        t[12 + i] = (a1 - d1) as i16 as i32;
        t[4 + i] = (b1 + c1) as i16 as i32;
        t[8 + i] = (b1 - c1) as i16 as i32;
    }
    for i in 0..4 {
        let r = &t[4 * i..4 * i + 4];
        let a1 = r[0] + r[2];
        let b1 = r[0] - r[2];
        let c1 = ((r[1] * S1) >> 16) - (r[3] + ((r[3] * C1) >> 16));
        let d1 = (r[1] + ((r[1] * C1) >> 16)) + ((r[3] * S1) >> 16);
        let res = [
            (a1 + d1 + 4) >> 3,
            (b1 + c1 + 4) >> 3,
            (b1 - c1 + 4) >> 3,
            (a1 - d1 + 4) >> 3,
        ];
        let row = &mut dst[off + i * stride..off + i * stride + 4];
        for (p, r) in row.iter_mut().zip(res) {
            *p = (*p as i32 + r).clamp(0, 255) as u8;
        }
    }
}

/// Predicts a size x size block (16 for luma, 8 for chroma) with a
/// macroblock-level mode, writing into the plane.
fn predict_mb(plane: &mut [u8], stride: usize, mx: usize, my: usize, size: usize, mode: usize) {
    let (x0, y0) = (mx * size, my * size);
    let mut above = [127u8; 16];
    let mut left = [129u8; 16];
    if my > 0 {
        above[..size]
            .copy_from_slice(&plane[(y0 - 1) * stride + x0..(y0 - 1) * stride + x0 + size]);
    }
    if mx > 0 {
        for (i, l) in left.iter_mut().enumerate().take(size) {
            *l = plane[(y0 + i) * stride + x0 - 1];
        }
    }
    let p = if my == 0 {
        127
    } else if mx == 0 {
        129
    } else {
        plane[(y0 - 1) * stride + x0 - 1]
    };
    let shift = if size == 16 { 4 } else { 3 };
    match mode {
        DC_PRED => {
            let sa: u32 = above[..size].iter().map(|&v| v as u32).sum();
            let sl: u32 = left[..size].iter().map(|&v| v as u32).sum();
            let v = match (mx > 0, my > 0) {
                (true, true) => (sa + sl + size as u32) >> (shift + 1),
                (false, true) => (sa + (size as u32 >> 1)) >> shift,
                (true, false) => (sl + (size as u32 >> 1)) >> shift,
                (false, false) => 128,
            } as u8;
            for y in 0..size {
                plane[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + size].fill(v);
            }
        }
        V_PRED => {
            for y in 0..size {
                plane[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + size]
                    .copy_from_slice(&above[..size]);
            }
        }
        H_PRED => {
            for y in 0..size {
                plane[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + size].fill(left[y]);
            }
        }
        _ => {
            for y in 0..size {
                for x in 0..size {
                    plane[(y0 + y) * stride + x0 + x] =
                        (left[y] as i32 + above[x] as i32 - p as i32).clamp(0, 255) as u8;
                }
            }
        }
    }
}

#[inline(always)]
fn avg3(x: u8, y: u8, z: u8) -> u8 {
    ((x as u16 + 2 * y as u16 + z as u16 + 2) >> 2) as u8
}

#[inline(always)]
fn avg2(x: u8, y: u8) -> u8 {
    ((x as u16 + y as u16 + 1) >> 1) as u8
}

/// 4x4 subblock prediction. `a[0]` is the top-left pixel P, `a[1..9]` the
/// 8 pixels above (4 above + 4 above-right), `l` the left column.
fn predict_sub(b: &mut [[u8; 4]; 4], a: &[u8; 9], l: &[u8; 4], mode: usize) {
    let p = a[0];
    let at = |i: usize| a[i + 1]; // A[i]
                                  // Edge array E: L[3], L[2], L[1], L[0], P, A[0..4].
    let e = [l[3], l[2], l[1], l[0], p, at(0), at(1), at(2), at(3)];
    match mode {
        B_DC_PRED => {
            let mut v = 4u32;
            for i in 0..4 {
                v += at(i) as u32 + l[i] as u32;
            }
            *b = [[(v >> 3) as u8; 4]; 4];
        }
        B_TM_PRED => {
            for r in 0..4 {
                for c in 0..4 {
                    b[r][c] = (l[r] as i32 + at(c) as i32 - p as i32).clamp(0, 255) as u8;
                }
            }
        }
        B_VE_PRED => {
            for c in 0..4 {
                let prev = if c == 0 { p } else { at(c - 1) };
                let v = avg3(prev, at(c), at(c + 1));
                for row in b.iter_mut() {
                    row[c] = v;
                }
            }
        }
        B_HE_PRED => {
            let v = [
                avg3(p, l[0], l[1]),
                avg3(l[0], l[1], l[2]),
                avg3(l[1], l[2], l[3]),
                avg3(l[2], l[3], l[3]),
            ];
            for r in 0..4 {
                b[r] = [v[r]; 4];
            }
        }
        B_LD_PRED => {
            let s = |i: usize| {
                if i == 7 {
                    avg3(at(6), at(7), at(7))
                } else {
                    avg3(at(i - 1), at(i), at(i + 1))
                }
            };
            for r in 0..4 {
                for c in 0..4 {
                    b[r][c] = s(r + c + 1);
                }
            }
        }
        B_RD_PRED => {
            // Value on the diagonal r - c = k is avg3p(E + 4 - k).
            for r in 0..4 {
                for c in 0..4 {
                    let i = 4 + c - r;
                    b[r][c] = avg3(e[i - 1], e[i], e[i + 1]);
                }
            }
        }
        B_VR_PRED => {
            let a3 = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
            let a2 = |i: usize| avg2(e[i], e[i + 1]);
            b[3][0] = a3(2);
            b[2][0] = a3(3);
            b[3][1] = a3(4);
            b[1][0] = a3(4);
            b[2][1] = a2(4);
            b[0][0] = a2(4);
            b[3][2] = a3(5);
            b[1][1] = a3(5);
            b[2][2] = a2(5);
            b[0][1] = a2(5);
            b[3][3] = a3(6);
            b[1][2] = a3(6);
            b[2][3] = a2(6);
            b[0][2] = a2(6);
            b[1][3] = a3(7);
            b[0][3] = a2(7);
        }
        B_VL_PRED => {
            let a3 = |i: usize| avg3(at(i - 1), at(i), at(i + 1));
            let a2 = |i: usize| avg2(at(i), at(i + 1));
            b[0][0] = a2(0);
            b[1][0] = a3(1);
            b[2][0] = a2(1);
            b[0][1] = a2(1);
            b[1][1] = a3(2);
            b[3][0] = a3(2);
            b[2][1] = a2(2);
            b[0][2] = a2(2);
            b[3][1] = a3(3);
            b[1][2] = a3(3);
            b[2][2] = a2(3);
            b[0][3] = a2(3);
            b[3][2] = a3(4);
            b[1][3] = a3(4);
            b[2][3] = a3(5);
            b[3][3] = a3(6);
        }
        B_HD_PRED => {
            let a3 = |i: usize| avg3(e[i - 1], e[i], e[i + 1]);
            let a2 = |i: usize| avg2(e[i], e[i + 1]);
            b[3][0] = a2(0);
            b[3][1] = a3(1);
            b[2][0] = a2(1);
            b[3][2] = a2(1);
            b[2][1] = a3(2);
            b[3][3] = a3(2);
            b[2][2] = a2(2);
            b[1][0] = a2(2);
            b[2][3] = a3(3);
            b[1][1] = a3(3);
            b[1][2] = a2(3);
            b[0][0] = a2(3);
            b[1][3] = a3(4);
            b[0][1] = a3(4);
            b[0][2] = a3(5);
            b[0][3] = a3(6);
        }
        _ => {
            // B_HU_PRED
            b[0][0] = avg2(l[0], l[1]);
            b[0][1] = avg3(l[0], l[1], l[2]);
            b[0][2] = avg2(l[1], l[2]);
            b[1][0] = b[0][2];
            b[0][3] = avg3(l[1], l[2], l[3]);
            b[1][1] = b[0][3];
            b[1][2] = avg2(l[2], l[3]);
            b[2][0] = b[1][2];
            b[1][3] = avg3(l[2], l[3], l[3]);
            b[2][1] = b[1][3];
            b[2][2] = l[3];
            b[2][3] = l[3];
            b[3] = [l[3]; 4];
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn reconstruct_mb(
    f: &mut Frame,
    mb_cols: usize,
    mx: usize,
    my: usize,
    ymode: usize,
    bmodes: &[usize; 16],
    uvmode: usize,
    coeffs: &[[i16; 16]; 25],
) {
    let ys = f.y_stride;
    if ymode == B_PRED {
        for sb in 0..16 {
            let (bx, by) = (sb % 4, sb / 4);
            let (x0, y0) = (mx * 16 + bx * 4, my * 16 + by * 4);
            let mut a = [127u8; 9];
            let mut l = [129u8; 4];
            if x0 > 0 {
                for (i, v) in l.iter_mut().enumerate() {
                    *v = f.y[(y0 + i) * ys + x0 - 1];
                }
            }
            if y0 > 0 {
                a[0] = if x0 > 0 {
                    f.y[(y0 - 1) * ys + x0 - 1]
                } else {
                    129
                };
                a[1..5].copy_from_slice(&f.y[(y0 - 1) * ys + x0..(y0 - 1) * ys + x0 + 4]);
                // Above-right pixels.
                if bx < 3 && by > 0 {
                    a[5..9].copy_from_slice(&f.y[(y0 - 1) * ys + x0 + 4..(y0 - 1) * ys + x0 + 8]);
                } else if my == 0 {
                    a[5..9].fill(127);
                } else {
                    let row = (my * 16 - 1) * ys;
                    if bx < 3 {
                        a[5..9].copy_from_slice(&f.y[row + x0 + 4..row + x0 + 8]);
                    } else if mx + 1 < mb_cols {
                        a[5..9].copy_from_slice(&f.y[row + mx * 16 + 16..row + mx * 16 + 20]);
                    } else {
                        a[5..9].fill(f.y[row + mx * 16 + 15]);
                    }
                }
            }
            let mut pred = [[0u8; 4]; 4];
            predict_sub(&mut pred, &a, &l, bmodes[sb]);
            for r in 0..4 {
                f.y[(y0 + r) * ys + x0..(y0 + r) * ys + x0 + 4].copy_from_slice(&pred[r]);
            }
            idct_add(&coeffs[sb], &mut f.y, y0 * ys + x0, ys);
        }
    } else {
        predict_mb(&mut f.y, ys, mx, my, 16, ymode);
        for sb in 0..16 {
            let (x0, y0) = (mx * 16 + (sb % 4) * 4, my * 16 + (sb / 4) * 4);
            idct_add(&coeffs[sb], &mut f.y, y0 * ys + x0, ys);
        }
    }
    let us = f.uv_stride;
    for (plane, base) in [(&mut f.u, 16), (&mut f.v, 20)] {
        predict_mb(plane, us, mx, my, 8, uvmode);
        for j in 0..4 {
            let (x0, y0) = (mx * 8 + (j % 2) * 4, my * 8 + (j / 2) * 4);
            idct_add(&coeffs[base + j], plane, y0 * us + x0, us);
        }
    }
}

// ---------------------------------------------------------------- loop filter

#[inline(always)]
fn s8(v: i32) -> i32 {
    v.clamp(-128, 127)
}

#[inline(always)]
fn u8c(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// Applies the edge filters to one segment of pixels p3..q3 located at
/// `pos + k * step` for k = -4..4.
struct Seg<'a> {
    d: &'a mut [u8],
    pos: usize,
    step: usize,
}

impl Seg<'_> {
    #[inline(always)]
    fn get(&self, k: isize) -> i32 {
        self.d[(self.pos as isize + k * self.step as isize) as usize] as i32
    }
    #[inline(always)]
    fn set(&mut self, k: isize, v: u8) {
        let i = (self.pos as isize + k * self.step as isize) as usize;
        self.d[i] = v;
    }
}

#[inline(always)]
fn simple_threshold(s: &Seg, limit: i32) -> bool {
    (s.get(-1) - s.get(0)).abs() * 2 + ((s.get(-2) - s.get(1)).abs() >> 1) <= limit
}

#[inline(always)]
fn normal_threshold(s: &Seg, e: i32, i: i32) -> bool {
    let (p3, p2, p1, p0) = (s.get(-4), s.get(-3), s.get(-2), s.get(-1));
    let (q0, q1, q2, q3) = (s.get(0), s.get(1), s.get(2), s.get(3));
    (p0 - q0).abs() * 2 + ((p1 - q1).abs() >> 1) <= 2 * e + i
        && (p3 - p2).abs() <= i
        && (p2 - p1).abs() <= i
        && (p1 - p0).abs() <= i
        && (q3 - q2).abs() <= i
        && (q2 - q1).abs() <= i
        && (q1 - q0).abs() <= i
}

#[inline(always)]
fn hev(s: &Seg, t: i32) -> bool {
    (s.get(-2) - s.get(-1)).abs() > t || (s.get(1) - s.get(0)).abs() > t
}

#[inline(always)]
fn filter_common(s: &mut Seg, outer: bool) {
    let (p1, p0, q0, q1) = (
        s.get(-2) - 128,
        s.get(-1) - 128,
        s.get(0) - 128,
        s.get(1) - 128,
    );
    let mut a = 3 * (q0 - p0);
    if outer {
        a += s8(p1 - q1);
    }
    let a = s8(a);
    let f1 = s8(a + 4) >> 3;
    let f2 = s8(a + 3) >> 3;
    s.set(-1, u8c(s8(p0 + f2) + 128));
    s.set(0, u8c(s8(q0 - f1) + 128));
    if !outer {
        let a = (f1 + 1) >> 1;
        s.set(-2, u8c(s8(p1 + a) + 128));
        s.set(1, u8c(s8(q1 - a) + 128));
    }
}

#[inline(always)]
fn filter_mb_edge(s: &mut Seg) {
    let (p2, p1, p0) = (s.get(-3) - 128, s.get(-2) - 128, s.get(-1) - 128);
    let (q0, q1, q2) = (s.get(0) - 128, s.get(1) - 128, s.get(2) - 128);
    let w = s8(s8(p1 - q1) + 3 * (q0 - p0));
    let a = s8((27 * w + 63) >> 7);
    s.set(-1, u8c(s8(p0 + a) + 128));
    s.set(0, u8c(s8(q0 - a) + 128));
    let a = s8((18 * w + 63) >> 7);
    s.set(-2, u8c(s8(p1 + a) + 128));
    s.set(1, u8c(s8(q1 - a) + 128));
    let a = s8((9 * w + 63) >> 7);
    s.set(-3, u8c(s8(p2 + a) + 128));
    s.set(2, u8c(s8(q2 - a) + 128));
}

/// Filters `len` segments across an edge. `pos` is the first pixel after
/// the edge, `step` crosses the edge, `along` moves along it.
#[allow(clippy::too_many_arguments)]
fn filter_edge(
    d: &mut [u8],
    pos: usize,
    step: usize,
    along: usize,
    len: usize,
    mb: bool,
    e: i32,
    i: i32,
    t: i32,
) {
    for k in 0..len {
        let mut s = Seg {
            d: &mut *d,
            pos: pos + k * along,
            step,
        };
        if normal_threshold(&s, e, i) {
            if mb {
                if hev(&s, t) {
                    filter_common(&mut s, true);
                } else {
                    filter_mb_edge(&mut s);
                }
            } else {
                let h = hev(&s, t);
                filter_common(&mut s, h);
            }
        }
    }
}

fn filter_edge_simple(d: &mut [u8], pos: usize, step: usize, along: usize, limit: i32) {
    for k in 0..16 {
        let mut s = Seg {
            d: &mut *d,
            pos: pos + k * along,
            step,
        };
        if simple_threshold(&s, limit) {
            filter_common(&mut s, true);
        }
    }
}

fn loop_filter(f: &mut Frame, h: &Header, info: &[MbInfo], mb_cols: usize, mb_rows: usize) {
    let (ys, us) = (f.y_stride, f.uv_stride);
    for my in 0..mb_rows {
        for mx in 0..mb_cols {
            let mi = info[my * mb_cols + mx];
            let level = mi.filter_level as i32;
            if level == 0 {
                continue;
            }
            let mut interior = level;
            if h.sharpness > 0 {
                interior >>= if h.sharpness > 4 { 2 } else { 1 };
                interior = interior.min(9 - h.sharpness);
            }
            let interior = interior.max(1);
            let hev_t = if level >= 40 {
                2
            } else if level >= 15 {
                1
            } else {
                0
            };
            let (yx, yy) = (mx * 16, my * 16);
            let (cx, cy) = (mx * 8, my * 8);
            if h.simple_filter {
                let mb_limit = (level + 2) * 2 + interior;
                let b_limit = level * 2 + interior;
                if mx > 0 {
                    filter_edge_simple(&mut f.y, yy * ys + yx, 1, ys, mb_limit);
                }
                if mi.inner {
                    for k in [4, 8, 12] {
                        filter_edge_simple(&mut f.y, yy * ys + yx + k, 1, ys, b_limit);
                    }
                }
                if my > 0 {
                    filter_edge_simple(&mut f.y, yy * ys + yx, ys, 1, mb_limit);
                }
                if mi.inner {
                    for k in [4, 8, 12] {
                        filter_edge_simple(&mut f.y, (yy + k) * ys + yx, ys, 1, b_limit);
                    }
                }
                continue;
            }
            let me = level + 2;
            if mx > 0 {
                filter_edge(&mut f.y, yy * ys + yx, 1, ys, 16, true, me, interior, hev_t);
                filter_edge(&mut f.u, cy * us + cx, 1, us, 8, true, me, interior, hev_t);
                filter_edge(&mut f.v, cy * us + cx, 1, us, 8, true, me, interior, hev_t);
            }
            if mi.inner {
                for k in [4, 8, 12] {
                    filter_edge(
                        &mut f.y,
                        yy * ys + yx + k,
                        1,
                        ys,
                        16,
                        false,
                        level,
                        interior,
                        hev_t,
                    );
                }
                filter_edge(
                    &mut f.u,
                    cy * us + cx + 4,
                    1,
                    us,
                    8,
                    false,
                    level,
                    interior,
                    hev_t,
                );
                filter_edge(
                    &mut f.v,
                    cy * us + cx + 4,
                    1,
                    us,
                    8,
                    false,
                    level,
                    interior,
                    hev_t,
                );
            }
            if my > 0 {
                filter_edge(&mut f.y, yy * ys + yx, ys, 1, 16, true, me, interior, hev_t);
                filter_edge(&mut f.u, cy * us + cx, us, 1, 8, true, me, interior, hev_t);
                filter_edge(&mut f.v, cy * us + cx, us, 1, 8, true, me, interior, hev_t);
            }
            if mi.inner {
                for k in [4, 8, 12] {
                    filter_edge(
                        &mut f.y,
                        (yy + k) * ys + yx,
                        ys,
                        1,
                        16,
                        false,
                        level,
                        interior,
                        hev_t,
                    );
                }
                filter_edge(
                    &mut f.u,
                    (cy + 4) * us + cx,
                    us,
                    1,
                    8,
                    false,
                    level,
                    interior,
                    hev_t,
                );
                filter_edge(
                    &mut f.v,
                    (cy + 4) * us + cx,
                    us,
                    1,
                    8,
                    false,
                    level,
                    interior,
                    hev_t,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idct_dc_only_matches_full() {
        let mut c = [0i16; 16];
        c[0] = 100;
        let mut a = [50u8; 16];
        idct_add(&c, &mut a, 0, 4);
        assert!(a.iter().all(|&v| v == 50 + 13));
    }

    #[test]
    fn wht_dc() {
        let mut c = [0i16; 16];
        c[0] = 80;
        let o = inverse_wht(&c);
        assert!(o.iter().all(|&v| v == 10));
    }
}
