//! Downmixing of WAVE-order multichannel audio to stereo.
//!
//! Default layouts by channel count (WAVE / SMPTE order):
//! 3 = L R C, 4 = L R BL BR, 5 = L R C BL BR, 6 = L R C LFE BL BR,
//! 7 = L R C LFE BC SL SR, 8 = L R C LFE BL BR SL SR.
//! Centre and surround channels are mixed at -3 dB (ITU-R BS.775), the LFE
//! channel is dropped, and the result is normalised by the sum of the
//! coefficients feeding each output so it can never clip.

const C3DB: f32 = std::f32::consts::FRAC_1_SQRT_2;

/// Left/right gains for each input channel of a layout.
fn coefficients(channels: usize) -> Vec<(f32, f32)> {
    let c = C3DB;
    match channels {
        3 => vec![(1.0, 0.0), (0.0, 1.0), (c, c)],
        4 => vec![(1.0, 0.0), (0.0, 1.0), (c, 0.0), (0.0, c)],
        5 => vec![(1.0, 0.0), (0.0, 1.0), (c, c), (c, 0.0), (0.0, c)],
        6 => vec![(1.0, 0.0), (0.0, 1.0), (c, c), (0.0, 0.0), (c, 0.0), (0.0, c)],
        7 => vec![(1.0, 0.0), (0.0, 1.0), (c, c), (0.0, 0.0), (c * c, c * c), (c, 0.0), (0.0, c)],
        8 => vec![(1.0, 0.0), (0.0, 1.0), (c, c), (0.0, 0.0), (c, 0.0), (0.0, c), (c, 0.0), (0.0, c)],
        n => (0..n).map(|i| if i % 2 == 0 { (1.0, 0.0) } else { (0.0, 1.0) }).collect(),
    }
}

pub(crate) fn downmix_to_stereo(input: &[f32], channels: u16, out: &mut Vec<f32>) {
    let ch = channels as usize;
    match ch {
        0 => {}
        1 => {
            out.reserve(input.len() * 2);
            for &s in input {
                out.push(s);
                out.push(s);
            }
        }
        2 => out.extend_from_slice(&input[..input.len() & !1]),
        _ => {
            let mut coef = coefficients(ch);
            let sum_l: f32 = coef.iter().map(|c| c.0).sum();
            let sum_r: f32 = coef.iter().map(|c| c.1).sum();
            let norm = 1.0 / sum_l.max(sum_r).max(1.0);
            for c in &mut coef {
                c.0 *= norm;
                c.1 *= norm;
            }
            out.reserve(input.len() / ch * 2);
            for frame in input.chunks_exact(ch) {
                let mut l = 0.0;
                let mut r = 0.0;
                for (s, (cl, cr)) in frame.iter().zip(&coef) {
                    l += s * cl;
                    r += s * cr;
                }
                out.push(l);
                out.push(r);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mono_and_stereo() {
        let mut out = Vec::new();
        downmix_to_stereo(&[0.5, -0.25], 1, &mut out);
        assert_eq!(out, vec![0.5, 0.5, -0.25, -0.25]);
        out.clear();
        downmix_to_stereo(&[0.1, 0.2, 0.3, 0.4], 2, &mut out);
        assert_eq!(out, vec![0.1, 0.2, 0.3, 0.4]);
    }

    #[test]
    fn surround_never_clips() {
        for ch in 3..=10u16 {
            let input = vec![1.0f32; ch as usize * 4];
            let mut out = Vec::new();
            downmix_to_stereo(&input, ch, &mut out);
            assert_eq!(out.len(), 8);
            assert!(out.iter().all(|&v| v <= 1.0 + 1e-6 && v > 0.3), "{ch}: {out:?}");
        }
        // 5.1: LFE is dropped, centre goes to both sides.
        let mut out = Vec::new();
        downmix_to_stereo(&[0.0, 0.0, 1.0, 1.0, 0.0, 0.0], 6, &mut out);
        assert!((out[0] - out[1]).abs() < 1e-6 && out[0] > 0.2);
        out.clear();
        downmix_to_stereo(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0], 6, &mut out);
        assert_eq!(out, vec![0.0, 0.0]);
    }
}
