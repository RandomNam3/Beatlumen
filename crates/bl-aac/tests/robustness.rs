//! Corrupted / truncated input must never panic (and must not hang).
//! Uses synthetic HE-AAC streams (no external tools needed) and, when
//! ffmpeg is available, ffmpeg-encoded AAC-LC streams with all LC tools.

mod common;
use common::synth::{generate, Synth};
use common::*;
use bl_aac::{AacDecoder, AdtsHeader};
use std::process::Command;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        self.0 >> 33
    }
}

/// Decodes every frame after applying a deterministic mutation; returns
/// (frames decoded ok, frames with errors).
fn fuzz_stream(data: &[u8], seed: u64, rounds: usize) -> (usize, usize) {
    let frames = adts_frames(data);
    let h = AdtsHeader::parse(frames[0]).unwrap();
    let mut rng = Lcg(seed);
    let (mut ok, mut err) = (0, 0);
    for round in 0..rounds {
        let mut dec = AacDecoder::new(&h.audio_specific_config()).unwrap();
        let mut out = Vec::new();
        for (i, f) in frames.iter().enumerate() {
            let payload = &f[h.header_length..];
            let mut buf = payload.to_vec();
            match (round + i) % 5 {
                0 => {
                    // Flip a few random bits.
                    for _ in 0..1 + rng.next() % 4 {
                        if buf.is_empty() {
                            break;
                        }
                        let p = (rng.next() as usize) % buf.len();
                        buf[p] ^= 1 << (rng.next() % 8);
                    }
                }
                1 => {
                    // Truncate.
                    let n = (rng.next() as usize) % (buf.len() + 1);
                    buf.truncate(n);
                }
                2 => {
                    // Random garbage of random length.
                    let n = (rng.next() as usize) % 64;
                    buf = (0..n).map(|_| rng.next() as u8).collect();
                }
                3 => {
                    // Overwrite a random byte range.
                    if !buf.is_empty() {
                        let p = (rng.next() as usize) % buf.len();
                        let n = ((rng.next() as usize) % 8).min(buf.len() - p);
                        for b in &mut buf[p..p + n] {
                            *b = rng.next() as u8;
                        }
                    }
                }
                _ => {} // intact frame (decoder must recover)
            }
            let before = out.len();
            match dec.decode_frame(&buf, &mut out) {
                Ok(n) => {
                    ok += 1;
                    assert_eq!(out.len() - before, n * dec.channels() as usize);
                    assert!(out[before..].iter().all(|v| v.is_finite() && v.abs() <= 1.0));
                }
                Err(_) => err += 1,
            }
        }
    }
    (ok, err)
}

#[test]
fn corrupted_he_aac_does_not_panic() {
    for (i, s) in [
        Synth { stereo: false, ps: false, ps_type_b: false, frames: 60 },
        Synth { stereo: true, ps: false, ps_type_b: false, frames: 60 },
        Synth { stereo: false, ps: true, ps_type_b: true, frames: 60 },
    ]
    .iter()
    .enumerate()
    {
        let data = generate(s);
        let (ok, err) = fuzz_stream(&data, 0x5eed + i as u64, 6);
        eprintln!("synthetic stream {i}: {ok} frames decoded, {err} rejected");
        assert!(ok > 0);
    }
}

#[test]
fn corrupted_lc_does_not_panic() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found; skipping LC part");
        return;
    }
    let dir = temp_dir("fuzz");
    for (rate, ch, br) in [(44100u32, 2usize, "96k"), (48000, 6, "256k"), (16000, 1, "24k")] {
        let wav = dir.join(format!("f{rate}_{ch}.wav"));
        let aac = dir.join(format!("f{rate}_{ch}.aac"));
        assert!(make_wav(&wav, rate, ch, 1.5));
        assert!(run(Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&wav)
            .args(["-c:a", "aac", "-b:a", br, "-aac_is", "1", "-aac_pns", "1", "-f", "adts"])
            .arg(&aac)));
        let data = std::fs::read(&aac).unwrap();
        let (ok, err) = fuzz_stream(&data, rate as u64, 4);
        eprintln!("LC {rate} Hz {ch} ch: {ok} frames decoded, {err} rejected");
        assert!(ok > 0);
    }
}

#[test]
fn garbage_configs_are_rejected_without_panic() {
    let mut rng = Lcg(42);
    for len in 0..64usize {
        for _ in 0..50 {
            let asc: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            if let Ok(mut dec) = AacDecoder::new(&asc) {
                let mut out = Vec::new();
                for _ in 0..4 {
                    let n = (rng.next() % 300) as usize;
                    let pkt: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
                    let _ = dec.decode_frame(&pkt, &mut out);
                }
            }
        }
    }
}
