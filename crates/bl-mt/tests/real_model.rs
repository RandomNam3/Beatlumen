//! Translation with a real OPUS-MT model (opus-mt-en-de) from the
//! directory in `BL_MT_MODEL_DIR`. Run with
//! `cargo test -p bl-mt --release -- --ignored --nocapture`.

use bl_mt::marian::sinusoidal;
use bl_mt::torch::{Checkpoint, Keep};
use bl_mt::{Search, Translator};
use std::path::PathBuf;
use std::time::Instant;

fn model_dir() -> PathBuf {
    std::env::var_os("BL_MT_MODEL_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(
                "/tmp/claude-0/-home-user/1d9406f4-cd4e-59be-8af8-5565f563a899/scratchpad/mt/en-de",
            )
        })
}

#[test]
#[ignore]
fn stored_positions_match_the_computed_ones() {
    let ckpt = Checkpoint::open(model_dir().join("pytorch_model.bin")).unwrap();
    let name = "model.encoder.embed_positions.weight";
    let Some(t) = ckpt.get(name) else { return };
    let stored = ckpt
        .load(&[(name, Keep::F32)])
        .unwrap()
        .remove(name)
        .unwrap();
    let computed = sinusoidal(t.shape[0], t.shape[1]);
    let worst = stored
        .values()
        .iter()
        .zip(&computed)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    assert!(worst < 1e-4, "largest difference {worst}");
}

#[test]
#[ignore]
fn translates_english_to_german() {
    let t0 = Instant::now();
    let t = Translator::load(model_dir()).unwrap();
    println!(
        "loaded in {:.2} s, weights {:.1} MB",
        t0.elapsed().as_secs_f64(),
        t.weight_bytes() as f64 / 1e6
    );
    // Peak and current memory of the process (Linux).
    if let Ok(s) = std::fs::read_to_string("/proc/self/status") {
        for l in s
            .lines()
            .filter(|l| l.starts_with("VmHWM") || l.starts_with("VmRSS"))
        {
            println!("{l}");
        }
    }
    let lines = [
        "Who let the dogs out?",
        "I love you more than words can say.",
        "The weather is nice today.",
        "Haven't I made it clear?",
        "",
        "We're only friends.",
    ];
    for search in [Search::Beam(4), Search::Greedy] {
        for line in lines {
            let t0 = Instant::now();
            let out = t.translate_line(line, search);
            println!(
                "{search:?} {:.3} s: {line:?} -> {out:?}",
                t0.elapsed().as_secs_f64()
            );
            if line.is_empty() {
                assert!(out.is_empty());
            } else {
                assert!(!out.is_empty());
            }
        }
    }
    let out = t.translate(&lines);
    assert_eq!(out.len(), lines.len());
    assert!(out[0].contains("Hunde"), "{}", out[0]);
    assert!(out[1].contains("liebe"), "{}", out[1]);
    assert!(out[2].contains("Wetter"), "{}", out[2]);
    assert!(out[4].is_empty());
}
