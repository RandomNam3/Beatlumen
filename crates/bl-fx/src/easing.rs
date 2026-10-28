//! Penner/tween.js style easing functions.

use std::f32::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Easing {
    Linear,
    QuadIn,
    QuadOut,
    QuadInOut,
    CubicIn,
    CubicOut,
    CubicInOut,
    QuarticIn,
    QuarticOut,
    QuarticInOut,
    QuinticIn,
    QuinticOut,
    QuinticInOut,
    SineIn,
    SineOut,
    SineInOut,
    ExponentialIn,
    ExponentialOut,
    ExponentialInOut,
    CircularIn,
    CircularOut,
    CircularInOut,
    ElasticIn,
    ElasticOut,
    ElasticInOut,
    BackIn,
    BackOut,
    BackInOut,
    BounceIn,
    BounceOut,
    BounceInOut,
}

/// All easings with their preset names.
pub const ALL: &[(Easing, &str)] = &[
    (Easing::Linear, "Linear"),
    (Easing::QuadIn, "QuadIn"),
    (Easing::QuadOut, "QuadOut"),
    (Easing::QuadInOut, "QuadInOut"),
    (Easing::CubicIn, "CubicIn"),
    (Easing::CubicOut, "CubicOut"),
    (Easing::CubicInOut, "CubicInOut"),
    (Easing::QuarticIn, "QuarticIn"),
    (Easing::QuarticOut, "QuarticOut"),
    (Easing::QuarticInOut, "QuarticInOut"),
    (Easing::QuinticIn, "QuinticIn"),
    (Easing::QuinticOut, "QuinticOut"),
    (Easing::QuinticInOut, "QuinticInOut"),
    (Easing::SineIn, "SineIn"),
    (Easing::SineOut, "SineOut"),
    (Easing::SineInOut, "SineInOut"),
    (Easing::ExponentialIn, "ExponentialIn"),
    (Easing::ExponentialOut, "ExponentialOut"),
    (Easing::ExponentialInOut, "ExponentialInOut"),
    (Easing::CircularIn, "CircularIn"),
    (Easing::CircularOut, "CircularOut"),
    (Easing::CircularInOut, "CircularInOut"),
    (Easing::ElasticIn, "ElasticIn"),
    (Easing::ElasticOut, "ElasticOut"),
    (Easing::ElasticInOut, "ElasticInOut"),
    (Easing::BackIn, "BackIn"),
    (Easing::BackOut, "BackOut"),
    (Easing::BackInOut, "BackInOut"),
    (Easing::BounceIn, "BounceIn"),
    (Easing::BounceOut, "BounceOut"),
    (Easing::BounceInOut, "BounceInOut"),
];

impl Easing {
    pub fn name(self) -> &'static str {
        ALL.iter()
            .find(|(e, _)| *e == self)
            .map(|(_, n)| *n)
            .unwrap_or("Linear")
    }

    /// Parses a preset easing name. Accepts the misspelled "BouceIn" used by
    /// presets written by the original application.
    pub fn from_name(s: &str) -> Option<Easing> {
        if s == "BouceIn" {
            return Some(Easing::BounceIn);
        }
        ALL.iter().find(|(_, n)| *n == s).map(|(e, _)| *e)
    }

    pub fn apply(self, k: f32) -> f32 {
        let k = k.clamp(0.0, 1.0);
        match self {
            Easing::Linear => k,
            Easing::QuadIn => k * k,
            Easing::QuadOut => k * (2.0 - k),
            Easing::QuadInOut => {
                let t = k * 2.0;
                if t < 1.0 {
                    0.5 * t * t
                } else {
                    let t = t - 1.0;
                    -0.5 * (t * (t - 2.0) - 1.0)
                }
            }
            Easing::CubicIn => k * k * k,
            Easing::CubicOut => {
                let t = k - 1.0;
                t * t * t + 1.0
            }
            Easing::CubicInOut => {
                let t = k * 2.0;
                if t < 1.0 {
                    0.5 * t * t * t
                } else {
                    let t = t - 2.0;
                    0.5 * (t * t * t + 2.0)
                }
            }
            Easing::QuarticIn => k * k * k * k,
            Easing::QuarticOut => {
                let t = k - 1.0;
                1.0 - t * t * t * t
            }
            Easing::QuarticInOut => {
                let t = k * 2.0;
                if t < 1.0 {
                    0.5 * t * t * t * t
                } else {
                    let t = t - 2.0;
                    -0.5 * (t * t * t * t - 2.0)
                }
            }
            Easing::QuinticIn => k * k * k * k * k,
            Easing::QuinticOut => {
                let t = k - 1.0;
                t * t * t * t * t + 1.0
            }
            Easing::QuinticInOut => {
                let t = k * 2.0;
                if t < 1.0 {
                    0.5 * t * t * t * t * t
                } else {
                    let t = t - 2.0;
                    0.5 * (t * t * t * t * t + 2.0)
                }
            }
            Easing::SineIn => 1.0 - (k * PI / 2.0).cos(),
            Easing::SineOut => (k * PI / 2.0).sin(),
            Easing::SineInOut => 0.5 * (1.0 - (PI * k).cos()),
            Easing::ExponentialIn => {
                if k == 0.0 {
                    0.0
                } else {
                    1024f32.powf(k - 1.0)
                }
            }
            Easing::ExponentialOut => {
                if k == 1.0 {
                    1.0
                } else {
                    1.0 - 2f32.powf(-10.0 * k)
                }
            }
            Easing::ExponentialInOut => {
                if k == 0.0 {
                    0.0
                } else if k == 1.0 {
                    1.0
                } else {
                    let t = k * 2.0;
                    if t < 1.0 {
                        0.5 * 1024f32.powf(t - 1.0)
                    } else {
                        0.5 * (2.0 - 2f32.powf(-10.0 * (t - 1.0)))
                    }
                }
            }
            Easing::CircularIn => 1.0 - (1.0 - k * k).max(0.0).sqrt(),
            Easing::CircularOut => {
                let t = k - 1.0;
                (1.0 - t * t).max(0.0).sqrt()
            }
            Easing::CircularInOut => {
                let t = k * 2.0;
                if t < 1.0 {
                    -0.5 * ((1.0 - t * t).max(0.0).sqrt() - 1.0)
                } else {
                    let t = t - 2.0;
                    0.5 * ((1.0 - t * t).max(0.0).sqrt() + 1.0)
                }
            }
            Easing::ElasticIn => {
                if k == 0.0 || k == 1.0 {
                    k
                } else {
                    -(2f32.powf(10.0 * (k - 1.0))) * (5.0 * (k - 1.1) * PI).sin()
                }
            }
            Easing::ElasticOut => {
                if k == 0.0 || k == 1.0 {
                    k
                } else {
                    2f32.powf(-10.0 * k) * (5.0 * (k - 0.1) * PI).sin() + 1.0
                }
            }
            Easing::ElasticInOut => {
                if k == 0.0 || k == 1.0 {
                    k
                } else {
                    let t = k * 2.0;
                    if t < 1.0 {
                        -0.5 * 2f32.powf(10.0 * (t - 1.0)) * (5.0 * (t - 1.1) * PI).sin()
                    } else {
                        0.5 * 2f32.powf(-10.0 * (t - 1.0)) * (5.0 * (t - 1.1) * PI).sin() + 1.0
                    }
                }
            }
            Easing::BackIn => {
                let s = 1.70158;
                k * k * ((s + 1.0) * k - s)
            }
            Easing::BackOut => {
                let s = 1.70158;
                let t = k - 1.0;
                t * t * ((s + 1.0) * t + s) + 1.0
            }
            Easing::BackInOut => {
                let s = 2.594_909_5;
                let t = k * 2.0;
                if t < 1.0 {
                    0.5 * (t * t * ((s + 1.0) * t - s))
                } else {
                    let t = t - 2.0;
                    0.5 * (t * t * ((s + 1.0) * t + s) + 2.0)
                }
            }
            Easing::BounceIn => 1.0 - bounce_out(1.0 - k),
            Easing::BounceOut => bounce_out(k),
            Easing::BounceInOut => {
                if k < 0.5 {
                    0.5 * (1.0 - bounce_out(1.0 - k * 2.0))
                } else {
                    0.5 * bounce_out(k * 2.0 - 1.0) + 0.5
                }
            }
        }
    }
}

fn bounce_out(k: f32) -> f32 {
    if k < 1.0 / 2.75 {
        7.5625 * k * k
    } else if k < 2.0 / 2.75 {
        let t = k - 1.5 / 2.75;
        7.5625 * t * t + 0.75
    } else if k < 2.5 / 2.75 {
        let t = k - 2.25 / 2.75;
        7.5625 * t * t + 0.9375
    } else {
        let t = k - 2.625 / 2.75;
        7.5625 * t * t + 0.984375
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoints() {
        for (e, name) in ALL {
            assert!((e.apply(0.0)).abs() < 1e-4, "{name} at 0");
            assert!((e.apply(1.0) - 1.0).abs() < 1e-4, "{name} at 1");
            assert_eq!(Easing::from_name(name), Some(*e));
        }
        assert_eq!(Easing::from_name("BouceIn"), Some(Easing::BounceIn));
    }
}
