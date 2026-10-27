//! inflate/deflate round trips and interop, plus corruption / truncation
//! fuzzing of every decoder (must return errors or images, never panic).

mod common;

use bl_image::{decode, decode_max, deflate, encode_jpeg, encode_png, inflate, Image};
use common::*;
use std::process::{Command, Stdio};

fn sample_data(rng: &mut Rng) -> Vec<Vec<u8>> {
    let mut v = vec![Vec::new(), vec![0u8], vec![7u8; 100_000]];
    // Random bytes (incompressible).
    v.push((0..70_000).map(|_| rng.next() as u8).collect());
    // Text-like data with repetitions.
    let words = [
        "image ",
        "decoder ",
        "deflate ",
        "huffman ",
        "beatlumen ",
        "\n",
        "0123 ",
        "zlib ",
    ];
    let mut t = Vec::new();
    while t.len() < 200_000 {
        t.extend_from_slice(words[rng.below(words.len())].as_bytes());
    }
    v.push(t);
    // Low-entropy noise and long-distance repeats.
    let mut m: Vec<u8> = (0..50_000)
        .map(|i| ((i / 7) as u8).wrapping_add((rng.next() % 3) as u8))
        .collect();
    let copy = m[1000..40_000].to_vec();
    m.extend_from_slice(&copy);
    v.push(m);
    v
}

#[test]
fn zlib_round_trip_all_levels() {
    let mut rng = Rng(0x1234_5678);
    for data in sample_data(&mut rng) {
        for level in 0..=9 {
            let z = deflate::zlib_compress(&data, level);
            let back = inflate::zlib_decompress(&z).expect("round trip");
            assert_eq!(back, data, "level {level} len {}", data.len());
            let raw = deflate::deflate(&data, level);
            assert_eq!(inflate::inflate(&raw).unwrap(), data);
        }
        let z6 = deflate::zlib_compress(&data, 6);
        println!("len {:7} -> level 6: {:7} bytes", data.len(), z6.len());
    }
}

fn python_available() -> bool {
    Command::new("python3")
        .arg("-c")
        .arg("import zlib")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Runs python3 zlib on stdin data: `mode` is "compress<level>" or
/// "decompress".
fn python_zlib(mode: &str, data: &[u8]) -> Option<Vec<u8>> {
    use std::io::Write;
    let script = if let Some(level) = mode.strip_prefix("compress") {
        format!("import sys,zlib;sys.stdout.buffer.write(zlib.compress(sys.stdin.buffer.read(),{level}))")
    } else {
        "import sys,zlib;sys.stdout.buffer.write(zlib.decompress(sys.stdin.buffer.read()))"
            .to_string()
    };
    let mut child = Command::new("python3")
        .arg("-c")
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let mut stdin = child.stdin.take()?;
    let input = data.to_vec();
    let writer = std::thread::spawn(move || {
        let _ = stdin.write_all(&input);
    });
    let out = child.wait_with_output().ok()?;
    let _ = writer.join();
    out.status.success().then_some(out.stdout)
}

#[test]
fn zlib_interop_with_reference() {
    if !python_available() {
        eprintln!("note: python3 zlib unavailable, skipping interop test");
        return;
    }
    let mut rng = Rng(42);
    for data in sample_data(&mut rng) {
        for level in [1, 6, 9] {
            let theirs = python_zlib(&format!("compress{level}"), &data).unwrap();
            assert_eq!(inflate::zlib_decompress(&theirs).unwrap(), data);
            let ours = deflate::zlib_compress(&data, level as u8);
            assert_eq!(python_zlib("decompress", &ours).unwrap(), data);
            if level == 9 {
                println!(
                    "len {:7}: zlib -9 {:7} bytes, ours -9 {:7} bytes",
                    data.len(),
                    theirs.len(),
                    ours.len()
                );
            }
        }
    }
}

#[test]
fn inflate_corruption_never_panics() {
    let mut rng = Rng(99);
    let datas = sample_data(&mut rng);
    let mut errors = 0;
    let mut runs = 0;
    for data in &datas {
        for level in [0u8, 1, 6, 9] {
            let z = deflate::zlib_compress(data, level);
            for _ in 0..60 {
                let mut m = z.clone();
                match rng.below(3) {
                    0 => {
                        let n = 1 + rng.below(8);
                        for _ in 0..n {
                            let i = rng.below(m.len());
                            m[i] ^= 1 << rng.below(8);
                        }
                    }
                    1 => m.truncate(rng.below(m.len())),
                    _ => {
                        let i = rng.below(m.len());
                        for b in &mut m[i..] {
                            *b = rng.next() as u8;
                        }
                    }
                }
                runs += 1;
                if inflate::zlib_decompress_with_limit(&m, 1 << 22).is_err() {
                    errors += 1;
                }
                let _ = inflate::inflate_with_limit(&m, 1 << 22);
            }
        }
    }
    // Random garbage.
    for _ in 0..2000 {
        let n = rng.below(300);
        let g: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        let _ = inflate::inflate_with_limit(&g, 1 << 20);
        let _ = inflate::zlib_decompress_with_limit(&g, 1 << 20);
    }
    println!("{runs} corrupted streams, {errors} rejected");
    assert!(errors > runs / 2);
}

#[test]
fn inflate_bomb_limit() {
    let z = deflate::zlib_compress(&vec![0u8; 10_000_000], 9);
    assert!(z.len() < 20_000);
    assert!(inflate::zlib_decompress_with_limit(&z, 1_000_000).is_err());
    assert_eq!(
        inflate::zlib_decompress_with_limit(&z, 10_000_000)
            .unwrap()
            .len(),
        10_000_000
    );
    // Literal-heavy streams decode exactly up to the limit.
    let mut rng = Rng(7);
    for n in [1usize, 2, 3, 4, 5, 100, 70_001] {
        let data: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        for level in [1, 6] {
            let z = deflate::zlib_compress(&data, level);
            assert_eq!(inflate::zlib_decompress_with_limit(&z, n).unwrap(), data);
            assert!(inflate::zlib_decompress_with_limit(&z, n - 1).is_err());
        }
    }
}

/// Fixtures used for fuzzing: our own encoders always, plus ffmpeg /
/// ImageMagick generated files when available.
fn fuzz_fixtures() -> Vec<(String, Vec<u8>)> {
    let mut img = Image::new(37, 23);
    for (i, p) in img.pixels.chunks_exact_mut(4).enumerate() {
        p.copy_from_slice(&[
            (i * 7) as u8,
            (i * 3) as u8,
            (i * 11) as u8,
            (i % 256) as u8,
        ]);
    }
    let mut v = vec![
        ("ours.png".to_string(), encode_png(&img)),
        ("ours.jpg".to_string(), encode_jpeg(&img, 80)),
    ];
    if !have_ffmpeg() {
        return v;
    }
    let dir = fixture_dir("fuzz");
    let src = "testsrc2=s=67x45,format=rgb24";
    let gens: Vec<(&str, Vec<&str>)> = vec![
        ("a.png", vec!["-pix_fmt", "rgba"]),
        ("p.png", vec!["-pix_fmt", "pal8"]),
        ("g16.png", vec!["-pix_fmt", "gray16be"]),
        ("b.jpg", vec!["-pix_fmt", "yuvj420p"]),
        ("c.jpg", vec!["-pix_fmt", "yuvj444p"]),
        ("d.bmp", vec!["-pix_fmt", "bgr24"]),
        ("e.bmp", vec!["-pix_fmt", "pal8"]),
        ("f.gif", vec![]),
        ("l.webp", vec!["-c:v", "libwebp", "-lossless", "1"]),
        ("y.webp", vec!["-c:v", "libwebp", "-quality", "70"]),
    ];
    for (name, extra) in gens {
        let p = dir.join(name);
        let mut args = vec!["-f", "lavfi", "-i", src, "-frames:v", "1"];
        args.extend(extra.iter());
        let t = p.to_str().unwrap().to_string();
        args.push(&t);
        if ffmpeg(&args) {
            v.push((name.to_string(), std::fs::read(&p).unwrap()));
        }
    }
    if have_convert() {
        let s = dir.join("a.png");
        for (name, opts) in [
            ("prog.jpg", vec!["-interlace", "JPEG", "-quality", "80"]),
            ("cmyk.jpg", vec!["-colorspace", "CMYK"]),
            ("inter.png", vec!["-interlace", "PNG"]),
            ("rle.bmp", vec!["-colors", "50", "-compress", "RLE"]),
            ("alpha.webp", vec!["-quality", "70"]),
        ] {
            let out = dir.join(name);
            let mut args = vec![s.to_str().unwrap()];
            args.extend(opts.iter());
            let o = if name == "rle.bmp" {
                format!("BMP3:{}", out.to_str().unwrap())
            } else {
                out.to_str().unwrap().to_string()
            };
            args.push(&o);
            if convert(&args) {
                v.push((name.to_string(), std::fs::read(&out).unwrap()));
            }
        }
    }
    v
}

#[test]
fn decoders_survive_corruption() {
    // OC_IMAGE_FUZZ_SEED / OC_IMAGE_FUZZ_ITERS allow longer campaigns.
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    let mut rng = Rng(env("OC_IMAGE_FUZZ_SEED", 0xdead_beef) | 1);
    let fixtures = fuzz_fixtures();
    for (name, data) in &fixtures {
        assert!(decode(data).is_ok(), "{name} must decode before corruption");
        let mut ok = 0;
        let iters = env("OC_IMAGE_FUZZ_ITERS", 400) as usize;
        for it in 0..iters {
            let mut m = data.clone();
            match it % 4 {
                0 => {
                    let n = 1 + rng.below(6);
                    for _ in 0..n {
                        let i = rng.below(m.len());
                        m[i] = rng.next() as u8;
                    }
                }
                1 => m.truncate(rng.below(m.len())),
                2 => {
                    // Corrupt a header-sized prefix region.
                    let i = rng.below(m.len().min(64));
                    m[i] = rng.next() as u8;
                }
                _ => {
                    let i = rng.below(m.len());
                    let n = rng.below(32);
                    for k in i..(i + n).min(m.len()) {
                        m[k] = rng.next() as u8;
                    }
                }
            }
            if let Ok(img) = decode(&m) {
                assert_eq!(
                    img.pixels.len(),
                    img.width as usize * img.height as usize * 4
                );
                ok += 1;
            }
            if it % 8 == 0 {
                let _ = decode_max(&m, 16);
            }
        }
        println!("{name}: {ok}/{iters} corrupted variants still decoded");
    }
    // Pure garbage with valid magic numbers.
    for magic in [
        &b"\x89PNG\r\n\x1a\n"[..],
        &[0xff, 0xd8, 0xff],
        b"BM",
        b"GIF89a",
        b"RIFF\0\0\0\0WEBP",
    ] {
        for _ in 0..300 {
            let mut g = magic.to_vec();
            let n = rng.below(400);
            g.extend((0..n).map(|_| rng.next() as u8));
            let _ = decode(&g);
            let _ = decode_max(&g, 32);
        }
    }
}

#[test]
fn huge_dimensions_are_rejected_cheaply() {
    // PNG claiming 16000x16000 with a few bytes of data.
    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&16000u32.to_be_bytes());
    ihdr.extend_from_slice(&16000u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    for (ty, body) in [
        (&b"IHDR"[..], ihdr),
        (b"IDAT", vec![0x78, 0x9c, 0x63, 0, 0]),
        (b"IEND", vec![]),
    ] {
        png.extend_from_slice(&(body.len() as u32).to_be_bytes());
        png.extend_from_slice(ty);
        png.extend_from_slice(&body);
        png.extend_from_slice(&[0, 0, 0, 0]);
    }
    assert!(decode(&png).is_err());
    // JPEG frame header claiming 65000x65000.
    let mut img = Image::new(8, 8);
    img.pixels.fill(128);
    let mut jpg = encode_jpeg(&img, 50);
    let sof = jpg.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
    jpg[sof + 5..sof + 9].copy_from_slice(&[0xfd, 0xe8, 0xfd, 0xe8]);
    assert!(decode(&jpg).is_err());
}

#[test]
fn resize_edge_cases() {
    let img = Image {
        width: 3,
        height: 2,
        pixels: vec![200; 24],
    };
    for (w, h) in [(1, 1), (1, 100), (100, 1), (3, 2), (6, 4), (2, 1), (0, 5)] {
        let r = bl_image::resize(&img, w, h);
        assert_eq!(r.pixels.len(), (w * h * 4) as usize);
        assert!(r.pixels.iter().all(|&v| v == 200));
    }
    // Inconsistent buffer must not panic.
    let bad = Image {
        width: 10,
        height: 10,
        pixels: vec![1; 7],
    };
    let _ = bl_image::resize(&bad, 5, 5);
    let _ = encode_png(&bad);
    let _ = encode_jpeg(&bad, 50);
    let empty = Image {
        width: 0,
        height: 0,
        pixels: vec![],
    };
    let _ = bl_image::resize(&empty, 4, 4);
    let _ = encode_png(&empty);
    let _ = encode_jpeg(&empty, 50);
}
