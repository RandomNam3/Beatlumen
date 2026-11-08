//! Transform blocks of intra blocks: prediction (intra, palette, chroma
//! from luma), coefficient reading and reconstruction (AV1 specification
//! sections 5.11.34 to 5.11.39, 7.11.2, 7.11.4, 7.11.5 and 7.12).

use super::decoder::*;
use super::predict::{self, Directional, Edges, EDGE_LEN, EDGE_OFF, UV_CFL_PRED};
use super::qm_tables::{QM_32X16, QM_32X32};
use super::scan_tables::*;
use super::tables::*;
use super::transform::{self, H_ADST, H_DCT, H_FLIPADST, IDTX, V_ADST, V_DCT, V_FLIPADST};
use crate::Error;

const TX_CLASS_2D: usize = 0;
const TX_CLASS_HORIZ: usize = 1;
const TX_CLASS_VERT: usize = 2;

fn tx_class(t: u8) -> usize {
    match t {
        V_DCT | V_ADST | V_FLIPADST => TX_CLASS_VERT,
        H_DCT | H_ADST | H_FLIPADST => TX_CLASS_HORIZ,
        _ => TX_CLASS_2D,
    }
}

fn mrow_scan(tx: usize) -> &'static [u16] {
    match tx {
        0 => &MROW_SCAN_4X4,
        5 => &MROW_SCAN_4X8,
        6 => &MROW_SCAN_8X4,
        1 => &MROW_SCAN_8X8,
        7 => &MROW_SCAN_8X16,
        8 => &MROW_SCAN_16X8,
        2 => &MROW_SCAN_16X16,
        13 => &MROW_SCAN_4X16,
        _ => &MROW_SCAN_16X4,
    }
}

fn mcol_scan(tx: usize) -> &'static [u16] {
    match tx {
        0 => &MCOL_SCAN_4X4,
        5 => &MCOL_SCAN_4X8,
        6 => &MCOL_SCAN_8X4,
        1 => &MCOL_SCAN_8X8,
        7 => &MCOL_SCAN_8X16,
        8 => &MCOL_SCAN_16X8,
        2 => &MCOL_SCAN_16X16,
        13 => &MCOL_SCAN_4X16,
        _ => &MCOL_SCAN_16X4,
    }
}

fn default_scan(tx: usize) -> &'static [u16] {
    match tx {
        0 => &DEFAULT_SCAN_4X4,
        5 => &DEFAULT_SCAN_4X8,
        6 => &DEFAULT_SCAN_8X4,
        1 => &DEFAULT_SCAN_8X8,
        7 => &DEFAULT_SCAN_8X16,
        8 => &DEFAULT_SCAN_16X8,
        2 => &DEFAULT_SCAN_16X16,
        9 => &DEFAULT_SCAN_16X32,
        10 => &DEFAULT_SCAN_32X16,
        13 => &DEFAULT_SCAN_4X16,
        14 => &DEFAULT_SCAN_16X4,
        15 => &DEFAULT_SCAN_8X32,
        16 => &DEFAULT_SCAN_32X8,
        _ => &DEFAULT_SCAN_32X32,
    }
}

fn get_scan(tx: usize, tx_type: u8) -> &'static [u16] {
    if tx == 17 {
        return &DEFAULT_SCAN_16X32;
    }
    if tx == 18 {
        return &DEFAULT_SCAN_32X16;
    }
    if TX_SIZE_SQR_UP[tx] as usize == TX_64X64 {
        return &DEFAULT_SCAN_32X32;
    }
    if tx_type == IDTX {
        return default_scan(tx);
    }
    match tx_class(tx_type) {
        TX_CLASS_VERT => mrow_scan(tx),
        TX_CLASS_HORIZ => mcol_scan(tx),
        _ => default_scan(tx),
    }
}

/// Transform set for intra blocks: 0 = DCT only, 1 = TX_SET_INTRA_1,
/// 2 = TX_SET_INTRA_2.
fn tx_set_intra(tx: usize, reduced: bool) -> usize {
    let sqr_up = TX_SIZE_SQR_UP[tx] as usize;
    if sqr_up > TX_32X32 || sqr_up == TX_32X32 {
        0
    } else if reduced || TX_SIZE_SQR[tx] as usize == TX_16X16 {
        2
    } else {
        1
    }
}

impl Decoder {
    fn plane_tx_size(&self, b: &Blk, plane: usize) -> usize {
        if b.lossless {
            return TX_4X4;
        }
        if plane == 0 {
            return b.tx_size;
        }
        let uv = MAX_TX_SIZE_RECT[self.residual_size(b.size, plane)] as usize;
        if TX_WIDTH[uv] == 64 || TX_HEIGHT[uv] == 64 {
            if TX_WIDTH[uv] == 16 {
                return TX_16X32;
            }
            if TX_HEIGHT[uv] == 16 {
                return TX_32X16;
            }
            return TX_32X32;
        }
        uv
    }

    pub(super) fn residual(&mut self, t: &mut Tile, b: &mut Blk) -> Result<(), Error> {
        let wc = (block_width(b.size) >> 6).max(1);
        let hc = (block_height(b.size) >> 6).max(1);
        let size_chunk = if wc > 1 || hc > 1 {
            BLOCK_64X64
        } else {
            b.size
        };
        for cy in 0..hc {
            for cx in 0..wc {
                let planes = if b.has_chroma { 3 } else { 1 };
                for plane in 0..planes {
                    let tx = self.plane_tx_size(b, plane);
                    let step_x = (TX_WIDTH[tx] >> 2) as usize;
                    let step_y = (TX_HEIGHT[tx] >> 2) as usize;
                    let psz = self.residual_size(size_chunk, plane);
                    let n4w = NUM_4X4_BLOCKS_WIDE[psz] as usize;
                    let n4h = NUM_4X4_BLOCKS_HIGH[psz] as usize;
                    let (sx, sy) = if plane > 0 {
                        (self.ssx, self.ssy)
                    } else {
                        (0, 0)
                    };
                    let base_x = (b.mi_col >> sx) * 4;
                    let base_y = (b.mi_row >> sy) * 4;
                    let mut y = 0;
                    while y < n4h {
                        let mut x = 0;
                        while x < n4w {
                            self.transform_block(
                                t,
                                b,
                                plane,
                                base_x,
                                base_y,
                                tx,
                                x + ((cx << 4) >> sx),
                                y + ((cy << 4) >> sy),
                            )?;
                            x += step_x;
                        }
                        y += step_y;
                    }
                }
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_block(
        &mut self,
        t: &mut Tile,
        b: &mut Blk,
        plane: usize,
        base_x: usize,
        base_y: usize,
        tx: usize,
        x: usize,
        y: usize,
    ) -> Result<(), Error> {
        let start_x = base_x + 4 * x;
        let start_y = base_y + 4 * y;
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        let row = (start_y << sy) >> 2;
        let col = (start_x << sx) >> 2;
        let sb_mask = self.sb4 - 1;
        let sub_row = (row & sb_mask) >> sy;
        let sub_col = (col & sb_mask) >> sx;
        let step_x = (TX_WIDTH[tx] >> 2) as usize;
        let step_y = (TX_HEIGHT[tx] >> 2) as usize;
        let max_x = (self.mi_cols * 4) >> sx;
        let max_y = (self.mi_rows * 4) >> sy;
        if start_x >= max_x || start_y >= max_y {
            return Ok(());
        }
        if !b.is_inter {
            let palette = if plane == 0 {
                b.pal_size_y > 0
            } else {
                b.pal_size_uv > 0
            };
            if palette {
                self.predict_palette(b, plane, start_x, start_y, x, y, tx);
            } else {
                let is_cfl = plane > 0 && b.uv_mode == UV_CFL_PRED;
                let mode = if plane == 0 {
                    b.y_mode
                } else if is_cfl {
                    predict::DC_PRED
                } else {
                    b.uv_mode
                };
                let have_left = (if plane == 0 {
                    b.avail_l
                } else {
                    b.avail_l_chroma
                }) || x > 0;
                let have_above = (if plane == 0 {
                    b.avail_u
                } else {
                    b.avail_u_chroma
                }) || y > 0;
                let (si, sc) = (sub_row as isize, sub_col as isize);
                let have_ar = self.block_decoded(plane, si - 1, sc + step_x as isize);
                let have_bl = self.block_decoded(plane, si + step_y as isize, sc - 1);
                self.predict_intra(
                    b,
                    plane,
                    start_x,
                    start_y,
                    have_left,
                    have_above,
                    have_ar,
                    have_bl,
                    mode,
                    TX_WIDTH_LOG2[tx] as u32,
                    TX_HEIGHT_LOG2[tx] as u32,
                );
                if is_cfl {
                    self.predict_cfl(b, plane, start_x, start_y, tx);
                }
            }
            if plane == 0 {
                b.max_luma_w = start_x + step_x * 4;
                b.max_luma_h = start_y + step_y * 4;
            }
        }
        if !b.skip {
            let (eob, tx_type) = self.coeffs(t, b, plane, start_x, start_y, tx);
            if eob > 0 {
                self.reconstruct(b, plane, start_x, start_y, tx, tx_type, eob);
            }
        }
        for i in 0..step_y {
            for j in 0..step_x {
                let ry = ((row >> sy) + i) << sy;
                let cx = ((col >> sx) + j) << sx;
                if ry < self.mi_rows && cx < self.mi_cols {
                    let k = self.mi_idx(ry, cx);
                    if plane == 0 {
                        self.mi[k].lf_tx_y = tx as u8;
                    } else {
                        self.mi[k].lf_tx_uv = tx as u8;
                    }
                }
                self.set_block_decoded(plane, (sub_row + i) as isize, (sub_col + j) as isize);
            }
        }
        Ok(())
    }

    // ---- prediction -------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn predict_intra(
        &mut self,
        b: &Blk,
        plane: usize,
        x: usize,
        y: usize,
        have_left: bool,
        have_above: bool,
        have_ar: bool,
        have_bl: bool,
        mode: u8,
        log2w: u32,
        log2h: u32,
    ) {
        let (w, h) = (1usize << log2w, 1usize << log2h);
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        let max_x = ((self.mi_cols * 4) >> sx) - 1;
        let max_y = ((self.mi_rows * 4) >> sy) - 1;
        let bd = self.bd;
        let mut e = Edges {
            above: [0; EDGE_LEN],
            left: [0; EDGE_LEN],
        };
        let a = EDGE_OFF;
        let f = &self.frame;
        let n = w + h;
        if !have_above && have_left {
            let v = f.px(plane, x - 1, y) as i32;
            e.above[a..a + n].fill(v);
        } else if !have_above && !have_left {
            e.above[a..a + n].fill((1 << (bd - 1)) - 1);
        } else {
            let reach = if have_ar { 2 * w } else { w };
            let limit = max_x.min(x + reach - 1);
            for i in 0..n {
                e.above[a + i] = f.px(plane, (x + i).min(limit), y - 1) as i32;
            }
        }
        if !have_left && have_above {
            let v = f.px(plane, x, y - 1) as i32;
            e.left[a..a + n].fill(v);
        } else if !have_left && !have_above {
            e.left[a..a + n].fill((1 << (bd - 1)) + 1);
        } else {
            let reach = if have_bl { 2 * h } else { h };
            let limit = max_y.min(y + reach - 1);
            for i in 0..n {
                e.left[a + i] = f.px(plane, x - 1, (y + i).min(limit)) as i32;
            }
        }
        let corner = if have_above && have_left {
            f.px(plane, x - 1, y - 1) as i32
        } else if have_above {
            f.px(plane, x, y - 1) as i32
        } else if have_left {
            f.px(plane, x - 1, y) as i32
        } else {
            1 << (bd - 1)
        };
        e.above[a - 1] = corner;
        e.left[a - 1] = corner;
        let filter_intra = if plane == 0 && b.use_filter_intra {
            Some(b.filter_intra_mode)
        } else {
            None
        };
        let dir = Directional {
            angle_delta: if plane == 0 {
                b.angle_delta_y
            } else {
                b.angle_delta_uv
            },
            enable_edge_filter: self.seq.enable_intra_edge_filter,
            filter_type: if predict::is_directional(mode) && filter_intra.is_none() {
                self.filter_type(b, plane)
            } else {
                false
            },
            have_left,
            have_above,
            room_x: max_x as i32 - x as i32 + 1,
            room_y: max_y as i32 - y as i32 + 1,
            bit_depth: bd,
        };
        let mut pred = std::mem::take(&mut self.scratch);
        predict::predict(
            &mut e,
            &mut pred,
            mode,
            log2w,
            log2h,
            have_left,
            have_above,
            filter_intra,
            &dir,
        );
        for i in 0..h {
            let row = self.frame.row_mut(plane, y + i);
            for j in 0..w {
                row[x + j] = pred[i * w + j] as u16;
            }
        }
        self.scratch = pred;
    }

    /// get_filter_type(): whether the block above or to the left uses a
    /// smooth mode.
    fn filter_type(&self, b: &Blk, plane: usize) -> bool {
        let is_smooth = |r: usize, c: usize| -> bool {
            let m = self.mi_at(r, c);
            let mode = if plane == 0 {
                m.y_mode
            } else {
                if m.is_inter {
                    return false;
                }
                m.uv_mode
            };
            (predict::SMOOTH_PRED..=predict::SMOOTH_H_PRED).contains(&mode)
        };
        let (mut above, mut left) = (false, false);
        let avail_u = if plane == 0 {
            b.avail_u
        } else {
            b.avail_u_chroma
        };
        let avail_l = if plane == 0 {
            b.avail_l
        } else {
            b.avail_l_chroma
        };
        if avail_u {
            let mut r = b.mi_row - 1;
            let mut c = b.mi_col;
            if plane > 0 {
                if self.ssx == 1 && b.mi_col & 1 == 0 {
                    c += 1;
                }
                if self.ssy == 1 && b.mi_row & 1 == 1 {
                    r -= 1;
                }
            }
            above = is_smooth(r, c);
        }
        if avail_l {
            let mut r = b.mi_row;
            let mut c = b.mi_col - 1;
            if plane > 0 {
                if self.ssx == 1 && b.mi_col & 1 == 1 {
                    c -= 1;
                }
                if self.ssy == 1 && b.mi_row & 1 == 0 {
                    r += 1;
                }
            }
            left = is_smooth(r, c);
        }
        above || left
    }

    #[allow(clippy::too_many_arguments)]
    fn predict_palette(
        &mut self,
        b: &Blk,
        plane: usize,
        sx: usize,
        sy: usize,
        x: usize,
        y: usize,
        tx: usize,
    ) {
        let w = TX_WIDTH[tx] as usize;
        let h = TX_HEIGHT[tx] as usize;
        let (pal, map, stride) = match plane {
            0 => (&b.pal_y, &self.color_map_y, 64),
            1 => (&b.pal_u, &self.color_map_uv, 66),
            _ => (&b.pal_v, &self.color_map_uv, 66),
        };
        let mut vals = [0u16; 64];
        for i in 0..h {
            for j in 0..w {
                vals[j] = pal[map[(y * 4 + i) * stride + x * 4 + j] as usize];
            }
            let row = self.frame.row_mut(plane, sy + i);
            row[sx..sx + w].copy_from_slice(&vals[..w]);
        }
    }

    fn predict_cfl(&mut self, b: &Blk, plane: usize, sx: usize, sy: usize, tx: usize) {
        let w = TX_WIDTH[tx] as usize;
        let h = TX_HEIGHT[tx] as usize;
        let (ssx, ssy) = (self.ssx, self.ssy);
        let alpha = if plane == 1 {
            b.cfl_alpha_u
        } else {
            b.cfl_alpha_v
        };
        let mut l = std::mem::take(&mut self.scratch);
        let mut sum: i64 = 0;
        for i in 0..h {
            let ly = ((sy + i) << ssy).min(b.max_luma_h - (1 << ssy));
            for j in 0..w {
                let lx = ((sx + j) << ssx).min(b.max_luma_w - (1 << ssx));
                let mut t = 0i32;
                for dy in 0..=ssy {
                    let row = self.frame.row(0, ly + dy);
                    for dx in 0..=ssx {
                        t += row[lx + dx] as i32;
                    }
                }
                let v = t << (3 - ssx - ssy);
                l[i * w + j] = v;
                sum += v as i64;
            }
        }
        let n = TX_WIDTH_LOG2[tx] as u32 + TX_HEIGHT_LOG2[tx] as u32;
        let avg = ((sum + (1 << (n - 1))) >> n) as i32;
        let max = (1i32 << self.bd) - 1;
        for i in 0..h {
            let row = self.frame.row_mut(plane, sy + i);
            for j in 0..w {
                let dc = row[sx + j] as i32;
                let s = alpha * (l[i * w + j] - avg);
                let scaled = if s >= 0 {
                    (s + 32) >> 6
                } else {
                    -((-s + 32) >> 6)
                };
                row[sx + j] = (dc + scaled).clamp(0, max) as u16;
            }
        }
        self.scratch = l;
    }

    // ---- coefficients -----------------------------------------------------

    fn compute_tx_type(&self, b: &Blk, plane: usize, tx: usize, x4: usize, y4: usize) -> u8 {
        let sqr_up = TX_SIZE_SQR_UP[tx] as usize;
        if b.lossless || sqr_up > TX_32X32 {
            return transform::DCT_DCT;
        }
        let set = tx_set_intra(tx, self.fh.reduced_tx_set);
        if plane == 0 {
            let lx = x4 - b.mi_col;
            let ly = y4 - b.mi_row;
            return b.tx_types[ly * 32 + lx];
        }
        let t = MODE_TO_TXFM[b.uv_mode as usize];
        if TX_TYPE_IN_SET_INTRA[set][t as usize] == 0 {
            return transform::DCT_DCT;
        }
        t
    }

    /// Reads the coefficients of a transform block into `self.quant`.
    /// Returns eob and the transform type.
    fn coeffs(
        &mut self,
        t: &mut Tile,
        b: &mut Blk,
        plane: usize,
        start_x: usize,
        start_y: usize,
        tx: usize,
    ) -> (usize, u8) {
        let x4 = start_x >> 2;
        let y4 = start_y >> 2;
        let w4 = (TX_WIDTH[tx] >> 2) as usize;
        let h4 = (TX_HEIGHT[tx] >> 2) as usize;
        let tx_ctx = ((TX_SIZE_SQR[tx] + TX_SIZE_SQR_UP[tx] + 1) >> 1) as usize;
        let ptype = (plane > 0) as usize;
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        let max_x4 = self.mi_cols >> sx;
        let max_y4 = self.mi_rows >> sy;
        let w = TX_WIDTH[tx] as usize;
        let h = TX_HEIGHT[tx] as usize;
        // all_zero context
        let bsize = self.residual_size(b.size, plane);
        let (bw, bh) = (block_width(bsize), block_height(bsize));
        let ctx = if plane == 0 {
            let mut top = 0u8;
            let mut left = 0u8;
            for k in 0..w4 {
                if x4 + k < max_x4 {
                    top = top.max(self.above_level[0][x4 + k]);
                }
            }
            for k in 0..h4 {
                if y4 + k < max_y4 {
                    left = left.max(t.left_level[0][(y4 + k) & 31]);
                }
            }
            let (top, left) = (top as usize, left as usize);
            if bw == w && bh == h {
                0
            } else if top == 0 && left == 0 {
                1
            } else if top == 0 || left == 0 {
                2 + (top.max(left) > 3) as usize
            } else if top.max(left) <= 3 {
                4
            } else if top.min(left) <= 3 {
                5
            } else {
                6
            }
        } else {
            let mut above = 0u8;
            let mut left = 0u8;
            for i in 0..w4 {
                if x4 + i < max_x4 {
                    above |= self.above_level[plane][x4 + i] | self.above_dc[plane][x4 + i];
                }
            }
            for i in 0..h4 {
                if y4 + i < max_y4 {
                    left |= t.left_level[plane][(y4 + i) & 31] | t.left_dc[plane][(y4 + i) & 31];
                }
            }
            let mut c = 7 + (above != 0) as usize + (left != 0) as usize;
            if bw * bh > w * h {
                c += 3;
            }
            c
        };
        let all_zero = t.sd.symbol(&mut t.cdf.txb_skip[tx_ctx][ctx]) == 1;
        let mut eob = 0;
        let mut cul_level = 0u32;
        let mut dc_category = 0u8;
        let mut tx_type = transform::DCT_DCT;
        if all_zero {
            if plane == 0 {
                for i in 0..w4 {
                    for j in 0..h4 {
                        let (lx, ly) = (x4 + i - b.mi_col, y4 + j - b.mi_row);
                        if lx < 32 && ly < 32 {
                            b.tx_types[ly * 32 + lx] = transform::DCT_DCT;
                        }
                    }
                }
            }
        } else {
            if plane == 0 {
                self.transform_type(t, b, x4, y4, tx);
            }
            tx_type = self.compute_tx_type(b, plane, tx, x4, y4);
            let scan = get_scan(tx, tx_type);
            let class = tx_class(tx_type);
            let eob_multisize = (TX_WIDTH_LOG2[tx].min(5) + TX_HEIGHT_LOG2[tx].min(5)) as usize - 4;
            let ectx = (class != TX_CLASS_2D) as usize;
            let cdf = &mut t.cdf;
            let eob_pt = 1 + match eob_multisize {
                0 => t.sd.symbol(&mut cdf.eob_pt16[ptype][ectx]),
                1 => t.sd.symbol(&mut cdf.eob_pt32[ptype][ectx]),
                2 => t.sd.symbol(&mut cdf.eob_pt64[ptype][ectx]),
                3 => t.sd.symbol(&mut cdf.eob_pt128[ptype][ectx]),
                4 => t.sd.symbol(&mut cdf.eob_pt256[ptype][ectx]),
                5 => t.sd.symbol(&mut cdf.eob_pt512[ptype]),
                _ => t.sd.symbol(&mut cdf.eob_pt1024[ptype]),
            };
            eob = if eob_pt < 2 {
                eob_pt
            } else {
                (1 << (eob_pt - 2)) + 1
            };
            let eob_shift = eob_pt as i32 - 3;
            if eob_shift >= 0 {
                if t.sd.symbol(&mut t.cdf.eob_extra[tx_ctx][ptype][eob_pt - 3]) == 1 {
                    eob += 1 << eob_shift;
                }
                for i in 1..(eob_pt as i32 - 2).max(0) {
                    let sh = (eob_pt as i32 - 2).max(0) - 1 - i;
                    if t.sd.literal(1) == 1 {
                        eob += 1 << sh;
                    }
                }
            }
            let adj = ADJUSTED_TX_SIZE[tx] as usize;
            let bwl = TX_WIDTH_LOG2[adj] as usize;
            let width = 1usize << bwl;
            let height = TX_HEIGHT[adj] as usize;
            let quant = &mut self.quant;
            quant[..width * height].fill(0);
            for c in (0..eob).rev() {
                let pos = scan[c] as usize;
                let mut level;
                if c == eob - 1 {
                    let ctx = if c == 0 {
                        0
                    } else if c <= (height << bwl) / 8 {
                        1
                    } else if c <= (height << bwl) / 4 {
                        2
                    } else {
                        3
                    };
                    level = t.sd.symbol(&mut t.cdf.coeff_base_eob[tx_ctx][ptype][ctx]) + 1;
                } else {
                    let row = pos >> bwl;
                    let col = pos - (row << bwl);
                    let mut mag = 0;
                    for o in &SIG_REF_DIFF_OFFSET[class] {
                        let rr = row + o[0] as usize;
                        let cc = col + o[1] as usize;
                        if rr < height && cc < width {
                            mag += quant[(rr << bwl) + cc].min(3);
                        }
                    }
                    let mut ctx = ((mag + 1) >> 1).min(4) as usize;
                    if class == TX_CLASS_2D {
                        if row == 0 && col == 0 {
                            ctx = 0;
                        } else {
                            ctx += COEFF_BASE_CTX_OFFSET[tx][row.min(4)][col.min(4)] as usize;
                        }
                    } else {
                        let idx = if class == TX_CLASS_VERT { row } else { col };
                        ctx += COEFF_BASE_POS_CTX_OFFSET[idx.min(2)] as usize;
                    }
                    level = t.sd.symbol(&mut t.cdf.coeff_base[tx_ctx][ptype][ctx]);
                }
                if level > 2 {
                    // coeff_br context
                    let row = pos >> bwl;
                    let col = pos - (row << bwl);
                    let mut mag = 0;
                    for o in &MAG_REF_OFFSET_WITH_TX_CLASS[class] {
                        let rr = row + o[0] as usize;
                        let cc = col + o[1] as usize;
                        if rr < height && cc < width {
                            mag += quant[rr * width + cc].min(15);
                        }
                    }
                    let mag = ((mag + 1) >> 1).min(6) as usize;
                    let ctx = if pos == 0 {
                        mag
                    } else if class == TX_CLASS_2D {
                        if row < 2 && col < 2 {
                            mag + 7
                        } else {
                            mag + 14
                        }
                    } else if class == TX_CLASS_HORIZ {
                        if col == 0 {
                            mag + 7
                        } else {
                            mag + 14
                        }
                    } else if row == 0 {
                        mag + 7
                    } else {
                        mag + 14
                    };
                    let br_tx = tx_ctx.min(TX_32X32);
                    for _ in 0..4 {
                        let br = t.sd.symbol(&mut t.cdf.coeff_br[br_tx][ptype][ctx]);
                        level += br;
                        if br < 3 {
                            break;
                        }
                    }
                }
                quant[pos] = level as i32;
            }
            for c in 0..eob {
                let pos = scan[c] as usize;
                let mut sign = false;
                if self.quant[pos] != 0 {
                    if c == 0 {
                        let mut dc_sign = 0i32;
                        for k in 0..w4 {
                            if x4 + k < max_x4 {
                                match self.above_dc[plane][x4 + k] {
                                    1 => dc_sign -= 1,
                                    2 => dc_sign += 1,
                                    _ => {}
                                }
                            }
                        }
                        for k in 0..h4 {
                            if y4 + k < max_y4 {
                                match t.left_dc[plane][(y4 + k) & 31] {
                                    1 => dc_sign -= 1,
                                    2 => dc_sign += 1,
                                    _ => {}
                                }
                            }
                        }
                        let ctx = if dc_sign < 0 {
                            1
                        } else if dc_sign > 0 {
                            2
                        } else {
                            0
                        };
                        sign = t.sd.symbol(&mut t.cdf.dc_sign[ptype][ctx]) == 1;
                    } else {
                        sign = t.sd.literal(1) == 1;
                    }
                }
                let quant = &mut self.quant;
                if quant[pos] > 14 {
                    let mut length = 0;
                    loop {
                        length += 1;
                        if t.sd.literal(1) == 1 || length >= 32 {
                            break;
                        }
                    }
                    let mut x: u64 = 1;
                    for _ in 0..length - 1 {
                        x = (x << 1) | t.sd.literal(1) as u64;
                    }
                    quant[pos] = (x + 14).min(i32::MAX as u64) as i32;
                }
                if pos == 0 && quant[pos] > 0 {
                    dc_category = if sign { 1 } else { 2 };
                }
                quant[pos] &= 0xfffff;
                cul_level += quant[pos] as u32;
                if sign {
                    quant[pos] = -quant[pos];
                }
            }
            cul_level = cul_level.min(63);
        }
        for i in 0..w4 {
            if x4 + i < self.above_level[plane].len() {
                self.above_level[plane][x4 + i] = cul_level as u8;
                self.above_dc[plane][x4 + i] = dc_category;
            }
        }
        for i in 0..h4 {
            t.left_level[plane][(y4 + i) & 31] = cul_level as u8;
            t.left_dc[plane][(y4 + i) & 31] = dc_category;
        }
        (eob, tx_type)
    }

    fn transform_type(&mut self, t: &mut Tile, b: &mut Blk, x4: usize, y4: usize, tx: usize) {
        let set = tx_set_intra(tx, self.fh.reduced_tx_set);
        let q = if self.fh.segmentation_enabled {
            self.fh.qindex(true, b.segment_id, t.current_q)
        } else {
            self.fh.base_q_idx
        };
        let mut tx_type = transform::DCT_DCT;
        if set > 0 && q > 0 {
            let dir = if b.use_filter_intra {
                FILTER_INTRA_MODE_TO_INTRA_DIR[b.filter_intra_mode as usize]
            } else {
                b.y_mode
            } as usize;
            let sqr = TX_SIZE_SQR[tx] as usize;
            tx_type = if set == 1 {
                TX_TYPE_INTRA_INV_SET1[t.sd.symbol(&mut t.cdf.intra_tx_set1[sqr][dir])]
            } else {
                TX_TYPE_INTRA_INV_SET2[t.sd.symbol(&mut t.cdf.intra_tx_set2[sqr][dir])]
            };
        }
        let w4 = (TX_WIDTH[tx] >> 2) as usize;
        let h4 = (TX_HEIGHT[tx] >> 2) as usize;
        for i in 0..w4 {
            for j in 0..h4 {
                let (lx, ly) = (x4 + i - b.mi_col, y4 + j - b.mi_row);
                if lx < 32 && ly < 32 {
                    b.tx_types[ly * 32 + lx] = tx_type;
                }
            }
        }
    }

    // ---- reconstruction ---------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn reconstruct(
        &mut self,
        b: &Blk,
        plane: usize,
        x: usize,
        y: usize,
        tx: usize,
        tx_type: u8,
        eob: usize,
    ) {
        let dq_denom = match tx {
            3 | 9 | 10 | 17 | 18 => 2,
            4 | 11 | 12 => 4,
            _ => 1,
        };
        let log2w = TX_WIDTH_LOG2[tx] as u32;
        let log2h = TX_HEIGHT_LOG2[tx] as u32;
        let (w, h) = (1usize << log2w, 1usize << log2h);
        let tw = w.min(32);
        let th = h.min(32);
        let bd = self.bd;
        let q = self.fh.qindex(false, b.segment_id, b.cur_q);
        let qidx = (bd as usize - 8) >> 1;
        let (dc_delta, ac_delta) = match plane {
            0 => (self.fh.delta_q_y_dc, 0),
            1 => (self.fh.delta_q_u_dc, self.fh.delta_q_u_ac),
            _ => (self.fh.delta_q_v_dc, self.fh.delta_q_v_ac),
        };
        let dc_q = DC_QLOOKUP[qidx][(q + dc_delta).clamp(0, 255) as usize] as i64;
        let ac_q = AC_QLOOKUP[qidx][(q + ac_delta).clamp(0, 255) as usize] as i64;
        let qm_level = if self.fh.using_qmatrix {
            self.fh.seg_qm_level[plane][b.segment_id]
        } else {
            15
        };
        let use_qm = self.fh.using_qmatrix && tx_type < IDTX && qm_level < 15;
        let lim = 1i64 << (7 + bd);
        let mut coef = std::mem::take(&mut self.scratch);
        // Rows that hold coefficients: the last scan position bounds them.
        let mut rows_nz = 0;
        for i in 0..th {
            for j in 0..tw {
                let qv = self.quant[i * tw + j] as i64;
                if qv == 0 {
                    coef[i * tw + j] = 0;
                    continue;
                }
                rows_nz = i + 1;
                let mut qq = if i == 0 && j == 0 { dc_q } else { ac_q };
                if use_qm {
                    let m = qm_value(qm_level as usize, (plane > 0) as usize, tw, th, i, j) as i64;
                    qq = (qq * m + 16) >> 5;
                }
                let dq = qv * qq;
                let dq2 = if dq < 0 {
                    -((-dq & 0xffffff) / dq_denom)
                } else {
                    (dq & 0xffffff) / dq_denom
                };
                coef[i * tw + j] = dq2.clamp(-lim, lim - 1) as i32;
            }
        }
        let _ = eob;
        let mut res = std::mem::take(&mut self.residual);
        transform::inverse_2d(&coef, &mut res, tx, tx_type, b.lossless, bd, rows_nz);
        let (flip_ud, flip_lr) = transform::flips(tx_type);
        let max = (1i32 << bd) - 1;
        for i in 0..h {
            let yy = if flip_ud { h - i - 1 } else { i };
            let row = self.frame.row_mut(plane, y + yy);
            for j in 0..w {
                let xx = if flip_lr { w - j - 1 } else { j };
                let v = row[x + xx] as i32 + res[i * w + j];
                row[x + xx] = v.clamp(0, max) as u16;
            }
        }
        self.residual = res;
        self.scratch = coef;
    }
}

/// Quantizer matrix value for a (tw x th) transform (tw, th <= 32), derived
/// from the fundamental matrices.
fn qm_value(level: usize, ptype: usize, tw: usize, th: usize, i: usize, j: usize) -> u8 {
    if tw == th {
        let r = 32 / tw;
        let ph = (r + 1) / 2 - 1;
        QM_32X32[level][ptype][(r * i + ph) * 32 + r * j + ph]
    } else if tw > th {
        let rw = 32 / tw;
        let rh = 16 / th;
        let (pw, ph) = ((rw + 1) / 2 - 1, (rh + 1) / 2 - 1);
        QM_32X16[level][ptype][(rh * i + ph) * 32 + rw * j + pw]
    } else {
        // 16x32: transpose of 32x16.
        let rw = 16 / tw;
        let rh = 32 / th;
        let (pw, ph) = ((rw + 1) / 2 - 1, (rh + 1) / 2 - 1);
        let (fi, fj) = (rh * i + ph, rw * j + pw);
        QM_32X16[level][ptype][fj * 32 + fi]
    }
}
