//! The UI's image providers decode every picture with the core's decoders
//! (`bl_image_thumbnail` for files, `bl_image_fetch` for the network).
//! Corrupt, truncated and oversized pictures give null (the UI shows its
//! placeholder), never a crash or a huge allocation.

use beatlumen_core::*;
use std::ffi::CString;
use std::io::{Read, Write};
use std::net::TcpListener;

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { 0xedb8_8320 ^ (c >> 1) } else { c >> 1 };
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    let mut body = kind.to_vec();
    body.extend_from_slice(data);
    out.extend_from_slice(&body);
    out.extend_from_slice(&crc32(&body).to_be_bytes());
}

/// A PNG whose header claims `w` x `h` but whose data is a few bytes.
fn png_claiming(w: u32, h: u32) -> Vec<u8> {
    let mut p = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&w.to_be_bytes());
    ihdr.extend_from_slice(&h.to_be_bytes());
    ihdr.extend_from_slice(&[8, 6, 0, 0, 0]);
    chunk(&mut p, b"IHDR", &ihdr);
    // A zlib stream of a few zero bytes (stored block).
    chunk(&mut p, b"IDAT", &[0x78, 0x01, 0x01, 0x04, 0x00, 0xfb, 0xff, 0, 0, 0, 0, 0, 4, 0, 1]);
    chunk(&mut p, b"IEND", &[]);
    p
}

/// A JPEG whose frame header claims 65535 x 65535.
fn jpeg_claiming_huge() -> Vec<u8> {
    let mut j = vec![0xff, 0xd8];
    // Baseline frame: precision 8, height, width, one component.
    j.extend_from_slice(&[0xff, 0xc0, 0x00, 0x0b, 0x08, 0xff, 0xff, 0xff, 0xff, 0x01, 0x01, 0x11, 0x00]);
    j.extend_from_slice(&[0xff, 0xd9]);
    j
}

/// A GIF whose screen claims 65535 x 65535, with one tiny frame.
fn gif_claiming_huge() -> Vec<u8> {
    let mut g = b"GIF89a".to_vec();
    g.extend_from_slice(&[0xff, 0xff, 0xff, 0xff, 0x00, 0, 0]);
    g.extend_from_slice(&[0x2c, 0, 0, 0, 0, 1, 0, 1, 0, 0, 0x02, 0x02, 0x44, 0x01, 0x00, 0x3b]);
    g
}

fn good_png() -> Vec<u8> {
    let mut img = bl_image::Image::new(64, 48);
    for p in img.pixels.chunks_exact_mut(4) {
        p.copy_from_slice(&[200, 30, 30, 255]);
    }
    bl_image::encode_png(&img)
}

/// What the decoders make of a picture.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Expect {
    Picture,
    Null,
    /// Damaged but small: what can be read, or null.
    Either,
}
use Expect::*;

/// The crafted pictures.
fn samples() -> Vec<(&'static str, Vec<u8>, Expect)> {
    let good = good_png();
    let mut noise = vec![0xff, 0xd8, 0xff, 0xe0];
    let mut x = 0x1234_5678u32;
    for _ in 0..50_000 {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        noise.push(x as u8);
    }
    let mut flipped = good.clone();
    let mid = flipped.len() / 2;
    for b in &mut flipped[mid..mid + 16] {
        *b ^= 0x5a;
    }
    vec![
        ("good.png", good.clone(), Picture),
        ("truncated.png", good[..good.len() / 2].to_vec(), Either),
        ("flipped.png", flipped, Either),
        ("empty.jpg", Vec::new(), Null),
        ("noise.jpg", noise, Either),
        ("huge.png", png_claiming(100_000, 100_000), Null),
        ("wide.png", png_claiming(u32::MAX >> 1, 1), Null),
        ("huge.jpg", jpeg_claiming_huge(), Null),
        ("huge.gif", gif_claiming_huge(), Null),
        ("text.txt", b"not a picture at all".to_vec(), Null),
    ]
}

fn check(name: &str, expect: Expect, got: Option<(u32, u32)>) {
    match (expect, got) {
        (Picture, Some((w, h))) => {
            assert!(w <= 64 && h <= 48 && (w >= 32 || h >= 32), "{name}: {w}x{h}")
        }
        // Never larger than asked for (covering 32 x 32, within the source).
        (Either, Some((w, h))) => assert!(w <= 2048 && h <= 2048, "{name}: {w}x{h}"),
        (Null | Either, None) => {}
        _ => panic!("{name}: expected {expect:?}, got {got:?}"),
    }
}

/// Takes pixels from the core: (width, height) and checks the length.
unsafe fn take(p: *mut u8, w: u32, h: u32, len: usize) -> Option<(u32, u32)> {
    if p.is_null() {
        return None;
    }
    assert_eq!(len, w as usize * h as usize * 4);
    bl_bytes_free(p, len);
    Some((w, h))
}

#[test]
fn local_pictures_decode_or_give_null() {
    let dir = std::env::temp_dir().join(format!("bl-ffi-images-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, bytes, expect) in samples() {
        let path = dir.join(name);
        std::fs::write(&path, &bytes).unwrap();
        let c = CString::new(path.to_string_lossy().as_bytes()).unwrap();
        let (mut w, mut h, mut len) = (0u32, 0u32, 0usize);
        // SAFETY: valid path and out pointers.
        let got = unsafe {
            take(bl_image_thumbnail(c.as_ptr(), 32, 32, &mut w, &mut h, &mut len), w, h, len)
        };
        check(name, expect, got);
    }
    // Folders, missing files and null are no pictures either.
    for p in [dir.clone(), dir.join("missing.png")] {
        let c = CString::new(p.to_string_lossy().as_bytes()).unwrap();
        // SAFETY: valid path; null out pointers are allowed.
        let r = unsafe {
            bl_image_thumbnail(c.as_ptr(), 32, 32, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
        };
        assert!(r.is_null());
    }
    // SAFETY: null is allowed.
    let r = unsafe {
        bl_image_thumbnail(std::ptr::null(), 0, 0, std::ptr::null_mut(), std::ptr::null_mut(), std::ptr::null_mut())
    };
    assert!(r.is_null());
    let _ = std::fs::remove_dir_all(&dir);
}

/// Serves `pictures` by name over HTTP; "/big" claims 100 MiB, "/gone" 404s.
fn serve(pictures: Vec<(&'static str, Vec<u8>, Expect)>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for conn in listener.incoming() {
            let Ok(mut c) = conn else { continue };
            let mut req = [0u8; 2048];
            let n = c.read(&mut req).unwrap_or(0);
            let line = String::from_utf8_lossy(&req[..n]);
            let path = line.split_whitespace().nth(1).unwrap_or("").to_string();
            let name = path.trim_start_matches('/');
            if name == "big" {
                let _ = c.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 104857600\r\n\r\n");
                let _ = c.write_all(&[0xff; 64 * 1024]);
                continue;
            }
            match pictures.iter().find(|(n, ..)| *n == name) {
                Some((_, bytes, _)) => {
                    let _ = write!(c, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len());
                    let _ = c.write_all(bytes);
                }
                None => {
                    let _ = c.write_all(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");
                }
            }
        }
    });
    port
}

#[test]
fn pictures_from_the_network_decode_or_give_null() {
    let samples = samples();
    let port = serve(samples.clone());
    let fetch = |url: &str| {
        let c = CString::new(url).unwrap();
        let (mut w, mut h, mut len) = (0u32, 0u32, 0usize);
        // SAFETY: valid URL and out pointers.
        unsafe { take(bl_image_fetch(c.as_ptr(), 32, 32, &mut w, &mut h, &mut len), w, h, len) }
    };
    for (name, _, expect) in &samples {
        check(name, *expect, fetch(&format!("http://127.0.0.1:{port}/{name}")));
    }
    // Too large to fetch, missing, or not http: null.
    assert!(fetch(&format!("http://127.0.0.1:{port}/big")).is_none());
    assert!(fetch(&format!("http://127.0.0.1:{port}/gone")).is_none());
    assert!(fetch("file:///etc/passwd").is_none());
    assert!(fetch("").is_none());
}
