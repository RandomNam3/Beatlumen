//! Audio analysis for the visualizer.
//!
//! * [`fft`]: real FFT and windows.
//! * [`track`]: offline whole-track analysis (energy, novelty curves,
//!   structural transitions) that drives effect triggers.
//! * [`realtime`]: streaming spectrum/novelty used while the offline analysis
//!   is not available yet and for live spectrum displays.
//! * Binary (de)serialization of [`TrackAnalysis`] for caching and for
//!   transferring server side analyses to clients.

#![forbid(unsafe_code)]

pub mod fft;
pub mod realtime;
pub mod track;

pub use track::{analyze, AnalysisConfig, NoveltyConfig, Spectrum, TrackAnalysis, TRANSITION_MAJOR, TRANSITION_MINOR, TRANSITION_NONE};

/// Bumped whenever analysis output changes; part of cache keys.
pub const ANALYSIS_VERSION: u32 = 1;

const MAGIC: &[u8; 4] = b"OCAN";

/// Serializes an analysis into a compact little-endian binary blob.
pub fn encode(a: &TrackAnalysis) -> Vec<u8> {
    let n = a.energy.len();
    let mut out = Vec::with_capacity(32 + 4 * (n * (1 + a.novelty.len()) + a.transitions.len()));
    out.extend_from_slice(MAGIC);
    for v in [ANALYSIS_VERSION, a.sample_rate, a.hop_size as u32, a.transition_down_sampling as u32, n as u32, a.novelty.len() as u32, a.transitions.len() as u32] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    let mut put = |s: &[f32]| {
        for v in s {
            out.extend_from_slice(&v.to_le_bytes());
        }
    };
    put(&a.energy);
    for c in &a.novelty {
        put(c);
    }
    put(&a.transitions);
    out
}

/// Parses a blob produced by [`encode`].
pub fn decode(data: &[u8]) -> Option<TrackAnalysis> {
    if data.len() < 32 || &data[..4] != MAGIC {
        return None;
    }
    let u = |i: usize| u32::from_le_bytes(data[4 + i * 4..8 + i * 4].try_into().unwrap());
    if u(0) != ANALYSIS_VERSION {
        return None;
    }
    let (sample_rate, hop, down, n, k, m) = (u(1), u(2) as usize, u(3) as usize, u(4) as usize, u(5) as usize, u(6) as usize);
    if hop == 0 || sample_rate == 0 || k > 64 {
        return None;
    }
    let total = n.checked_mul(k + 1)?.checked_add(m)?;
    if data.len() != 32 + total.checked_mul(4)? {
        return None;
    }
    let mut pos = 32;
    let mut take = |len: usize| {
        let v: Vec<f32> = data[pos..pos + len * 4].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
        pos += len * 4;
        v
    };
    let energy = take(n);
    let novelty = (0..k).map(|_| take(n)).collect();
    let transitions = take(m);
    Some(TrackAnalysis { sample_rate, hop_size: hop, energy, novelty, transitions, transition_down_sampling: down.max(1) })
}

/// Downmixes interleaved audio to mono by averaging channels.
pub fn to_mono(interleaved: &[f32], channels: usize, out: &mut Vec<f32>) {
    let channels = channels.max(1);
    out.reserve(interleaved.len() / channels);
    if channels == 1 {
        out.extend_from_slice(interleaved);
        return;
    }
    let inv = 1.0 / channels as f32;
    for frame in interleaved.chunks_exact(channels) {
        out.push(frame.iter().sum::<f32>() * inv);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encode_decode_roundtrip() {
        let samples: Vec<f32> = (0..44100).map(|i| (i as f32 * 0.05).sin() * 0.5).collect();
        let a = analyze(&samples, 44100, &AnalysisConfig::default(), &mut |_| true).unwrap();
        let blob = encode(&a);
        assert_eq!(decode(&blob).unwrap(), a);
        assert!(decode(&blob[..blob.len() - 1]).is_none());
        assert!(decode(b"OCANxxxx").is_none());
    }
}
