//! Runs the effect engine over a track and prints when audio triggered
//! filters rise, to check their timing against the music.
//!
//! Usage: `trigger_timing in.f32 preset.hfx [fps]`: `in.f32` is raw mono
//! f32le audio at 48 kHz. Output: one line per rendered frame,
//! `time filter=level ...` (time = audible playback position).

use bl_fx::{Engine, FrameInput, Preset, Rng};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).expect("read audio");
    let preset = std::fs::read(&args[2]).expect("read preset");
    let fps: f64 = args.get(3).map_or(60.0, |f| f.parse().expect("fps"));
    let mono: Vec<f32> = data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let preset = Preset::parse(&preset, "test", &mut Rng::new(1)).expect("preset");
    let analysis = bl_analysis::analyze(&mono, 48000, &preset.analysis_config(), &mut |_| true)
        .expect("analysis");
    let mut engine = Engine::new(1);
    engine.set_preset(Some(preset));
    let dt = 1.0 / fps;
    let mut t = 0.0;
    while t < mono.len() as f64 / 48000.0 {
        let input = FrameInput {
            dt,
            playing: true,
            position: t,
            analysis: Some(&analysis),
            live_novelty: [0.0; 3],
            live_energy: 0.0,
            view_aspect: 16.0 / 9.0,
            menu_blur: 0.0,
        };
        engine.update(&input);
        let levels: Vec<String> = engine
            .filter_levels()
            .iter()
            .map(|(n, v)| format!("{n}={v:.4}"))
            .collect();
        println!("{t:.4} {}", levels.join(" "));
        t += dt;
    }
}
