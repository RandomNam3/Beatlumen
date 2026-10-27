//! TrueType outlines (`loca` / `glyf`): simple and composite glyphs, with
//! `gvar` variations applied when the font is set to a non-default
//! instance.

use crate::gvar::{self, Gvar};
use crate::hmtx::Hmtx;
use crate::reader::{u16_at, u32_at, Reader};
use crate::sfnt::TableRange;

/// Maximum composite nesting depth.
const MAX_DEPTH: u32 = 8;
/// Maximum number of points of one (fully assembled) glyph.
const MAX_POINTS: usize = 1 << 18;
/// Maximum number of components processed for one glyph.
const MAX_COMPONENTS: usize = 4096;

// Simple glyph flags.
const ON_CURVE: u8 = 0x01;
const X_SHORT: u8 = 0x02;
const Y_SHORT: u8 = 0x04;
const REPEAT: u8 = 0x08;
const X_SAME_OR_POS: u8 = 0x10;
const Y_SAME_OR_POS: u8 = 0x20;

// Composite glyph flags.
const ARG_WORDS: u16 = 0x0001;
const ARGS_ARE_XY: u16 = 0x0002;
const HAVE_SCALE: u16 = 0x0008;
const MORE_COMPONENTS: u16 = 0x0020;
const HAVE_XY_SCALE: u16 = 0x0040;
const HAVE_2X2: u16 = 0x0080;
const SCALED_OFFSET: u16 = 0x0800;
const UNSCALED_OFFSET: u16 = 0x1000;

/// `loca` + `glyf` table locations.
#[derive(Debug, Clone)]
pub(crate) struct Glyf {
    pub loca: TableRange,
    pub glyf: TableRange,
    pub long_loca: bool,
}

/// A decoded TrueType outline in font units.
#[derive(Debug, Default)]
pub(crate) struct Outline {
    pub pts: Vec<[f32; 2]>,
    pub on: Vec<bool>,
    /// Inclusive end index of every contour.
    pub ends: Vec<usize>,
}

#[derive(Debug, Clone, Copy)]
struct Component {
    gid: u16,
    flags: u16,
    arg1: i32,
    arg2: i32,
    m: [f32; 4],
}

impl Glyf {
    /// Raw `glyf` data of glyph `gid` (empty slice for empty glyphs).
    pub fn glyph_data<'a>(&self, data: &'a [u8], gid: u16, num_glyphs: u16) -> Option<&'a [u8]> {
        if gid >= num_glyphs {
            return None;
        }
        let loca = data.get(self.loca.clone())?;
        let glyf = data.get(self.glyf.clone())?;
        let g = gid as usize;
        let (s, e) = if self.long_loca {
            (u32_at(loca, 4 * g)? as usize, u32_at(loca, 4 * g + 4)? as usize)
        } else {
            (u16_at(loca, 2 * g)? as usize * 2, u16_at(loca, 2 * g + 2)? as usize * 2)
        };
        if e <= s {
            return Some(&[]);
        }
        glyf.get(s..e.min(glyf.len()))
    }
}

/// Outline loader holding the per-call context and scratch buffers.
pub(crate) struct Loader<'a> {
    pub data: &'a [u8],
    pub glyf: &'a Glyf,
    pub hmtx: Option<&'a Hmtx>,
    pub num_glyphs: u16,
    /// Variation tables and normalized coordinates (only when the font is
    /// set to a non-default instance).
    pub var: Option<(&'a Gvar, &'a [f32])>,
    flags: Vec<u8>,
    orig: Vec<[f32; 2]>,
    deltas: Vec<[f32; 2]>,
    local_ends: Vec<usize>,
    scratch: gvar::Scratch,
    components: usize,
}

impl<'a> Loader<'a> {
    pub fn new(
        data: &'a [u8],
        glyf: &'a Glyf,
        hmtx: Option<&'a Hmtx>,
        num_glyphs: u16,
        var: Option<(&'a Gvar, &'a [f32])>,
    ) -> Self {
        Loader {
            data,
            glyf,
            hmtx,
            num_glyphs,
            var,
            flags: Vec::new(),
            orig: Vec::new(),
            deltas: Vec::new(),
            local_ends: Vec::new(),
            scratch: gvar::Scratch::default(),
            components: 0,
        }
    }

    fn advance(&self, gid: u16) -> f32 {
        self.hmtx.map_or(0.0, |h| h.advance(self.data, gid) as f32)
    }

    fn lsb(&self, gid: u16) -> f32 {
        self.hmtx.map_or(0.0, |h| h.lsb(self.data, gid) as f32)
    }

    /// Loads glyph `gid` into `out`, positioned so that the origin is at
    /// phantom point 1.
    pub fn load(&mut self, gid: u16, out: &mut Outline) -> Option<()> {
        self.components = 0;
        let pp1 = self.load_rec(gid, 0, out)?;
        if pp1 != 0.0 {
            for p in &mut out.pts {
                p[0] -= pp1;
            }
        }
        Some(())
    }

    /// Returns the x coordinate of phantom point 1 (after variations).
    fn load_rec(&mut self, gid: u16, depth: u32, out: &mut Outline) -> Option<f32> {
        if depth > MAX_DEPTH {
            return None;
        }
        let g = self.glyf.glyph_data(self.data, gid, self.num_glyphs)?;
        if g.is_empty() {
            return Some(0.0);
        }
        let n_contours = u16_at(g, 0)? as i16;
        let x_min = u16_at(g, 2)? as i16 as f32;
        let pp1 = x_min - self.lsb(gid);
        if n_contours >= 0 {
            self.load_simple(gid, g, n_contours as usize, pp1, out)
        } else {
            self.load_composite(gid, g, pp1, depth, out)
        }
    }

    fn load_simple(&mut self, gid: u16, g: &[u8], n_contours: usize, pp1: f32, out: &mut Outline) -> Option<f32> {
        let base = out.pts.len();
        let ends_base = out.ends.len();
        let mut r = Reader::at(g, 10);
        let mut n_pts = 0usize;
        for i in 0..n_contours {
            let e = r.u16()? as usize;
            if i > 0 && e < n_pts {
                // Decreasing contour ends: malformed.
                out.ends.truncate(ends_base);
                return None;
            }
            n_pts = e + 1;
            out.ends.push(base + e);
        }
        if base + n_pts > MAX_POINTS {
            out.ends.truncate(ends_base);
            return None;
        }
        let ins_len = r.u16()? as usize;
        r.skip(ins_len)?;
        let flags = &mut self.flags;
        flags.clear();
        while flags.len() < n_pts {
            let f = r.u8()?;
            flags.push(f);
            if f & REPEAT != 0 {
                let n = r.u8()? as usize;
                let n = n.min(n_pts - flags.len());
                flags.resize(flags.len() + n, f);
            }
        }
        let parsed = (|| {
            out.pts.reserve(n_pts);
            let mut x = 0i32;
            for &f in flags.iter() {
                if f & X_SHORT != 0 {
                    let d = r.u8()? as i32;
                    x = x.wrapping_add(if f & X_SAME_OR_POS != 0 { d } else { -d });
                } else if f & X_SAME_OR_POS == 0 {
                    x = x.wrapping_add(r.i16()? as i32);
                }
                out.pts.push([x as f32, 0.0]);
            }
            let mut y = 0i32;
            for (i, &f) in flags.iter().enumerate() {
                if f & Y_SHORT != 0 {
                    let d = r.u8()? as i32;
                    y = y.wrapping_add(if f & Y_SAME_OR_POS != 0 { d } else { -d });
                } else if f & Y_SAME_OR_POS == 0 {
                    y = y.wrapping_add(r.i16()? as i32);
                }
                out.pts[base + i][1] = y as f32;
                out.on.push(f & ON_CURVE != 0);
            }
            Some(())
        })();
        if parsed.is_none() {
            out.pts.truncate(base);
            out.on.truncate(base);
            out.ends.truncate(ends_base);
            return None;
        }
        let mut pp1 = pp1;
        if let Some((gvar, coords)) = self.var {
            if gvar.has_data(self.data, gid) {
                let adv = self.advance(gid);
                self.orig.clear();
                self.orig.extend_from_slice(&out.pts[base..]);
                self.orig.extend_from_slice(&[[pp1, 0.0], [pp1 + adv, 0.0], [0.0, 0.0], [0.0, 0.0]]);
                self.deltas.clear();
                self.deltas.resize(self.orig.len(), [0.0; 2]);
                self.local_ends.clear();
                self.local_ends.extend(out.ends[ends_base..].iter().map(|&e| e - base));
                let ok = gvar.glyph_deltas(
                    self.data,
                    gid,
                    coords,
                    &self.orig,
                    &self.local_ends,
                    &mut self.deltas,
                    &mut self.scratch,
                );
                if ok.is_some() {
                    for (p, d) in out.pts[base..].iter_mut().zip(self.deltas.iter()) {
                        p[0] += d[0];
                        p[1] += d[1];
                    }
                    pp1 += self.deltas[n_pts][0];
                }
            }
        }
        Some(pp1)
    }

    fn load_composite(&mut self, gid: u16, g: &[u8], pp1: f32, depth: u32, out: &mut Outline) -> Option<f32> {
        let mut r = Reader::at(g, 10);
        let mut comps: Vec<Component> = Vec::new();
        loop {
            let flags = r.u16()?;
            let cg = r.u16()?;
            let (arg1, arg2) = match (flags & ARG_WORDS != 0, flags & ARGS_ARE_XY != 0) {
                (true, true) => (r.i16()? as i32, r.i16()? as i32),
                (true, false) => (r.u16()? as i32, r.u16()? as i32),
                (false, true) => (r.i8()? as i32, r.i8()? as i32),
                (false, false) => (r.u8()? as i32, r.u8()? as i32),
            };
            let m = if flags & HAVE_SCALE != 0 {
                let s = r.f2dot14()?;
                [s, 0.0, 0.0, s]
            } else if flags & HAVE_XY_SCALE != 0 {
                let sx = r.f2dot14()?;
                let sy = r.f2dot14()?;
                [sx, 0.0, 0.0, sy]
            } else if flags & HAVE_2X2 != 0 {
                [r.f2dot14()?, r.f2dot14()?, r.f2dot14()?, r.f2dot14()?]
            } else {
                [1.0, 0.0, 0.0, 1.0]
            };
            comps.push(Component { gid: cg, flags, arg1, arg2, m });
            self.components += 1;
            if self.components > MAX_COMPONENTS {
                return None;
            }
            if flags & MORE_COMPONENTS == 0 {
                break;
            }
        }

        // Component offset deltas (one "point" per component + phantoms).
        let nc = comps.len();
        let mut offsets: Vec<[f32; 2]> = comps
            .iter()
            .map(|c| if c.flags & ARGS_ARE_XY != 0 { [c.arg1 as f32, c.arg2 as f32] } else { [0.0, 0.0] })
            .collect();
        let mut pp1 = pp1;
        if let Some((gvar, coords)) = self.var {
            if gvar.has_data(self.data, gid) {
                let adv = self.advance(gid);
                self.orig.clear();
                self.orig.extend_from_slice(&offsets);
                self.orig.extend_from_slice(&[[pp1, 0.0], [pp1 + adv, 0.0], [0.0, 0.0], [0.0, 0.0]]);
                self.deltas.clear();
                self.deltas.resize(self.orig.len(), [0.0; 2]);
                let ok = gvar.glyph_deltas(
                    self.data,
                    gid,
                    coords,
                    &self.orig,
                    &[],
                    &mut self.deltas,
                    &mut self.scratch,
                );
                if ok.is_some() {
                    for (o, d) in offsets.iter_mut().zip(self.deltas.iter()) {
                        o[0] += d[0];
                        o[1] += d[1];
                    }
                    pp1 += self.deltas[nc][0];
                }
            }
        }

        let comp_base = out.pts.len();
        for (c, off) in comps.iter().zip(offsets.iter()) {
            let child_base = out.pts.len();
            self.load_rec(c.gid, depth + 1, out)?;
            let m = c.m;
            let identity = m == [1.0, 0.0, 0.0, 1.0];
            if !identity {
                for p in &mut out.pts[child_base..] {
                    let (x, y) = (p[0], p[1]);
                    *p = [m[0] * x + m[2] * y, m[1] * x + m[3] * y];
                }
            }
            let (dx, dy) = if c.flags & ARGS_ARE_XY != 0 {
                let (dx, dy) = (off[0], off[1]);
                if c.flags & SCALED_OFFSET != 0 && c.flags & UNSCALED_OFFSET == 0 && !identity {
                    (m[0] * dx + m[2] * dy, m[1] * dx + m[3] * dy)
                } else {
                    (dx, dy)
                }
            } else {
                // Point matching: parent point arg1 aligns with child point arg2.
                let p1 = comp_base.checked_add(c.arg1 as usize)?;
                let p2 = child_base.checked_add(c.arg2 as usize)?;
                if p1 < child_base && p2 < out.pts.len() {
                    (out.pts[p1][0] - out.pts[p2][0], out.pts[p1][1] - out.pts[p2][1])
                } else {
                    (0.0, 0.0)
                }
            };
            if dx != 0.0 || dy != 0.0 {
                for p in &mut out.pts[child_base..] {
                    p[0] += dx;
                    p[1] += dy;
                }
            }
        }
        Some(pp1)
    }

    /// Advance width delta of glyph `gid` computed from its `gvar` phantom
    /// points (used when the font has no `HVAR` table).
    pub fn phantom_advance_delta(&mut self, gid: u16) -> f32 {
        self.phantom_advance_delta_opt(gid).unwrap_or(0.0)
    }

    fn phantom_advance_delta_opt(&mut self, gid: u16) -> Option<f32> {
        let (gvar, coords) = self.var?;
        let g = self.glyf.glyph_data(self.data, gid, self.num_glyphs)?;
        let n_points = if g.is_empty() {
            0
        } else {
            let n_contours = u16_at(g, 0)? as i16;
            if n_contours > 0 {
                u16_at(g, 10 + 2 * (n_contours as usize - 1))? as usize + 1
            } else if n_contours == 0 {
                0
            } else {
                let mut r = Reader::at(g, 10);
                let mut n = 0usize;
                loop {
                    let flags = r.u16()?;
                    r.skip(2)?;
                    let mut skip = if flags & ARG_WORDS != 0 { 4 } else { 2 };
                    if flags & HAVE_SCALE != 0 {
                        skip += 2;
                    } else if flags & HAVE_XY_SCALE != 0 {
                        skip += 4;
                    } else if flags & HAVE_2X2 != 0 {
                        skip += 8;
                    }
                    r.skip(skip)?;
                    n += 1;
                    if flags & MORE_COMPONENTS == 0 || n > MAX_COMPONENTS {
                        break;
                    }
                }
                n
            }
        };
        if n_points > MAX_POINTS {
            return None;
        }
        self.orig.clear();
        self.orig.resize(n_points + 4, [0.0; 2]);
        self.deltas.clear();
        self.deltas.resize(n_points + 4, [0.0; 2]);
        gvar.glyph_deltas(self.data, gid, coords, &self.orig, &[], &mut self.deltas, &mut self.scratch)?;
        Some(self.deltas[n_points + 1][0] - self.deltas[n_points][0])
    }
}
