//! Shared helpers for the integration tests: ffmpeg fixture generation,
//! ADTS splitting, alignment search and SNR measurement.
#![allow(dead_code)]

pub mod synth;

use bl_aac::{AacDecoder, AdtsHeader};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn temp_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir()
        .join(format!("bl-aac-tests-{}", std::process::id()))
        .join(name);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn run(cmd: &mut Command) -> bool {
    match cmd.output() {
        Ok(o) if o.status.success() => true,
        Ok(o) => {
            eprintln!(
                "command failed: {:?}\n{}",
                cmd,
                String::from_utf8_lossy(&o.stderr)
            );
            false
        }
        Err(e) => {
            eprintln!("command failed to start: {e}");
            false
        }
    }
}

/// A deterministic test signal expression per channel (tones, a chirp,
/// noise and periodic transients to trigger block switching).
pub fn signal_expr(channels: usize) -> String {
    let mut exprs = Vec::new();
    for c in 0..channels {
        let f = 220.0 * (1.0 + c as f64 * 0.37);
        exprs.push(format!(
            "0.25*sin(2*PI*{f}*t)*(1+0.5*sin(2*PI*{am}*t))+0.12*sin(2*PI*({f2}+{sw}*t)*t)+0.05*(random({seed})-0.5)\
             +0.4*lt(mod(t+{off},0.37),0.004)*sin(2*PI*2500*t)+0.08*sin(2*PI*{f3}*t)",
            am = 2.0 + c as f64,
            f2 = 500.0 + 100.0 * c as f64,
            sw = 1500.0 + 300.0 * c as f64,
            seed = c + 1,
            off = 0.05 * c as f64,
            f3 = 5000.0 + 700.0 * c as f64
        ));
    }
    exprs.join("|")
}

/// Generates a WAV test signal.
pub fn make_wav(path: &Path, rate: u32, channels: usize, seconds: f64) -> bool {
    let layout = match channels {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        6 => "5.1",
        8 => "7.1",
        _ => "",
    };
    let layout = if layout.is_empty() {
        String::new()
    } else {
        format!(":c={layout}")
    };
    run(Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-f",
            "lavfi",
            "-i",
        ])
        .arg(format!(
            "aevalsrc='{}':s={rate}:d={seconds}{layout}",
            signal_expr(channels)
        ))
        .args(["-c:a", "pcm_f32le"])
        .arg(path))
}

/// Decodes `input` with ffmpeg to interleaved f32 (native channel order).
pub fn ffmpeg_decode(input: &Path) -> Option<Vec<f32>> {
    let out = input.with_extension("ref.f32");
    if !run(Command::new("ffmpeg")
        .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
        .arg(input)
        .args(["-f", "f32le"])
        .arg(&out))
    {
        return None;
    }
    let bytes = std::fs::read(&out).ok()?;
    Some(
        bytes
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
    )
}

/// Splits an ADTS stream into frames.
pub fn adts_frames(data: &[u8]) -> Vec<&[u8]> {
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + 7 <= data.len() {
        match AdtsHeader::parse(&data[pos..]) {
            Some(h) if pos + h.frame_length <= data.len() => {
                frames.push(&data[pos..pos + h.frame_length]);
                pos += h.frame_length;
            }
            _ => pos += 1,
        }
    }
    frames
}

/// Decodes a whole ADTS stream with our decoder.
pub fn decode_adts(data: &[u8]) -> Result<(Vec<f32>, u32, u16), bl_aac::Error> {
    let frames = adts_frames(data);
    let h = AdtsHeader::parse(frames[0]).unwrap();
    let mut dec = AacDecoder::new(&h.audio_specific_config())?;
    let mut out = Vec::new();
    for f in frames {
        dec.decode_adts_frame(f, &mut out)?;
    }
    Ok((out, dec.sample_rate(), dec.channels()))
}

/// Finds the frame offset `d` (ours[i + d] ~ reference[i]) maximising
/// correlation over the first channel, searching `-max..=max`.
pub fn best_offset(ours: &[f32], reference: &[f32], channels: usize, max: isize) -> isize {
    let n_ours = ours.len() / channels;
    let n_ref = reference.len() / channels;
    let mut best = (f64::MIN, 0isize);
    for d in -max..=max {
        let mut dot = 0.0f64;
        let mut e = 0.0f64;
        let mut i = 0usize;
        while i < n_ref.min(48000) {
            let j = i as isize + d;
            if j >= 0 && (j as usize) < n_ours {
                for c in 0..channels {
                    let a = ours[j as usize * channels + c] as f64;
                    let b = reference[i * channels + c] as f64;
                    dot += a * b;
                    e += a * a;
                }
            }
            i += 1;
        }
        let score = if e > 0.0 { dot / e.sqrt() } else { f64::MIN };
        if score > best.0 {
            best = (score, d);
        }
    }
    best.1
}

pub struct Compare {
    pub snr_db: f64,
    pub max_abs_err: f32,
    pub compared_frames: usize,
    pub offset: isize,
}

/// Compares two interleaved signals after alignment.
pub fn compare(ours: &[f32], reference: &[f32], channels: usize, max_offset: isize) -> Compare {
    let offset = best_offset(ours, reference, channels, max_offset);
    let n_ours = ours.len() / channels;
    let n_ref = reference.len() / channels;
    let mut sig = 0.0f64;
    let mut err = 0.0f64;
    let mut max_abs_err = 0.0f32;
    let mut count = 0;
    for i in 0..n_ref {
        let j = i as isize + offset;
        if j < 0 || j as usize >= n_ours {
            continue;
        }
        count += 1;
        for c in 0..channels {
            let a = ours[j as usize * channels + c];
            let b = reference[i * channels + c];
            sig += (b as f64).powi(2);
            err += ((a - b) as f64).powi(2);
            max_abs_err = max_abs_err.max((a - b).abs());
        }
    }
    let snr_db = if err == 0.0 {
        200.0
    } else {
        10.0 * (sig / err).log10()
    };
    Compare {
        snr_db,
        max_abs_err,
        compared_frames: count,
        offset,
    }
}
