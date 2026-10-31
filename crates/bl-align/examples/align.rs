//! Word times of a known text in a WAV file.
//!
//! ```text
//! cargo run --release -p bl-align --example align -- \
//!     MODEL_DIR FILE.wav "the text" [START END] [--f32] [--dump PREFIX]
//! ```
//!
//! The WAV file must be 16 kHz PCM, 16-bit (stereo is mixed down); START
//! and END (seconds) pick a stretch of it. `--f32` keeps the weights as
//! f32; `--dump` writes the logits (`PREFIX.logits.f32`) and the frames
//! of each token (`PREFIX.spans`) for comparing with other tools.

use bl_align::{ctc, Aligner, Keep};
use std::time::Instant;

/// The samples of a 16 kHz 16-bit PCM WAV file, as f32 mono.
fn read_wav(path: &str) -> Result<Vec<f32>, String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let mut at = 12;
    let mut channels = 0;
    while at + 8 <= b.len() {
        let id = &b[at..at + 4];
        let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
        let body = &b[at + 8..(at + 8 + len).min(b.len())];
        if id == b"fmt " && body.len() >= 16 {
            let format = u16::from_le_bytes([body[0], body[1]]);
            channels = u16::from_le_bytes([body[2], body[3]]) as usize;
            let rate = u32::from_le_bytes([body[4], body[5], body[6], body[7]]);
            let bits = u16::from_le_bytes([body[14], body[15]]);
            if format != 1 || bits != 16 || rate != bl_align::SAMPLE_RATE || channels == 0 {
                return Err("need 16 kHz 16-bit PCM".into());
            }
        } else if id == b"data" {
            if channels == 0 {
                return Err("no format before the data".into());
            }
            let frames = body.chunks_exact(2 * channels);
            return Ok(frames
                .map(|f| {
                    let sum: f32 = f
                        .chunks_exact(2)
                        .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32)
                        .sum();
                    sum / (channels as f32 * 32768.0)
                })
                .collect());
        }
        at += 8 + len + (len & 1);
    }
    Err("no data in the WAV file".into())
}

fn peak_memory() -> String {
    std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM"))
                .map(|l| l.split_whitespace().skip(1).collect::<Vec<_>>().join(" "))
        })
        .unwrap_or_default()
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let keep = if let Some(i) = args.iter().position(|a| a == "--f32") {
        args.remove(i);
        Keep::F32
    } else {
        Keep::F16
    };
    let dump = args.iter().position(|a| a == "--dump").map(|i| {
        args.remove(i);
        args.remove(i)
    });
    if args.len() != 3 && args.len() != 5 {
        eprintln!("usage: align MODEL_DIR FILE.wav TEXT [START END] [--f32] [--dump PREFIX]");
        std::process::exit(2);
    }
    let t0 = Instant::now();
    let aligner = Aligner::load_with(&args[0], keep).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    println!(
        "loaded in {:.2} s, weights {:.0} MB",
        t0.elapsed().as_secs_f64(),
        aligner.weight_bytes() as f64 / 1e6
    );
    let all = read_wav(&args[1]).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let (start, end) = if args.len() == 5 {
        let s: f64 = args[3].parse().expect("START");
        let e: f64 = args[4].parse().expect("END");
        (s, e)
    } else {
        (0.0, all.len() as f64 / 16_000.0)
    };
    let a = ((start * 16_000.0) as usize).min(all.len());
    let b = ((end * 16_000.0) as usize).clamp(a, all.len());
    let samples = &all[a..b];
    let text = &args[2];

    let t1 = Instant::now();
    let logits = aligner.logits(samples);
    let secs = t1.elapsed().as_secs_f64();
    let c = aligner.config();
    let lp = bl_align::LogProbs::from_logits(&logits, c.vocab_size, c.frame_seconds());
    println!(
        "{:.2} s of audio, {} frames: {:.2} s ({:.2} s per 10 s of audio)",
        samples.len() as f64 / 16_000.0,
        lp.frames,
        secs,
        secs * 160_000.0 / samples.len().max(1) as f64
    );
    println!("hears: {}", aligner.hears(&lp));
    let v = aligner.vocab();
    let target = v.target(&v.words(text));
    println!("target: {}", v.show(&target.tokens));
    let t2 = Instant::now();
    match aligner.align_log_probs(&lp, text) {
        Ok(words) => {
            println!("aligned in {:.1} ms", t2.elapsed().as_secs_f64() * 1e3);
            for w in &words {
                println!(
                    "{:8.3} {:8.3}  {:5.3}  {}",
                    start + w.start,
                    start + w.end,
                    w.score,
                    w.text
                );
            }
        }
        Err(e) => println!("{e}"),
    }
    println!("peak memory {}", peak_memory());
    if let Some(prefix) = dump {
        let bytes: Vec<u8> = logits.iter().flat_map(|v| v.to_le_bytes()).collect();
        std::fs::write(format!("{prefix}.logits.f32"), bytes).expect("write logits");
        if let Ok(spans) = ctc::token_spans(&lp, &target.tokens, c.blank as u32) {
            let lines: String = spans
                .iter()
                .zip(&target.tokens)
                .map(|(s, &t)| format!("{} {} {}\n", v.texts[t as usize], s.start, s.end))
                .collect();
            std::fs::write(format!("{prefix}.spans"), lines).expect("write spans");
        }
    }
}
