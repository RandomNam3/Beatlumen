//! TrueType / OpenType font parsing and anti-aliased glyph rasterization,
//! std only.
//!
//! Features:
//! - TrueType (`glyf`) outlines including composite glyphs, and OpenType
//!   `CFF ` outlines (Type 2 charstrings, CID-keyed fonts).
//! - Character mapping via `cmap` formats 0, 4, 6, 10, 12 and 13.
//! - Metrics from `head`, `hhea`, `OS/2` (typographic metrics when
//!   `USE_TYPO_METRICS` is set) and `hmtx`.
//! - Pair kerning from GPOS (`kern` feature, PairPos formats 1/2, extension
//!   lookups) or the legacy `kern` table.
//! - Variable fonts: `fvar`, `avar`, `gvar` (with IUP), `HVAR` and variable
//!   GPOS device tables.
//! - An exact-area coverage rasterizer producing tight 8-bit bitmaps.
//!
//! The crate never panics on malformed data: parsing errors are reported by
//! [`Font::from_bytes`] and later lookups degrade to empty results.
//!
//! ```no_run
//! # fn main() -> Result<(), oc_font::Error> {
//! let data = std::fs::read("Inter-Variable.ttf").unwrap();
//! let mut font = oc_font::Font::from_bytes(data)?;
//! font.set_variation(*b"wght", 600.0);
//! let g = font.glyph_id('A').unwrap_or(0);
//! if let Some(bitmap) = font.rasterize(g, 16.0, 0.25) {
//!     println!("{}x{} at ({}, {})", bitmap.width, bitmap.height, bitmap.left, bitmap.top);
//! }
//! # Ok(())
//! # }
//! ```

#![forbid(unsafe_code)]

mod cff;
mod cmap;
mod glyf;
mod gvar;
mod hmtx;
mod kern;
mod outline;
mod raster;
mod reader;
mod sfnt;
mod var;

pub use outline::OutlineSink;
pub use var::VariationAxis;

use outline::{BoundsSink, Flattener, Line};
use reader::{i16_at, u16_at};
use sfnt::TableRange;

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid font data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported font feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Coverage bitmap of a rasterized glyph.
#[derive(Debug, Clone, Default)]
pub struct GlyphBitmap {
    pub width: u32,
    pub height: u32,
    /// Offset in pixels from the pen position to the left edge of the bitmap.
    pub left: i32,
    /// Offset in pixels from the baseline up to the top edge of the bitmap.
    pub top: i32,
    /// `width * height` coverage values (0 = empty, 255 = full), top row first.
    pub coverage: Vec<u8>,
}

/// Largest bitmap dimension produced by [`Font::rasterize`].
const MAX_BITMAP_DIM: f32 = 8192.0;
/// Largest bitmap area produced by [`Font::rasterize`].
const MAX_BITMAP_AREA: usize = 1 << 24;

enum Outlines {
    TrueType(glyf::Glyf),
    Cff(cff::Cff),
}

pub struct Font {
    data: Vec<u8>,
    units_per_em: u16,
    num_glyphs: u16,
    ascent: i16,
    descent: i16,
    line_gap: i16,
    x_height: Option<i16>,
    cap_height: Option<i16>,
    hmtx: Option<hmtx::Hmtx>,
    cmap: Option<cmap::Cmap>,
    outlines: Outlines,
    kerning: kern::Kerning,
    axes: Vec<VariationAxis>,
    avar: Option<TableRange>,
    gvar: Option<gvar::Gvar>,
    hvar: Option<var::Hvar>,
    /// Current user-space axis values.
    user_coords: Vec<f32>,
    /// Normalized (and `avar`-mapped) coordinates.
    coords: Vec<f32>,
    /// True when `coords` is not the default instance.
    var_active: bool,
    hvar_scalars: Vec<f32>,
    gdef_scalars: Vec<f32>,
}

impl std::fmt::Debug for Font {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Font")
            .field("units_per_em", &self.units_per_em)
            .field("num_glyphs", &self.num_glyphs)
            .field("cff", &matches!(self.outlines, Outlines::Cff(_)))
            .field("axes", &self.axes)
            .field("user_coords", &self.user_coords)
            .finish()
    }
}

impl Font {
    /// Parses a TrueType (glyf) font, or an OpenType/CFF font if supported.
    /// For a font collection (`.ttc`) the first font is loaded.
    pub fn from_bytes(data: Vec<u8>) -> Result<Font, Error> {
        Font::from_collection(data, 0)
    }

    /// Number of fonts in a font file (1 for plain fonts, n for `.ttc`
    /// collections, 0 if the data is not a recognized font).
    pub fn collection_count(data: &[u8]) -> u32 {
        sfnt::collection_count(data)
    }

    /// Parses font number `index` of a font collection (`index` 0 also
    /// accepts plain font files).
    pub fn from_collection(data: Vec<u8>, index: u32) -> Result<Font, Error> {
        let offset = sfnt::font_offset(&data, index)?;
        let dir = sfnt::parse_directory(&data, offset)?;
        let d = &data[..];

        let head = dir.get(b"head").ok_or(Error::InvalidData("missing head table"))?;
        let units_per_em = u16_at(d, head.start + 18).ok_or(Error::InvalidData("truncated head table"))?;
        if !(16..=16384).contains(&units_per_em) {
            return Err(Error::InvalidData("unitsPerEm out of range"));
        }
        let long_loca = i16_at(d, head.start + 50).ok_or(Error::InvalidData("truncated head table"))? != 0;

        let maxp = dir.get(b"maxp").ok_or(Error::InvalidData("missing maxp table"))?;
        let mut num_glyphs = u16_at(d, maxp.start + 4).ok_or(Error::InvalidData("truncated maxp table"))?;

        // Vertical metrics.
        let hhea = dir.get(b"hhea").filter(|r| r.len() >= 36);
        let (mut ascent, mut descent, mut line_gap, num_hmetrics) = match &hhea {
            Some(r) => (
                i16_at(d, r.start + 4).unwrap_or(0),
                i16_at(d, r.start + 6).unwrap_or(0),
                i16_at(d, r.start + 8).unwrap_or(0),
                u16_at(d, r.start + 34).unwrap_or(0),
            ),
            None => (0, 0, 0, 0),
        };
        let mut x_height = None;
        let mut cap_height = None;
        if let Some(os2) = dir.get(b"OS/2").filter(|r| r.len() >= 78) {
            let s = os2.start;
            let version = u16_at(d, s).unwrap_or(0);
            let fs_selection = u16_at(d, s + 62).unwrap_or(0);
            let typo = (
                i16_at(d, s + 68).unwrap_or(0),
                i16_at(d, s + 70).unwrap_or(0),
                i16_at(d, s + 72).unwrap_or(0),
            );
            let win = (u16_at(d, s + 74).unwrap_or(0), u16_at(d, s + 76).unwrap_or(0));
            const USE_TYPO_METRICS: u16 = 1 << 7;
            if fs_selection & USE_TYPO_METRICS != 0 && (typo.0 != 0 || typo.1 != 0) {
                (ascent, descent, line_gap) = typo;
            } else if ascent == 0 && descent == 0 {
                if typo.0 != 0 || typo.1 != 0 {
                    (ascent, descent, line_gap) = typo;
                } else {
                    ascent = win.0.min(i16::MAX as u16) as i16;
                    descent = -(win.1.min(i16::MAX as u16) as i16);
                    line_gap = 0;
                }
            }
            if version >= 2 && os2.len() >= 90 {
                x_height = i16_at(d, s + 86).filter(|&v| v > 0);
                cap_height = i16_at(d, s + 88).filter(|&v| v > 0);
            }
        }

        let hmtx = dir.get(b"hmtx").map(|r| {
            let max_metrics = (r.len() / 4).min(u16::MAX as usize) as u16;
            hmtx::Hmtx::new(r, num_hmetrics.min(max_metrics))
        });
        let cmap = dir.get(b"cmap").and_then(|r| cmap::parse(d, r.start, r.end));

        let outlines = match (dir.get(b"glyf"), dir.get(b"loca"), dir.get(b"CFF ")) {
            (Some(glyf), Some(loca), _) => {
                let entry = if long_loca { 4 } else { 2 };
                let max = (loca.len() / entry).saturating_sub(1);
                num_glyphs = num_glyphs.min(max.min(u16::MAX as usize) as u16);
                Outlines::TrueType(glyf::Glyf { loca, glyf, long_loca })
            }
            (_, _, Some(cff)) => {
                let cff = cff::Cff::parse(d, cff).ok_or(Error::InvalidData("malformed CFF table"))?;
                num_glyphs = num_glyphs.min(cff.num_glyphs().min(u16::MAX as u32) as u16);
                Outlines::Cff(cff)
            }
            _ => {
                return Err(if dir.get(b"CFF2").is_some() {
                    Error::Unsupported("CFF2 outlines")
                } else {
                    Error::Unsupported("no glyf or CFF outlines")
                })
            }
        };

        let kerning = kern::Kerning::parse(d, dir.get(b"GPOS"), dir.get(b"kern"), dir.get(b"GDEF"));

        let axes = dir.get(b"fvar").map(|r| var::parse_fvar(d, r)).unwrap_or_default();
        let (avar, gvar, hvar) = if axes.is_empty() {
            (None, None, None)
        } else {
            let gvar = match outlines {
                Outlines::TrueType(_) => dir.get(b"gvar").and_then(|r| gvar::Gvar::parse(d, r)),
                Outlines::Cff(_) => None,
            };
            (dir.get(b"avar"), gvar, dir.get(b"HVAR").and_then(|r| var::Hvar::parse(d, r)))
        };
        let user_coords = axes.iter().map(|a| a.default).collect();
        let coords = vec![0.0; axes.len()];

        Ok(Font {
            data,
            units_per_em,
            num_glyphs,
            ascent,
            descent,
            line_gap,
            x_height,
            cap_height,
            hmtx,
            cmap,
            outlines,
            kerning,
            axes,
            avar,
            gvar,
            hvar,
            user_coords,
            coords,
            var_active: false,
            hvar_scalars: Vec::new(),
            gdef_scalars: Vec::new(),
        })
    }

    pub fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Typographic ascent (positive, font units).
    pub fn ascent(&self) -> i16 {
        self.ascent
    }

    /// Typographic descent (negative, font units).
    pub fn descent(&self) -> i16 {
        self.descent
    }

    pub fn line_gap(&self) -> i16 {
        self.line_gap
    }

    /// Height of lowercase letters (OS/2 `sxHeight`), if known.
    pub fn x_height(&self) -> Option<i16> {
        self.x_height
    }

    /// Height of capital letters (OS/2 `sCapHeight`), if known.
    pub fn cap_height(&self) -> Option<i16> {
        self.cap_height
    }

    /// Number of glyphs in the font.
    pub fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    pub fn glyph_id(&self, c: char) -> Option<u16> {
        let cmap = self.cmap.as_ref()?;
        let g = cmap.lookup(&self.data, c as u32);
        (g != 0 && g < self.num_glyphs).then_some(g)
    }

    /// Horizontal advance in font units (with the current variation applied).
    pub fn advance(&self, glyph: u16) -> f32 {
        let Some(h) = &self.hmtx else { return 0.0 };
        if glyph >= self.num_glyphs {
            return 0.0;
        }
        let mut adv = h.advance(&self.data, glyph) as f32;
        if self.var_active {
            if let Some(hv) = &self.hvar {
                adv += hv.advance_delta(&self.data, glyph, &self.hvar_scalars);
            } else if let (Some(gv), Outlines::TrueType(g)) = (&self.gvar, &self.outlines) {
                let mut loader =
                    glyf::Loader::new(&self.data, g, Some(h), self.num_glyphs, Some((gv, &self.coords)));
                adv += loader.phantom_advance_delta(glyph);
            }
        }
        adv.max(0.0)
    }

    /// Pair kerning adjustment in font units (GPOS 'kern' pair positioning,
    /// falling back to the legacy 'kern' table).
    pub fn kerning(&self, left: u16, right: u16) -> f32 {
        if self.kerning.is_empty() {
            return 0.0;
        }
        let scalars: &[f32] = if self.var_active { &self.gdef_scalars } else { &[] };
        self.kerning.kerning(&self.data, left, right, scalars)
    }

    /// True if the font has pair kerning data (GPOS or `kern`).
    pub fn has_kerning(&self) -> bool {
        !self.kerning.is_empty()
    }

    /// Variation axes of a variable font (empty for static fonts).
    pub fn variation_axes(&self) -> &[VariationAxis] {
        &self.axes
    }

    /// Current user-space value of a variation axis.
    pub fn variation(&self, axis: [u8; 4]) -> Option<f32> {
        self.axes.iter().position(|a| a.tag == axis).map(|i| self.user_coords[i])
    }

    /// Sets a variation axis value (e.g. `*b"wght"`, 600.0) for variable
    /// fonts. Unknown axes are ignored.
    pub fn set_variation(&mut self, axis: [u8; 4], value: f32) {
        let mut changed = false;
        for (a, u) in self.axes.iter().zip(self.user_coords.iter_mut()) {
            if a.tag == axis {
                *u = if value.is_nan() { a.default } else { value.clamp(a.min, a.max) };
                changed = true;
            }
        }
        if changed {
            self.update_coords();
        }
    }

    fn update_coords(&mut self) {
        for ((c, a), &u) in self.coords.iter_mut().zip(self.axes.iter()).zip(self.user_coords.iter()) {
            *c = var::normalize(a, u);
        }
        var::apply_avar(&self.data, self.avar.clone(), &mut self.coords);
        self.var_active = self.coords.iter().any(|&c| c != 0.0);
        self.hvar_scalars.clear();
        self.gdef_scalars.clear();
        if self.var_active {
            if let Some(h) = &self.hvar {
                self.hvar_scalars = h.store.region_scalars(&self.data, &self.coords);
            }
            if let Some(s) = &self.kerning.gdef_store {
                self.gdef_scalars = s.region_scalars(&self.data, &self.coords);
            }
        }
    }

    /// Emits the outline of `glyph` in font units (y up) with the current
    /// variation applied. Returns `false` if the glyph does not exist or its
    /// data is malformed; empty glyphs return `true` without emitting
    /// anything.
    pub fn outline(&self, glyph: u16, sink: &mut dyn OutlineSink) -> bool {
        if glyph >= self.num_glyphs {
            return false;
        }
        match &self.outlines {
            Outlines::TrueType(g) => {
                let var = match (&self.gvar, self.var_active) {
                    (Some(gv), true) => Some((gv, &self.coords[..])),
                    _ => None,
                };
                let mut loader = glyf::Loader::new(&self.data, g, self.hmtx.as_ref(), self.num_glyphs, var);
                let mut o = glyf::Outline::default();
                if loader.load(glyph, &mut o).is_none() {
                    return false;
                }
                outline::emit_quadratic(&o.pts, &o.on, &o.ends, sink);
                true
            }
            Outlines::Cff(c) => c.outline(&self.data, glyph, sink).is_some(),
        }
    }

    /// Control box of the glyph outline in font units as
    /// `(x_min, y_min, x_max, y_max)`, or `None` for empty glyphs.
    pub fn glyph_bounds(&self, glyph: u16) -> Option<(f32, f32, f32, f32)> {
        let mut b = BoundsSink::default();
        if !self.outline(glyph, &mut b) || b.min_x > b.max_x {
            return None;
        }
        Some((b.min_x, b.min_y, b.max_x, b.max_y))
    }

    /// Rasterizes a glyph at `px_size` pixels per em with a horizontal
    /// sub-pixel offset in [0, 1). Returns `None` for empty glyphs (space).
    pub fn rasterize(&self, glyph: u16, px_size: f32, offset_x: f32) -> Option<GlyphBitmap> {
        if !(px_size.is_finite() && px_size > 0.0) {
            return None;
        }
        let dx = if offset_x.is_finite() { offset_x } else { 0.0 };
        let scale = px_size / self.units_per_em as f32;
        let mut fl = Flattener::new(scale, dx);
        if !self.outline(glyph, &mut fl) {
            return None;
        }
        fl.finish();
        if fl.lines.is_empty() || fl.overflow {
            return None;
        }
        let x0 = fl.min_x.floor();
        let y0 = fl.min_y.floor();
        let x1 = fl.max_x.ceil();
        let y1 = fl.max_y.ceil();
        let (wf, hf) = (x1 - x0, y1 - y0);
        if !(wf.is_finite() && hf.is_finite())
            || wf <= 0.0
            || hf <= 0.0
            || wf > MAX_BITMAP_DIM
            || hf > MAX_BITMAP_DIM
            || x0.abs() > 1.0e6
            || y0.abs() > 1.0e6
        {
            return None;
        }
        let (w, h) = (wf as usize, hf as usize);
        if w * h > MAX_BITMAP_AREA {
            return None;
        }
        let mut ras = raster::Raster::new(w, h);
        for l in &fl.lines {
            ras.draw_line(&Line { x0: l.x0 - x0, y0: l.y0 - y0, x1: l.x1 - x0, y1: l.y1 - y0 });
        }
        let coverage = ras.coverage();
        if coverage.iter().all(|&c| c == 0) {
            return None;
        }
        Some(GlyphBitmap { width: w as u32, height: h as u32, left: x0 as i32, top: -(y0 as i32), coverage })
    }
}
