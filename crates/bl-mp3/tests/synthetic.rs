//! Streams synthesized in the test (no encoder available for these):
//! random Layer I frames (incl. joint stereo) and free-format Layer III,
//! compared against ffmpeg / the regular decode.

mod common;

use common::*;

struct BitWriter {
    out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    fn new() -> Self {
        BitWriter {
            out: Vec::new(),
            acc: 0,
            n: 0,
        }
    }
    fn put(&mut self, v: u32, bits: u32) {
        for i in (0..bits).rev() {
            self.acc = (self.acc << 1) | u64::from((v >> i) & 1);
            self.n += 1;
            if self.n == 8 {
                self.out.push(self.acc as u8);
                self.acc = 0;
                self.n = 0;
            }
        }
    }
    fn bits(&self) -> usize {
        self.out.len() * 8 + self.n as usize
    }
}

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
}

/// One MPEG-1 Layer I frame, 48 kHz, 384 kbit/s (384 bytes), random
/// allocations/scale factors/samples in the lower subbands.
fn layer1_frame(rng: &mut Rng, joint: Option<u32>) -> Vec<u8> {
    let mut w = BitWriter::new();
    let (mode, ext) = match joint {
        Some(e) => (1, e),
        None => (0, 0),
    };
    w.put(0xFFF, 12);
    w.put(1, 1); // MPEG-1
    w.put(3, 2); // layer I
    w.put(1, 1); // no CRC
    w.put(12, 4); // 384 kbit/s
    w.put(1, 2); // 48 kHz
    w.put(0, 1); // padding
    w.put(0, 1); // private
    w.put(mode, 2);
    w.put(ext, 2);
    w.put(0, 4); // copyright, original, emphasis
    let bound = if joint.is_some() {
        4 * (ext as usize + 1)
    } else {
        32
    };
    let mut alloc = [[0u32; 2]; 32];
    for (sb, a) in alloc.iter_mut().enumerate() {
        let used = sb < 10 && rng.next() & 3 != 0;
        let v = if used { 1 + rng.next() % 6 } else { 0 };
        a[0] = v;
        a[1] = if sb < bound {
            if used {
                1 + rng.next() % 6
            } else {
                0
            }
        } else {
            v
        };
    }
    for (sb, a) in alloc.iter().enumerate() {
        w.put(a[0], 4);
        if sb < bound {
            w.put(a[1], 4);
        }
    }
    for a in alloc.iter() {
        for &x in a {
            if x != 0 {
                w.put(rng.next() % 50 + 5, 6);
            }
        }
    }
    for _ in 0..12 {
        for (sb, a) in alloc.iter().enumerate() {
            let chans = if sb < bound { 2 } else { 1 };
            for &x in a.iter().take(chans) {
                if x != 0 {
                    let nb = x + 1;
                    w.put(rng.next() % ((1 << nb) - 1), nb);
                }
            }
        }
    }
    assert!(w.bits() <= 384 * 8);
    while w.bits() < 384 * 8 {
        w.put(0, 1);
    }
    w.out
}

#[test]
fn layer1_random_frames_vs_ffmpeg() {
    if !ffmpeg_available() {
        eprintln!("ffmpeg not available, skipping");
        return;
    }
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    let mut data = Vec::new();
    for i in 0..300 {
        let joint = if (i / 30) % 2 == 1 {
            Some((i / 60) as u32 % 4)
        } else {
            None
        };
        data.extend(layer1_frame(&mut rng, joint));
    }
    let path = fixture_dir().join("synthetic_layer1.mp1");
    std::fs::write(&path, &data).unwrap();
    let ours = decode_bytes(&data);
    assert_eq!(ours.sample_rate, 48000);
    assert_eq!(ours.channels, 2);
    let reference = ffmpeg_decode(&path, &["-f", "mp3", "-c:a", "mp1float"]);
    let (snr, max) = snr(&reference, &ours.pcm);
    println!(
        "layer I synthetic: {} vs {} samples, SNR {snr:.1} dB, max err {max:.2e}",
        ours.pcm.len(),
        reference.len()
    );
    assert_eq!(ours.pcm.len(), 300 * 384 * 2);
    assert_eq!(ours.pcm.len(), reference.len());
    assert!(snr > 80.0, "SNR {snr:.1} dB");
}

/// Rewrites every frame header of a CBR MPEG-1 Layer III stream to the
/// free-format bitrate index; the decoded audio must not change.
#[test]
fn free_format_layer3() {
    let args = [
        "-f",
        "lavfi",
        "-i",
        "sine=f=440:d=2",
        "-ac",
        "2",
        "-c:a",
        "libmp3lame",
        "-b:a",
        "128k",
    ];
    let Some(path) = gen("free_src.mp3", &args) else {
        return;
    };
    let mut data = std::fs::read(&path).unwrap();
    let plain = decode_bytes(&data);
    let mut pos = 0;
    if &data[..3] == b"ID3" {
        let s = &data[6..10];
        pos = 10
            + ((s[0] as usize) << 21
                | (s[1] as usize) << 14
                | (s[2] as usize) << 7
                | s[3] as usize);
    }
    let mut frames = 0;
    while pos + 4 <= data.len() && data[pos] == 0xFF && data[pos + 1] == 0xFB {
        let len = 144 * 128_000 / 44_100 + usize::from(data[pos + 2] & 2 != 0);
        data[pos + 2] &= 0x0F;
        pos += len;
        frames += 1;
    }
    assert!(frames > 50);
    let free = decode_bytes(&data);
    assert_eq!(free.total_frames, plain.total_frames);
    assert!(free.pcm == plain.pcm, "free-format decode differs");
}
