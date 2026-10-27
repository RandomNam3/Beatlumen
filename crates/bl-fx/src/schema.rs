//! Declarative description of every effect type: its configuration fields,
//! defaults, ranges and trigger fields. Used to fill defaults when loading
//! presets, to clamp values and to generate editor forms.

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FieldKind {
    Number,
    Integer,
    Easing,
    /// String enumeration: (value, label).
    Enum(&'static [(&'static str, &'static str)]),
    /// Integer enumeration: (value, label).
    IntEnum(&'static [(i64, &'static str)]),
    /// 0xRRGGBB colour stored as an integer.
    Color,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Field {
    pub key: &'static str,
    pub label: &'static str,
    pub kind: FieldKind,
    /// Default numeric value, or index into the enum list for enums
    /// (for `Easing` the default name is in `default_str`).
    pub default: f64,
    pub default_str: &'static str,
    pub min: f64,
    pub max: f64,
    pub step: f64,
    pub unit: &'static str,
    pub help: &'static str,
}

const INF: f64 = f64::INFINITY;

const fn num(key: &'static str, label: &'static str, default: f64, min: f64, max: f64, step: f64) -> Field {
    Field { key, label, kind: FieldKind::Number, default, default_str: "", min, max, step, unit: "", help: "" }
}
const fn int(key: &'static str, label: &'static str, default: f64, min: f64, max: f64) -> Field {
    Field { key, label, kind: FieldKind::Integer, default, default_str: "", min, max, step: 1.0, unit: "", help: "" }
}
const fn deg(key: &'static str, label: &'static str, default: f64, min: f64, max: f64) -> Field {
    Field { key, label, kind: FieldKind::Number, default, default_str: "", min, max, step: 1.0, unit: "°", help: "" }
}
const fn easing(key: &'static str, label: &'static str, default: &'static str) -> Field {
    Field { key, label, kind: FieldKind::Easing, default: 0.0, default_str: default, min: 0.0, max: 0.0, step: 0.0, unit: "", help: "" }
}
const fn senum(key: &'static str, label: &'static str, values: &'static [(&'static str, &'static str)], default: &'static str) -> Field {
    Field { key, label, kind: FieldKind::Enum(values), default: 0.0, default_str: default, min: 0.0, max: 0.0, step: 0.0, unit: "", help: "" }
}
const fn ienum(key: &'static str, label: &'static str, values: &'static [(i64, &'static str)], default: f64) -> Field {
    Field { key, label, kind: FieldKind::IntEnum(values), default, default_str: "", min: 0.0, max: 0.0, step: 0.0, unit: "", help: "" }
}
const fn secs(key: &'static str, label: &'static str, default: f64) -> Field {
    Field { key, label, kind: FieldKind::Number, default, default_str: "", min: 0.0, max: INF, step: 0.01, unit: "s", help: "" }
}

pub const NOVELTY_TYPES: &[(&str, &str)] = &[("None", "None"), ("Lows", "Lows"), ("Mids", "Mids"), ("Highs", "Highs")];
pub const TRANSITION_TYPES: &[(&str, &str)] = &[("None", "None"), ("Minor", "Minor"), ("Major", "Major")];
pub const CAMERA_TYPES: &[(&str, &str)] = &[("None", "None"), ("Absolute", "Absolute"), ("Relative", "Relative")];
pub const DIRECTIONS: &[(i64, &str)] = &[(0, "Left"), (1, "Right"), (2, "Top"), (3, "Bottom")];
pub const SPECTRA: &[(&str, &str)] = &[("Raw", "Raw"), ("Harmonic", "Harmonic"), ("Percussive", "Percussive")];

/// Novelty trigger base fields (Camera and Filter effects).
pub const NOVELTY_TRIGGER: &[Field] = &[
    senum("type", "Novelty", NOVELTY_TYPES, "None"),
    num("sensitivity", "Sensitivity", 1.0, 0.0, INF, 0.1),
    num("threshold", "Threshold", 0.0, 0.0, 1.0, 0.01),
    num("smoothing", "Smoothing", 0.8, 0.0, 1.0, 0.01),
];

/// Transition trigger fields.
pub const TRANSITION_TRIGGER: &[Field] = &[
    senum("type", "Transition", TRANSITION_TYPES, "None"),
    num("weight", "Weight", 1.0, 0.0, INF, 1.0),
    num("threshold", "Energy Threshold", 0.0, 0.0, 1.0, 0.01),
];

/// Range fields of a camera position block, optionally prefixed.
macro_rules! ranges {
    ($p:literal) => {
        [
            num(concat!($p, "xMin"), "Horizontal Min", 0.0, 0.0, 1.0, 0.01),
            num(concat!($p, "xMax"), "Horizontal Max", 1.0, 0.0, 1.0, 0.01),
            num(concat!($p, "yMin"), "Vertical Min", 0.0, 0.0, 1.0, 0.01),
            num(concat!($p, "yMax"), "Vertical Max", 1.0, 0.0, 1.0, 0.01),
            num(concat!($p, "zMin"), "Zoom Min", 0.8, 0.0, 1.0, 0.01),
            num(concat!($p, "zMax"), "Zoom Max", 1.0, 0.0, 1.0, 0.01),
            deg(concat!($p, "zrMin"), "Rotation Min", 0.0, 0.0, 360.0),
            deg(concat!($p, "zrMax"), "Rotation Max", 0.0, 0.0, 360.0),
        ]
    };
}
macro_rules! offsets {
    ($p:literal) => {
        [
            num(concat!($p, "xOffset"), "Horizontal Offset", 0.0, 0.0, 1.0, 0.01),
            num(concat!($p, "yOffset"), "Vertical Offset", 0.0, 0.0, 1.0, 0.01),
            num(concat!($p, "zOffset"), "Zoom Offset", 0.0, 0.0, 1.0, 0.01),
            deg(concat!($p, "zrOffset"), "Rotation Offset", 0.0, 0.0, 360.0),
        ]
    };
}

const fn concat_fields<const A: usize, const B: usize, const C: usize>(a: [Field; A], b: [Field; B]) -> [Field; C] {
    let mut out = [a[0]; C];
    let mut i = 0;
    while i < A {
        out[i] = a[i];
        i += 1;
    }
    let mut j = 0;
    while j < B {
        out[A + j] = b[j];
        j += 1;
    }
    out
}

const RANGES: [Field; 8] = ranges!("");
const OFFSETS: [Field; 4] = offsets!("");
const POSITION: [Field; 12] = concat_fields(RANGES, OFFSETS);

/// Camera block "exit"/"finish": type + easing + ranges + offsets.
const CAM_EXIT: [Field; 14] =
    concat_fields(concat_fields::<1, 1, 2>([senum("type", "Type", CAMERA_TYPES, "None")], [easing("easing", "Easing", "CubicOut")]), POSITION);
const CAM_FINISH: [Field; 14] =
    concat_fields(concat_fields::<1, 1, 2>([senum("type", "Type", CAMERA_TYPES, "None")], [easing("easing", "Easing", "CubicIn")]), POSITION);
pub const CAMERA_EXIT: &[Field] = &CAM_EXIT;
pub const CAMERA_ENTER: &[Field] = &RANGES;
pub const CAMERA_FINISH: &[Field] = &CAM_FINISH;
pub const CAMERA_SPEED: Field = num("speed", "Camera Speed", 0.0, 0.0, 1.0, 0.01);

const SHARED: [Field; 2] = [easing("easing", "Easing", "CubicOut"), num("speed", "Speed", 0.0, 0.0, 1.0, 0.01)];
const MOVE: [Field; 15] = concat_fields(concat_fields::<2, 1, 3>(SHARED, [senum("type", "Camera Type", CAMERA_TYPES, "None")]), POSITION);
const FINISH_PREFIXED: [Field; 13] = concat_fields(
    concat_fields::<1, 8, 9>([senum("finishType", "Finish Type", CAMERA_TYPES, "None")], ranges_finish()),
    offsets_finish(),
);
const fn ranges_finish() -> [Field; 8] {
    [
        num("finishXMin", "Finish Horizontal Min", 0.0, 0.0, 1.0, 0.01),
        num("finishXMax", "Finish Horizontal Max", 1.0, 0.0, 1.0, 0.01),
        num("finishYMin", "Finish Vertical Min", 0.0, 0.0, 1.0, 0.01),
        num("finishYMax", "Finish Vertical Max", 1.0, 0.0, 1.0, 0.01),
        num("finishZMin", "Finish Zoom Min", 0.8, 0.0, 1.0, 0.01),
        num("finishZMax", "Finish Zoom Max", 1.0, 0.0, 1.0, 0.01),
        deg("finishZRMin", "Finish Rotation Min", 0.0, 0.0, 360.0),
        deg("finishZRMax", "Finish Rotation Max", 0.0, 0.0, 360.0),
    ]
}
const fn offsets_finish() -> [Field; 4] {
    [
        num("finishXOffset", "Finish Horizontal Offset", 0.0, 0.0, 1.0, 0.01),
        num("finishYOffset", "Finish Vertical Offset", 0.0, 0.0, 1.0, 0.01),
        num("finishZOffset", "Finish Zoom Offset", 0.0, 0.0, 1.0, 0.01),
        deg("finishZROffset", "Finish Rotation Offset", 0.0, 0.0, 360.0),
    ]
}
const SWITCH: [Field; 28] = concat_fields(MOVE, FINISH_PREFIXED);

const BLUR_T: [Field; 4] = concat_fields(SHARED, [num("intensity", "Intensity", 1.0, 0.0, INF, 0.01), int("samples", "Samples", 32.0, 0.0, 64.0)]);
const WIPE_T: [Field; 5] = concat_fields(
    SHARED,
    [ienum("direction", "Direction", DIRECTIONS, 1.0), num("gradient", "Gradient", 0.0, 0.0, 1.0, 0.01), deg("angle", "Angle", 0.0, -INF, INF)],
);
const SLIDE_T: [Field; 6] = concat_fields(
    SHARED,
    [
        ienum("direction", "Direction", DIRECTIONS, 1.0),
        int("slides", "Slides", 1.0, 0.0, 100.0),
        num("intensity", "Intensity", 1.0, 0.0, INF, 0.01),
        int("samples", "Samples", 32.0, 0.0, 64.0),
    ],
);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TriggerKind {
    None,
    Novelty,
    Transition,
}

#[derive(Debug)]
pub struct EffectSpec {
    pub effect_type: &'static str,
    pub ty: &'static str,
    pub default_name: &'static str,
    pub description: &'static str,
    pub config: &'static [Field],
    pub trigger: TriggerKind,
    /// Filter trigger target fields ("Trigger Animation").
    pub targets: &'static [Field],
    /// Whether the effect has a `camera` block (visual transitions).
    pub camera: bool,
}

pub const EFFECTS: &[EffectSpec] = &[
    EffectSpec {
        effect_type: "Camera",
        ty: "Sway",
        default_name: "Camera Sway",
        description: "Continuous random drift of the camera.",
        config: &[
            easing("easing", "Easing", "Linear"),
            num("x", "Horizontal Sway", 0.1, 0.0, 1.0, 0.01),
            num("y", "Vertical Sway", 0.1, 0.0, 1.0, 0.01),
            num("z", "Zoom Sway", 0.05, 0.0, 1.0, 0.01),
            deg("zr", "Rotational Sway", 0.0, -INF, INF),
            secs("duration", "Cycle Duration", 3.0),
        ],
        trigger: TriggerKind::Novelty,
        targets: &[],
        camera: false,
    },
    EffectSpec {
        effect_type: "Camera",
        ty: "Shake",
        default_name: "Camera Shake",
        description: "Audio driven camera jitter (requires a trigger).",
        config: &[
            num("x", "Horizontal Shake", 0.0, 0.0, 1.0, 0.01),
            num("y", "Vertical Shake", 0.0, 0.0, 1.0, 0.01),
            num("z", "Zoom Shake", 0.0, 0.0, 1.0, 0.01),
            deg("zr", "Rotational Shake", 0.0, -INF, INF),
        ],
        trigger: TriggerKind::Novelty,
        targets: &[],
        camera: false,
    },
    EffectSpec {
        effect_type: "Particle",
        ty: "Basic",
        default_name: "Basic Particle",
        description: "Drifting soft particles.",
        config: &[
            int("amount", "Amount", 50.0, 0.0, 100000.0),
            num("minSize", "Min Size", 0.0, 0.0, 500.0, 1.0),
            num("maxSize", "Max Size", 10.0, 0.0, 500.0, 1.0),
            num("minOpacity", "Min Opacity", 0.0, 0.0, 1.0, 0.01),
            num("maxOpacity", "Max Opacity", 1.0, 0.0, 1.0, 0.01),
            num("minGradient", "Min Gradient", 0.0, 0.0, 1.0, 0.01),
            num("maxGradient", "Max Gradient", 1.0, 0.0, 1.0, 0.01),
            num("smoothing", "Smoothing", 0.5, 0.0, 1.0, 0.01),
            Field { key: "color", label: "Color", kind: FieldKind::Color, default: 16777215.0, default_str: "", min: 0.0, max: 16777215.0, step: 1.0, unit: "", help: "" },
            num("speed", "Speed", 0.1, 0.0, 1.0, 0.01),
            easing("easing", "Sway Easing", "Linear"),
            deg("angle", "Angle", 0.0, -INF, INF),
            num("swayDistance", "Sway Distance", 0.025, 0.0, 1.0, 0.001),
            secs("swayDuration", "Sway Duration", 1.5),
        ],
        trigger: TriggerKind::None,
        targets: &[],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "Blur",
        default_name: "Blur Filter",
        description: "Gaussian blur.",
        config: &[num("radius", "Radius", 1.0, 0.0, INF, 0.01), int("passes", "Passes", 1.0, 0.0, 10.0)],
        trigger: TriggerKind::Novelty,
        targets: &[num("radius", "Radius", 1.0, 0.0, INF, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "Bloom",
        default_name: "Bloom Filter",
        description: "Glow by blending a blurred copy.",
        config: &[num("opacity", "Opacity", 1.0, 0.0, INF, 0.01), num("radius", "Radius", 1.0, 0.0, INF, 0.01), int("passes", "Passes", 1.0, 0.0, 10.0)],
        trigger: TriggerKind::Novelty,
        targets: &[num("opacity", "Opacity", 1.0, 0.0, INF, 0.01), num("radius", "Radius", 1.0, 0.0, INF, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "RgbShift",
        default_name: "RGB Shift Filter",
        description: "Chromatic aberration.",
        config: &[num("amount", "Amount", 0.005, 0.0, 1.0, 0.001), deg("angle", "Angle", 0.0, -INF, INF)],
        trigger: TriggerKind::Novelty,
        targets: &[num("amount", "Amount", 0.005, 0.0, 1.0, 0.001)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "Vignette",
        default_name: "Vignette Filter",
        description: "Darkened edges.",
        config: &[num("darkness", "Darkness", 1.0, 0.0, INF, 0.01), num("offset", "Offset", 1.0, 0.0, INF, 0.01)],
        trigger: TriggerKind::Novelty,
        targets: &[num("darkness", "Darkness", 1.0, 0.0, INF, 0.01), num("offset", "Offset", 1.0, 0.0, INF, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "VignetteBlur",
        default_name: "Vignette Blur Filter",
        description: "Blurred edges.",
        config: &[num("size", "Size", 1.0, 0.0, INF, 0.01), num("radius", "Radius", 1.0, 0.0, INF, 0.01), int("passes", "Passes", 1.0, 0.0, 10.0)],
        trigger: TriggerKind::Novelty,
        targets: &[num("size", "Size", 1.0, 0.0, INF, 0.01), num("radius", "Radius", 1.0, 0.0, INF, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "MotionBlur",
        default_name: "Motion Blur Filter",
        description: "Blur along camera movement.",
        config: &[num("intensity", "Intensity", 1.0, 0.0, INF, 0.01), int("samples", "Samples", 32.0, 0.0, 64.0)],
        trigger: TriggerKind::Novelty,
        targets: &[num("intensity", "Intensity", 1.0, 0.0, INF, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Filter",
        ty: "Glitch",
        default_name: "Glitch Filter",
        description: "Digital glitch blocks.",
        config: &[num("amount", "Amount", 0.5, 0.0, 1.0, 0.01), int("frequency", "Frequency", 1.0, 1.0, 60.0)],
        trigger: TriggerKind::Novelty,
        targets: &[num("amount", "Amount", 0.5, 0.0, 1.0, 0.01)],
        camera: false,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "CameraMove",
        default_name: "Camera Move Transition",
        description: "Moves the camera within the current image.",
        config: &MOVE,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: false,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "CameraSwitch",
        default_name: "Camera Switch Transition",
        description: "Cuts to a new camera position on the beat, then glides.",
        config: &SWITCH,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: false,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "Blend",
        default_name: "Blend Transition",
        description: "Cross fade.",
        config: &SHARED,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: true,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "Blur",
        default_name: "Blur Transition",
        description: "Cross fade with horizontal motion blur.",
        config: &BLUR_T,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: true,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "Wipe",
        default_name: "Wipe Transition",
        description: "Angled wipe with soft edge.",
        config: &WIPE_T,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: true,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "Slide",
        default_name: "Slide Transition",
        description: "Sliding panels with motion blur.",
        config: &SLIDE_T,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: true,
    },
    EffectSpec {
        effect_type: "Transition",
        ty: "Glitch",
        default_name: "Glitch Transition",
        description: "Glitchy cut.",
        config: &SHARED,
        trigger: TriggerKind::Transition,
        targets: &[],
        camera: true,
    },
];

pub fn find(effect_type: &str, ty: &str) -> Option<&'static EffectSpec> {
    EFFECTS.iter().find(|s| s.effect_type == effect_type && s.ty == ty)
}

/// Analysis configuration fields (`analysisConfig`).
pub const ANALYSIS: &[Field] = &[
    int("spectralSmoothing", "Spectral Smoothing", 4.0, 1.0, 50.0),
    num("spectralSeparation", "Spectral Separation", 3.0, 1.0, 5.0, 0.1),
    int("chromaSmoothing", "Chroma Smoothing", 30.0, 1.0, 100.0),
    int("chromaDownSampling", "Chroma Downsampling", 10.0, 1.0, 50.0),
    int("majorTransitionWindow", "Major Transition Window", 30.0, 1.0, 100.0),
    int("minorTransitionWindow", "Minor Transition Window", 5.0, 1.0, 100.0),
];

/// Fields of one `noveltyConfigs` entry.
pub const NOVELTY: &[Field] = &[
    senum("spectrum", "Audio Spectrum", SPECTRA, "Percussive"),
    num("normalization", "Normalization", 1.0, 1.0, 50.0, 0.1),
    Field { key: "startFreq", label: "Start Frequency", kind: FieldKind::Number, default: 0.0, default_str: "", min: 0.0, max: 4000.0, step: 10.0, unit: "Hz", help: "" },
    Field { key: "endFreq", label: "End Frequency", kind: FieldKind::Number, default: 300.0, default_str: "", min: 0.0, max: 4000.0, step: 10.0, unit: "Hz", help: "" },
];

/// Default novelty band settings (Low, Mid, High).
pub const NOVELTY_DEFAULTS: [(&str, &str, f64, f64, f64); 3] =
    [("Low", "Percussive", 10.0, 0.0, 300.0), ("Mid", "Percussive", 3.0, 300.0, 800.0), ("High", "Percussive", 2.0, 800.0, 2000.0)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_specs_and_keys() {
        for (i, a) in EFFECTS.iter().enumerate() {
            for b in &EFFECTS[i + 1..] {
                assert!(!(a.effect_type == b.effect_type && a.ty == b.ty));
            }
            let mut keys: Vec<_> = a.config.iter().map(|f| f.key).collect();
            keys.sort();
            keys.dedup();
            assert_eq!(keys.len(), a.config.len(), "{}", a.ty);
        }
        assert_eq!(find("Transition", "CameraSwitch").unwrap().config.len(), 28);
    }
}
