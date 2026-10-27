//! Unit tests using the decoding examples of RFC 9639 Appendix D.

use crate::{probe, FlacDecoder, FlacFrameDecoder, MetadataBlock};
use std::io::Cursor;

fn hex(s: &str) -> Vec<u8> {
    let digits: Vec<u8> = s
        .lines()
        .flat_map(|l| {
            let l = l.trim();
            let l = l.split_once(':').map_or(l, |(_, r)| r);
            l.chars()
                .filter(|c| c.is_ascii_hexdigit())
                .map(|c| c.to_digit(16).unwrap() as u8)
                .collect::<Vec<_>>()
        })
        .collect();
    digits.chunks(2).map(|p| p[0] << 4 | p[1]).collect()
}

fn example1() -> Vec<u8> {
    hex("00000000: 664c 6143 8000 0022 1000 1000
         0000000c: 0000 0f00 000f 0ac4 42f0 0000
         00000018: 0001 3e84 b418 07dc 6903 0758
         00000024: 6a3d ad1a 2e0f fff8 6918 0000
         00000030: bf03 58fd 0312 8baa 9a")
}

fn example2() -> Vec<u8> {
    hex("00000000: 664c 6143 0000 0022 0010 0010
         0000000c: 0000 1700 0044 0ac4 42f0 0000
         00000018: 0013 d5b0 5649 75e9 8b8d 8b93
         00000024: 0422 757b 8103 0300 0012 0000
         00000030: 0000 0000 0000 0000 0000 0000
         0000003c: 0000 0010 0400 003a 2000 0000
         00000048: 7265 6665 7265 6e63 6520 6c69
         00000054: 6246 4c41 4320 312e 332e 3320
         00000060: 3230 3139 3038 3034 0100 0000
         0000006c: 0e00 0000 5449 544c 453d d7a9
         00000078: d79c d795 d79d 8100 0006 0000
         00000084: 0000 0000 fff8 6998 000f 9912
         00000090: 0867 0162 3d14 4299 8f5d f70d
         0000009c: 6fe0 0c17 caeb 2100 0ee7 a77a
         000000a8: 24a1 590c 1217 b603 097b 784f
         000000b4: aa9a 33d2 85e0 70ad 5b1b 4851
         000000c0: b401 0d99 d2cd 1a68 f1e6 b810
         000000cc: fff8 6918 0102 a402 c382 c40b
         000000d8: c14a 03ee 48dd 03b6 7c13 30")
}

fn example2_samples() -> Vec<i32> {
    let le = hex("8428 B617 7946 3129 5E3A 2722 D445 D128 0B3D B723 EB45 DF28
                  723f 1E25 9D46 4929 B841 7026 5747 B829 8F43 8127 AEC7 14DF
                  9FC4 41DD 54C7 E4DE A5C4 40DD 1EC6 33DE 82C3 90DC 0BC4 02DD
                  4AC1 3EDB");
    le.chunks(2)
        .map(|b| i16::from_le_bytes([b[0], b[1]]) as i32)
        .collect()
}

fn example3() -> Vec<u8> {
    hex("00000000: 664c 6143 8000 0022 1000 1000
         0000000c: 0000 1f00 001f 07d0 0070 0000
         00000018: 0018 f8f9 e396 f5cb cfc6 dc80
         00000024: 7f99 7790 6b32 fff8 6802 0017
         00000030: e944 004f 6f31 3d10 47d2 27cb
         0000003c: 6d09 0831 452b dc28 2222 8057
         00000048: a3")
}

const EXAMPLE3_SAMPLES: [i32; 24] = [
    0, 79, 111, 78, 8, -61, -90, -68, -13, 42, 67, 53, 13, -27, -46, -38, -12, 14, 24, 19, 6, -4,
    -5, 0,
];

fn decode_all_i32(data: &[u8]) -> Vec<i32> {
    let mut dec = FlacDecoder::new(Cursor::new(data)).expect("open");
    let mut out = Vec::new();
    while dec.next_chunk_i32(&mut out).expect("decode") {}
    assert_eq!(dec.corrupt_frames(), 0);
    out
}

#[test]
fn rfc_example_1() {
    let data = example1();
    let dec = FlacDecoder::new(Cursor::new(&data)).unwrap();
    assert_eq!(dec.sample_rate(), 44100);
    assert_eq!(dec.channels(), 2);
    assert_eq!(dec.bits_per_sample(), 16);
    assert_eq!(dec.total_frames(), Some(1));
    assert_eq!(decode_all_i32(&data), vec![25588, 10416]);
}

#[test]
fn rfc_example_2() {
    let data = example2();
    let dec = FlacDecoder::new(Cursor::new(&data)).unwrap();
    let types: Vec<u8> = dec.metadata_blocks().iter().map(|b| b.block_type).collect();
    assert_eq!(
        types,
        vec![
            MetadataBlock::STREAMINFO,
            MetadataBlock::SEEKTABLE,
            MetadataBlock::VORBIS_COMMENT
        ]
    );
    assert_eq!(dec.total_frames(), Some(19));
    assert_eq!(decode_all_i32(&data), example2_samples());
    // f32 output is the integer output scaled by 2^-15.
    let mut dec = FlacDecoder::new(Cursor::new(&data)).unwrap();
    let mut out = Vec::new();
    while dec.next_chunk(&mut out).unwrap() {}
    let expect: Vec<f32> = example2_samples()
        .iter()
        .map(|&s| s as f32 / 32768.0)
        .collect();
    assert_eq!(out, expect);
}

#[test]
fn rfc_example_3() {
    let data = example3();
    assert_eq!(decode_all_i32(&data), EXAMPLE3_SAMPLES.to_vec());
}

#[test]
fn seek_in_examples() {
    let data = example2();
    let all = example2_samples();
    for target in [0u64, 1, 15, 16, 17, 18, 19, 25] {
        let mut dec = FlacDecoder::new(Cursor::new(&data)).unwrap();
        let pos = dec.seek(target).unwrap();
        assert_eq!(pos, target.min(19));
        let mut out = Vec::new();
        while dec.next_chunk_i32(&mut out).unwrap() {}
        assert_eq!(out, all[(pos as usize * 2)..].to_vec(), "target {target}");
    }
}

#[test]
fn frame_decoder_on_examples() {
    let data = example2();
    let mut dec = FlacFrameDecoder::new(&data).unwrap();
    assert_eq!(dec.channels(), 2);
    let mut out = Vec::new();
    assert_eq!(dec.decode_i32(&data[0x88..0xcc], &mut out).unwrap(), 16);
    assert_eq!(dec.decode_i32(&data[0xcc..], &mut out).unwrap(), 3);
    assert_eq!(out, example2_samples());

    // Bare STREAMINFO payload and a corrupted frame (silence, not an error).
    let mut dec = FlacFrameDecoder::new(&data[8..42]).unwrap();
    let mut frame = data[0x88..0xcc].to_vec();
    frame[30] ^= 0x10;
    let mut out = Vec::new();
    assert_eq!(dec.decode(&frame, &mut out).unwrap(), 16);
    assert_eq!(out, vec![0.0; 32]);
    assert_eq!(dec.corrupt_frames(), 1);
    assert!(dec.decode(&[0u8; 10], &mut out).is_err());

    // Ogg FLAC first header packet.
    let mut ogg = vec![0x7f, b'F', b'L', b'A', b'C', 1, 0, 0, 1];
    ogg.extend_from_slice(&data[..42]);
    let dec = FlacFrameDecoder::new(&ogg).unwrap();
    assert_eq!(dec.sample_rate(), 44100);
}

#[test]
fn id3v2_is_skipped() {
    let mut data = b"ID3\x04\x00\x00\x00\x00\x01\x00".to_vec(); // 128 byte tag
    data.extend_from_slice(&[0u8; 128]);
    data.extend_from_slice(&example3());
    assert!(probe(&data));
    assert!(!probe(b"RIFF....WAVE"));
    assert_eq!(decode_all_i32(&data), EXAMPLE3_SAMPLES.to_vec());
}

/// Simple deterministic xorshift generator.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
}

#[test]
fn mutated_examples_never_panic() {
    let mut rng = Rng(0x1234_5678_9abc_def1);
    for base in [example1(), example2(), example3()] {
        for _ in 0..3000 {
            let mut d = base.clone();
            let n = 1 + rng.next() % 4;
            for _ in 0..n {
                let i = (rng.next() % d.len() as u64) as usize;
                d[i] = rng.next() as u8;
            }
            if rng.next() & 3 == 0 {
                let len = (rng.next() % d.len() as u64) as usize;
                d.truncate(len);
            }
            if let Ok(mut dec) = FlacDecoder::new(Cursor::new(&d)) {
                let mut out = Vec::new();
                let mut calls = 0;
                while let Ok(true) = dec.next_chunk(&mut out) {
                    calls += 1;
                    assert!(calls < 100_000);
                }
                let _ = dec.seek(rng.next() % 40);
                let _ = dec.next_chunk(&mut out);
            }
            if let Ok(mut fd) = FlacFrameDecoder::new(&d) {
                let mut out = Vec::new();
                let start = d.len().min(0x2a);
                let _ = fd.decode(&d[start..], &mut out);
            }
        }
    }
}
