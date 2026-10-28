//! Decodes an ADTS (.aac) file to raw interleaved f32le.
//!
//! Usage: `cargo run --release -p bl-aac --example aacdec -- in.aac out.f32 [asc-hex]`
//! The optional AudioSpecificConfig (hex) overrides the one derived from the
//! first ADTS header (useful to test explicit HE-AAC signalling).
//!
//! Inputs ending in `.raw` use a simple length-prefixed packet format
//! (u32be ASC length, ASC, then u32be length + raw_data_block per packet),
//! e.g. extracted from MP4 files by a test script.

use bl_aac::{AacDecoder, AdtsHeader};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: aacdec in.aac out.f32 [asc-hex]");
        std::process::exit(2);
    }
    let data = std::fs::read(&args[1]).expect("read input");
    if args[1].ends_with(".raw") {
        return decode_raw(&data, &args[2]);
    }
    let mut frames = Vec::new();
    let mut pos = 0;
    while pos + 7 <= data.len() {
        match AdtsHeader::parse(&data[pos..]) {
            Some(h) if pos + h.frame_length <= data.len() => {
                frames.push(&data[pos..pos + h.frame_length]);
                pos += h.frame_length;
            }
            _ => pos += 1,
        }
    }
    let first = AdtsHeader::parse(frames.first().expect("no ADTS frames")).unwrap();
    let asc: Vec<u8> = match args.get(3) {
        Some(hex) => (0..hex.len() / 2)
            .map(|i| u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).unwrap())
            .collect(),
        None => first.audio_specific_config().to_vec(),
    };
    let mut dec = AacDecoder::new(&asc).expect("decoder init");
    let mut out = Vec::new();
    let t0 = std::time::Instant::now();
    let mut errors = 0;
    for f in &frames {
        if let Err(e) = dec.decode_adts_frame(f, &mut out) {
            errors += 1;
            if errors < 5 {
                eprintln!("frame error: {e}");
            }
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    let ch = dec.channels().max(1) as usize;
    let secs = out.len() as f64 / ch as f64 / dec.sample_rate() as f64;
    eprintln!(
        "{} frames, {} Hz, {} ch, {:.2} s audio in {:.3} s ({:.0}x realtime), {} errors",
        frames.len(),
        dec.sample_rate(),
        ch,
        secs,
        dt,
        secs / dt,
        errors
    );
    let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(&args[2], bytes).expect("write output");
}

fn decode_raw(data: &[u8], out_path: &str) {
    let be32 =
        |p: usize| u32::from_be_bytes([data[p], data[p + 1], data[p + 2], data[p + 3]]) as usize;
    let asc_len = be32(0);
    let asc = &data[4..4 + asc_len];
    let mut dec = AacDecoder::new(asc).expect("decoder init");
    let mut pos = 4 + asc_len;
    let mut out = Vec::new();
    let mut errors = 0;
    let mut packets = 0;
    let t0 = std::time::Instant::now();
    while pos + 4 <= data.len() {
        let n = be32(pos);
        pos += 4;
        let pkt = &data[pos..(pos + n).min(data.len())];
        pos += n;
        packets += 1;
        if let Err(e) = dec.decode_frame(pkt, &mut out) {
            errors += 1;
            if errors < 5 {
                eprintln!("packet {packets} error: {e}");
            }
        }
    }
    let dt = t0.elapsed().as_secs_f64();
    let ch = dec.channels().max(1) as usize;
    let secs = out.len() as f64 / ch as f64 / dec.sample_rate() as f64;
    eprintln!(
        "{packets} packets, {} Hz, {ch} ch, {secs:.2} s audio in {dt:.3} s ({:.0}x realtime), {errors} errors",
        dec.sample_rate(),
        secs / dt
    );
    let bytes: Vec<u8> = out.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(out_path, bytes).expect("write output");
}
