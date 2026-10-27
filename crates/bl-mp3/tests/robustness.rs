//! Container robustness: tags, junk, truncation and corrupted data.

mod common;

use bl_mp3::{probe, Mp3Decoder, SeekMode};
use common::*;
use std::io::Cursor;

/// Deterministic xorshift PRNG.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

fn fixture() -> Option<Vec<u8>> {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:d=2",
        "-f",
        "lavfi",
        "-i",
        "anoisesrc=d=2:a=0.3",
        "-filter_complex",
        "[0:a][1:a]amerge=inputs=2[a]",
        "-map",
        "[a]",
        "-c:a",
        "libmp3lame",
        "-q:a",
        "3",
    ];
    gen("robust_vbr.mp3", &args).map(|p| std::fs::read(p).unwrap())
}

fn id3v2(version: u8, body: usize, footer: bool) -> Vec<u8> {
    let mut t = vec![b'I', b'D', b'3', version, 0, if footer { 0x10 } else { 0 }];
    let n = body as u32;
    t.extend_from_slice(&[
        (n >> 21) as u8 & 0x7F,
        (n >> 14) as u8 & 0x7F,
        (n >> 7) as u8 & 0x7F,
        n as u8 & 0x7F,
    ]);
    // Body full of sync-like bytes that must not be mistaken for audio.
    t.extend((0..body).map(|i| if i % 3 == 0 { 0xFF } else { 0xFB }));
    if footer {
        t.extend_from_slice(b"3DI");
        t.extend_from_slice(&[version, 0, 0x10]);
        t.extend_from_slice(&[
            (n >> 21) as u8 & 0x7F,
            (n >> 14) as u8 & 0x7F,
            (n >> 7) as u8 & 0x7F,
            n as u8 & 0x7F,
        ]);
    }
    t
}

fn ape_tag(items: &[u8]) -> Vec<u8> {
    let size = (items.len() + 32) as u32;
    let mut hdr = b"APETAGEX".to_vec();
    hdr.extend_from_slice(&2000u32.to_le_bytes());
    hdr.extend_from_slice(&size.to_le_bytes());
    hdr.extend_from_slice(&1u32.to_le_bytes());
    hdr.extend_from_slice(&0xA000_0000u32.to_le_bytes());
    hdr.extend_from_slice(&[0; 8]);
    let mut ftr = hdr.clone();
    ftr[20..24].copy_from_slice(&0x8000_0000u32.to_le_bytes());
    let mut t = hdr;
    t.extend_from_slice(items);
    t.extend_from_slice(&ftr);
    t
}

#[test]
fn tags_are_skipped() {
    let Some(mp3) = fixture() else { return };
    let plain = decode_bytes(&mp3);

    // A valid MP3 frame header (copied from the stream) inside tag data.
    let frame_like: Vec<u8> = mp3[..4].iter().copied().cycle().take(400).collect();
    let mut file = id3v2(3, 1000, false);
    file.extend(id3v2(4, 300, true));
    file.extend_from_slice(&mp3);
    file.extend(ape_tag(&frame_like));
    let mut v1 = b"TAG".to_vec();
    v1.extend(frame_like.iter().take(125));
    file.extend(v1);
    assert!(probe(&file));
    let tagged = decode_bytes(&file);
    assert_eq!(tagged.total_frames, plain.total_frames);
    assert_eq!(tagged.pcm.len(), plain.pcm.len());
    assert!(tagged.pcm == plain.pcm, "tags changed the decoded audio");
}

#[test]
fn junk_before_and_inside_stream() {
    let Some(mp3) = fixture() else { return };
    let plain = decode_bytes(&mp3);
    let mut rng = Rng(0x1234_5678_9abc);
    let mut file: Vec<u8> = (0..3000).map(|_| rng.next() as u8).collect();
    file.extend_from_slice(&mp3);
    let d = decode_bytes(&file);
    assert_eq!(d.pcm, plain.pcm, "leading junk changed the decoded audio");

    // Junk in the middle: decoding resynchronizes and loses at most a few
    // frames around the damage.
    let mid = mp3.len() / 2;
    let mut file = mp3[..mid].to_vec();
    file.extend((0..777).map(|_| rng.next() as u8));
    file.extend_from_slice(&mp3[mid..]);
    let d = decode_bytes(&file);
    let ch = d.channels as usize;
    let diff = (d.pcm.len() as i64 - plain.pcm.len() as i64).abs() / ch as i64;
    assert!(diff <= 3 * 1152, "lost {diff} samples per channel");
    assert!(d.pcm.iter().all(|x| x.is_finite() && x.abs() <= 1.0));
}

fn decode_capped(data: &[u8]) -> Option<usize> {
    let mut d = Mp3Decoder::new(Cursor::new(data)).ok()?;
    let _ = d.total_frames();
    let mut out = Vec::new();
    let mut chunks = 0;
    while let Ok(true) = d.next_chunk(&mut out) {
        chunks += 1;
        assert!(chunks < 100_000, "decoder does not terminate");
        assert!(
            out.iter().all(|x| x.is_finite() && x.abs() <= 1.0),
            "sample out of range"
        );
        out.clear();
    }
    Some(chunks)
}

fn fuzz(name: &str, base: &[u8], seed: u64, iters: usize) {
    let mut rng = Rng(seed);
    for iter in 0..iters {
        let mut data = base.to_vec();
        let n = 1 + rng.below(if iter % 10 == 0 { 2000 } else { 40 });
        for _ in 0..n {
            let i = rng.below(data.len());
            match rng.below(3) {
                0 => data[i] = rng.next() as u8,
                1 => data[i] ^= 1 << rng.below(8),
                _ => data[i] = 0xFF,
            }
        }
        if iter % 7 == 0 {
            data.truncate(rng.below(data.len()));
        }
        let _ = decode_capped(&data);
        // Seeking in damaged data must not panic either.
        if let Ok(mut d) = Mp3Decoder::new(Cursor::new(&data[..])) {
            if iter % 2 == 1 {
                d.set_seek_mode(SeekMode::Fast);
            }
            for _ in 0..3 {
                let t = rng.below(200_000) as u64;
                let _ = d.seek(t);
                let mut out = Vec::new();
                let _ = d.next_chunk(&mut out);
                assert!(
                    out.iter().all(|x| x.is_finite() && x.abs() <= 1.0),
                    "{name}"
                );
            }
        }
    }
}

/// Random byte mutations and truncations (more iterations with
/// `OC_MP3_FUZZ_ITERS`); decoding and seeking must never panic or loop.
#[test]
fn corrupted_streams_do_not_panic() {
    let iters: usize = std::env::var("OC_MP3_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150);
    let Some(mp3) = fixture() else { return };
    fuzz("vbr", &mp3, 42, iters);
    let lsf = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=300:d=2",
        "-ar",
        "16000",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "24k",
    ];
    if let Some(p) = gen("robust_lsf.mp3", &lsf) {
        fuzz("lsf", &std::fs::read(p).unwrap(), 43, iters / 2);
    }
    let l2 = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=300:d=2",
        "-c:a",
        "mp2",
        "-b:a",
        "128k",
    ];
    if let Some(p) = gen("robust_l2.mp2", &l2) {
        fuzz("layer2", &std::fs::read(p).unwrap(), 44, iters / 2);
    }
}

#[test]
fn truncated_streams() {
    let Some(mp3) = fixture() else { return };
    let full = decode_bytes(&mp3);
    let ch = full.channels as usize;
    for k in 1..40 {
        let cut = mp3.len() * k / 40;
        if let Some(_chunks) = decode_capped(&mp3[..cut]) {
            let d = decode_bytes(&mp3[..cut]);
            // Output is a prefix of the full decode (gapless trimming aside).
            let n = d.pcm.len().min(full.pcm.len());
            assert!(
                d.pcm[..n] == full.pcm[..n],
                "cut at {cut}: output is not a prefix"
            );
            assert!(d.pcm.len() <= full.pcm.len() + 1152 * ch);
        }
    }
}

#[test]
fn garbage_is_rejected() {
    let mut rng = Rng(7);
    let noise: Vec<u8> = (0..100_000).map(|_| rng.next() as u8).collect();
    assert!(Mp3Decoder::new(Cursor::new(&noise[..])).is_err());
    assert!(!probe(&noise[..4096]));
    assert!(Mp3Decoder::new(Cursor::new(&[][..])).is_err());
    assert!(Mp3Decoder::new(Cursor::new(&[0xFF, 0xFB, 0x90][..])).is_err());
    assert!(Mp3Decoder::new(Cursor::new(&vec![0u8; 50_000][..])).is_err());
    assert!(!probe(b"fLaC\0\0\0\x22"));
    assert!(!probe(b"OggS\0\x02\0\0\0\0\0\0\0\0"));
    assert!(!probe(&[]));
    // A lone ID3 tag is accepted by probe but holds no audio.
    let tag = id3v2(4, 100, false);
    assert!(probe(&tag));
    assert!(Mp3Decoder::new(Cursor::new(&tag[..])).is_err());
}

#[test]
fn probe_accepts_streams() {
    let Some(mp3) = fixture() else { return };
    assert!(probe(&mp3[..4096]));
    assert!(probe(&mp3[..600]));
    let mut junk = vec![0u8; 100];
    junk.extend_from_slice(&mp3[..4096]);
    assert!(probe(&junk));
}
