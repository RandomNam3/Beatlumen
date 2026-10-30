//! Checks against the real model and reference outputs of Open-Unmix
//! (PyTorch). Skipped unless `BL_SEP_MODEL` names the weights file;
//! the comparison also needs `BL_SEP_REF`, a folder with raw little-endian
//! f32 files of a clip: `input.f32` (2 x N samples), `mag.f32` and
//! `spec.f32` (mixture and model output magnitudes, 2 x 2049 x F),
//! `vocals0.f32` and `vocals1.f32` (separated without and with the Wiener
//! step, 2 x N) and `shape.txt` ("N F").

use bl_sep::stft::Stft;
use bl_sep::umx::{Umx, BINS, HIDDEN, MAX_BIN};
use bl_sep::{Options, Separator};

fn model() -> Option<String> {
    std::env::var("BL_SEP_MODEL").ok()
}

fn read(dir: &str, name: &str) -> Vec<f32> {
    std::fs::read(format!("{dir}/{name}"))
        .unwrap()
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

/// Largest absolute difference, relative to the largest reference value,
/// and the signal-to-error ratio in dB.
fn compare(what: &str, ours: &[f32], want: &[f32]) -> (f32, f32) {
    assert_eq!(ours.len(), want.len(), "{what}");
    let err = ours
        .iter()
        .zip(want)
        .map(|(a, b)| (a - b).abs())
        .fold(0f32, f32::max);
    let peak = want.iter().map(|v| v.abs()).fold(0f32, f32::max);
    let signal: f64 = want.iter().map(|&v| v as f64 * v as f64).sum();
    let noise: f64 = ours
        .iter()
        .zip(want)
        .map(|(&a, &b)| (a as f64 - b as f64).powi(2))
        .sum();
    let snr = (10.0 * (signal / noise.max(1e-30)).log10()) as f32;
    println!(
        "{what}: max abs error {err:.3e} (peak {peak:.3e}, relative {:.3e}), SNR {snr:.1} dB",
        err / peak
    );
    (err / peak, snr)
}

#[test]
fn loads_the_model() {
    let Some(path) = model() else { return };
    Umx::load(&path).unwrap();
    assert!(Umx::load("/nonexistent/vocals.pth").is_err());
}

#[test]
fn matches_open_unmix() {
    let (Some(path), Ok(dir)) = (model(), std::env::var("BL_SEP_REF")) else {
        return;
    };
    let shape = std::fs::read_to_string(format!("{dir}/shape.txt")).unwrap();
    let mut it = shape
        .split_whitespace()
        .map(|v| v.parse::<usize>().unwrap());
    let (n, frames) = (it.next().unwrap(), it.next().unwrap());
    let input = read(&dir, "input.f32");
    let (left, right) = input.split_at(n);

    // The mixture's magnitudes, as channel x bin x frame.
    let stft = Stft::new(4096, 1024);
    assert_eq!(stft.frames(n), frames);
    let spec = [left, right].map(|c| stft.forward(c, 0..frames));
    let mut mag = vec![0f32; 2 * BINS * frames];
    for (ch, s) in spec.iter().enumerate() {
        for t in 0..frames {
            for f in 0..BINS {
                mag[(ch * BINS + f) * frames + t] = s[t * BINS + f].norm();
            }
        }
    }
    let (rel, _) = compare("mixture magnitude", &mag, &read(&dir, "mag.f32"));
    assert!(rel < 1e-5);

    // The network on the reference magnitudes.
    let ref_mag = read(&dir, "mag.f32");
    let at = |ch: usize, f: usize, t: usize| ref_mag[(ch * BINS + f) * frames + t];
    let model = Umx::load(&path).unwrap();
    let mut x = Vec::with_capacity(frames * 2 * MAX_BIN);
    for t in 0..frames {
        for ch in 0..2 {
            x.extend((0..MAX_BIN).map(|f| at(ch, f, t)));
        }
    }
    let encoded = model.encode(&x, frames);
    let lstm = model.lstm(&encoded, frames, &mut |_| true).unwrap();
    assert_eq!(lstm.len(), frames * HIDDEN);
    let mask = model.decode(&encoded, &lstm, frames);
    let mut est = vec![0f32; 2 * BINS * frames];
    for t in 0..frames {
        for ch in 0..2 {
            for f in 0..BINS {
                est[(ch * BINS + f) * frames + t] = mask[(t * 2 + ch) * BINS + f] * at(ch, f, t);
            }
        }
    }
    let (rel, _) = compare("model output", &est, &read(&dir, "spec.f32"));
    assert!(rel < 1e-3);

    // The whole separation, without and with the Wiener step.
    let sep = Separator::load(&path).unwrap();
    for (wiener, file) in [(false, "vocals0.f32"), (true, "vocals1.f32")] {
        let t = std::time::Instant::now();
        let opts = Options {
            wiener,
            progress: &mut |_| true,
        };
        let [l, r] = sep.separate_stereo(left, right, opts).unwrap();
        println!("separated in {:.2} s", t.elapsed().as_secs_f32());
        let ours: Vec<f32> = l.into_iter().chain(r).collect();
        let (_, snr) = compare(file, &ours, &read(&dir, file));
        assert!(snr > 40.0);
    }
}
