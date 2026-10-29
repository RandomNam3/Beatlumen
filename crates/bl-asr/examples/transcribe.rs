//! cargo run --release -p bl-asr --example transcribe -- MODEL WAV_16K_MONO [LANG] [PROMPT]
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
    let t = bl_asr::transcribe::Transcriber::new(&m);
    let t1 = std::time::Instant::now();
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
    }
    eprintln!(
        "transcribed {:.1} s of audio in {:?}",
        samples.len() as f64 / 16000.0,
        t1.elapsed()
    );
}
