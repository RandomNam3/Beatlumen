//! AAC-LC conformance against ffmpeg's float decoder, using streams produced
//! by ffmpeg's native AAC encoder (mono / stereo / 5.1, 8-96 kHz,
//! 32-320 kbit/s). Skips (passes) when ffmpeg is not installed.

mod common;
use common::*;
use std::process::Command;

struct Case {
    rate: u32,
    channels: usize,
    bitrate: u32,
    extra: &'static [&'static str],
    min_snr: f64,
}

fn run_case(c: &Case) -> Option<f64> {
    let dir = temp_dir("lc");
    let tag = format!("{}_{}_{}_{}", c.rate, c.channels, c.bitrate, c.extra.join("").replace(['-', ':'], ""));
    let wav = dir.join(format!("{tag}.wav"));
    let aac = dir.join(format!("{tag}.aac"));
    assert!(make_wav(&wav, c.rate, c.channels, 3.0), "fixture generation failed");
    let mut cmd = Command::new("ffmpeg");
    cmd.args(["-y", "-hide_banner", "-loglevel", "error", "-i"]).arg(&wav).args(["-c:a", "aac", "-b:a"]).arg(format!("{}", c.bitrate));
    cmd.args(c.extra).args(["-f", "adts"]).arg(&aac);
    assert!(run(&mut cmd), "encoding failed");
    let data = std::fs::read(&aac).unwrap();
    let reference = ffmpeg_decode(&aac).expect("reference decode failed");
    let t0 = std::time::Instant::now();
    let (ours, rate, channels) = decode_adts(&data).expect("decode failed");
    let dt = t0.elapsed().as_secs_f64();
    assert_eq!(rate, c.rate);
    assert_eq!(channels as usize, c.channels);
    let cmp = compare(&ours, &reference, c.channels, 2048);
    let secs = ours.len() as f64 / c.channels as f64 / c.rate as f64;
    eprintln!(
        "{tag:>28}: SNR {:7.2} dB, max err {:.2e}, offset {}, {} frames, {:.0}x realtime",
        cmp.snr_db,
        cmp.max_abs_err,
        cmp.offset,
        cmp.compared_frames,
        secs / dt
    );
    assert!(cmp.compared_frames as f64 > 0.9 * reference.len() as f64 / c.channels as f64);
    assert!(cmp.snr_db > c.min_snr, "{tag}: SNR {} dB below {}", cmp.snr_db, c.min_snr);
    Some(cmp.snr_db)
}

#[test]
fn lc_matches_ffmpeg() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found; skipping");
        return;
    }
    const NO_PNS: &[&str] = &["-aac_pns", "0"];
    let cases = [
        Case { rate: 44100, channels: 2, bitrate: 128_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 48000, channels: 1, bitrate: 64_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 48000, channels: 2, bitrate: 320_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 32000, channels: 2, bitrate: 96_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 22050, channels: 2, bitrate: 48_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 16000, channels: 1, bitrate: 32_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 8000, channels: 1, bitrate: 32_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 11025, channels: 2, bitrate: 32_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 12000, channels: 1, bitrate: 32_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 24000, channels: 2, bitrate: 64_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 64000, channels: 2, bitrate: 192_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 88200, channels: 2, bitrate: 256_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 96000, channels: 2, bitrate: 320_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 48000, channels: 6, bitrate: 320_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 48000, channels: 8, bitrate: 512_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 44100, channels: 3, bitrate: 160_000, extra: NO_PNS, min_snr: 70.0 },
        Case { rate: 44100, channels: 2, bitrate: 32_000, extra: NO_PNS, min_snr: 70.0 },
        // Intensity stereo and M/S forced on, TNS off.
        Case { rate: 44100, channels: 2, bitrate: 64_000, extra: &["-aac_pns", "0", "-aac_is", "1", "-aac_ms", "1", "-aac_tns", "0"], min_snr: 70.0 },
    ];
    for c in &cases {
        run_case(c);
    }
}
