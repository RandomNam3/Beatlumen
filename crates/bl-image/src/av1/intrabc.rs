//! Intra block copy (AV1 specification sections 5.11.26, 5.11.31, 5.11.32,
//! 7.10.2 and 7.11.3 as far as they apply to intra frames): the spatial
//! motion vector candidates, motion vector reading and the bilinear copy
//! from already decoded, unfiltered samples of the frame.

use super::decoder::{block_height, block_width, Blk, Decoder, Tile, TX_4X4};
use super::tables::*;

const REF_CAT_LEVEL: i32 = 640;
const MV_BORDER: i32 = 128;

/// Candidate motion vectors with their weights.
struct Stack {
    mv: [[i32; 2]; 8],
    weight: [i32; 8],
    n: usize,
}

impl Stack {
    fn add(&mut self, mv: [i32; 2], weight: i32) {
        for i in 0..self.n {
            if self.mv[i] == mv {
                self.weight[i] += weight;
                return;
            }
        }
        if self.n < 8 {
            self.mv[self.n] = mv;
            self.weight[self.n] = weight;
            self.n += 1;
        }
    }

    fn sort(&mut self, start: usize, mut end: usize) {
        while end > start {
            let mut new_end = start;
            for i in start + 1..end {
                if self.weight[i - 1] < self.weight[i] {
                    self.weight.swap(i - 1, i);
                    self.mv.swap(i - 1, i);
                    new_end = i;
                }
            }
            end = new_end;
        }
    }
}

impl Decoder {
    fn inside(t: &Tile, r: isize, c: isize) -> bool {
        c >= t.mi_col_start as isize
            && c < t.mi_col_end as isize
            && r >= t.mi_row_start as isize
            && r < t.mi_row_end as isize
    }

    /// add_ref_mv_candidate for single prediction from INTRA_FRAME.
    fn add_candidate(&self, s: &mut Stack, r: usize, c: usize, weight: i32) -> bool {
        let m = self.mi_at(r, c);
        if !m.is_inter {
            return false;
        }
        // lower_mv_precision with force_integer_mv (intrabc vectors are
        // whole samples already).
        let mut mv = [m.mv[0] as i32, m.mv[1] as i32];
        for v in mv.iter_mut() {
            let a = (v.abs() + 3) >> 3;
            *v = if *v > 0 { a << 3 } else { -(a << 3) };
        }
        s.add(mv, weight);
        true
    }

    fn scan_row(&self, t: &Tile, b: &Blk, s: &mut Stack, mut delta_row: isize) -> bool {
        let bw4 = b.bw4;
        let end4 = bw4.min(self.mi_cols - b.mi_col).min(16);
        let mut delta_col: isize = 0;
        let step16 = bw4 >= 16;
        if delta_row.abs() > 1 {
            delta_row += (b.mi_row & 1) as isize;
            delta_col = 1 - (b.mi_col & 1) as isize;
        }
        let mut found = false;
        let mut i = 0;
        while i < end4 {
            let r = b.mi_row as isize + delta_row;
            let c = b.mi_col as isize + delta_col + i as isize;
            if !Self::inside(t, r, c) {
                break;
            }
            let (r, c) = (r as usize, c as usize);
            let mut len = bw4.min(NUM_4X4_BLOCKS_WIDE[self.mi_at(r, c).bsize as usize] as usize);
            if delta_row.abs() > 1 {
                len = len.max(2);
            }
            if step16 {
                len = len.max(4);
            }
            found |= self.add_candidate(s, r, c, 2 * len as i32);
            i += len;
        }
        found
    }

    fn scan_col(&self, t: &Tile, b: &Blk, s: &mut Stack, mut delta_col: isize) -> bool {
        let bh4 = b.bh4;
        let end4 = bh4.min(self.mi_rows - b.mi_row).min(16);
        let mut delta_row: isize = 0;
        let step16 = bh4 >= 16;
        if delta_col.abs() > 1 {
            delta_row = 1 - (b.mi_row & 1) as isize;
            delta_col += (b.mi_col & 1) as isize;
        }
        let mut found = false;
        let mut i = 0;
        while i < end4 {
            let r = b.mi_row as isize + delta_row + i as isize;
            let c = b.mi_col as isize + delta_col;
            if !Self::inside(t, r, c) {
                break;
            }
            let (r, c) = (r as usize, c as usize);
            let mut len = bh4.min(NUM_4X4_BLOCKS_HIGH[self.mi_at(r, c).bsize as usize] as usize);
            if delta_col.abs() > 1 {
                len = len.max(2);
            }
            if step16 {
                len = len.max(4);
            }
            found |= self.add_candidate(s, r, c, 2 * len as i32);
            i += len;
        }
        found
    }

    fn scan_point(&self, t: &Tile, b: &Blk, s: &mut Stack, dr: isize, dc: isize) -> bool {
        let r = b.mi_row as isize + dr;
        let c = b.mi_col as isize + dc;
        if !Self::inside(t, r, c) {
            return false;
        }
        let (r, c) = (r as usize, c as usize);
        // Only locations decoded in this frame.
        if self.mi_at(r, c).row_tag != r as u32 {
            return false;
        }
        self.add_candidate(s, r, c, 4)
    }

    /// find_mv_stack( 0 ) for intra block copy: the first two candidates.
    fn intrabc_stack(&self, t: &Tile, b: &Blk) -> [[i32; 2]; 2] {
        let mut s = Stack {
            mv: [[0; 2]; 8],
            weight: [0; 8],
            n: 0,
        };
        self.scan_row(t, b, &mut s, -1);
        self.scan_col(t, b, &mut s, -1);
        if b.bw4.max(b.bh4) <= 16 {
            self.scan_point(t, b, &mut s, -1, b.bw4 as isize);
        }
        let num_nearest = s.n;
        for i in 0..num_nearest {
            s.weight[i] += REF_CAT_LEVEL;
        }
        self.scan_point(t, b, &mut s, -1, -1);
        self.scan_row(t, b, &mut s, -3);
        self.scan_col(t, b, &mut s, -3);
        if b.bh4 > 1 {
            self.scan_row(t, b, &mut s, -5);
        }
        if b.bw4 > 1 {
            self.scan_col(t, b, &mut s, -5);
        }
        s.sort(0, num_nearest);
        let n = s.n;
        s.sort(num_nearest, n);
        // Extra search finds no inter candidates in intra frames; global
        // motion is zero.
        for i in s.n..2 {
            s.mv[i] = [0, 0];
        }
        let bw = block_width(b.size) as i32;
        let bh = block_height(b.size) as i32;
        for i in 0..s.n.min(2) {
            let mv = &mut s.mv[i];
            let top = -((b.mi_row as i32 * 4) * 8);
            let bottom = ((self.mi_rows as i32 - b.bh4 as i32 - b.mi_row as i32) * 4) * 8;
            mv[0] = mv[0].clamp(top - (MV_BORDER + bh * 8), bottom + MV_BORDER + bh * 8);
            let left = -((b.mi_col as i32 * 4) * 8);
            let right = ((self.mi_cols as i32 - b.bw4 as i32 - b.mi_col as i32) * 4) * 8;
            mv[1] = mv[1].clamp(left - (MV_BORDER + bw * 8), right + MV_BORDER + bw * 8);
        }
        [s.mv[0], s.mv[1]]
    }

    fn read_mv_component(t: &mut Tile, comp: usize) -> i32 {
        let sign = t.sd.symbol(&mut t.cdf.mv_sign[comp]) == 1;
        let class = t.sd.symbol(&mut t.cdf.mv_class[comp]);
        let mag = if class == 0 {
            let bit = t.sd.symbol(&mut t.cdf.mv_class0_bit[comp]) as i32;
            // force_integer_mv: fr = 3, hp = 1.
            ((bit << 3) | (3 << 1) | 1) + 1
        } else {
            let mut d = 0i32;
            for i in 0..class {
                d |= (t.sd.symbol(&mut t.cdf.mv_bit[comp][i]) as i32) << i;
            }
            (2 << (class + 2)) + ((d << 3) | (3 << 1) | 1) + 1
        };
        if sign {
            -mag
        } else {
            mag
        }
    }

    /// assign_mv( 0 ) for intra block copy.
    pub(super) fn intrabc_mv(&mut self, t: &mut Tile, b: &mut Blk) {
        let stack = self.intrabc_stack(t, b);
        let mut pred = stack[0];
        if pred == [0, 0] {
            pred = stack[1];
        }
        if pred == [0, 0] {
            let sb4 = self.sb4 as i32;
            if (b.mi_row as i32) - sb4 < t.mi_row_start as i32 {
                pred = [0, -(sb4 * 4 + 256) * 8];
            } else {
                pred = [-(sb4 * 4 * 8), 0];
            }
        }
        let joint = t.sd.symbol(&mut t.cdf.mv_joint);
        let mut diff = [0, 0];
        if joint == 2 || joint == 3 {
            diff[0] = Self::read_mv_component(t, 0);
        }
        if joint == 1 || joint == 3 {
            diff[1] = Self::read_mv_component(t, 1);
        }
        b.mv = [pred[0] + diff[0], pred[1] + diff[1]];
    }

    /// Prediction of an intra block copy block for all its planes.
    pub(super) fn predict_intrabc(&mut self, b: &Blk) {
        let planes = if b.has_chroma { 3 } else { 1 };
        let bd = self.bd;
        let (round0, round1) = if bd == 12 { (5, 9) } else { (3, 11) };
        let max = (1i32 << bd) - 1;
        let filt = &SUBPEL_FILTERS[3];
        for plane in 0..planes {
            let (sx, sy) = if plane > 0 {
                (self.ssx, self.ssy)
            } else {
                (0, 0)
            };
            let psz = self.residual_size(b.size, plane);
            let w = 4 * NUM_4X4_BLOCKS_WIDE[psz] as usize;
            let h = 4 * NUM_4X4_BLOCKS_HIGH[psz] as usize;
            let x = (b.mi_col >> sx) * 4;
            let y = (b.mi_row >> sy) * 4;
            let start_x = ((((x as i64) << 4) + ((2 * b.mv[1] as i64) >> sx)) << 6) + 32;
            let start_y = ((((y as i64) << 4) + ((2 * b.mv[0] as i64) >> sy)) << 6) + 32;
            let last_x = (((self.mi_cols * 4) + sx) >> sx) as i64 - 1;
            let last_y = (((self.mi_rows * 4) + sy) >> sy) as i64 - 1;
            let ih = h + 7;
            let mut inter = vec![0i32; ih * w];
            let rec = &self.frame.rec[plane];
            for r in 0..ih {
                let ry = ((start_y >> 10) + r as i64 - 3).clamp(0, last_y) as usize;
                let row = rec.row(ry);
                for c in 0..w {
                    let p = start_x + 1024 * c as i64;
                    let f = &filt[((p >> 6) & 15) as usize];
                    let mut s = 0i64;
                    for (tt, &k) in f.iter().enumerate() {
                        let rx = ((p >> 10) + tt as i64 - 3).clamp(0, last_x) as usize;
                        s += k as i64 * row[rx] as i64;
                    }
                    inter[r * w + c] = ((s + (1 << (round0 - 1))) >> round0) as i32;
                }
            }
            let mut pred = vec![0u16; w * h];
            for r in 0..h {
                let p = (start_y & 1023) + 1024 * r as i64;
                let f = &filt[((p >> 6) & 15) as usize];
                let base = (p >> 10) as usize;
                for c in 0..w {
                    let mut s = 0i64;
                    for (tt, &k) in f.iter().enumerate() {
                        s += k as i64 * inter[(base + tt) * w + c] as i64;
                    }
                    let v = ((s + (1 << (round1 - 1))) >> round1) as i32;
                    pred[r * w + c] = v.clamp(0, max) as u16;
                }
            }
            for r in 0..h {
                let row = self.frame.rec[plane].row_mut(y + r);
                row[x..x + w].copy_from_slice(&pred[r * w..(r + 1) * w]);
            }
        }
    }

    /// read_var_tx_size(): the transform tree of an intra block copy block.
    pub(super) fn read_var_tx(
        &mut self,
        t: &mut Tile,
        b: &mut Blk,
        row: usize,
        col: usize,
        tx: usize,
        depth: usize,
    ) {
        if row >= self.mi_rows || col >= self.mi_cols {
            return;
        }
        let split = if tx == TX_4X4 || depth == 2 {
            false
        } else {
            let above = self.above_tx_width(b, row, col) < TX_WIDTH[tx] as usize;
            let left = self.left_tx_height(b, row, col) < TX_HEIGHT[tx] as usize;
            let size = 64.min(block_width(b.size).max(block_height(b.size)));
            let max_tx = match size {
                4 => 0,
                8 => 1,
                16 => 2,
                32 => 3,
                _ => 4,
            };
            let sqr_up = TX_SIZE_SQR_UP[tx] as usize;
            let ctx =
                (sqr_up != max_tx) as usize * 3 + (4 - max_tx) * 6 + above as usize + left as usize;
            t.sd.symbol(&mut t.cdf.txfm_split[ctx]) == 1
        };
        let w4 = (TX_WIDTH[tx] / 4) as usize;
        let h4 = (TX_HEIGHT[tx] / 4) as usize;
        if split {
            let sub = SPLIT_TX_SIZE[tx] as usize;
            let sw = (TX_WIDTH[sub] / 4) as usize;
            let sh = (TX_HEIGHT[sub] / 4) as usize;
            let mut i = 0;
            while i < h4 {
                let mut j = 0;
                while j < w4 {
                    self.read_var_tx(t, b, row + i, col + j, sub, depth + 1);
                    j += sw;
                }
                i += sh;
            }
        } else {
            for i in 0..h4 {
                for j in 0..w4 {
                    let (lr, lc) = (row + i - b.mi_row, col + j - b.mi_col);
                    if lr < 32 && lc < 32 {
                        b.inter_tx[lr * 32 + lc] = tx as u8;
                    }
                }
            }
            b.tx_size = tx;
        }
    }

    fn above_tx_width(&self, b: &Blk, row: usize, col: usize) -> usize {
        if row == b.mi_row {
            if !b.avail_u {
                return 64;
            }
            let m = self.mi_at(row - 1, col);
            if m.skip && m.is_inter {
                return block_width(m.bsize as usize);
            }
            return TX_WIDTH[m.tx_size as usize] as usize;
        }
        TX_WIDTH[b.inter_tx[(row - 1 - b.mi_row) * 32 + col - b.mi_col] as usize] as usize
    }

    fn left_tx_height(&self, b: &Blk, row: usize, col: usize) -> usize {
        if col == b.mi_col {
            if !b.avail_l {
                return 64;
            }
            let m = self.mi_at(row, col - 1);
            if m.skip && m.is_inter {
                return block_height(m.bsize as usize);
            }
            return TX_HEIGHT[m.tx_size as usize] as usize;
        }
        TX_HEIGHT[b.inter_tx[(row - b.mi_row) * 32 + col - 1 - b.mi_col] as usize] as usize
    }
}
