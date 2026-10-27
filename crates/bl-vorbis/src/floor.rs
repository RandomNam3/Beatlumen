//! Floor curves: type 0 (LSP, spec section 6) and type 1 (piecewise linear,
//! spec section 7). Floors are decoded before the residue and applied to
//! the residue spectrum afterwards ("dot product").
//!
//! The floor 0 curve evaluation (`Floor0::apply`) and Bark map follow the
//! single-precision arithmetic of libvorbis' `vorbis_lsp_to_curve` and
//! `floor0_map_lazy_init`:
//! Copyright (c) 2002-2020 Xiph.org Foundation, BSD-3-Clause (see NOTICE).

use crate::bitreader::BitReader;
use crate::codebook::{ilog, Codebook};
use crate::tables::FLOOR1_INVERSE_DB;
use crate::Error;

const EOP: Error = Error::InvalidData("truncated floor setup");

pub(crate) enum Floor {
    Zero(Floor0),
    One(Box<Floor1>),
}

/// Per-channel floor data decoded from an audio packet.
#[derive(Default)]
pub(crate) struct FloorState {
    /// False if the floor is "unused" in this packet (channel is silent).
    pub used: bool,
    /// Floor 1: final Y values; floor 0: unused.
    pub ys: Vec<i32>,
    /// Floor 1: step-2 flags.
    pub flags: Vec<bool>,
    /// Floor 0: amplitude.
    pub amplitude: u32,
    /// Floor 0: LSP coefficients.
    pub coeffs: Vec<f32>,
}

impl Floor {
    pub fn parse(
        br: &mut BitReader,
        books: &[Codebook],
        blocksizes: [usize; 2],
    ) -> Result<Floor, Error> {
        match br.read(16).ok_or(EOP)? {
            0 => Ok(Floor::Zero(Floor0::parse(br, books, blocksizes)?)),
            1 => Ok(Floor::One(Box::new(Floor1::parse(br, books)?))),
            _ => Err(Error::InvalidData("bad floor type")),
        }
    }

    /// Decodes the floor of one channel from an audio packet.
    pub fn decode(
        &self,
        br: &mut BitReader,
        books: &[Codebook],
        st: &mut FloorState,
        scratch: &mut Vec<f32>,
    ) {
        st.used = match self {
            Floor::Zero(f) => f.decode(br, books, st, scratch),
            Floor::One(f) => f.decode(br, books, st),
        };
    }

    /// Multiplies `spectrum` (length blocksize/2) by the floor curve.
    pub fn apply(&self, st: &FloorState, blockflag: usize, spectrum: &mut [f32]) {
        match self {
            Floor::Zero(f) => f.apply(st, blockflag, spectrum),
            Floor::One(f) => f.apply(st, spectrum),
        }
    }
}

// ---------------------------------------------------------------------------
// Floor 0

pub(crate) struct Floor0 {
    order: usize,
    bark_map_size: u32,
    amplitude_bits: u32,
    amplitude_offset: u32,
    books: Vec<u8>,
    /// Bark-scale map for each blocksize (`n/2` entries plus a -1 sentinel).
    maps: [Vec<i32>; 2],
}

/// Bark scale as computed by the reference decoder (mixed float/double).
fn to_bark(x: f32) -> f64 {
    13.1f32 as f64 * ((0.00074f32 * x) as f64).atan()
        + 2.24f32 as f64 * ((x * x * 1.85e-8f32) as f64).atan()
        + (1e-4f32 * x) as f64
}

impl Floor0 {
    fn parse(
        br: &mut BitReader,
        books: &[Codebook],
        blocksizes: [usize; 2],
    ) -> Result<Self, Error> {
        let order = br.read(8).ok_or(EOP)? as usize;
        let rate = br.read(16).ok_or(EOP)?;
        let bark_map_size = br.read(16).ok_or(EOP)?;
        let amplitude_bits = br.read(6).ok_or(EOP)?;
        let amplitude_offset = br.read(8).ok_or(EOP)?;
        let nbooks = br.read(4).ok_or(EOP)? as usize + 1;
        let mut list = Vec::with_capacity(nbooks);
        for _ in 0..nbooks {
            let b = br.read(8).ok_or(EOP)? as usize;
            match books.get(b) {
                Some(book) if book.has_vq() => list.push(b as u8),
                _ => return Err(Error::InvalidData("bad floor 0 codebook")),
            }
        }
        if order == 0 || rate == 0 || bark_map_size == 0 {
            return Err(Error::InvalidData("bad floor 0 setup"));
        }
        let maps = [0, 1].map(|w| {
            let n = blocksizes[w] / 2;
            let scale = (bark_map_size as f64 / to_bark(rate as f32 / 2.0)) as f32;
            let mut map: Vec<i32> = (0..n)
                .map(|j| {
                    let x = (rate as f32 / 2.0) / n as f32 * j as f32;
                    let v = (to_bark(x) * scale as f64).floor();
                    (v as i64).clamp(0, bark_map_size as i64 - 1) as i32
                })
                .collect();
            map.push(-1);
            map
        });
        Ok(Floor0 {
            order,
            bark_map_size,
            amplitude_bits,
            amplitude_offset,
            books: list,
            maps,
        })
    }

    fn decode(
        &self,
        br: &mut BitReader,
        books: &[Codebook],
        st: &mut FloorState,
        scratch: &mut Vec<f32>,
    ) -> bool {
        if self.amplitude_bits > 32 {
            // The reference decoder cannot read such fields: always unused.
            return false;
        }
        let Some(amplitude) = br.read(self.amplitude_bits) else {
            return false;
        };
        if amplitude == 0 {
            return false;
        }
        let Some(booknum) = br.read(ilog(self.books.len() as u32)) else {
            return false;
        };
        let Some(&book_index) = self.books.get(booknum as usize) else {
            return false;
        };
        let book = &books[book_index as usize];
        st.amplitude = amplitude;
        st.coeffs.clear();
        let mut last = 0f32;
        while st.coeffs.len() < self.order {
            let Some(v) = book.decode_vector(br, scratch) else {
                return false;
            };
            for &x in v {
                st.coeffs.push(x + last);
            }
            last = *st.coeffs.last().unwrap_or(&0.0);
        }
        st.coeffs.truncate(self.order);
        true
    }

    fn apply(&self, st: &FloorState, blockflag: usize, spectrum: &mut [f32]) {
        let map = &self.maps[blockflag];
        let n = spectrum.len().min(map.len() - 1);
        let m = self.order;
        let lsp: Vec<f32> = st
            .coeffs
            .iter()
            .map(|&c| 2.0 * (c as f64).cos() as f32)
            .collect();
        let maxval = ((1u64 << self.amplitude_bits) - 1) as f32;
        let amp = st.amplitude as f32 / maxval * self.amplitude_offset as f32;
        let ampoffset = self.amplitude_offset as f32;
        let wdel = (std::f64::consts::PI / self.bark_map_size as f64) as f32;
        let mut i = 0;
        while i < n {
            let k = map[i];
            let mut p = 0.5f32;
            let mut q = 0.5f32;
            let w = 2.0 * ((wdel * k as f32) as f64).cos() as f32;
            let mut j = 1;
            while j < m {
                q *= w - lsp[j - 1];
                p *= w - lsp[j];
                j += 2;
            }
            if j == m {
                // Odd order.
                q *= w - lsp[j - 1];
                p = p * (p * (4.0 - w * w));
                q *= q;
            } else {
                p = p * (p * (2.0 - w));
                q = q * (q * (2.0 + w));
            }
            let x = amp / (p + q).sqrt() - ampoffset;
            let val = ((x * 0.115_129_25) as f64).exp() as f32;
            while i < n && map[i] == k {
                spectrum[i] *= val;
                i += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Floor 1

pub(crate) struct Floor1 {
    partition_class: Vec<u8>,
    class_dims: [u8; 16],
    class_subclasses: [u8; 16],
    class_masterbook: [u8; 16],
    subclass_books: [[i16; 8]; 16],
    multiplier: i32,
    xs: Vec<i32>,
    /// Indices of `xs` in ascending X order.
    sorted: Vec<u8>,
    low: Vec<u8>,
    high: Vec<u8>,
}

const FLOOR1_RANGE: [i32; 4] = [256, 128, 86, 64];

impl Floor1 {
    fn parse(br: &mut BitReader, books: &[Codebook]) -> Result<Self, Error> {
        let partitions = br.read(5).ok_or(EOP)? as usize;
        let mut partition_class = Vec::with_capacity(partitions);
        for _ in 0..partitions {
            partition_class.push(br.read(4).ok_or(EOP)? as u8);
        }
        let max_class = partition_class
            .iter()
            .copied()
            .max()
            .map_or(-1, |m| m as i32);
        let mut class_dims = [0u8; 16];
        let mut class_subclasses = [0u8; 16];
        let mut class_masterbook = [0u8; 16];
        let mut subclass_books = [[-1i16; 8]; 16];
        let nbooks = books.len();
        for c in 0..(max_class + 1) as usize {
            class_dims[c] = br.read(3).ok_or(EOP)? as u8 + 1;
            class_subclasses[c] = br.read(2).ok_or(EOP)? as u8;
            if class_subclasses[c] != 0 {
                let b = br.read(8).ok_or(EOP)? as usize;
                if b >= nbooks {
                    return Err(Error::InvalidData("bad floor 1 master book"));
                }
                class_masterbook[c] = b as u8;
            }
            for slot in subclass_books[c].iter_mut().take(1 << class_subclasses[c]) {
                let b = br.read(8).ok_or(EOP)? as i16 - 1;
                if b >= nbooks as i16 {
                    return Err(Error::InvalidData("bad floor 1 subclass book"));
                }
                *slot = b;
            }
        }
        let multiplier = br.read(2).ok_or(EOP)? as i32 + 1;
        let rangebits = br.read(4).ok_or(EOP)?;
        let mut xs = vec![0i32, 1 << rangebits];
        for &c in &partition_class {
            for _ in 0..class_dims[c as usize] {
                if xs.len() >= 65 {
                    return Err(Error::InvalidData("too many floor 1 values"));
                }
                xs.push(br.read(rangebits).ok_or(EOP)? as i32);
            }
        }
        let mut sorted: Vec<u8> = (0..xs.len() as u8).collect();
        sorted.sort_by_key(|&i| xs[i as usize]);
        if sorted
            .windows(2)
            .any(|w| xs[w[0] as usize] == xs[w[1] as usize])
        {
            return Err(Error::InvalidData("duplicate floor 1 X values"));
        }
        let mut low = vec![0u8; xs.len()];
        let mut high = vec![1u8; xs.len()];
        for i in 2..xs.len() {
            let x = xs[i];
            let (mut lo, mut hi) = (0usize, 1usize);
            for j in 0..i {
                if xs[j] < x && xs[j] > xs[lo] {
                    lo = j;
                }
                if xs[j] > x && xs[j] < xs[hi] {
                    hi = j;
                }
            }
            low[i] = lo as u8;
            high[i] = hi as u8;
        }
        Ok(Floor1 {
            partition_class,
            class_dims,
            class_subclasses,
            class_masterbook,
            subclass_books,
            multiplier,
            xs,
            sorted,
            low,
            high,
        })
    }

    /// Packet decode plus step 1 of the curve computation (final Y values).
    fn decode(&self, br: &mut BitReader, books: &[Codebook], st: &mut FloorState) -> bool {
        match br.read_bit() {
            Some(true) => {}
            _ => return false,
        }
        let range = FLOOR1_RANGE[(self.multiplier - 1) as usize];
        let ybits = ilog(range as u32 - 1);
        let ys = &mut st.ys;
        ys.clear();
        let (Some(y0), Some(y1)) = (br.read(ybits), br.read(ybits)) else {
            return false;
        };
        ys.push((y0 as i32).min(range - 1));
        ys.push((y1 as i32).min(range - 1));
        for &class in &self.partition_class {
            let c = class as usize;
            let cdim = self.class_dims[c];
            let cbits = self.class_subclasses[c] as u32;
            let csub = (1u32 << cbits) - 1;
            let mut cval = 0;
            if cbits > 0 {
                match books[self.class_masterbook[c] as usize].decode_scalar(br) {
                    Some(v) => cval = v,
                    None => return false,
                }
            }
            for _ in 0..cdim {
                let book = self.subclass_books[c][(cval & csub) as usize];
                cval >>= cbits;
                if book >= 0 {
                    match books[book as usize].decode_scalar(br) {
                        Some(v) => ys.push(v as i32),
                        None => return false,
                    }
                } else {
                    ys.push(0);
                }
            }
        }

        // Step 1: amplitude value synthesis.
        let flags = &mut st.flags;
        flags.clear();
        flags.resize(ys.len(), false);
        flags[0] = true;
        flags[1] = true;
        for i in 2..ys.len() {
            let lo = self.low[i] as usize;
            let hi = self.high[i] as usize;
            let predicted = render_point(self.xs[lo], ys[lo], self.xs[hi], ys[hi], self.xs[i]);
            let val = ys[i];
            let highroom = range - predicted;
            let lowroom = predicted;
            let room = if highroom < lowroom {
                highroom * 2
            } else {
                lowroom * 2
            };
            if val != 0 {
                flags[lo] = true;
                flags[hi] = true;
                flags[i] = true;
                ys[i] = if val >= room {
                    if highroom > lowroom {
                        val - lowroom + predicted
                    } else {
                        predicted - val + highroom - 1
                    }
                } else if val & 1 == 1 {
                    predicted - (val + 1) / 2
                } else {
                    predicted + val / 2
                };
            } else {
                ys[i] = predicted;
            }
            // Guard against out-of-range values from malicious setups (valid
            // streams cannot produce them); also keeps the arithmetic in
            // `render_point` far from overflow.
            ys[i] = ys[i].clamp(0, range - 1);
        }
        true
    }

    /// Step 2: renders the curve, multiplying it into `spectrum`.
    fn apply(&self, st: &FloorState, spectrum: &mut [f32]) {
        let n = spectrum.len() as i32;
        let ys = &st.ys;
        let mut lx = 0;
        let mut ly = ys[self.sorted[0] as usize] * self.multiplier;
        let mut hx = 0;
        let mut hy = ly;
        for &idx in &self.sorted[1..] {
            let i = idx as usize;
            if st.flags[i] {
                hy = ys[i] * self.multiplier;
                hx = self.xs[i];
                render_line(lx, ly, hx, hy, spectrum);
                lx = hx;
                ly = hy;
            }
        }
        if hx < n {
            render_line(hx, hy, n, hy, spectrum);
        }
    }
}

fn render_point(x0: i32, y0: i32, x1: i32, y1: i32, x: i32) -> i32 {
    let dy = y1 - y0;
    let adx = x1 - x0;
    if adx == 0 {
        return y0;
    }
    let off = dy.abs() * (x - x0) / adx;
    if dy < 0 {
        y0 - off
    } else {
        y0 + off
    }
}

#[inline(always)]
fn inverse_db(y: i32) -> f32 {
    FLOOR1_INVERSE_DB[y.clamp(0, 255) as usize]
}

/// Integer line drawing of spec 9.2.7, multiplying `v[x]` by the inverse dB
/// value of each plotted point; points at or beyond `v.len()` are dropped.
fn render_line(x0: i32, y0: i32, x1: i32, y1: i32, v: &mut [f32]) {
    let n = v.len() as i32;
    if x0 >= n || x1 <= x0 || x0 < 0 {
        return;
    }
    let dy = y1 - y0;
    let adx = x1 - x0;
    let base = dy / adx;
    let sy = if dy < 0 { base - 1 } else { base + 1 };
    let ady = dy.abs() - base.abs() * adx;
    let end = x1.min(n) as usize;
    let mut y = y0;
    let mut err = 0;
    let x0 = x0 as usize;
    v[x0] *= inverse_db(y);
    if dy == 0 {
        let f = inverse_db(y);
        for s in &mut v[x0 + 1..end] {
            *s *= f;
        }
        return;
    }
    for s in &mut v[x0 + 1..end] {
        err += ady;
        if err >= adx {
            err -= adx;
            y += sy;
        } else {
            y += base;
        }
        *s *= inverse_db(y);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_matches_spec_shape() {
        let mut v = vec![1f32; 10];
        render_line(0, 255, 10, 245, &mut v);
        // Monotonically decreasing from table[255] = 1.0 to table[246].
        assert_eq!(v[0], 1.0);
        assert!(v.windows(2).all(|w| w[1] <= w[0]));
        assert_eq!(v[9], FLOOR1_INVERSE_DB[246]);
        assert_eq!(render_point(0, 0, 10, 100, 5), 50);
        assert_eq!(render_point(0, 100, 10, 0, 3), 70);
    }
}
