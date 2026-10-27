//! Decode speed measurements for 12 MP images. Ignored by default; run with
//! `cargo test -p oc-image --release --test bench -- --ignored --nocapture`.

mod common;

use common::*;
use std::time::Instant;

fn best_of<T>(n: usize, mut f: impl FnMut() -> T) -> (f64, T) {
    let mut best = f64::INFINITY;
    let mut out = None;
    for _ in 0..n {
        let t = Instant::now();
        let r = f();
        best = best.min(t.elapsed().as_secs_f64());
        out = Some(r);
    }
    (best, out.unwrap_or_else(|| unreachable!()))
}

#[test]
#[ignore]
fn bench_12mp() {
    if !have_ffmpeg() {
        return;
    }
    let dir = fixture_dir("bench");
    // Photo-like content: blurred test pattern plus grain, encoded like a
    // camera JPEG (quality 92, 4:2:0) when ImageMagick is available.
    let src = "testsrc2=s=4000x3000,format=yuv444p,gblur=sigma=1.5,noise=alls=20:allf=u,format=rgb24";
    let jpg = dir.join("12mp.jpg");
    let png = dir.join("12mp.png");
    let prog = dir.join("12mp_progressive.jpg");
    assert!(ffmpeg(&["-f", "lavfi", "-i", src, "-frames:v", "1", png.to_str().unwrap()]));
    let im = have_convert();
    if !(im && convert(&[png.to_str().unwrap(), "-quality", "92", "-sampling-factor", "2x2", jpg.to_str().unwrap()])) {
        assert!(ffmpeg(&["-i", png.to_str().unwrap(), "-pix_fmt", "yuvj420p", "-q:v", "2", "-qmin", "1", jpg.to_str().unwrap()]));
    }
    let have_prog = im && convert(&[png.to_str().unwrap(), "-quality", "92", "-sampling-factor", "2x2", "-interlace", "JPEG", prog.to_str().unwrap()]);
    let mp = 12.0;
    let report = |name: &str, bytes: usize, secs: f64| {
        println!("{name:34} {:6.1} ms  {:6.1} MP/s  ({:.1} MB file)", secs * 1e3, mp / secs, bytes as f64 / 1e6);
    };
    for (name, path) in [("JPEG baseline 4:2:0 q92", &jpg), ("PNG RGB8", &png), ("JPEG progressive 4:2:0 q92", &prog)] {
        if name.contains("progressive") && !have_prog {
            continue;
        }
        let data = std::fs::read(path).unwrap();
        let (t, img) = best_of(3, || oc_image::decode(&data).unwrap());
        assert_eq!((img.width, img.height), (4000, 3000));
        report(&format!("decode {name}"), data.len(), t);
        if name.starts_with("JPEG") {
            for max in [1024, 512, 256] {
                let (t, im) = best_of(3, || oc_image::decode_max(&data, max).unwrap());
                report(&format!("decode_max({max}) {name}"), data.len(), t);
                assert!(im.width.max(im.height) <= max);
            }
        } else {
            let (t, _) = best_of(3, || oc_image::decode_max(&data, 512).unwrap());
            report(&format!("decode_max(512) {name}"), data.len(), t);
        }
    }
    let data = std::fs::read(&jpg).unwrap();
    let img = oc_image::decode(&data).unwrap();
    let (t, small) = best_of(3, || oc_image::resize(&img, 1024, 768));
    report("resize 4000x3000 -> 1024x768", 0, t);
    let (t, j) = best_of(3, || oc_image::encode_jpeg(&small, 85));
    println!("encode_jpeg 1024x768 q85          {:6.1} ms  ({} bytes)", t * 1e3, j.len());
    let (t, p) = best_of(3, || oc_image::encode_png(&small));
    println!("encode_png 1024x768               {:6.1} ms  ({} bytes)", t * 1e3, p.len());
    let raw: Vec<u8> = img.pixels.iter().step_by(2).copied().collect();
    let z = oc_image::deflate::zlib_compress(&raw, 6);
    let (t, _) = best_of(3, || oc_image::inflate::zlib_decompress(&z).unwrap());
    println!("inflate {:.1} MB -> {:.1} MB         {:6.1} ms  {:6.0} MB/s output", z.len() as f64 / 1e6, raw.len() as f64 / 1e6, t * 1e3, raw.len() as f64 / 1e6 / t);
}
