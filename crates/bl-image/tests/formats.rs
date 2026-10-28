//! Decoder / encoder conformance against ffmpeg and ImageMagick.

mod common;

use bl_image::{decode, decode_max, encode_jpeg, encode_png, probe_format, Format, Image};
use common::*;
use std::path::Path;

/// Sharp synthetic test pattern (odd dimensions, true RGB).
const SRC: &str = "testsrc2=s=317x211,format=rgb24";
/// Blurred variant, closer to photographic content.
const SMOOTH: &str = "testsrc2=s=317x211,format=rgb24,gblur=sigma=1.5";

fn load(path: &Path) -> Vec<u8> {
    std::fs::read(path).expect("read fixture")
}

/// Decodes with bl-image and compares exactly against ffmpeg's RGBA output.
fn check_exact(path: &Path, tolerance: u8) {
    let data = load(path);
    let img = decode(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let (w, h, reference) = ffmpeg_rgba(path).expect("ffmpeg reference decode");
    assert_eq!((img.width, img.height), (w, h), "{}", path.display());
    let d = diff(&img.pixels, &reference);
    println!(
        "{}: mean {:.4} max {}",
        path.file_name().unwrap().to_string_lossy(),
        d.mean,
        d.max
    );
    assert!(
        d.max <= tolerance,
        "{}: max error {} (mean {:.4})",
        path.display(),
        d.max,
        d.mean
    );
}

#[test]
fn png_color_types_and_depths() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("png");
    for fmt in [
        "rgba", "rgb24", "gray", "pal8", "rgb48be", "gray16be", "monob", "ya8", "rgba64be",
        "ya16be",
    ] {
        let p = dir.join(format!("{fmt}.png"));
        if !ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            SRC,
            "-frames:v",
            "1",
            "-pix_fmt",
            fmt,
            p.to_str().unwrap(),
        ]) {
            continue;
        }
        // ffmpeg's 16 -> 8 bit conversion rounds differently (ImageMagick's
        // matches exactly, see png_imagemagick_variants).
        let tol = if fmt.contains("16") || fmt.contains("48") || fmt.contains("64") {
            2
        } else {
            0
        };
        check_exact(&p, tol);
    }
    // Alpha gradient (exercises tRNS-free RGBA and premultiplication-free path).
    let p = dir.join("alpha.png");
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='mod(X*3+Y,256)'"),
        "-frames:v",
        "1",
        p.to_str().unwrap(),
    ]) {
        check_exact(&p, 0);
    }
}

#[test]
fn png_imagemagick_variants() {
    if !have_ffmpeg() || !have_convert() {
        eprintln!("note: ImageMagick missing, skipping");
        return;
    }
    let dir = fixture_dir("png_im");
    let src = dir.join("src.png");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &format!("{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(gt(X,150),255,if(gt(X,40),128,0))'"),
        "-frames:v",
        "1",
        src.to_str().unwrap(),
    ]));
    let s = src.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "interlaced_rgba.png",
            vec![s, "-interlace", "PNG", "PNG32:"],
        ),
        (
            "interlaced_rgb.png",
            vec![s, "-alpha", "off", "-interlace", "PNG", "PNG24:"],
        ),
        (
            "interlaced_rgb16.png",
            vec![s, "-alpha", "off", "-interlace", "PNG", "PNG48:"],
        ),
        ("rgba16.png", vec![s, "PNG64:"]),
        (
            "gray4.png",
            vec![
                s,
                "-alpha",
                "off",
                "-colorspace",
                "gray",
                "-depth",
                "4",
                "PNG:",
            ],
        ),
        (
            "gray2.png",
            vec![
                s,
                "-alpha",
                "off",
                "-colorspace",
                "gray",
                "-depth",
                "2",
                "PNG:",
            ],
        ),
        (
            "gray1_interlaced.png",
            vec![
                s,
                "-alpha",
                "off",
                "-colorspace",
                "gray",
                "-depth",
                "1",
                "-interlace",
                "PNG",
                "PNG:",
            ],
        ),
        ("pal_trns.png", vec![s, "-colors", "64", "PNG8:"]),
        (
            "pal_4bit.png",
            vec![s, "-alpha", "off", "-colors", "12", "PNG8:"],
        ),
        (
            "pal_interlaced.png",
            vec![s, "-colors", "200", "-interlace", "PNG", "PNG8:"],
        ),
    ];
    for (name, mut args) in cases {
        let out = dir.join(name);
        let last = args.pop().unwrap();
        let target = format!("{last}{}", out.to_str().unwrap());
        args.push(&target);
        if !convert(&args) {
            continue;
        }
        let data = load(&out);
        let img = decode(&data).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (w, h, reference) = convert_rgba(&out, &[]).expect("convert reference");
        assert_eq!((img.width, img.height), (w, h));
        let d = diff(&img.pixels, &reference);
        println!("{name}: mean {:.4} max {}", d.mean, d.max);
        // ImageMagick may rescale 16-bit samples with different rounding.
        assert!(d.max <= 1, "{name}: max error {}", d.max);
    }
}

#[test]
fn apng_first_frame() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("apng");
    let p = dir.join("anim.png");
    if !ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=64x48:r=10",
        "-frames:v",
        "3",
        "-plays",
        "0",
        "-f",
        "apng",
        p.to_str().unwrap(),
    ]) {
        return;
    }
    check_exact(&p, 0);
}

#[test]
fn png_encode_round_trip() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("png_enc");
    for (name, filter) in [
        ("opaque", SRC.to_string()),
        (
            "alpha",
            format!("{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='X'"),
        ),
    ] {
        let src = dir.join(format!("{name}_src.png"));
        assert!(ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &filter,
            "-frames:v",
            "1",
            "-pix_fmt",
            "rgba",
            src.to_str().unwrap()
        ]));
        let img = decode(&load(&src)).unwrap();
        let png = encode_png(&img);
        let back = decode(&png).unwrap();
        assert_eq!(back.pixels, img.pixels);
        let out = dir.join(format!("{name}_ours.png"));
        std::fs::write(&out, &png).unwrap();
        let (_, _, reference) = ffmpeg_rgba(&out).expect("ffmpeg decodes our PNG");
        assert_eq!(reference, img.pixels);
        println!(
            "{name}: raw {} bytes, ffmpeg png {} bytes, ours {} bytes",
            img.pixels.len(),
            load(&src).len(),
            png.len()
        );
    }
}

/// JPEG decode accuracy: compares against libjpeg-turbo (ImageMagick) when
/// available, otherwise against ffmpeg.
fn check_jpeg(path: &Path, extra: &[&str], max_mean: f64, max_err: u8) {
    let data = load(path);
    assert_eq!(probe_format(&data), Some(Format::Jpeg));
    let img = decode(&data).unwrap_or_else(|e| panic!("{}: {e}", path.display()));
    let name = path.file_name().unwrap().to_string_lossy().to_string();
    if have_convert() {
        let (w, h, reference) = convert_rgba(path, extra).expect("convert reference");
        assert_eq!((img.width, img.height), (w, h), "{name}");
        let d = diff(&img.pixels, &reference);
        println!(
            "{name}: vs libjpeg-turbo mean {:.4} max {} psnr {:.1} dB",
            d.mean, d.max, d.psnr
        );
        assert!(
            d.mean < max_mean && d.max <= max_err,
            "{name}: mean {:.3} max {}",
            d.mean,
            d.max
        );
    }
    if extra.is_empty() {
        if let Some((w, h, reference)) = ffmpeg_rgba(path) {
            assert_eq!((img.width, img.height), (w, h), "{name}");
            let d = diff(&img.pixels, &reference);
            println!(
                "{name}: vs ffmpeg mean {:.4} max {} psnr {:.1} dB",
                d.mean, d.max, d.psnr
            );
            // ffmpeg's swscale upsamples chroma differently (it differs from
            // libjpeg-turbo by ~1.7 on the sharp test pattern itself).
            assert!(d.mean < 2.5, "{name}: mean {:.3} vs ffmpeg", d.mean);
        }
    }
}

#[test]
fn jpeg_ffmpeg_variants() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("jpeg");
    // (ffmpeg's mjpeg encoder writes gray as 4:4:4 YCbCr; true grayscale
    // JPEGs are covered by the ImageMagick fixtures.)
    for fmt in ["yuvj420p", "yuvj422p", "yuvj444p"] {
        for q in [2, 10, 31] {
            let p = dir.join(format!("{fmt}_q{q}.jpg"));
            if !ffmpeg(&[
                "-f",
                "lavfi",
                "-i",
                SRC,
                "-frames:v",
                "1",
                "-pix_fmt",
                fmt,
                "-q:v",
                &q.to_string(),
                p.to_str().unwrap(),
            ]) {
                continue;
            }
            check_jpeg(&p, &[], 0.25, 6);
        }
    }
}

#[test]
fn jpeg_imagemagick_variants() {
    if !have_ffmpeg() || !have_convert() {
        eprintln!("note: ImageMagick missing, skipping progressive/CMYK/EXIF JPEG tests");
        return;
    }
    let dir = fixture_dir("jpeg_im");
    let src = dir.join("src.png");
    // Smooth content: libjpeg-turbo replicates (instead of interpolating)
    // chroma for 4:1:1, which only matters at sharp chroma edges.
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        SMOOTH,
        "-frames:v",
        "1",
        src.to_str().unwrap()
    ]));
    let s = src.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "baseline_gray.jpg",
            vec![s, "-colorspace", "gray", "-quality", "90"],
        ),
        (
            "progressive_420.jpg",
            vec![
                s,
                "-quality",
                "85",
                "-interlace",
                "JPEG",
                "-sampling-factor",
                "2x2",
            ],
        ),
        (
            "progressive_444.jpg",
            vec![
                s,
                "-quality",
                "95",
                "-interlace",
                "JPEG",
                "-sampling-factor",
                "1x1",
            ],
        ),
        (
            "progressive_gray.jpg",
            vec![
                s,
                "-colorspace",
                "gray",
                "-quality",
                "80",
                "-interlace",
                "JPEG",
            ],
        ),
        (
            "baseline_411.jpg",
            vec![s, "-quality", "90", "-sampling-factor", "4x1"],
        ),
        (
            "baseline_440.jpg",
            vec![s, "-quality", "90", "-sampling-factor", "1x2"],
        ),
        (
            "progressive_411.jpg",
            vec![
                s,
                "-quality",
                "90",
                "-interlace",
                "JPEG",
                "-sampling-factor",
                "4x1",
            ],
        ),
        ("cmyk.jpg", vec![s, "-colorspace", "CMYK", "-quality", "92"]),
        (
            "cmyk_progressive.jpg",
            vec![
                s,
                "-colorspace",
                "CMYK",
                "-quality",
                "92",
                "-interlace",
                "JPEG",
            ],
        ),
        ("q100.jpg", vec![s, "-quality", "100"]),
        ("q5.jpg", vec![s, "-quality", "5"]),
    ];
    for (name, mut args) in cases {
        let out = dir.join(name);
        let target = out.to_str().unwrap().to_string();
        args.push(&target);
        if !convert(&args) {
            continue;
        }
        let extra: &[&str] = if name.starts_with("cmyk") {
            &["-colorspace", "sRGB"]
        } else {
            &[]
        };
        if name.contains("411") {
            // libjpeg-turbo replicates chroma for 4:1:1 (no fancy upsampling)
            // while we interpolate, so compare both against the source.
            let ours = decode(&load(&out)).unwrap();
            let (_, _, lj) = convert_rgba(&out, &[]).unwrap();
            let orig = decode(&load(&src)).unwrap();
            let (d_ours, d_lj) = (diff(&ours.pixels, &orig.pixels), diff(&lj, &orig.pixels));
            println!(
                "{name}: psnr vs source: ours {:.2} dB, libjpeg-turbo {:.2} dB",
                d_ours.psnr, d_lj.psnr
            );
            assert!(d_ours.psnr > d_lj.psnr - 0.3, "{name}");
            continue;
        }
        check_jpeg(&out, extra, 0.25, 8);
    }
}

#[test]
fn jpeg_exif_orientation() {
    if !have_ffmpeg() || !have_convert() {
        return;
    }
    let dir = fixture_dir("jpeg_exif");
    let src = dir.join("src.png");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=96x64",
        "-frames:v",
        "1",
        src.to_str().unwrap()
    ]));
    let orients = [
        "top-left",
        "top-right",
        "bottom-right",
        "bottom-left",
        "left-top",
        "right-top",
        "right-bottom",
        "left-bottom",
    ];
    for (i, o) in orients.iter().enumerate() {
        let out = dir.join(format!("orient{}.jpg", i + 1));
        if !convert(&[
            src.to_str().unwrap(),
            "-quality",
            "95",
            "-orient",
            o,
            out.to_str().unwrap(),
        ]) {
            continue;
        }
        let img = decode(&load(&out)).unwrap();
        let (w, h, reference) = convert_rgba(&out, &["-auto-orient"]).unwrap();
        assert_eq!((img.width, img.height), (w, h), "orientation {}", i + 1);
        let d = diff(&img.pixels, &reference);
        println!(
            "orientation {}: {}x{} mean {:.3} max {}",
            i + 1,
            w,
            h,
            d.mean,
            d.max
        );
        assert!(d.mean < 1.0, "orientation {}", i + 1);
    }
}

#[test]
fn jpeg_decode_max_scaling() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("jpeg_scale");
    let mut worst: f64 = f64::INFINITY;
    for fmt in ["yuvj420p", "yuvj444p", "yuvj422p"] {
        let p = dir.join(format!("big_{fmt}.jpg"));
        let src = "testsrc2=s=1203x799,format=rgb24,gblur=sigma=1.5";
        if !ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            src,
            "-frames:v",
            "1",
            "-pix_fmt",
            fmt,
            "-q:v",
            "3",
            p.to_str().unwrap(),
        ]) {
            continue;
        }
        let data = load(&p);
        let full = decode(&data).unwrap();
        for max_dim in [600, 300, 150, 140, 64] {
            let fast = decode_max(&data, max_dim).unwrap();
            assert!(fast.width.max(fast.height) <= max_dim);
            let slow = bl_image::resize(&full, fast.width, fast.height);
            let d = diff(&fast.pixels, &slow.pixels);
            println!(
                "{fmt} max_dim {max_dim}: {}x{} vs full decode+resize: mean {:.3} psnr {:.1} dB",
                fast.width, fast.height, d.mean, d.psnr
            );
            worst = worst.min(d.psnr);
        }
    }
    assert!(worst > 32.0, "worst psnr {worst:.1}");
}

#[test]
fn jpeg_encode_quality() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("jpeg_enc");
    let src = dir.join("src.png");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        SMOOTH,
        "-frames:v",
        "1",
        src.to_str().unwrap()
    ]));
    let img = decode(&load(&src)).unwrap();
    for q in [50u8, 75, 90, 100] {
        let jpg = encode_jpeg(&img, q);
        let out = dir.join(format!("ours_q{q}.jpg"));
        std::fs::write(&out, &jpg).unwrap();
        let (w, h, reference) = ffmpeg_rgba(&out).expect("ffmpeg decodes our JPEG");
        assert_eq!((w, h), (img.width, img.height));
        let d_ff = diff(&reference, &img.pixels);
        let ours = decode(&jpg).unwrap();
        let d_ours = diff(&ours.pixels, &img.pixels);
        println!(
            "q{q}: {} bytes, psnr (ffmpeg decode) {:.2} dB, psnr (our decode) {:.2} dB",
            jpg.len(),
            d_ff.psnr,
            d_ours.psnr
        );
        if q >= 90 {
            assert!(d_ff.psnr > 30.0 && d_ours.psnr > 30.0);
        }
    }
    // Odd sizes and tiny images.
    for (w, h) in [(1, 1), (7, 3), (17, 33)] {
        let mut im = Image::new(w, h);
        for (i, p) in im.pixels.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(i * 40) as u8, 128, (i * 10) as u8, 255]);
        }
        let jpg = encode_jpeg(&im, 90);
        let out = dir.join(format!("tiny_{w}x{h}.jpg"));
        std::fs::write(&out, &jpg).unwrap();
        assert!(ffmpeg_rgba(&out).is_some());
        let back = decode(&jpg).unwrap();
        assert_eq!((back.width, back.height), (w, h));
    }
}

#[test]
fn bmp_variants() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("bmp");
    for fmt in [
        "bgr24",
        "bgra",
        "rgb565le",
        "rgb555le",
        "rgb444le",
        "pal8",
        "gray",
        "monob",
        "rgb8",
        "rgb4_byte",
    ] {
        let p = dir.join(format!("{fmt}.bmp"));
        if !ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            SRC,
            "-frames:v",
            "1",
            "-pix_fmt",
            fmt,
            p.to_str().unwrap(),
        ]) {
            continue;
        }
        // ffmpeg expands 4-bit channels as v << 4; we replicate bits
        // (v * 17) so that 0xF maps to 255.
        check_exact(&p, if fmt == "rgb444le" { 15 } else { 0 });
    }
    if have_convert() {
        let src = dir.join("src.png");
        assert!(ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            SRC,
            "-frames:v",
            "1",
            src.to_str().unwrap()
        ]));
        let s = src.to_str().unwrap();
        let cases: Vec<(&str, Vec<&str>)> = vec![
            (
                "rle8.bmp",
                vec![s, "-colors", "200", "-compress", "RLE", "BMP3:"],
            ),
            ("v3_24.bmp", vec![s, "BMP3:"]),
            ("v2_os2.bmp", vec![s, "BMP2:"]),
            ("pal4.bmp", vec![s, "-colors", "16", "BMP3:"]),
            (
                "v5_32.bmp",
                vec![
                    s,
                    "-alpha",
                    "set",
                    "-channel",
                    "A",
                    "-evaluate",
                    "set",
                    "60%",
                    "+channel",
                    "BMP:",
                ],
            ),
        ];
        for (name, mut args) in cases {
            let out = dir.join(name);
            let last = args.pop().unwrap();
            let target = format!("{last}{}", out.to_str().unwrap());
            args.push(&target);
            if !convert(&args) {
                continue;
            }
            let img = decode(&load(&out)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let (w, h, reference) = convert_rgba(&out, &[]).unwrap();
            assert_eq!((img.width, img.height), (w, h));
            let d = diff(&img.pixels, &reference);
            println!("{name}: mean {:.4} max {}", d.mean, d.max);
            assert!(d.max <= 1, "{name}: max {}", d.max);
        }
    }
}

#[test]
fn gif_variants() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("gif");
    let p = dir.join("ffmpeg.gif");
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        SRC,
        "-frames:v",
        "1",
        p.to_str().unwrap(),
    ]) {
        check_exact(&p, 0);
    }
    let p = dir.join("anim.gif");
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=80x60:r=5",
        "-frames:v",
        "4",
        p.to_str().unwrap(),
    ]) {
        check_exact(&p, 0);
    }
    if have_convert() {
        let src = dir.join("src.png");
        assert!(ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &format!(
                "{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(gt(X,100),255,0)'"
            ),
            "-frames:v",
            "1",
            src.to_str().unwrap(),
        ]));
        let s = src.to_str().unwrap();
        for (name, args) in [
            (
                "interlaced.gif",
                vec![s, "-alpha", "off", "-interlace", "GIF"],
            ),
            ("transparent.gif", vec![s, "-colors", "100"]),
            (
                "small_palette.gif",
                vec![s, "-alpha", "off", "-colors", "4"],
            ),
        ] {
            let out = dir.join(name);
            let mut args = args.clone();
            let t = out.to_str().unwrap().to_string();
            args.push(&t);
            if !convert(&args) {
                continue;
            }
            let img = decode(&load(&out)).unwrap_or_else(|e| panic!("{name}: {e}"));
            let (w, h, mut reference) = convert_rgba(&out, &[]).unwrap();
            assert_eq!((img.width, img.height), (w, h));
            // ImageMagick keeps the palette color under alpha 0; normalize.
            for px in reference.chunks_exact_mut(4) {
                if px[3] == 0 {
                    px.copy_from_slice(&[0, 0, 0, 0]);
                }
            }
            let d = diff(&img.pixels, &reference);
            println!("{name}: mean {:.4} max {}", d.mean, d.max);
            assert_eq!(d.max, 0, "{name}");
        }
    }
}

#[test]
fn webp_variants() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("webp");
    let lossless = [
        ("lossless_rgb.webp", SRC.to_string(), "rgb24"),
        (
            "lossless_rgba.webp",
            format!("{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='mod(X+Y,256)'"),
            "rgba",
        ),
        (
            "lossless_small_palette.webp",
            "testsrc2=s=64x64,format=gray,lut=y='val/64*64'".to_string(),
            "rgb24",
        ),
    ];
    for (name, filter, pix) in lossless {
        let p = dir.join(name);
        if !ffmpeg(&[
            "-f",
            "lavfi",
            "-i",
            &filter,
            "-frames:v",
            "1",
            "-pix_fmt",
            if pix == "rgba" { "bgra" } else { "yuv420p" },
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            p.to_str().unwrap(),
        ]) {
            eprintln!("note: libwebp encoder unavailable");
            return;
        }
        let img = decode(&load(&p)).unwrap_or_else(|e| panic!("{name}: {e}"));
        // ffmpeg's native webp decoder is the reference.
        let (w, h, reference) = ffmpeg_rgba(&p).unwrap();
        assert_eq!((img.width, img.height), (w, h));
        let d = diff(&img.pixels, &reference);
        println!("{name}: mean {:.4} max {}", d.mean, d.max);
        assert_eq!(d.max, 0, "{name}");
    }
    for (name, filter, extra) in [
        ("lossy_q75.webp", SRC.to_string(), vec!["-quality", "75"]),
        ("lossy_q95.webp", SRC.to_string(), vec!["-quality", "95"]),
        ("lossy_q20.webp", SRC.to_string(), vec!["-quality", "20"]),
        (
            "lossy_alpha.webp",
            format!("{SRC},format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='mod(X*2,256)'"),
            vec!["-quality", "80"],
        ),
    ] {
        let p = dir.join(name);
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            filter.as_str(),
            "-frames:v",
            "1",
            "-c:v",
            "libwebp",
        ];
        if name.contains("alpha") {
            args.extend(["-pix_fmt", "yuva420p"]);
        }
        args.extend(extra.iter());
        let t = p.to_str().unwrap().to_string();
        args.push(&t);
        if !ffmpeg(&args) {
            continue;
        }
        let img = match decode(&load(&p)) {
            Ok(i) => i,
            Err(e) => panic!("{name}: {e}"),
        };
        // ImageMagick uses libwebp (bit-exact reference); ffmpeg's native
        // decoder upsamples chroma differently, so allow more deviation.
        let (reference, limit) = if have_convert() {
            (convert_rgba(&p, &[]), 1.5)
        } else {
            (ffmpeg_rgba(&p), 2.5)
        };
        let (w, h, reference) = reference.unwrap();
        assert_eq!((img.width, img.height), (w, h));
        let d = diff(&img.pixels, &reference);
        println!(
            "{name}: mean {:.4} max {} psnr {:.1} dB",
            d.mean, d.max, d.psnr
        );
        assert!(d.mean < limit, "{name}: mean {:.3}", d.mean);
    }
}

#[test]
fn webp_imagemagick_variants() {
    if !have_ffmpeg() || !have_convert() {
        return;
    }
    let dir = fixture_dir("webp_im");
    let src = dir.join("src.png");
    assert!(ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=403x277,format=rgba,geq=r='r(X,Y)':g='g(X,Y)':b='b(X,Y)':a='if(gt(X,200),255,X)'",
        "-frames:v",
        "1",
        src.to_str().unwrap(),
    ]));
    let s = src.to_str().unwrap();
    let cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "simple_filter.webp",
            vec![
                "-alpha",
                "off",
                "-quality",
                "60",
                "-define",
                "webp:filter-type=0",
                "-define",
                "webp:filter-strength=60",
            ],
        ),
        (
            "sharp_partitions.webp",
            vec![
                "-alpha",
                "off",
                "-quality",
                "70",
                "-define",
                "webp:filter-sharpness=6",
                "-define",
                "webp:partitions=3",
            ],
        ),
        (
            "segments1.webp",
            vec![
                "-alpha",
                "off",
                "-quality",
                "50",
                "-define",
                "webp:segments=1",
                "-define",
                "webp:sns-strength=0",
            ],
        ),
        (
            "strong_filter.webp",
            vec![
                "-alpha",
                "off",
                "-quality",
                "10",
                "-define",
                "webp:filter-strength=100",
            ],
        ),
        (
            "alpha_raw.webp",
            vec!["-quality", "80", "-define", "webp:alpha-compression=0"],
        ),
        (
            "alpha_filtered.webp",
            vec!["-quality", "80", "-define", "webp:alpha-filter=best"],
        ),
        (
            "alpha_lossy_quality.webp",
            vec!["-quality", "80", "-define", "webp:alpha-quality=50"],
        ),
        (
            "lossless_m0.webp",
            vec!["-define", "webp:lossless=true", "-define", "webp:method=0"],
        ),
        (
            "lossless_m6.webp",
            vec!["-define", "webp:lossless=true", "-define", "webp:method=6"],
        ),
        (
            "lossless_exact.webp",
            vec![
                "-define",
                "webp:lossless=true",
                "-define",
                "webp:exact=true",
            ],
        ),
    ];
    for (name, opts) in cases {
        let out = dir.join(name);
        let mut args = vec![s];
        args.extend(opts.iter());
        let t = out.to_str().unwrap().to_string();
        args.push(&t);
        if !convert(&args) {
            continue;
        }
        let img = decode(&load(&out)).unwrap_or_else(|e| panic!("{name}: {e}"));
        let (w, h, reference) = convert_rgba(&out, &[]).unwrap();
        assert_eq!((img.width, img.height), (w, h), "{name}");
        let d = diff(&img.pixels, &reference);
        println!("{name}: mean {:.4} max {}", d.mean, d.max);
        if name.starts_with("lossless") {
            assert_eq!(d.max, 0, "{name}");
        } else {
            assert!(
                d.mean < 0.05 && d.max <= 2,
                "{name}: mean {:.4} max {}",
                d.mean,
                d.max
            );
        }
    }
    // Animated WebP: first frame.
    let anim = dir.join("anim.webp");
    if ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        "testsrc2=s=96x64:r=5",
        "-frames:v",
        "3",
        "-c:v",
        "libwebp_anim",
        "-lossless",
        "1",
        anim.to_str().unwrap(),
    ]) {
        let img = decode(&load(&anim)).unwrap();
        let (w, h, reference) = convert_rgba(&anim, &[]).unwrap();
        assert_eq!((img.width, img.height), (w, h));
        let d = diff(&img.pixels, &reference);
        println!("anim.webp: mean {:.4} max {}", d.mean, d.max);
        assert_eq!(d.max, 0);
    }
}
