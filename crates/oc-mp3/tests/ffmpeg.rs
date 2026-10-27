//! Compares the decoder against ffmpeg's float decoder on ffmpeg-generated
//! fixtures (skipped when ffmpeg is not installed).

mod common;

use common::*;

const STEREO_SRC: [&str; 6] = ["-f", "lavfi", "-i", "sine=f=440:d=3", "-f", "lavfi"];

fn stereo_args<'a>(noise: &'a str, out: &[&'a str]) -> Vec<&'a str> {
    let mut v: Vec<&str> = STEREO_SRC.to_vec();
    v.extend_from_slice(&[
        "-i",
        noise,
        "-filter_complex",
        "[0:a][1:a]amerge=inputs=2[a]",
        "-map",
        "[a]",
    ]);
    v.extend_from_slice(out);
    v
}

/// Decodes `name` with both decoders and checks SNR and sample count.
fn check(name: &str, args: &[&str], min_snr: f64, exact_len: bool) {
    check_with(name, args, min_snr, exact_len, &["-c:a", "mp3float"]);
}

/// Like [`check`] with explicit ffmpeg input options (decoder choice).
fn check_with(name: &str, args: &[&str], min_snr: f64, exact_len: bool, dec_args: &[&str]) {
    let Some(path) = gen(name, args) else { return };
    let ours = decode_file(&path);
    let reference = ffmpeg_decode(&path, dec_args);
    let (snr, max) = snr(&reference, &ours.pcm);
    let ch = ours.channels as usize;
    println!(
        "{name}: {} Hz {} ch, ours {} frames, ffmpeg {} frames, total_frames() {:?}, SNR {snr:.1} dB, max err {max:.2e}",
        ours.sample_rate,
        ch,
        ours.pcm.len() / ch,
        reference.len() / ch,
        ours.total_frames
    );
    assert!(snr > min_snr, "{name}: SNR {snr:.1} dB below {min_snr} dB");
    if exact_len {
        assert_eq!(
            ours.pcm.len(),
            reference.len(),
            "{name}: sample count differs from ffmpeg"
        );
        assert_eq!(
            ours.total_frames,
            Some((ours.pcm.len() / ch) as u64),
            "{name}: total_frames"
        );
    } else {
        let diff = (ours.pcm.len() as i64 - reference.len() as i64).abs();
        assert!(
            diff <= (1152 * ch) as i64,
            "{name}: length {} vs {}",
            ours.pcm.len(),
            reference.len()
        );
    }
}

#[test]
fn cbr_128k_stereo() {
    let args = stereo_args(
        "anoisesrc=d=3:a=0.3",
        &["-c:a", "libmp3lame", "-b:a", "128k", "-joint_stereo", "0"],
    );
    check("cbr128_stereo.mp3", &args, 80.0, true);
}

#[test]
fn vbr_q2() {
    let args = stereo_args("anoisesrc=d=3:a=0.3", &["-c:a", "libmp3lame", "-q:a", "2"]);
    check("vbr_q2.mp3", &args, 80.0, true);
}

#[test]
fn joint_stereo() {
    // Correlated channels so LAME picks M/S stereo for most frames.
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:d=3",
        "-f",
        "lavfi",
        "-i",
        "anoisesrc=d=3:a=0.05",
        "-filter_complex",
        "[0:a][1:a]amix=inputs=2:normalize=0,asplit[x][y];[y]volume=0.8[z];[x][z]amerge=inputs=2[a]",
        "-map",
        "[a]",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "96k",
        "-joint_stereo",
        "1",
    ];
    check("joint_stereo.mp3", &args, 80.0, true);
}

#[test]
fn mono_64k() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=1000:d=3",
        "-ac",
        "1",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "64k",
    ];
    check("mono64.mp3", &args, 80.0, true);
}

#[test]
fn mpeg2_22khz() {
    let args = stereo_args(
        "anoisesrc=d=3:a=0.3",
        &["-ar", "22050", "-c:a", "libmp3lame", "-b:a", "64k"],
    );
    check("mpeg2_22k.mp3", &args, 80.0, true);
}

#[test]
fn mpeg25_11khz() {
    let args = stereo_args(
        "anoisesrc=d=3:a=0.3",
        &["-ar", "11025", "-c:a", "libmp3lame", "-b:a", "32k"],
    );
    check("mpeg25_11k.mp3", &args, 80.0, true);
}

#[test]
fn mpeg25_8khz_mono_vbr() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=300:d=3",
        "-ar",
        "8000",
        "-ac",
        "1",
        "-c:a",
        "libmp3lame",
        "-q:a",
        "6",
    ];
    check("mpeg25_8k.mp3", &args, 80.0, true);
}

#[test]
fn layer2_stereo() {
    let args = stereo_args("anoisesrc=d=3:a=0.3", &["-c:a", "mp2", "-b:a", "192k"]);
    check_with("layer2.mp2", &args, 80.0, true, &["-c:a", "mp2float"]);
}

#[test]
fn layer2_lsf_mono() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=500:d=3",
        "-ar",
        "24000",
        "-ac",
        "1",
        "-c:a",
        "mp2",
        "-b:a",
        "48k",
    ];
    check_with("layer2_lsf.mp2", &args, 80.0, true, &["-c:a", "mp2float"]);
}

#[test]
fn shine_encoder() {
    let args = stereo_args("anoisesrc=d=3:a=0.3", &["-c:a", "libshine", "-b:a", "128k"]);
    check("shine.mp3", &args, 80.0, true);
}

/// Prints decoding speed (x real time); asserts a floor in release builds.
#[test]
fn decode_speed() {
    let cases: [(&str, &[&str]); 3] = [
        ("speed_l3_192k.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"]),
        ("speed_l3_vbr.mp3", &["-c:a", "libmp3lame", "-q:a", "0"]),
        ("speed_l2_192k.mp2", &["-c:a", "mp2", "-b:a", "192k"]),
    ];
    for (name, codec) in cases {
        let mut args = vec![
            "-f",
            "lavfi",
            "-i",
            "sine=f=440:d=60",
            "-f",
            "lavfi",
            "-i",
            "anoisesrc=d=60:a=0.3",
            "-filter_complex",
            "[0:a][1:a]amerge=inputs=2[a]",
            "-map",
            "[a]",
            "-ar",
            "44100",
        ];
        args.extend_from_slice(codec);
        let Some(path) = gen(name, &args) else { return };
        let data = std::fs::read(&path).unwrap();
        let mut best = f64::MAX;
        let mut frames = 0;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let d = decode_bytes(&data);
            best = best.min(t.elapsed().as_secs_f64());
            frames = d.pcm.len() / 2;
        }
        let speed = frames as f64 / 44100.0 / best;
        println!(
            "{name}: {:.1} s of audio decoded at {speed:.0}x real time",
            frames as f64 / 44100.0
        );
        if !cfg!(debug_assertions) {
            assert!(speed > 50.0, "{name}: only {speed:.0}x real time");
        }
    }
}
