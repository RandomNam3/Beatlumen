//! Layer III decoding: side information, scale factors, Huffman decoding
//! and requantization, stereo processing, reordering, alias reduction and
//! the hybrid (IMDCT) filterbank.
//!
//! Structure and numerical kernels are ported from minimp3
//! (<https://github.com/lieff/minimp3>, CC0 1.0 / public domain). Deviations:
//! Huffman region boundaries and count1 overrun handling follow the ISO
//! reference decoder, the dequantization tables are computed in f64, and
//! 8 kHz mixed blocks use the ISO band layout.

use crate::bits::BitReader;
use crate::header::FrameHeader;
use crate::huffman::{COUNT1_A, COUNT1_B, HUFF_TABS, LINBITS, TAB_INDEX};
use crate::synth::SynthState;
use crate::tables::*;

const SQRT2: f32 = std::f32::consts::SQRT_2;

/// Per-granule, per-channel side information.
#[derive(Clone, Copy, Default, Debug)]
struct Granule {
    part2_3_length: usize,
    big_values: usize,
    global_gain: i32,
    scalefac_compress: u32,
    window_switching: bool,
    block_type: u8,
    mixed: bool,
    table_select: [u8; 3],
    region0_count: usize,
    region1_count: usize,
    subblock_gain: [u8; 3],
    preflag: bool,
    scalefac_scale: bool,
    count1_table: bool,
    /// Scale factor selection info (granule 1 of MPEG-1 only).
    scfsi: u8,
}

impl Granule {
    #[inline]
    fn is_short(&self) -> bool {
        self.window_switching && self.block_type == 2
    }
}

#[derive(Default)]
struct SideInfo {
    main_data_begin: usize,
    ngr: usize,
    gr: [[Granule; 2]; 2],
}

fn read_side_info(br: &mut BitReader, h: &FrameHeader) -> Option<SideInfo> {
    let nch = h.channels();
    let mpeg1 = h.is_mpeg1();
    let mut si = SideInfo::default();
    let mut scfsi = [0u8; 2];
    if mpeg1 {
        si.main_data_begin = br.read(9) as usize;
        br.read(if nch == 1 { 5 } else { 3 });
        for s in scfsi.iter_mut().take(nch) {
            *s = br.read(4) as u8;
        }
        si.ngr = 2;
    } else {
        si.main_data_begin = br.read(8) as usize;
        br.read(nch as u32);
        si.ngr = 1;
    }
    for gr in 0..si.ngr {
        for (ch, g) in si.gr[gr].iter_mut().enumerate().take(nch) {
            g.part2_3_length = br.read(12) as usize;
            g.big_values = br.read(9) as usize;
            if g.big_values > 288 {
                return None;
            }
            g.global_gain = br.read(8) as i32;
            g.scalefac_compress = br.read(if mpeg1 { 4 } else { 9 });
            g.window_switching = br.read_bool();
            if g.window_switching {
                g.block_type = br.read(2) as u8;
                if g.block_type == 0 {
                    return None;
                }
                g.mixed = br.read_bool();
                g.table_select[0] = br.read(5) as u8;
                g.table_select[1] = br.read(5) as u8;
                for sg in g.subblock_gain.iter_mut() {
                    *sg = br.read(3) as u8;
                }
            } else {
                for ts in g.table_select.iter_mut() {
                    *ts = br.read(5) as u8;
                }
                g.region0_count = br.read(4) as usize;
                g.region1_count = br.read(3) as usize;
            }
            if mpeg1 {
                g.preflag = br.read_bool();
            }
            g.scalefac_scale = br.read_bool();
            g.count1_table = br.read_bool();
            g.scfsi = if gr == 1 && !g.is_short() {
                scfsi[ch]
            } else {
                0
            };
        }
    }
    Some(si)
}

/// Scale factor band layout of one granule.
struct Layout {
    widths: &'static [u8],
    n_long: usize,
    n_short: usize,
}

fn layout(g: &Granule, h: &FrameHeader) -> Layout {
    let row = sfb_row(h.sr_index_all());
    if g.is_short() {
        if g.mixed {
            let (w, n) = &SFB_MIXED[row];
            Layout {
                widths: &w[..*n],
                n_long: if h.is_mpeg1() { 8 } else { 6 },
                n_short: 30,
            }
        } else {
            Layout {
                widths: &SFB_SHORT[row],
                n_long: 0,
                n_short: 39,
            }
        }
    } else {
        Layout {
            widths: &SFB_LONG[row],
            n_long: 22,
            n_short: 0,
        }
    }
}

/// Reads the scale factors of one granule/channel and converts them to
/// per-band gains in `scf`. `ist_pos` keeps the raw values (used for
/// intensity positions and for scfsi reuse in granule 1).
fn decode_scalefactors(
    h: &FrameHeader,
    br: &mut BitReader,
    g: &Granule,
    lay: &Layout,
    ist_pos: &mut [u8; 39],
    right_intensity: bool,
    scf: &mut [f32; 40],
) {
    let part_row = if lay.n_short == 0 {
        0
    } else if lay.n_long > 0 {
        1
    } else {
        2
    };
    let mut sizes = [0u8; 4];
    let group;
    let mut scfsi: i32;
    let mut preflag = g.preflag;
    if h.is_mpeg1() {
        let part = SCFC_DECODE[(g.scalefac_compress & 15) as usize];
        sizes = [part >> 2, part >> 2, part & 3, part & 3];
        group = 0;
        scfsi = i32::from(g.scfsi);
    } else {
        let ist = usize::from(right_intensity);
        let mut sfc = (g.scalefac_compress >> ist) as i32;
        let mut k = ist * 12;
        for row in 0..3 {
            let mut modprod = 1i32;
            for i in (0..4).rev() {
                let m = i32::from(SCF_MOD[k + i]);
                sizes[i] = ((sfc / modprod) % m) as u8;
                modprod *= m;
            }
            k += 4;
            if sfc < modprod {
                // Rows 0 and 1 of the non-intensity table carry no preflag.
                preflag = !right_intensity && row == 2;
                break;
            }
            sfc -= modprod;
        }
        group = k / 4;
        scfsi = -16;
    }
    let counts = &SCF_PARTITIONS[part_row][group * 4..group * 4 + 4];
    let mut iscf = [0u8; 40];
    let mut idx = 0;
    for (&cnt, &bits) in counts.iter().zip(sizes.iter()) {
        let cnt = cnt as usize;
        if cnt == 0 {
            break;
        }
        if scfsi & 8 != 0 {
            iscf[idx..idx + cnt].copy_from_slice(&ist_pos[idx..idx + cnt]);
        } else if bits == 0 {
            ist_pos[idx..idx + cnt].fill(0);
        } else {
            let bits = u32::from(bits);
            let max_scf = if scfsi < 0 {
                (1u32 << bits) - 1
            } else {
                u32::MAX
            };
            for k in 0..cnt {
                let s = br.read(bits);
                ist_pos[idx + k] = if s == max_scf { 255 } else { s as u8 };
                iscf[idx + k] = s as u8;
            }
        }
        idx += cnt;
        scfsi = scfsi.wrapping_mul(2);
    }

    let shift = 1 + u32::from(g.scalefac_scale);
    if lay.n_short > 0 {
        let sh = 3 - shift;
        for i in (0..lay.n_short).step_by(3) {
            for w in 0..3 {
                iscf[lay.n_long + i + w] += g.subblock_gain[w] << sh;
            }
        }
    } else if preflag {
        for (i, &p) in PRETAB.iter().enumerate() {
            iscf[11 + i] += p;
        }
    }
    let gain_exp = g.global_gain - 214 - if h.ms_stereo() { 2 } else { 0 };
    for (s, &is) in scf.iter_mut().zip(iscf.iter()).take(lay.widths.len()) {
        *s = pow2_quarter(gain_exp - (i32::from(is) << shift));
    }
}

/// Line index where long band `n` starts (576 past the end).
fn long_bound(row: &[u8; 22], n: usize) -> usize {
    if n >= 22 {
        576
    } else {
        row[..n].iter().map(|&w| w as usize).sum()
    }
}

/// Decodes the Huffman-coded spectrum of one granule/channel and
/// requantizes it into `dst` (576 lines, pre-zeroed). `limit` is the bit
/// position where this granule's part 2/3 data ends.
#[allow(clippy::too_many_arguments)]
fn huffman(
    dst: &mut [f32],
    br: &mut BitReader,
    g: &Granule,
    lay: &Layout,
    long_row: &[u8; 22],
    scf: &[f32; 40],
    limit: usize,
    pow43: &[f32; POW43_LEN],
) {
    let bv_end = (g.big_values * 2).min(576);
    let (r1, r2) = if g.window_switching {
        let r1 = if g.block_type != 2 {
            long_bound(long_row, 8)
        } else if g.mixed {
            lay.widths[..lay.n_long].iter().map(|&w| w as usize).sum()
        } else {
            // Three short bands (nine window entries), as LAME and minimp3
            // use: 36 lines except at 8 kHz (72).
            lay.widths[..9].iter().map(|&w| w as usize).sum()
        };
        (r1, 576)
    } else {
        (
            long_bound(long_row, g.region0_count + 1),
            long_bound(long_row, g.region0_count + g.region1_count + 2),
        )
    };
    let widths = lay.widths;
    let mut sfb = 0usize;
    let mut sfb_end = widths[0] as usize;
    let mut scale = scf[0];
    let mut pos = 0usize;

    while pos < bv_end {
        while pos >= sfb_end && sfb + 1 < widths.len() {
            sfb += 1;
            sfb_end += widths[sfb] as usize;
            scale = scf[sfb];
        }
        let (tab, region_end) = if pos < r1 {
            (g.table_select[0], r1)
        } else if pos < r2 {
            (g.table_select[1], r2)
        } else {
            (g.table_select[2], 576)
        };
        let seg_end = bv_end.min(region_end).min(sfb_end).max(pos + 2);
        let base = TAB_INDEX[tab as usize] as usize;
        let linbits = u32::from(LINBITS[tab as usize]);
        while pos < seg_end {
            br.refill();
            let mut leaf = i32::from(HUFF_TABS[base + br.peek(5) as usize]);
            let mut w = 5;
            while leaf < 0 {
                br.skip(w);
                w = (leaf & 7) as u32;
                leaf = i32::from(HUFF_TABS[base + br.peek(w) as usize + (-(leaf >> 3)) as usize]);
            }
            br.skip((leaf >> 8) as u32);
            for j in 0..2 {
                let mut v = ((leaf >> (4 * j)) & 15) as usize;
                if v == 0 {
                    dst[pos + j] = 0.0;
                    continue;
                }
                if v == 15 && linbits > 0 {
                    v += br.peek(linbits) as usize;
                    br.skip(linbits);
                }
                let m = pow43[v] * scale;
                dst[pos + j] = if br.top_bit() { -m } else { m };
                br.skip(1);
            }
            pos += 2;
        }
    }

    // count1 region: quadruples of values in {-1, 0, 1}.
    let table: &[u8] = if g.count1_table { &COUNT1_B } else { &COUNT1_A };
    while pos + 4 <= 576 {
        if br.position() >= limit {
            break;
        }
        br.refill();
        let mut leaf = table[br.peek(4) as usize];
        if leaf & 8 == 0 {
            let extra = u32::from(leaf & 3);
            let idx = (leaf >> 3) as usize + (br.peek(4 + extra) & ((1 << extra) - 1)) as usize;
            leaf = table[idx];
        }
        br.skip(u32::from(leaf & 7));
        let mut v = [0f32; 4];
        for (s, val) in v.iter_mut().enumerate() {
            if leaf & (128 >> s) != 0 {
                *val = if br.top_bit() { -1.0 } else { 1.0 };
                br.skip(1);
            }
        }
        if br.position() > limit {
            // The last quadruple overran the granule: discard it (ISO).
            break;
        }
        for half in 0..2 {
            let p = pos + 2 * half;
            while p >= sfb_end && sfb + 1 < widths.len() {
                sfb += 1;
                sfb_end += widths[sfb] as usize;
                scale = scf[sfb];
            }
            dst[p] = v[2 * half] * scale;
            dst[p + 1] = v[2 * half + 1] * scale;
        }
        pos += 4;
    }
}

fn midside(buf: &mut [f32; 1152], start: usize, n: usize) {
    let (l, r) = buf.split_at_mut(576);
    for (a, b) in l[start..start + n]
        .iter_mut()
        .zip(r[start..start + n].iter_mut())
    {
        let (x, y) = (*a, *b);
        *a = x + y;
        *b = x - y;
    }
}

/// Intensity stereo (with M/S for non-intensity bands when enabled).
fn intensity_stereo(
    buf: &mut [f32; 1152],
    ist_pos: &mut [u8; 39],
    lay: &Layout,
    h: &FrameHeader,
    right: &Granule,
) {
    let widths = lay.widths;
    let n_sfb = widths.len();
    let mut max_band = [-1i32; 3];
    let mut off = 576;
    for (i, &w) in widths.iter().enumerate() {
        let w = w as usize;
        if buf[off..off + w].iter().any(|&x| x != 0.0) {
            max_band[i % 3] = i as i32;
        }
        off += w;
    }
    if lay.n_long > 0 {
        let m = max_band[0].max(max_band[1]).max(max_band[2]);
        max_band = [m; 3];
    }
    let max_blocks = if lay.n_short > 0 { 3 } else { 1 };
    let default_pos = if h.is_mpeg1() { 3 } else { 0 };
    for (i, &mb) in max_band.iter().enumerate().take(max_blocks) {
        let itop = n_sfb - max_blocks + i;
        let prev = itop - max_blocks;
        ist_pos[itop] = if mb >= prev as i32 {
            default_pos
        } else {
            ist_pos[prev]
        };
    }

    let mpeg1 = h.is_mpeg1();
    let max_pos = if mpeg1 { 7 } else { 64 };
    let lsf_shift = right.scalefac_compress & 1;
    let ms = h.ms_stereo();
    let s = if ms { SQRT2 } else { 1.0 };
    let mut off = 0;
    for (i, &w) in widths.iter().enumerate() {
        let w = w as usize;
        let ipos = u32::from(ist_pos[i]);
        if i as i32 > max_band[i % 3] && ipos < max_pos {
            let (kl, kr) = if mpeg1 {
                (PAN[2 * ipos as usize], PAN[2 * ipos as usize + 1])
            } else {
                let k = pow2_quarter(-((((ipos + 1) >> 1) << lsf_shift) as i32));
                if ipos & 1 != 0 {
                    (k, 1.0)
                } else {
                    (1.0, k)
                }
            };
            let (kl, kr) = (kl * s, kr * s);
            let (l, r) = buf.split_at_mut(576);
            for (a, b) in l[off..off + w].iter_mut().zip(r[off..off + w].iter_mut()) {
                *b = *a * kr;
                *a *= kl;
            }
        } else if ms {
            midside(buf, off, w);
        }
        off += w;
    }
}

/// Reorders short-block lines from band/window/frequency order into
/// subband order with interleaved windows.
fn reorder(g: &mut [f32], scratch: &mut [f32; 576], short_widths: &[u8]) {
    let mut src = 0;
    let mut dst = 0;
    for tri in short_widths.chunks_exact(3) {
        let len = tri[0] as usize;
        for i in 0..len {
            scratch[dst] = g[src + i];
            scratch[dst + 1] = g[src + len + i];
            scratch[dst + 2] = g[src + 2 * len + i];
            dst += 3;
        }
        src += 3 * len;
    }
    g[..dst].copy_from_slice(&scratch[..dst]);
}

fn antialias(g: &mut [f32], nbands: usize) {
    for b in 0..nbands {
        let base = b * 18;
        for i in 0..8 {
            let u = g[base + 18 + i];
            let d = g[base + 17 - i];
            g[base + 18 + i] = u * AA[0][i] - d * AA[1][i];
            g[base + 17 - i] = u * AA[1][i] + d * AA[0][i];
        }
    }
}

fn dct3_9(y: &mut [f32; 9]) {
    let (mut s0, mut s2, mut s4, mut s6, mut s8) = (y[0], y[2], y[4], y[6], y[8]);
    let mut t0 = s0 + s6 * 0.5;
    s0 -= s6;
    let mut t4 = (s4 + s2) * 0.93969262;
    let mut t2 = (s8 + s2) * 0.76604444;
    s6 = (s4 - s8) * 0.17364818;
    s4 += s8 - s2;

    s2 = s0 - s4 * 0.5;
    y[4] = s4 + s0;
    s8 = t0 - t2 + s6;
    s0 = t0 - t4 + t2;
    s4 = t0 + t4 - s6;

    let (mut s1, mut s3, mut s5, mut s7) = (y[1], y[3], y[5], y[7]);
    s3 *= 0.86602540;
    t0 = (s5 + s1) * 0.98480775;
    t4 = (s5 - s7) * 0.34202014;
    t2 = (s1 + s7) * 0.64278761;
    s1 = (s1 - s5 - s7) * 0.86602540;

    s5 = t0 - s3 - t2;
    s7 = t4 - s3 - t0;
    s3 = t4 + s3 - t2;

    y[0] = s4 - s7;
    y[1] = s2 + s1;
    y[2] = s0 - s3;
    y[3] = s8 + s5;
    y[5] = s8 - s5;
    y[6] = s0 + s3;
    y[7] = s2 - s1;
    y[8] = s4 + s7;
}

fn imdct36(g: &mut [f32], overlap: &mut [f32], window: &[f32; 18], nbands: usize) {
    for (gb, ov) in g
        .chunks_exact_mut(18)
        .zip(overlap.chunks_exact_mut(9))
        .take(nbands)
    {
        let mut co = [0f32; 9];
        let mut si = [0f32; 9];
        co[0] = -gb[0];
        si[0] = gb[17];
        for i in 0..4 {
            si[8 - 2 * i] = gb[4 * i + 1] - gb[4 * i + 2];
            co[1 + 2 * i] = gb[4 * i + 1] + gb[4 * i + 2];
            si[7 - 2 * i] = gb[4 * i + 4] - gb[4 * i + 3];
            co[2 + 2 * i] = -(gb[4 * i + 3] + gb[4 * i + 4]);
        }
        dct3_9(&mut co);
        dct3_9(&mut si);
        si[1] = -si[1];
        si[3] = -si[3];
        si[5] = -si[5];
        si[7] = -si[7];
        for i in 0..9 {
            let ovl = ov[i];
            let sum = co[i] * TWID9[9 + i] + si[i] * TWID9[i];
            ov[i] = co[i] * TWID9[i] - si[i] * TWID9[9 + i];
            gb[i] = ovl * window[i] - sum * window[9 + i];
            gb[17 - i] = ovl * window[9 + i] + sum * window[i];
        }
    }
}

#[inline]
fn idct3(x0: f32, x1: f32, x2: f32) -> [f32; 3] {
    let m1 = x1 * 0.86602540;
    let a1 = x0 - x2 * 0.5;
    [a1 + m1, x0 + x2, a1 - m1]
}

/// 12-point IMDCT of one short window; `x` holds the window's 6 lines at
/// stride 3.
fn imdct12(x: &[f32], dst: &mut [f32], overlap: &mut [f32]) {
    let co = idct3(-x[0], x[6] + x[3], x[12] + x[9]);
    let mut si = idct3(x[15], x[12] - x[9], x[6] - x[3]);
    si[1] = -si[1];
    for i in 0..3 {
        let ovl = overlap[i];
        let sum = co[i] * TWID3[3 + i] + si[i] * TWID3[i];
        overlap[i] = co[i] * TWID3[i] - si[i] * TWID3[3 + i];
        dst[i] = ovl * TWID3[2 - i] - sum * TWID3[5 - i];
        dst[5 - i] = ovl * TWID3[5 - i] + sum * TWID3[2 - i];
    }
}

fn imdct_short(g: &mut [f32], overlap: &mut [f32], nbands: usize) {
    for (gb, ov) in g
        .chunks_exact_mut(18)
        .zip(overlap.chunks_exact_mut(9))
        .take(nbands)
    {
        let mut tmp = [0f32; 18];
        tmp.copy_from_slice(gb);
        gb[..6].copy_from_slice(&ov[..6]);
        let (ov_out, ov_state) = ov.split_at_mut(6);
        imdct12(&tmp, &mut gb[6..12], ov_state);
        imdct12(&tmp[1..], &mut gb[12..18], ov_state);
        imdct12(&tmp[2..], ov_out, ov_state);
    }
}

fn change_sign(g: &mut [f32]) {
    for b in (1..32).step_by(2) {
        for i in (1..18).step_by(2) {
            g[b * 18 + i] = -g[b * 18 + i];
        }
    }
}

/// Hybrid filterbank of one granule/channel (576 lines in `g`).
fn hybrid(
    g: &mut [f32],
    overlap: &mut [f32; 288],
    gr: &Granule,
    lay: &Layout,
    h: &FrameHeader,
    scratch: &mut [f32; 576],
) {
    let n_long_bands = if gr.mixed {
        if h.sr_index_all() == 2 {
            4
        } else {
            2
        }
    } else {
        0
    };
    let aa_bands = if gr.is_short() {
        reorder(
            &mut g[n_long_bands * 18..],
            scratch,
            &lay.widths[lay.n_long..],
        );
        n_long_bands.saturating_sub(1)
    } else {
        31
    };
    antialias(g, aa_bands);
    if n_long_bands > 0 {
        imdct36(
            &mut g[..n_long_bands * 18],
            &mut overlap[..n_long_bands * 9],
            &MDCT_WINDOW[0],
            n_long_bands,
        );
    }
    let (g_rest, ov_rest) = (
        &mut g[n_long_bands * 18..],
        &mut overlap[n_long_bands * 9..],
    );
    if gr.is_short() {
        imdct_short(g_rest, ov_rest, 32 - n_long_bands);
    } else {
        let w = &MDCT_WINDOW[usize::from(gr.window_switching && gr.block_type == 3)];
        imdct36(g_rest, ov_rest, w, 32 - n_long_bands);
    }
    change_sign(g);
}

/// Persistent Layer III state: IMDCT overlap and the bit reservoir.
pub(crate) struct Layer3 {
    overlap: Box<[[f32; 288]; 2]>,
    reservoir: Vec<u8>,
    maindata: Vec<u8>,
    ist_pos: [[u8; 39]; 2],
    scf: [f32; 40],
    scratch: Box<[f32; 576]>,
}

impl Layer3 {
    pub fn new() -> Self {
        Layer3 {
            overlap: Box::new([[0.0; 288]; 2]),
            reservoir: Vec::with_capacity(1024),
            maindata: Vec::with_capacity(4096),
            ist_pos: [[0; 39]; 2],
            scf: [0.0; 40],
            scratch: Box::new([0.0; 576]),
        }
    }

    pub fn reset(&mut self) {
        self.overlap.iter_mut().for_each(|o| o.fill(0.0));
        self.reservoir.clear();
    }

    /// Decodes one Layer III frame into `pcm` (`spf * channels` interleaved
    /// samples). Returns false if (part of) the frame could not be decoded
    /// and was replaced by silence.
    pub fn decode_frame(
        &mut self,
        syn: &mut SynthState,
        h: &FrameHeader,
        frame: &[u8],
        pcm: &mut [f32],
    ) -> bool {
        let nch = h.channels();
        let hdr_len = 4 + if h.crc { 2 } else { 0 };
        let main_start = hdr_len + h.side_info_len();
        let pow43 = pow43();
        let row = sfb_row(h.sr_index_all());

        let si = frame
            .get(hdr_len..main_start)
            .and_then(|b| read_side_info(&mut BitReader::new(b), h));
        let main = frame.get(main_start..).unwrap_or(&[]);

        let Some(si) = si else {
            // Corrupt side info: keep the reservoir consistent, output a
            // decaying (zero-input) granule sequence.
            self.push_reservoir(main);
            let ngr = if h.is_mpeg1() { 2 } else { 1 };
            let g = Granule::default();
            for gr in 0..ngr {
                syn.grbuf.fill(0.0);
                for ch in 0..nch {
                    let lay = layout(&g, h);
                    hybrid(
                        &mut syn.grbuf[ch * 576..ch * 576 + 576],
                        &mut self.overlap[ch],
                        &g,
                        &lay,
                        h,
                        &mut self.scratch,
                    );
                }
                syn.run(18, nch, &mut pcm[gr * 576 * nch..(gr + 1) * 576 * nch]);
            }
            return false;
        };

        let mdb = si.main_data_begin;
        let have = mdb.min(self.reservoir.len());
        let missing_bits = (mdb - have) * 8;
        self.maindata.clear();
        self.maindata
            .extend_from_slice(&self.reservoir[self.reservoir.len() - have..]);
        self.maindata.extend_from_slice(main);
        let total_bits: usize = (0..si.ngr)
            .flat_map(|gr| (0..nch).map(move |c| (gr, c)))
            .map(|(gr, c)| si.gr[gr][c].part2_3_length)
            .sum();
        let valid = total_bits <= (mdb + main.len()) * 8;
        self.push_reservoir(main);

        let mut ok = valid && missing_bits == 0;
        let mut bitpos = 0usize;
        for gr in 0..si.ngr {
            syn.grbuf.fill(0.0);
            let gr_bits: usize = si.gr[gr].iter().take(nch).map(|g| g.part2_3_length).sum();
            if valid && bitpos >= missing_bits {
                let mut p = bitpos - missing_bits;
                for ch in 0..nch {
                    let g = &si.gr[gr][ch];
                    let lay = layout(g, h);
                    let limit = p + g.part2_3_length;
                    let mut br = BitReader::at(&self.maindata, p);
                    let right_is = ch == 1 && h.i_stereo();
                    decode_scalefactors(
                        h,
                        &mut br,
                        g,
                        &lay,
                        &mut self.ist_pos[ch],
                        right_is,
                        &mut self.scf,
                    );
                    huffman(
                        &mut syn.grbuf[ch * 576..ch * 576 + 576],
                        &mut br,
                        g,
                        &lay,
                        &SFB_LONG[row],
                        &self.scf,
                        limit,
                        pow43,
                    );
                    p = limit;
                }
                if nch == 2 {
                    if h.i_stereo() {
                        let lay = layout(&si.gr[gr][0], h);
                        intensity_stereo(
                            &mut syn.grbuf,
                            &mut self.ist_pos[1],
                            &lay,
                            h,
                            &si.gr[gr][1],
                        );
                    } else if h.ms_stereo() {
                        midside(&mut syn.grbuf, 0, 576);
                    }
                }
            } else {
                ok = false;
            }
            bitpos += gr_bits;
            for ch in 0..nch {
                let g = &si.gr[gr][ch];
                let lay = layout(g, h);
                hybrid(
                    &mut syn.grbuf[ch * 576..ch * 576 + 576],
                    &mut self.overlap[ch],
                    g,
                    &lay,
                    h,
                    &mut self.scratch,
                );
            }
            syn.run(18, nch, &mut pcm[gr * 576 * nch..(gr + 1) * 576 * nch]);
        }
        ok
    }

    fn push_reservoir(&mut self, main: &[u8]) {
        const KEEP: usize = 511;
        if main.len() >= KEEP {
            self.reservoir.clear();
            self.reservoir.extend_from_slice(&main[main.len() - KEEP..]);
        } else {
            self.reservoir.extend_from_slice(main);
            if self.reservoir.len() > KEEP {
                let excess = self.reservoir.len() - KEEP;
                self.reservoir.drain(..excess);
            }
        }
    }
}
