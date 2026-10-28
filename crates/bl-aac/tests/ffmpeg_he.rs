//! HE-AAC (SBR) and HE-AAC v2 (PS) against ffmpeg's decoder, using
//! synthetic ADTS streams (implicit SBR signalling) built by
//! `common::synth` — no HE-AAC encoder is needed. Skips when ffmpeg is
//! missing.

mod common;
use common::synth::{generate, Synth};
use common::*;

fn run(name: &str, s: &Synth, min_snr: f64) {
    let dir = temp_dir("he");
    let path = dir.join(format!("{name}.aac"));
    let data = generate(s);
    std::fs::write(&path, &data).unwrap();
    let reference = ffmpeg_decode(&path).expect("ffmpeg failed to decode synthetic stream");
    let (ours, rate, channels) = decode_adts(&data).expect("decode failed");
    assert_eq!(rate, 44100, "{name}: SBR output rate");
    let expect_ch = if s.stereo || s.ps { 2 } else { 2 }; // mono SBR is upmixed (PS)
    assert_eq!(channels as usize, expect_ch, "{name}: channels");
    let cmp = compare(&ours, &reference, channels as usize, 4096);
    eprintln!(
        "{name:>14}: SNR {:7.2} dB, max err {:.2e}, offset {}, {} frames",
        cmp.snr_db, cmp.max_abs_err, cmp.offset, cmp.compared_frames
    );
    assert_eq!(ours.len(), s.frames * 2048 * channels as usize);
    assert!(cmp.compared_frames > s.frames * 2048 * 9 / 10);
    assert!(cmp.snr_db > min_snr, "{name}: SNR {} dB", cmp.snr_db);
}

#[test]
fn he_aac_matches_ffmpeg() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    run(
        "sbr_mono",
        &Synth {
            stereo: false,
            ps: false,
            ps_type_b: false,
            frames: 120,
        },
        70.0,
    );
    run(
        "sbr_stereo",
        &Synth {
            stereo: true,
            ps: false,
            ps_type_b: false,
            frames: 120,
        },
        70.0,
    );
    run(
        "ps_mix_a",
        &Synth {
            stereo: false,
            ps: true,
            ps_type_b: false,
            frames: 120,
        },
        70.0,
    );
    run(
        "ps_mix_b",
        &Synth {
            stereo: false,
            ps: true,
            ps_type_b: true,
            frames: 120,
        },
        70.0,
    );
}
