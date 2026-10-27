//! Decode speed report (release builds are expected to be far faster than
//! real time). Prints realtime factors; asserts only a loose lower bound.

mod common;
use common::synth::{generate, Synth};
use common::*;
use std::process::Command;
use std::time::Instant;

fn measure(name: &str, data: &[u8]) -> f64 {
    // Warm up (table initialisation), then time a full decode.
    let _ = decode_adts(&data[..data.len().min(4096)]);
    let t0 = Instant::now();
    let (pcm, rate, ch) = decode_adts(data).expect("decode");
    let dt = t0.elapsed().as_secs_f64();
    let secs = pcm.len() as f64 / ch as f64 / rate as f64;
    let factor = secs / dt;
    eprintln!("{name:>34}: {secs:6.1} s of audio in {:7.1} ms -> {factor:6.0}x realtime", dt * 1000.0);
    factor
}

#[test]
fn decode_speed() {
    let min = if cfg!(debug_assertions) { 1.0 } else { 20.0 };
    let he = generate(&Synth { stereo: true, ps: false, ps_type_b: false, frames: 1300 });
    assert!(measure("HE-AAC v1 stereo 44.1 kHz (synth)", &he) > min);
    let he2 = generate(&Synth { stereo: false, ps: true, ps_type_b: false, frames: 1300 });
    assert!(measure("HE-AAC v2 (PS) 44.1 kHz (synth)", &he2) > min);
    if !ffmpeg_available() {
        eprintln!("ffmpeg not found; skipping LC speed measurement");
        return;
    }
    let dir = temp_dir("speed");
    for (rate, ch, br) in [(44100u32, 2usize, "128k"), (48000, 6, "384k")] {
        let wav = dir.join(format!("s{rate}_{ch}.wav"));
        let aac = dir.join(format!("s{rate}_{ch}.aac"));
        assert!(make_wav(&wav, rate, ch, 30.0));
        assert!(run(Command::new("ffmpeg")
            .args(["-y", "-hide_banner", "-loglevel", "error", "-i"])
            .arg(&wav)
            .args(["-c:a", "aac", "-b:a", br, "-f", "adts"])
            .arg(&aac)));
        let data = std::fs::read(&aac).unwrap();
        assert!(measure(&format!("AAC-LC {ch} ch {} kHz {br}", rate / 1000), &data) > min);
    }
}
