//! Deterministic fuzzing for the robustness tests of the parsers: damaged
//! copies of sample inputs (bytes changed, cut short, spliced together,
//! ranges repeated or dropped, tokens of the format put in) go through a
//! parser under `catch_unwind`. Panics and very slow inputs are collected
//! and reported at the end.
//!
//! Shared by several crates (`#[path = "../../bl-json/tests/fuzz/mod.rs"]`).
//! `BL_FUZZ_ITERS` sets the number of variants per suite for long runs,
//! `BL_FUZZ_SEED` another seed.
#![allow(dead_code)]

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::time::{Duration, Instant};

/// Deterministic xorshift PRNG.
pub struct Rng(pub u64);

impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
}

/// Variants per suite: `BL_FUZZ_ITERS`, else `default`.
pub fn iters(default: usize) -> usize {
    std::env::var("BL_FUZZ_ITERS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn seed(name: &str) -> u64 {
    let base = std::env::var("BL_FUZZ_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x9e37_79b9_7f4a_7c15u64);
    // A different stream for each suite.
    name.bytes()
        .fold(base, |h, b| (h ^ b as u64).wrapping_mul(0x100_0000_01b3))
        | 1
}

/// Values that often sit at the edge of a check.
const EDGES: &[u64] = &[
    0,
    1,
    0x7f,
    0x80,
    0xff,
    0x100,
    0x7fff,
    0x8000,
    0xffff,
    0x7fff_ffff,
    0x8000_0000,
    0xffff_ffff,
    0x7fff_ffff_ffff_ffff,
    u64::MAX,
];

/// A damaged copy of one of `samples`; `tokens` are pieces of the format
/// (brackets, keywords, tags) that are put in now and then.
pub fn mutate(rng: &mut Rng, samples: &[Vec<u8>], tokens: &[&[u8]]) -> Vec<u8> {
    let mut d = samples[rng.below(samples.len())].clone();
    let rounds = 1 + rng.below(4);
    for _ in 0..rounds {
        let len = d.len();
        match rng.below(10) {
            // Random bytes.
            0 | 1 => {
                for _ in 0..1 + rng.below(8) {
                    if !d.is_empty() {
                        let i = rng.below(d.len());
                        d[i] = rng.next() as u8;
                    }
                }
            }
            // Bit flips, often near the start (headers).
            2 => {
                if !d.is_empty() {
                    let region = if rng.below(2) == 0 { len.min(64) } else { len };
                    let i = rng.below(region);
                    d[i] ^= 1 << rng.below(8);
                }
            }
            // Cut short.
            3 => d.truncate(rng.below(len + 1)),
            // Splice: a piece of another sample at a random place.
            4 => {
                let other = &samples[rng.below(samples.len())];
                if !other.is_empty() {
                    let a = rng.below(other.len());
                    let b = (a + 1 + rng.below(other.len() - a)).min(other.len());
                    let at = rng.below(len + 1);
                    d.splice(at..at, other[a..b].iter().copied());
                }
            }
            // A range repeated (deep nesting, many keys, many frames).
            5 => {
                if len > 0 {
                    let a = rng.below(len);
                    let b = (a + 1 + rng.below((len - a).min(256))).min(len);
                    let piece = d[a..b].to_vec();
                    let most = if rng.below(8) == 0 { 2000 } else { 16 };
                    let times = 1 + rng.below(most);
                    let mut rep = Vec::with_capacity(piece.len() * times);
                    for _ in 0..times {
                        rep.extend_from_slice(&piece);
                    }
                    d.splice(b..b, rep);
                }
            }
            // A range dropped.
            6 => {
                if len > 0 {
                    let a = rng.below(len);
                    let b = (a + 1 + rng.below(len - a)).min(len);
                    d.drain(a..b);
                }
            }
            // An edge value written over 1, 2, 4 or 8 bytes.
            7 => {
                let width = 1 << rng.below(4);
                if len >= width {
                    let v = EDGES[rng.below(EDGES.len())];
                    let bytes = if rng.below(2) == 0 {
                        v.to_le_bytes()
                    } else {
                        v.to_be_bytes()
                    };
                    let i = rng.below(len - width + 1);
                    d[i..i + width].copy_from_slice(&bytes[8 - width..]);
                }
            }
            // A token of the format, maybe many times.
            _ => {
                if !tokens.is_empty() {
                    let t = tokens[rng.below(tokens.len())];
                    let times = if rng.below(6) == 0 {
                        1 + rng.below(500)
                    } else {
                        1
                    };
                    let at = rng.below(len + 1);
                    let mut ins = Vec::with_capacity(t.len() * times);
                    for _ in 0..times {
                        ins.extend_from_slice(t);
                    }
                    d.splice(at..at, ins);
                }
            }
        }
    }
    d
}

/// One damaged input that panicked or took too long.
fn describe(e: Box<dyn std::any::Any + Send>) -> String {
    e.downcast_ref::<String>()
        .cloned()
        .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_default()
}

/// Longest time one input may take (unoptimised build) before it counts
/// as a finding: parsers are linear, so a sample-sized input takes
/// milliseconds.
const SLOW: Duration = Duration::from_secs(5);

/// Runs `f` on each sample and on `default_iters` (or `BL_FUZZ_ITERS`)
/// damaged variants of them. Fails with the list of panics and slow inputs;
/// the first failing input of each kind is written next to the test binary
/// for a closer look.
pub fn run(
    name: &str,
    samples: &[Vec<u8>],
    tokens: &[&[u8]],
    default_iters: usize,
    f: impl Fn(&[u8]),
) {
    let hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(|_| {}));
    let mut rng = Rng(seed(name));
    let n = iters(default_iters);
    let mut findings: Vec<String> = Vec::new();
    let mut saved = 0;
    let started = Instant::now();
    for i in 0..samples.len() + n {
        let input = match samples.get(i) {
            Some(s) => s.clone(),
            None => mutate(&mut rng, samples, tokens),
        };
        let t = Instant::now();
        let r = catch_unwind(AssertUnwindSafe(|| f(&input)));
        let took = t.elapsed();
        let problem = match r {
            Err(e) => Some(format!("panic: {}", describe(e))),
            Ok(()) if took > SLOW => Some(format!("took {took:?}")),
            Ok(()) => None,
        };
        if let Some(p) = problem {
            if saved < 3 {
                let path = std::env::temp_dir().join(format!("bl-fuzz-{name}-{i}.bin"));
                if input.len() < 16 << 20 && std::fs::write(&path, &input).is_ok() {
                    saved += 1;
                }
            }
            findings.push(format!("{name} #{i} ({} bytes): {p}", input.len()));
        }
    }
    std::panic::set_hook(hook);
    println!(
        "{name}: {} inputs in {:?}, {} findings",
        samples.len() + n,
        started.elapsed(),
        findings.len()
    );
    assert!(
        findings.is_empty(),
        "{} findings:\n{}",
        findings.len(),
        findings
            .iter()
            .take(40)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}
