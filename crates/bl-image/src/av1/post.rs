//! Loop filters after the reconstruction of a superblock row (AV1
//! specification sections 7.14 to 7.17): deblocking of the row just
//! decoded, CDEF of the row above it, super-resolution upscaling and loop
//! restoration of the 64-row stripes that are then complete. Each stage
//! writes into its own ring, so the filters see exactly the inputs the
//! specification gives them (deblocked samples for CDEF and next to
//! stripes, CDEF output inside stripes).

use super::decoder::{block_height, block_width, Decoder};
use super::frame::Ring;
use super::obu::SEG_LVL_ALT_LF_Y_V;
use super::tables::*;

pub(crate) struct Post {
    pub lf_on: bool,
    pub cdef_on: bool,
    pub superres: bool,
    pub lr_on: [bool; 3],
    pub cdef: [Ring; 3],
    pub up_rec: [Ring; 3],
    pub up_cdef: [Ring; 3],
    pub out: [Ring; 3],
    /// Luma rows finished by CDEF and by loop restoration.
    pub cdef_done: usize,
    pub lr_done: usize,
    /// Downscaled and upscaled plane widths and the plane heights
    /// (Round2 of the frame size).
    pub dw: [usize; 3],
    pub uw: [usize; 3],
    pub ph: [usize; 3],
    /// Loop restoration source block of a stripe: rows -3 .. h + 3,
    /// columns -3 .. w + 3.
    stripe: Vec<u16>,
    flt: [Vec<i32>; 2],
    box_a: Vec<i32>,
    box_b: Vec<i32>,
}

impl Post {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        planes: usize,
        ssx: usize,
        ssy: usize,
        frame_w: usize,
        upscaled_w: usize,
        frame_h: usize,
        mi_cols: usize,
        sb: usize,
        lf_on: bool,
        superres: bool,
        lr_on: [bool; 3],
        cdef_on: bool,
    ) -> Post {
        let mut p = Post {
            lf_on,
            cdef_on,
            superres,
            lr_on,
            cdef: [Ring::empty(), Ring::empty(), Ring::empty()],
            up_rec: [Ring::empty(), Ring::empty(), Ring::empty()],
            up_cdef: [Ring::empty(), Ring::empty(), Ring::empty()],
            out: [Ring::empty(), Ring::empty(), Ring::empty()],
            cdef_done: 0,
            lr_done: 0,
            dw: [0; 3],
            uw: [0; 3],
            ph: [0; 3],
            stripe: Vec::new(),
            flt: [Vec::new(), Vec::new()],
            box_a: Vec::new(),
            box_b: Vec::new(),
        };
        for pl in 0..planes {
            let (sx, sy) = if pl > 0 { (ssx, ssy) } else { (0, 0) };
            p.dw[pl] = (frame_w + sx) >> sx;
            p.uw[pl] = (upscaled_w + sx) >> sx;
            p.ph[pl] = (frame_h + sy) >> sy;
            let padded = (mi_cols * 4) >> sx;
            p.cdef[pl] = Ring::new(padded, (sb + 16) >> sy);
            if superres {
                p.up_rec[pl] = Ring::new(p.uw[pl], (2 * sb + 16) >> sy);
                p.up_cdef[pl] = Ring::new(p.uw[pl], (sb + 16) >> sy);
            }
            p.out[pl] = Ring::new(p.uw[pl].max(padded), (2 * sb + 16) >> sy);
        }
        p
    }
}

#[inline]
fn round2(x: i64, n: u32) -> i64 {
    if n == 0 {
        x
    } else {
        (x + (1 << (n - 1))) >> n
    }
}

impl Decoder {
    /// Runs the loop filters after superblock row `r0` (MI row) has been
    /// decoded. Returns the luma rows [a, b) that are now final in the
    /// output rings.
    pub fn post_sb_row(&mut self, r0: usize, last: bool) -> (usize, usize) {
        let r1 = (r0 + self.sb4).min(self.mi_rows);
        if self.post.lf_on {
            self.deblock(r0, r1);
        }
        let start = self.post.lr_done;
        let y0 = r0 * 4;
        if y0 > 0 {
            let done = self.post.cdef_done;
            self.cdef_rows(done, y0);
            self.post.cdef_done = y0;
            self.restore_until(y0 - 8);
        }
        if last {
            let done = self.post.cdef_done;
            self.cdef_rows(done, r1 * 4);
            self.post.cdef_done = r1 * 4;
            self.restore_until(self.fh.frame_height);
        }
        (start, self.post.lr_done)
    }

    // ---- deblocking ---------------------------------------------------------

    fn deblock(&mut self, r0: usize, r1: usize) {
        for plane in 0..self.planes {
            if plane > 0 && self.fh.lf_level[1 + plane] == 0 {
                continue;
            }
            let (sx, sy) = if plane > 0 {
                (self.ssx, self.ssy)
            } else {
                (0, 0)
            };
            for pass in 0..2 {
                let mut row = r0;
                while row < r1 {
                    let mut col = 0;
                    while col < self.mi_cols {
                        self.edge(plane, pass, row, col);
                        col += 1 << sx;
                    }
                    row += 1 << sy;
                }
            }
        }
    }

    fn lf_strength(
        &self,
        row: usize,
        col: usize,
        plane: usize,
        pass: usize,
    ) -> (i32, i32, i32, i32) {
        let mi = self.mi_at(row, col);
        let i = if plane == 0 { pass } else { plane + 1 };
        let delta = if self.fh.delta_lf_multi {
            mi.delta_lf[i]
        } else {
            mi.delta_lf[0]
        } as i32;
        let seg = mi.seg as usize;
        let mut lvl = (delta + self.fh.lf_level[i]).clamp(0, 63);
        let feature = SEG_LVL_ALT_LF_Y_V + i;
        if self.fh.seg_feature_active_idx(seg, feature) {
            lvl = (self.fh.feature_data[seg][feature] + lvl).clamp(0, 63);
        }
        if self.fh.lf_delta_enabled {
            let n_shift = lvl >> 5;
            lvl += self.fh.lf_ref_deltas[0] << n_shift;
            lvl = lvl.clamp(0, 63);
        }
        let sharp = self.fh.lf_sharpness;
        let shift = if sharp > 4 {
            2
        } else if sharp > 0 {
            1
        } else {
            0
        };
        let limit = if sharp > 0 {
            (lvl >> shift).clamp(1, 9 - sharp)
        } else {
            (lvl >> shift).max(1)
        };
        (lvl, limit, 2 * (lvl + 2) + limit, lvl >> 4)
    }

    fn edge(&mut self, plane: usize, pass: usize, row0: usize, col0: usize) {
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        let x = col0 * 4;
        let y = row0 * 4;
        let row = row0 | sy;
        let col = col0 | sx;
        if x >= self.fh.frame_width || y >= self.fh.frame_height {
            return;
        }
        if (pass == 0 && x == 0) || (pass == 1 && y == 0) {
            return;
        }
        let xp = x >> sx;
        let yp = y >> sy;
        let (prev_row, prev_col) = if pass == 0 {
            (row, col - (1 << sx))
        } else {
            (row - (1 << sy), col)
        };
        let tx_of = |r: usize, c: usize| -> usize {
            let m = self.mi_at((r >> sy) << sy, (c >> sx) << sx);
            (if plane == 0 { m.lf_tx_y } else { m.lf_tx_uv }) as usize
        };
        let tx = tx_of(row, col);
        let prev_tx = tx_of(prev_row, prev_col);
        // Intra frames: every transform edge is filtered.
        let is_tx_edge = if pass == 0 {
            xp % TX_WIDTH[tx] as usize == 0
        } else {
            yp % TX_HEIGHT[tx] as usize == 0
        };
        if !is_tx_edge {
            return;
        }
        let _ = (block_width, block_height);
        let base = if pass == 0 {
            TX_WIDTH[prev_tx].min(TX_WIDTH[tx])
        } else {
            TX_HEIGHT[prev_tx].min(TX_HEIGHT[tx])
        } as usize;
        let filter_size = if plane == 0 {
            base.min(16)
        } else {
            base.min(8)
        };
        let (mut lvl, mut limit, mut blimit, mut thresh) = self.lf_strength(row, col, plane, pass);
        if lvl == 0 {
            (lvl, limit, blimit, thresh) = self.lf_strength(prev_row, prev_col, plane, pass);
        }
        if lvl == 0 {
            return;
        }
        let filter_len = if filter_size == 4 {
            4
        } else if plane > 0 {
            6
        } else if filter_size == 8 {
            8
        } else {
            16
        };
        let k = match filter_len {
            4 => 2,
            6 => 3,
            8 => 4,
            _ => 7,
        };
        let bd = self.bd;
        let sh = bd - 8;
        let rec = &mut self.frame.rec[plane];
        let mut s = [0i32; 14];
        for i in 0..4 {
            if pass == 0 {
                let yy = yp + i;
                let r = rec.row_mut(yy);
                for t in 0..2 * k {
                    s[t] = r[xp + t - k] as i32;
                }
                if filter_samples(
                    &mut s,
                    k,
                    filter_size,
                    filter_len,
                    plane,
                    limit,
                    blimit,
                    thresh,
                    sh,
                ) {
                    for t in 0..2 * k {
                        r[xp + t - k] = s[t] as u16;
                    }
                }
            } else {
                let xx = xp + i;
                for t in 0..2 * k {
                    s[t] = rec.at(xx, yp + t - k) as i32;
                }
                if filter_samples(
                    &mut s,
                    k,
                    filter_size,
                    filter_len,
                    plane,
                    limit,
                    blimit,
                    thresh,
                    sh,
                ) {
                    for t in 0..2 * k {
                        rec.row_mut(yp + t - k)[xx] = s[t] as u16;
                    }
                }
            }
        }
    }

    // ---- CDEF -----------------------------------------------------------------

    /// CDEF of luma rows [y0, y1) (multiples of 8 except at the frame
    /// end), from the deblocked rows into the CDEF rings; with
    /// super-resolution, both are upscaled afterwards.
    fn cdef_rows(&mut self, y0: usize, y1: usize) {
        if y1 <= y0 {
            return;
        }
        let planes = self.planes;
        // Copy first: blocks without filtering keep the deblocked samples.
        for p in 0..planes {
            let sy = if p > 0 { self.ssy } else { 0 };
            let w = self.post.cdef[p].stride;
            for y in (y0 >> sy)..(y1 >> sy).min(self.frame.h[p]) {
                let src = &self.frame.rec[p].row(y)[..w];
                self.post.cdef[p].row_mut(y).copy_from_slice(src);
            }
        }
        let mut r = y0 / 4;
        while self.post.cdef_on && r < (y1 / 4).min(self.mi_rows) {
            let mut c = 0;
            while c < self.mi_cols {
                self.cdef_block(r, c);
                c += 2;
            }
            r += 2;
        }
        if self.post.superres {
            for p in 0..planes {
                let sy = if p > 0 { self.ssy } else { 0 };
                for y in (y0 >> sy)..(y1 >> sy).min(self.post.ph[p]) {
                    self.upscale_row(p, y);
                }
            }
        }
    }

    fn cdef_block(&mut self, r: usize, c: usize) {
        let idx = self.cdef_get(r & !15, c & !15);
        if idx < 0 {
            return;
        }
        let idx = idx as usize;
        let skip = self.mi_at(r, c).skip
            && self.mi_at(r + 1, c).skip
            && self.mi_at(r, c + 1).skip
            && self.mi_at(r + 1, c + 1).skip;
        if skip {
            return;
        }
        let coeff_shift = self.bd - 8;
        let (y_dir, var) = self.cdef_direction(r, c);
        let mut pri = self.fh.cdef_y_pri[idx] << coeff_shift;
        let sec = self.fh.cdef_y_sec[idx] << coeff_shift;
        let dir = if pri == 0 { 0 } else { y_dir };
        let var_str = if (var >> 6) != 0 {
            ((var >> 6) as u32).ilog2().min(12) as i32
        } else {
            0
        };
        pri = if var != 0 {
            (pri * (4 + var_str) + 8) >> 4
        } else {
            0
        };
        let damping = self.fh.cdef_damping + coeff_shift as i32;
        self.cdef_filter(0, r, c, pri, sec, damping, dir);
        if self.planes == 1 {
            return;
        }
        let pri = self.fh.cdef_uv_pri[idx] << coeff_shift;
        let sec = self.fh.cdef_uv_sec[idx] << coeff_shift;
        let dir = if pri == 0 {
            0
        } else {
            CDEF_UV_DIR[self.ssx][self.ssy][y_dir] as usize
        };
        let damping = self.fh.cdef_damping + coeff_shift as i32 - 1;
        self.cdef_filter(1, r, c, pri, sec, damping, dir);
        self.cdef_filter(2, r, c, pri, sec, damping, dir);
    }

    fn cdef_direction(&self, r: usize, c: usize) -> (usize, i32) {
        let mut cost = [0i64; 8];
        let mut partial = [[0i64; 15]; 8];
        let x0 = c * 4;
        let y0 = r * 4;
        let sh = self.bd - 8;
        for i in 0..8 {
            let row = self.frame.rec[0].row(y0 + i);
            for j in 0..8 {
                let x = ((row[x0 + j] >> sh) as i64) - 128;
                partial[0][i + j] += x;
                partial[1][i + j / 2] += x;
                partial[2][i] += x;
                partial[3][3 + i - j / 2] += x;
                partial[4][7 + i - j] += x;
                partial[5][3 - i / 2 + j] += x;
                partial[6][j] += x;
                partial[7][i / 2 + j] += x;
            }
        }
        for i in 0..8 {
            cost[2] += partial[2][i] * partial[2][i];
            cost[6] += partial[6][i] * partial[6][i];
        }
        cost[2] *= DIV_TABLE[8] as i64;
        cost[6] *= DIV_TABLE[8] as i64;
        for i in 0..7 {
            cost[0] += (partial[0][i] * partial[0][i] + partial[0][14 - i] * partial[0][14 - i])
                * DIV_TABLE[i + 1] as i64;
            cost[4] += (partial[4][i] * partial[4][i] + partial[4][14 - i] * partial[4][14 - i])
                * DIV_TABLE[i + 1] as i64;
        }
        cost[0] += partial[0][7] * partial[0][7] * DIV_TABLE[8] as i64;
        cost[4] += partial[4][7] * partial[4][7] * DIV_TABLE[8] as i64;
        let mut i = 1;
        while i < 8 {
            for j in 0..5 {
                cost[i] += partial[i][3 + j] * partial[i][3 + j];
            }
            cost[i] *= DIV_TABLE[8] as i64;
            for j in 0..3 {
                cost[i] += (partial[i][j] * partial[i][j]
                    + partial[i][10 - j] * partial[i][10 - j])
                    * DIV_TABLE[2 * j + 2] as i64;
            }
            i += 2;
        }
        let mut best = 0i64;
        let mut dir = 0;
        for (i, &c) in cost.iter().enumerate() {
            if c > best {
                best = c;
                dir = i;
            }
        }
        (dir, ((best - cost[(dir + 4) & 7]) >> 10) as i32)
    }

    #[allow(clippy::too_many_arguments)]
    fn cdef_filter(
        &mut self,
        plane: usize,
        r: usize,
        c: usize,
        pri: i32,
        sec: i32,
        damping: i32,
        dir: usize,
    ) {
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        let x0 = (c * 4) >> sx;
        let y0 = (r * 4) >> sy;
        let w = 8 >> sx;
        let h = 8 >> sy;
        if pri == 0 && sec == 0 {
            // Nothing changes (the copy is already in place).
            return;
        }
        let coeff_shift = self.bd - 8;
        let pri_tap = CDEF_PRI_TAPS[((pri >> coeff_shift) & 1) as usize];
        let sec_tap = CDEF_SEC_TAPS[((pri >> coeff_shift) & 1) as usize];
        // The block with a border of 2 samples; unavailable samples (outside
        // the frame) are marked.
        const B: usize = 12;
        // Rows of 8 starting anywhere in the block stay inside.
        let mut px = [0i32; B * B + 16];
        let mut avail = [true; B * B];
        let pw = ((self.mi_cols * 4) >> sx) as isize;
        let ph = ((self.mi_rows * 4) >> sy) as isize;
        let rec = &self.frame.rec[plane];
        let mut edge = false;
        let inner =
            x0 >= 2 && y0 >= 2 && (x0 + w + 2) as isize <= pw && (y0 + h + 2) as isize <= ph;
        if inner {
            for i in 0..h + 4 {
                let row = &rec.row(y0 + i - 2)[x0 - 2..x0 + w + 2];
                for (d, &v) in px[i * B..i * B + w + 4].iter_mut().zip(row) {
                    *d = v as i32;
                }
            }
        }
        for i in 0..if inner { 0 } else { h + 4 } {
            let y = y0 as isize + i as isize - 2;
            if y < 0 || y >= ph {
                for j in 0..w + 4 {
                    avail[i * B + j] = false;
                }
                edge = true;
                continue;
            }
            let row = rec.row(y as usize);
            for j in 0..w + 4 {
                let x = x0 as isize + j as isize - 2;
                if x < 0 || x >= pw {
                    avail[i * B + j] = false;
                    edge = true;
                } else {
                    px[i * B + j] = row[x as usize] as i32;
                }
            }
        }
        let pri_adj = if pri > 0 {
            (damping - (pri as u32).ilog2() as i32).max(0)
        } else {
            0
        };
        let sec_adj = if sec > 0 {
            (damping - (sec as u32).ilog2() as i32).max(0)
        } else {
            0
        };
        // Tap offsets (row, column) in the bordered block.
        let off = |d: usize, k: usize| -> (usize, usize) {
            let o = &CDEF_DIRECTIONS[d][k];
            (o[0] as isize as usize, o[1] as isize as usize)
        };
        let po = [off(dir, 0), off(dir, 1)];
        let s1 = [off((dir + 2) & 7, 0), off((dir + 2) & 7, 1)];
        let s2 = [off((dir + 6) & 7, 0), off((dir + 6) & 7, 1)];
        let mut out = [0u16; 64];
        if !edge {
            // All taps available: whole rows of 8 at once (vectorises).
            let lin = |o: (usize, usize)| -> isize { (o.0 as isize) * B as isize + o.1 as isize };
            // (offset, strength, adjustment, tap weight) of the 12 taps.
            let mut taps = [(0isize, 0i32, 0i32, 0i32); 12];
            let mut n = 0;
            for k in 0..2 {
                for sign in [-1isize, 1] {
                    taps[n] = (sign * lin(po[k]), pri, pri_adj, pri_tap[k] as i32);
                    taps[n + 1] = (sign * lin(s2[k]), sec, sec_adj, sec_tap[k] as i32);
                    taps[n + 2] = (sign * lin(s1[k]), sec, sec_adj, sec_tap[k] as i32);
                    n += 3;
                }
            }
            // The block as 64 lanes (8 x 8; chroma blocks use part of it).
            let mut xr = [0i32; 64];
            for i in 0..h {
                let c0 = (i + 2) * B + 2;
                xr[i * 8..i * 8 + 8].copy_from_slice(&px[c0..c0 + 8]);
            }
            let lanes = h * 8;
            let mut sum = [0i32; 64];
            let mut max = xr;
            let mut min = xr;
            let mut v = [0i32; 64];
            for &(o, strength, adj, t) in &taps {
                for i in 0..h {
                    let st = ((i + 2) * B + 2) as isize + o;
                    let st = st.clamp(0, (B * B) as isize) as usize;
                    v[i * 8..i * 8 + 8].copy_from_slice(&px[st..st + 8]);
                }
                for j in 0..lanes {
                    let d = v[j] - xr[j];
                    let ad = d.abs();
                    let m = (strength - (ad >> adj)).max(0).min(ad);
                    sum[j] += t * if d < 0 { -m } else { m };
                    max[j] = max[j].max(v[j]);
                    min[j] = min[j].min(v[j]);
                }
            }
            for i in 0..h {
                for j in 0..w {
                    let k = i * 8 + j;
                    let val = xr[k] + ((8 + sum[k] - (sum[k] < 0) as i32) >> 4);
                    out[i * 8 + j] = val.clamp(min[k], max[k]) as u16;
                }
            }
        } else {
            let cons = |diff: i32, threshold: i32, adj: i32| -> i32 {
                let mag = (threshold - (diff.abs() >> adj)).clamp(0, diff.abs());
                if diff < 0 {
                    -mag
                } else {
                    mag
                }
            };
            let lin = |o: (usize, usize)| -> isize { (o.0 as isize) * B as isize + o.1 as isize };
            for i in 0..h {
                for j in 0..w {
                    let ci = ((i + 2) * B + j + 2) as isize;
                    let x = px[ci as usize];
                    let mut sum = 0i32;
                    let mut max = x;
                    let mut min = x;
                    for k in 0..2 {
                        for sign in [-1isize, 1] {
                            let p = (ci + sign * lin(po[k])) as usize;
                            if avail[p] {
                                let v = px[p];
                                sum += pri_tap[k] as i32 * cons(v - x, pri, pri_adj);
                                max = max.max(v);
                                min = min.min(v);
                            }
                            for so in [s2[k], s1[k]] {
                                let q = (ci + sign * lin(so)) as usize;
                                if avail[q] {
                                    let v = px[q];
                                    sum += sec_tap[k] as i32 * cons(v - x, sec, sec_adj);
                                    max = max.max(v);
                                    min = min.min(v);
                                }
                            }
                        }
                    }
                    let v = x + ((8 + sum - (sum < 0) as i32) >> 4);
                    out[i * 8 + j] = v.clamp(min, max) as u16;
                }
            }
        }
        let ring = &mut self.post.cdef[plane];
        for i in 0..h {
            let row = ring.row_mut(y0 + i);
            row[x0..x0 + w].copy_from_slice(&out[i * 8..i * 8 + w]);
        }
    }

    // ---- super-resolution -------------------------------------------------------

    /// Upscales row `y` of plane `p` of the deblocked frame and of the CDEF
    /// output.
    fn upscale_row(&mut self, p: usize, y: usize) {
        let sx = if p > 0 { self.ssx } else { 0 };
        let down = self.post.dw[p] as i64;
        let up = self.post.uw[p] as i64;
        let step = ((down << 14) + up / 2) / up;
        let err = up * step - (down << 14);
        let init = ((-((up - down) << 13)) + up / 2) / up + (1 << 7) - err / 2;
        let init = init & ((1 << 14) - 1);
        let max_x = (((self.mi_cols >> sx) * 4) - 1) as i64;
        let maxv = (1i64 << self.bd) - 1;
        for which in 0..2 {
            let mut out = std::mem::take(if which == 0 {
                &mut self.post.up_rec[p]
            } else {
                &mut self.post.up_cdef[p]
            });
            {
                let src = if which == 0 {
                    self.frame.rec[p].row(y)
                } else {
                    self.post.cdef[p].row(y)
                };
                let dst = out.row_mut(y);
                for x in 0..up {
                    let src_x = -(1i64 << 14) + init + x * step;
                    let px = src_x >> 14;
                    let sub = ((src_x & ((1 << 14) - 1)) >> 8) as usize;
                    let mut sum = 0i64;
                    for k in 0..8 {
                        let sxp = (px + k as i64 - 3).clamp(0, max_x) as usize;
                        sum += src[sxp] as i64 * UPSCALE_FILTER[sub][k] as i64;
                    }
                    dst[x as usize] = round2(sum, 7).clamp(0, maxv) as u16;
                }
            }
            if which == 0 {
                self.post.up_rec[p] = out;
            } else {
                self.post.up_cdef[p] = out;
            }
        }
    }

    // ---- loop restoration ---------------------------------------------------------

    /// Loop restoration (or a copy) of luma rows up to `y_end` into the
    /// output rings.
    fn restore_until(&mut self, y_end: usize) {
        let y_end = y_end.min(self.fh.frame_height);
        let y0 = self.post.lr_done;
        if y_end <= y0 {
            return;
        }
        for p in 0..self.planes {
            let sy = if p > 0 { self.ssy } else { 0 };
            let (a, b) = if y_end >= self.fh.frame_height {
                ((y0 + sy) >> sy, self.post.ph[p])
            } else {
                ((y0 + sy) >> sy, (y_end + sy) >> sy)
            };
            let a = if y0 == 0 { 0 } else { a };
            if self.post.lr_on[p] {
                // Stripe by stripe.
                let sh = 64 >> sy;
                let off = 8 >> sy;
                let mut s = a;
                while s < b {
                    let stripe_num = (s + off) / sh;
                    let st = (stripe_num * sh) as isize - off as isize;
                    let en = st + sh as isize - 1;
                    let e = ((en + 1) as usize).min(b);
                    self.lr_stripe(p, s, e, st, en);
                    s = e;
                }
            } else {
                for y in a..b {
                    let w = self.post.uw[p];
                    let src = if self.post.superres {
                        &self.post.up_cdef[p].row(y)[..w]
                    } else {
                        &self.post.cdef[p].row(y)[..w]
                    };
                    let dst = &mut self.post.out[p].row_mut(y)[..w];
                    dst.copy_from_slice(src);
                }
            }
        }
        self.post.lr_done = y_end;
    }

    /// Restores plane rows [y0, y1) of the stripe [st, en].
    fn lr_stripe(&mut self, p: usize, y0: usize, y1: usize, st: isize, en: isize) {
        let sy = if p > 0 { self.ssy } else { 0 };
        let w = self.post.uw[p];
        let plane_end_y = self.post.ph[p] as isize - 1;
        let h = y1 - y0;
        let sw = w + 6;
        let sh = h + 6;
        let mut src = std::mem::take(&mut self.post.stripe);
        src.clear();
        src.resize(sw * sh, 0);
        // get_source_sample for rows y0 - 3 .. y1 + 2 and columns -3 .. w + 2.
        for i in 0..sh {
            let mut y = y0 as isize + i as isize - 3;
            y = y.min(plane_end_y).max(0);
            let row: &[u16] = if y < st {
                let y = y.max(st - 2) as usize;
                if self.post.superres {
                    self.post.up_rec[p].row(y)
                } else {
                    self.frame.rec[p].row(y)
                }
            } else if y > en {
                let y = y.min(en + 2) as usize;
                if self.post.superres {
                    self.post.up_rec[p].row(y)
                } else {
                    self.frame.rec[p].row(y)
                }
            } else if self.post.superres {
                self.post.up_cdef[p].row(y as usize)
            } else {
                self.post.cdef[p].row(y as usize)
            };
            let d = &mut src[i * sw..(i + 1) * sw];
            d[3..3 + w].copy_from_slice(&row[..w]);
            for k in 0..3 {
                d[k] = row[0];
                d[3 + w + k] = row[w - 1];
            }
        }
        let unit_size = self.fh.lr_unit_size[p];
        let (cols, rows, _) = self.lr_units[p];
        let unit_row = (rows - 1).min((y0 + (8 >> sy)) / unit_size);
        let mut x = 0;
        while x < w {
            let unit_col = (cols - 1).min(x / unit_size);
            let x_end = if unit_col == cols - 1 {
                w
            } else {
                (x + unit_size).min(w)
            };
            let u = self.lr_units[p].2[unit_row * cols + unit_col];
            match u.kind {
                1 => self.wiener(p, &src, sw, x, x_end, y0, h, &u.wiener),
                2 => self.sgr(p, &src, sw, x, x_end, y0, h, u.sgr_set as usize, u.sgr_xqd),
                _ => {
                    for i in 0..h {
                        let s = &src[(i + 3) * sw + 3 + x..(i + 3) * sw + 3 + x_end];
                        self.post.out[p].row_mut(y0 + i)[x..x_end].copy_from_slice(s);
                    }
                }
            }
            x = x_end;
        }
        self.post.stripe = src;
    }

    #[allow(clippy::too_many_arguments)]
    fn wiener(
        &mut self,
        p: usize,
        src: &[u16],
        sw: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        h: usize,
        coef: &[[i32; 3]; 2],
    ) {
        let bd = self.bd;
        let round0 = if bd == 12 { 5 } else { 3 };
        let round1 = if bd == 12 { 9 } else { 11 };
        let filt = |c: &[i32; 3]| -> [i32; 7] {
            let mut f = [0i32; 7];
            f[3] = 128;
            for i in 0..3 {
                f[i] = c[i];
                f[6 - i] = c[i];
                f[3] -= 2 * c[i];
            }
            f
        };
        let vf = filt(&coef[0]);
        let hf = filt(&coef[1]);
        let offset = 1i32 << (bd + 7 - round0 - 1);
        let limit = (1i32 << (bd + 1 + 7 - round0)) - 1;
        let w = x1 - x0;
        let mut inter = std::mem::take(&mut self.post.flt[0]);
        inter.clear();
        inter.resize(w * (h + 6), 0);
        for r in 0..h + 6 {
            let row = &src[r * sw..];
            for c in 0..w {
                let mut s = 0i32;
                for t in 0..7 {
                    s += hf[t] * row[x0 + c + t] as i32;
                }
                let v = round2(s as i64, round0) as i32;
                inter[r * w + c] = v.clamp(-offset, limit - offset);
            }
        }
        let max = (1i32 << bd) - 1;
        for r in 0..h {
            let out = &mut self.post.out[p].row_mut(y0 + r)[x0..x1];
            for c in 0..w {
                let mut s = 0i32;
                for t in 0..7 {
                    s += vf[t] * inter[(r + t) * w + c];
                }
                out[c] = (round2(s as i64, round1) as i32).clamp(0, max) as u16;
            }
        }
        self.post.flt[0] = inter;
    }

    #[allow(clippy::too_many_arguments)]
    fn sgr(
        &mut self,
        p: usize,
        src: &[u16],
        sw: usize,
        x0: usize,
        x1: usize,
        y0: usize,
        h: usize,
        set: usize,
        xqd: [i32; 2],
    ) {
        let w = x1 - x0;
        let mut flt0 = std::mem::take(&mut self.post.flt[0]);
        let mut flt1 = std::mem::take(&mut self.post.flt[1]);
        self.box_filter(src, sw, x0, w, h, set, 0, &mut flt0);
        self.box_filter(src, sw, x0, w, h, set, 1, &mut flt1);
        let w0 = xqd[0];
        let w1 = xqd[1];
        let w2 = (1 << 7) - w0 - w1;
        let r0 = SGR_PARAMS[set][0];
        let r1 = SGR_PARAMS[set][2];
        let max = (1i64 << self.bd) - 1;
        for i in 0..h {
            let out = &mut self.post.out[p].row_mut(y0 + i)[x0..x1];
            for j in 0..w {
                let u = (src[(i + 3) * sw + 3 + x0 + j] as i64) << 4;
                let mut v = w1 as i64 * u;
                v += if r0 != 0 {
                    w0 as i64 * flt0[i * w + j] as i64
                } else {
                    w0 as i64 * u
                };
                v += if r1 != 0 {
                    w2 as i64 * flt1[i * w + j] as i64
                } else {
                    w2 as i64 * u
                };
                out[j] = round2(v, 4 + 7).clamp(0, max) as u16;
            }
        }
        self.post.flt[0] = flt0;
        self.post.flt[1] = flt1;
    }

    /// Box filter of the self guided filter for columns [x0, x0 + w) and
    /// rows 0..h of the stripe block (which has 3 rows / columns of
    /// margin).
    #[allow(clippy::too_many_arguments)]
    fn box_filter(
        &mut self,
        src: &[u16],
        sw: usize,
        x0: usize,
        w: usize,
        h: usize,
        set: usize,
        pass: usize,
        f: &mut Vec<i32>,
    ) {
        let r = SGR_PARAMS[set][pass * 2] as isize;
        if r == 0 {
            return;
        }
        let eps = SGR_PARAMS[set][pass * 2 + 1] as i64;
        let bd = self.bd;
        let n = ((2 * r + 1) * (2 * r + 1)) as i64;
        let n2e = n * n * eps;
        let s = ((1i64 << 20) + n2e / 2) / n2e;
        let one_over_n = ((1i64 << 12) + n / 2) / n;
        // A and B for i = -1..h, j = -1..w.
        let aw = w + 2;
        let ah = h + 2;
        let mut aa = std::mem::take(&mut self.post.box_a);
        let mut bb = std::mem::take(&mut self.post.box_b);
        aa.clear();
        aa.resize(aw * ah, 0);
        bb.clear();
        bb.resize(aw * ah, 0);
        for i in -1..=(h as isize) {
            if pass == 0 && (i & 1) == 0 {
                continue;
            }
            for j in -1..=(w as isize) {
                let mut a: i64 = 0;
                let mut b: i64 = 0;
                for dy in -r..=r {
                    let row = &src[((i + dy + 3) as usize) * sw..];
                    for dx in -r..=r {
                        let c = row[(x0 as isize + j + dx + 3) as usize] as i64;
                        a += c * c;
                        b += c;
                    }
                }
                let a = round2(a, 2 * (bd - 8));
                let d = round2(b, bd - 8);
                let pp = (a * n - d * d).max(0);
                let z = round2(pp * s, 20);
                let a2 = if z >= 255 {
                    256
                } else if z == 0 {
                    1
                } else {
                    ((z << 8) + z / 2) / (z + 1)
                };
                let b2 = ((1 << 8) - a2) * b * one_over_n;
                let k = ((i + 1) as usize) * aw + (j + 1) as usize;
                aa[k] = a2 as i32;
                bb[k] = round2(b2, 12) as i32;
            }
        }
        f.clear();
        f.resize(w * h, 0);
        for i in 0..h {
            let shift = if pass == 0 && (i & 1) == 1 { 4 } else { 5 };
            for j in 0..w {
                let mut a: i64 = 0;
                let mut b: i64 = 0;
                for dy in -1isize..=1 {
                    for dx in -1isize..=1 {
                        let weight = if pass == 0 {
                            if ((i as isize + dy) & 1) == 1 {
                                if dx == 0 {
                                    6
                                } else {
                                    5
                                }
                            } else {
                                0
                            }
                        } else if dx == 0 || dy == 0 {
                            4
                        } else {
                            3
                        };
                        if weight == 0 {
                            continue;
                        }
                        let k =
                            ((i as isize + dy + 1) as usize) * aw + (j as isize + dx + 1) as usize;
                        a += weight * aa[k] as i64;
                        b += weight * bb[k] as i64;
                    }
                }
                let v = a * src[(i + 3) * sw + 3 + x0 + j] as i64 + b;
                f[i * w + j] = round2(v, 8 + shift - 4) as i32;
            }
        }
        self.post.box_a = aa;
        self.post.box_b = bb;
    }
}

/// The deblocking filter for one line across an edge. `s` holds k samples
/// on each side (s[k - 1] = p0, s[k] = q0). Returns whether samples were
/// changed.
#[allow(clippy::too_many_arguments)]
fn filter_samples(
    s: &mut [i32; 14],
    k: usize,
    filter_size: usize,
    filter_len: usize,
    plane: usize,
    limit: i32,
    blimit: i32,
    thresh: i32,
    sh: u32,
) -> bool {
    let q = |i: usize| s[k + i];
    let p = |i: usize| s[k - 1 - i];
    let thresh_bd = thresh << sh;
    let hev = (p(1) - p(0)).abs() > thresh_bd || (q(1) - q(0)).abs() > thresh_bd;
    let limit_bd = limit << sh;
    let blimit_bd = blimit << sh;
    let mut mask = (p(1) - p(0)).abs() > limit_bd
        || (q(1) - q(0)).abs() > limit_bd
        || (p(0) - q(0)).abs() * 2 + (p(1) - q(1)).abs() / 2 > blimit_bd;
    if filter_len >= 6 {
        mask |= (p(2) - p(1)).abs() > limit_bd || (q(2) - q(1)).abs() > limit_bd;
    }
    if filter_len >= 8 {
        mask |= (p(3) - p(2)).abs() > limit_bd || (q(3) - q(2)).abs() > limit_bd;
    }
    if mask {
        return false;
    }
    let one = 1 << sh;
    let mut flat = false;
    if filter_size >= 8 {
        let mut m = (p(1) - p(0)).abs() > one
            || (q(1) - q(0)).abs() > one
            || (p(2) - p(0)).abs() > one
            || (q(2) - q(0)).abs() > one;
        if filter_len >= 8 {
            m |= (p(3) - p(0)).abs() > one || (q(3) - q(0)).abs() > one;
        }
        flat = !m;
    }
    let mut flat2 = false;
    if filter_size >= 16 {
        let m = (p(6) - p(0)).abs() > one
            || (q(6) - q(0)).abs() > one
            || (p(5) - p(0)).abs() > one
            || (q(5) - q(0)).abs() > one
            || (p(4) - p(0)).abs() > one
            || (q(4) - q(0)).abs() > one;
        flat2 = !m;
    }
    if filter_size == 4 || !flat {
        // Narrow filter.
        let bd = sh + 8;
        let lo = -(1i32 << (bd - 1));
        let hi = (1i32 << (bd - 1)) - 1;
        let c = |v: i32| v.clamp(lo, hi);
        let o = 0x80 << sh;
        let ps1 = p(1) - o;
        let ps0 = p(0) - o;
        let qs0 = q(0) - o;
        let qs1 = q(1) - o;
        let mut filter = if hev { c(ps1 - qs1) } else { 0 };
        filter = c(filter + 3 * (qs0 - ps0));
        let filter1 = c(filter + 4) >> 3;
        let filter2 = c(filter + 3) >> 3;
        s[k] = c(qs0 - filter1) + o;
        s[k - 1] = c(ps0 + filter2) + o;
        if !hev {
            let f = (filter1 + 1) >> 1;
            s[k + 1] = c(qs1 - f) + o;
            s[k - 2] = c(ps1 + f) + o;
        }
    } else {
        let log2 = if filter_size == 8 || !flat2 { 3 } else { 4 };
        let n: isize = if log2 == 4 {
            6
        } else if plane == 0 {
            3
        } else {
            2
        };
        let n2: isize = if log2 == 3 && plane == 0 { 0 } else { 1 };
        let mut f = [0i32; 12];
        let at = |i: isize| s[(k as isize + i) as usize];
        for i in -n..n {
            let mut t = 0;
            for j in -n..=n {
                let pp = (i + j).clamp(-(n + 1), n);
                let tap = if j.abs() <= n2 { 2 } else { 1 };
                t += at(pp) * tap;
            }
            f[(i + n) as usize] = (t + (1 << (log2 - 1))) >> log2;
        }
        for i in -n..n {
            s[(k as isize + i) as usize] = f[(i + n) as usize];
        }
    }
    true
}
