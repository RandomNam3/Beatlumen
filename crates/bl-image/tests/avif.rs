//! AVIF and HEIC: the AV1 decoder bit-exact against dav1d (through ffmpeg)
//! for many encoders and coding tools, the AVIF container (alpha, grids,
//! rotation, mirroring, clean aperture, image sequences) and HEIC through a
//! stand-in system decoder. AV1 fixtures are made with ffmpeg (libaom,
//! SVT-AV1, rav1e); missing encoders skip their cases with a note.

mod common;

use bl_image::{decode, decode_fit, info, Fit, Format, Image};
use common::heif as hf;
use common::*;
use std::path::Path;
use std::process::Command;

/// Encodes one frame of `src` (an ffmpeg lavfi source) into an OBU stream.
fn encode_obu(dir: &Path, name: &str, src: &str, args: &[&str]) -> Option<Vec<u8>> {
    let out = dir.join(format!("{name}.obu"));
    let o = out.to_str()?.to_string();
    let mut a = vec!["-f", "lavfi", "-i", src, "-frames:v", "1"];
    a.extend_from_slice(args);
    a.extend_from_slice(&["-f", "obu", &o]);
    if !ffmpeg(&a) {
        return None;
    }
    std::fs::read(&out).ok().filter(|d| !d.is_empty())
}

/// The planes dav1d decodes (native pixel format, film grain off).
fn dav1d_raw(path: &Path) -> Option<Vec<u8>> {
    let p = path.to_str()?;
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-c:v",
            "libdav1d",
            "-filmgrain",
            "0",
            "-i",
            p,
        ])
        .args(["-frames:v", "1", "-f", "rawvideo", "-"])
        .output()
        .ok()?;
    if out.status.success() && !out.stdout.is_empty() {
        Some(out.stdout)
    } else {
        None
    }
}

#[test]
fn av1_bit_exact_against_dav1d() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("av1-exact");
    let photo = |s: &str, f: &str| {
        format!("testsrc2=s={s},format=yuv444p,gblur=sigma=1.2,noise=alls=12:allf=t,format={f}")
    };
    let synth = |s: &str, f: &str| format!("testsrc2=s={s},format={f}");
    let aom = |extra: &[&'static str]| -> Vec<&'static str> {
        let mut v = vec![
            "-c:v",
            "libaom-av1",
            "-still-picture",
            "1",
            "-cpu-used",
            "6",
        ];
        v.extend_from_slice(extra);
        v
    };
    const NOF: &str = "enable-cdef=0:enable-restoration=0:loopfilter-control=0";
    let nof = NOF;
    let cases: Vec<(&str, String, Vec<&str>)> = vec![
        (
            "lossless",
            synth("96x64", "yuv420p"),
            aom(&["-aom-params", "lossless=1"]),
        ),
        (
            "no-filters",
            photo("200x120", "yuv420p"),
            aom(&["-crf", "30", "-aom-params", nof]),
        ),
        ("default", photo("320x200", "yuv420p"), aom(&["-crf", "30"])),
        (
            "restoration",
            photo("320x200", "yuv420p"),
            aom(&["-crf", "40", "-aom-params", "enable-restoration=1"]),
        ),
        (
            "odd-size",
            photo("198x118", "yuv420p"),
            aom(&["-crf", "25"]),
        ),
        (
            "444-10bit",
            photo("160x96", "yuv444p10le"),
            aom(&["-crf", "35", "-aom-params", "enable-restoration=1"]),
        ),
        ("422", photo("160x96", "yuv422p"), aom(&["-crf", "35"])),
        (
            "422-10bit",
            photo("160x96", "yuv422p10le"),
            aom(&["-crf", "35"]),
        ),
        (
            "420-12bit",
            photo("160x96", "yuv420p12le"),
            aom(&["-crf", "35", "-aom-params", "enable-restoration=1"]),
        ),
        (
            "444-12bit",
            photo("160x96", "yuv444p12le"),
            aom(&["-crf", "35"]),
        ),
        ("gray", photo("160x96", "gray"), aom(&["-crf", "30"])),
        (
            "gray-10bit",
            photo("160x96", "gray10le"),
            aom(&["-crf", "30"]),
        ),
        (
            "screen-palette",
            synth("240x160", "yuv420p"),
            aom(&[
                "-crf",
                "30",
                "-aom-params",
                "tune-content=screen:enable-intrabc=0",
            ]),
        ),
        (
            "screen-intrabc",
            synth("240x160", "yuv420p"),
            aom(&[
                "-crf",
                "30",
                "-aom-params",
                "tune-content=screen:enable-intrabc=1",
            ]),
        ),
        (
            "intrabc-444",
            synth("160x96", "yuv444p"),
            aom(&[
                "-crf",
                "20",
                "-aom-params",
                "tune-content=screen:enable-intrabc=1",
            ]),
        ),
        (
            "intrabc-lossless",
            synth("160x96", "yuv420p"),
            aom(&[
                "-aom-params",
                "lossless=1:tune-content=screen:enable-intrabc=1",
            ]),
        ),
        (
            "sb128-tiles",
            photo("640x360", "yuv420p"),
            aom(&[
                "-crf",
                "40",
                "-tiles",
                "2x2",
                "-aom-params",
                "sb-size=128:enable-restoration=1",
            ]),
        ),
        (
            "qmatrix",
            photo("200x120", "yuv420p"),
            aom(&["-crf", "30", "-aom-params", "enable-qm=1:qm-min=0:qm-max=8"]),
        ),
        (
            "delta-q",
            photo("200x120", "yuv420p"),
            aom(&[
                "-crf",
                "30",
                "-aq-mode",
                "1",
                "-aom-params",
                "deltaq-mode=1",
            ]),
        ),
        (
            "reduced-tx",
            photo("200x120", "yuv420p"),
            aom(&["-crf", "30", "-aom-params", "reduced-tx-type-set=1"]),
        ),
        (
            "tune-ssim",
            photo("200x120", "yuv420p"),
            aom(&["-crf", "30", "-aom-params", "tune=ssim:enable-qm=1"]),
        ),
        ("tiny", synth("8x8", "yuv420p"), aom(&["-crf", "20"])),
        (
            "svt",
            photo("320x200", "yuv420p"),
            vec!["-c:v", "libsvtav1", "-crf", "35", "-preset", "8"],
        ),
        (
            "svt-intrabc",
            synth("320x200", "yuv420p"),
            vec!["-c:v", "libsvtav1", "-crf", "35", "-preset", "8"],
        ),
        (
            "svt-superres-10bit",
            photo("320x200", "yuv420p10le"),
            vec![
                "-c:v",
                "libsvtav1",
                "-crf",
                "30",
                "-preset",
                "8",
                "-svtav1-params",
                "superres-mode=1:superres-denom=12:superres-kf-denom=12",
            ],
        ),
        (
            "rav1e",
            photo("320x200", "yuv420p"),
            vec!["-c:v", "librav1e", "-qp", "100", "-speed", "8"],
        ),
        (
            "rav1e-tiles-444",
            photo("320x200", "yuv444p"),
            vec![
                "-c:v", "librav1e", "-qp", "100", "-speed", "8", "-tiles", "4",
            ],
        ),
    ];
    let mut ran = 0;
    for (name, src, args) in &cases {
        let Some(obu) = encode_obu(&dir, name, src, args) else {
            eprintln!("note: could not encode {name}, skipped");
            continue;
        };
        let Some(raw) = dav1d_raw(&dir.join(format!("{name}.obu"))) else {
            eprintln!("note: no dav1d in ffmpeg, skipped");
            return;
        };
        let p = bl_image::av1::decode_planes(&obu, &[]).unwrap_or_else(|e| panic!("{name}: {e}"));
        let hb = p.bit_depth > 8;
        let mut off = 0;
        for (pi, plane) in p.planes.iter().enumerate() {
            let (sx, sy) = if pi > 0 { (p.ssx, p.ssy) } else { (0, 0) };
            let pw = (p.width + sx) >> sx;
            let ph = (p.height + sy) >> sy;
            for y in 0..ph {
                for x in 0..pw {
                    let i = y * pw + x;
                    let r = if hb {
                        u16::from_le_bytes([raw[off + 2 * i], raw[off + 2 * i + 1]])
                    } else {
                        raw[off + i] as u16
                    };
                    assert_eq!(plane[i], r, "{name}: plane {pi} differs at ({x}, {y})");
                }
            }
            off += pw * ph * if hb { 2 } else { 1 };
        }
        assert_eq!(off, raw.len(), "{name}: frame size");
        ran += 1;
    }
    eprintln!("{ran} of {} AV1 streams bit-exact", cases.len());
}

fn pixel(img: &Image, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * img.width + x) * 4) as usize;
    img.pixels[i..i + 4].try_into().unwrap()
}

fn map(img: &Image, w: u32, h: u32, f: impl Fn(u32, u32) -> (u32, u32)) -> Image {
    let mut out = Image::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (sx, sy) = f(x, y);
            let p = pixel(img, sx, sy);
            let i = ((y * w + x) * 4) as usize;
            out.pixels[i..i + 4].copy_from_slice(&p);
        }
    }
    out
}

/// irot (quarter turns anti-clockwise) followed by imir.
fn transformed(img: &Image, rot: u8, mirror: Option<u8>) -> Image {
    let (w, h) = (img.width, img.height);
    let r = match rot {
        1 => map(img, h, w, |x, y| (w - 1 - y, x)),
        2 => map(img, w, h, |x, y| (w - 1 - x, h - 1 - y)),
        3 => map(img, h, w, |x, y| (y, h - 1 - x)),
        _ => img.clone(),
    };
    let (w, h) = (r.width, r.height);
    match mirror {
        Some(0) => map(&r, w, h, |x, y| (w - 1 - x, y)),
        Some(_) => map(&r, w, h, |x, y| (x, h - 1 - y)),
        None => r,
    }
}

#[test]
fn avif_container_features() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("avif-features");
    let aom = [
        "-c:v",
        "libaom-av1",
        "-still-picture",
        "1",
        "-cpu-used",
        "6",
        "-crf",
        "20",
    ];
    // 4:4:4 so that grids and single images convert identically.
    let Some(color) = encode_obu(&dir, "c", "testsrc2=s=96x64,format=yuv444p", &aom) else {
        eprintln!("note: no libaom, skipped");
        return;
    };
    let color = hf::strip_td(&color);
    let props = |extra: Vec<Vec<u8>>| {
        let mut v = vec![hf::av1c(&color), hf::ispe(96, 64)];
        v.extend(extra);
        v
    };
    let single = |extra: Vec<Vec<u8>>| {
        hf::file(
            b"avif",
            &[hf::Item::new(b"av01", color.clone(), props(extra))],
            1,
            &[],
        )
    };
    let plain = single(vec![]);
    let i = info(&plain).unwrap();
    assert_eq!(
        (i.format, i.width, i.height, i.frames),
        (Format::Avif, 96, 64, 1)
    );
    let base = decode(&plain).unwrap();
    assert_eq!((base.width, base.height), (96, 64));
    // Against ffmpeg's decode of the same stream (4:4:4: only the colour
    // conversion's rounding differs).
    let obu_path = dir.join("c.obu");
    if let Some((w, h, reference)) = ffmpeg_rgba(&obu_path) {
        assert_eq!((w, h), (96, 64));
        let d = diff(&base.pixels, &reference);
        assert!(d.max <= 3, "colour conversion differs by {}", d.max);
    }

    // Rotation and mirroring.
    for rot in 0..4u8 {
        for mirror in [None, Some(0u8), Some(1u8)] {
            let mut extra = Vec::new();
            if rot > 0 {
                extra.push(hf::irot(rot));
            }
            if let Some(m) = mirror {
                extra.push(hf::imir(m));
            }
            let f = single(extra);
            let img = decode(&f).unwrap();
            let want = transformed(&base, rot, mirror);
            assert_eq!(
                (img.width, img.height),
                (want.width, want.height),
                "irot {rot} imir {mirror:?}"
            );
            assert_eq!(img.pixels, want.pixels, "irot {rot} imir {mirror:?}");
            let i = info(&f).unwrap();
            assert_eq!((i.width, i.height), (want.width, want.height));
        }
    }

    // Clean aperture: a centred 64x48 crop, and one shifted by (-8, +4).
    for (off_x2, off_y2) in [(0, 0), (-16, 8)] {
        let f = single(vec![hf::clap(64, 48, off_x2, off_y2)]);
        let img = decode(&f).unwrap();
        assert_eq!((img.width, img.height), (64, 48));
        let (l, t) = ((16 + off_x2 / 2) as u32, (8 + off_y2 / 2) as u32);
        let want = map(&base, 64, 48, |x, y| (x + l, y + t));
        assert_eq!(img.pixels, want.pixels, "clap offset {off_x2} {off_y2}");
    }

    // Alpha: a monochrome stream as auxiliary image.
    let Some(alpha) = encode_obu(&dir, "a", "testsrc2=s=96x64,format=gray", &aom) else {
        return;
    };
    let alpha = hf::strip_td(&alpha);
    let f = hf::file(
        b"avif",
        &[
            hf::Item::new(b"av01", color.clone(), props(vec![])),
            hf::Item::new(
                b"av01",
                alpha.clone(),
                vec![hf::av1c(&alpha), hf::ispe(96, 64), hf::auxc_alpha()],
            ),
        ],
        1,
        &[(b"auxl", 2, vec![1])],
    );
    let img = decode(&f).unwrap();
    let gray = decode(&hf::file(
        b"avif",
        &[hf::Item::new(
            b"av01",
            alpha.clone(),
            vec![hf::av1c(&alpha), hf::ispe(96, 64)],
        )],
        1,
        &[],
    ))
    .unwrap();
    for (p, (q, g)) in img
        .pixels
        .chunks_exact(4)
        .zip(base.pixels.chunks_exact(4).zip(gray.pixels.chunks_exact(4)))
    {
        assert_eq!(&p[..3], &q[..3], "colour of the image with alpha");
        assert!(
            p[3].abs_diff(g[0]) <= 1,
            "alpha {} against grey {}",
            p[3],
            g[0]
        );
    }
    assert!(
        img.pixels.chunks_exact(4).any(|p| p[3] < 128)
            && img.pixels.chunks_exact(4).any(|p| p[3] > 200)
    );

    // Grid of 3 x 2 tiles of 32x32, cropped to 90x60.
    let mut items = Vec::new();
    let mut tiles = Vec::new();
    let mut tile_imgs = Vec::new();
    for r in 0..2 {
        for c in 0..3 {
            let src = format!(
                "testsrc2=s=96x64,format=yuv444p,crop=32:32:{}:{}",
                c * 32,
                r * 32
            );
            let t = hf::strip_td(&encode_obu(&dir, &format!("t{r}{c}"), &src, &aom).unwrap());
            tile_imgs.push(
                decode(&hf::file(
                    b"avif",
                    &[hf::Item::new(
                        b"av01",
                        t.clone(),
                        vec![hf::av1c(&t), hf::ispe(32, 32)],
                    )],
                    1,
                    &[],
                ))
                .unwrap(),
            );
            let mut it = hf::Item::new(b"av01", t.clone(), vec![hf::av1c(&t), hf::ispe(32, 32)]);
            it.hidden = true;
            items.push(it);
            tiles.push((items.len()) as u32);
        }
    }
    items.push(hf::Item::new(
        b"grid",
        hf::grid(2, 3, 90, 60),
        vec![hf::ispe(90, 60)],
    ));
    let grid_id = items.len() as u32;
    let f = hf::file(b"avif", &items, grid_id, &[(b"dimg", grid_id, tiles)]);
    let img = decode(&f).unwrap();
    assert_eq!((img.width, img.height), (90, 60));
    for y in 0..60 {
        for x in 0..90 {
            let t = &tile_imgs[(y / 32 * 3 + x / 32) as usize];
            assert_eq!(
                pixel(&img, x, y),
                pixel(t, x % 32, y % 32),
                "grid pixel ({x}, {y})"
            );
        }
    }
    // Scaled while decoded.
    let small = decode_fit(&f, &Fit::within(45)).unwrap();
    assert_eq!((small.width, small.height), (45, 30));

    // An image sequence from ffmpeg: the first frame.
    let seq = dir.join("seq.avif");
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=64x48:r=5,format=yuv444p",
        "-frames:v",
        "3",
        "-c:v",
        "libaom-av1",
        "-cpu-used",
        "8",
        "-crf",
        "20",
        seq.to_str().unwrap(),
    ]) {
        let data = std::fs::read(&seq).unwrap();
        let i = info(&data).unwrap();
        assert_eq!(
            (i.format, i.width, i.height, i.frames),
            (Format::Avif, 64, 48, 1)
        );
        let img = decode(&data).unwrap();
        if let Some((w, h, reference)) = ffmpeg_rgba(&seq) {
            assert_eq!((w, h), (64, 48));
            assert!(diff(&img.pixels, &reference).max <= 3);
        }
    }

    // HDR (PQ, BT.2020): tone mapped, not clipped to white.
    let f = single(vec![hf::nclx(9, 16, 9, false)]);
    let img = decode(&f).unwrap();
    let mean: f64 = img.pixels.chunks_exact(4).map(|p| p[1] as f64).sum::<f64>() / (96.0 * 64.0);
    assert!(mean > 20.0 && mean < 235.0, "HDR mean {mean}");
}

/// A stand-in HEVC decoder: the coded data of every item is one byte, the
/// image is filled with a colour made from it.
fn fake_system_decoder(file: &[u8], need_w: u32, need_h: u32) -> Option<Image> {
    assert!(&file[4..12] == b"ftypheic", "a HEIC file");
    let find = |kind: &[u8]| file.windows(4).position(|w| w == kind);
    find(b"hvcC")?;
    let ispe = find(b"ispe")?;
    let rd = |o: usize| u32::from_be_bytes(file[o..o + 4].try_into().unwrap());
    let (w, h) = (rd(ispe + 8), rd(ispe + 12));
    assert!(need_w <= w && need_h <= h);
    let mdat = find(b"mdat")?;
    let v = file[mdat + 4];
    let mut img = Image::new(w, h);
    for p in img.pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[v, 255 - v, v / 2, 255]);
    }
    Some(img)
}

#[test]
fn heic_through_a_system_decoder() {
    let hevc = |id: u8, w: u32, h: u32| {
        hf::Item::new(b"hvc1", vec![id], vec![hf::hvcc_stub(), hf::ispe(w, h)])
    };
    let single = hf::file(b"heic", &[hevc(40, 64, 48)], 1, &[]);
    let i = info(&single).unwrap();
    assert_eq!((i.format, i.width, i.height), (Format::Heic, 64, 48));
    bl_image::set_system_decoder(None);
    assert!(decode(&single).is_err(), "no decoder registered");
    bl_image::set_system_decoder(Some(Box::new(fake_system_decoder)));
    assert!(bl_image::has_system_decoder());
    let img = decode(&single).unwrap();
    assert_eq!((img.width, img.height), (64, 48));
    assert_eq!(pixel(&img, 10, 10), [40, 215, 20, 255]);

    // Grid of 3 x 2 tiles of 32x32 cropped to 90x60, rotated, with an alpha
    // grid.
    let mut items = Vec::new();
    let mut tiles = Vec::new();
    for k in 0..6u8 {
        let mut it = hevc(10 + 30 * k, 32, 32);
        it.hidden = true;
        items.push(it);
        tiles.push(items.len() as u32);
    }
    let mut alpha_tiles = Vec::new();
    for _ in 0..6 {
        let mut it = hevc(200, 32, 32);
        it.hidden = true;
        items.push(it);
        alpha_tiles.push(items.len() as u32);
    }
    items.push(hf::Item::new(
        b"grid",
        hf::grid(2, 3, 90, 60),
        vec![hf::ispe(90, 60), hf::irot(1)],
    ));
    let grid = items.len() as u32;
    items.push(hf::Item::new(
        b"grid",
        hf::grid(2, 3, 90, 60),
        vec![hf::ispe(90, 60), hf::auxc_alpha()],
    ));
    let agrid = items.len() as u32;
    let f = hf::file(
        b"heic",
        &items,
        grid,
        &[
            (b"dimg", grid, tiles),
            (b"dimg", agrid, alpha_tiles),
            (b"auxl", agrid, vec![grid]),
        ],
    );
    let i = info(&f).unwrap();
    assert_eq!((i.format, i.width, i.height), (Format::Heic, 60, 90));
    let img = decode(&f).unwrap();
    assert_eq!((img.width, img.height), (60, 90));
    // Rotated anti-clockwise: the top right tile (index 2) is now top left.
    for (x, y, tile) in [
        (5u32, 5u32, 2u8),
        (40, 5, 5),
        (5, 80, 0),
        (40, 80, 3),
        (20, 45, 1),
    ] {
        let v = 10 + 30 * tile;
        assert_eq!(
            pixel(&img, x, y),
            [v, 255 - v, v / 2, 200],
            "rotated pixel ({x}, {y})"
        );
    }
    let small = decode_fit(&f, &Fit::within(30)).unwrap();
    assert_eq!((small.width, small.height), (20, 30));
    bl_image::set_system_decoder(None);
}
