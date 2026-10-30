//! cargo run --release -p bl-asr --example transcribe -- MODEL WAV_16K_MONO [LANG] [PROMPT]
//! cargo run --release -p bl-asr --example transcribe -- MODEL WAV_16K_MONO --lrc [TEXT_OR_LRC_FILE]
//!
//! The first form prints the recognised segments and their words; the
//! second times a song text (or writes down the singing) as the server
//! and the apps do, and prints the enhanced LRC.
fn main() {
    let a: Vec<String> = std::env::args().collect();
    let t0 = std::time::Instant::now();
    let m = bl_asr::model::Model::load(&mut std::io::BufReader::new(
        std::fs::File::open(&a[1]).unwrap(),
    ))
    .unwrap();
    eprintln!("loaded in {:?}", t0.elapsed());
    let raw = std::fs::read(&a[2]).unwrap();
    // 16-bit PCM WAV, data after the 44-byte header.
    let samples: Vec<f32> = raw[44..]
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
        .collect();
    let t1 = std::time::Instant::now();
    if a.get(3).map(String::as_str) == Some("--lrc") {
        let text = a.get(4).map(|p| std::fs::read_to_string(p).unwrap());
        // TITLE in the environment: the song's title.
        let title = std::env::var("TITLE").ok();
        match bl_asr::sync::time_song(
            &m,
            "test",
            &samples,
            text.as_deref(),
            title.as_deref(),
            &mut |_| true,
        ) {
            Ok(t) => print!("{}", t.lrc),
            Err(e) => println!("error: {e}"),
        }
    } else {
        let t = bl_asr::transcribe::Transcriber::new(&m);
        let segs = t
            .transcribe(
                &samples,
                bl_asr::transcribe::Options {
                    language: a.get(3).map(|s| s.as_str()),
                    prompt: a.get(4).map(|s| s.as_str()),
                    progress: &mut |_| true,
                },
            )
            .unwrap();
        for s in &segs {
            println!(
                "[{:6.2} - {:6.2}] ({:.2}) {}",
                s.start, s.end, s.avg_logprob, s.text
            );
            for w in &s.words {
                let pieces: Vec<String> = w
                    .pieces
                    .iter()
                    .map(|(t, p)| format!("{t:.2}:{p}"))
                    .collect();
                println!(
                    "    {:6.2} - {:6.2} {} {}",
                    w.start,
                    w.end,
                    w.text,
                    pieces.join(" ")
                );
            }
        }
    }
    eprintln!(
        "{:.1} s of audio in {:?}",
        samples.len() as f64 / 16000.0,
        t1.elapsed()
    );
}
