//! Joint stereo and noise tools: M/S stereo, intensity stereo and
//! perceptual noise substitution (ISO/IEC 14496-3 4.6.8, 4.6.13).

use crate::ics::{ChannelData, INTENSITY_HCB, INTENSITY_HCB2, MAX_SFB, NOISE_HCB};

/// Pseudo-random generator for PNS (32-bit LCG).
pub(crate) struct NoiseGen {
    pub state: u32,
}

impl NoiseGen {
    #[inline]
    fn next(&mut self) -> f32 {
        self.state = self.state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        (self.state as i32) as f32 * (1.0 / 2_147_483_648.0)
    }
}

fn window_len(ch: &ChannelData) -> usize {
    if ch.info.is_short() {
        128
    } else {
        1024
    }
}

/// Fills one band of one window with noise of energy 2^(nrg/2).
fn noise_band(spec: &mut [f32], nrg: i32, rng: &mut NoiseGen) {
    let mut energy = 0.0f32;
    for v in spec.iter_mut() {
        let r = rng.next();
        *v = r;
        energy += r * r;
    }
    if energy > 0.0 {
        let scale = 2f32.powf(0.25 * nrg as f32) / energy.sqrt();
        spec.iter_mut().for_each(|v| *v *= scale);
    }
}

/// Perceptual noise substitution for one channel. When `left` is given
/// (right channel of a CPE) and M/S is signalled for a band where both
/// channels are noise, the left channel's noise is reused (correlated).
pub(crate) fn apply_pns(ch: &mut ChannelData, left: Option<(&ChannelData, &[bool])>, rng: &mut NoiseGen) {
    let info = ch.info;
    let wlen = window_len(ch);
    let mut win = 0;
    for g in 0..info.num_groups {
        let glen = info.group_len[g] as usize;
        for sfb in 0..info.max_sfb {
            let i = g * MAX_SFB + sfb;
            if ch.band_type[i] != NOISE_HCB {
                continue;
            }
            let start = info.swb[sfb] as usize;
            let end = info.swb[sfb + 1] as usize;
            let nrg = ch.sf[i];
            match left {
                Some((l, ms_used)) if ms_used[i] && l.band_type[i] == NOISE_HCB => {
                    let ratio = 2f32.powf(0.25 * (nrg - l.sf[i]) as f32);
                    for w in win..win + glen {
                        let r = w * wlen + start..w * wlen + end;
                        for (d, s) in ch.spec[r.clone()].iter_mut().zip(&l.spec[r]) {
                            *d = s * ratio;
                        }
                    }
                }
                _ => {
                    for w in win..win + glen {
                        noise_band(&mut ch.spec[w * wlen + start..w * wlen + end], nrg, rng);
                    }
                }
            }
        }
        win += glen;
    }
}

/// M/S stereo reconstruction (skips noise and intensity bands).
pub(crate) fn apply_ms(l: &mut ChannelData, r: &mut ChannelData, ms_used: &[bool]) {
    let info = l.info;
    let wlen = window_len(l);
    let mut win = 0;
    for g in 0..info.num_groups {
        let glen = info.group_len[g] as usize;
        for sfb in 0..info.max_sfb {
            let i = g * MAX_SFB + sfb;
            if !ms_used[i] || l.band_type[i] >= NOISE_HCB || r.band_type[i] >= NOISE_HCB {
                continue;
            }
            let start = info.swb[sfb] as usize;
            let end = info.swb[sfb + 1] as usize;
            for w in win..win + glen {
                let range = w * wlen + start..w * wlen + end;
                for (a, b) in l.spec[range.clone()].iter_mut().zip(&mut r.spec[range]) {
                    let m = *a;
                    let s = *b;
                    *a = m + s;
                    *b = m - s;
                }
            }
        }
        win += glen;
    }
}

/// Intensity stereo: right channel bands coded with the intensity codebooks
/// are derived from the left channel.
pub(crate) fn apply_intensity(l: &ChannelData, r: &mut ChannelData, ms_used: &[bool], ms_present: bool) {
    let info = r.info;
    let wlen = window_len(r);
    let mut win = 0;
    for g in 0..info.num_groups {
        let glen = info.group_len[g] as usize;
        for sfb in 0..info.max_sfb {
            let i = g * MAX_SFB + sfb;
            let cb = r.band_type[i];
            if cb != INTENSITY_HCB && cb != INTENSITY_HCB2 {
                continue;
            }
            let mut scale = 0.5f32.powf(0.25 * r.sf[i] as f32);
            if cb == INTENSITY_HCB2 {
                scale = -scale;
            }
            if ms_present && ms_used[i] {
                scale = -scale;
            }
            let start = info.swb[sfb] as usize;
            let end = info.swb[sfb + 1] as usize;
            for w in win..win + glen {
                let range = w * wlen + start..w * wlen + end;
                for (d, s) in r.spec[range.clone()].iter_mut().zip(&l.spec[range]) {
                    *d = s * scale;
                }
            }
        }
        win += glen;
    }
}
