//! cargo run --release -p bl-align --example sung_score -- MODEL VOCALS SONG.lrc [--threads N]
//!
//! How well the CTC head (`MODEL` with its `.head` next to it) tells a sung
//! line's own text from other text, for the karaoke mode's text points:
//! each line of the timed text (with 0.3 s around it) scored against its
//! own words, against another line's words and three seconds early.
//! Prints the scores and what a line costs.

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let files: Vec<&String> = {
        let mut skip = false;
        args.iter()
            .filter(|a| {
                let keep = !skip && !a.starts_with("--");
                skip = a.starts_with("--");
                keep
            })
            .collect()
    };
    if files.len() != 3 {
        eprintln!("usage: sung_score MODEL VOCALS SONG.lrc [--threads N] [--lang en]");
        std::process::exit(2);
    }
    if let Some(n) = opt("--threads").and_then(|v| v.parse().ok()) {
        bl_asr::ops::set_max_threads(n);
    }
    let model_path = std::path::Path::new(files[0]);
    let model = bl_asr::model::Model::load(&mut std::io::BufReader::new(
        std::fs::File::open(model_path).expect("model"),
    ))
    .expect("model");
    let head = bl_align::Head::for_model(model_path).expect("no .head next to the model");
    let src = std::fs::File::open(files[1]).expect("audio");
    let ext = std::path::Path::new(files[1])
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string);
    let audio = bl_media::decode_mono(Box::new(src), ext.as_deref(), 16000, 20 * 60, &mut || true)
        .expect("decoding");
    let song = bl_pitch::lrc::parse(&std::fs::read_to_string(files[2]).expect("lrc"));
    let lang = opt("--lang").unwrap_or_else(|| "en".into());
    let lines: Vec<(f64, f64, String)> = song
        .lines
        .iter()
        .filter(|l| !l.syllables.is_empty())
        .map(|l| {
            let text: String = l.syllables.iter().map(|s| s.text.as_str()).collect();
            (
                l.syllables[0].start,
                l.syllables.last().unwrap().end,
                text.trim().to_string(),
            )
        })
        .collect();
    let cut = |a: f64, b: f64| -> &[f32] {
        let i = ((a.max(0.0)) * 16000.0) as usize;
        let j = ((b * 16000.0) as usize).min(audio.len());
        &audio[i.min(j)..j]
    };
    let (mut own, mut other, mut early) = (Vec::new(), Vec::new(), Vec::new());
    let mut secs = 0.0;
    let mut took = 0.0;
    for (k, (a, b, text)) in lines.iter().enumerate() {
        let clip = cut(a - 0.3, b + 0.3);
        let t = Instant::now();
        let s_own = head.sung_score(&model, clip, &lang, text);
        took += t.elapsed().as_secs_f64();
        secs += clip.len() as f64 / 16000.0;
        let wrong = &lines[(k + lines.len() / 2) % lines.len()].2;
        let s_other = head.sung_score(&model, clip, &lang, wrong);
        let s_early = head.sung_score(&model, cut(a - 3.3, b - 2.7), &lang, text);
        println!(
            "{a:7.2} {:5} {:5} {:5}  {text}",
            s_own.map_or("-".into(), |v| format!("{v:.2}")),
            s_other.map_or("-".into(), |v| format!("{v:.2}")),
            s_early.map_or("-".into(), |v| format!("{v:.2}")),
        );
        if let (Some(x), Some(y), Some(z)) = (s_own, s_other, s_early) {
            own.push(x);
            other.push(y);
            early.push(z);
        }
    }
    let med = |v: &mut Vec<f32>| {
        v.sort_by(f32::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    let wins = own.iter().zip(&other).filter(|(a, b)| a > b).count();
    println!(
        "{} lines: median own {:.2}, other line's words {:.2}, 3 s early {:.2}; own beats other in {} ({:.0} %)",
        own.len(),
        med(&mut own.clone()),
        med(&mut other.clone()),
        med(&mut early.clone()),
        wins,
        100.0 * wins as f64 / own.len().max(1) as f64
    );
    println!(
        "{:.0} ms per line on average ({:.2} s of audio per second)",
        1000.0 * took / lines.len().max(1) as f64,
        secs / took.max(1e-9)
    );
}
