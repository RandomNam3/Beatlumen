//! Shared helpers for the integration tests: ffmpeg fixture generation,
//! reference decoding and comparison metrics.

#![allow(dead_code)]

use oc_mp3::Mp3Decoder;
use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn fixture_dir() -> PathBuf {
    let d = std::env::temp_dir().join("oc-mp3-fixtures");
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Generates `name` with ffmpeg (inputs + output options) unless cached.
pub fn gen(name: &str, args: &[&str]) -> Option<PathBuf> {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available, skipping");
        return None;
    }
    let path = fixture_dir().join(name);
    if !path.exists() {
        static COUNTER: AtomicUsize = AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        let tmp = fixture_dir().join(format!("tmp-{}-{n}-{name}", std::process::id()));
        let st = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y"])
            .args(args)
            .arg(&tmp)
            .status()
            .expect("run ffmpeg");
        if !st.success() {
            eprintln!("ffmpeg failed to create {name}, skipping");
            return None;
        }
        if std::fs::rename(&tmp, &path).is_err() && !path.exists() {
            panic!("could not create fixture {name}");
        }
    }
    Some(path)
}

/// Decodes with ffmpeg to interleaved f32 (native channel count).
pub fn ffmpeg_decode(path: &Path, extra_input_args: &[&str]) -> Vec<f32> {
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error"])
        .args(extra_input_args)
        .arg("-i")
        .arg(path)
        .args(["-f", "f32le", "-"])
        .output()
        .expect("run ffmpeg");
    assert!(
        out.status.success(),
        "ffmpeg decode failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    out.stdout
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

pub struct Decoded {
    pub pcm: Vec<f32>,
    pub sample_rate: u32,
    pub channels: u16,
    pub total_frames: Option<u64>,
}

pub fn decode_bytes(data: &[u8]) -> Decoded {
    let mut d = Mp3Decoder::new(Cursor::new(data)).expect("open");
    let total_frames = d.total_frames();
    let mut pcm = Vec::new();
    while d.next_chunk(&mut pcm).expect("decode") {}
    Decoded {
        pcm,
        sample_rate: d.sample_rate(),
        channels: d.channels(),
        total_frames,
    }
}

pub fn decode_file(path: &Path) -> Decoded {
    decode_bytes(&std::fs::read(path).unwrap())
}

/// Signal-to-noise ratio of `test` against `reference` in dB, and the
/// maximum absolute difference (over the common length).
pub fn snr(reference: &[f32], test: &[f32]) -> (f64, f64) {
    let n = reference.len().min(test.len());
    let mut sig = 0f64;
    let mut noise = 0f64;
    let mut max = 0f64;
    for i in 0..n {
        let r = f64::from(reference[i].clamp(-1.0, 1.0));
        let e = (f64::from(test[i]) - r).abs();
        sig += r * r;
        noise += e * e;
        max = max.max(e);
    }
    let snr = if noise == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / noise).log10()
    };
    (snr, max)
}
