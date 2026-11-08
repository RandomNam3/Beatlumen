//! cargo run --release -p bl-karaoke --example karaoke_time -- MODEL SONG SONG.lrc [--threads N] [--json OUT.json]
//!
//! Computes the karaoke data of a song as the server and the apps do (the
//! vocals separated with the `bl-sep` model `MODEL`, their melody, one
//! note per syllable of the timed text) and prints what each step took,
//! for measuring on phones (4 threads) and servers (6 threads).

use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let mut files = Vec::new();
    let mut skip = false;
    for a in &args {
        if skip {
            skip = false;
        } else if a.starts_with("--") {
            skip = true;
        } else {
            files.push(a.clone());
        }
    }
    if files.len() != 3 {
        eprintln!("usage: karaoke_time MODEL SONG SONG.lrc [--threads N] [--json OUT.json]");
        std::process::exit(2);
    }
    if let Some(n) = opt("--threads").and_then(|v| v.parse().ok()) {
        bl_asr::ops::set_max_threads(n);
    }
    let t = Instant::now();
    let ext = std::path::Path::new(&files[1])
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_string);
    let src = std::fs::File::open(&files[1]).expect("song");
    let audio = bl_karaoke::decode(Box::new(src), ext.as_deref(), &mut || true).expect("decoding");
    let decoded = t.elapsed().as_secs_f64();
    let t = Instant::now();
    let sep = bl_sep::Separator::load(&files[0]).expect("model");
    let loaded = t.elapsed().as_secs_f64();
    let lyrics = std::fs::read_to_string(&files[2]).expect("song text");
    let t = Instant::now();
    let (track, report) =
        bl_karaoke::compute(&sep, &audio, &lyrics, &mut |_| true).expect("karaoke");
    let all = t.elapsed().as_secs_f64();
    println!(
        "{:.1} s of audio on {} threads: decoded in {decoded:.2} s, model loaded in {loaded:.2} s, separated in {:.2} s, pitch in {:.2} s, all {all:.2} s ({:.1}x real time)",
        audio.secs(),
        bl_asr::ops::threads(),
        report.separation_secs,
        report.pitch_secs,
        audio.secs() / all
    );
    println!(
        "{} lines, {} notes ({} freestyle), {:.0} % voiced, key {}, language {}",
        track.lines.len(),
        report.notes.notes,
        report.notes.freestyle,
        report.notes.voiced_share * 100.0,
        track.key.map(|k| k.name()).unwrap_or_default(),
        report.language
    );
    if let Some(out) = opt("--json") {
        std::fs::write(out, track.to_json().to_json_pretty()).expect("writing");
    }
}
