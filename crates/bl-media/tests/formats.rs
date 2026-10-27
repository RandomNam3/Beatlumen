//! End-to-end tests against ffmpeg-generated fixtures: `read_info`
//! (durations vs ffprobe, codec/container names), full decodes vs
//! `ffmpeg -f f32le`, and sample-accurate seeking. Run with `--nocapture`
//! to see the result table. Skipped (passing) when ffmpeg is missing.

mod common;

use common::*;
use std::path::PathBuf;

fn path_of(fx: &Fixture) -> Option<PathBuf> {
    make(fx.file, fx.master, fx.args)
}

#[test]
fn info_durations_match_ffprobe() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let mut failures = Vec::new();
    for fx in fixtures() {
        let Some(p) = path_of(&fx) else {
            eprintln!("{:<18} fixture generation failed (encoder missing?)", fx.file);
            continue;
        };
        let mut f = std::fs::File::open(&p).unwrap();
        let t0 = std::time::Instant::now();
        let info = match bl_media::read_info(&mut f, Some(&ext_of(&p)), false) {
            Ok(i) => i,
            Err(e) => {
                failures.push(format!("{}: read_info failed: {e}", fx.file));
                continue;
            }
        };
        let us = t0.elapsed().as_micros();
        let probe = ffprobe_duration(&p).unwrap_or(0.0);
        let ours = info.duration_ms.unwrap_or(0) as f64 / 1000.0;
        let diff_ms = (ours - probe).abs() * 1000.0;
        println!(
            "info {:<18} {:<9} {:<7} {:>6} Hz {} ch  dur {:.3}s ffprobe {:.3}s (diff {:5.1} ms)  {:>4?} kbps  {} us",
            fx.file, info.container, info.codec, info.sample_rate, info.channels, ours, probe, diff_ms, info.bitrate_kbps, us
        );
        if info.codec != fx.codec || info.container != fx.container {
            failures.push(format!("{}: codec/container {}/{} expected {}/{}", fx.file, info.codec, info.container, fx.codec, fx.container));
        }
        // Lossy streams: ffprobe and we may disagree on priming handling.
        let tol = if fx.lossless { 2.0 } else { 70.0 };
        if diff_ms > tol {
            failures.push(format!("{}: duration {ours:.3}s vs ffprobe {probe:.3}s", fx.file));
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn decode_matches_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let mut failures = Vec::new();
    let mut ok_formats = Vec::new();
    for fx in fixtures() {
        let Some(p) = path_of(&fx) else { continue };
        let mut reference = ffmpeg_decode(&p).expect("ffmpeg decode");
        let t0 = std::time::Instant::now();
        let (spec, total, ours, codec) = match decode_all(&p) {
            Ok(v) => v,
            Err(e) if is_unsupported(&e) => {
                println!("dec  {:<18} SKIPPED (unsupported: {e})", fx.file);
                continue;
            }
            Err(e) => {
                failures.push(format!("{}: decode error: {e}", fx.file));
                continue;
            }
        };
        let secs = t0.elapsed().as_secs_f64();
        let ch = spec.channels as usize;
        reference.drain(..(fx.ref_skip * ch).min(reference.len()));
        let frames = ours.len() / ch.max(1);
        let ref_frames = if fx.master_len { master_frames(&fx) } else { reference.len() / ch.max(1) };
        let audio_secs = frames as f64 / spec.sample_rate as f64;
        let (max_err, snr) = compare(&ours, &reference);
        let offset = if fx.lossless || snr > 30.0 { 0 } else { best_offset(&ours, &reference, ch, 3000) };
        // Gapless alignment against the original signal (same rate only).
        let (m_spec, _, m_samples, _) = decode_all(&master(fx.master)).unwrap();
        let master_offset = if m_spec.sample_rate == spec.sample_rate && m_spec.channels == spec.channels {
            Some(best_offset(&ours, &m_samples, ch, 2500))
        } else {
            None
        };
        println!(
            "dec  {:<18} {:<6} {} ch {:>6} Hz frames {:>7} (ffmpeg {:>7}, total_frames {:?})  max_err {:.2e}  SNR {:6.1} dB  {} vs-master offset {:?}  speed {:.0}x",
            fx.file,
            codec,
            ch,
            spec.sample_rate,
            frames,
            ref_frames,
            total,
            max_err,
            snr,
            if offset != 0 { format!("best offset {offset}") } else { String::new() },
            master_offset,
            audio_secs / secs.max(1e-9)
        );
        if fx.align.is_some() && master_offset != fx.align {
            failures.push(format!("{}: offset against the source signal {master_offset:?}, expected {:?}", fx.file, fx.align));
        }
        if codec != fx.codec {
            failures.push(format!("{}: codec name {codec}", fx.file));
        }
        if fx.lossless {
            if frames != ref_frames || max_err > 1e-6 {
                failures.push(format!("{}: lossless mismatch (frames {frames} vs {ref_frames}, max err {max_err})", fx.file));
            }
        } else if frames.abs_diff(ref_frames) > 0 || snr < 40.0 {
            failures.push(format!("{}: lossy mismatch (frames {frames} vs {ref_frames}, SNR {snr:.1} dB, offset {offset})", fx.file));
        }
        if failures.last().map_or(true, |f| !f.starts_with(fx.file)) {
            ok_formats.push(fx.file);
        }
    }
    println!("decode OK: {}", ok_formats.join(", "));
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}

#[test]
fn seek_is_sample_accurate() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    let mut failures = Vec::new();
    for fx in fixtures() {
        let Some(p) = path_of(&fx) else { continue };
        let (spec, _, full, _) = match decode_all(&p) {
            Ok(v) => v,
            Err(_) => continue, // reported by decode_matches_ffmpeg
        };
        let ch = spec.channels as usize;
        let frames = (full.len() / ch) as u64;
        let mut worst = (0f32, f64::INFINITY);
        for &target in &[frames / 2 + 1234, 777, spec.sample_rate as u64 + 1, frames.saturating_sub(5000), 0] {
            let len = 4096usize.min((frames - target.min(frames)) as usize);
            let (at, got) = match decode_after_seek(&p, target, len) {
                Ok(v) => v,
                Err(e) => {
                    failures.push(format!("{}: seek to {target} failed: {e}", fx.file));
                    continue;
                }
            };
            let want = &full[target as usize * ch..(target as usize + len) * ch];
            if at != target || got.len() != want.len() {
                failures.push(format!("{}: seek to {target}: positioned at {at}, got {} of {} samples", fx.file, got.len(), want.len()));
                continue;
            }
            let (m, s) = compare(&got, want);
            worst.0 = worst.0.max(m);
            worst.1 = worst.1.min(s);
        }
        println!("seek {:<18} worst max_err {:.2e}  worst SNR {:6.1} dB", fx.file, worst.0, worst.1);
        // Exact formats must be bit identical; Opus decoder state only
        // converges during the pre-roll.
        // ffmpeg's AAC encoder uses perceptual noise substitution by
        // default, whose random noise cannot repeat after a seek (the
        // `*_nopns` fixtures verify bit-exact AAC seeking).
        let need = match fx.file {
            "aac.m4a" | "adts.aac" | "aac.mka" => 45.0,
            _ if fx.codec == "opus" => 35.0,
            _ => 90.0,
        };
        if worst.1 < need {
            failures.push(format!("{}: seek output differs (SNR {:.1} dB)", fx.file, worst.1));
        }
    }
    assert!(failures.is_empty(), "failures:\n{}", failures.join("\n"));
}
