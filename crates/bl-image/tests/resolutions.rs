//! Background images in every common resolution (720p to 16K) and format,
//! still and animated: each is decoded to a screen-sized target and
//! compared with ffmpeg's area-averaged downscale, and the memory the
//! decoder allocates on top of the file is measured with a counting
//! allocator (the image must never exist at full size).
//!
//! AVIF comes from ffmpeg's libaom (a single frame at every size; 10-bit
//! 4:4:4 up to 8K, as libaom needs several GB at 16K) and as a 16K grid of
//! 512x512 tiles built here.
//!
//! Fixtures are generated with ffmpeg (progressive JPEG with jpegtran or
//! Pillow, interlaced / tiled / planar variants with ImageMagick up to 8K,
//! as it keeps several full-size copies); missing tools skip the variants
//! with a note. They are written
//! to `BL_IMAGE_FIXTURES` (default: the temp directory) and removed after
//! use; the 16K BMP alone is about 400 MB.
//!
//! `BL_IMAGE_MAX_RESOLUTION=8K` (any name of the list) stops after that
//! size, for machines where the encoders may not use the several GB they
//! need at 16K.
//!
//! This file holds a single test so that no other test allocates while the
//! peak is measured.

mod common;

use bl_image::{decode_fit_owned, info, Animation, Fit, Format};
use common::*;
use std::alloc::{GlobalAlloc, Layout, System};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

struct Counting;

static CUR: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grow(n: usize) {
    let c = CUR.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(c, Ordering::Relaxed);
}

// SAFETY: forwards to the system allocator and only counts sizes.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = System.alloc(l);
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = System.alloc_zeroed(l);
        if !p.is_null() {
            grow(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        System.dealloc(p, l);
        CUR.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = System.realloc(p, l, new);
        if !q.is_null() {
            if new >= l.size() {
                grow(new - l.size());
            } else {
                CUR.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Runs `f` and returns its result and the peak of additional memory.
fn measured<T>(f: impl FnOnce() -> T) -> (T, usize) {
    let base = CUR.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let r = f();
    (r, PEAK.load(Ordering::Relaxed).saturating_sub(base))
}

const RESOLUTIONS: [(&str, u32, u32); 6] = [
    ("720p", 1280, 720),
    ("1080p", 1920, 1080),
    ("1440p", 2560, 1440),
    ("4K", 3840, 2160),
    ("8K", 7680, 4320),
    ("16K", 15360, 8640),
];

/// The screen the images are fitted to (cover, as for backgrounds).
const SCREEN: (u32, u32) = (1920, 1080);

fn fixtures_dir() -> PathBuf {
    let base = std::env::var_os("BL_IMAGE_FIXTURES")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("bl-image-resolutions-{}", std::process::id()));
    std::fs::create_dir_all(&d).expect("fixture dir");
    d
}

/// Photo-like source: test pattern, smoothed, with grain (so that the
/// files compress like real pictures and not like flat areas).
fn source(w: u32, h: u32, alpha: bool, rate: u32) -> String {
    format!(
        "testsrc2=s={w}x{h}:r={rate},format=yuv444p,gblur=sigma=1.2,noise=alls=12:allf=t,format={}",
        if alpha { "rgba" } else { "rgb24" }
    )
}

fn gen(out: &Path, w: u32, h: u32, frames: u32, alpha: bool, extra: &[&str]) -> bool {
    let src = source(w, h, alpha, 10);
    let o = out.to_str().unwrap().to_string();
    let n = frames.to_string();
    let mut args: Vec<&str> = vec!["-f", "lavfi", "-i", &src, "-frames:v", &n];
    args.extend_from_slice(extra);
    args.push(&o);
    ffmpeg(&args)
}

fn progressive(baseline: &Path, out: &Path) -> bool {
    let (b, o) = (baseline.to_str().unwrap(), out.to_str().unwrap());
    if Command::new("jpegtran")
        .args(["-progressive", "-copy", "none", "-outfile", o, b])
        .status()
        .is_ok_and(|s| s.success())
    {
        return true;
    }
    let script = format!(
        "from PIL import Image\nImage.MAX_IMAGE_PIXELS=None\nImage.open({b:?}).save({o:?}, quality=90, progressive=True)"
    );
    Command::new("python3")
        .args(["-c", &script])
        .status()
        .is_ok_and(|s| s.success())
}

/// ffmpeg's area-averaged downscale of the first frame (the reference).
fn reference(path: &Path, w: u32, h: u32) -> Option<Vec<u8>> {
    let vf = format!("scale={w}:{h}:flags=area,format=rgba");
    let out = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-i",
            path.to_str()?,
            "-frames:v",
            "1",
            "-vf",
            &vf,
        ])
        .args(["-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .output()
        .ok()?;
    (out.status.success() && out.stdout.len() == (w * h * 4) as usize).then_some(out.stdout)
}

struct Case {
    name: &'static str,
    ext: &'static str,
    frames: u32,
    alpha: bool,
    args: &'static [&'static str],
    /// Minimum PSNR against the reference (None: no ffmpeg decoder).
    psnr: Option<f64>,
}

const CASES: &[Case] = &[
    Case {
        name: "jpeg",
        ext: "jpg",
        frames: 1,
        alpha: false,
        args: &["-c:v", "mjpeg", "-q:v", "3", "-pix_fmt", "yuvj420p"],
        psnr: Some(30.0),
    },
    Case {
        name: "png",
        ext: "png",
        frames: 1,
        alpha: false,
        args: &["-compression_level", "1"],
        psnr: Some(32.0),
    },
    Case {
        name: "png-rgba",
        ext: "png",
        frames: 1,
        alpha: true,
        args: &["-compression_level", "1", "-pix_fmt", "rgba"],
        psnr: Some(32.0),
    },
    Case {
        name: "webp",
        ext: "webp",
        frames: 1,
        alpha: false,
        args: &[
            "-c:v",
            "libwebp",
            "-quality",
            "80",
            "-compression_level",
            "0",
        ],
        psnr: Some(30.0),
    },
    Case {
        name: "webp-alpha",
        ext: "webp",
        frames: 1,
        alpha: true,
        args: &[
            "-c:v",
            "libwebp",
            "-quality",
            "80",
            "-compression_level",
            "0",
            "-pix_fmt",
            "yuva420p",
        ],
        psnr: Some(30.0),
    },
    Case {
        name: "webp-lossless",
        ext: "webp",
        frames: 1,
        alpha: false,
        args: &[
            "-c:v",
            "libwebp",
            "-lossless",
            "1",
            "-compression_level",
            "0",
        ],
        psnr: Some(32.0),
    },
    Case {
        name: "gif",
        ext: "gif",
        frames: 1,
        alpha: false,
        args: &["-pix_fmt", "pal8"],
        psnr: Some(26.0),
    },
    Case {
        name: "bmp",
        ext: "bmp",
        frames: 1,
        alpha: false,
        args: &["-pix_fmt", "bgr24"],
        psnr: Some(32.0),
    },
    Case {
        name: "tiff-lzw",
        ext: "tif",
        frames: 1,
        alpha: false,
        args: &["-compression_algo", "lzw", "-pix_fmt", "rgb24"],
        psnr: Some(32.0),
    },
    Case {
        name: "tiff-deflate-rgba",
        ext: "tif",
        frames: 1,
        alpha: true,
        args: &["-compression_algo", "deflate", "-pix_fmt", "rgba"],
        psnr: Some(32.0),
    },
    Case {
        name: "gif-animated",
        ext: "gif",
        frames: 3,
        alpha: false,
        args: &["-pix_fmt", "pal8", "-loop", "0"],
        psnr: Some(26.0),
    },
    Case {
        name: "apng",
        ext: "png",
        frames: 3,
        alpha: false,
        args: &["-f", "apng", "-plays", "0", "-compression_level", "1"],
        psnr: Some(32.0),
    },
    Case {
        name: "webp-animated",
        ext: "webp",
        frames: 3,
        alpha: false,
        args: &[
            "-c:v",
            "libwebp_anim",
            "-quality",
            "75",
            "-compression_level",
            "0",
            "-loop",
            "0",
        ],
        psnr: None,
    },
    Case {
        name: "avif",
        ext: "avif",
        frames: 1,
        alpha: false,
        args: &[
            "-c:v",
            "libaom-av1",
            "-still-picture",
            "1",
            "-cpu-used",
            "8",
            "-crf",
            "30",
            "-pix_fmt",
            "yuv420p",
        ],
        psnr: Some(30.0),
    },
    Case {
        name: "avif-10bit-444",
        ext: "avif",
        frames: 1,
        alpha: false,
        args: &[
            "-c:v",
            "libaom-av1",
            "-still-picture",
            "1",
            "-cpu-used",
            "8",
            "-crf",
            "30",
            "-pix_fmt",
            "yuv444p10le",
        ],
        psnr: Some(30.0),
    },
];

#[test]
fn every_resolution_and_format() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixtures_dir();
    let fit = Fit::cover(SCREEN.0, SCREEN.1, 8192, 1 << 25);
    let mut report = Vec::new();
    let max_res = std::env::var("BL_IMAGE_MAX_RESOLUTION").ok();
    let last = RESOLUTIONS
        .iter()
        .position(|r| Some(r.0) == max_res.as_deref())
        .unwrap_or(RESOLUTIONS.len() - 1);
    for &(res, w, h) in &RESOLUTIONS[..=last] {
        let big = w * h > 20_000_000;
        let (tw, th) = fit.target(w, h);
        assert_eq!((tw, th), (1920, 1080).min((w, h)), "{res}");
        // (name, file, case, lossless copy of the "png" case).
        let mut cases: Vec<(String, PathBuf, &Case, bool)> = Vec::new();
        for c in CASES {
            if c.name == "avif-10bit-444" && w as u64 * h as u64 > 64 << 20 {
                continue;
            }
            let frames = if big { c.frames.min(2) } else { c.frames };
            let p = dir.join(format!("{res}-{}.{}", c.name, c.ext));
            let t = Instant::now();
            if !gen(&p, w, h, frames, c.alpha, c.args) {
                // The Linux CI has every encoder; other builds of ffmpeg may
                // lack one (libwebp, APNG).
                assert!(
                    !cfg!(target_os = "linux"),
                    "ffmpeg could not create {}",
                    p.display()
                );
                eprintln!("note: ffmpeg could not create {}, skipped", p.display());
                continue;
            }
            eprintln!(
                "  made {} in {:.1}s",
                p.display(),
                t.elapsed().as_secs_f64()
            );
            cases.push((c.name.to_string(), p, c, false));
        }
        // Variants from other tools.
        let base = dir.join(format!("{res}-jpeg.jpg"));
        let prog = dir.join(format!("{res}-jpeg-progressive.jpg"));
        if progressive(&base, &prog) {
            cases.push(("jpeg-progressive".into(), prog, &CASES[0], false));
        } else {
            eprintln!("note: neither jpegtran nor Pillow, no progressive JPEG at {res}");
        }
        // ImageMagick holds several full copies (Q16): at most 8K.
        if have_convert() && w as u64 * h as u64 <= 64 << 20 {
            let png = dir.join(format!("{res}-png.png"));
            for (name, out, opts) in [
                (
                    "png-interlaced",
                    format!("{res}-interlaced.png"),
                    vec!["-interlace", "PNG"],
                ),
                (
                    "tiff-tiled-zip",
                    format!("{res}-tiled.tif"),
                    vec!["-compress", "Zip", "-define", "tiff:tile-geometry=256x256"],
                ),
                (
                    "tiff-planar-packbits",
                    format!("{res}-planar.tif"),
                    vec!["-compress", "RLE", "-interlace", "Plane"],
                ),
            ] {
                let o = dir.join(&out);
                let mut args = vec!["-limit", "area", "1GP", png.to_str().unwrap()];
                args.extend(opts.iter());
                let os = o.to_str().unwrap().to_string();
                args.push(&os);
                if convert(&args) {
                    cases.push((name.into(), o, &CASES[1], true));
                }
            }
        } else if have_convert() {
            eprintln!("note: no ImageMagick above 8K (memory), no interlaced / tiled / planar variants at {res}");
        }
        let mut png_img: Option<bl_image::Image> = None;
        for (name, path, case, copy_of_png) in &cases {
            let data = std::fs::read(path).unwrap();
            let i = info(&data).unwrap_or_else(|e| panic!("{res} {name}: {e}"));
            assert_eq!((i.width, i.height), (w, h), "{res} {name}");
            let animated = case.frames > 1;
            assert_eq!(i.is_animated(), animated, "{res} {name}");
            let t = Instant::now();
            // As the app does: the decoder owns the file's bytes.
            let owned = data.clone();
            let (img, peak) = measured(move || decode_fit_owned(owned, &fit));
            let secs = t.elapsed().as_secs_f64();
            let img = img.unwrap_or_else(|e| panic!("{res} {name}: {e}"));
            assert_eq!((img.width, img.height), (tw, th), "{res} {name}");
            let out_bytes = tw as usize * th as usize * 4;
            // A few target-size buffers and fixed working memory (VP8L
            // history, inflate windows, JPEG coefficients of the scaled
            // planes, the coarse grid of an interlaced PNG), never the full
            // image.
            let budget = 5 * out_bytes + (64 << 20);
            assert!(
                peak <= budget,
                "{res} {name}: {} MB peak, budget {} MB",
                peak >> 20,
                budget >> 20
            );
            // Lossless copies of the PNG (ImageMagick variants, whose
            // tiled / predicted TIFFs ffmpeg cannot read) must decode to
            // the same pixels.
            let quality = if *copy_of_png {
                let png = png_img.as_ref().expect("png case decoded first");
                let d = diff(&img.pixels, &png.pixels);
                if name == "png-interlaced" && w as u64 * h as u64 > 8 << 20 {
                    // Large interlaced PNGs are read from a coarser pass grid.
                    assert!(
                        d.psnr >= 28.0,
                        "{res} {name}: PSNR {:.1} against the PNG",
                        d.psnr
                    );
                } else {
                    assert!(
                        d.max <= 1,
                        "{res} {name}: differs from the PNG by {}",
                        d.max
                    );
                }
                "same as PNG".to_string()
            } else {
                match (case.psnr, reference(path, tw, th)) {
                    (Some(min), Some(r)) => {
                        let d = diff(&img.pixels, &r);
                        assert!(d.psnr >= min, "{res} {name}: PSNR {:.1} < {min}", d.psnr);
                        format!("PSNR {:.1} dB", d.psnr)
                    }
                    _ => "no reference".to_string(),
                }
            };
            if name == "png" {
                png_img = Some(img.clone());
            }
            // Animations: all frames at target size, within the same budget.
            let mut anim_note = String::new();
            if animated {
                let shared = data.clone();
                let ((n, first), apeak) = measured(|| {
                    let mut a = Animation::new(shared, &fit).unwrap().expect("animated");
                    assert_eq!(a.size(), (tw, th));
                    let first = a.next_frame();
                    let mut n = 1;
                    while a.position() != 0 {
                        let f = a.next_frame();
                        assert_eq!((f.image.width, f.image.height), (tw, th));
                        assert!(f.delay_ms >= 20);
                        n += 1;
                    }
                    (n, first)
                });
                assert_eq!(
                    n,
                    if big { 2 } else { case.frames as usize },
                    "{res} {name}"
                );
                // An animation keeps its file (it decodes the frames again
                // in every loop); the bytes were allocated before measuring.
                assert!(apeak <= budget, "{res} {name} frames: {} MB", apeak >> 20);
                // The first frame equals the still decode.
                let d = diff(&first.image.pixels, &img.pixels);
                assert!(d.max <= 2, "{res} {name}: first frame differs by {}", d.max);
                anim_note = format!(", {n} frames, {} MB", apeak >> 20);
            }
            let line = format!(
                "{res:>5} {name:<22} {:>6.2}s {:>4} MB file {:>4} MB peak, {quality}{anim_note}",
                secs,
                data.len() >> 20,
                peak >> 20
            );
            eprintln!("{line}");
            report.push(line);
            drop(data);
            let _ = std::fs::remove_file(path);
        }
        let _ = std::fs::remove_file(&base);
    }
    grid_16k(&dir, &fit, &mut report);
    let _ = std::fs::remove_dir_all(&dir);
    eprintln!("\n{}", report.join("\n"));
    // Formats reported by `info` for the cases above.
    let _ = Format::Tiff;
}

/// A 16K AVIF grid of 30 x 17 tiles of 512x512 (all the same coded tile, in
/// 4:4:4 so that no chroma is interpolated across tile edges): decoded to
/// 1920x1080 every tile becomes the 8:1 reduction of the tile.
fn grid_16k(dir: &Path, fit: &Fit, report: &mut Vec<String>) {
    use common::heif as hf;
    let p = dir.join("tile.obu");
    let src = source(512, 512, false, 10);
    let o = p.to_str().unwrap().to_string();
    if !ffmpeg(&[
        "-f",
        "lavfi",
        "-i",
        &src,
        "-frames:v",
        "1",
        "-c:v",
        "libaom-av1",
        "-still-picture",
        "1",
        "-cpu-used",
        "8",
        "-crf",
        "30",
        "-pix_fmt",
        "yuv444p",
        "-f",
        "obu",
        &o,
    ]) {
        assert!(
            !cfg!(target_os = "linux"),
            "ffmpeg could not encode an AV1 tile"
        );
        eprintln!("note: no libaom, no 16K AVIF grid");
        return;
    }
    let obu = hf::strip_td(&std::fs::read(&p).unwrap());
    let props = vec![hf::av1c(&obu), hf::ispe(512, 512)];
    let (cols, rows) = (30usize, 17usize);
    let mut items = Vec::new();
    for k in 0..cols * rows {
        items.push(hf::Item {
            kind: *b"av01",
            data: if k == 0 {
                hf::Data::Own(obu.clone())
            } else {
                hf::Data::SameAs(0)
            },
            props: props.clone(),
            hidden: true,
        });
    }
    items.push(hf::Item::new(
        b"grid",
        hf::grid(rows as u8, cols as u8, 15360, 8640),
        vec![hf::ispe(15360, 8640)],
    ));
    let grid = items.len() as u32;
    let tiles: Vec<u32> = (1..grid).collect();
    let file = hf::file(b"avif", &items, grid, &[(b"dimg", grid, tiles)]);
    let i = info(&file).unwrap();
    assert_eq!((i.format, i.width, i.height), (Format::Avif, 15360, 8640));
    let t = Instant::now();
    let owned = file.clone();
    let (img, peak) = measured(move || decode_fit_owned(owned, fit));
    let secs = t.elapsed().as_secs_f64();
    let img = img.unwrap();
    assert_eq!((img.width, img.height), (1920, 1080));
    let budget = 5 * 1920 * 1080 * 4 + (64 << 20);
    assert!(peak <= budget, "16K grid: {} MB peak", peak >> 20);
    let tile = bl_image::decode_fit(
        &hf::file(
            b"avif",
            &[hf::Item::new(b"av01", obu.clone(), props)],
            1,
            &[],
        ),
        &Fit::within(64),
    )
    .unwrap();
    assert_eq!((tile.width, tile.height), (64, 64));
    let mut max = 0u8;
    for y in 0..1080usize {
        for x in 0..1920usize {
            let a = &img.pixels[(y * 1920 + x) * 4..(y * 1920 + x) * 4 + 4];
            let b = &tile.pixels[((y % 64) * 64 + x % 64) * 4..((y % 64) * 64 + x % 64) * 4 + 4];
            for c in 0..4 {
                max = max.max(a[c].abs_diff(b[c]));
            }
        }
    }
    assert!(max <= 1, "16K grid differs from its tile by {max}");
    let line = format!(
        "  16K avif-grid-512        {secs:>6.2}s {:>4} MB file {:>4} MB peak, max diff {max}",
        file.len() >> 20,
        peak >> 20
    );
    eprintln!("{line}");
    report.push(line);
}
