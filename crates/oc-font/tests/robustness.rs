//! Robustness: corrupted, truncated and random font data must never panic.

mod common;

use common::Rng;
use oc_font::{Font, OutlineSink};

struct Null;
impl OutlineSink for Null {
    fn move_to(&mut self, _: f32, _: f32) {}
    fn line_to(&mut self, _: f32, _: f32) {}
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
    fn close(&mut self) {}
}

/// Table directory entries (tag, offset, length) of a plain sfnt file.
fn tables(d: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let n = u16::from_be_bytes([d[4], d[5]]) as usize;
    (0..n)
        .map(|i| {
            let r = &d[12 + 16 * i..28 + 16 * i];
            let off = u32::from_be_bytes([r[8], r[9], r[10], r[11]]) as usize;
            let len = u32::from_be_bytes([r[12], r[13], r[14], r[15]]) as usize;
            ([r[0], r[1], r[2], r[3]], off, len)
        })
        .collect()
}

fn exercise(f: &mut Font, rng: &mut Rng, all_glyphs: bool) {
    let _ = (f.units_per_em(), f.ascent(), f.descent(), f.line_gap(), f.num_glyphs());
    let mut gids: Vec<u16> = "AVTo aeÄöß@&g1".chars().map(|c| f.glyph_id(c).unwrap_or(0)).collect();
    for _ in 0..8 {
        gids.push(rng.below(f.num_glyphs() as usize + 2) as u16);
    }
    let axes: Vec<_> = f.variation_axes().to_vec();
    for pass in 0..2 {
        if pass == 1 {
            for a in &axes {
                f.set_variation(a.tag, a.min + (a.max - a.min) * 0.8);
            }
        }
        for w in gids.windows(2) {
            let _ = f.advance(w[0]);
            let _ = f.kerning(w[0], w[1]);
            let _ = f.glyph_bounds(w[0]);
        }
        for &g in &gids {
            let _ = f.rasterize(g, 14.0, 0.3);
        }
        let _ = f.rasterize(gids[0], 1.0, 0.0);
        let _ = f.rasterize(gids[1], 150.0, 0.9);
        if all_glyphs {
            for g in 0..f.num_glyphs() {
                let _ = f.outline(g, &mut Null);
            }
        }
    }
}

/// Iteration count (overridable with `OC_FONT_FUZZ_ITERS`).
fn iters() -> usize {
    std::env::var("OC_FONT_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 200 } else { 1500 })
}

fn fuzz_file(path: &str, iterations: usize, full_every: usize, seed: u64) {
    let Ok(orig) = std::fs::read(path) else {
        eprintln!("note: {path} not found, skipping");
        return;
    };
    let dir = tables(&orig);
    let mut rng = Rng(seed);
    let mut parsed = 0;
    for it in 0..iterations {
        let mut d = orig.clone();
        match rng.below(10) {
            // Mutate bytes inside one table.
            0..=6 => {
                let (_, off, len) = dir[rng.below(dir.len())];
                if len == 0 {
                    continue;
                }
                // Bias towards table headers where offsets live.
                let span = if rng.below(2) == 0 { len.min(256) } else { len };
                for _ in 0..1 + rng.below(8) {
                    let p = off + rng.below(span);
                    if p < d.len() {
                        d[p] = match rng.below(4) {
                            0 => 0xFF,
                            1 => 0,
                            2 => d[p] ^ (1 << rng.below(8)),
                            _ => rng.next() as u8,
                        };
                    }
                }
            }
            // Mutate the table directory / header.
            7 => {
                for _ in 0..1 + rng.below(4) {
                    let p = rng.below(12 + 16 * dir.len());
                    d[p] = rng.next() as u8;
                }
            }
            // Truncate.
            8 => {
                let n = rng.below(d.len());
                d.truncate(n);
            }
            // Random bytes anywhere.
            _ => {
                for _ in 0..1 + rng.below(32) {
                    let p = rng.below(d.len());
                    d[p] = rng.next() as u8;
                }
            }
        }
        if let Ok(mut f) = Font::from_bytes(d) {
            parsed += 1;
            exercise(&mut f, &mut rng, it % full_every == 0);
        }
    }
    eprintln!("{path}: {iterations} mutations, {parsed} parsed");
}

#[test]
fn fuzz_inter_variable() {
    let path = common::inter_path();
    let n = iters();
    fuzz_file(path.to_str().unwrap(), n, 10, 0x1234_5678_9abc_def1);
}

#[test]
fn fuzz_cff() {
    let n = iters();
    fuzz_file("/usr/share/fonts/opentype/inter/Inter-Regular.otf", n, 10, 0x0bad_cafe_f00d_1234);
    fuzz_file("/usr/share/fonts/opentype/tlwg/Loma.otf", n, 5, 0x5eed_0001_0002_0003);
}

#[test]
fn fuzz_truetype_static() {
    let n = iters();
    fuzz_file("/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf", n, 10, 0x0123_4567_89ab_cdef);
}

#[test]
fn garbage_and_tiny_inputs() {
    let mut rng = Rng(42);
    assert!(Font::from_bytes(Vec::new()).is_err());
    for len in [0usize, 1, 4, 11, 12, 13, 27, 28, 100, 1000] {
        let mut d = vec![0u8; len];
        for b in d.iter_mut() {
            *b = rng.next() as u8;
        }
        let _ = Font::from_bytes(d.clone());
        // With a valid sfnt tag and table count.
        for tag in [[0u8, 1, 0, 0], *b"OTTO", *b"ttcf", *b"true"] {
            let mut e = d.clone();
            if e.len() >= 6 {
                e[..4].copy_from_slice(&tag);
                e[4] = 0;
                e[5] = 40;
            }
            if let Ok(mut f) = Font::from_bytes(e) {
                exercise(&mut f, &mut rng, true);
            }
        }
    }
    // Truncations of the real font at many lengths.
    let orig = std::fs::read(common::inter_path()).unwrap();
    for i in 0..200 {
        let n = (i * 7919 * 13) % orig.len();
        if let Ok(mut f) = Font::from_bytes(orig[..n].to_vec()) {
            exercise(&mut f, &mut rng, false);
        }
    }
}
