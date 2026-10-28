//! Prints the novelty curves of a track for timing checks.
//!
//! Usage: `novelty_dump in.f32 [rate]`: `in.f32` is raw mono f32le audio
//! (e.g. `ffmpeg -i song -ac 1 -ar 48000 -f f32le in.f32`). Output: one
//! line per analysis frame, `time low mid high` (time = frame start).

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data =
        std::fs::read(args.get(1).expect("usage: novelty_dump in.f32 [rate]")).expect("read");
    let rate: u32 = args.get(2).map_or(48000, |r| r.parse().expect("rate"));
    let mono: Vec<f32> = data
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect();
    let cfg = bl_analysis::AnalysisConfig::default();
    let a = bl_analysis::analyze(&mono, rate, &cfg, &mut |_| true).expect("analysis");
    eprintln!("hop {} frame rate {:.2}", a.hop_size, a.frame_rate());
    for i in 0..a.frame_count() {
        let t = i as f64 / a.frame_rate();
        let v: Vec<String> = a.novelty.iter().map(|c| format!("{:.4}", c[i])).collect();
        println!("{t:.4} {}", v.join(" "));
    }
}
