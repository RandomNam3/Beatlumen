//! TrueType / OpenType font parsing and anti-aliased glyph rasterization,
//! std only.

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

pub struct Font {
    _data: Vec<u8>,
}

impl Font {
    /// Parses a TrueType (glyf) font, or an OpenType/CFF font if supported.
    pub fn from_bytes(data: Vec<u8>) -> Result<Font, Error> {
        let _ = data;
        Err(Error::Unsupported("not implemented yet"))
    }
    pub fn units_per_em(&self) -> u16 {
        1000
    }
    /// Typographic ascent (positive, font units).
    pub fn ascent(&self) -> i16 {
        0
    }
    /// Typographic descent (negative, font units).
    pub fn descent(&self) -> i16 {
        0
    }
    pub fn line_gap(&self) -> i16 {
        0
    }
    pub fn glyph_id(&self, c: char) -> Option<u16> {
        let _ = c;
        None
    }
    /// Horizontal advance in font units (with the current variation applied).
    pub fn advance(&self, glyph: u16) -> f32 {
        let _ = glyph;
        0.0
    }
    /// Pair kerning adjustment in font units (GPOS 'kern' pair positioning,
    /// falling back to the legacy 'kern' table).
    pub fn kerning(&self, left: u16, right: u16) -> f32 {
        let _ = (left, right);
        0.0
    }
    /// Sets a variation axis value (e.g. `*b"wght"`, 600.0) for variable
    /// fonts. Unknown axes are ignored.
    pub fn set_variation(&mut self, axis: [u8; 4], value: f32) {
        let _ = (axis, value);
    }
    /// Rasterizes a glyph at `px_size` pixels per em with a horizontal
    /// sub-pixel offset in [0, 1). Returns `None` for empty glyphs (space).
    pub fn rasterize(&self, glyph: u16, px_size: f32, offset_x: f32) -> Option<GlyphBitmap> {
        let _ = (glyph, px_size, offset_x);
        None
    }
}
