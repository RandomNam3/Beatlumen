//! Development tool: decodes a file and compares it against a raw f32le
//! reference, printing per-block error statistics.
use bl_mp3::Mp3Decoder;
use std::io::Cursor;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&args[1]).unwrap();
    let mut d = Mp3Decoder::new(Cursor::new(&data[..])).unwrap();
    let ch = d.channels() as usize;
    let mut pcm = Vec::new();
    let t = std::time::Instant::now();
    while d.next_chunk(&mut pcm).unwrap() {}
    let el = t.elapsed().as_secs_f64();
    let secs = pcm.len() as f64 / ch as f64 / d.sample_rate() as f64;
    println!("{} frames, {:.1}x realtime", pcm.len() / ch, secs / el);
    if let Ok(p) = std::env::var("DUMP") {
        let b: Vec<u8> = pcm.iter().flat_map(|x| x.to_le_bytes()).collect();
        std::fs::write(p, b).unwrap();
    }
    if args.len() > 2 {
        let r = std::fs::read(&args[2]).unwrap();
        let r: Vec<f32> = r
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect();
        let block: usize = args.get(3).map(|s| s.parse().unwrap()).unwrap_or(1152);
        println!("len ours {} ref {}", pcm.len() / ch, r.len() / ch);
        let n = pcm.len().min(r.len()) / ch;
        for b in 0..n.div_ceil(block) {
            let (mut s, mut e) = (0f64, 0f64);
            for i in b * block * ch..((b + 1) * block * ch).min(n * ch) {
                s += (r[i] as f64).powi(2);
                e += (r[i] as f64 - pcm[i] as f64).powi(2);
            }
            println!(
                "block {b}: snr {:.1} dB (sig {:.3e})",
                10.0 * (s / e.max(1e-30)).log10(),
                s
            );
        }
    }
}
