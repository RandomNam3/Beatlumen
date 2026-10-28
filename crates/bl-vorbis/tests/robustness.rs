//! Corrupted / truncated input must never panic.

mod common;

use bl_vorbis::VorbisDecoder;
use common::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        // xorshift64*
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn mutate(rng: &mut Rng, data: &mut Vec<u8>) {
    if data.is_empty() {
        data.push(rng.next() as u8);
        return;
    }
    match rng.below(5) {
        0 => {
            // Flip a few bits.
            for _ in 0..1 + rng.below(4) {
                let i = rng.below(data.len());
                data[i] ^= 1 << rng.below(8);
            }
        }
        1 => {
            let i = rng.below(data.len());
            data[i] = rng.next() as u8;
        }
        2 => {
            let n = rng.below(data.len() + 1);
            data.truncate(n);
        }
        3 => {
            // Overwrite a run with random bytes.
            let start = rng.below(data.len());
            let len = 1 + rng.below(16);
            for i in start..(start + len).min(data.len()) {
                data[i] = rng.next() as u8;
            }
        }
        _ => {
            let i = rng.below(data.len() + 1);
            for _ in 0..1 + rng.below(8) {
                data.insert(i, rng.next() as u8);
            }
        }
    }
}

fn fixtures(tag: &str) -> Vec<Vec<Vec<u8>>> {
    let dir = temp_dir();
    let mut out = Vec::new();
    let specs: [(&str, u32, usize, &[&str]); 3] = [
        ("rb_stereo", 44100, 2, &["-c:a", "libvorbis", "-q:a", "4"]),
        ("rb_51", 48000, 6, &["-c:a", "libvorbis", "-q:a", "2"]),
        ("rb_mono", 16000, 1, &["-c:a", "libvorbis", "-q:a", "7"]),
    ];
    for (name, rate, ch, args) in specs {
        if let Some(p) = make_fixture(&dir, &format!("{name}_{tag}"), rate, ch, 1.5, args) {
            let data = std::fs::read(&p).unwrap();
            out.push(read_packets(&data).0);
            let _ = std::fs::remove_file(p);
        }
    }
    out
}

#[test]
fn corrupted_audio_packets() {
    if !ffmpeg_available() || !has_encoder("libvorbis") {
        eprintln!("ffmpeg with libvorbis not available; skipping");
        return;
    }
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut errors = 0usize;
    let mut decoded = 0usize;
    for packets in fixtures("audio") {
        for _round in 0..60 {
            let mut dec = VorbisDecoder::new(&packets[0], &packets[1], &packets[2]).unwrap();
            let mut out = Vec::new();
            for p in &packets[3..] {
                let mut p = p.clone();
                if rng.below(4) == 0 {
                    mutate(&mut rng, &mut p);
                }
                let _ = bl_vorbis::packet_block_size(&dec, &p);
                let _ = dec.packet_window_size(&p);
                match dec.decode_packet(&p, &mut out) {
                    Ok(n) => decoded += n,
                    Err(_) => errors += 1,
                }
                if rng.below(50) == 0 {
                    dec.reset();
                }
            }
            // Pure random packets.
            for _ in 0..20 {
                let len = rng.below(600);
                let p: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                let _ = dec.decode_packet(&p, &mut out);
            }
        }
    }
    println!("decoded {decoded} frames, {errors} packets rejected");
}

#[test]
fn corrupted_headers() {
    if !ffmpeg_available() || !has_encoder("libvorbis") {
        eprintln!("ffmpeg with libvorbis not available; skipping");
        return;
    }
    let mut rng = Rng(0xfeed_beef_cafe_0001);
    let mut accepted = 0usize;
    for packets in fixtures("headers") {
        let (id, cm, setup) = (&packets[0], &packets[1], &packets[2]);
        // Every truncation of each header.
        for n in 0..id.len() {
            assert!(VorbisDecoder::new(&id[..n], cm, setup).is_err());
        }
        for n in 0..cm.len() {
            let _ = VorbisDecoder::new(id, &cm[..n], setup);
        }
        for n in 0..setup.len() {
            assert!(VorbisDecoder::new(id, cm, &setup[..n]).is_err());
        }
        // Random mutations; decode some audio with whatever is accepted.
        for _ in 0..400 {
            let mut h = [id.clone(), cm.clone(), setup.clone()];
            let which = rng.below(3);
            mutate(&mut rng, &mut h[which]);
            if let Ok(mut dec) = VorbisDecoder::new(&h[0], &h[1], &h[2]) {
                accepted += 1;
                let mut out = Vec::new();
                for p in packets[3..].iter().take(30) {
                    let _ = dec.decode_packet(p, &mut out);
                }
            }
        }
    }
    println!("{accepted} mutated header sets accepted");
}
