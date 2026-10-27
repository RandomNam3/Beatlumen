//! Tests against the bundled Inter variable font (assets/fonts).
//!
//! Reference advances and kerning values were obtained with FreeType 2.14 /
//! HarfBuzz (via Pillow) at a size of 2816 px (= unitsPerEm).

mod common;

use common::{inter, Canvas};
use oc_font::{Font, OutlineSink};

fn gid(f: &Font, c: char) -> u16 {
    f.glyph_id(c).unwrap_or_else(|| panic!("no glyph for {c:?}"))
}

#[test]
fn metrics() {
    let f = inter();
    assert_eq!(f.units_per_em(), 2816);
    assert_eq!(f.ascent(), 2728);
    assert_eq!(f.descent(), -680);
    assert_eq!(f.line_gap(), 0);
    assert_eq!(f.num_glyphs(), 2537);
    assert!(f.cap_height().is_some() && f.x_height().is_some());
    let axes = f.variation_axes();
    assert_eq!(axes.len(), 2);
    assert_eq!(axes[0].tag, *b"wght");
    assert_eq!((axes[0].min, axes[0].default, axes[0].max), (100.0, 400.0, 900.0));
    assert_eq!(axes[1].tag, *b"slnt");
    assert_eq!((axes[1].min, axes[1].default, axes[1].max), (-10.0, 0.0, 0.0));
    assert!(f.has_kerning());
}

#[test]
fn ascii_glyph_ids_and_advances() {
    let f = inter();
    for c in ' '..='~' {
        let g = f.glyph_id(c).unwrap_or_else(|| panic!("no glyph for {c:?}"));
        assert!(f.advance(g) > 0.0, "{c:?}");
    }
    assert_eq!(f.glyph_id('\u{10FFFF}'), None);
    assert_eq!(f.glyph_id('\u{0}'), None);
    // Non-BMP lookup goes through the format 12 subtable and must not fail.
    let _ = f.glyph_id('\u{1F600}');
    let expected = [
        ('A', 1904.0),
        ('V', 1904.0),
        ('a', 1588.0),
        ('o', 1680.0),
        ('H', 2084.0),
        (' ', 792.0),
        ('Ä', 1904.0),
        ('W', 2672.0),
        ('T', 1808.0),
        ('g', 1716.0),
    ];
    for (c, adv) in expected {
        assert_eq!(f.advance(gid(&f, c)), adv, "{c:?}");
    }
    // Out of range glyph ids.
    assert_eq!(f.advance(u16::MAX), 0.0);
    assert!(f.rasterize(u16::MAX, 16.0, 0.0).is_none());
}

#[test]
fn variable_advances() {
    let mut f = inter();
    let table: [(f32, [f32; 9]); 4] = [
        (600.0, [2038.0, 2038.0, 1618.0, 1712.0, 2095.0, 699.0, 2835.0, 1858.0, 1761.0]),
        (700.0, [2106.0, 2106.0, 1634.0, 1728.0, 2101.0, 653.0, 2917.0, 1882.0, 1783.0]),
        (900.0, [2240.0, 2240.0, 1664.0, 1760.0, 2112.0, 560.0, 3080.0, 1932.0, 1828.0]),
        (100.0, [1736.0, 1736.0, 1524.0, 1632.0, 2072.0, 792.0, 2512.0, 1728.0, 1644.0]),
    ];
    let chars = ['A', 'V', 'a', 'o', 'H', ' ', 'W', 'T', 'g'];
    for (w, advs) in table {
        f.set_variation(*b"wght", w);
        assert_eq!(f.variation(*b"wght"), Some(w));
        for (c, adv) in chars.iter().zip(advs) {
            let a = f.advance(gid(&f, *c));
            // FreeType reports rounded advances.
            assert!((a - adv).abs() <= 0.5 + 1e-3, "wght {w} {c:?}: {a} vs {adv}");
        }
    }
    // Back to default restores the static advances.
    f.set_variation(*b"wght", 400.0);
    assert_eq!(f.advance(gid(&f, 'A')), 1904.0);
    // Unknown axes are ignored, out of range values are clamped.
    f.set_variation(*b"XXXX", 3.0);
    f.set_variation(*b"wght", 5000.0);
    assert_eq!(f.variation(*b"wght"), Some(900.0));
    assert_eq!(f.advance(gid(&f, 'A')), 2240.0);
}

#[test]
fn kerning_pairs() {
    let mut f = inter();
    let k = |f: &Font, a: char, b: char| f.kerning(gid(f, a), gid(f, b));
    let default = [
        ("AV", -192.0),
        ("To", -224.0),
        ("Te", -224.0),
        ("VA", -176.0),
        ("Yo", -216.0),
        ("LT", -272.0),
        ("P.", -96.0),
        ("AT", -240.0),
        ("WA", -160.0),
        ("Av", -192.0),
    ];
    for (p, v) in default {
        let c: Vec<char> = p.chars().collect();
        assert_eq!(k(&f, c[0], c[1]), v, "{p}");
    }
    assert_eq!(k(&f, 'H', 'H'), 0.0);
    assert_eq!(k(&f, 'o', 'o'), 0.0);
    // Variable kerning (GPOS VariationIndex device tables via GDEF).
    f.set_variation(*b"wght", 700.0);
    let bold = [("AV", -254.40625), ("VA", -272.0), ("Yo", -319.2031), ("LT", -255.2031), ("AT", -252.0), ("WA", -208.0)];
    for (p, v) in bold {
        let c: Vec<char> = p.chars().collect();
        let got = k(&f, c[0], c[1]);
        assert!((got - v).abs() < 1.0, "{p}: {got} vs {v}");
    }
}

#[derive(Default)]
struct Counter {
    moves: usize,
    segments: usize,
}

impl OutlineSink for Counter {
    fn move_to(&mut self, _: f32, _: f32) {
        self.moves += 1;
    }
    fn line_to(&mut self, _: f32, _: f32) {
        self.segments += 1;
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        self.segments += 1;
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.segments += 1;
    }
    fn close(&mut self) {}
}

#[test]
fn composite_glyph() {
    let mut f = inter();
    let a = gid(&f, 'A');
    let ae = gid(&f, 'Ä');
    let (ax0, ay0, ax1, ay1) = f.glyph_bounds(a).unwrap();
    let (ex0, ey0, ex1, ey1) = f.glyph_bounds(ae).unwrap();
    // Same base glyph, plus a dieresis above the cap height.
    assert_eq!((ax0, ay0, ax1), (ex0, ey0, ex1));
    assert!(ey1 > ay1 + 200.0, "{ey1} vs {ay1}");
    let mut ca = Counter::default();
    let mut ce = Counter::default();
    assert!(f.outline(a, &mut ca));
    assert!(f.outline(ae, &mut ce));
    assert_eq!(ce.moves, ca.moves + 2, "A plus two dots");
    // The composite follows the variation of its components.
    f.set_variation(*b"wght", 900.0);
    let (bx0, _, bx1, by1) = f.glyph_bounds(ae).unwrap();
    assert!(bx1 - bx0 > ex1 - ex0 + 100.0);
    let (_, _, _, aby1) = f.glyph_bounds(gid(&f, 'A')).unwrap();
    assert!(by1 > aby1 + 200.0);
}

#[test]
fn coverage_matches_area() {
    // 'I', '-' and '|' are plain rectangles in Inter.
    let f = inter();
    for c in ['I', '-', '|'] {
        let g = gid(&f, c);
        let (x0, y0, x1, y1) = f.glyph_bounds(g).unwrap();
        for &px in &[12.0f32, 16.0, 33.3, 100.0] {
            for &dx in &[0.0f32, 0.3, 0.75] {
                let s = px / f.units_per_em() as f32;
                let bm = f.rasterize(g, px, dx).unwrap();
                let total: f32 = bm.coverage.iter().map(|&v| v as f32 / 255.0).sum();
                let area = (x1 - x0) * (y1 - y0) * s * s;
                assert!((total - area).abs() < 0.01 * area + 0.05, "{c:?} {px}px: {total} vs {area}");
                // Tight bitmap: placement consistent with the outline bounds.
                assert_eq!(bm.left, (x0 * s + dx).floor() as i32, "{c:?}");
                assert_eq!(bm.top, (y1 * s).ceil() as i32, "{c:?}");
                assert_eq!(bm.width as i32, ((x1 * s + dx).ceil() - (x0 * s + dx).floor()) as i32);
            }
        }
    }
}

#[test]
fn empty_and_degenerate() {
    let f = inter();
    let space = gid(&f, ' ');
    assert!(f.rasterize(space, 16.0, 0.0).is_none());
    let a = gid(&f, 'a');
    assert!(f.rasterize(a, 0.0, 0.0).is_none());
    assert!(f.rasterize(a, -3.0, 0.0).is_none());
    assert!(f.rasterize(a, f32::NAN, 0.0).is_none());
    assert!(f.rasterize(a, f32::INFINITY, 0.0).is_none());
    assert!(f.rasterize(a, 1.0e6, 0.0).is_none(), "huge bitmaps are refused");
    let bm = f.rasterize(a, 16.0, f32::NAN).unwrap();
    assert!(bm.width > 0);
    let bm = f.rasterize(a, 0.5, 0.0).unwrap();
    assert!(bm.width == 1 && bm.height <= 2, "{}x{}", bm.width, bm.height);
}

#[test]
fn subpixel_offset_shifts_edges() {
    // The left edge of the vertical stem of 'l', derived from the coverage
    // of its first pixel, moves exactly by the sub-pixel offset.
    let f = inter();
    let g = gid(&f, 'l');
    let edge = |dx: f32| {
        let bm = f.rasterize(g, 16.0, dx).unwrap();
        let row = bm.height / 2;
        let cov = bm.coverage[(row * bm.width) as usize] as f32 / 255.0;
        bm.left as f32 + 1.0 - cov
    };
    let e0 = edge(0.0);
    let expected = 216.0 * 16.0 / 2816.0;
    assert!((e0 - expected).abs() < 0.005, "{e0} vs {expected}");
    for dx in [0.25f32, 0.5, 0.75, 0.9] {
        let e = edge(dx);
        assert!((e - e0 - dx).abs() < 0.005, "{dx}: {e} vs {e0}");
    }
}

fn render_sample(f: &mut Font, wght: f32, name: &str) {
    f.set_variation(*b"wght", wght);
    let lines = [
        "The quick brown fox jumps over the lazy dog. 0123456789",
        "Falsches Üben von Xylophonmusik quält jeden größeren Zwerg.",
        "ÄÖÜ äöü ß — AV To Te Yo LT P. WA “Quotes” (1/2) @#&%",
    ];
    let mut c = Canvas::new(900, 260);
    let mut y = 18;
    for &px in &[12.0f32, 16.0, 32.0] {
        for line in &lines {
            y += (px * 1.25) as i32;
            c.text(f, line, px, 6.0, y, true);
        }
        y += 4;
    }
    c.save(name);
}

#[test]
fn render_pangrams_png() {
    let mut f = inter();
    render_sample(&mut f, 400.0, "inter-regular.png");
    render_sample(&mut f, 700.0, "inter-bold.png");
    // Weight ramp at 24 px.
    let mut c = Canvas::new(900, 330);
    for (i, w) in [100.0f32, 200.0, 300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 900.0].iter().enumerate() {
        f.set_variation(*b"wght", *w);
        c.text(&f, &format!("Hamburgefonstiv Ärger {w}"), 28.0, 6.0, 34 + 34 * i as i32, true);
    }
    f.set_variation(*b"wght", 400.0);
    f.set_variation(*b"slnt", -10.0);
    c.text(&f, "Slanted -10°", 28.0, 520.0, 34, true);
    c.save("inter-weights.png");
    // Large glyphs to inspect curve quality.
    let mut f = inter();
    let mut c = Canvas::new(700, 260);
    let x = c.text(&f, "Rag&ß", 160.0, 6.0, 190, true);
    f.set_variation(*b"wght", 900.0);
    c.text(&f, "g", 160.0, x + 10.0, 190, true);
    c.save("inter-large.png");
}

#[test]
fn rasterize_speed() {
    let mut f = inter();
    let glyphs: Vec<u16> = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789&@ÄÖÜß"
        .chars()
        .map(|c| gid(&f, c))
        .collect();
    let mut report = String::new();
    for (wght, label) in [(400.0f32, "default"), (600.0, "wght=600")] {
        f.set_variation(*b"wght", wght);
        for &px in &[16.0f32, 32.0] {
            let iters = if cfg!(debug_assertions) { 3 } else { 300 };
            let t = std::time::Instant::now();
            let mut sum = 0usize;
            for i in 0..iters {
                for &g in &glyphs {
                    if let Some(b) = f.rasterize(g, px, (i % 4) as f32 * 0.25) {
                        sum += b.coverage.len();
                    }
                }
            }
            let us = t.elapsed().as_secs_f64() * 1e6 / (iters * glyphs.len()) as f64;
            assert!(sum > 0);
            report += &format!("{label} {px}px: {us:.2} us/glyph\n");
        }
    }
    eprintln!("rasterize speed:\n{report}");
}
