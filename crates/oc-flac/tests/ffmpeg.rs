//! Integration tests against FLAC files produced by ffmpeg (generated at test
//! time; skipped with a note when ffmpeg is not installed).
//!
//! FLAC is lossless, so the decoded integers must equal both the PCM that
//! was fed to the encoder and ffmpeg's own decode, bit for bit.

use oc_flac::{FlacDecoder, MetadataBlock};
use std::io::Cursor;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Instant;

// ---------------------------------------------------------------------------
// Helpers

fn ffmpeg_available() -> bool {
    let ok = Command::new("ffmpeg")
        .arg("-version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        eprintln!("note: ffmpeg not found, skipping ffmpeg based FLAC tests");
    }
    ok
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join("oc_flac_tests");
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    /// Uniform in [-1, 1).
    fn uni(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 52) as f64 - 1.0
    }
}

/// Deterministic test signal (interleaved, integers at `bps`) with segments
/// that exercise the different subframe types: tonal (LPC), full-scale
/// noise (verbatim / escapes), digital silence and DC (constant), zeroed
/// low bits (wasted bits) and transients. Channels are correlated so that
/// the encoder picks stereo decorrelation.
fn gen_signal(channels: usize, rate: u32, bps: u32, frames: usize, seed: u64) -> Vec<i32> {
    let mut rng = Rng(seed | 1);
    let max = ((1i64 << (bps - 1)) - 1) as f64;
    let mut out = vec![0i32; frames * channels];
    let mut own = vec![0f64; channels];
    for i in 0..frames {
        let t = i as f64 / rate as f64;
        let pos = i as f64 / frames as f64;
        let base_tone = 0.45 * (2.0 * std::f64::consts::PI * 220.0 * t).sin() * (0.6 + 0.4 * (t * 1.3).sin())
            + 0.2 * (2.0 * std::f64::consts::PI * 1375.0 * t).sin()
            + 0.05 * (2.0 * std::f64::consts::PI * (300.0 + 2000.0 * pos) * t).sin();
        for c in 0..channels {
            let tone = 0.3 * (2.0 * std::f64::consts::PI * (330.0 + 110.0 * c as f64) * t).sin();
            own[c] = 0.97 * own[c] + 0.03 * rng.uni();
            let x = if pos < 0.40 {
                0.8 * base_tone + 0.2 * tone + 0.002 * rng.uni()
            } else if pos < 0.47 {
                0.999 * rng.uni()
            } else if pos < 0.52 {
                0.0
            } else if pos < 0.56 {
                0.25 - 0.05 * c as f64
            } else if pos < 0.72 {
                // Wasted bits: quantised below.
                0.7 * base_tone + 0.2 * tone
            } else {
                let k = (i % (rate as usize / 4)) as f64 / rate as f64;
                0.6 * (-k * 30.0).exp() * (2.0 * std::f64::consts::PI * 880.0 * k).sin()
                    + 0.3 * base_tone
                    + 0.05 * own[c]
                    + 0.0005 * rng.uni()
            };
            let mut v = (x * max).round().clamp(-max - 1.0, max) as i64;
            if (0.56..0.72).contains(&pos) && bps > 8 {
                v &= !0xf;
            }
            out[i * channels + c] = v as i32;
        }
    }
    out
}

/// Encodes `pcm` with ffmpeg and returns the FLAC file bytes.
fn encode(name: &str, pcm: &[i32], channels: usize, rate: u32, bps: u32, extra: &[&str]) -> Vec<u8> {
    let dir = tmp_dir();
    let pid = std::process::id();
    let raw = dir.join(format!("{name}_{pid}.raw"));
    let flac = dir.join(format!("{name}_{pid}.flac"));
    let (fmt, bytes): (&str, Vec<u8>) = if bps <= 16 {
        ("s16le", pcm.iter().flat_map(|&s| ((s << (16 - bps)) as i16).to_le_bytes()).collect())
    } else {
        ("s32le", pcm.iter().flat_map(|&s| (s << (32 - bps)).to_le_bytes()).collect())
    };
    std::fs::write(&raw, bytes).unwrap();
    let rate_s = rate.to_string();
    let ch_s = channels.to_string();
    let bps_s = bps.to_string();
    let mut args: Vec<&str> = vec![
        "-hide_banner", "-loglevel", "error", "-y", "-f", fmt, "-ar", &rate_s, "-ac", &ch_s, "-i",
        raw.to_str().unwrap(), "-c:a", "flac",
    ];
    if bps > 16 {
        args.extend_from_slice(&["-sample_fmt", "s32", "-bits_per_raw_sample", &bps_s, "-strict", "experimental"]);
    }
    args.extend_from_slice(extra);
    args.push(flac.to_str().unwrap());
    let out = Command::new("ffmpeg").args(&args).stdin(Stdio::null()).output().expect("run ffmpeg");
    assert!(out.status.success(), "ffmpeg encode failed: {}", String::from_utf8_lossy(&out.stderr));
    let data = std::fs::read(&flac).unwrap();
    let _ = std::fs::remove_file(&raw);
    let _ = std::fs::remove_file(&flac);
    data
}

/// Decodes a FLAC file with ffmpeg to interleaved s32 (left-aligned).
fn ffmpeg_decode_s32(name: &str, flac: &[u8]) -> Vec<i32> {
    let path = tmp_dir().join(format!("{name}_{}.ref.flac", std::process::id()));
    std::fs::write(&path, flac).unwrap();
    let out = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-i", path.to_str().unwrap(), "-f", "s32le", "-"])
        .stdin(Stdio::null())
        .output()
        .expect("run ffmpeg");
    let _ = std::fs::remove_file(&path);
    assert!(out.status.success(), "ffmpeg decode failed: {}", String::from_utf8_lossy(&out.stderr));
    out.stdout.chunks_exact(4).map(|b| i32::from_le_bytes([b[0], b[1], b[2], b[3]])).collect()
}

fn decode_i32(data: &[u8]) -> (Vec<i32>, u64) {
    let mut dec = FlacDecoder::new(Cursor::new(data)).expect("open");
    let mut out = Vec::new();
    while dec.next_chunk_i32(&mut out).expect("decode") {}
    (out, dec.corrupt_frames())
}

fn decode_f32(data: &[u8]) -> Vec<f32> {
    let mut dec = FlacDecoder::new(Cursor::new(data)).expect("open");
    let mut out = Vec::new();
    while dec.next_chunk(&mut out).expect("decode") {}
    out
}

// --- Minimal independent frame splitter (for building derived files) -----

fn crc8(d: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in d {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 7 } else { c << 1 };
        }
    }
    c
}

fn crc16(d: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in d {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
        }
    }
    c
}

/// Header length (incl. CRC-8) of a valid frame header at `d`, if any.
fn header_len(d: &[u8]) -> Option<usize> {
    if d.len() < 6 || d[0] != 0xff || d[1] & 0xfe != 0xf8 {
        return None;
    }
    let n = d[4].leading_ones() as usize;
    let mut len = 5 + if n == 0 { 0 } else { n - 1 };
    len += match d[2] >> 4 {
        6 => 1,
        7 => 2,
        _ => 0,
    };
    len += match d[2] & 15 {
        12 => 1,
        13 | 14 => 2,
        _ => 0,
    };
    (d.len() > len && crc8(&d[..len]) == d[len]).then_some(len + 1)
}

/// Returns (metadata end offset, frame byte ranges).
fn split_frames(data: &[u8]) -> (usize, Vec<(usize, usize)>) {
    let mut p = 4;
    loop {
        let last = data[p] & 0x80 != 0;
        let len = u32::from_be_bytes([0, data[p + 1], data[p + 2], data[p + 3]]) as usize;
        p += 4 + len;
        if last {
            break;
        }
    }
    let starts: Vec<usize> = (p..data.len()).filter(|&i| header_len(&data[i..]).is_some()).collect();
    let mut frames = Vec::new();
    let mut s = p;
    let mut idx = 0;
    while s < data.len() {
        let mut end = data.len();
        while idx < starts.len() {
            let e = starts[idx];
            idx += 1;
            if e > s + 2 && crc16(&data[s..e]) == 0 {
                end = e;
                break;
            }
        }
        frames.push((s, end));
        s = end;
    }
    (p, frames)
}

fn encode_coded_number(mut v: u64) -> Vec<u8> {
    if v < 0x80 {
        return vec![v as u8];
    }
    let mut n = 2;
    while n < 7 && v >= 1u64 << (5 * n + 1) {
        n += 1;
    }
    let mut out = vec![0u8; n];
    for i in (1..n).rev() {
        out[i] = 0x80 | (v & 0x3f) as u8;
        v >>= 6;
    }
    out[0] = (0xff00u16 >> n) as u8 | v as u8;
    out
}

fn streaminfo(data: &[u8]) -> Vec<u8> {
    data[8..42].to_vec()
}

fn block(t: u8, last: bool, payload: &[u8]) -> Vec<u8> {
    let mut b = vec![t | if last { 0x80 } else { 0 }];
    b.extend_from_slice(&(payload.len() as u32).to_be_bytes()[1..]);
    b.extend_from_slice(payload);
    b
}

/// Rebuilds a FLAC file from STREAMINFO + optional seek table + frames.
fn rebuild(si: &[u8], seek: Option<Vec<u8>>, frames: &[u8]) -> Vec<u8> {
    let mut f = b"fLaC".to_vec();
    f.extend(block(0, seek.is_none(), si));
    if let Some(st) = seek {
        f.extend(block(3, true, &st));
    }
    f.extend_from_slice(frames);
    f
}

/// Adds a SEEKTABLE with a point every `every` frames (offsets shifted by
/// `skew` bytes to simulate a broken table).
fn with_seek_table(data: &[u8], every: usize, skew: u64) -> Vec<u8> {
    let (meta_end, frames) = split_frames(data);
    let si = streaminfo(data);
    let bs = u16::from_be_bytes([si[2], si[3]]) as u64;
    let mut st = Vec::new();
    for (i, &(s, e)) in frames.iter().enumerate().step_by(every) {
        st.extend((i as u64 * bs).to_be_bytes());
        st.extend(((s - meta_end) as u64 + skew).to_be_bytes());
        st.extend(((e - s) as u16).to_be_bytes()); // not the sample count, unused
    }
    // A placeholder point, which must be ignored.
    st.extend(u64::MAX.to_be_bytes());
    st.extend([0u8; 10]);
    rebuild(&si, Some(st), &data[meta_end..])
}

// ---------------------------------------------------------------------------
// Tests

struct Case {
    name: &'static str,
    channels: usize,
    rate: u32,
    bps: u32,
    secs: f64,
    args: &'static [&'static str],
}

fn run_case(c: &Case) {
    let frames = (c.rate as f64 * c.secs) as usize;
    let pcm = gen_signal(c.channels, c.rate, c.bps, frames, 0x9e37_79b9 ^ c.name.len() as u64);
    let data = encode(c.name, &pcm, c.channels, c.rate, c.bps, c.args);

    let dec = FlacDecoder::new(Cursor::new(&data)).expect("open");
    assert_eq!(dec.sample_rate(), c.rate, "{}", c.name);
    assert_eq!(dec.channels() as usize, c.channels, "{}", c.name);
    assert_eq!(dec.bits_per_sample(), c.bps, "{}", c.name);
    assert_eq!(dec.total_frames(), Some(frames as u64), "{}", c.name);

    let (ours, corrupt) = decode_i32(&data);
    assert_eq!(corrupt, 0, "{}", c.name);
    assert_eq!(ours.len(), pcm.len(), "{}: length", c.name);
    let mismatch = ours.iter().zip(&pcm).position(|(a, b)| a != b);
    assert_eq!(mismatch, None, "{}: differs from encoder input", c.name);

    let reference = ffmpeg_decode_s32(c.name, &data);
    if reference.is_empty() {
        // ffmpeg's decoder rejects some valid streams (e.g. 65535-sample
        // blocks); the comparison with the encoder input above suffices.
        eprintln!("note: ffmpeg could not decode {}, compared with encoder input only", c.name);
    } else {
        assert_eq!(reference.len(), ours.len(), "{}: ffmpeg length", c.name);
    }
    let shift = 32 - c.bps;
    let mismatch = ours.iter().zip(&reference).position(|(a, b)| a << shift != *b);
    assert_eq!(mismatch, None, "{}: differs from ffmpeg decode", c.name);

    // f32 output is exactly the integers scaled by 2^-(bps-1).
    let f = decode_f32(&data);
    let scale = 1.0 / (1u64 << (c.bps - 1)) as f32;
    let mismatch = f.iter().zip(&ours).position(|(&a, &b)| a != b as f32 * scale);
    assert_eq!(mismatch, None, "{}: f32 output", c.name);
    assert!(f.iter().all(|v| (-1.0..=1.0).contains(v)));
    eprintln!(
        "{:<22} {}ch {:>6} Hz {:>2} bit: {} samples, {} bytes, bit-exact (max abs err 0, SNR inf)",
        c.name,
        c.channels,
        c.rate,
        c.bps,
        ours.len(),
        data.len()
    );
}

#[test]
fn ffmpeg_bit_exact() {
    if !ffmpeg_available() {
        return;
    }
    let cases = [
        Case { name: "s16_l0", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-compression_level", "0"] },
        Case { name: "s16_l2", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-compression_level", "2"] },
        Case { name: "s16_l5", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-compression_level", "5"] },
        Case { name: "s16_l8", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-compression_level", "8"] },
        Case { name: "s16_l12", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-compression_level", "12"] },
        Case {
            name: "s16_order32",
            channels: 2,
            rate: 44100,
            bps: 16,
            secs: 1.5,
            args: &["-compression_level", "12", "-max_prediction_order", "32", "-lpc_type", "cholesky"],
        },
        Case { name: "s16_fixed", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-lpc_type", "fixed"] },
        Case { name: "s16_verbatim", channels: 2, rate: 44100, bps: 16, secs: 1.0, args: &["-lpc_type", "none"] },
        Case { name: "s16_indep", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-ch_mode", "indep"] },
        Case { name: "s16_left_side", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-ch_mode", "left_side"] },
        Case { name: "s16_right_side", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-ch_mode", "right_side"] },
        Case { name: "s16_mid_side", channels: 2, rate: 44100, bps: 16, secs: 2.0, args: &["-ch_mode", "mid_side"] },
        Case {
            name: "s16_exact_rice",
            channels: 2,
            rate: 44100,
            bps: 16,
            secs: 2.0,
            args: &["-exact_rice_parameters", "1", "-multi_dim_quant", "1", "-max_partition_order", "8"],
        },
        Case { name: "s16_frame192", channels: 2, rate: 44100, bps: 16, secs: 1.0, args: &["-frame_size", "192"] },
        Case { name: "s16_frame1000", channels: 2, rate: 44100, bps: 16, secs: 1.0, args: &["-frame_size", "1000"] },
        Case { name: "s16_frame32768", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-frame_size", "32768"] },
        Case { name: "s16_frame65535", channels: 2, rate: 44100, bps: 16, secs: 3.0, args: &["-frame_size", "65535"] },
        Case { name: "s16_mono_8k", channels: 1, rate: 8000, bps: 16, secs: 3.0, args: &[] },
        Case { name: "s16_mono_11025", channels: 1, rate: 11025, bps: 16, secs: 2.0, args: &[] },
        Case { name: "s16_3ch_32k", channels: 3, rate: 32000, bps: 16, secs: 2.0, args: &[] },
        Case { name: "s16_6ch_48k", channels: 6, rate: 48000, bps: 16, secs: 2.0, args: &["-compression_level", "8"] },
        Case { name: "s16_8ch_48k", channels: 8, rate: 48000, bps: 16, secs: 1.0, args: &[] },
        Case { name: "s24_l5_96k", channels: 2, rate: 96000, bps: 24, secs: 2.0, args: &[] },
        Case { name: "s24_l8_96k", channels: 2, rate: 96000, bps: 24, secs: 2.0, args: &["-compression_level", "8"] },
        Case { name: "s24_l12_44k", channels: 2, rate: 44100, bps: 24, secs: 1.5, args: &["-compression_level", "12"] },
        Case { name: "s24_mid_side", channels: 2, rate: 48000, bps: 24, secs: 1.5, args: &["-ch_mode", "mid_side"] },
        Case { name: "s24_mono_8k", channels: 1, rate: 8000, bps: 24, secs: 2.0, args: &["-frame_size", "256"] },
        Case { name: "s24_6ch_48k", channels: 6, rate: 48000, bps: 24, secs: 1.5, args: &[] },
        Case { name: "s24_192k", channels: 2, rate: 192000, bps: 24, secs: 1.0, args: &[] },
        Case { name: "s32_l5", channels: 2, rate: 44100, bps: 32, secs: 1.5, args: &[] },
        Case { name: "s32_l8", channels: 2, rate: 48000, bps: 32, secs: 1.0, args: &["-compression_level", "8"] },
        Case { name: "s32_left_side", channels: 2, rate: 44100, bps: 32, secs: 1.0, args: &["-ch_mode", "left_side"] },
        Case { name: "s32_right_side", channels: 2, rate: 44100, bps: 32, secs: 1.0, args: &["-ch_mode", "right_side"] },
        Case { name: "s32_mid_side", channels: 2, rate: 44100, bps: 32, secs: 1.0, args: &["-ch_mode", "mid_side"] },
        Case { name: "s32_mono_fixed", channels: 1, rate: 44100, bps: 32, secs: 1.0, args: &["-lpc_type", "fixed"] },
    ];
    for c in &cases {
        run_case(c);
    }
}

/// Builds a variable block size stream from segments encoded with different
/// frame sizes, rewriting the frame headers (blocking bit, sample numbers,
/// CRCs).
fn variable_block_stream(pcm: &[i32], channels: usize, rate: u32) -> Vec<u8> {
    let sizes = [576usize, 4096, 192, 1152, 2048, 300];
    let seg_len = 4096 * 6; // multiple of all sizes except 300 (handled by the last segment)
    let total = pcm.len() / channels;
    let mut frames_out = Vec::new();
    let mut sample = 0u64;
    let mut si = Vec::new();
    let (mut min_bs, mut max_bs) = (u16::MAX, 0u16);
    let mut start = 0usize;
    let mut k = 0;
    while start < total {
        let bs = sizes[k % sizes.len()];
        let len = if k % sizes.len() == sizes.len() - 1 { bs * 50 } else { seg_len };
        let end = (start + len).min(total);
        let seg = &pcm[start * channels..end * channels];
        let bs_s = bs.to_string();
        let data = encode(&format!("var_{k}"), seg, channels, rate, 16, &["-frame_size", &bs_s]);
        if si.is_empty() {
            si = streaminfo(&data);
        }
        let (_, frames) = split_frames(&data);
        for (s, e) in frames {
            let f = &data[s..e];
            let hl = header_len(f).unwrap();
            let coded = f[4].leading_ones() as usize;
            let coded_len = if coded == 0 { 1 } else { coded };
            let tail = &f[4 + coded_len..hl - 1]; // uncommon block size / sample rate
            let fbs = match f[2] >> 4 {
                1 => 192,
                n @ 2..=5 => 144 << n,
                6 => tail[0] as usize + 1,
                7 => u16::from_be_bytes([tail[0], tail[1]]) as usize + 1,
                n => 1 << n,
            };
            if end < total || e < data.len() {
                min_bs = min_bs.min(fbs as u16);
            }
            max_bs = max_bs.max(fbs as u16);
            let mut h = vec![0xff, 0xf9, f[2], f[3]];
            h.extend(encode_coded_number(sample));
            h.extend_from_slice(tail);
            h.push(crc8(&h));
            let mut nf = h;
            nf.extend_from_slice(&f[hl..f.len() - 2]);
            let crc = crc16(&nf);
            nf.extend(crc.to_be_bytes());
            frames_out.extend(nf);
            sample += fbs as u64;
        }
        start = end;
        k += 1;
    }
    assert_eq!(sample as usize, total);
    si[0..2].copy_from_slice(&min_bs.to_be_bytes());
    si[2..4].copy_from_slice(&max_bs.to_be_bytes());
    si[4..10].fill(0); // frame sizes unknown
    si[18..34].fill(0); // MD5 unknown
    // total samples (36 bits) at bit 108 onwards: bytes 13..18, low 4 bits of byte 13.
    let packed = u64::from_be_bytes(si[10..18].try_into().unwrap());
    let packed = (packed & !((1u64 << 36) - 1)) | total as u64;
    si[10..18].copy_from_slice(&packed.to_be_bytes());
    rebuild(&si, None, &frames_out)
}

#[test]
fn variable_block_size() {
    if !ffmpeg_available() {
        return;
    }
    let (ch, rate) = (2, 44100);
    let frames = 44100 * 4;
    let pcm = gen_signal(ch, rate, 16, frames, 77);
    let data = variable_block_stream(&pcm, ch, rate);
    let (ours, corrupt) = decode_i32(&data);
    assert_eq!(corrupt, 0);
    assert_eq!(ours.len(), pcm.len());
    assert!(ours == pcm, "variable block size stream differs");
    let reference = ffmpeg_decode_s32("variable", &data);
    assert!(ours.iter().zip(&reference).all(|(a, b)| a << 16 == *b) && reference.len() == ours.len());
    check_seeks("variable", &data, &pcm, ch);
    eprintln!("variable block size stream: {} samples bit-exact, seeks exact", ours.len());
}

fn check_seeks(name: &str, data: &[u8], pcm: &[i32], ch: usize) {
    let total = (pcm.len() / ch) as u64;
    let mut rng = Rng(0xdead_beef);
    let mut targets = vec![0, 1, 191, 192, 4095, 4096, 4097, 4607, 4608, 12345, total / 3, total / 2 + 123];
    targets.extend([total - 1, total - 4096, total - 1000, total, total + 5]);
    for _ in 0..12 {
        targets.push(rng.next() % total);
    }
    let mut dec = FlacDecoder::new(Cursor::new(data)).unwrap();
    for &t in &targets {
        let pos = dec.seek(t).unwrap();
        assert_eq!(pos, t.min(total), "{name}: seek {t}");
        let mut out = Vec::new();
        while out.len() < 5000 * ch && dec.next_chunk_i32(&mut out).unwrap() {}
        let start = pos as usize * ch;
        let n = out.len().min(pcm.len() - start);
        assert_eq!(n, out.len(), "{name}: seek {t} returned too much");
        assert!(out[..n] == pcm[start..start + n], "{name}: seek {t} mismatch");
        if pos < total {
            assert!(n > 0, "{name}: no data after seek {t}");
        }
    }
    // Interleaved seeks backwards and forwards with f32 output.
    let mut f = Vec::new();
    dec.seek(total / 4).unwrap();
    dec.next_chunk(&mut f).unwrap();
    assert!(!f.is_empty());
}

#[test]
fn seeking_exact() {
    if !ffmpeg_available() {
        return;
    }
    // Without seek table (bisection only).
    let pcm = gen_signal(2, 44100, 16, 44100 * 10, 5);
    let data = encode("seek16", &pcm, 2, 44100, 16, &[]);
    assert!(!FlacDecoder::new(Cursor::new(&data))
        .unwrap()
        .metadata_blocks()
        .iter()
        .any(|b| b.block_type == MetadataBlock::SEEKTABLE));
    check_seeks("bisection", &data, &pcm, 2);

    // With a seek table (point every 10 frames) incl. a placeholder.
    let st = with_seek_table(&data, 10, 0);
    assert!(FlacDecoder::new(Cursor::new(&st))
        .unwrap()
        .metadata_blocks()
        .iter()
        .any(|b| b.block_type == MetadataBlock::SEEKTABLE));
    check_seeks("seektable", &st, &pcm, 2);
    // A seek table with wrong offsets must not break seeking.
    check_seeks("bad seektable", &with_seek_table(&data, 7, 3), &pcm, 2);

    // Small frames, 24-bit, 6 channels.
    let pcm6 = gen_signal(6, 48000, 24, 48000 * 4, 6);
    let data6 = encode("seek24", &pcm6, 6, 48000, 24, &["-frame_size", "1000"]);
    check_seeks("6ch24", &data6, &pcm6, 6);

    // Mono with large frames.
    let pcm1 = gen_signal(1, 8000, 16, 8000 * 20, 7);
    let data1 = encode("seekmono", &pcm1, 1, 8000, 16, &["-frame_size", "16384"]);
    check_seeks("mono_large_frames", &data1, &pcm1, 1);

    // Seek cost (seek only, best of 3 rounds of 200 seeks).
    for (name, file) in [("bisection", &data), ("seek table", &st)] {
        let mut dec = FlacDecoder::new(Cursor::new(file)).unwrap();
        let mut best = f64::MAX;
        for _ in 0..3 {
            let t0 = Instant::now();
            for i in 0..200u64 {
                dec.seek((i * 7919 * 13) % (44100 * 10)).unwrap();
            }
            best = best.min(t0.elapsed().as_secs_f64());
        }
        eprintln!("seek ({name}, 10 s 16-bit stereo file in memory): {:.0} us per seek", best / 200.0 * 1e6);
    }
}

#[test]
fn id3_prefixed_file() {
    if !ffmpeg_available() {
        return;
    }
    let pcm = gen_signal(2, 44100, 16, 20000, 9);
    let data = encode("id3", &pcm, 2, 44100, 16, &[]);
    let mut tagged = b"ID3\x03\x00\x00\x00\x00\x02\x00".to_vec(); // 256 byte body
    tagged.extend(vec![0u8; 256]);
    tagged.extend_from_slice(&data);
    assert!(oc_flac::probe(&tagged));
    assert!(decode_i32(&tagged).0 == pcm);
}

#[test]
fn corruption_recovery() {
    if !ffmpeg_available() {
        return;
    }
    let ch = 2;
    let pcm = gen_signal(ch, 44100, 16, 44100 * 6, 11);
    let data = encode("corrupt", &pcm, ch, 44100, 16, &["-frame_size", "4096"]);
    let (meta_end, frames) = split_frames(&data);
    let total = pcm.len() / ch;

    // Flip a byte inside a few frames and wipe a whole region spanning frames.
    let mut bad = data.clone();
    let mut damaged = Vec::new();
    for &fi in &[3usize, 10, 11, 25] {
        let (s, e) = frames[fi];
        bad[s + (e - s) / 2] ^= 0x5a;
        damaged.push(fi);
    }
    let (s, _) = frames[40];
    let (_, e) = frames[42];
    for b in &mut bad[s + 100..e - 50] {
        *b = 0;
    }
    damaged.extend([40, 41, 42]);
    // Corrupt a frame header (CRC-8 fails, frame is skipped).
    bad[frames[50].0 + 2] ^= 0x01;
    damaged.push(50);

    let mut dec = FlacDecoder::new(Cursor::new(&bad)).unwrap();
    let mut out = Vec::new();
    while dec.next_chunk_i32(&mut out).unwrap() {}
    assert!(dec.corrupt_frames() >= 5, "corrupt events: {}", dec.corrupt_frames());
    assert_eq!(out.len(), pcm.len(), "timeline not preserved");
    let mut exact_frames = 0;
    for (fi, _) in frames.iter().enumerate() {
        let s = fi * 4096 * ch;
        let e = ((fi + 1) * 4096).min(total) * ch;
        if damaged.contains(&fi) {
            assert!(out[s..e].iter().all(|&v| v == 0), "damaged frame {fi} not silent");
        } else {
            assert!(out[s..e] == pcm[s..e], "undamaged frame {fi} differs");
            exact_frames += 1;
        }
    }
    eprintln!(
        "corruption: {} corrupt events, {} of {} frames bit-exact, damaged frames silent, length preserved",
        dec.corrupt_frames(),
        exact_frames,
        frames.len()
    );
    assert!(meta_end > 0);

    // Corrupted last frame: silence up to total_frames.
    let mut bad_tail = data.clone();
    let (s, e) = *frames.last().unwrap();
    bad_tail[(s + e) / 2] ^= 0xff;
    let (out, corrupt) = decode_i32(&bad_tail);
    assert!(corrupt >= 1);
    assert_eq!(out.len(), pcm.len());

    // Truncation at many points: no panic, never more output than expected.
    for cut in (0..data.len()).step_by(data.len() / 97 + 1) {
        let d = &data[..cut];
        if let Ok(mut dec) = FlacDecoder::new(Cursor::new(d)) {
            let mut out = Vec::new();
            while let Ok(true) = dec.next_chunk_i32(&mut out) {}
            assert!(out.len() <= pcm.len());
            // Everything but the (silenced) truncated last frame is exact.
            let good = out.iter().zip(&pcm).take_while(|(a, b)| a == b).count();
            assert!(out.len() - good <= 2 * 4096 * ch, "cut {cut}: {} of {} exact", good, out.len());
        }
    }

    // Random mutation loop on a short file.
    let short = encode("mutate", &pcm[..8000 * ch], ch, 44100, 16, &["-frame_size", "1024"]);
    let mut rng = Rng(42);
    for iter in 0..300 {
        let mut d = short.clone();
        let n = 1 + rng.next() % 8;
        for _ in 0..n {
            let i = (rng.next() % d.len() as u64) as usize;
            d[i] = rng.next() as u8;
        }
        if iter % 5 == 0 {
            let cut = (rng.next() % d.len() as u64) as usize;
            d.truncate(cut);
        }
        if let Ok(mut dec) = FlacDecoder::new(Cursor::new(&d)) {
            let mut out = Vec::new();
            let mut calls = 0;
            while let Ok(true) = dec.next_chunk(&mut out) {
                calls += 1;
                assert!(calls < 100_000);
            }
            assert!(out.iter().all(|v| v.is_finite()));
            assert!(out.len() <= 8000 * ch + (1 << 20) * ch);
            let _ = dec.seek(rng.next() % 10_000);
            let _ = dec.next_chunk(&mut out);
        }
    }
}

#[test]
fn frame_decoder_api() {
    if !ffmpeg_available() {
        return;
    }
    let pcm = gen_signal(2, 48000, 24, 48000, 13);
    let data = encode("packets", &pcm, 2, 48000, 24, &["-frame_size", "1152"]);
    let (meta_end, frames) = split_frames(&data);
    // Matroska style CodecPrivate: "fLaC" + metadata blocks.
    let mut dec = oc_flac::FlacFrameDecoder::new(&data[..meta_end]).unwrap();
    assert_eq!(dec.bits_per_sample(), 24);
    let mut out = Vec::new();
    let mut n = 0;
    for &(s, e) in &frames {
        n += dec.decode_i32(&data[s..e], &mut out).unwrap();
    }
    assert_eq!(n, 48000);
    assert!(out == pcm);
    let mut f = Vec::new();
    dec.decode(&data[frames[0].0..frames[0].1], &mut f).unwrap();
    assert_eq!(f.len(), 1152 * 2);
    assert_eq!(f[0], pcm[0] as f32 / 8_388_608.0);
}

fn speed(name: &str, data: &[u8], secs: f64) -> f64 {
    let mut best = f64::MAX;
    let mut out = Vec::with_capacity(1 << 16);
    for _ in 0..3 {
        let t0 = Instant::now();
        let mut dec = FlacDecoder::new(Cursor::new(data)).unwrap();
        let mut n = 0usize;
        loop {
            out.clear();
            if !dec.next_chunk(&mut out).unwrap() {
                break;
            }
            n += out.len();
        }
        std::hint::black_box(n);
        best = best.min(t0.elapsed().as_secs_f64());
    }
    let x = secs / best;
    eprintln!("speed {name}: {secs:.0} s of audio decoded in {:.1} ms = {x:.0}x real time", best * 1e3);
    x
}

#[test]
fn decode_speed_report() {
    if !ffmpeg_available() {
        return;
    }
    let secs = 30.0;
    let pcm = gen_signal(2, 44100, 16, (44100.0 * secs) as usize, 21);
    let l5 = encode("speed_l5", &pcm, 2, 44100, 16, &[]);
    let x = speed("16-bit stereo 44.1 kHz, level 5", &l5, secs);
    let l8 = encode("speed_l8", &pcm, 2, 44100, 16, &["-compression_level", "8"]);
    speed("16-bit stereo 44.1 kHz, level 8", &l8, secs);
    let pcm24 = gen_signal(2, 96000, 24, (96000.0 * 10.0) as usize, 22);
    let d24 = encode("speed_24", &pcm24, 2, 96000, 24, &["-compression_level", "8"]);
    speed("24-bit stereo 96 kHz, level 8", &d24, 10.0);
    if !cfg!(debug_assertions) {
        assert!(x > 200.0, "decoder slower than 200x real time: {x:.0}x");
    }
}
