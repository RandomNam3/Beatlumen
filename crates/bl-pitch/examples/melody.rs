//! cargo run --release -p bl-pitch --example melody -- VOCALS.wav SONG.lrc OUT.txt
//!     [--lang en] [--bpm 120 --offset 0.25] [--json OUT.json]
//!
//! Writes UltraStar TXT from a WAV of the singing alone (e.g. from the
//! `bl-sep` example) and a song text with word times (enhanced LRC; lines
//! without word times get their words spread evenly). Prints what came
//! out: the voiced share, the range of the notes, octave jumps, the key
//! and how long the pitch tracking took.

use bl_pitch::{lrc, notes, pyin, syllables, track, ultrastar};
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let opt = |name: &str| -> Option<String> {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1).cloned())
    };
    let lang = opt("--lang").unwrap_or_else(|| "en".into());
    let bpm: Option<f64> = opt("--bpm").and_then(|v| v.parse().ok());
    let offset: f64 = opt("--offset").and_then(|v| v.parse().ok()).unwrap_or(0.0);
    let json = opt("--json");
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
        eprintln!("usage: melody VOCALS.wav SONG.lrc OUT.txt [--lang en] [--bpm B --offset S] [--json OUT.json]");
        std::process::exit(2);
    }

    let (samples, rate) = read_wav(&files[0]).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1)
    });
    let secs = samples.len() as f64 / rate as f64;
    let text = std::fs::read_to_string(&files[1]).expect("song text");
    let song = lrc::parse(&text);

    let t = Instant::now();
    let frames = pyin::track(&samples, rate, &pyin::PyinOptions::default());
    let took = t.elapsed().as_secs_f64();

    let lines: Vec<notes::LineIn> = song
        .lines
        .iter()
        .map(|l| notes::LineIn {
            voice: l.voice,
            syllables: syllables::syllabify(&l.syllables, &lang),
        })
        .collect();
    let opts = notes::NoteOptions {
        grid: bpm.map(|bpm| notes::BeatGrid {
            bpm,
            offset,
            division: 4,
        }),
        ..Default::default()
    };
    let t = Instant::now();
    let (mut tr, report) = notes::build_track(&frames, &lines, &opts);
    let took_notes = t.elapsed().as_secs_f64();
    tr.title = song.title;
    tr.artist = song.artist;
    tr.language = lang;
    std::fs::write(&files[2], ultrastar::to_ultrastar(&tr)).expect("writing");
    if let Some(j) = json {
        std::fs::write(j, tr.to_json().to_json_pretty()).expect("writing JSON");
    }

    // What came out.
    let voiced = frames.iter().filter(|f| f.f0.is_some()).count();
    println!(
        "{:.1} s at {rate} Hz: pitch tracked in {took:.2} s ({:.0}x real time), notes in {:.3} s; {} frames, {:.0} % voiced",
        secs,
        secs / took,
        took_notes,
        frames.len(),
        100.0 * voiced as f64 / frames.len().max(1) as f64
    );
    println!(
        "{} lines, {} notes ({} extra \"~\", {} freestyle), {:.0} % of the frames within syllables voiced, {} octave fixes",
        tr.lines.len(),
        report.notes,
        report.extra_notes,
        report.freestyle,
        100.0 * report.voiced_share,
        report.octave_fixes
    );
    if let Some((lo, hi)) = tr.range() {
        // Where the notes lie, weighted by duration.
        let mut by_pitch: Vec<(i32, i64)> = tr
            .notes()
            .filter(|n| n.kind.has_pitch())
            .map(|n| (n.pitch, n.dur_ms))
            .collect();
        by_pitch.sort_unstable();
        let total: i64 = by_pitch.iter().map(|p| p.1).sum();
        let at = |q: f64| {
            let mut acc = 0;
            by_pitch
                .iter()
                .find(|p| {
                    acc += p.1;
                    acc as f64 >= q * total as f64
                })
                .map_or(lo, |p| p.0)
        };
        println!(
            "range {} – {} (90 % within {} – {}, median {})",
            bl_pitch::midi_name(lo),
            bl_pitch::midi_name(hi),
            bl_pitch::midi_name(at(0.05)),
            bl_pitch::midi_name(at(0.95)),
            bl_pitch::midi_name(at(0.5))
        );
    }
    let mut jumps = Vec::new();
    for l in &tr.lines {
        let pitched: Vec<&track::Note> = l.notes.iter().filter(|n| n.kind.has_pitch()).collect();
        for w in pitched.windows(2) {
            if (w[1].pitch - w[0].pitch).abs() >= 12 {
                jumps.push(format!(
                    "{:.2}s {}→{} ({:?})",
                    w[1].start_ms as f64 / 1000.0,
                    bl_pitch::midi_name(w[0].pitch),
                    bl_pitch::midi_name(w[1].pitch),
                    w[1].text
                ));
            }
        }
    }
    println!(
        "{} jumps of an octave or more between neighbouring notes",
        jumps.len()
    );
    for j in jumps.iter().take(8) {
        println!("  {j}");
    }
    if let Some(k) = tr.key {
        println!("key {} (fit {:.2})", k.name(), k.strength);
    }
}

/// A WAV file (PCM 16/24/32 bit or float) as mono samples and its rate.
fn read_wav(path: &str) -> Result<(Vec<f32>, u32), String> {
    let b = std::fs::read(path).map_err(|e| format!("{path}: {e}"))?;
    if b.len() < 12 || &b[..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        return Err("not a WAV file".into());
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let mut at = 12;
    let mut fmt = None;
    while at + 8 <= b.len() {
        let len = u32::from_le_bytes([b[at + 4], b[at + 5], b[at + 6], b[at + 7]]) as usize;
        let body = at + 8;
        let end = (body + len).min(b.len());
        match &b[at..at + 4] {
            b"fmt " if len >= 16 => {
                let rate = u32::from_le_bytes([b[body + 4], b[body + 5], b[body + 6], b[body + 7]]);
                fmt = Some((
                    u16_at(body),
                    u16_at(body + 2) as usize,
                    rate,
                    u16_at(body + 14),
                ));
            }
            b"data" => {
                let (format, ch, rate, bits) = fmt.ok_or("no format before the data")?;
                if ch == 0 || !matches!((format, bits), (1 | 0xfffe, 16 | 24 | 32) | (3, 32)) {
                    return Err(format!("unsupported WAV format {format}, {bits} bit"));
                }
                let width = bits as usize / 8;
                let sample = |s: &[u8]| -> f32 {
                    match (format, bits) {
                        (3, _) => f32::from_le_bytes([s[0], s[1], s[2], s[3]]),
                        (_, 16) => i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0,
                        (_, 24) => {
                            (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0
                        }
                        _ => i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0,
                    }
                };
                let mono = b[body..end]
                    .chunks_exact(width * ch)
                    .map(|f| f.chunks_exact(width).map(sample).sum::<f32>() / ch as f32)
                    .collect();
                return Ok((mono, rate));
            }
            _ => {}
        }
        at = body + len + (len & 1);
    }
    Err("no data in the WAV file".into())
}
