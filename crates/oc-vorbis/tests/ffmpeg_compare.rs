//! Compares the decoder against ffmpeg's Vorbis decoder on ffmpeg-encoded
//! fixtures (libvorbis and ffmpeg's native encoder). Skipped when ffmpeg is
//! not installed.

#![allow(clippy::neg_cmp_op_on_partial_ord)]

mod common;

use common::*;

struct Case {
    name: String,
    rate: u32,
    channels: usize,
    secs: f64,
    args: Vec<String>,
}

fn libvorbis_case(rate: u32, channels: usize, q: i32) -> Case {
    Case {
        name: format!("lv_{rate}_{channels}ch_q{q}"),
        rate,
        channels,
        secs: 3.0,
        args: ["-c:a", "libvorbis", "-q:a", &q.to_string()]
            .iter()
            .map(|s| s.to_string())
            .collect(),
    }
}

/// Which decoder the SNR threshold applies to.
#[derive(Clone, Copy, PartialEq)]
enum Reference {
    /// ffmpeg's native float decoder.
    FfmpegNative,
    /// libvorbis through ffmpeg (16-bit output, so ~80 dB at most).
    Libvorbis,
}

fn run_cases(cases: &[Case], min_snr: f64, reference_kind: Reference) {
    let dir = temp_dir();
    let mut failures = Vec::new();
    for case in cases {
        let args: Vec<&str> = case.args.iter().map(|s| s.as_str()).collect();
        let Some(path) = make_fixture(&dir, &case.name, case.rate, case.channels, case.secs, &args)
        else {
            failures.push(format!("{}: fixture generation failed", case.name));
            continue;
        };
        let data = std::fs::read(&path).unwrap();
        let reference = ffmpeg_decode(&path, "vorbis").expect("ffmpeg decode");
        let dec = decode_ogg(&data);
        assert_eq!(dec.channels, case.channels);
        assert_eq!(dec.rate, case.rate);
        let (snr, max_err) = compare(&reference, &dec.samples);
        let lv = if has_decoder("libvorbis") {
            ffmpeg_decode(&path, "libvorbis")
        } else {
            None
        };
        let lv = lv.map(|r| vorbis_to_wave(&r, case.channels));
        let lv_snr = lv.map(|r| compare(&r, &dec.samples).0).unwrap_or(f64::NAN);
        let (bs0, bs1) = dec.decoder.block_sizes();
        println!(
            "{:<24} {:>6} bytes  blocks {}/{}  frames {:>7} (ref {:>7})  SNR {:>7.1} dB  max err {:.2e}  (vs libvorbis {:>6.1} dB)",
            case.name,
            data.len(),
            bs0,
            bs1,
            dec.samples.len() / dec.channels,
            reference.len() / dec.channels,
            snr,
            max_err,
            lv_snr
        );
        if reference.len() != dec.samples.len() {
            failures.push(format!(
                "{}: length {} vs reference {}",
                case.name,
                dec.samples.len(),
                reference.len()
            ));
        }
        let checked = if reference_kind == Reference::Libvorbis {
            lv_snr
        } else {
            snr
        };
        if !(checked >= min_snr) {
            failures.push(format!("{}: SNR {checked:.1} dB", case.name));
        }
        let _ = std::fs::remove_file(&path);
    }
    assert!(failures.is_empty(), "failures: {failures:#?}");
}

#[test]
fn libvorbis_quality_levels() {
    if !ffmpeg_available() || !has_encoder("libvorbis") {
        eprintln!("ffmpeg with libvorbis not available; skipping");
        return;
    }
    let cases: Vec<Case> = (0..=10).map(|q| libvorbis_case(44100, 2, q)).collect();
    run_cases(&cases, 90.0, Reference::FfmpegNative);
}

#[test]
fn libvorbis_rates_and_layouts() {
    if !ffmpeg_available() || !has_encoder("libvorbis") {
        eprintln!("ffmpeg with libvorbis not available; skipping");
        return;
    }
    let mut cases = Vec::new();
    for &rate in &[8000, 11025, 16000, 22050, 32000, 44100, 48000, 88200, 96000] {
        cases.push(libvorbis_case(rate, 1, 3));
    }
    for &(rate, q) in &[(8000, 0), (22050, 5), (48000, 8), (96000, 4)] {
        cases.push(libvorbis_case(rate, 2, q));
    }
    for ch in 3..=8 {
        cases.push(libvorbis_case(48000, ch, 4));
    }
    cases.push(libvorbis_case(44100, 6, 0));
    cases.push(libvorbis_case(96000, 6, 8));
    run_cases(&cases, 90.0, Reference::FfmpegNative);
}

#[test]
fn native_ffmpeg_encoder() {
    if !ffmpeg_available() || !has_encoder("vorbis") {
        eprintln!("ffmpeg native vorbis encoder not available; skipping");
        return;
    }
    // The native encoder only supports stereo. Its streams decode
    // differently in ffmpeg's own decoder and in libvorbis (the second,
    // coupled channel differs); this decoder follows libvorbis (verified at
    // ~135 dB against libvorbisfile's float output), so compare with the
    // libvorbis decoder, whose 16-bit output limits the SNR.
    if !has_decoder("libvorbis") {
        eprintln!("ffmpeg libvorbis decoder not available; skipping");
        return;
    }
    let mut cases = Vec::new();
    for &(rate, ch, q) in &[(44100, 2, 3), (22050, 2, 8), (48000, 2, 0), (32000, 2, 5)] {
        cases.push(Case {
            name: format!("ff_{rate}_{ch}ch_q{q}"),
            rate,
            channels: ch,
            secs: 3.0,
            args: ["-c:a", "vorbis", "-strict", "-2", "-q:a", &q.to_string()]
                .iter()
                .map(|s| s.to_string())
                .collect(),
        });
    }
    run_cases(&cases, 70.0, Reference::Libvorbis);
}
