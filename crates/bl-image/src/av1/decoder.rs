//! Tile and block decoding of intra frames: partitions, mode info,
//! palettes, transform sizes (AV1 specification sections 5.11 and 6.10).
//! Coefficients and reconstruction are in `residual.rs`, the frame
//! buffers and loop filters in `frame.rs`.

use super::cdf::Cdfs;
use super::frame::FrameBuf;
use super::obu::{FrameHeader, SequenceHeader, SEG_LVL_SKIP};
use super::post::Post;
use super::predict::{is_directional, DC_PRED, UV_CFL_PRED};
use super::symbol::SymbolDecoder;
use super::tables::*;
use crate::Error;

pub(crate) const BLOCK_4X4: usize = 0;
pub(crate) const BLOCK_8X8: usize = 3;
pub(crate) const BLOCK_64X64: usize = 12;
pub(crate) const BLOCK_128X128: usize = 15;
pub(crate) const BLOCK_INVALID: u8 = 22;

const PARTITION_NONE: usize = 0;
const PARTITION_HORZ: usize = 1;
const PARTITION_VERT: usize = 2;
const PARTITION_SPLIT: usize = 3;
const PARTITION_HORZ_A: usize = 4;
const PARTITION_HORZ_B: usize = 5;
const PARTITION_VERT_A: usize = 6;
const PARTITION_VERT_B: usize = 7;
const PARTITION_HORZ_4: usize = 8;
const PARTITION_VERT_4: usize = 9;

pub(crate) const TX_4X4: usize = 0;
pub(crate) const TX_16X16: usize = 2;
pub(crate) const TX_32X32: usize = 3;
pub(crate) const TX_64X64: usize = 4;
pub(crate) const TX_16X32: usize = 9;
pub(crate) const TX_32X16: usize = 10;

#[inline]
pub(crate) fn block_width(b: usize) -> usize {
    4 * NUM_4X4_BLOCKS_WIDE[b] as usize
}

#[inline]
pub(crate) fn block_height(b: usize) -> usize {
    4 * NUM_4X4_BLOCKS_HIGH[b] as usize
}

/// Mode info of one 4x4 unit, kept for the superblock rows the loop
/// filters still need.
#[derive(Clone, Copy, Default)]
pub(crate) struct Mi {
    pub bsize: u8,
    pub y_mode: u8,
    pub uv_mode: u8,
    pub skip: bool,
    pub is_inter: bool,
    pub seg: u8,
    /// TxSize (InterTxSizes) of the block.
    pub tx_size: u8,
    /// Loop filter transform sizes: luma here, chroma at the luma
    /// position of the chroma 4x4 unit.
    pub lf_tx_y: u8,
    pub lf_tx_uv: u8,
    pub pal_y: u8,
    pub pal_uv: u8,
    pub delta_lf: [i8; 4],
    /// Intra block copy vector (row, column) in 1/8 samples.
    pub mv: [i16; 2],
    /// The MI row this entry was written for (the ring reuses entries).
    pub row_tag: u32,
}

/// Entropy decoding state of one tile.
pub(crate) struct Tile<'a> {
    pub sd: SymbolDecoder<'a>,
    pub cdf: Box<Cdfs>,
    pub mi_row_start: usize,
    pub mi_row_end: usize,
    pub mi_col_start: usize,
    pub mi_col_end: usize,
    pub current_q: i32,
    pub delta_lf: [i32; 4],
    pub ref_sgr_xqd: [[i32; 2]; 3],
    pub ref_lr_wiener: [[[i32; 3]; 2]; 3],
    pub left_level: [[u8; 32]; 3],
    pub left_dc: [[u8; 32]; 3],
}

/// Loop restoration parameters of one unit.
#[derive(Clone, Copy, Default)]
pub(crate) struct LrUnit {
    pub kind: u8,
    pub wiener: [[i32; 3]; 2],
    pub sgr_set: u8,
    pub sgr_xqd: [i32; 2],
}

/// The state of the block being decoded.
#[derive(Clone)]
pub(crate) struct Blk {
    pub mi_row: usize,
    pub mi_col: usize,
    pub size: usize,
    pub bw4: usize,
    pub bh4: usize,
    pub has_chroma: bool,
    pub avail_u: bool,
    pub avail_l: bool,
    pub avail_u_chroma: bool,
    pub avail_l_chroma: bool,
    pub skip: bool,
    pub segment_id: usize,
    pub lossless: bool,
    pub is_inter: bool,
    pub y_mode: u8,
    pub uv_mode: u8,
    pub angle_delta_y: i32,
    pub angle_delta_uv: i32,
    pub cfl_alpha_u: i32,
    pub cfl_alpha_v: i32,
    pub use_filter_intra: bool,
    pub filter_intra_mode: u8,
    pub pal_size_y: usize,
    pub pal_size_uv: usize,
    pub pal_y: [u16; 8],
    pub pal_u: [u16; 8],
    pub pal_v: [u16; 8],
    pub tx_size: usize,
    pub max_luma_w: usize,
    pub max_luma_h: usize,
    /// Luma transform types by 4x4 position within the block.
    pub tx_types: [u8; 1024],
    /// Intra block copy motion vector (row, col) in 1/8 samples.
    pub mv: [i32; 2],
    /// CurrentQIndex when the block was read.
    pub cur_q: i32,
    /// InterTxSizes by 4x4 position within the block.
    pub inter_tx: [u8; 1024],
}

impl Default for Blk {
    fn default() -> Blk {
        Blk {
            mi_row: 0,
            mi_col: 0,
            size: 0,
            bw4: 1,
            bh4: 1,
            has_chroma: false,
            avail_u: false,
            avail_l: false,
            avail_u_chroma: false,
            avail_l_chroma: false,
            skip: false,
            segment_id: 0,
            lossless: false,
            is_inter: false,
            y_mode: 0,
            uv_mode: 0,
            angle_delta_y: 0,
            angle_delta_uv: 0,
            cfl_alpha_u: 0,
            cfl_alpha_v: 0,
            use_filter_intra: false,
            filter_intra_mode: 0,
            pal_size_y: 0,
            pal_size_uv: 0,
            pal_y: [0; 8],
            pal_u: [0; 8],
            pal_v: [0; 8],
            tx_size: 0,
            max_luma_w: 0,
            max_luma_h: 0,
            tx_types: [0; 1024],
            mv: [0; 2],
            cur_q: 0,
            inter_tx: [0; 1024],
        }
    }
}

/// Decoder of one intra frame.
pub(crate) struct Decoder {
    pub seq: SequenceHeader,
    pub fh: FrameHeader,
    pub planes: usize,
    pub ssx: usize,
    pub ssy: usize,
    pub bd: u32,
    pub sb4: usize,
    pub sb_size: usize,
    pub mi_cols: usize,
    pub mi_rows: usize,
    /// Mode info ring: `mi_cap` rows of `mi_cols`.
    pub mi: Vec<Mi>,
    pub mi_cap: usize,
    /// Palette colours (Y, U) per mode info unit, when screen content
    /// tools are on.
    pub pal: Vec<[[u16; 8]; 2]>,
    /// cdef_idx per 64x64 block, ring of `cdef_cap` rows.
    pub cdef_idx: Vec<i8>,
    pub cdef_cols: usize,
    pub cdef_cap: usize,
    pub above_level: [Vec<u8>; 3],
    pub above_dc: [Vec<u8>; 3],
    /// Loop restoration units per plane: (columns, rows, units).
    pub lr_units: [(usize, usize, Vec<LrUnit>); 3],
    /// BlockDecoded flags of the superblock, (sb4 + 3)^2 per plane with
    /// an offset of 1.
    pub decoded: [Vec<bool>; 3],
    pub frame: FrameBuf,
    pub post: Post,
    pub blk: Box<Blk>,
    pub color_map_y: Vec<u8>,
    pub color_map_uv: Vec<u8>,
    pub quant: Vec<i32>,
    pub scratch: Vec<i32>,
    pub residual: Vec<i32>,
    pub read_deltas: bool,
}

impl Decoder {
    pub fn new(seq: SequenceHeader, fh: FrameHeader) -> Decoder {
        let planes = seq.color.num_planes();
        let ssx = seq.color.ssx as usize;
        let ssy = seq.color.ssy as usize;
        let sb4 = if seq.sb128 { 32 } else { 16 };
        let mi_cols = fh.mi_cols;
        let mi_rows = fh.mi_rows;
        let mi_cap = 2 * sb4;
        let cdef_cols = mi_cols.div_ceil(16);
        let cdef_cap = 2 * sb4 / 16;
        let screen = fh.allow_screen_content_tools;
        let mut lr_units: [(usize, usize, Vec<LrUnit>); 3] = Default::default();
        for p in 0..planes {
            if fh.lr_type[p] != 0 {
                let (sx, sy) = if p == 0 { (0, 0) } else { (ssx, ssy) };
                let size = fh.lr_unit_size[p];
                let count = |frame_size: usize| ((frame_size + (size >> 1)) / size).max(1);
                let rows = count((fh.frame_height + sy) >> sy);
                let cols = count((fh.upscaled_width + sx) >> sx);
                lr_units[p] = (cols, rows, vec![LrUnit::default(); cols * rows]);
            }
        }
        let dsz = (sb4 + 3) * (sb4 + 3);
        let sb = sb4 * 4;
        // Intra block copy reads anywhere above in the tile (loop filters
        // are off then): keep the whole frame.
        let rows = if fh.allow_intrabc {
            mi_rows * 4 + sb
        } else {
            2 * sb + 16
        };
        let frame = FrameBuf::new(planes, mi_cols * 4, mi_rows * 4, ssx, ssy, rows);
        let mut lr_on = [false; 3];
        for (p, on) in lr_on.iter_mut().enumerate().take(planes) {
            *on = fh.lr_type[p] != 0;
        }
        let post = Post::new(
            planes,
            ssx,
            ssy,
            fh.frame_width,
            fh.upscaled_width,
            fh.frame_height,
            mi_cols,
            sb,
            fh.lf_level[0] != 0 || fh.lf_level[1] != 0,
            fh.use_superres,
            lr_on,
        );
        Decoder {
            planes,
            ssx,
            ssy,
            bd: seq.color.bit_depth,
            sb4,
            sb_size: if seq.sb128 {
                BLOCK_128X128
            } else {
                BLOCK_64X64
            },
            mi_cols,
            mi_rows,
            mi: vec![
                Mi {
                    row_tag: u32::MAX,
                    ..Mi::default()
                };
                mi_cap * mi_cols
            ],
            mi_cap,
            pal: if screen {
                vec![[[0; 8]; 2]; mi_cap * mi_cols]
            } else {
                Vec::new()
            },
            cdef_idx: vec![-1; cdef_cap * cdef_cols],
            cdef_cols,
            cdef_cap,
            above_level: [
                vec![0; mi_cols + 32],
                vec![0; mi_cols + 32],
                vec![0; mi_cols + 32],
            ],
            above_dc: [
                vec![0; mi_cols + 32],
                vec![0; mi_cols + 32],
                vec![0; mi_cols + 32],
            ],
            lr_units,
            post,
            decoded: [vec![false; dsz], vec![false; dsz], vec![false; dsz]],
            frame,
            blk: Box::default(),
            color_map_y: vec![0; 64 * 64],
            color_map_uv: vec![0; 66 * 66],
            quant: vec![0; 1024],
            scratch: vec![0; 64 * 64],
            residual: vec![0; 64 * 64],
            read_deltas: false,
            seq,
            fh,
        }
    }

    #[inline]
    pub fn mi_idx(&self, row: usize, col: usize) -> usize {
        (row & (self.mi_cap - 1)) * self.mi_cols + col
    }

    #[inline]
    pub fn mi_at(&self, row: usize, col: usize) -> &Mi {
        &self.mi[self.mi_idx(row, col)]
    }

    #[inline]
    fn cdef_at(&mut self, row: usize, col: usize) -> &mut i8 {
        let i = ((row >> 4) % self.cdef_cap) * self.cdef_cols + (col >> 4);
        &mut self.cdef_idx[i]
    }

    pub fn cdef_get(&self, row: usize, col: usize) -> i8 {
        self.cdef_idx[((row >> 4) % self.cdef_cap) * self.cdef_cols + (col >> 4)]
    }

    /// Starts a tile: clears its above context.
    pub fn new_tile<'a>(&mut self, data: &'a [u8], row: usize, col: usize) -> Tile<'a> {
        let t = &self.fh.tile;
        let tile = Tile {
            sd: SymbolDecoder::new(data, self.fh.disable_cdf_update),
            cdf: Cdfs::new(self.fh.base_q_idx),
            mi_row_start: t.row_starts[row],
            mi_row_end: t.row_starts[row + 1],
            mi_col_start: t.col_starts[col],
            mi_col_end: t.col_starts[col + 1],
            current_q: self.fh.base_q_idx,
            delta_lf: [0; 4],
            ref_sgr_xqd: [[-32, 31]; 3],
            ref_lr_wiener: [[[3, -7, 15]; 2]; 3],
            left_level: [[0; 32]; 3],
            left_dc: [[0; 32]; 3],
        };
        for p in 0..self.planes {
            let sx = if p > 0 { self.ssx } else { 0 };
            let a = tile.mi_col_start >> sx;
            let b = (tile.mi_col_end >> sx)
                .max(a + 1)
                .min(self.above_level[p].len());
            self.above_level[p][a..b].fill(0);
            self.above_dc[p][a..b].fill(0);
        }
        tile
    }

    /// Decodes one superblock row of a tile (decode_tile's inner loop).
    pub fn decode_sb_row(&mut self, t: &mut Tile, r: usize) -> Result<(), Error> {
        t.left_level = [[0; 32]; 3];
        t.left_dc = [[0; 32]; 3];
        let mut c = t.mi_col_start;
        while c < t.mi_col_end {
            self.read_deltas = self.fh.delta_q_present;
            self.clear_cdef(r, c);
            self.clear_block_decoded(t, r, c);
            self.read_lr(t, r, c, self.sb_size);
            self.decode_partition(t, r, c, self.sb_size)?;
            c += self.sb4;
        }
        Ok(())
    }

    fn clear_cdef(&mut self, r: usize, c: usize) {
        *self.cdef_at(r, c) = -1;
        if self.seq.sb128 {
            if c + 16 < self.mi_cols {
                *self.cdef_at(r, c + 16) = -1;
            }
            *self.cdef_at(r + 16, c) = -1;
            if c + 16 < self.mi_cols {
                *self.cdef_at(r + 16, c + 16) = -1;
            }
        }
    }

    #[inline]
    fn dec_idx(&self, y: isize, x: isize) -> usize {
        ((y + 1) as usize) * (self.sb4 + 3) + (x + 1) as usize
    }

    #[inline]
    pub fn block_decoded(&self, plane: usize, y: isize, x: isize) -> bool {
        self.decoded[plane][self.dec_idx(y, x)]
    }

    pub fn set_block_decoded(&mut self, plane: usize, y: isize, x: isize) {
        let i = self.dec_idx(y, x);
        self.decoded[plane][i] = true;
    }

    fn clear_block_decoded(&mut self, t: &Tile, r: usize, c: usize) {
        let sb4 = self.sb4 as isize;
        for plane in 0..self.planes {
            let (sx, sy) = if plane > 0 {
                (self.ssx, self.ssy)
            } else {
                (0, 0)
            };
            let sbw4 = ((t.mi_col_end - c) >> sx) as isize;
            let sbh4 = ((t.mi_row_end - r) >> sy) as isize;
            for y in -1..=(sb4 >> sy) {
                for x in -1..=(sb4 >> sx) {
                    let v = if y < 0 && x < sbw4 {
                        true
                    } else {
                        x < 0 && y < sbh4
                    };
                    let i = self.dec_idx(y, x);
                    self.decoded[plane][i] = v;
                }
            }
            let i = self.dec_idx(sb4 >> sy, -1);
            self.decoded[plane][i] = false;
        }
    }

    // ---- loop restoration coefficients -----------------------------------

    fn read_lr(&mut self, t: &mut Tile, r: usize, c: usize, bsize: usize) {
        if self.fh.allow_intrabc {
            return;
        }
        let w = NUM_4X4_BLOCKS_WIDE[bsize] as usize;
        let h = NUM_4X4_BLOCKS_HIGH[bsize] as usize;
        for plane in 0..self.planes {
            if self.fh.lr_type[plane] == 0 {
                continue;
            }
            let (sx, sy) = if plane > 0 {
                (self.ssx, self.ssy)
            } else {
                (0, 0)
            };
            let unit_size = self.fh.lr_unit_size[plane];
            let (cols, rows, _) = self.lr_units[plane];
            let row_start = (r * (4 >> sy)).div_ceil(unit_size);
            let row_end = rows.min(((r + h) * (4 >> sy)).div_ceil(unit_size));
            let (num, den) = if self.fh.use_superres {
                ((4 >> sx) * self.fh.superres_denom, unit_size * 8)
            } else {
                (4 >> sx, unit_size)
            };
            let col_start = (c * num).div_ceil(den);
            let col_end = cols.min(((c + w) * num).div_ceil(den));
            for ur in row_start..row_end {
                for uc in col_start..col_end {
                    let u = self.read_lr_unit(t, plane);
                    self.lr_units[plane].2[ur * cols + uc] = u;
                }
            }
        }
    }

    fn read_lr_unit(&mut self, t: &mut Tile, plane: usize) -> LrUnit {
        let mut u = LrUnit::default();
        let ft = self.fh.lr_type[plane];
        u.kind = match ft {
            1 => {
                if t.sd.symbol(&mut t.cdf.use_wiener) == 1 {
                    1
                } else {
                    0
                }
            }
            2 => {
                if t.sd.symbol(&mut t.cdf.use_sgrproj) == 1 {
                    2
                } else {
                    0
                }
            }
            _ => t.sd.symbol(&mut t.cdf.restoration_type) as u8,
        };
        if u.kind == 1 {
            for pass in 0..2 {
                let first = if plane > 0 { 1 } else { 0 };
                for j in first..3 {
                    let min = WIENER_TAPS_MIN[j] as i32;
                    let max = WIENER_TAPS_MAX[j] as i32;
                    let k = WIENER_TAPS_K[j] as u32;
                    let v = decode_signed_subexp_bool(
                        &mut t.sd,
                        min,
                        max + 1,
                        k,
                        t.ref_lr_wiener[plane][pass][j],
                    );
                    u.wiener[pass][j] = v;
                    t.ref_lr_wiener[plane][pass][j] = v;
                }
            }
        } else if u.kind == 2 {
            let set = t.sd.literal(4) as usize;
            u.sgr_set = set as u8;
            for i in 0..2 {
                let radius = SGR_PARAMS[set][i * 2];
                let min = SGRPROJ_XQD_MIN[i] as i32;
                let max = SGRPROJ_XQD_MAX[i] as i32;
                let v = if radius != 0 {
                    decode_signed_subexp_bool(&mut t.sd, min, max + 1, 4, t.ref_sgr_xqd[plane][i])
                } else if i == 1 {
                    ((1 << 7) - t.ref_sgr_xqd[plane][0]).clamp(min, max)
                } else {
                    0
                };
                u.sgr_xqd[i] = v;
                t.ref_sgr_xqd[plane][i] = v;
            }
        }
        u
    }

    // ---- partitions -------------------------------------------------------

    fn is_inside(t: &Tile, r: isize, c: isize) -> bool {
        c >= t.mi_col_start as isize
            && c < t.mi_col_end as isize
            && r >= t.mi_row_start as isize
            && r < t.mi_row_end as isize
    }

    fn partition_cdf<'c>(cdf: &'c mut Cdfs, bsl: usize, ctx: usize) -> &'c mut [u16] {
        match bsl {
            1 => &mut cdf.part_w8[ctx],
            2 => &mut cdf.part_w16[ctx],
            3 => &mut cdf.part_w32[ctx],
            4 => &mut cdf.part_w64[ctx],
            _ => &mut cdf.part_w128[ctx],
        }
    }

    fn decode_partition(
        &mut self,
        t: &mut Tile,
        r: usize,
        c: usize,
        bsize: usize,
    ) -> Result<(), Error> {
        if r >= self.mi_rows || c >= self.mi_cols {
            return Ok(());
        }
        let avail_u = Self::is_inside(t, r as isize - 1, c as isize);
        let avail_l = Self::is_inside(t, r as isize, c as isize - 1);
        let num4 = NUM_4X4_BLOCKS_WIDE[bsize] as usize;
        let half = num4 >> 1;
        let quarter = half >> 1;
        let has_rows = (r + half) < self.mi_rows;
        let has_cols = (c + half) < self.mi_cols;
        let partition = if bsize < BLOCK_8X8 {
            PARTITION_NONE
        } else {
            let bsl = MI_WIDTH_LOG2[bsize];
            let above = avail_u && MI_WIDTH_LOG2[self.mi_at(r - 1, c).bsize as usize] < bsl;
            let left = avail_l && MI_HEIGHT_LOG2[self.mi_at(r, c - 1).bsize as usize] < bsl;
            let ctx = left as usize * 2 + above as usize;
            let bsl = bsl as usize;
            if has_rows && has_cols {
                t.sd.symbol(Self::partition_cdf(&mut t.cdf, bsl, ctx))
            } else if has_cols {
                // split_or_horz
                let pc = Self::partition_cdf(&mut t.cdf, bsl, ctx);
                let p =
                    |k: usize| -> u32 { pc[k] as u32 - if k > 0 { pc[k - 1] as u32 } else { 0 } };
                let mut psum = p(PARTITION_VERT)
                    + p(PARTITION_SPLIT)
                    + p(PARTITION_HORZ_A)
                    + p(PARTITION_VERT_A)
                    + p(PARTITION_VERT_B);
                if bsize != BLOCK_128X128 {
                    psum += p(PARTITION_VERT_4);
                }
                let mut cdf = [(32768 - psum) as u16, 32768, 0];
                if t.sd.symbol(&mut cdf) == 1 {
                    PARTITION_SPLIT
                } else {
                    PARTITION_HORZ
                }
            } else if has_rows {
                let pc = Self::partition_cdf(&mut t.cdf, bsl, ctx);
                let p =
                    |k: usize| -> u32 { pc[k] as u32 - if k > 0 { pc[k - 1] as u32 } else { 0 } };
                let mut psum = p(PARTITION_HORZ)
                    + p(PARTITION_SPLIT)
                    + p(PARTITION_HORZ_A)
                    + p(PARTITION_HORZ_B)
                    + p(PARTITION_VERT_A);
                if bsize != BLOCK_128X128 {
                    psum += p(PARTITION_HORZ_4);
                }
                let mut cdf = [(32768 - psum) as u16, 32768, 0];
                if t.sd.symbol(&mut cdf) == 1 {
                    PARTITION_SPLIT
                } else {
                    PARTITION_VERT
                }
            } else {
                PARTITION_SPLIT
            }
        };
        let sub = PARTITION_SUBSIZE[partition][bsize] as usize;
        let split = PARTITION_SUBSIZE[PARTITION_SPLIT][bsize] as usize;
        if sub as u8 == BLOCK_INVALID {
            return Err(Error::InvalidData("av1: invalid partition"));
        }
        match partition {
            PARTITION_NONE => self.decode_block(t, r, c, sub)?,
            PARTITION_HORZ => {
                self.decode_block(t, r, c, sub)?;
                if has_rows {
                    self.decode_block(t, r + half, c, sub)?;
                }
            }
            PARTITION_VERT => {
                self.decode_block(t, r, c, sub)?;
                if has_cols {
                    self.decode_block(t, r, c + half, sub)?;
                }
            }
            PARTITION_SPLIT => {
                self.decode_partition(t, r, c, sub)?;
                self.decode_partition(t, r, c + half, sub)?;
                self.decode_partition(t, r + half, c, sub)?;
                self.decode_partition(t, r + half, c + half, sub)?;
            }
            PARTITION_HORZ_A => {
                self.decode_block(t, r, c, split)?;
                self.decode_block(t, r, c + half, split)?;
                self.decode_block(t, r + half, c, sub)?;
            }
            PARTITION_HORZ_B => {
                self.decode_block(t, r, c, sub)?;
                self.decode_block(t, r + half, c, split)?;
                self.decode_block(t, r + half, c + half, split)?;
            }
            PARTITION_VERT_A => {
                self.decode_block(t, r, c, split)?;
                self.decode_block(t, r + half, c, split)?;
                self.decode_block(t, r, c + half, sub)?;
            }
            PARTITION_VERT_B => {
                self.decode_block(t, r, c, sub)?;
                self.decode_block(t, r, c + half, split)?;
                self.decode_block(t, r + half, c + half, split)?;
            }
            PARTITION_HORZ_4 => {
                for k in 0..4 {
                    let rr = r + quarter * k;
                    if k < 3 || rr < self.mi_rows {
                        self.decode_block(t, rr, c, sub)?;
                    }
                }
            }
            _ => {
                for k in 0..4 {
                    let cc = c + quarter * k;
                    if k < 3 || cc < self.mi_cols {
                        self.decode_block(t, r, cc, sub)?;
                    }
                }
            }
        }
        Ok(())
    }

    // ---- blocks -------------------------------------------------------------

    fn decode_block(&mut self, t: &mut Tile, r: usize, c: usize, size: usize) -> Result<(), Error> {
        let mut b = std::mem::take(&mut self.blk);
        b.mi_row = r;
        b.mi_col = c;
        b.size = size;
        b.bw4 = NUM_4X4_BLOCKS_WIDE[size] as usize;
        b.bh4 = NUM_4X4_BLOCKS_HIGH[size] as usize;
        let (bw4, bh4) = (b.bw4, b.bh4);
        b.has_chroma = if bh4 == 1 && self.ssy == 1 && (r & 1) == 0 {
            false
        } else if bw4 == 1 && self.ssx == 1 && (c & 1) == 0 {
            false
        } else {
            self.planes > 1
        };
        let (ri, ci) = (r as isize, c as isize);
        b.avail_u = Self::is_inside(t, ri - 1, ci);
        b.avail_l = Self::is_inside(t, ri, ci - 1);
        b.avail_u_chroma = b.avail_u;
        b.avail_l_chroma = b.avail_l;
        if b.has_chroma {
            if self.ssy == 1 && bh4 == 1 {
                b.avail_u_chroma = Self::is_inside(t, ri - 2, ci);
            }
            if self.ssx == 1 && bw4 == 1 {
                b.avail_l_chroma = Self::is_inside(t, ri, ci - 2);
            }
        } else {
            b.avail_u_chroma = false;
            b.avail_l_chroma = false;
        }
        let res = self.decode_block_inner(t, &mut b);
        self.blk = b;
        res
    }

    fn decode_block_inner(&mut self, t: &mut Tile, b: &mut Blk) -> Result<(), Error> {
        self.intra_frame_mode_info(t, b)?;
        b.cur_q = t.current_q;
        if b.is_inter {
            self.intrabc_mv(t, b);
        }
        self.palette_tokens(t, b);
        self.read_block_tx_size(t, b);
        let (r, c) = (b.mi_row, b.mi_col);
        if b.skip {
            self.reset_block_context(t, b);
        }
        // Store the mode info now (the prediction of later transform blocks
        // of this block looks at neighbours only, but palette caches and
        // contexts of following blocks read it).
        let x_end = (c + b.bw4).min(self.mi_cols);
        let y_end = (r + b.bh4).min(self.mi_rows);
        let mi = Mi {
            bsize: b.size as u8,
            y_mode: b.y_mode,
            uv_mode: b.uv_mode,
            skip: b.skip,
            is_inter: b.is_inter,
            seg: b.segment_id as u8,
            tx_size: b.tx_size as u8,
            lf_tx_y: b.tx_size as u8,
            lf_tx_uv: 0,
            pal_y: b.pal_size_y as u8,
            pal_uv: b.pal_size_uv as u8,
            delta_lf: [
                t.delta_lf[0] as i8,
                t.delta_lf[1] as i8,
                t.delta_lf[2] as i8,
                t.delta_lf[3] as i8,
            ],
            mv: [b.mv[0] as i16, b.mv[1] as i16],
            row_tag: 0,
        };
        for y in r..y_end {
            for x in c..x_end {
                let i = self.mi_idx(y, x);
                let old_uv = self.mi[i].lf_tx_uv;
                self.mi[i] = mi;
                self.mi[i].lf_tx_uv = old_uv;
                self.mi[i].row_tag = y as u32;
                self.mi[i].tx_size = b.inter_tx[(y - r) * 32 + (x - c)];
                if !self.pal.is_empty() {
                    self.pal[i] = [b.pal_y, b.pal_u];
                }
            }
        }
        if b.is_inter {
            self.predict_intrabc(b);
        }
        self.residual(t, b)
    }

    fn reset_block_context(&mut self, t: &mut Tile, b: &Blk) {
        let planes = if b.has_chroma { 3 } else { 1 };
        for plane in 0..planes {
            let (sx, sy) = if plane > 0 {
                (self.ssx, self.ssy)
            } else {
                (0, 0)
            };
            let a = b.mi_col >> sx;
            let e = (b.mi_col + b.bw4) >> sx;
            for i in a..e {
                self.above_level[plane][i] = 0;
                self.above_dc[plane][i] = 0;
            }
            let a = b.mi_row >> sy;
            let e = (b.mi_row + b.bh4) >> sy;
            for i in a..e {
                t.left_level[plane][i & 31] = 0;
                t.left_dc[plane][i & 31] = 0;
            }
        }
    }

    fn intra_frame_mode_info(&mut self, t: &mut Tile, b: &mut Blk) -> Result<(), Error> {
        b.skip = false;
        if self.fh.seg_id_pre_skip {
            self.intra_segment_id(t, b);
        }
        // read_skip()
        if self.fh.seg_id_pre_skip && self.fh.seg_feature_active_idx(b.segment_id, SEG_LVL_SKIP) {
            b.skip = true;
        } else {
            let mut ctx = 0;
            if b.avail_u {
                ctx += self.mi_at(b.mi_row - 1, b.mi_col).skip as usize;
            }
            if b.avail_l {
                ctx += self.mi_at(b.mi_row, b.mi_col - 1).skip as usize;
            }
            b.skip = t.sd.symbol(&mut t.cdf.skip[ctx]) == 1;
        }
        if !self.fh.seg_id_pre_skip {
            self.intra_segment_id(t, b);
        }
        self.read_cdef(t, b);
        self.read_delta_qindex(t, b);
        self.read_delta_lf(t, b);
        self.read_deltas = false;
        b.use_filter_intra = false;
        b.pal_size_y = 0;
        b.pal_size_uv = 0;
        b.angle_delta_y = 0;
        b.angle_delta_uv = 0;
        b.is_inter = false;
        b.uv_mode = DC_PRED;
        if self.fh.allow_intrabc && t.sd.symbol(&mut t.cdf.intrabc) == 1 {
            b.is_inter = true;
            b.y_mode = DC_PRED;
            b.uv_mode = DC_PRED;
            return Ok(());
        }
        let above = if b.avail_u {
            self.mi_at(b.mi_row - 1, b.mi_col).y_mode
        } else {
            DC_PRED
        };
        let left = if b.avail_l {
            self.mi_at(b.mi_row, b.mi_col - 1).y_mode
        } else {
            DC_PRED
        };
        let actx = INTRA_MODE_CONTEXT[above as usize] as usize;
        let lctx = INTRA_MODE_CONTEXT[left as usize] as usize;
        b.y_mode = t.sd.symbol(&mut t.cdf.y_mode[actx][lctx]) as u8;
        if b.size >= BLOCK_8X8 && is_directional(b.y_mode) {
            let v = t.sd.symbol(&mut t.cdf.angle_delta[b.y_mode as usize - 1]);
            b.angle_delta_y = v as i32 - 3;
        }
        if b.has_chroma {
            let cfl_allowed = if b.lossless && self.residual_size(b.size, 1) == BLOCK_4X4 {
                true
            } else {
                !b.lossless && block_width(b.size).max(block_height(b.size)) <= 32
            };
            b.uv_mode = if cfl_allowed {
                t.sd.symbol(&mut t.cdf.uv_cfl_allowed[b.y_mode as usize])
            } else {
                t.sd.symbol(&mut t.cdf.uv_cfl_not_allowed[b.y_mode as usize])
            } as u8;
            if b.uv_mode == UV_CFL_PRED {
                let signs = t.sd.symbol(&mut t.cdf.cfl_sign) as i32;
                let sign_u = (signs + 1) / 3;
                let sign_v = (signs + 1) % 3;
                b.cfl_alpha_u = 0;
                b.cfl_alpha_v = 0;
                if sign_u != 0 {
                    let ctx = ((sign_u - 1) * 3 + sign_v) as usize;
                    let a = 1 + t.sd.symbol(&mut t.cdf.cfl_alpha[ctx]) as i32;
                    b.cfl_alpha_u = if sign_u == 1 { -a } else { a };
                }
                if sign_v != 0 {
                    let ctx = ((sign_v - 1) * 3 + sign_u) as usize;
                    let a = 1 + t.sd.symbol(&mut t.cdf.cfl_alpha[ctx]) as i32;
                    b.cfl_alpha_v = if sign_v == 1 { -a } else { a };
                }
            }
            if b.size >= BLOCK_8X8 && is_directional(b.uv_mode) {
                let v = t.sd.symbol(&mut t.cdf.angle_delta[b.uv_mode as usize - 1]);
                b.angle_delta_uv = v as i32 - 3;
            }
        }
        if b.size >= BLOCK_8X8
            && block_width(b.size) <= 64
            && block_height(b.size) <= 64
            && self.fh.allow_screen_content_tools
        {
            self.palette_mode_info(t, b);
        }
        // filter_intra_mode_info()
        if self.seq.enable_filter_intra
            && b.y_mode == DC_PRED
            && b.pal_size_y == 0
            && block_width(b.size).max(block_height(b.size)) <= 32
        {
            b.use_filter_intra = t.sd.symbol(&mut t.cdf.filter_intra[b.size]) == 1;
            if b.use_filter_intra {
                b.filter_intra_mode = t.sd.symbol(&mut t.cdf.filter_intra_mode) as u8;
            }
        }
        Ok(())
    }

    fn intra_segment_id(&mut self, t: &mut Tile, b: &mut Blk) {
        if self.fh.segmentation_enabled {
            let (r, c) = (b.mi_row, b.mi_col);
            let prev_ul: i32 = if b.avail_u && b.avail_l {
                self.mi_at(r - 1, c - 1).seg as i32
            } else {
                -1
            };
            let prev_u: i32 = if b.avail_u {
                self.mi_at(r - 1, c).seg as i32
            } else {
                -1
            };
            let prev_l: i32 = if b.avail_l {
                self.mi_at(r, c - 1).seg as i32
            } else {
                -1
            };
            let pred = if prev_u == -1 {
                if prev_l == -1 {
                    0
                } else {
                    prev_l
                }
            } else if prev_l == -1 {
                prev_u
            } else if prev_ul == prev_u {
                prev_u
            } else {
                prev_l
            };
            if b.skip {
                b.segment_id = pred as usize;
            } else {
                let ctx = if prev_ul < 0 {
                    0
                } else if prev_ul == prev_u && prev_ul == prev_l {
                    2
                } else if prev_ul == prev_u || prev_ul == prev_l || prev_u == prev_l {
                    1
                } else {
                    0
                };
                let v = t.sd.symbol(&mut t.cdf.segment_id[ctx]) as i32;
                let max = self.fh.last_active_seg_id as i32 + 1;
                b.segment_id = neg_deinterleave(v, pred, max).clamp(0, 7) as usize;
            }
        } else {
            b.segment_id = 0;
        }
        b.lossless = self.fh.lossless[b.segment_id];
    }

    fn read_cdef(&mut self, t: &mut Tile, b: &Blk) {
        if b.skip || self.fh.coded_lossless || !self.seq.enable_cdef || self.fh.allow_intrabc {
            return;
        }
        let r = b.mi_row & !15;
        let c = b.mi_col & !15;
        if *self.cdef_at(r, c) == -1 {
            let v = t.sd.literal(self.fh.cdef_bits) as i8;
            let w4 = b.bw4;
            let h4 = b.bh4;
            let mut i = r;
            while i < r + h4 {
                let mut j = c;
                while j < c + w4 {
                    if i < self.mi_rows && j < self.mi_cols {
                        *self.cdef_at(i, j) = v;
                    }
                    j += 16;
                }
                i += 16;
            }
        }
    }

    fn read_delta_qindex(&mut self, t: &mut Tile, b: &Blk) {
        if b.size == self.sb_size && b.skip {
            return;
        }
        if self.read_deltas {
            let mut abs = t.sd.symbol(&mut t.cdf.delta_q) as i32;
            if abs == 3 {
                let rem = t.sd.literal(3) + 1;
                abs = t.sd.literal(rem) as i32 + (1 << rem) + 1;
            }
            if abs != 0 {
                let sign = t.sd.literal(1) == 1;
                let reduced = if sign { -abs } else { abs };
                t.current_q = (t.current_q + (reduced << self.fh.delta_q_res)).clamp(1, 255);
            }
        }
    }

    fn read_delta_lf(&mut self, t: &mut Tile, b: &Blk) {
        if b.size == self.sb_size && b.skip {
            return;
        }
        if self.read_deltas && self.fh.delta_lf_present {
            let count = if self.fh.delta_lf_multi {
                if self.planes > 1 {
                    4
                } else {
                    2
                }
            } else {
                1
            };
            for i in 0..count {
                let cdf = if self.fh.delta_lf_multi {
                    &mut t.cdf.delta_lf_multi[i]
                } else {
                    &mut t.cdf.delta_lf
                };
                let mut abs = t.sd.symbol(cdf) as i32;
                if abs == 3 {
                    let n = t.sd.literal(3) + 1;
                    abs = t.sd.literal(n) as i32 + (1 << n) + 1;
                }
                if abs != 0 {
                    let sign = t.sd.literal(1) == 1;
                    let reduced = if sign { -abs } else { abs };
                    t.delta_lf[i] =
                        (t.delta_lf[i] + (reduced << self.fh.delta_lf_res)).clamp(-63, 63);
                }
            }
        }
    }

    // ---- palettes -----------------------------------------------------------

    fn palette_cache(&self, b: &Blk, plane: usize, cache: &mut [u16; 16]) -> usize {
        let (r, c) = (b.mi_row, b.mi_col);
        let above_n = if (r * 4) % 64 != 0 {
            let m = self.mi_at(r - 1, c);
            if plane == 0 {
                m.pal_y
            } else {
                m.pal_uv
            }
        } else {
            0
        } as usize;
        let left_n = if b.avail_l {
            let m = self.mi_at(r, c - 1);
            if plane == 0 {
                m.pal_y
            } else {
                m.pal_uv
            }
        } else {
            0
        } as usize;
        let above: [u16; 8] = if above_n > 0 {
            self.pal[self.mi_idx(r - 1, c)][plane]
        } else {
            [0; 8]
        };
        let left: [u16; 8] = if left_n > 0 {
            self.pal[self.mi_idx(r, c - 1)][plane]
        } else {
            [0; 8]
        };
        let (mut ai, mut li, mut n) = (0, 0, 0);
        while ai < above_n && li < left_n {
            let (ac, lc) = (above[ai], left[li]);
            if lc < ac {
                if n == 0 || lc != cache[n - 1] {
                    cache[n] = lc;
                    n += 1;
                }
                li += 1;
            } else {
                if n == 0 || ac != cache[n - 1] {
                    cache[n] = ac;
                    n += 1;
                }
                ai += 1;
                if lc == ac {
                    li += 1;
                }
            }
        }
        while ai < above_n {
            let v = above[ai];
            ai += 1;
            if n == 0 || v != cache[n - 1] {
                cache[n] = v;
                n += 1;
            }
        }
        while li < left_n {
            let v = left[li];
            li += 1;
            if n == 0 || v != cache[n - 1] {
                cache[n] = v;
                n += 1;
            }
        }
        n
    }

    fn palette_mode_info(&mut self, t: &mut Tile, b: &mut Blk) {
        let bsize_ctx = (MI_WIDTH_LOG2[b.size] + MI_HEIGHT_LOG2[b.size]) as usize - 2;
        let bd = self.bd;
        let max = (1u32 << bd) - 1;
        let mut cache = [0u16; 16];
        if b.y_mode == DC_PRED {
            let mut ctx = 0;
            if b.avail_u && self.mi_at(b.mi_row - 1, b.mi_col).pal_y > 0 {
                ctx += 1;
            }
            if b.avail_l && self.mi_at(b.mi_row, b.mi_col - 1).pal_y > 0 {
                ctx += 1;
            }
            if t.sd.symbol(&mut t.cdf.palette_y_mode[bsize_ctx][ctx]) == 1 {
                let n = t.sd.symbol(&mut t.cdf.palette_y_size[bsize_ctx]) + 2;
                b.pal_size_y = n;
                let cache_n = self.palette_cache(b, 0, &mut cache);
                let mut idx = 0;
                let mut i = 0;
                while i < cache_n && idx < n {
                    if t.sd.literal(1) == 1 {
                        b.pal_y[idx] = cache[i];
                        idx += 1;
                    }
                    i += 1;
                }
                if idx < n {
                    b.pal_y[idx] = t.sd.literal(bd) as u16;
                    idx += 1;
                }
                let mut bits = 0;
                if idx < n {
                    bits = bd - 3 + t.sd.literal(2);
                }
                while idx < n {
                    let delta = t.sd.literal(bits) + 1;
                    let v = (b.pal_y[idx - 1] as u32 + delta).min(max);
                    b.pal_y[idx] = v as u16;
                    let range = (1u32 << bd) - v - 1;
                    bits = bits.min(ceil_log2(range));
                    idx += 1;
                }
                b.pal_y[..n].sort_unstable();
            }
        }
        if b.has_chroma && b.uv_mode == DC_PRED {
            let ctx = (b.pal_size_y > 0) as usize;
            if t.sd.symbol(&mut t.cdf.palette_uv_mode[ctx]) == 1 {
                let n = t.sd.symbol(&mut t.cdf.palette_uv_size[bsize_ctx]) + 2;
                b.pal_size_uv = n;
                let cache_n = self.palette_cache(b, 1, &mut cache);
                let mut idx = 0;
                let mut i = 0;
                while i < cache_n && idx < n {
                    if t.sd.literal(1) == 1 {
                        b.pal_u[idx] = cache[i];
                        idx += 1;
                    }
                    i += 1;
                }
                if idx < n {
                    b.pal_u[idx] = t.sd.literal(bd) as u16;
                    idx += 1;
                }
                let mut bits = 0;
                if idx < n {
                    bits = bd - 3 + t.sd.literal(2);
                }
                while idx < n {
                    let delta = t.sd.literal(bits);
                    let v = (b.pal_u[idx - 1] as u32 + delta).min(max);
                    b.pal_u[idx] = v as u16;
                    let range = (1u32 << bd) - v;
                    bits = bits.min(ceil_log2(range));
                    idx += 1;
                }
                b.pal_u[..n].sort_unstable();
                if t.sd.literal(1) == 1 {
                    let min_bits = bd - 4;
                    let max_val = 1i32 << bd;
                    let bits = min_bits + t.sd.literal(2);
                    b.pal_v[0] = t.sd.literal(bd) as u16;
                    for idx in 1..n {
                        let mut delta = t.sd.literal(bits) as i32;
                        if delta != 0 && t.sd.literal(1) == 1 {
                            delta = -delta;
                        }
                        let mut val = b.pal_v[idx - 1] as i32 + delta;
                        if val < 0 {
                            val += max_val;
                        }
                        if val >= max_val {
                            val -= max_val;
                        }
                        b.pal_v[idx] = val.clamp(0, max as i32) as u16;
                    }
                } else {
                    for idx in 0..n {
                        b.pal_v[idx] = t.sd.literal(bd) as u16;
                    }
                }
            }
        }
    }

    fn palette_tokens(&mut self, t: &mut Tile, b: &Blk) {
        let bh = block_height(b.size);
        let bw = block_width(b.size);
        let on_h = bh.min((self.mi_rows - b.mi_row) * 4);
        let on_w = bw.min((self.mi_cols - b.mi_col) * 4);
        if b.pal_size_y > 0 {
            let mut map = std::mem::take(&mut self.color_map_y);
            read_color_map(t, &mut map, 64, bw, bh, on_w, on_h, b.pal_size_y, true);
            self.color_map_y = map;
        }
        if b.pal_size_uv > 0 {
            let mut bw = bw >> self.ssx;
            let mut bh = bh >> self.ssy;
            let mut on_w = on_w >> self.ssx;
            let mut on_h = on_h >> self.ssy;
            if bw < 4 {
                bw += 2;
                on_w += 2;
            }
            if bh < 4 {
                bh += 2;
                on_h += 2;
            }
            let mut map = std::mem::take(&mut self.color_map_uv);
            read_color_map(t, &mut map, 66, bw, bh, on_w, on_h, b.pal_size_uv, false);
            self.color_map_uv = map;
        }
    }

    // ---- transform size -------------------------------------------------

    fn read_block_tx_size(&mut self, t: &mut Tile, b: &mut Blk) {
        let (bw4, bh4) = (b.bw4.min(32), b.bh4.min(32));
        if self.fh.tx_mode_select && b.size > BLOCK_4X4 && b.is_inter && !b.skip && !b.lossless {
            let max_tx = MAX_TX_SIZE_RECT[b.size] as usize;
            let tw4 = (TX_WIDTH[max_tx] / 4) as usize;
            let th4 = (TX_HEIGHT[max_tx] / 4) as usize;
            let mut row = b.mi_row;
            while row < b.mi_row + b.bh4 {
                let mut col = b.mi_col;
                while col < b.mi_col + b.bw4 {
                    self.read_var_tx(t, b, row, col, max_tx, 0);
                    col += tw4;
                }
                row += th4;
            }
            return;
        }
        self.read_tx_size(t, b);
        for y in 0..bh4 {
            b.inter_tx[y * 32..y * 32 + bw4].fill(b.tx_size as u8);
        }
    }

    fn read_tx_size(&mut self, t: &mut Tile, b: &mut Blk) {
        if b.lossless {
            b.tx_size = TX_4X4;
            return;
        }
        let max_rect = MAX_TX_SIZE_RECT[b.size] as usize;
        let max_depth = MAX_TX_DEPTH[b.size];
        b.tx_size = max_rect;
        let allow_select = !b.skip || !b.is_inter;
        if b.size > BLOCK_4X4 && allow_select && self.fh.tx_mode_select {
            let max_w = TX_WIDTH[max_rect] as usize;
            let max_h = TX_HEIGHT[max_rect] as usize;
            let (r, c) = (b.mi_row, b.mi_col);
            // get_above_tx_width / get_left_tx_height at the block edge.
            let above_w = if b.avail_u {
                let m = self.mi_at(r - 1, c);
                if m.is_inter {
                    block_width(m.bsize as usize)
                } else if m.skip && m.is_inter {
                    block_width(m.bsize as usize)
                } else {
                    TX_WIDTH[m.tx_size as usize] as usize
                }
            } else {
                0
            };
            let left_h = if b.avail_l {
                let m = self.mi_at(r, c - 1);
                if m.is_inter {
                    block_height(m.bsize as usize)
                } else {
                    TX_HEIGHT[m.tx_size as usize] as usize
                }
            } else {
                0
            };
            let ctx = (above_w >= max_w) as usize + (left_h >= max_h) as usize;
            let depth = match max_depth {
                4 => t.sd.symbol(&mut t.cdf.tx_64x64[ctx]),
                3 => t.sd.symbol(&mut t.cdf.tx_32x32[ctx]),
                2 => t.sd.symbol(&mut t.cdf.tx_16x16[ctx]),
                _ => t.sd.symbol(&mut t.cdf.tx_8x8[ctx]),
            };
            for _ in 0..depth {
                b.tx_size = SPLIT_TX_SIZE[b.tx_size] as usize;
            }
        }
    }

    pub fn residual_size(&self, size: usize, plane: usize) -> usize {
        let (sx, sy) = if plane > 0 {
            (self.ssx, self.ssy)
        } else {
            (0, 0)
        };
        SUBSAMPLED_SIZE[size][sx][sy] as usize
    }
}

fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        0
    } else {
        32 - (x - 1).leading_zeros()
    }
}

fn neg_deinterleave(diff: i32, r: i32, max: i32) -> i32 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        max - (diff + 1)
    }
}

fn decode_subexp_bool(sd: &mut SymbolDecoder, num_syms: i32, k: u32) -> i32 {
    let mut i = 0;
    let mut mk = 0;
    loop {
        let b2 = if i > 0 { k + i - 1 } else { k };
        let a = 1 << b2;
        if num_syms <= mk + 3 * a {
            return sd.ns((num_syms - mk) as u32) as i32 + mk;
        } else if sd.literal(1) == 1 {
            i += 1;
            mk += a;
        } else {
            return sd.literal(b2) as i32 + mk;
        }
    }
}

fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

fn decode_signed_subexp_bool(sd: &mut SymbolDecoder, low: i32, high: i32, k: u32, r: i32) -> i32 {
    let mx = high - low;
    let r = r - low;
    let v = decode_subexp_bool(sd, mx, k);
    let x = if (r << 1) <= mx {
        inverse_recenter(r, v)
    } else {
        mx - 1 - inverse_recenter(mx - 1 - r, v)
    };
    x + low
}

/// Reads a palette colour index map in wavefront order (palette_tokens).
#[allow(clippy::too_many_arguments)]
fn read_color_map(
    t: &mut Tile,
    map: &mut [u8],
    stride: usize,
    bw: usize,
    bh: usize,
    on_w: usize,
    on_h: usize,
    n: usize,
    luma: bool,
) {
    map[0] = t.sd.ns(n as u32) as u8;
    for i in 1..(on_h + on_w - 1) {
        let j_start = i.min(on_w - 1);
        let j_end = i.saturating_sub(on_h - 1);
        let mut j = j_start as isize;
        while j >= j_end as isize {
            let (r, c) = (i - j as usize, j as usize);
            // get_palette_color_context()
            let mut scores = [0u32; 8];
            let mut order = [0u8, 1, 2, 3, 4, 5, 6, 7];
            if c > 0 {
                scores[map[r * stride + c - 1] as usize] += 2;
            }
            if r > 0 && c > 0 {
                scores[map[(r - 1) * stride + c - 1] as usize] += 1;
            }
            if r > 0 {
                scores[map[(r - 1) * stride + c] as usize] += 2;
            }
            for i in 0..3 {
                let mut max_score = scores[i];
                let mut max_idx = i;
                for j in i + 1..n {
                    if scores[j] > max_score {
                        max_score = scores[j];
                        max_idx = j;
                    }
                }
                if max_idx != i {
                    let ms = scores[max_idx];
                    let mo = order[max_idx];
                    for k in (i + 1..=max_idx).rev() {
                        scores[k] = scores[k - 1];
                        order[k] = order[k - 1];
                    }
                    scores[i] = ms;
                    order[i] = mo;
                }
            }
            let hash = scores[0] * PALETTE_COLOR_HASH_MULTIPLIERS[0] as u32
                + scores[1] * PALETTE_COLOR_HASH_MULTIPLIERS[1] as u32
                + scores[2] * PALETTE_COLOR_HASH_MULTIPLIERS[2] as u32;
            let ctx = PALETTE_COLOR_CONTEXT[hash as usize] as usize;
            let cdf = &mut t.cdf;
            let idx = if luma {
                match n {
                    2 => t.sd.symbol(&mut cdf.pal_y2[ctx]),
                    3 => t.sd.symbol(&mut cdf.pal_y3[ctx]),
                    4 => t.sd.symbol(&mut cdf.pal_y4[ctx]),
                    5 => t.sd.symbol(&mut cdf.pal_y5[ctx]),
                    6 => t.sd.symbol(&mut cdf.pal_y6[ctx]),
                    7 => t.sd.symbol(&mut cdf.pal_y7[ctx]),
                    _ => t.sd.symbol(&mut cdf.pal_y8[ctx]),
                }
            } else {
                match n {
                    2 => t.sd.symbol(&mut cdf.pal_uv2[ctx]),
                    3 => t.sd.symbol(&mut cdf.pal_uv3[ctx]),
                    4 => t.sd.symbol(&mut cdf.pal_uv4[ctx]),
                    5 => t.sd.symbol(&mut cdf.pal_uv5[ctx]),
                    6 => t.sd.symbol(&mut cdf.pal_uv6[ctx]),
                    7 => t.sd.symbol(&mut cdf.pal_uv7[ctx]),
                    _ => t.sd.symbol(&mut cdf.pal_uv8[ctx]),
                }
            };
            map[r * stride + c] = order[idx];
            j -= 1;
        }
    }
    for i in 0..on_h {
        for j in on_w..bw {
            map[i * stride + j] = map[i * stride + on_w - 1];
        }
    }
    for i in on_h..bh {
        for j in 0..bw {
            map[i * stride + j] = map[(on_h - 1) * stride + j];
        }
    }
}
