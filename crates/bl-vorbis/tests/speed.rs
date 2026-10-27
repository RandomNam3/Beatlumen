//! Decode speed report (run with `--release --nocapture`).

mod common;

use common::*;
use bl_vorbis::VorbisDecoder;
use std::time::Instant;

#[test]
fn decode_speed() {
    if !ffmpeg_available() || !has_encoder("libvorbis") {
        eprintln!("ffmpeg with libvorbis not available; skipping");
        return;
    }
    let dir = temp_dir();
    for &(name, rate, ch, q, secs) in &[
        ("speed_stereo", 44100u32, 2usize, "5", 60.0),
        ("speed_51", 48000, 6, "4", 30.0),
    ] {
        let Some(path) = make_fixture(
            &dir,
            name,
            rate,
            ch,
            secs,
            &["-c:a", "libvorbis", "-q:a", q],
        ) else {
            panic!("fixture generation failed");
        };
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        let (packets, _) = read_packets(&data);
        let mut best = f64::INFINITY;
        let mut frames = 0;
        for _ in 0..3 {
            let t = Instant::now();
            let mut dec = VorbisDecoder::new(&packets[0], &packets[1], &packets[2]).unwrap();
            let mut out = Vec::with_capacity((secs as usize + 1) * rate as usize * ch);
            frames = 0;
            for p in &packets[3..] {
                frames += dec.decode_packet(p, &mut out).unwrap();
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        let audio_secs = frames as f64 / rate as f64;
        let factor = audio_secs / best;
        println!(
            "{name}: {audio_secs:.1} s of {ch}ch {rate} Hz audio ({} kbit/s) decoded in {:.1} ms = {factor:.0}x real time",
            data.len() * 8 / (audio_secs as usize * 1000),
            best * 1000.0
        );
        if !cfg!(debug_assertions) {
            assert!(factor > 20.0, "decoding too slow: {factor:.1}x real time");
        }
    }
}
