//! cargo run --release -p bl-sep --example separate -- MODEL SONG OUT.wav [--wiener] [--16k]
//!
//! Writes the vocals of SONG (any format the player reads) to OUT.wav:
//! stereo at 44.1 kHz, or with `--16k` mono at 16 kHz as speech
//! recognition takes it. Prints the time each step took.

use std::io::Write;
use std::time::Instant;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let files: Vec<&String> = args[1..].iter().filter(|a| !a.starts_with("--")).collect();
    let flag = |f: &str| args.iter().any(|a| a == f);
    if files.len() != 3 {
        eprintln!("usage: separate MODEL SONG OUT.wav [--wiener] [--16k]");
        std::process::exit(2);
    }

    let t = Instant::now();
    let src = std::fs::File::open(files[1]).expect("song");
    let ext = std::path::Path::new(files[1])
        .extension()
        .and_then(|e| e.to_str());
    let mut dec = bl_media::open(Box::new(src), ext).expect("decoder");
    let spec = dec.spec();
    let mut interleaved = Vec::new();
    while dec.next_chunk(&mut interleaved).expect("decoding") {}
    let channels = spec.channels.max(1) as usize;
    let planar: Vec<Vec<f32>> = if channels > 2 {
        let mut stereo = Vec::new();
        bl_media::downmix_to_stereo(&interleaved, spec.channels, &mut stereo);
        (0..2)
            .map(|c| stereo.iter().skip(c).step_by(2).copied().collect())
            .collect()
    } else {
        (0..channels)
            .map(|c| {
                interleaved
                    .iter()
                    .skip(c)
                    .step_by(channels)
                    .copied()
                    .collect()
            })
            .collect()
    };
    let secs = planar[0].len() as f32 / spec.sample_rate as f32;
    println!(
        "decoded {secs:.1} s ({} Hz, {channels} channels) in {:.2} s",
        spec.sample_rate,
        t.elapsed().as_secs_f32()
    );

    let t = Instant::now();
    let sep = bl_sep::Separator::load(files[0]).expect("model");
    println!("loaded the model in {:.2} s", t.elapsed().as_secs_f32());

    let t = Instant::now();
    let mut last = 0.0;
    let mut progress = |p: f32| {
        if p - last >= 0.1 || p >= 1.0 {
            println!(
                "  {:3.0} % after {:.1} s",
                p * 100.0,
                t.elapsed().as_secs_f32()
            );
            last = p;
        }
        true
    };
    let refs: Vec<&[f32]> = planar.iter().map(|c| c.as_slice()).collect();
    let opts = bl_sep::Options {
        wiener: flag("--wiener"),
        progress: &mut progress,
    };
    let vocals = sep
        .separate(&refs, spec.sample_rate, opts)
        .expect("separation");
    let took = t.elapsed().as_secs_f32();
    println!("separated in {took:.2} s ({:.1}x real time)", secs / took);

    let (rate, chans, samples): (u32, u16, Vec<f32>) = if flag("--16k") {
        (16000, 1, vocals.mono(16000))
    } else {
        let s = vocals
            .left
            .iter()
            .zip(&vocals.right)
            .flat_map(|(l, r)| [*l, *r])
            .collect();
        (bl_sep::SAMPLE_RATE, 2, s)
    };
    write_wav(files[2], rate, chans, &samples);
    // Peak memory (Linux).
    if let Ok(status) = std::fs::read_to_string("/proc/self/status") {
        if let Some(l) = status.lines().find(|l| l.starts_with("VmHWM")) {
            println!("peak memory: {}", l[6..].trim());
        }
    }
}

/// 16-bit PCM WAV.
fn write_wav(path: &str, rate: u32, channels: u16, samples: &[f32]) {
    let data = samples.len() as u32 * 2;
    let mut w = Vec::with_capacity(44 + data as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    w.extend_from_slice(&(channels * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data.to_le_bytes());
    for s in samples {
        let v = (s.clamp(-1.0, 1.0) * 32767.0).round() as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    std::fs::File::create(path)
        .and_then(|mut f| f.write_all(&w))
        .expect("writing");
}
