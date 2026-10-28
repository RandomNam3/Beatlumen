//! Shared helpers for the bl-opus integration tests: deterministic test
//! signals, ffmpeg (libopus) encoding / reference decoding, Ogg demuxing with
//! RFC 7845 pre-skip and end trimming, and error metrics.
#![allow(dead_code)]

use std::fs::File;
use std::io::{BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::Command;

use bl_opus::{OpusDecoder, OpusHead};

pub fn ffmpeg_available() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-hide_banner")
        .arg("-encoders")
        .output();
    match ok {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains("libopus"),
        Err(_) => false,
    }
}

pub fn work_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("bl-opus-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Kinds of test material.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Signal {
    /// Polyphonic tones, chords, percussion, stereo panning.
    Music,
    /// Formant-filtered glottal pulses with syllable envelopes.
    Speech,
    /// Alternating speech / music / silence segments (provokes mode switches).
    Mixed,
}

/// Generates `seconds` of interleaved audio.
pub fn gen_signal(kind: Signal, rate: u32, channels: usize, seconds: f32) -> Vec<f32> {
    let n = (rate as f32 * seconds) as usize;
    let mut out = vec![0f32; n * channels];
    let mut rng = Lcg(0x1234_5678 + channels as u64 * 77 + rate as u64);
    let fs = rate as f32;
    // Music state
    let notes = [
        220.0f32, 261.63, 329.63, 392.0, 440.0, 523.25, 659.25, 783.99, 880.0, 1046.5,
    ];
    let mut phases = [0f32; 8];
    let mut perc_env = 0f32;
    // Speech state
    let mut glottal_phase = 0f32;
    let mut f_state = [[0f32; 2]; 3];
    let mut lp = 0f32;
    for i in 0..n {
        let t = i as f32 / fs;
        let seg = (t / 0.75) as usize;
        let kind_now = match kind {
            Signal::Mixed => match seg % 4 {
                0 => Signal::Speech,
                1 => Signal::Music,
                2 => Signal::Speech,
                _ => Signal::Music,
            },
            k => k,
        };
        let silent = kind == Signal::Mixed && (t % 3.0) > 2.85;
        let mut mono = 0f32;
        let mut pan = 0.5f32;
        if kind_now == Signal::Music {
            let step = (t / 0.4) as usize;
            for v in 0..4 {
                let f = notes[(step * 3 + v * 2) % notes.len()] * (1.0 + 0.003 * (6.0 * t).sin());
                for h in 0..2 {
                    let idx = v * 2 + h;
                    phases[idx] += 2.0 * std::f32::consts::PI * f * (h + 1) as f32 / fs;
                    if phases[idx] > 2.0 * std::f32::consts::PI {
                        phases[idx] -= 2.0 * std::f32::consts::PI;
                    }
                    mono += phases[idx].sin() * 0.08 / (h + 1) as f32;
                }
            }
            if i % (rate as usize / 4) == 0 {
                perc_env = 0.6;
            }
            perc_env *= 0.9993;
            lp = 0.7 * lp + 0.3 * rng.next();
            mono += perc_env * lp;
            pan = 0.5 + 0.4 * (0.7 * t).sin();
        } else if kind_now == Signal::Speech {
            let pitch = 110.0 + 40.0 * (2.3 * t).sin() + 20.0 * (0.9 * t).sin();
            glottal_phase += pitch / fs;
            let mut exc = 0f32;
            if glottal_phase >= 1.0 {
                glottal_phase -= 1.0;
                exc = 1.0;
            }
            let syll = (t * 4.0) % 1.0;
            let voiced = syll < 0.7;
            let env = (std::f32::consts::PI * (syll / 0.7).min(1.0))
                .sin()
                .max(0.0);
            let src = if voiced {
                exc * 0.9 + 0.02 * rng.next()
            } else {
                0.15 * rng.next()
            };
            let formants = [
                (500.0 + 200.0 * (1.7 * t).sin(), 80.0),
                (1500.0 + 300.0 * (1.1 * t).cos(), 120.0),
                (2500.0f32, 160.0f32),
            ];
            let mut y = 0f32;
            for (k, &(f, bw)) in formants.iter().enumerate() {
                if f >= fs * 0.45 {
                    continue;
                }
                let r = (-std::f32::consts::PI * bw / fs).exp();
                let a1 = 2.0 * r * (2.0 * std::f32::consts::PI * f / fs).cos();
                let a2 = -r * r;
                let v = src + a1 * f_state[k][0] + a2 * f_state[k][1];
                f_state[k][1] = f_state[k][0];
                f_state[k][0] = v;
                y += v * (1.0 - r) * 0.5;
            }
            mono = y * if voiced { env } else { 0.5 } * 2.0;
        }
        if silent {
            mono = 0.0;
        }
        for c in 0..channels {
            let g = match (channels, c) {
                (1, _) => 1.0,
                (2, 0) => 1.0 - pan,
                (2, _) => pan,
                (_, c) => 0.3 + 0.1 * ((c as f32 + 1.0) * 0.37 * t).sin(),
            } * 1.4;
            let ch_noise = if channels > 2 { 0.01 * rng.next() } else { 0.0 };
            out[i * channels + c] = (mono * g + ch_noise).clamp(-1.0, 1.0);
        }
    }
    out
}

pub fn write_f32(path: &Path, data: &[f32]) {
    let mut f = File::create(path).unwrap();
    let mut bytes = Vec::with_capacity(data.len() * 4);
    for v in data {
        bytes.extend_from_slice(&v.to_le_bytes());
    }
    f.write_all(&bytes).unwrap();
}

pub fn read_f32(path: &Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn layout_name(channels: usize) -> &'static str {
    match channels {
        1 => "mono",
        2 => "stereo",
        3 => "3.0",
        4 => "quad",
        5 => "5.0",
        6 => "5.1",
        7 => "6.1",
        8 => "7.1",
        _ => "stereo",
    }
}

/// Encodes raw f32 input with ffmpeg/libopus. Returns false on failure.
pub fn encode(input: &Path, rate: u32, channels: usize, args: &[&str], out: &Path) -> bool {
    let rate_s = rate.to_string();
    let ch_s = channels.to_string();
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-y",
        "-hide_banner",
        "-loglevel",
        "error",
        "-f",
        "f32le",
        "-ar",
        &rate_s,
        "-ac",
        &ch_s,
    ]);
    cmd.args(["-ch_layout", layout_name(channels)]);
    cmd.arg("-i").arg(input);
    cmd.args(["-c:a", "libopus"]);
    cmd.args(args);
    cmd.arg(out);
    match cmd.output() {
        Ok(o) => {
            if !o.status.success() {
                eprintln!(
                    "ffmpeg encode failed: {}",
                    String::from_utf8_lossy(&o.stderr)
                );
            }
            o.status.success()
        }
        Err(_) => false,
    }
}

/// Reference decode with the libopus decoder (float output, 48 kHz).
pub fn ref_decode(opus: &Path, out: &Path) -> Option<Vec<f32>> {
    let o = Command::new("ffmpeg")
        .args([
            "-y",
            "-hide_banner",
            "-loglevel",
            "error",
            "-request_sample_fmt",
            "flt",
            "-c:a",
            "libopus",
            "-i",
        ])
        .arg(opus)
        .args(["-f", "f32le", "-ar", "48000"])
        .arg(out)
        .output()
        .ok()?;
    if !o.status.success() {
        eprintln!(
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        return None;
    }
    Some(read_f32(out))
}

/// Opus packets of the first logical stream of an Ogg file.
pub struct OggOpus {
    pub head: OpusHead,
    pub packets: Vec<Vec<u8>>,
    pub last_granule: u64,
}

pub fn read_ogg_opus(path: &Path) -> OggOpus {
    let f = BufReader::new(File::open(path).unwrap());
    let mut r = bl_ogg::PacketReader::new(f);
    let first = r.next_packet().unwrap().unwrap();
    r.set_serial(Some(first.serial));
    let head = OpusHead::parse(&first.data).unwrap();
    let _tags = r.next_packet().unwrap().unwrap();
    let mut packets = Vec::new();
    let mut last_granule = 0;
    while let Some(p) = r.next_packet().unwrap() {
        if let Some(g) = p.granule {
            last_granule = g;
        }
        packets.push(p.data);
    }
    OggOpus {
        head,
        packets,
        last_granule,
    }
}

/// Decodes with bl-opus at `rate`, applying pre-skip and end trimming.
pub fn our_decode(ogg: &OggOpus, rate: u32) -> Vec<f32> {
    let mut dec = OpusDecoder::new(&ogg.head, rate).unwrap();
    let ch = ogg.head.channels as usize;
    let mut pcm = Vec::new();
    for p in &ogg.packets {
        dec.decode_packet(p, &mut pcm).unwrap();
    }
    let scale = |v: u64| (v * rate as u64 / 48000) as usize;
    let skip = scale(ogg.head.pre_skip as u64);
    let total = scale(ogg.last_granule.saturating_sub(ogg.head.pre_skip as u64));
    let start = (skip * ch).min(pcm.len());
    let end = ((skip + total) * ch).min(pcm.len());
    pcm[start..end].to_vec()
}

/// (SNR in dB, max abs error, compared length in samples)
pub fn compare(reference: &[f32], ours: &[f32]) -> (f64, f64, usize) {
    let n = reference.len().min(ours.len());
    let mut sig = 0f64;
    let mut err = 0f64;
    let mut maxe = 0f64;
    for i in 0..n {
        let r = reference[i] as f64;
        let d = r - ours[i] as f64;
        sig += r * r;
        err += d * d;
        maxe = maxe.max(d.abs());
    }
    let snr = if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    };
    (snr, maxe, n)
}

/// Histogram of TOC configurations (0..32) and code 0-3 usage.
pub fn toc_stats(packets: &[Vec<u8>]) -> String {
    let mut modes = [0usize; 3];
    let mut codes = [0usize; 4];
    for p in packets {
        if p.is_empty() {
            continue;
        }
        let toc = p[0];
        let m = if toc & 0x80 != 0 {
            2
        } else if toc & 0x60 == 0x60 {
            1
        } else {
            0
        };
        modes[m] += 1;
        codes[(toc & 3) as usize] += 1;
    }
    format!(
        "silk/hybrid/celt={}/{}/{} codes={:?}",
        modes[0], modes[1], modes[2], codes
    )
}
