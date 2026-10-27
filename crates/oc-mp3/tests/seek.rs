//! Seeking: after `seek(t)` the output must equal the continuous decode
//! from sample `t` on (sample accurate, identical bits).

mod common;

use common::*;
use oc_mp3::{Mp3Decoder, SeekMode};
use std::io::Cursor;

fn stereo(noise_amp: &str, out: &[&str]) -> Vec<String> {
    let mut v: Vec<String> = ["-f", "lavfi", "-i", "sine=f=440:d=4", "-f", "lavfi", "-i"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    v.push(format!("anoisesrc=d=4:a={noise_amp}"));
    v.extend(
        [
            "-filter_complex",
            "[0:a][1:a]amerge=inputs=2[a]",
            "-map",
            "[a]",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    v.extend(out.iter().map(|s| s.to_string()));
    v
}

fn read_n(d: &mut Mp3Decoder<Cursor<&[u8]>>, n: usize) -> Vec<f32> {
    let mut out = Vec::new();
    while out.len() < n {
        if !d.next_chunk(&mut out).unwrap() {
            break;
        }
    }
    out.truncate(n);
    out
}

fn check_seeks(name: &str, args: &[String], mode: SeekMode) {
    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let Some(path) = gen(name, &args) else { return };
    let data = std::fs::read(&path).unwrap();
    let full = decode_bytes(&data);
    let ch = full.channels as usize;
    let total = (full.pcm.len() / ch) as u64;
    assert_eq!(full.total_frames, Some(total), "{name}: total_frames");

    let mut d = Mp3Decoder::new(Cursor::new(&data[..])).unwrap();
    d.set_seek_mode(mode);
    let targets = [
        total / 2,
        0,
        1,
        575,
        576,
        1105,
        1152 * 10 + 7,
        1152 * 3,
        total - 1000,
        total / 3,
        total - 1,
        total / 2 + 1,
        12345,
        total,
    ];
    for &t in &targets {
        let pos = d.seek(t).unwrap();
        let got = read_n(&mut d, 3000 * ch);
        if mode == SeekMode::Exact {
            assert_eq!(pos, t, "{name}: seek position");
        }
        let start = (pos as usize * ch).min(full.pcm.len());
        let want = &full.pcm[start..(start + 3000 * ch).min(full.pcm.len())];
        assert_eq!(got.len(), want.len(), "{name}: length after seek to {t}");
        let mismatch = got.iter().zip(want).position(|(a, b)| a != b);
        assert!(
            mismatch.is_none(),
            "{name}: seek to {t}: first mismatch at {:?}",
            mismatch
        );
    }
    println!("{name}: {} seeks ok ({mode:?})", targets.len());
}

#[test]
fn seek_cbr() {
    check_seeks(
        "seek_cbr.mp3",
        &stereo("0.3", &["-c:a", "libmp3lame", "-b:a", "128k"]),
        SeekMode::Exact,
    );
}

#[test]
fn seek_vbr() {
    check_seeks(
        "seek_vbr.mp3",
        &stereo("0.3", &["-c:a", "libmp3lame", "-q:a", "4"]),
        SeekMode::Exact,
    );
}

#[test]
fn seek_lsf_low_bitrate() {
    // Small frames: the bit reservoir spans many frames.
    check_seeks(
        "seek_lsf.mp3",
        &stereo(
            "0.1",
            &["-ar", "16000", "-c:a", "libmp3lame", "-b:a", "16k"],
        ),
        SeekMode::Exact,
    );
}

#[test]
fn seek_layer2() {
    check_seeks(
        "seek_l2.mp2",
        &stereo("0.3", &["-c:a", "mp2", "-b:a", "160k"]),
        SeekMode::Exact,
    );
}

#[test]
fn seek_cbr_fast() {
    // With an Info (CBR) header the fast byte-rate seek is also exact.
    check_seeks(
        "seek_cbr.mp3",
        &stereo("0.3", &["-c:a", "libmp3lame", "-b:a", "128k"]),
        SeekMode::Fast,
    );
}

#[test]
fn seek_vbr_fast_toc() {
    let args = stereo("0.3", &["-c:a", "libmp3lame", "-q:a", "4"]);
    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let Some(path) = gen("seek_vbr.mp3", &args) else {
        return;
    };
    let data = std::fs::read(&path).unwrap();
    let full = decode_bytes(&data);
    let ch = full.channels as usize;
    let total = (full.pcm.len() / ch) as u64;
    let mut d = Mp3Decoder::new(Cursor::new(&data[..])).unwrap();
    d.set_seek_mode(SeekMode::Fast);
    for t in [total / 2, total / 5, total - 5000, 100] {
        let pos = d.seek(t).unwrap();
        // TOC estimates are approximate: within ~0.1 s.
        assert!(
            (pos as i64 - t as i64).abs() < 5000,
            "fast seek to {t} landed at {pos}"
        );
        let got = read_n(&mut d, 1000 * ch);
        assert_eq!(got.len(), 1000 * ch);
    }
    // Exact seeks still work afterwards.
    d.set_seek_mode(SeekMode::Exact);
    let t = total / 3;
    assert_eq!(d.seek(t).unwrap(), t);
    let got = read_n(&mut d, 2000 * ch);
    assert_eq!(
        &got[..],
        &full.pcm[t as usize * ch..(t as usize + 2000) * ch]
    );
}

#[test]
fn seek_past_end_and_rewind() {
    let args = stereo("0.3", &["-c:a", "libmp3lame", "-b:a", "128k"]);
    let args: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let Some(path) = gen("seek_cbr.mp3", &args) else {
        return;
    };
    let data = std::fs::read(&path).unwrap();
    let full = decode_bytes(&data);
    let ch = full.channels as usize;
    let total = (full.pcm.len() / ch) as u64;
    let mut d = Mp3Decoder::new(Cursor::new(&data[..])).unwrap();
    assert_eq!(d.seek(total + 100_000).unwrap(), total);
    let mut out = Vec::new();
    assert!(!d.next_chunk(&mut out).unwrap());
    assert_eq!(d.seek(0).unwrap(), 0);
    let mut all = Vec::new();
    while d.next_chunk(&mut all).unwrap() {}
    assert_eq!(all, full.pcm);
}
