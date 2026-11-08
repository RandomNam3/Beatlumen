//! WebP lossless (VP8L) decoder, written from RFC 9649 section 3:
//! transforms (predictor, color, subtract green, color indexing), color
//! cache, meta prefix codes and LZ77 backward references.

use crate::huffman::{HuffTable, INVALID};
use crate::inflate::BitReader;
use crate::Error;

const ALPHABET_LEN_DIST: usize = 40;
const NUM_LENGTH_CODES: usize = 24;
const CL_ORDER: [usize; 19] = [
    17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15,
];

/// (xi, yi) offsets for distance codes 1..=120.
const DIST_MAP: [(i8, i8); 120] = [
    (0, 1),
    (1, 0),
    (1, 1),
    (-1, 1),
    (0, 2),
    (2, 0),
    (1, 2),
    (-1, 2),
    (2, 1),
    (-2, 1),
    (2, 2),
    (-2, 2),
    (0, 3),
    (3, 0),
    (1, 3),
    (-1, 3),
    (3, 1),
    (-3, 1),
    (2, 3),
    (-2, 3),
    (3, 2),
    (-3, 2),
    (0, 4),
    (4, 0),
    (1, 4),
    (-1, 4),
    (4, 1),
    (-4, 1),
    (3, 3),
    (-3, 3),
    (2, 4),
    (-2, 4),
    (4, 2),
    (-4, 2),
    (0, 5),
    (3, 4),
    (-3, 4),
    (4, 3),
    (-4, 3),
    (5, 0),
    (1, 5),
    (-1, 5),
    (5, 1),
    (-5, 1),
    (2, 5),
    (-2, 5),
    (5, 2),
    (-5, 2),
    (4, 4),
    (-4, 4),
    (3, 5),
    (-3, 5),
    (5, 3),
    (-5, 3),
    (0, 6),
    (6, 0),
    (1, 6),
    (-1, 6),
    (6, 1),
    (-6, 1),
    (2, 6),
    (-2, 6),
    (6, 2),
    (-6, 2),
    (4, 5),
    (-4, 5),
    (5, 4),
    (-5, 4),
    (3, 6),
    (-3, 6),
    (6, 3),
    (-6, 3),
    (0, 7),
    (7, 0),
    (1, 7),
    (-1, 7),
    (5, 5),
    (-5, 5),
    (7, 1),
    (-7, 1),
    (4, 6),
    (-4, 6),
    (6, 4),
    (-6, 4),
    (2, 7),
    (-2, 7),
    (7, 2),
    (-7, 2),
    (3, 7),
    (-3, 7),
    (7, 3),
    (-7, 3),
    (5, 6),
    (-5, 6),
    (6, 5),
    (-6, 5),
    (8, 0),
    (4, 7),
    (-4, 7),
    (7, 4),
    (-7, 4),
    (8, 1),
    (8, 2),
    (6, 6),
    (-6, 6),
    (8, 3),
    (5, 7),
    (-5, 7),
    (7, 5),
    (-7, 5),
    (8, 4),
    (6, 7),
    (-6, 7),
    (7, 6),
    (-7, 6),
    (8, 5),
    (7, 7),
    (-7, 7),
    (8, 6),
    (8, 7),
];

/// Reads the size from a VP8L bitstream header.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), Error> {
    if data.len() < 5 || data[0] != 0x2f {
        return Err(Error::InvalidData("webp: bad VP8L signature"));
    }
    let mut br = BitReader::new(&data[1..]);
    let w = br.bits(14) + 1;
    let h = br.bits(14) + 1;
    Ok((w, h))
}

/// Decodes a complete VP8L bitstream (starting with the 0x2f signature)
/// row by row.
pub fn rows(data: &[u8]) -> Result<Rows<'_>, Error> {
    if data.len() < 5 || data[0] != 0x2f {
        return Err(Error::InvalidData("webp: bad VP8L signature"));
    }
    let mut br = BitReader::new(&data[1..]);
    let w = br.bits(14) as usize + 1;
    let h = br.bits(14) as usize + 1;
    let _alpha_used = br.bits(1);
    if br.bits(3) != 0 {
        return Err(Error::Unsupported("webp: unknown VP8L version"));
    }
    if w as u64 * h as u64 > crate::MAX_SOURCE_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    if (data.len() as u64) << 21 < w as u64 * h as u64 {
        return Err(Error::InvalidData(
            "webp: stream far too short for dimensions",
        ));
    }
    Rows::new(br, w, h, true)
}

/// Decodes a headerless image stream of known size (ALPH chunk payload)
/// row by row.
pub fn rows_headerless(data: &[u8], w: usize, h: usize) -> Result<Rows<'_>, Error> {
    Rows::new(BitReader::new(data), w, h, true)
}

enum Transform {
    Predictor {
        bits: u32,
        xsize: usize,
        data: Vec<u32>,
        /// Output of this stage for the previous row.
        prev: Vec<u32>,
    },
    Color {
        bits: u32,
        xsize: usize,
        data: Vec<u32>,
    },
    SubtractGreen,
    ColorIndexing {
        xsize: usize,
        palette: Vec<u32>,
    },
}

/// Largest LZ77 distance (distance prefix 39 minus the 120 plane codes,
/// or 7 rows plus 8 pixels of the widest image).
const MAX_DISTANCE: usize = (1 << 20) + (1 << 17);

/// Incremental decoder of an image stream: entropy-coded pixels go into a
/// ring buffer that covers the longest backward reference, and each
/// completed row runs through the inverse transforms. Memory stays a few
/// MiB however large the image.
pub struct Rows<'a> {
    br: BitReader<'a>,
    /// Width of the coded (possibly palette-packed) pixels.
    width: usize,
    xsize: usize,
    ysize: usize,
    transforms: Vec<Transform>,
    groups: Vec<Group>,
    meta: Option<(u32, usize, Vec<u32>)>,
    cache: Vec<u32>,
    cache_shift: u32,
    ring: Vec<u32>,
    mask: usize,
    /// Coded pixels decoded so far, entered into the cache, delivered.
    pos: usize,
    /// Column and row of `pos`.
    cx: usize,
    cy: usize,
    last_cached: usize,
    y: usize,
    gi: usize,
    error: Option<Error>,
    row: Vec<u32>,
    tmp: Vec<u32>,
}

impl<'a> Rows<'a> {
    fn new(mut br: BitReader<'a>, xsize: usize, ysize: usize, level0: bool) -> Result<Self, Error> {
        let mut transforms: Vec<Transform> = Vec::new();
        let mut width = xsize;
        if level0 {
            let mut seen = [false; 4];
            while br.bits(1) == 1 {
                let ty = br.bits(2) as usize;
                if seen[ty] {
                    return Err(Error::InvalidData("webp: repeated transform"));
                }
                seen[ty] = true;
                match ty {
                    0 | 1 => {
                        let bits = br.bits(3) + 2;
                        let data = sub_image(
                            &mut br,
                            div_round_up(width, bits),
                            div_round_up(ysize, bits),
                        )?;
                        transforms.push(if ty == 0 {
                            Transform::Predictor {
                                bits,
                                xsize: width,
                                data,
                                prev: Vec::new(),
                            }
                        } else {
                            Transform::Color {
                                bits,
                                xsize: width,
                                data,
                            }
                        });
                    }
                    2 => transforms.push(Transform::SubtractGreen),
                    _ => {
                        let n = br.bits(8) as usize + 1;
                        let mut palette = sub_image(&mut br, n, 1)?;
                        for i in 1..palette.len() {
                            palette[i] = add_pixels(palette[i], palette[i - 1]);
                        }
                        let wb = palette_bits(n);
                        transforms.push(Transform::ColorIndexing {
                            xsize: width,
                            palette,
                        });
                        width = div_round_up(width, wb);
                    }
                }
            }
        }
        // Color cache.
        let cache_bits = if br.bits(1) == 1 {
            let b = br.bits(4);
            if !(1..=11).contains(&b) {
                return Err(Error::InvalidData("webp: bad color cache size"));
            }
            b
        } else {
            0
        };
        let cache_size = if cache_bits > 0 {
            1usize << cache_bits
        } else {
            0
        };
        // Meta prefix codes.
        let mut meta: Option<(u32, usize, Vec<u32>)> = None;
        let mut num_groups = 1;
        if level0 && br.bits(1) == 1 {
            let bits = br.bits(3) + 2;
            let pw = div_round_up(width, bits);
            let mut img = sub_image(&mut br, pw, div_round_up(ysize, bits))?;
            let mut maxg = 0;
            for p in img.iter_mut() {
                *p = (*p >> 8) & 0xffff;
                maxg = maxg.max(*p as usize);
            }
            num_groups = maxg + 1;
            meta = Some((bits, pw, img));
        }
        let mut groups = Vec::with_capacity(num_groups.min(4096));
        for _ in 0..num_groups {
            groups.push(read_group(&mut br, cache_size)?);
            if br.overrun() {
                return Err(Error::InvalidData("webp: truncated stream"));
            }
        }
        let total = width
            .checked_mul(ysize)
            .ok_or(Error::InvalidData("webp: size overflow"))?;
        let ring_len = total
            .min(MAX_DISTANCE + width + 4096)
            .max(1)
            .next_power_of_two();
        Ok(Rows {
            br,
            width,
            xsize,
            ysize,
            transforms,
            groups,
            meta,
            cache: vec![0u32; cache_size],
            cache_shift: 32 - cache_bits,
            ring: vec![0u32; ring_len],
            mask: ring_len - 1,
            pos: 0,
            cx: 0,
            cy: 0,
            last_cached: 0,
            y: 0,
            gi: 0,
            error: None,
            row: vec![0u32; xsize.max(width)],
            tmp: vec![0u32; xsize.max(width)],
        })
    }

    pub fn width(&self) -> usize {
        self.xsize
    }

    /// The error that ended decoding early, if any.
    pub fn result(&mut self) -> Result<(), Error> {
        match self.error.take() {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    /// Writes the next row (`width()` ARGB pixels) into `out`. Returns
    /// false at the end of the image or at damaged data.
    pub fn next_row(&mut self, out: &mut [u32]) -> bool {
        if self.y >= self.ysize || self.error.is_some() {
            return false;
        }
        let end = (self.y + 1) * self.width;
        while self.pos < end {
            if let Err(e) = self.decode_symbol() {
                self.error = Some(e);
                return false;
            }
        }
        let start = self.y * self.width;
        for (i, d) in self.row[..self.width].iter_mut().enumerate() {
            *d = self.ring[(start + i) & self.mask];
        }
        // Inverse transforms, last read first.
        let y = self.y;
        let mut w = self.width;
        for t in self.transforms.iter_mut().rev() {
            match t {
                Transform::Predictor {
                    bits,
                    xsize,
                    data,
                    prev,
                } => {
                    inverse_predictor_row(&mut self.row[..*xsize], prev, y, *bits, data);
                    prev.clear();
                    prev.extend_from_slice(&self.row[..*xsize]);
                }
                Transform::Color { bits, xsize, data } => {
                    inverse_color_row(&mut self.row[..*xsize], y, *bits, data)
                }
                Transform::SubtractGreen => {
                    for p in self.row[..w].iter_mut() {
                        let g = (*p >> 8) & 0xff;
                        let rb = (*p & 0x00ff_00ff).wrapping_add((g << 16) | g) & 0x00ff_00ff;
                        *p = (*p & 0xff00_ff00) | rb;
                    }
                }
                Transform::ColorIndexing { xsize, palette } => {
                    inverse_color_indexing_row(&self.row[..w], &mut self.tmp[..*xsize], palette);
                    std::mem::swap(&mut self.row, &mut self.tmp);
                    w = *xsize;
                }
            }
        }
        let n = self.xsize.min(out.len());
        out[..n].copy_from_slice(&self.row[..n]);
        self.y += 1;
        true
    }

    #[inline]
    fn group_index(&self, x: usize, y: usize) -> usize {
        match &self.meta {
            Some((bits, pw, img)) => img
                .get((y >> bits) * pw + (x >> bits))
                .copied()
                .unwrap_or(0) as usize,
            None => 0,
        }
    }

    /// Decodes one literal, backward reference or cache entry.
    fn decode_symbol(&mut self) -> Result<(), Error> {
        let total = self.width * self.ysize;
        if let Some((bits, _, _)) = &self.meta {
            if self.cx & ((1usize << bits) - 1) == 0 || self.pos == 0 {
                self.gi = self.group_index(self.cx, self.cy);
            }
        }
        let g = self
            .groups
            .get(self.gi)
            .ok_or(Error::InvalidData("webp: bad prefix group"))?;
        let br = &mut self.br;
        let s = read_symbol(br, &g.codes[0])?;
        if s < 256 {
            let r = read_symbol(br, &g.codes[1])?;
            let b = read_symbol(br, &g.codes[2])?;
            let a = read_symbol(br, &g.codes[3])?;
            self.ring[self.pos & self.mask] = (a << 24) | (r << 16) | (s << 8) | b;
            self.advance(1);
        } else if s < 256 + NUM_LENGTH_CODES as u32 {
            let len = prefix_value(br, s - 256);
            let dsym = read_symbol(br, &g.codes[4])?;
            let dcode = prefix_value(br, dsym);
            let dist = if dcode > 120 {
                dcode - 120
            } else {
                let (xi, yi) = DIST_MAP[dcode - 1];
                let d = xi as isize + yi as isize * self.width as isize;
                d.max(1) as usize
            };
            if dist > self.pos || len > total - self.pos || dist > self.mask {
                return Err(Error::InvalidData("webp: invalid backward reference"));
            }
            for i in self.pos..self.pos + len {
                self.ring[i & self.mask] = self.ring[(i - dist) & self.mask];
            }
            self.advance(len);
            if self.pos < total && self.meta.is_some() {
                self.gi = self.group_index(self.cx, self.cy);
            }
        } else {
            let idx = (s - 256 - NUM_LENGTH_CODES as u32) as usize;
            if idx >= self.cache.len() {
                return Err(Error::InvalidData("webp: bad color cache index"));
            }
            self.update_cache();
            self.ring[self.pos & self.mask] = self.cache[idx];
            self.advance(1);
        }
        self.update_cache();
        if self.br.overrun() {
            return Err(Error::InvalidData("webp: truncated stream"));
        }
        Ok(())
    }

    #[inline]
    fn advance(&mut self, n: usize) {
        self.pos += n;
        self.cx += n;
        while self.cx >= self.width {
            self.cx -= self.width;
            self.cy += 1;
        }
    }

    #[inline]
    fn update_cache(&mut self) {
        if self.cache.is_empty() {
            self.last_cached = self.pos;
            return;
        }
        while self.last_cached < self.pos {
            let c = self.ring[self.last_cached & self.mask];
            self.cache[(0x1e35_a7bd_u32.wrapping_mul(c) >> self.cache_shift) as usize] = c;
            self.last_cached += 1;
        }
    }
}

/// Decodes a sub-image (transform data, palette, meta codes): no
/// transforms, no meta codes.
fn sub_image(br: &mut BitReader, xsize: usize, ysize: usize) -> Result<Vec<u32>, Error> {
    let mut r = Rows::new(*br, xsize, ysize, false)?;
    let mut out = vec![0u32; xsize * ysize];
    for y in 0..ysize {
        if !r.next_row(&mut out[y * xsize..(y + 1) * xsize]) {
            break;
        }
    }
    r.result()?;
    *br = r.br;
    Ok(out)
}

fn palette_bits(n: usize) -> u32 {
    match n {
        1..=2 => 3,
        3..=4 => 2,
        5..=16 => 1,
        _ => 0,
    }
}

/// One prefix code group: green/length/cache, red, blue, alpha, distance.
struct Group {
    codes: [HuffTable; 5],
}

#[inline]
fn read_symbol(br: &mut BitReader, t: &HuffTable) -> Result<u32, Error> {
    if br.bitcnt < 15 {
        br.refill();
    }
    let e = t.lookup(br.bitbuf);
    if e & INVALID != 0 {
        return Err(Error::InvalidData("webp: invalid prefix code"));
    }
    br.consume(e & 15);
    Ok(e >> 16)
}

fn read_code(br: &mut BitReader, alphabet: usize) -> Result<HuffTable, Error> {
    let mut lens = vec![0u8; alphabet];
    if br.bits(1) == 1 {
        // Simple code: one or two symbols.
        let n = br.bits(1) + 1;
        let first_bits = if br.bits(1) == 1 { 8 } else { 1 };
        let s0 = br.bits(first_bits) as usize;
        *lens
            .get_mut(s0)
            .ok_or(Error::InvalidData("webp: symbol out of range"))? = 1;
        if n == 2 {
            let s1 = br.bits(8) as usize;
            *lens
                .get_mut(s1)
                .ok_or(Error::InvalidData("webp: symbol out of range"))? = 1;
        }
    } else {
        let num_cl = br.bits(4) as usize + 4;
        if num_cl > 19 {
            return Err(Error::InvalidData("webp: too many code length codes"));
        }
        let mut cl_lens = [0u8; 19];
        for &i in CL_ORDER.iter().take(num_cl) {
            cl_lens[i] = br.bits(3) as u8;
        }
        let cl = HuffTable::build(&cl_lens, 7, true, |s| (s as u32) << 16)?;
        let mut max_symbol = if br.bits(1) == 1 {
            let nbits = 2 + 2 * br.bits(3);
            let m = 2 + br.bits(nbits) as usize;
            if m > alphabet {
                return Err(Error::InvalidData("webp: max_symbol too large"));
            }
            m
        } else {
            alphabet
        };
        let mut prev = 8u8;
        let mut i = 0;
        while i < alphabet {
            if max_symbol == 0 {
                break;
            }
            max_symbol -= 1;
            let sym = read_symbol(br, &cl)?;
            match sym {
                0..=15 => {
                    lens[i] = sym as u8;
                    if sym != 0 {
                        prev = sym as u8;
                    }
                    i += 1;
                }
                16..=18 => {
                    let (extra, base, val) = match sym {
                        16 => (2, 3, prev),
                        17 => (3, 3, 0),
                        _ => (7, 11, 0),
                    };
                    let rep = base + br.bits(extra) as usize;
                    if i + rep > alphabet {
                        return Err(Error::InvalidData("webp: code lengths overflow"));
                    }
                    lens[i..i + rep].fill(val);
                    i += rep;
                }
                _ => return Err(Error::InvalidData("webp: bad code length symbol")),
            }
        }
    }
    if br.overrun() {
        return Err(Error::InvalidData("webp: truncated stream"));
    }
    HuffTable::build(&lens, 8, true, |s| (s as u32) << 16)
}

fn read_group(br: &mut BitReader, cache_size: usize) -> Result<Group, Error> {
    let g = read_code(br, 256 + NUM_LENGTH_CODES + cache_size)?;
    let r = read_code(br, 256)?;
    let b = read_code(br, 256)?;
    let a = read_code(br, 256)?;
    let d = read_code(br, ALPHABET_LEN_DIST)?;
    Ok(Group {
        codes: [g, r, b, a, d],
    })
}

#[inline]
fn prefix_value(br: &mut BitReader, prefix: u32) -> usize {
    if prefix < 4 {
        return prefix as usize + 1;
    }
    let extra = (prefix - 2) >> 1;
    let offset = (2 + (prefix & 1)) << extra;
    (offset + br.bits(extra)) as usize + 1
}

#[inline]
fn div_round_up(n: usize, bits: u32) -> usize {
    n.div_ceil(1 << bits)
}

/// Per-channel addition modulo 256.
#[inline(always)]
fn add_pixels(a: u32, b: u32) -> u32 {
    let ag = (a & 0xff00_ff00).wrapping_add(b & 0xff00_ff00) & 0xff00_ff00;
    let rb = (a & 0x00ff_00ff).wrapping_add(b & 0x00ff_00ff) & 0x00ff_00ff;
    ag | rb
}

#[inline(always)]
fn average2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xfefe_fefe) >> 1).wrapping_add(a & b)
}

#[inline(always)]
fn channel(p: u32, s: u32) -> i32 {
    ((p >> s) & 0xff) as i32
}

fn select(l: u32, t: u32, tl: u32) -> u32 {
    let mut pl = 0;
    let mut pt = 0;
    for s in [0, 8, 16, 24] {
        let (cl, ct, ctl) = (channel(l, s), channel(t, s), channel(tl, s));
        pl += (ct - ctl).abs(); // |estimate - L| with estimate = L + T - TL
        pt += (cl - ctl).abs(); // |estimate - T|
    }
    if pl < pt {
        l
    } else {
        t
    }
}

fn clamp_add_subtract_full(a: u32, b: u32, c: u32) -> u32 {
    let mut out = 0;
    for s in [0, 8, 16, 24] {
        let v = (channel(a, s) + channel(b, s) - channel(c, s)).clamp(0, 255) as u32;
        out |= v << s;
    }
    out
}

fn clamp_add_subtract_half(a: u32, b: u32) -> u32 {
    let mut out = 0;
    for s in [0, 8, 16, 24] {
        let (ca, cb) = (channel(a, s), channel(b, s));
        let v = (ca + (ca - cb) / 2).clamp(0, 255) as u32;
        out |= v << s;
    }
    out
}

/// Inverse predictor transform of row `y` (`prev` = previous output row).
fn inverse_predictor_row(row: &mut [u32], prev: &[u32], y: usize, bits: u32, sub: &[u32]) {
    let w = row.len();
    if w == 0 {
        return;
    }
    if y == 0 || prev.len() < w {
        row[0] = add_pixels(row[0], 0xff00_0000);
        for x in 1..w {
            row[x] = add_pixels(row[x], row[x - 1]);
        }
        return;
    }
    let bw = div_round_up(w, bits);
    let t = prev;
    row[0] = add_pixels(row[0], t[0]);
    let srow = (y >> bits) * bw;
    for x in 1..w {
        let mode = (sub.get(srow + (x >> bits)).copied().unwrap_or(0) >> 8) & 15;
        let l = row[x - 1];
        let tt = t[x];
        // The top-right pixel of the last column is the row's first pixel.
        let tr = if x + 1 < w { t[x + 1] } else { row[0] };
        let tl = t[x - 1];
        let pred = match mode {
            0 => 0xff00_0000,
            1 => l,
            2 => tt,
            3 => tr,
            4 => tl,
            5 => average2(average2(l, tr), tt),
            6 => average2(l, tl),
            7 => average2(l, tt),
            8 => average2(tl, tt),
            9 => average2(tt, tr),
            10 => average2(average2(l, tl), average2(tt, tr)),
            11 => select(l, tt, tl),
            12 => clamp_add_subtract_full(l, tt, tl),
            13 => clamp_add_subtract_half(average2(l, tt), tl),
            _ => 0xff00_0000,
        };
        row[x] = add_pixels(row[x], pred);
    }
}

#[inline(always)]
fn color_delta(t: u32, c: u32) -> i32 {
    ((t as u8 as i8 as i32) * (c as u8 as i8 as i32)) >> 5
}

fn inverse_color_row(row: &mut [u32], y: usize, bits: u32, sub: &[u32]) {
    let w = row.len();
    let bw = div_round_up(w, bits);
    let srow = (y >> bits) * bw;
    for (x, p) in row.iter_mut().enumerate() {
        let e = sub.get(srow + (x >> bits)).copied().unwrap_or(0);
        let (g2r, g2b, r2b) = (e & 0xff, (e >> 8) & 0xff, (e >> 16) & 0xff);
        let g = (*p >> 8) & 0xff;
        let r = (((*p >> 16) & 0xff) as i32 + color_delta(g2r, g)) as u32 & 0xff;
        let b = ((*p & 0xff) as i32 + color_delta(g2b, g) + color_delta(r2b, r)) as u32 & 0xff;
        *p = (*p & 0xff00_ff00) | (r << 16) | b;
    }
}

fn inverse_color_indexing_row(packed: &[u32], out: &mut [u32], palette: &[u32]) {
    let wb = palette_bits(palette.len());
    let bits_per = 8 >> wb;
    let mask = (1u32 << bits_per) - 1;
    for (x, o) in out.iter_mut().enumerate() {
        let p = packed.get(x >> wb).copied().unwrap_or(0);
        let g = (p >> 8) & 0xff;
        let idx = if wb == 0 {
            g
        } else {
            (g >> ((x & ((1 << wb) - 1)) as u32 * bits_per)) & mask
        };
        *o = palette.get(idx as usize).copied().unwrap_or(0);
    }
}
