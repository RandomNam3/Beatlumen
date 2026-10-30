//! Alignment with real models: the English base model from the directory
//! in `BL_ALIGN_MODEL_DIR` (with `BL_ALIGN_WAV`, a 16 kHz recording of
//! "And so, my fellow Americans, ask not what your country can do for
//! you, ask what you can do for your country.", e.g. whisper.cpp's
//! `jfk.wav`), and the German model in `BL_ALIGN_MODEL_DIR_DE`. Each test
//! passes without doing anything when its files are missing. Run with
//! `cargo test -p bl-align --release -- --nocapture`.

use bl_align::{Aligner, Keep};
use std::path::PathBuf;
use std::time::Instant;

fn path(var: &str) -> Option<PathBuf> {
    let p = PathBuf::from(std::env::var_os(var)?);
    if p.exists() {
        Some(p)
    } else {
        println!("skipped: {} does not exist", p.display());
        None
    }
}

/// The samples of a 16 kHz mono 16-bit WAV file (chunks before the data
/// skipped).
fn read_wav(p: &PathBuf) -> Vec<f32> {
    let b = std::fs::read(p).unwrap();
    let mut at = 12;
    while at + 8 <= b.len() {
        let len = u32::from_le_bytes(b[at + 4..at + 8].try_into().unwrap()) as usize;
        if &b[at..at + 4] == b"data" {
            let end = (at + 8 + len).min(b.len());
            return b[at + 8..end]
                .chunks_exact(2)
                .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                .collect();
        }
        at += 8 + len + (len & 1);
    }
    panic!("no data chunk");
}

#[test]
fn aligns_jfk_with_the_base_model() {
    let (Some(dir), Some(wav)) = (path("BL_ALIGN_MODEL_DIR"), path("BL_ALIGN_WAV")) else {
        return;
    };
    let t0 = Instant::now();
    let a = Aligner::load(&dir).unwrap();
    println!("loaded in {:.2} s", t0.elapsed().as_secs_f64());
    let samples = read_wav(&wav);
    let text = "And so, my fellow Americans, ask not what your country can do for you, \
                ask what you can do for your country.";
    let t1 = Instant::now();
    let lp = a.log_probs(&samples);
    println!("forward {:.2} s", t1.elapsed().as_secs_f64());
    assert_eq!(
        a.hears(&lp),
        "AND SO MY FELLOW AMERICANS ASK NOT WHAT YOUR COUNTRY CAN DO FOR YOU \
         ASK WHAT YOU CAN DO FOR YOUR COUNTRY"
    );
    let words = a.align_log_probs(&lp, text).unwrap();
    for w in &words {
        println!("{:6.2} {:6.2} {:.3} {}", w.start, w.end, w.score, w.text);
    }
    assert_eq!(words.len(), text.split_whitespace().count());
    // Times from torchaudio's forced alignment of the same log-probs
    // (frames of 20 ms).
    let want = [
        ("And", 0.42, 0.52),
        ("so,", 0.64, 0.84),
        ("Americans,", 1.70, 2.20),
        ("ask", 3.54, 3.70),
        ("country.", 10.06, 10.42),
    ];
    for (text, start, end) in want {
        let w = words.iter().find(|w| w.text == text).unwrap();
        assert!(
            (w.start - start).abs() < 0.03 && (w.end - end).abs() < 0.03,
            "{text}: {} {}",
            w.start,
            w.end
        );
    }
    assert!(words.windows(2).all(|p| p[0].end <= p[1].start));
    assert!(words.iter().all(|w| w.score > 0.5));
    // Too much text for a short stretch fails cleanly.
    let long = text.repeat(4);
    assert!(a.align(&samples[..16_000], &long).is_err());
    assert!(a.align(&samples[..100], "and").is_err());
    assert!(a.align(&samples, "").unwrap().is_empty());
    // Weights kept as f32 give the same words.
    let f32 = Aligner::load_with(&dir, Keep::F32).unwrap();
    let again = f32.align(&samples, text).unwrap();
    for (a, b) in words.iter().zip(&again) {
        assert!((a.start - b.start).abs() < 0.03 && (a.end - b.end).abs() < 0.03);
    }
}

#[test]
fn aligns_german_with_the_large_model() {
    let (Some(dir), Some(wav)) = (path("BL_ALIGN_MODEL_DIR_DE"), path("BL_ALIGN_WAV_DE")) else {
        return;
    };
    let a = Aligner::load(&dir).unwrap();
    let samples = read_wav(&wav);
    // Optional stretch: BL_ALIGN_DE_RANGE="36.0 44.6".
    let (start, end) = std::env::var("BL_ALIGN_DE_RANGE")
        .ok()
        .and_then(|r| {
            let mut it = r.split_whitespace().map(|v| v.parse::<f64>().ok());
            Some((it.next()??, it.next()??))
        })
        .unwrap_or((0.0, samples.len() as f64 / 16_000.0));
    let piece =
        &samples[(start * 16_000.0) as usize..((end * 16_000.0) as usize).min(samples.len())];
    let text = std::env::var("BL_ALIGN_DE_TEXT").unwrap_or_else(|_| {
        "Rolle vor im Transit Pod, Die Sonne scheint auf die Blocks, \
         Hab' meinen Avatar gekillt, Weil ich selber auf die Party will"
            .into()
    });
    let words = a.align(piece, &text).unwrap();
    for w in &words {
        println!(
            "{:6.2} {:6.2} {:.3} {}",
            start + w.start,
            start + w.end,
            w.score,
            w.text
        );
    }
    assert_eq!(words.len(), text.split_whitespace().count());
    assert!(words.windows(2).all(|p| p[0].start <= p[1].start));
}
