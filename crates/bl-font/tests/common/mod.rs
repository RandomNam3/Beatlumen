//! Shared test helpers: font loading, a tiny PNG writer (stored deflate
//! blocks) and a simple text renderer.
#![allow(dead_code)]

use bl_font::Font;
use std::path::PathBuf;

pub fn inter_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../assets/fonts/Inter-Variable.ttf")
}

pub fn inter() -> Font {
    let data = std::fs::read(inter_path()).expect("Inter-Variable.ttf missing");
    Font::from_bytes(data).expect("Inter parses")
}

/// Loads a system font, or prints a note and returns `None`.
pub fn system_font(path: &str) -> Option<Font> {
    match std::fs::read(path) {
        Ok(d) => Some(Font::from_bytes(d).unwrap_or_else(|e| panic!("{path}: {e}"))),
        Err(_) => {
            eprintln!("note: {path} not found, skipping");
            None
        }
    }
}

/// Output directory for rendered PNGs (`OC_FONT_PNG_DIR` or a temp dir).
pub fn out_dir() -> PathBuf {
    let dir = std::env::var_os("OC_FONT_PNG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("bl-font-test"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn crc32(chunks: &[&[u8]]) -> u32 {
    let mut table = [0u32; 256];
    for (i, t) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xEDB8_8320 ^ (c >> 1) } else { c >> 1 };
        }
        *t = c;
    }
    let mut crc = 0xFFFF_FFFFu32;
    for chunk in chunks {
        for &b in *chunk {
            crc = table[((crc ^ b as u32) & 0xFF) as usize] ^ (crc >> 8);
        }
    }
    crc ^ 0xFFFF_FFFF
}

fn adler32(data: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for &x in data {
        a = (a + x as u32) % 65521;
        b = (b + a) % 65521;
    }
    (b << 16) | a
}

/// Encodes an 8-bit grayscale image as PNG with uncompressed deflate blocks.
pub fn png_gray(w: usize, h: usize, pixels: &[u8]) -> Vec<u8> {
    let mut raw = Vec::with_capacity((w + 1) * h);
    for row in pixels.chunks(w).take(h) {
        raw.push(0);
        raw.extend_from_slice(row);
    }
    let mut z = vec![0x78, 0x01];
    let mut blocks = raw.chunks(65535).peekable();
    if blocks.peek().is_none() {
        z.extend_from_slice(&[1, 0, 0, 0xFF, 0xFF]);
    }
    while let Some(block) = blocks.next() {
        z.push(if blocks.peek().is_none() { 1 } else { 0 });
        let len = block.len() as u16;
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(block);
    }
    z.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let mut chunk = |tag: &[u8; 4], body: &[u8]| {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        png.extend_from_slice(tag);
        png.extend_from_slice(body);
        png.extend_from_slice(&crc32(&[tag, body]).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 0, 0, 0, 0]);
    chunk(b"IHDR", &ihdr);
    chunk(b"IDAT", &z);
    chunk(b"IEND", &[]);
    png
}

/// A grayscale canvas accumulating coverage (0 = background).
pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u8>,
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Canvas {
        Canvas { w, h, px: vec![0; w * h] }
    }

    /// Draws `text` with its baseline at `baseline` starting at pen `x`.
    /// Returns the final pen position.
    pub fn text(&mut self, font: &Font, text: &str, px_size: f32, x: f32, baseline: i32, kern: bool) -> f32 {
        let scale = px_size / font.units_per_em() as f32;
        let mut pen = x;
        let mut prev: Option<u16> = None;
        for ch in text.chars() {
            let g = font.glyph_id(ch).unwrap_or(0);
            if let (Some(p), true) = (prev, kern) {
                pen += font.kerning(p, g) * scale;
            }
            let fx = pen.floor();
            if let Some(bm) = font.rasterize(g, px_size, pen - fx) {
                self.blit(&bm, fx as i32 + bm.left, baseline - bm.top);
            }
            pen += font.advance(g) * scale;
            prev = Some(g);
        }
        pen
    }

    pub fn blit(&mut self, bm: &bl_font::GlyphBitmap, x0: i32, y0: i32) {
        for y in 0..bm.height as i32 {
            for x in 0..bm.width as i32 {
                let (cx, cy) = (x0 + x, y0 + y);
                if cx < 0 || cy < 0 || cx >= self.w as i32 || cy >= self.h as i32 {
                    continue;
                }
                let c = bm.coverage[(y as u32 * bm.width + x as u32) as usize];
                let d = &mut self.px[cy as usize * self.w + cx as usize];
                *d = d.saturating_add(c);
            }
        }
    }

    /// Writes the canvas as dark text on white.
    pub fn save(&self, name: &str) -> PathBuf {
        let inv: Vec<u8> = self.px.iter().map(|&c| 255 - c).collect();
        let path = out_dir().join(name);
        std::fs::write(&path, png_gray(self.w, self.h, &inv)).unwrap();
        eprintln!("wrote {}", path.display());
        path
    }
}

/// Deterministic xorshift PRNG for fuzzing.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}
