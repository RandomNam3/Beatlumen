//! What leaves the decoder and the packing is, byte for byte, what the file
//! holds (against `ffmpeg`'s own decoding). This is what the direct output
//! relies on. Skipped (passing) when ffmpeg is missing.

mod common;

use bl_media::{pack_pcm, PcmFormat};
use common::*;
use std::path::Path;
use std::process::Command;

fn raw(path: &Path, bits: u8) -> Vec<u8> {
    let (format, codec) = if bits == 16 {
        ("s16le", "pcm_s16le")
    } else {
        ("s24le", "pcm_s24le")
    };
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-nostdin", "-i"])
        .arg(path)
        .args(["-map", "0:a:0", "-f", format, "-acodec", codec, "-"])
        .output()
        .unwrap();
    assert!(o.status.success());
    o.stdout
}

fn decoded(path: &Path) -> (Vec<f32>, Option<u8>) {
    let file = std::fs::File::open(path).unwrap();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string);
    let mut dec = bl_media::open(Box::new(file), ext.as_deref()).unwrap();
    assert_eq!(dec.spec().channels, 2);
    let mut out = Vec::new();
    while dec.next_chunk(&mut out).unwrap() {}
    (out, dec.bits_per_sample())
}

#[test]
fn lossless_files_come_out_as_they_went_in() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let s24_flac: &[&str] = &[
        "-c:a",
        "flac",
        "-sample_fmt",
        "s32",
        "-bits_per_raw_sample",
        "24",
    ];
    let cases: &[(&str, &[&str], u8)] = &[
        ("bx16.wav", &["-c:a", "pcm_s16le"], 16),
        ("bx24.wav", &["-c:a", "pcm_s24le"], 24),
        ("bx16.aiff", &["-c:a", "pcm_s16be"], 16),
        ("bx24.aiff", &["-c:a", "pcm_s24be"], 24),
        ("bx16.flac", &["-c:a", "flac", "-sample_fmt", "s16"], 16),
        ("bx24.flac", s24_flac, 24),
        ("bx16.mka", &["-c:a", "flac", "-sample_fmt", "s16"], 16),
        ("bx16a.m4a", &["-c:a", "alac", "-sample_fmt", "s16p"], 16),
        ("bx24a.m4a", &["-c:a", "alac", "-sample_fmt", "s32p"], 24),
    ];
    let mut checked = 0;
    for (name, args, bits) in cases {
        let Some(path) = make(name, "stereo44", args) else {
            eprintln!("{name}: fixture generation failed (encoder missing?)");
            continue;
        };
        let want = raw(&path, *bits);
        let (samples, reported) = decoded(&path);
        assert_eq!(reported, Some(*bits), "{name}");
        let format = if *bits == 16 {
            PcmFormat::S16
        } else {
            PcmFormat::S24Packed
        };
        let mut got = vec![0; samples.len() / 2 * format.bytes() * 2];
        pack_pcm(&samples, format, 2, &mut got);
        assert_eq!(got.len(), want.len(), "{name}: length");
        let first_difference = got.iter().zip(&want).position(|(a, b)| a != b);
        assert_eq!(first_difference, None, "{name}");
        // Also in the wider word most USB DACs want.
        let mut wide = vec![0; samples.len() / 2 * 8];
        pack_pcm(&samples, PcmFormat::S32, 2, &mut wide);
        let sample_bytes = *bits as usize / 8;
        for (w, n) in wide.chunks_exact(4).zip(want.chunks_exact(sample_bytes)) {
            assert_eq!(&w[4 - sample_bytes..], n, "{name}: in 32 bits");
            assert!(w[..4 - sample_bytes].iter().all(|b| *b == 0), "{name}");
        }
        checked += 1;
    }
    assert!(checked >= 7, "only {checked} formats could be checked");
}

#[test]
fn lossy_and_float_files_have_no_bits_to_keep() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    for (name, args) in [
        ("bxl.mp3", &["-c:a", "libmp3lame", "-b:a", "128k"][..]),
        ("bxl.ogg", &["-c:a", "libvorbis"][..]),
        ("bxf.wav", &["-c:a", "pcm_f32le"][..]),
    ] {
        let Some(path) = make(name, "stereo44", args) else {
            continue;
        };
        assert_eq!(decoded(&path).1, None, "{name}");
    }
}
