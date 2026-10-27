//! Text: font instances, glyph cache in a texture atlas, measuring,
//! ellipsizing, wrapping and drawing.

use oc_font::Font;
use oc_render::batch2d::ATLAS_SIZE;
use oc_render::{Batch2d, Color, Rect};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Weight {
    Regular,
    Medium,
    SemiBold,
    Bold,
}

impl Weight {
    fn index(self) -> usize {
        match self {
            Weight::Regular => 0,
            Weight::Medium => 1,
            Weight::SemiBold => 2,
            Weight::Bold => 3,
        }
    }
    fn wght(self) -> f32 {
        match self {
            Weight::Regular => 400.0,
            Weight::Medium => 500.0,
            Weight::SemiBold => 600.0,
            Weight::Bold => 700.0,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct GlyphKey {
    font: u8,
    glyph: u16,
    /// Physical pixel size * 4.
    size_q: u16,
    bin: u8,
}

#[derive(Debug, Clone, Copy)]
struct CachedGlyph {
    /// Atlas rectangle (empty glyphs have w = 0).
    atlas: Rect,
    left: f32,
    top: f32,
}

/// Simple shelf packer for the atlas.
struct Packer {
    x: u32,
    y: u32,
    row_h: u32,
}

impl Packer {
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        let pad = 1;
        if w + pad > ATLAS_SIZE || h + pad > ATLAS_SIZE {
            return None;
        }
        if self.x + w + pad > ATLAS_SIZE {
            self.x = 0;
            self.y += self.row_h;
            self.row_h = 0;
        }
        if self.y + h + pad > ATLAS_SIZE {
            return None;
        }
        let pos = (self.x, self.y);
        self.x += w + pad;
        self.row_h = self.row_h.max(h + pad);
        Some(pos)
    }
}

/// A pending atlas upload.
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
    pub data: Vec<u8>,
}

/// Image data cached in the atlas by other subsystems (icons).
#[derive(Debug, Clone, Copy)]
pub struct AtlasImage {
    pub atlas: Rect,
    /// Size in physical pixels.
    pub w: u32,
    pub h: u32,
}

pub struct FontSystem {
    fonts: Vec<Font>,
    fallbacks: Vec<Font>,
    cache: HashMap<GlyphKey, CachedGlyph>,
    images: HashMap<u64, AtlasImage>,
    packer: Packer,
    pending: Vec<Upload>,
    scale: f32,
    /// Incremented whenever the atlas was reset.
    pub generation: u64,
    char_map: HashMap<(char, usize), Option<(u8, u16)>>,
}

/// One positioned glyph of a laid out line (logical units, relative to
/// the line origin at the baseline).
#[derive(Debug, Clone, Copy)]
struct Placed {
    font: u8,
    glyph: u16,
    x: f32,
}

impl FontSystem {
    /// Creates the font system from a (variable) TrueType font.
    pub fn new(data: Vec<u8>) -> Result<FontSystem, String> {
        let base = Font::from_bytes(data.clone()).map_err(|e| e.to_string())?;
        let mut fonts = Vec::new();
        for w in [Weight::Regular, Weight::Medium, Weight::SemiBold, Weight::Bold] {
            let mut f = Font::from_bytes(data.clone()).map_err(|e| e.to_string())?;
            f.set_variation(*b"wght", w.wght());
            fonts.push(f);
        }
        drop(base);
        Ok(FontSystem {
            fonts,
            fallbacks: Vec::new(),
            cache: HashMap::new(),
            images: HashMap::new(),
            packer: Packer { x: 0, y: 0, row_h: 0 },
            pending: Vec::new(),
            scale: 1.0,
            generation: 0,
            char_map: HashMap::new(),
        })
    }

    /// Adds a fallback font for characters missing from the main font.
    pub fn add_fallback(&mut self, data: Vec<u8>) -> bool {
        match Font::from_bytes(data) {
            Ok(f) => {
                self.fallbacks.push(f);
                self.char_map.clear();
                true
            }
            Err(_) => false,
        }
    }

    pub fn set_scale(&mut self, scale: f32) {
        let scale = scale.clamp(0.25, 8.0);
        if (scale - self.scale).abs() > 1e-4 {
            self.scale = scale;
        }
    }

    pub fn scale(&self) -> f32 {
        self.scale
    }

    fn font(&self, idx: u8) -> &Font {
        let i = idx as usize;
        if i < self.fonts.len() {
            &self.fonts[i]
        } else {
            &self.fallbacks[i - self.fonts.len()]
        }
    }

    /// (font index, glyph id) for a character in the given weight.
    fn lookup(&mut self, c: char, weight: Weight) -> Option<(u8, u16)> {
        let wi = weight.index();
        if let Some(r) = self.char_map.get(&(c, wi)) {
            return *r;
        }
        let r = if let Some(g) = self.fonts[wi].glyph_id(c).filter(|&g| g != 0) {
            Some((wi as u8, g))
        } else {
            self.fallbacks
                .iter()
                .enumerate()
                .find_map(|(i, f)| f.glyph_id(c).filter(|&g| g != 0).map(|g| ((self.fonts.len() + i) as u8, g)))
                .or_else(|| self.fonts[wi].glyph_id('\u{fffd}').map(|g| (wi as u8, g)))
        };
        self.char_map.insert((c, wi), r);
        r
    }

    fn units(&self, font: u8, size: f32) -> f32 {
        size / self.font(font).units_per_em().max(1) as f32
    }

    pub fn ascent(&self, size: f32) -> f32 {
        self.fonts[0].ascent() as f32 * self.units(0, size)
    }

    pub fn descent(&self, size: f32) -> f32 {
        -(self.fonts[0].descent() as f32) * self.units(0, size)
    }

    /// Recommended line height.
    pub fn line_height(&self, size: f32) -> f32 {
        (self.ascent(size) + self.descent(size)) * 1.12
    }

    fn layout(&mut self, text: &str, size: f32, weight: Weight, out: &mut Vec<Placed>) -> f32 {
        out.clear();
        let mut x = 0.0f32;
        let mut prev: Option<(u8, u16)> = None;
        for c in text.chars() {
            let c = if c == '\t' { ' ' } else { c };
            if c.is_control() {
                continue;
            }
            let Some((font, glyph)) = self.lookup(c, weight) else { continue };
            if let Some((pf, pg)) = prev {
                if pf == font {
                    x += self.font(font).kerning(pg, glyph) * self.units(font, size);
                }
            }
            out.push(Placed { font, glyph, x });
            x += self.font(font).advance(glyph) * self.units(font, size);
            prev = Some((font, glyph));
        }
        x
    }

    /// Width of `text` in logical units.
    pub fn measure(&mut self, text: &str, size: f32, weight: Weight) -> f32 {
        let mut tmp = Vec::new();
        self.layout(text, size, weight, &mut tmp)
    }

    /// Shortens `text` with an ellipsis so it fits `max_width`.
    pub fn ellipsize(&mut self, text: &str, size: f32, weight: Weight, max_width: f32) -> String {
        if self.measure(text, size, weight) <= max_width {
            return text.to_string();
        }
        let ell = "…";
        let ell_w = self.measure(ell, size, weight);
        let mut placed = Vec::new();
        let total = self.layout(text, size, weight, &mut placed);
        let chars: Vec<(usize, char)> = text.char_indices().filter(|(_, c)| !c.is_control() || *c == '\t').collect();
        let mut cut = 0;
        for i in 0..placed.len() {
            let end = placed.get(i + 1).map(|n| n.x).unwrap_or(total);
            if end + ell_w > max_width {
                break;
            }
            cut = i + 1;
        }
        let byte_end = chars.get(cut).map(|(b, _)| *b).unwrap_or(text.len());
        let mut s = text[..byte_end].trim_end().to_string();
        if s.is_empty() && max_width < ell_w {
            return String::new();
        }
        s.push_str(ell);
        s
    }

    /// Splits text into lines no wider than `max_width` (breaking at
    /// spaces when possible, honouring '\n').
    pub fn wrap(&mut self, text: &str, size: f32, weight: Weight, max_width: f32) -> Vec<String> {
        let mut lines = Vec::new();
        for para in text.split('\n') {
            let mut line = String::new();
            for word in para.split(' ') {
                let candidate = if line.is_empty() { word.to_string() } else { format!("{line} {word}") };
                if self.measure(&candidate, size, weight) <= max_width || line.is_empty() {
                    if line.is_empty() && self.measure(word, size, weight) > max_width {
                        // Hard-break very long words.
                        let mut part = String::new();
                        for c in word.chars() {
                            part.push(c);
                            if self.measure(&part, size, weight) > max_width && part.chars().count() > 1 {
                                let last = part.pop().unwrap();
                                lines.push(std::mem::take(&mut part));
                                part.push(last);
                            }
                        }
                        line = part;
                    } else {
                        line = candidate;
                    }
                } else {
                    lines.push(std::mem::take(&mut line));
                    line = word.to_string();
                }
            }
            lines.push(line);
        }
        lines
    }

    /// Byte offset in `text` closest to horizontal position `x`.
    pub fn hit_test(&mut self, text: &str, size: f32, weight: Weight, x: f32) -> usize {
        let mut placed = Vec::new();
        let total = self.layout(text, size, weight, &mut placed);
        let idx: Vec<usize> = text.char_indices().filter(|(_, c)| !c.is_control()).map(|(i, _)| i).collect();
        for (i, p) in placed.iter().enumerate() {
            let next = placed.get(i + 1).map(|n| n.x).unwrap_or(total);
            if x < (p.x + next) / 2.0 {
                return idx.get(i).copied().unwrap_or(text.len());
            }
        }
        text.len()
    }

    /// X position of byte offset `byte` in `text`.
    pub fn caret_x(&mut self, text: &str, size: f32, weight: Weight, byte: usize) -> f32 {
        let b = byte.min(text.len());
        let mut end = b;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        self.measure(&text[..end], size, weight)
    }

    fn reset_atlas(&mut self) {
        self.cache.clear();
        self.images.clear();
        self.packer = Packer { x: 0, y: 0, row_h: 0 };
        self.pending.clear();
        self.generation += 1;
    }

    fn glyph(&mut self, key: GlyphKey) -> Option<CachedGlyph> {
        if let Some(g) = self.cache.get(&key) {
            return Some(*g);
        }
        let px = key.size_q as f32 / 4.0;
        let bm = self.font(key.font).rasterize(key.glyph, px, key.bin as f32 / 4.0);
        let cached = match bm {
            Some(b) if b.width > 0 && b.height > 0 => {
                let pos = match self.packer.alloc(b.width, b.height) {
                    Some(p) => p,
                    None => {
                        self.reset_atlas();
                        self.packer.alloc(b.width, b.height)?
                    }
                };
                self.pending.push(Upload { x: pos.0, y: pos.1, w: b.width, h: b.height, data: b.coverage });
                CachedGlyph { atlas: Rect::new(pos.0 as f32, pos.1 as f32, b.width as f32, b.height as f32), left: b.left as f32, top: b.top as f32 }
            }
            _ => CachedGlyph { atlas: Rect::default(), left: 0.0, top: 0.0 },
        };
        self.cache.insert(key, cached);
        Some(cached)
    }

    /// Stores (or looks up) an arbitrary coverage image in the atlas.
    pub fn atlas_image(&mut self, key: u64, make: impl FnOnce() -> (u32, u32, Vec<u8>)) -> Option<AtlasImage> {
        if let Some(i) = self.images.get(&key) {
            return Some(*i);
        }
        let (w, h, data) = make();
        if w == 0 || h == 0 || data.len() != (w * h) as usize {
            return None;
        }
        let pos = match self.packer.alloc(w, h) {
            Some(p) => p,
            None => {
                self.reset_atlas();
                self.packer.alloc(w, h)?
            }
        };
        self.pending.push(Upload { x: pos.0, y: pos.1, w, h, data });
        let img = AtlasImage { atlas: Rect::new(pos.0 as f32, pos.1 as f32, w as f32, h as f32), w, h };
        self.images.insert(key, img);
        Some(img)
    }

    /// Draws a single line of text with its top-left at (x, y); returns
    /// the advance width.
    pub fn draw(&mut self, batch: &mut Batch2d, x: f32, y: f32, text: &str, size: f32, weight: Weight, color: Color) -> f32 {
        if text.is_empty() || color.3 <= 0.0 {
            return if text.is_empty() { 0.0 } else { self.measure(text, size, weight) };
        }
        let s = self.scale;
        let mut placed = Vec::new();
        let width = self.layout(text, size, weight, &mut placed);
        let baseline = ((y + self.ascent(size)) * s).round();
        let size_q = ((size * s * 4.0).round() as u32).clamp(4, 4 * 512) as u16;
        let gen = self.generation;
        for p in &placed {
            let pen = (x + p.x) * s;
            let ix = pen.floor();
            let bin = (((pen - ix) * 4.0).floor() as u8).min(3);
            let key = GlyphKey { font: p.font, glyph: p.glyph, size_q, bin };
            let Some(g) = self.glyph(key) else { continue };
            if self.generation != gen {
                // Atlas was reset mid-draw; previously emitted quads point to
                // stale data for one frame, which is acceptable.
            }
            if g.atlas.w == 0.0 {
                continue;
            }
            let gx = (ix + g.left) / s;
            let gy = (baseline - g.top) / s;
            batch.glyph(Rect::new(gx, gy, g.atlas.w / s, g.atlas.h / s), g.atlas, color);
        }
        width
    }

    /// Draws text centred vertically in `r`, ellipsized to its width.
    pub fn draw_in(&mut self, batch: &mut Batch2d, r: Rect, text: &str, size: f32, weight: Weight, color: Color, align: Align) -> f32 {
        let t = self.ellipsize(text, size, weight, r.w.max(0.0));
        let w = self.measure(&t, size, weight);
        let x = match align {
            Align::Left => r.x,
            Align::Center => r.x + (r.w - w) / 2.0,
            Align::Right => r.right() - w,
        };
        let h = self.ascent(size) + self.descent(size);
        let y = r.y + (r.h - h) / 2.0;
        self.draw(batch, x, y, &t, size, weight, color);
        w
    }

    /// Uploads newly rasterized glyphs to the atlas texture.
    pub fn flush(&mut self, gl: &oc_gl::Gl, batch: &Batch2d) {
        for u in self.pending.drain(..) {
            batch.atlas_upload(gl, u.x, u.y, u.w, u.h, &u.data);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    Left,
    Center,
    Right,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs() -> FontSystem {
        let data = std::fs::read(concat!(env!("CARGO_MANIFEST_DIR"), "/../../assets/fonts/Inter-Variable.ttf")).unwrap();
        FontSystem::new(data).unwrap()
    }

    #[test]
    fn measure_and_ellipsize() {
        let mut f = fs();
        let w = f.measure("Hello World", 16.0, Weight::Regular);
        assert!(w > 60.0 && w < 120.0, "{w}");
        assert!(f.measure("Hello World", 16.0, Weight::Bold) > w);
        let e = f.ellipsize("A very long title that will not fit", 14.0, Weight::Regular, 80.0);
        assert!(e.ends_with('…'));
        assert!(f.measure(&e, 14.0, Weight::Regular) <= 80.5);
        let lines = f.wrap("one two three four five six seven", 14.0, Weight::Regular, 60.0);
        assert!(lines.len() > 2);
        assert_eq!(f.hit_test("abc", 14.0, Weight::Regular, 0.0), 0);
        assert_eq!(f.hit_test("abc", 14.0, Weight::Regular, 1000.0), 3);
    }
}
