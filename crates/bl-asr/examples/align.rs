//! cargo run --release -p bl-asr --example align -- MODEL WAV_16K_MONO TEXT SEGMENTS_CACHE
//!
//! Times a plain song text like the server and the apps do, step by step:
//! the recognised segments (kept in SEGMENTS_CACHE, so the alignment can be
//! tried again without recognising the song again), then the lines' times.
use bl_asr::timing::Word;
use bl_asr::transcribe::{Options, Segment, Transcriber};

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let text = std::fs::read_to_string(&a[3]).unwrap();
    let segments = match std::fs::read_to_string(&a[4]) {
        Ok(cache) => read(&cache),
        Err(_) => {
            let m = bl_asr::model::Model::load(&mut std::io::BufReader::new(
                std::fs::File::open(&a[1]).unwrap(),
            ))
            .unwrap();
            let raw = std::fs::read(&a[2]).unwrap();
            let samples: Vec<f32> = raw[44..]
                .chunks_exact(2)
                .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
                .collect();
            let words: Vec<&str> = text.split_whitespace().collect();
            let prompt = words[words.len().saturating_sub(120)..].join(" ");
            let segs = Transcriber::new(&m)
                .transcribe(
                    &samples,
                    Options {
                        language: None,
                        prompt: std::env::var("PROMPT").is_ok().then_some(prompt.as_str()),
                        progress: &mut |_| true,
                    },
                )
                .unwrap();
            std::fs::write(&a[4], write(&segs)).unwrap();
            segs
        }
    };
    for s in &segments {
        println!("# [{:6.2} - {:6.2}] {}", s.start, s.end, s.text);
    }
    let (anchors, matched) = bl_asr::align::anchor_lines(&text, &segments);
    println!("# matched {:.0} %", matched * 100.0);
    for a in anchors {
        println!(
            "{:7.2} {} {}",
            a.time,
            if a.matched { "*" } else { " " },
            a.text
        );
    }
}

fn write(segs: &[Segment]) -> String {
    let mut out = String::new();
    for s in segs {
        out += &format!("S\t{}\t{}\t{}\t{}\n", s.start, s.end, s.avg_logprob, s.text);
        for w in &s.words {
            out += &format!("W\t{}\t{}\t{}\n", w.start, w.end, w.text);
        }
    }
    out
}

fn read(cache: &str) -> Vec<Segment> {
    let mut segs: Vec<Segment> = Vec::new();
    for l in cache.lines() {
        let f: Vec<&str> = l.splitn(5, '\t').collect();
        match f[0] {
            "S" => segs.push(Segment {
                start: f[1].parse().unwrap(),
                end: f[2].parse().unwrap(),
                avg_logprob: f[3].parse().unwrap(),
                text: f[4].to_string(),
                words: Vec::new(),
            }),
            _ => segs.last_mut().unwrap().words.push(Word {
                start: f[1].parse().unwrap(),
                end: f[2].parse().unwrap(),
                text: f[3].to_string(),
                pieces: Vec::new(),
            }),
        }
    }
    segs
}
