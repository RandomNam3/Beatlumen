//! Tests with system fonts from /usr/share/fonts (skipped when missing).
//!
//! Reference kerning values come from HarfBuzz (GPOS) and FreeType's
//! `FT_Get_Kerning` (legacy `kern` table).

mod common;

use common::{system_font, Canvas};
use bl_font::Font;

const DEJAVU: &str = "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf";
const FREESERIF: &str = "/usr/share/fonts/truetype/freefont/FreeSerif.ttf";
const LIBERATION: &str = "/usr/share/fonts/truetype/liberation/LiberationSans-Regular.ttf";
const INTER_OTF: &str = "/usr/share/fonts/opentype/inter/Inter-Regular.otf";
const INTER_OTF_BI: &str = "/usr/share/fonts/opentype/inter/Inter-BoldItalic.otf";
const IPA_GOTHIC: &str = "/usr/share/fonts/opentype/ipafont-gothic/ipag.ttf";
const UNIFONT: &str = "/usr/share/fonts/opentype/unifont/unifont.otf";
const WQY_TTC: &str = "/usr/share/fonts/truetype/wqy/wqy-zenhei.ttc";
const LOMA: &str = "/usr/share/fonts/opentype/tlwg/Loma.otf";

fn kern_pairs(f: &Font, expected: &[(&str, f32)]) {
    for (pair, v) in expected {
        let c: Vec<char> = pair.chars().collect();
        let (a, b) = (f.glyph_id(c[0]).unwrap(), f.glyph_id(c[1]).unwrap());
        assert_eq!(f.kerning(a, b), *v, "{pair}");
    }
}

fn advances(f: &Font, expected: &[(char, f32)]) {
    for (c, v) in expected {
        assert_eq!(f.advance(f.glyph_id(*c).unwrap()), *v, "{c:?}");
    }
}

/// Renames the GPOS table so that the legacy `kern` table is used.
fn without_gpos(path: &str) -> Option<Font> {
    let mut d = std::fs::read(path).ok()?;
    let n = u16::from_be_bytes([d[4], d[5]]) as usize;
    for i in 0..n {
        let rec = 12 + 16 * i;
        if &d[rec..rec + 4] == b"GPOS" {
            d[rec..rec + 4].copy_from_slice(b"XPOS");
        }
    }
    Some(Font::from_bytes(d).unwrap())
}

#[test]
fn dejavu_sans() {
    let Some(f) = system_font(DEJAVU) else { return };
    assert_eq!(f.units_per_em(), 2048);
    // USE_TYPO_METRICS not set: hhea metrics.
    assert_eq!((f.ascent(), f.descent(), f.line_gap()), (1901, -483, 0));
    advances(&f, &[('A', 1401.0), ('V', 1401.0), ('a', 1255.0), ('o', 1253.0), (' ', 651.0)]);
    let pairs = [
        ("AV", -131.0),
        ("To", -348.0),
        ("Te", -348.0),
        ("Yo", -272.0),
        ("LT", -282.0),
        ("P.", -319.0),
        ("WA", -112.0),
        ("Av", -120.0),
    ];
    kern_pairs(&f, &pairs);
    // Same values from the legacy kern table.
    let legacy = without_gpos(DEJAVU).unwrap();
    assert!(legacy.has_kerning());
    kern_pairs(&legacy, &pairs);
    assert!(f.variation_axes().is_empty());
}

#[test]
fn freeserif_big_kern_table() {
    // The kern subtable is larger than 64 KiB (its u16 length overflows).
    let Some(f) = system_font(FREESERIF) else { return };
    assert_eq!((f.ascent(), f.descent(), f.line_gap()), (800, -200, 100), "typo metrics");
    advances(&f, &[('A', 721.0), ('V', 701.0), ('a', 435.0), ('o', 491.0), (' ', 250.0)]);
    kern_pairs(&f, &[("AV", -57.0), ("To", -25.0), ("VA", -60.0), ("Yo", -41.0), ("P.", 0.0), ("WA", -60.0)]);
    let legacy = without_gpos(FREESERIF).unwrap();
    kern_pairs(&legacy, &[("AV", -57.0), ("To", -25.0), ("VA", -60.0), ("Yo", -41.0), ("P.", -50.0), ("WA", -60.0)]);
}

#[test]
fn liberation_sans() {
    let Some(f) = system_font(LIBERATION) else { return };
    kern_pairs(&f, &[("AV", -152.0), ("To", -227.0), ("Yo", -188.0), ("P.", -264.0), ("WA", -76.0), ("Av", -37.0)]);
}

#[test]
fn cff_inter_otf() {
    let Some(f) = system_font(INTER_OTF) else { return };
    assert_eq!(f.units_per_em(), 2048);
    assert_eq!((f.ascent(), f.descent(), f.line_gap()), (1984, -494, 0));
    advances(&f, &[('A', 1413.0), ('V', 1413.0), ('a', 1150.0), ('o', 1228.0), (' ', 576.0)]);
    kern_pairs(
        &f,
        &[("AV", -140.0), ("To", -160.0), ("VA", -140.0), ("Yo", -157.0), ("LT", -197.0), ("P.", -69.0), ("WA", -116.0)],
    );
    // CFF outlines: 'I' is a rectangle; check coverage = area.
    let g = f.glyph_id('I').unwrap();
    let (x0, y0, x1, y1) = f.glyph_bounds(g).unwrap();
    let s = 20.0 / 2048.0;
    let bm = f.rasterize(g, 20.0, 0.4).unwrap();
    let total: f32 = bm.coverage.iter().map(|&v| v as f32 / 255.0).sum();
    let area = (x1 - x0) * (y1 - y0) * s * s;
    assert!((total - area).abs() < 0.01 * area, "{total} vs {area}");
    // 'o' has two contours with opposite winding: center must be empty.
    let o = f.rasterize(f.glyph_id('o').unwrap(), 48.0, 0.0).unwrap();
    let center = o.coverage[(o.height / 2 * o.width + o.width / 2) as usize];
    assert_eq!(center, 0);
    // Every glyph in the font must decode.
    let mut drawn = 0;
    for g in 0..f.num_glyphs() {
        assert!(f.outline(g, &mut common_sink::Null), "glyph {g}");
        if f.rasterize(g, 13.0, 0.5).is_some() {
            drawn += 1;
        }
    }
    assert!(drawn > 2000, "{drawn}");
}

mod common_sink {
    pub struct Null;
    impl bl_font::OutlineSink for Null {
        fn move_to(&mut self, _: f32, _: f32) {}
        fn line_to(&mut self, _: f32, _: f32) {}
        fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {}
        fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {}
        fn close(&mut self) {}
    }
}

#[test]
fn all_inter_variable_glyphs_decode() {
    let mut f = common::inter();
    for w in [400.0, 650.0, 100.0] {
        f.set_variation(*b"wght", w);
        for g in 0..f.num_glyphs() {
            assert!(f.outline(g, &mut common_sink::Null), "glyph {g} at wght {w}");
            let _ = f.rasterize(g, 11.0, 0.3);
        }
    }
}

#[test]
fn cjk_fonts() {
    if let Some(f) = system_font(IPA_GOTHIC) {
        assert_eq!(f.units_per_em(), 2048);
        assert_eq!((f.ascent(), f.descent()), (1802, -246));
        let g = f.glyph_id('漢').unwrap();
        assert_eq!(f.advance(g), 2048.0);
        assert!(f.rasterize(g, 16.0, 0.0).is_some());
    }
    if let Some(f) = system_font(UNIFONT) {
        assert_eq!(f.units_per_em(), 64);
        let g = f.glyph_id('漢').unwrap();
        assert_eq!(f.advance(g), 64.0);
        // Pixel font at its native size: crisp, fully opaque pixels.
        let bm = f.rasterize(g, 16.0, 0.0).unwrap();
        assert!(bm.coverage.iter().all(|&c| c == 0 || c == 255));
        let a = f.rasterize(f.glyph_id('A').unwrap(), 16.0, 0.0).unwrap();
        assert!(a.coverage.iter().all(|&c| c == 0 || c == 255));
    }
    if let Ok(data) = std::fs::read(WQY_TTC) {
        assert_eq!(Font::collection_count(&data), 3);
        let f0 = Font::from_bytes(data.clone()).unwrap();
        let f1 = Font::from_collection(data.clone(), 1).unwrap();
        assert!(Font::from_collection(data, 3).is_err());
        for f in [&f0, &f1] {
            let g = f.glyph_id('中').unwrap();
            assert!(f.rasterize(g, 16.0, 0.0).is_some());
        }
    }
}

#[test]
fn render_system_fonts_png() {
    let mut c = Canvas::new(820, 420);
    let mut y = 10;
    let samples: [(&str, &str, f32); 9] = [
        (DEJAVU, "DejaVu Sans: The quick brown fox jumps over the lazy dog. ÄÖÜ äöü ß", 15.0),
        (LIBERATION, "Liberation Sans: AVATAR To Ye Wo — fiﬂ 1/2 € @", 15.0),
        (FREESERIF, "FreeSerif: Falsches Üben von Xylophonmusik quält jeden Zwerg", 16.0),
        (INTER_OTF, "Inter OTF (CFF): The quick brown fox jumps over the lazy dog", 16.0),
        (INTER_OTF_BI, "Inter Bold Italic (CFF): Größere Ärger, AV To Yo", 16.0),
        (IPA_GOTHIC, "IPAゴシック: 日本語のテキスト、漢字とひらがな。", 16.0),
        (UNIFONT, "Unifont (CFF): 日本語 ÄÖÜ Hello, world", 16.0),
        (WQY_TTC, "文泉驿正黑 (TTC): 中文字体测试，你好世界", 16.0),
        (LOMA, "Loma (CFF): ภาษาไทย Thai script sample", 16.0),
    ];
    for (path, text, px) in samples {
        y += 28;
        if let Ok(d) = std::fs::read(path) {
            let f = Font::from_bytes(d).unwrap();
            c.text(&f, text, px, 6.0, y, true);
        }
    }
    y += 50;
    if let Ok(d) = std::fs::read(INTER_OTF) {
        let f = Font::from_bytes(d).unwrap();
        c.text(&f, "CFF curves: Sg@&ß", 48.0, 6.0, y, true);
    }
    if let Ok(d) = std::fs::read(IPA_GOTHIC) {
        let f = Font::from_bytes(d).unwrap();
        c.text(&f, "漢字", 48.0, 520.0, y, true);
    }
    c.save("system-fonts.png");
}
