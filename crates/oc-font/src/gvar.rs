//! `gvar` table: per-glyph point deltas for variable TrueType fonts.
//!
//! Implements the tuple variation store as used by `gvar`: shared and
//! embedded peak tuples, intermediate regions, shared/private packed point
//! numbers, packed deltas, and interpolation of untouched points (IUP).

use crate::reader::{u16_at, u32_at, Reader};
use crate::sfnt::TableRange;
use crate::var::axis_scalar;

#[derive(Debug, Clone, Copy)]
pub(crate) struct Gvar {
    axis_count: usize,
    shared_tuples: usize,
    shared_count: u16,
    glyph_count: u16,
    long_offsets: bool,
    offsets_start: usize,
    data_start: usize,
    end: usize,
}

/// Reusable buffers for delta computation.
#[derive(Default)]
pub(crate) struct Scratch {
    points: Vec<u16>,
    shared_points: Vec<u16>,
    xs: Vec<f32>,
    ys: Vec<f32>,
    tuple: Vec<[f32; 2]>,
    touched: Vec<bool>,
    refs: Vec<usize>,
}

impl Gvar {
    pub fn parse(data: &[u8], range: TableRange) -> Option<Gvar> {
        let b = range.start;
        let major = u16_at(data, b)?;
        if major != 1 {
            return None;
        }
        let axis_count = u16_at(data, b + 4)? as usize;
        let shared_count = u16_at(data, b + 6)?;
        let shared_off = u32_at(data, b + 8)? as usize;
        let glyph_count = u16_at(data, b + 12)?;
        let flags = u16_at(data, b + 14)?;
        let data_off = u32_at(data, b + 16)? as usize;
        Some(Gvar {
            axis_count,
            shared_tuples: b.checked_add(shared_off)?,
            shared_count,
            glyph_count,
            long_offsets: flags & 1 != 0,
            offsets_start: b + 20,
            data_start: b.checked_add(data_off)?,
            end: range.end,
        })
    }

    fn glyph_range(&self, data: &[u8], gid: u16) -> Option<(usize, usize)> {
        if gid >= self.glyph_count {
            return None;
        }
        let g = gid as usize;
        let (s, e) = if self.long_offsets {
            (
                u32_at(data, self.offsets_start + 4 * g)? as usize,
                u32_at(data, self.offsets_start + 4 * g + 4)? as usize,
            )
        } else {
            (
                u16_at(data, self.offsets_start + 2 * g)? as usize * 2,
                u16_at(data, self.offsets_start + 2 * g + 2)? as usize * 2,
            )
        };
        let s = self.data_start.checked_add(s)?;
        let e = self.data_start.checked_add(e)?;
        if e <= s || e > self.end {
            return None;
        }
        Some((s, e))
    }

    /// True if glyph `gid` has any variation data.
    pub fn has_data(&self, data: &[u8], gid: u16) -> bool {
        self.glyph_range(data, gid).is_some()
    }

    /// Accumulates the variation deltas of glyph `gid` at `coords` into
    /// `deltas`. `orig` holds the default outline points (including the
    /// four phantom points) and `ends` the inclusive contour end indices
    /// used for interpolating untouched points (empty for composites).
    #[allow(clippy::too_many_arguments)]
    pub fn glyph_deltas(
        &self,
        data: &[u8],
        gid: u16,
        coords: &[f32],
        orig: &[[f32; 2]],
        ends: &[usize],
        deltas: &mut [[f32; 2]],
        s: &mut Scratch,
    ) -> Option<()> {
        let Some((start, end)) = self.glyph_range(data, gid) else { return Some(()) };
        let gd = data.get(start..end)?;
        let n = orig.len().min(deltas.len());
        let mut hr = Reader::new(gd);
        let tc = hr.u16()?;
        let data_off = hr.u16()? as usize;
        let count = tc & 0x0FFF;
        let mut ser = Reader::at(gd, data_off);
        let Scratch { points, shared_points, xs, ys, tuple, touched, refs } = s;
        let shared_all = if tc & 0x8000 != 0 { Some(parse_points(&mut ser, shared_points)?) } else { None };
        let ac = self.axis_count;
        for _ in 0..count {
            let size = hr.u16()? as usize;
            let tidx = hr.u16()?;
            let peak = if tidx & 0x8000 != 0 {
                let r = hr;
                hr.skip(2 * ac)?;
                Some(r)
            } else if tidx & 0x0FFF < self.shared_count {
                let off = ((tidx & 0x0FFF) as usize).checked_mul(2 * ac)?;
                Some(Reader::at(data, self.shared_tuples.checked_add(off)?))
            } else {
                None
            };
            let inter = if tidx & 0x4000 != 0 {
                let sr = hr;
                hr.skip(2 * ac)?;
                let er = hr;
                hr.skip(2 * ac)?;
                Some((sr, er))
            } else {
                None
            };
            let tuple_data = ser.bytes(size)?;
            let Some(peak) = peak else { continue };
            let scalar = tuple_scalar(coords, ac, peak, inter)?;
            if scalar == 0.0 {
                continue;
            }
            let mut tr = Reader::new(tuple_data);
            let (all, pts): (bool, &[u16]) = if tidx & 0x2000 != 0 {
                let all = parse_points(&mut tr, points)?;
                (all, points.as_slice())
            } else if let Some(all) = shared_all {
                (all, shared_points.as_slice())
            } else {
                (true, &[])
            };
            let k = if all { n } else { pts.len() };
            unpack_deltas(&mut tr, k, xs)?;
            unpack_deltas(&mut tr, k, ys)?;
            if all {
                for ((d, &x), &y) in deltas[..n].iter_mut().zip(xs.iter()).zip(ys.iter()) {
                    d[0] += scalar * x;
                    d[1] += scalar * y;
                }
            } else {
                tuple.clear();
                tuple.resize(n, [0.0; 2]);
                touched.clear();
                touched.resize(n, false);
                let mut any = false;
                for (j, &p) in pts.iter().enumerate() {
                    let p = p as usize;
                    if p < n {
                        tuple[p] = [xs[j], ys[j]];
                        touched[p] = true;
                        any = true;
                    }
                }
                if !any {
                    continue;
                }
                if !ends.is_empty() {
                    iup(orig, ends, tuple, touched, refs);
                }
                for (d, t) in deltas[..n].iter_mut().zip(tuple.iter()) {
                    d[0] += scalar * t[0];
                    d[1] += scalar * t[1];
                }
            }
        }
        Some(())
    }
}

fn tuple_scalar(
    coords: &[f32],
    axis_count: usize,
    mut peak: Reader,
    inter: Option<(Reader, Reader)>,
) -> Option<f32> {
    let mut scalar = 1.0f32;
    let (mut sr, mut er) = match inter {
        Some((s, e)) => (Some(s), Some(e)),
        None => (None, None),
    };
    for i in 0..axis_count {
        let p = peak.f2dot14()?;
        let (st, en) = match (&mut sr, &mut er) {
            (Some(s), Some(e)) => (s.f2dot14()?, e.f2dot14()?),
            _ => (p.min(0.0), p.max(0.0)),
        };
        if p == 0.0 {
            continue;
        }
        let v = coords.get(i).copied().unwrap_or(0.0);
        scalar *= axis_scalar(st, p, en, v);
        if scalar == 0.0 {
            return Some(0.0);
        }
    }
    Some(scalar)
}

/// Reads packed point numbers. Returns `true` for "all points".
fn parse_points(r: &mut Reader, out: &mut Vec<u16>) -> Option<bool> {
    out.clear();
    let b0 = r.u8()?;
    if b0 == 0 {
        return Some(true);
    }
    let count = if b0 & 0x80 != 0 {
        (((b0 & 0x7F) as usize) << 8) | r.u8()? as usize
    } else {
        b0 as usize
    };
    let mut last = 0u16;
    while out.len() < count {
        let ctrl = r.u8()?;
        let run = (ctrl & 0x7F) as usize + 1;
        let words = ctrl & 0x80 != 0;
        for _ in 0..run {
            let d = if words { r.u16()? } else { r.u8()? as u16 };
            last = last.wrapping_add(d);
            out.push(last);
        }
    }
    out.truncate(count);
    Some(false)
}

/// Reads `count` packed deltas.
fn unpack_deltas(r: &mut Reader, count: usize, out: &mut Vec<f32>) -> Option<()> {
    out.clear();
    while out.len() < count {
        let ctrl = r.u8()?;
        let run = (ctrl & 0x3F) as usize + 1;
        match ctrl & 0xC0 {
            0x80 => out.resize(out.len() + run, 0.0),
            0x40 => {
                for _ in 0..run {
                    out.push(r.i16()? as f32);
                }
            }
            0xC0 => {
                for _ in 0..run {
                    out.push(r.i32()? as f32);
                }
            }
            _ => {
                for _ in 0..run {
                    out.push(r.i8()? as f32);
                }
            }
        }
    }
    out.truncate(count);
    Some(())
}

/// Interpolates deltas of untouched points per contour (IUP).
fn iup(orig: &[[f32; 2]], ends: &[usize], d: &mut [[f32; 2]], touched: &[bool], refs: &mut Vec<usize>) {
    let n = d.len().min(orig.len()).min(touched.len());
    let mut start = 0usize;
    for &end in ends {
        if end < start || end >= n {
            break;
        }
        refs.clear();
        refs.extend((start..=end).filter(|&i| touched[i]));
        let m = refs.len();
        if m > 0 && m < end - start + 1 {
            for k in 0..m {
                let a = refs[k];
                let b = refs[(k + 1) % m];
                let mut p = if a == end { start } else { a + 1 };
                while p != b {
                    for ax in 0..2 {
                        let (mut i1, mut i2) = (orig[a][ax], orig[b][ax]);
                        let (mut d1, mut d2) = (d[a][ax], d[b][ax]);
                        if i1 > i2 {
                            std::mem::swap(&mut i1, &mut i2);
                            std::mem::swap(&mut d1, &mut d2);
                        }
                        let c = orig[p][ax];
                        d[p][ax] = if i1 == i2 {
                            if d1 == d2 {
                                d1
                            } else {
                                0.0
                            }
                        } else if c <= i1 {
                            d1
                        } else if c >= i2 {
                            d2
                        } else {
                            d1 + (c - i1) * (d2 - d1) / (i2 - i1)
                        };
                    }
                    p = if p == end { start } else { p + 1 };
                }
            }
        }
        start = end + 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn packed_points_and_deltas() {
        // 3 points: run of 3 bytes: 1, +2, +3 -> 1, 3, 6
        let d = [3u8, 0x02, 1, 2, 3];
        let mut out = Vec::new();
        assert_eq!(parse_points(&mut Reader::new(&d), &mut out), Some(false));
        assert_eq!(out, vec![1, 3, 6]);
        assert_eq!(parse_points(&mut Reader::new(&[0]), &mut out), Some(true));
        // deltas: 2 zeros, then 2 words (300, -2), then 1 byte (-1)
        let d = [0x81u8, 0x41, 0x01, 0x2C, 0xFF, 0xFE, 0x00, 0xFF];
        let mut v = Vec::new();
        unpack_deltas(&mut Reader::new(&d), 5, &mut v).unwrap();
        assert_eq!(v, vec![0.0, 0.0, 300.0, -2.0, -1.0]);
        assert!(unpack_deltas(&mut Reader::new(&d[..3]), 5, &mut v).is_none());
    }

    #[test]
    fn interpolate_untouched() {
        // square contour, points 0 and 2 touched
        let orig = [[0.0, 0.0], [10.0, 0.0], [10.0, 10.0], [0.0, 10.0], [5.0, 5.0]];
        let mut d = [[0.0f32; 2]; 5];
        d[0] = [-2.0, -2.0];
        d[2] = [2.0, 2.0];
        let touched = [true, false, true, false, false];
        let mut refs = Vec::new();
        iup(&orig, &[3], &mut d, &touched, &mut refs);
        assert_eq!(d[1], [2.0, -2.0]);
        assert_eq!(d[3], [-2.0, 2.0]);
        // point outside contours (phantom) untouched
        assert_eq!(d[4], [0.0, 0.0]);
        // single touched point shifts the whole contour
        let mut d = [[0.0f32; 2]; 5];
        d[1] = [1.0, 3.0];
        let touched = [false, true, false, false, false];
        iup(&orig, &[3], &mut d, &touched, &mut refs);
        assert_eq!(d[0], [1.0, 3.0]);
        assert_eq!(d[3], [1.0, 3.0]);
    }
}
