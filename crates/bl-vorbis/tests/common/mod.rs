//! Shared helpers for the integration tests: ffmpeg fixture generation,
//! Ogg demuxing and signal comparison.

#![allow(dead_code)]

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use bl_vorbis::VorbisDecoder;

pub fn ffmpeg_available() -> bool {
    Command::new("ffmpeg")
        .arg("-version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn has_encoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-encoders"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(name))
        })
        .unwrap_or(false)
}

pub fn has_decoder(name: &str) -> bool {
    Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .map(|o| {
            String::from_utf8_lossy(&o.stdout)
                .lines()
                .any(|l| l.split_whitespace().nth(1) == Some(name))
        })
        .unwrap_or(false)
}

pub fn temp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("bl-vorbis-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// ffmpeg `aevalsrc` expression for channel `c`: tones, a chirp, noise and
/// periodic bursts (the transients make the encoder switch block sizes).
pub fn channel_expr(c: usize) -> String {
    format!(
        "0.25*sin(2*PI*{f}*t)+0.1*sin(2*PI*2000*t*t)+0.04*(random({c})-0.5)\
         +0.4*sin(2*PI*{g}*t)*lt(mod(t+{o},0.37),0.006)",
        f = 220 + 97 * c,
        g = 1500 + 300 * c,
        o = c as f64 * 0.05,
    )
}

/// Encodes a synthetic signal with ffmpeg and returns the .ogg path.
pub fn make_fixture(
    dir: &Path,
    name: &str,
    rate: u32,
    channels: usize,
    secs: f64,
    enc_args: &[&str],
) -> Option<PathBuf> {
    let expr: Vec<String> = (0..channels).map(channel_expr).collect();
    let src = format!("aevalsrc='{}':s={}:d={}", expr.join("|"), rate, secs);
    let out = dir.join(format!("{name}.ogg"));
    let mut cmd = Command::new("ffmpeg");
    cmd.args([
        "-hide_banner",
        "-loglevel",
        "error",
        "-y",
        "-f",
        "lavfi",
        "-i",
        &src,
    ]);
    cmd.args(enc_args);
    cmd.arg(&out);
    let o = cmd.output().ok()?;
    if !o.status.success() {
        eprintln!(
            "ffmpeg encode failed for {name}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        return None;
    }
    Some(out)
}

/// Decodes `path` with ffmpeg (decoder `codec`) to interleaved f32.
pub fn ffmpeg_decode(path: &Path, codec: &str) -> Option<Vec<f32>> {
    let o = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-c:a", codec, "-i"])
        .arg(path)
        .args(["-f", "f32le", "-c:a", "pcm_f32le", "-"])
        .output()
        .ok()?;
    if !o.status.success() {
        eprintln!(
            "ffmpeg decode failed: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        return None;
    }
    Some(
        o.stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect(),
    )
}

/// Reorders interleaved samples from Vorbis channel order (as output by
/// ffmpeg's libvorbis wrapper) to WAVE order (as output by this crate).
pub fn vorbis_to_wave(samples: &[f32], ch: usize) -> Vec<f32> {
    let remap: &[usize] = match ch {
        3 => &[0, 2, 1],
        5 => &[0, 2, 1, 3, 4],
        6 => &[0, 2, 1, 4, 5, 3],
        7 => &[0, 2, 1, 5, 6, 4, 3],
        8 => &[0, 2, 1, 6, 7, 4, 5, 3],
        _ => return samples.to_vec(),
    };
    let mut out = vec![0f32; samples.len()];
    for (src, dst) in samples.chunks_exact(ch).zip(out.chunks_exact_mut(ch)) {
        for (c, &d) in remap.iter().enumerate() {
            dst[d] = src[c];
        }
    }
    out
}

/// All packets of the first logical stream plus the final granule position.
pub fn read_packets(data: &[u8]) -> (Vec<Vec<u8>>, Option<u64>) {
    let mut r = bl_ogg::PacketReader::new(Cursor::new(data));
    let mut packets = Vec::new();
    let mut serial = None;
    let mut last_granule = None;
    while let Ok(Some(p)) = r.next_packet() {
        if *serial.get_or_insert(p.serial) != p.serial {
            continue;
        }
        if p.granule.is_some() {
            last_granule = p.granule;
        }
        packets.push(p.data);
    }
    (packets, last_granule)
}

pub struct Decoded {
    pub samples: Vec<f32>,
    pub channels: usize,
    pub rate: u32,
    pub decoder: VorbisDecoder,
}

/// Decodes an Ogg Vorbis file, trimming the end to the final granule.
pub fn decode_ogg(data: &[u8]) -> Decoded {
    let (packets, last_granule) = read_packets(data);
    assert!(packets.len() >= 3, "missing header packets");
    let mut dec = VorbisDecoder::new(&packets[0], &packets[1], &packets[2]).expect("header parse");
    let ch = dec.channels() as usize;
    let mut out = Vec::new();
    for p in &packets[3..] {
        let before = out.len();
        let expected = bl_vorbis::packet_block_size(&dec, p);
        let n = dec.decode_packet(p, &mut out).expect("decode");
        assert_eq!(out.len() - before, n * ch);
        assert_eq!(expected, Some(n));
    }
    if let Some(g) = last_granule {
        let g = g as usize * ch;
        if out.len() > g {
            out.truncate(g);
        }
    }
    let rate = dec.sample_rate();
    Decoded {
        samples: out,
        channels: ch,
        rate,
        decoder: dec,
    }
}

/// Returns (SNR in dB, max abs error) of `test` against `reference`.
pub fn compare(reference: &[f32], test: &[f32]) -> (f64, f64) {
    let n = reference.len().min(test.len());
    let mut sig = 0f64;
    let mut err = 0f64;
    let mut max_err = 0f64;
    for i in 0..n {
        let r = reference[i] as f64;
        let d = r - test[i] as f64;
        sig += r * r;
        err += d * d;
        max_err = max_err.max(d.abs());
    }
    let snr = if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    };
    (snr, max_err)
}
