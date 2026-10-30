//! Encoder tests: every packet must decode with the decoder of this crate
//! with the same final range coder state (so encoder and decoder agree on
//! every coded symbol), at the requested size, and the decoded audio must
//! match the input within quality limits.

use bl_opus::{OpusDecoder, OpusEncoder, OpusHead};

const PRE_SKIP: usize = OpusEncoder::LOOKAHEAD;

struct Lcg(u64);
impl Lcg {
    fn next(&mut self) -> f32 {
        self.0 = self
            .0
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

fn head(channels: u8) -> OpusHead {
    let mut h = b"OpusHead".to_vec();
    h.extend_from_slice(&[1, channels]);
    h.extend_from_slice(&(PRE_SKIP as u16).to_le_bytes());
    h.extend_from_slice(&48000u32.to_le_bytes());
    h.extend_from_slice(&[0, 0, 0]);
    OpusHead::parse(&h).unwrap()
}

#[derive(Clone, Copy, Debug)]
enum Sig {
    Sine1k,
    Sweep,
    Noise,
    StereoMix,
    Clicks,
    /// Frames alternating between silence, a full-scale square wave, quiet
    /// noise and a sine (with a DC offset).
    Switching,
    Silence,
}

fn gen(sig: Sig, channels: usize, frames: usize) -> Vec<f32> {
    let n = frames * 960;
    let mut out = vec![0f32; n * channels];
    let mut rng = Lcg(7);
    let tau = std::f32::consts::TAU;
    for i in 0..n {
        let t = i as f32 / 48000.0;
        for c in 0..channels {
            out[i * channels + c] = match sig {
                Sig::Sine1k => 0.5 * (tau * 1000.0 * t).sin(),
                Sig::Sweep => {
                    // 50 Hz .. 16 kHz exponential sweep.
                    let dur = n as f32 / 48000.0;
                    let k = (16000.0f32 / 50.0).ln() / dur;
                    0.4 * (tau * 50.0 * ((k * t).exp() - 1.0) / k).sin()
                }
                Sig::Noise => 0.3 * rng.next(),
                Sig::StereoMix => {
                    let a = 0.3 * (tau * 220.0 * t).sin() + 0.15 * (tau * 3300.0 * t).sin();
                    let b = 0.25 * (tau * 554.37 * t).sin() + 0.1 * (tau * 7000.0 * t).sin();
                    if c == 0 {
                        a + 0.5 * b
                    } else {
                        0.5 * a + b
                    }
                }
                Sig::Clicks => {
                    let ph = (i + 9600 - 500) % 9600;
                    let env = (-(ph as f32) / 400.0).exp();
                    0.6 * env * rng.next() + 0.1 * (tau * 440.0 * t).sin()
                }
                Sig::Switching => match (i / 960) % 4 {
                    0 => 0.0,
                    1 => {
                        if (t * 2000.0).fract() < 0.5 {
                            1.0
                        } else {
                            -1.0
                        }
                    }
                    2 => 0.01 * rng.next(),
                    _ => 0.3 + 0.5 * (tau * 300.0 * t).sin(),
                },
                Sig::Silence => 0.0,
            };
        }
    }
    out
}

struct Result {
    snr: f64,
    sizes: Vec<usize>,
    decoded: Vec<f32>,
}

fn run(sig: Sig, channels: usize, bitrate: u32, frames: usize) -> Result {
    run_with(sig, channels, bitrate, frames, true)
}

fn run_with(sig: Sig, channels: usize, bitrate: u32, frames: usize, transients: bool) -> Result {
    let input = gen(sig, channels, frames);
    let mut enc = OpusEncoder::new(channels as u8, bitrate).unwrap();
    enc.set_transients(transients);
    let mut dec = OpusDecoder::new(&head(channels as u8), 48000).unwrap();
    let mut decoded = Vec::new();
    let mut sizes = Vec::new();
    let mut packet = Vec::new();
    for (f, chunk) in input.chunks(960 * channels).enumerate() {
        packet.clear();
        let len = enc.encode(chunk, &mut packet).unwrap();
        assert_eq!(len, packet.len());
        sizes.push(len);
        let n = dec
            .decode_packet(&packet, &mut decoded)
            .unwrap_or_else(|e| panic!("{sig:?} frame {f}: {e}"));
        assert_eq!(n, 960);
        assert_eq!(
            dec.final_range(),
            enc.final_range(),
            "{sig:?} {channels}ch {bitrate}: range mismatch in frame {f}"
        );
    }
    // Align by the pre-skip and skip the first frames (start-up).
    let skip = 960 * 2;
    let mut sig_e = 0f64;
    let mut err_e = 0f64;
    for i in skip..frames * 960 - PRE_SKIP {
        for c in 0..channels {
            let x = input[i * channels + c] as f64;
            let y = decoded[(i + PRE_SKIP) * channels + c] as f64;
            sig_e += x * x;
            err_e += (x - y) * (x - y);
        }
    }
    Result {
        snr: 10.0 * (sig_e / err_e.max(1e-30)).log10(),
        sizes,
        decoded,
    }
}

#[test]
fn signals_decode_in_sync() {
    for &bitrate in &[48_000u32, 64_000, 96_000] {
        for &(sig, ch) in &[
            (Sig::Sine1k, 1),
            (Sig::Sine1k, 2),
            (Sig::Sweep, 1),
            (Sig::Sweep, 2),
            (Sig::Noise, 1),
            (Sig::Noise, 2),
            (Sig::StereoMix, 2),
            (Sig::Clicks, 1),
            (Sig::Clicks, 2),
        ] {
            let r = run(sig, ch, bitrate, 100);
            let target = bitrate as usize / 400;
            for &s in &r.sizes {
                assert!(
                    s.abs_diff(target) * 10 <= target,
                    "packet size {s} for {bitrate}"
                );
            }
            println!("{bitrate:>6} {sig:?} {ch}ch: SNR {:.1} dB", r.snr);
            // Waveform SNR floors, a few dB below what is measured (noise is
            // coded perceptually, not as a waveform).
            let floor = match sig {
                Sig::Sine1k => 30.0,
                Sig::Sweep => 20.0,
                Sig::StereoMix => 10.0,
                Sig::Clicks => 3.0,
                _ => -2.0,
            };
            assert!(r.snr > floor, "{sig:?} {ch}ch {bitrate}: {:.1} dB", r.snr);
        }
    }
}

/// Very low and very high bitrates exercise the budget corner cases (energy
/// coding running out of bits, skipped bands, huge pulse counts).
#[test]
fn extreme_bitrates_stay_in_sync() {
    for &bitrate in &[
        6_000u32, 8_000, 12_000, 16_000, 24_000, 32_000, 160_000, 256_000, 510_000,
    ] {
        for &(sig, ch) in &[
            (Sig::Sweep, 1),
            (Sig::Noise, 2),
            (Sig::StereoMix, 2),
            (Sig::Clicks, 2),
            (Sig::Switching, 1),
            (Sig::Switching, 2),
        ] {
            let r = run(sig, ch, bitrate, 25);
            assert!(r.decoded.iter().all(|v| v.is_finite()));
            if bitrate >= 160_000 && matches!(sig, Sig::Sweep | Sig::StereoMix) {
                assert!(r.snr > 25.0, "{sig:?} {ch}ch {bitrate}: {:.1} dB", r.snr);
            }
        }
    }
}

/// Short blocks on transients reduce the pre-echo: the coding error in
/// the quiet part just before each click.
#[test]
fn transients_use_short_blocks() {
    let frames = 50;
    for &ch in &[1usize, 2] {
        let input = gen(Sig::Clicks, ch, frames);
        let pre_echo = |transients: bool| {
            let r = run_with(Sig::Clicks, ch, 64_000, frames, transients);
            let mut e = 0f64;
            // Click onsets are at 500 + k * 9600 samples.
            for onset in (500 + 9600..frames * 960 - 960).step_by(9600) {
                for i in onset - 400..onset {
                    for c in 0..ch {
                        let d = input[i * ch + c] - r.decoded[(i + PRE_SKIP) * ch + c];
                        e += (d as f64).powi(2);
                    }
                }
            }
            e
        };
        let with = pre_echo(true);
        let without = pre_echo(false);
        let gain = 10.0 * (without / with).log10();
        println!("clicks {ch}ch: pre-echo {gain:.1} dB lower with short blocks");
        assert!(gain > 3.0);
    }
}

#[test]
fn silence_stays_silent_and_small() {
    for ch in 1..=2 {
        let r = run(Sig::Silence, ch, 64_000, 20);
        assert!(r.decoded.iter().all(|v| v.abs() < 1e-6));
        assert!(r.sizes.iter().all(|&s| s == 3));
    }
}

mod common;

/// Music-like test material: our encoder against libopus (CELT-only, CBR,
/// via ffmpeg), both decoded with bl-opus. Skipped without ffmpeg/libopus.
#[test]
fn quality_vs_libopus() {
    if !common::ffmpeg_available() {
        eprintln!("ffmpeg with libopus not available, skipping");
        return;
    }
    let dir = common::work_dir();
    for &ch in &[1usize, 2] {
        let input = common::gen_signal(common::Signal::Music, 48000, ch, 5.0);
        let raw = dir.join(format!("music{ch}.f32"));
        common::write_f32(&raw, &input);
        for &bitrate in &[48_000u32, 64_000, 96_000] {
            // Ours.
            let mut enc = OpusEncoder::new(ch as u8, bitrate).unwrap();
            let mut dec = OpusDecoder::new(&head(ch as u8), 48000).unwrap();
            let mut ours = Vec::new();
            let mut padded = input.clone();
            padded.resize(input.len().div_ceil(960 * ch) * 960 * ch + 960 * ch, 0.0);
            let mut pkt = Vec::new();
            for f in padded.chunks(960 * ch) {
                pkt.clear();
                enc.encode(f, &mut pkt).unwrap();
                dec.decode_packet(&pkt, &mut ours).unwrap();
                assert_eq!(dec.final_range(), enc.final_range());
            }
            let ours = &ours[PRE_SKIP * ch..];
            // libopus.
            let out = dir.join(format!("music{ch}_{bitrate}.opus"));
            let br = format!("{}k", bitrate / 1000);
            let args = ["-application", "lowdelay", "-vbr", "off", "-b:a", &br];
            assert!(common::encode(&raw, 48000, ch, &args, &out));
            let theirs = common::our_decode(&common::read_ogg_opus(&out), 48000);
            let skip = 4800 * ch;
            let (snr_ours, _, _) = common::compare(&input[skip..], &ours[skip..input.len()]);
            let (snr_ref, _, _) = common::compare(&input[skip..], &theirs[skip..]);
            println!("music {ch}ch {bitrate}: ours {snr_ours:.1} dB, libopus {snr_ref:.1} dB");
            assert!(snr_ours > snr_ref - 3.0);
        }
    }
}

/// Encoding speed (meaningful with `--release`).
#[test]
fn encode_speed() {
    for &(ch, bitrate) in &[(2usize, 96_000u32), (2, 64_000), (1, 48_000)] {
        let input = common::gen_signal(common::Signal::Music, 48000, ch, 10.0);
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t = std::time::Instant::now();
            let mut enc = OpusEncoder::new(ch as u8, bitrate).unwrap();
            let mut pkt = Vec::new();
            for f in input.chunks_exact(960 * ch) {
                pkt.clear();
                enc.encode(f, &mut pkt).unwrap();
            }
            best = best.min(t.elapsed().as_secs_f64());
        }
        println!(
            "encode {ch}ch {bitrate}: {:.0}x realtime",
            input.len() as f64 / ch as f64 / 48000.0 / best
        );
    }
}
