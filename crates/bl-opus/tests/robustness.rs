//! Robustness (malformed / truncated / random packets), concealment, other
//! output rates and decoding speed.

mod common;
use common::*;
use bl_opus::{OpusDecoder, OpusHead};
use std::time::Instant;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (self.0 >> 33) as u32
    }
}

fn head(channels: u8, family: u8, streams: u8, coupled: u8, mapping: &[u8]) -> OpusHead {
    let mut h = Vec::new();
    h.extend_from_slice(b"OpusHead");
    h.push(1);
    h.push(channels);
    h.extend_from_slice(&312u16.to_le_bytes());
    h.extend_from_slice(&48000u32.to_le_bytes());
    h.extend_from_slice(&0i16.to_le_bytes());
    h.push(family);
    if family != 0 {
        h.push(streams);
        h.push(coupled);
        h.extend_from_slice(mapping);
    }
    OpusHead::parse(&h).unwrap()
}

/// Random packets with every TOC configuration must never panic.
#[test]
fn random_packets_do_not_panic() {
    let heads = [
        head(1, 0, 1, 0, &[]),
        head(2, 0, 1, 1, &[]),
        head(6, 1, 4, 2, &[0, 4, 1, 2, 3, 5]),
        head(3, 255, 3, 0, &[0, 1, 255]),
    ];
    let seed = std::env::var("OC_OPUS_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(12345);
    let mut rng = Rng(seed);
    for h in &heads {
        for &rate in &[48000u32, 24000, 16000, 12000, 8000] {
            let mut dec = OpusDecoder::new(h, rate).unwrap();
            let mut out = Vec::new();
            let iters = std::env::var("OC_OPUS_FUZZ_ITERS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(if cfg!(debug_assertions) { 2000 } else { 8000 });
            let nstreams = h.stream_count.max(1) as usize;
            for iter in 0..iters {
                if nstreams > 1 && iter % 2 == 0 {
                    // Well-formed multistream framing around random frames.
                    let toc = (rng.next() as u8) & !0x03;
                    let mut pkt = Vec::new();
                    for s in 0..nstreams {
                        let len = (rng.next() % 200) as usize;
                        pkt.push(toc);
                        if s + 1 < nstreams {
                            pkt.push(len as u8);
                        }
                        pkt.extend((0..len).map(|_| rng.next() as u8));
                    }
                    out.clear();
                    let _ = dec.decode_packet(&pkt, &mut out);
                    assert!(out.iter().all(|v| v.is_finite()));
                    continue;
                }
                let len = match iter % 4 {
                    0 => (rng.next() % 8) as usize,
                    1 => (rng.next() % 64) as usize,
                    2 => (rng.next() % 400) as usize,
                    _ => (rng.next() % 1600) as usize,
                };
                let mut pkt: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
                if !pkt.is_empty() && iter % 3 == 0 {
                    // Bias towards valid framing: code 0..2, sane sizes.
                    pkt[0] &= !0x03;
                    pkt[0] |= (rng.next() % 3) as u8;
                }
                out.clear();
                let _ = dec.decode_packet(&pkt, &mut out);
                assert!(out.iter().all(|v| v.is_finite()));
                if iter % 97 == 0 {
                    dec.reset();
                }
            }
        }
    }
}

/// Mutated / truncated real packets (when ffmpeg is available).
#[test]
fn corrupted_real_packets_do_not_panic() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let dir = work_dir();
    let cases: &[(&str, Signal, u32, usize, &[&str])] = &[
        ("rb_celt", Signal::Music, 48000, 2, &["-b:a", "96k"]),
        (
            "rb_hybrid",
            Signal::Mixed,
            48000,
            2,
            &[
                "-b:a",
                "32k",
                "-frame_duration",
                "40",
                "-packet_loss",
                "10",
                "-fec",
                "1",
            ],
        ),
        (
            "rb_silk",
            Signal::Speech,
            16000,
            1,
            &[
                "-b:a",
                "12k",
                "-application",
                "voip",
                "-frame_duration",
                "60",
            ],
        ),
        (
            "rb_51",
            Signal::Mixed,
            48000,
            6,
            &["-b:a", "128k", "-mapping_family", "1"],
        ),
    ];
    let mut rng = Rng(777);
    for (name, sig, rate, ch, args) in cases {
        let raw = dir.join(format!("{name}.raw"));
        let opus = dir.join(format!("{name}.opus"));
        write_f32(&raw, &gen_signal(*sig, *rate, *ch, 2.0));
        if !encode(&raw, *rate, *ch, args, &opus) {
            continue;
        }
        let _ = std::fs::remove_file(&raw);
        let ogg = read_ogg_opus(&opus);
        for round in 0..6 {
            let mut dec = OpusDecoder::new(&ogg.head, 48000).unwrap();
            let mut out = Vec::new();
            for p in &ogg.packets {
                let mut p = p.clone();
                match round % 3 {
                    0 => {
                        for _ in 0..1 + rng.next() % 4 {
                            if !p.is_empty() {
                                let i = rng.next() as usize % p.len();
                                p[i] ^= 1 << (rng.next() % 8);
                            }
                        }
                    }
                    1 => {
                        let l = rng.next() as usize % (p.len() + 1);
                        p.truncate(l);
                    }
                    _ => {
                        if rng.next() % 5 < 1 {
                            p.clear(); // simulated loss -> concealment
                        } else if !p.is_empty() {
                            let i = 1 + rng.next() as usize % p.len().max(2).saturating_sub(1);
                            if i < p.len() {
                                p[i] = rng.next() as u8;
                            }
                        }
                    }
                }
                out.clear();
                let _ = dec.decode_packet(&p, &mut out);
                assert!(out.iter().all(|v| v.is_finite()));
            }
        }
    }
}

/// Empty packets conceal with the previous packet's duration.
#[test]
fn packet_loss_concealment() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let dir = work_dir();
    for (name, sig, rate, ch, args) in [
        (
            "plc_celt",
            Signal::Music,
            48000u32,
            2usize,
            &["-b:a", "64k"][..],
        ),
        (
            "plc_silk",
            Signal::Speech,
            16000,
            1,
            &["-b:a", "16k", "-application", "voip"][..],
        ),
        (
            "plc_hybrid",
            Signal::Speech,
            48000,
            2,
            &[
                "-b:a",
                "28k",
                "-application",
                "voip",
                "-frame_duration",
                "10",
            ][..],
        ),
    ] {
        let raw = dir.join(format!("{name}.raw"));
        let opus = dir.join(format!("{name}.opus"));
        write_f32(&raw, &gen_signal(sig, rate, ch, 2.0));
        if !encode(&raw, rate, ch, args, &opus) {
            continue;
        }
        let _ = std::fs::remove_file(&raw);
        let ogg = read_ogg_opus(&opus);
        let mut dec = OpusDecoder::new(&ogg.head, 48000).unwrap();
        let mut out = Vec::new();
        let mut total = 0;
        let mut expected = 0;
        for (i, p) in ogg.packets.iter().enumerate() {
            let n = bl_opus::packet_samples_48k(p).unwrap();
            let lost = i % 7 == 3 || i % 23 == 5;
            let got = dec
                .decode_packet(if lost { &[] } else { p }, &mut out)
                .unwrap();
            assert_eq!(got, n, "{name}: packet {i} (lost={lost})");
            total += got;
            expected += n;
        }
        assert_eq!(total, expected);
        assert_eq!(out.len(), total * ch);
        assert!(out.iter().all(|v| v.is_finite() && v.abs() < 4.0));
        println!("{name}: concealment ok ({} packets)", ogg.packets.len());
    }
}

/// Decoding at 8/12/16/24 kHz: correct lengths and energy close to the 48 kHz decode.
#[test]
fn other_output_rates() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let dir = work_dir();
    for (name, sig, rate, ch, args) in [
        (
            "rates_celt",
            Signal::Music,
            48000u32,
            2usize,
            &["-b:a", "96k"][..],
        ),
        (
            "rates_silk",
            Signal::Speech,
            16000,
            1,
            &["-b:a", "16k", "-application", "voip"][..],
        ),
        ("rates_mixed", Signal::Mixed, 48000, 2, &["-b:a", "32k"][..]),
    ] {
        let raw = dir.join(format!("{name}.raw"));
        let opus = dir.join(format!("{name}.opus"));
        write_f32(&raw, &gen_signal(sig, rate, ch, 3.0));
        if !encode(&raw, rate, ch, args, &opus) {
            continue;
        }
        let _ = std::fs::remove_file(&raw);
        let ogg = read_ogg_opus(&opus);
        let full = our_decode(&ogg, 48000);
        let rms = |v: &[f32]| {
            (v.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
        };
        let r48 = rms(&full);
        for out_rate in [24000u32, 16000, 12000, 8000] {
            let pcm = our_decode(&ogg, out_rate);
            let expect =
                (ogg.last_granule - ogg.head.pre_skip as u64) * out_rate as u64 / 48000 * ch as u64;
            assert!(
                (pcm.len() as i64 - expect as i64).abs() <= ch as i64,
                "{name}@{out_rate}: len {} vs {}",
                pcm.len(),
                expect
            );
            let r = rms(&pcm);
            println!("{name}@{out_rate}: rms {:.4} (48k: {:.4})", r, r48);
            assert!(
                r > 0.3 * r48 && r < 1.5 * r48,
                "{name}@{out_rate}: rms {r} vs {r48}"
            );
        }
    }
}

#[test]
fn decode_speed() {
    if !ffmpeg_available() {
        eprintln!("NOTE: ffmpeg with libopus not found, skipping");
        return;
    }
    let dir = work_dir();
    for (name, sig, rate, ch, args) in [
        (
            "speed_celt_stereo_128k",
            Signal::Music,
            48000u32,
            2usize,
            &["-b:a", "128k"][..],
        ),
        (
            "speed_hybrid_stereo_32k",
            Signal::Speech,
            48000,
            2,
            &["-b:a", "32k", "-application", "voip"][..],
        ),
        (
            "speed_silk_wb_mono",
            Signal::Speech,
            16000,
            1,
            &["-b:a", "16k", "-application", "voip"][..],
        ),
        (
            "speed_5.1_256k",
            Signal::Music,
            48000,
            6,
            &["-b:a", "256k", "-mapping_family", "1"][..],
        ),
    ] {
        let raw = dir.join(format!("{name}.raw"));
        let opus = dir.join(format!("{name}.opus"));
        write_f32(&raw, &gen_signal(sig, rate, ch, 10.0));
        if !encode(&raw, rate, ch, args, &opus) {
            continue;
        }
        let _ = std::fs::remove_file(&raw);
        let ogg = read_ogg_opus(&opus);
        let mut out = Vec::with_capacity(48000 * 11 * ch);
        // Best of several runs (the machine may be shared).
        let mut best = f64::MAX;
        let mut samples = 0usize;
        for _ in 0..5 {
            let t = Instant::now();
            let mut dec = OpusDecoder::new(&ogg.head, 48000).unwrap();
            out.clear();
            samples = 0;
            for p in &ogg.packets {
                samples += dec.decode_packet(p, &mut out).unwrap();
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        let secs = best;
        let audio = samples as f64 / 48000.0;
        println!(
            "{name}: {:.1}x realtime ({:.2} s audio in {:.3} s)",
            audio / secs,
            audio,
            secs
        );
    }
}
