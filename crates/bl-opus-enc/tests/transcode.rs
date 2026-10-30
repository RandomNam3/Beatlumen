//! End-to-end tests: WAV (via `bl_media`) -> Transcoder -> Ogg Opus, decoded
//! with `bl-opus` (and with ffmpeg's decoders when ffmpeg is installed).

use bl_opus::{OpusDecoder, OpusHead};
use bl_opus_enc::{transcode, OggOpusWriter, OpusEncoder, TranscodeOptions, Transcoder};
use std::io::{Cursor, Read};
use std::path::PathBuf;
use std::process::Command;

/// 16-bit PCM WAV file from interleaved samples.
fn wav(samples: &[f32], rate: u32, channels: u16) -> Vec<u8> {
    let data_len = samples.len() as u32 * 2;
    let mut w = Vec::new();
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVEfmt ");
    w.extend_from_slice(&16u32.to_le_bytes());
    w.extend_from_slice(&1u16.to_le_bytes());
    w.extend_from_slice(&channels.to_le_bytes());
    w.extend_from_slice(&rate.to_le_bytes());
    w.extend_from_slice(&(rate * channels as u32 * 2).to_le_bytes());
    w.extend_from_slice(&(channels * 2).to_le_bytes());
    w.extend_from_slice(&16u16.to_le_bytes());
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    for &s in samples {
        let v = (s * 32767.0).round().clamp(-32768.0, 32767.0) as i16;
        w.extend_from_slice(&v.to_le_bytes());
    }
    w
}

/// Two tones per channel (different per channel), as a function of time.
fn tone(t: f64, c: usize) -> f32 {
    let tau = std::f64::consts::TAU;
    let (f1, f2) = if c == 0 {
        (1000.0, 3150.0)
    } else {
        (660.0, 5200.0)
    };
    (0.4 * (tau * f1 * t).sin() + 0.1 * (tau * f2 * t).sin()) as f32
}

fn gen(rate: u32, channels: usize, secs: f64) -> Vec<f32> {
    let n = (rate as f64 * secs) as usize;
    let mut v = Vec::with_capacity(n * channels);
    for i in 0..n {
        for c in 0..channels {
            v.push(tone(i as f64 / rate as f64, c.min(1)));
        }
    }
    v
}

fn open_wav(data: Vec<u8>) -> Box<dyn bl_media::Decoder> {
    bl_media::open(Box::new(Cursor::new(data)), Some("wav")).unwrap()
}

/// Decodes an Ogg Opus stream with `bl-opus`, applying pre-skip and end
/// trimming. Returns (channels, samples, packet sizes).
fn decode(ogg: &[u8]) -> (usize, Vec<f32>, Vec<usize>) {
    let mut r = bl_ogg::PacketReader::new(Cursor::new(ogg));
    let head_pkt = r.next_packet().unwrap().unwrap();
    assert!(head_pkt.bos);
    let head = OpusHead::parse(&head_pkt.data).unwrap();
    let tags = r.next_packet().unwrap().unwrap();
    assert!(tags.data.starts_with(b"OpusTags"));
    let mut dec = OpusDecoder::new(&head, 48000).unwrap();
    let ch = head.channels as usize;
    let mut out = Vec::new();
    let mut sizes = Vec::new();
    let mut last_granule = 0;
    let mut eos = false;
    while let Some(p) = r.next_packet().unwrap() {
        assert!(!eos, "packet after end of stream");
        dec.decode_packet(&p.data, &mut out).unwrap();
        sizes.push(p.data.len());
        if let Some(g) = p.granule {
            assert!(g >= last_granule);
            last_granule = g;
        }
        eos = p.eos;
    }
    assert!(eos, "missing end-of-stream page");
    let total = last_granule as usize - head.pre_skip as usize;
    let skip = head.pre_skip as usize * ch;
    assert!(out.len() >= skip + total * ch);
    (ch, out[skip..skip + total * ch].to_vec(), sizes)
}

/// SNR (dB) of `y` against `x` over the given frame range.
fn snr(x: &[f32], y: &[f32], ch: usize, range: std::ops::Range<usize>) -> f64 {
    let (mut s, mut e) = (0f64, 0f64);
    for i in range {
        for c in 0..ch {
            let a = x[i * ch + c] as f64;
            let b = y[i * ch + c] as f64;
            s += a * a;
            e += (a - b) * (a - b);
        }
    }
    10.0 * (s / e.max(1e-30)).log10()
}

fn transcode_all(input: Vec<u8>, opts: &TranscodeOptions, read_size: usize) -> Vec<u8> {
    let mut t = Transcoder::with_options(open_wav(input), opts).unwrap();
    let mut out = Vec::new();
    let mut buf = vec![0u8; read_size];
    loop {
        let n = t.read(&mut buf).unwrap();
        if n == 0 {
            break;
        }
        out.extend_from_slice(&buf[..n]);
    }
    out
}

#[test]
fn transcode_44k_stereo() {
    let secs = 3.0;
    let input = gen(44100, 2, secs);
    for &bitrate in &[48_000u32, 64_000, 96_000] {
        let ogg = transcode_all(wav(&input, 44100, 2), &TranscodeOptions::new(bitrate), 777);
        let (ch, pcm, sizes) = decode(&ogg);
        assert_eq!(ch, 2);
        // Same duration as the input (sample exact at 48 kHz).
        let expected = (input.len() / 2 * 48000).div_ceil(44100);
        assert_eq!(pcm.len() / 2, expected);
        // CBR packet sizes.
        let target = bitrate as usize / 400;
        assert!(sizes.iter().all(|&s| s == target), "{bitrate}: {sizes:?}");
        // Compare with the ideal 48 kHz signal (time aligned).
        let reference = gen(48000, 2, secs);
        let n = expected.min(reference.len() / 2);
        let q = snr(&reference, &pcm, 2, 4800..n - 4800);
        println!(
            "44.1 kHz stereo two-tone at {bitrate}: SNR {q:.1} dB, {} bytes",
            ogg.len()
        );
        assert!(q > 12.0, "{bitrate}: SNR {q:.1} dB");
        // Byte rate within 10 % of the bitrate (Ogg overhead included).
        let rate = ogg.len() as f64 * 8.0 / secs;
        assert!((rate / bitrate as f64 - 1.0).abs() < 0.1, "{rate}");
    }
}

#[test]
fn transcode_48k_mono_and_multichannel() {
    // 48 kHz mono: no resampling, pre-skip = encoder look-ahead.
    let input = gen(48000, 1, 1.0);
    let mut t = transcode(open_wav(wav(&input, 48000, 1)), 64000).unwrap();
    assert_eq!(t.pre_skip(), OpusEncoder::LOOKAHEAD as u64);
    let mut ogg = Vec::new();
    t.read_to_end(&mut ogg).unwrap();
    let (ch, pcm, _) = decode(&ogg);
    assert_eq!((ch, pcm.len()), (1, input.len()));
    let q = snr(&input, &pcm, 1, 4800..input.len() - 4800);
    assert!(q > 20.0, "mono SNR {q:.1} dB");

    // 6 channels at 32 kHz are downmixed to stereo.
    let six = gen(32000, 6, 0.5);
    let ogg = transcode_all(wav(&six, 32000, 6), &TranscodeOptions::new(96000), 4096);
    let (ch, pcm, _) = decode(&ogg);
    assert_eq!((ch, pcm.len() / 2), (2, 24000));
    assert!(pcm.iter().any(|v| v.abs() > 0.1));

    // Forced mono with tags.
    let mut opts = TranscodeOptions::new(48000);
    opts.mono = true;
    opts.tags.push(("TITLE".into(), "Test tone".into()));
    let ogg = transcode_all(wav(&gen(44100, 2, 0.5), 44100, 2), &opts, 1 << 16);
    let (ch, pcm, _) = decode(&ogg);
    assert_eq!((ch, pcm.len()), (1, 24000));
    assert!(ogg.windows(15).any(|w| w == b"TITLE=Test tone"));
}

#[test]
fn silence_and_tiny_input() {
    let ogg = transcode_all(
        wav(&vec![0.0; 44100 * 2], 44100, 2),
        &TranscodeOptions::new(64000),
        999,
    );
    let (_, pcm, sizes) = decode(&ogg);
    assert_eq!(pcm.len() / 2, 48000);
    assert!(pcm.iter().all(|v| v.abs() < 1e-6));
    assert!(sizes.iter().all(|&s| s == 3));
    // A few samples only.
    let ogg = transcode_all(
        wav(&[0.5, -0.5, 0.25, 0.1], 44100, 2),
        &TranscodeOptions::new(64000),
        999,
    );
    let (_, pcm, _) = decode(&ogg);
    assert_eq!(pcm.len() / 2, 3);
    // Empty input.
    let ogg = transcode_all(wav(&[], 44100, 2), &TranscodeOptions::new(64000), 999);
    let (_, pcm, _) = decode(&ogg);
    assert!(pcm.is_empty());
}

#[test]
fn rejects_bad_parameters() {
    let d = open_wav(wav(&[0.0; 100], 44100, 2));
    assert!(transcode(d, 1000).is_err());
    assert!(OpusEncoder::new(3, 64000).is_err());
    let mut e = OpusEncoder::new(2, 64000).unwrap();
    assert!(e.encode(&[0.0; 100], &mut Vec::new()).is_err());
    // Garbage input never panics and still produces valid packets.
    let mut pkt = Vec::new();
    let mut frame = vec![0f32; 1920];
    for (i, v) in frame.iter_mut().enumerate() {
        *v = match i % 5 {
            0 => f32::NAN,
            1 => f32::INFINITY,
            2 => -1e30,
            3 => 1e-40,
            _ => (i as f32).sin() * 4.0,
        };
    }
    let head = OpusHead::parse(b"OpusHead\x01\x02\x78\x00\x80\xbb\x00\x00\x00\x00\x00").unwrap();
    let mut dec = OpusDecoder::new(&head, 48000).unwrap();
    let mut out = Vec::new();
    for _ in 0..5 {
        pkt.clear();
        e.encode(&frame, &mut pkt).unwrap();
        dec.decode_packet(&pkt, &mut out).unwrap();
        assert_eq!(dec.final_range(), e.final_range());
    }
    assert!(out.iter().all(|v| v.is_finite()));
}

/// Whether a command line tool is installed (it could be started).
fn tool(name: &str) -> bool {
    let flag = if name == "ffmpeg" {
        "-version"
    } else {
        "--version"
    };
    Command::new(name).arg(flag).output().is_ok()
}

fn work_file(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bl-opus-enc-tests-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

/// ffmpeg's native Opus decoder and libopus (if ffmpeg has it) must accept
/// the stream and produce the same audio as `bl-opus`.
#[test]
fn external_decoders_accept_output() {
    if !tool("ffmpeg") {
        eprintln!("ffmpeg not installed, skipping");
        return;
    }
    let input = gen(44100, 2, 2.0);
    let ogg = transcode_all(wav(&input, 44100, 2), &TranscodeOptions::new(64000), 4096);
    let (_, ours, _) = decode(&ogg);
    let path = work_file("t.opus");
    std::fs::write(&path, &ogg).unwrap();
    let has_libopus = Command::new("ffmpeg")
        .args(["-hide_banner", "-decoders"])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).contains("libopus"))
        .unwrap_or(false);
    for codec in ["opus", "libopus"] {
        if codec == "libopus" && !has_libopus {
            continue;
        }
        let o = Command::new("ffmpeg")
            .args(["-v", "error", "-c:a", codec, "-i"])
            .arg(&path)
            .args(["-f", "f32le", "-ar", "48000", "-"])
            .output()
            .unwrap();
        assert!(
            o.status.success(),
            "{codec}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        assert!(
            o.stderr.is_empty(),
            "{codec}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
        let pcm: Vec<f32> = o
            .stdout
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        assert_eq!(pcm.len(), ours.len(), "{codec}: length");
        let q = snr(&ours, &pcm, 2, 0..ours.len() / 2);
        println!("ffmpeg {codec} vs bl-opus: {q:.1} dB");
        // Same audio up to float rounding.
        assert!(q > 60.0, "{codec}: {q:.1} dB");
    }
    for t in ["opusinfo", "opusdec"] {
        if !tool(t) {
            eprintln!("{t} not installed, skipping");
            continue;
        }
        let mut cmd = Command::new(t);
        if t == "opusdec" {
            cmd.arg(&path).arg(work_file("t.wav"));
        } else {
            cmd.arg(&path);
        }
        let o = cmd.output().unwrap();
        assert!(
            o.status.success(),
            "{t}: {}",
            String::from_utf8_lossy(&o.stderr)
        );
    }
}

/// The writer can also be used directly with the encoder.
#[test]
fn writer_with_encoder() {
    let mut enc = OpusEncoder::new(1, 48000).unwrap();
    let mut w = OggOpusWriter::new(Vec::new(), 1, OpusEncoder::LOOKAHEAD as u16).unwrap();
    let input = gen(48000, 1, 0.5);
    let mut frames = input.clone();
    frames.resize(input.len().div_ceil(960) * 960 + 960, 0.0);
    let mut pkt = Vec::new();
    for f in frames.chunks(960) {
        pkt.clear();
        enc.encode(f, &mut pkt).unwrap();
        w.write_packet(&pkt, 960).unwrap();
    }
    let ogg = w.finish(Some(input.len() as u64)).unwrap();
    let (_, pcm, _) = decode(&ogg);
    assert_eq!(pcm.len(), input.len());
}

/// Transcoding speed including WAV decoding and resampling (meaningful
/// with `--release`).
#[test]
fn transcode_speed() {
    let secs = 10.0;
    let data = wav(&gen(44100, 2, secs), 44100, 2);
    let t = std::time::Instant::now();
    let ogg = transcode_all(data, &TranscodeOptions::new(96000), 1 << 16);
    let el = t.elapsed().as_secs_f64();
    println!(
        "transcode 44.1 kHz stereo -> 96 kbit/s: {:.0}x realtime, {} bytes",
        secs / el,
        ogg.len()
    );
}
