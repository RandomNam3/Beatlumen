//! Corruption robustness: truncated and randomly mutated versions of every
//! fixture must never panic in `read_info`, `open`, decoding or seeking.

mod common;

use common::*;
use std::io::Cursor;
use std::panic::{catch_unwind, AssertUnwindSafe};

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Exercises the whole API on `data`; returns an error description if
/// something panicked.
fn exercise(data: Vec<u8>, ext: &str) -> Result<(), String> {
    let r = catch_unwind(AssertUnwindSafe(|| {
        let mut c = Cursor::new(data.clone());
        let _ = bl_media::read_info(&mut c, Some(ext), true);
        if let Ok(mut d) = bl_media::open(Box::new(Cursor::new(data)), Some(ext)) {
            let mut out = Vec::new();
            let mut chunks = 0;
            loop {
                match d.next_chunk(&mut out) {
                    Ok(true) => {}
                    Ok(false) => break,
                    // Errors are acceptable; decoders may continue afterwards.
                    Err(_) => {
                        if chunks > 20 {
                            break;
                        }
                    }
                }
                chunks += 1;
                if chunks > 3000 || out.len() > 50_000_000 {
                    break;
                }
                out.clear();
            }
            let total = d.total_frames().unwrap_or(100_000);
            for t in [total / 3, 0, total.saturating_sub(100), total + 1000] {
                if d.seek(t).is_ok() {
                    let _ = d.next_chunk(&mut out);
                }
            }
        }
    }));
    r.map_err(|e| {
        e.downcast_ref::<String>().cloned().or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string())).unwrap_or_default()
    })
}

#[test]
fn corrupted_files_never_panic() {
    if !have_ffmpeg() {
        eprintln!("ffmpeg not found: skipping");
        return;
    }
    // Keep panics quiet; they are collected and reported below.
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut failures = Vec::new();
    let mut runs = 0;
    for fx in fixtures() {
        let Some(p) = make(fx.file, fx.master, fx.args) else { continue };
        let original = std::fs::read(&p).unwrap();
        let ext = ext_of(&p);
        let mut rng = Rng(0x9e3779b97f4a7c15 ^ original.len() as u64);
        let mut cases: Vec<(String, Vec<u8>)> = Vec::new();
        for frac in [0.0, 0.001, 0.01, 0.05, 0.3, 0.7, 0.99] {
            let n = (original.len() as f64 * frac) as usize;
            cases.push((format!("truncate {n}"), original[..n].to_vec()));
        }
        for i in 0..14 {
            let mut d = original.clone();
            // Concentrate some mutations on the headers.
            let region = if i % 2 == 0 { d.len().min(4096) } else { d.len() };
            let count = 1 + rng.below(if i < 7 { 4 } else { 64 });
            for _ in 0..count {
                let at = rng.below(region);
                d[at] = rng.next() as u8;
            }
            cases.push((format!("mutate #{i} ({count} bytes)"), d));
        }
        // Random garbage after a valid prefix.
        let mut g = original[..original.len() / 2].to_vec();
        g.extend((0..20_000).map(|_| rng.next() as u8));
        cases.push(("garbage tail".into(), g));
        for (what, data) in cases {
            runs += 1;
            if let Err(msg) = exercise(data, &ext) {
                failures.push(format!("{} [{what}]: panic: {msg}", fx.file));
            }
        }
    }
    // Pure garbage with each extension hint.
    let mut rng = Rng(42);
    for ext in bl_media::SUPPORTED_EXTENSIONS {
        for len in [0usize, 1, 7, 100, 5000] {
            let d: Vec<u8> = (0..len).map(|_| rng.next() as u8).collect();
            runs += 1;
            if let Err(msg) = exercise(d, ext) {
                failures.push(format!("garbage .{ext} len {len}: panic: {msg}"));
            }
        }
    }
    std::panic::set_hook(hook);
    println!("{runs} corrupted inputs exercised, {} panics", failures.len());
    assert!(failures.is_empty(), "panics:\n{}", failures.join("\n"));
}
