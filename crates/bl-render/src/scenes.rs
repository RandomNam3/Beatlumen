//! Endless animations that need no pictures, and the limiter of the
//! photosensitive mode. All shaders here are written for Beatlumen.

use crate::gpu::Program;
use crate::gpu::FULLSCREEN_VS;
use bl_gl::Gl;

/// An endless animation that replaces the picture of the background.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SceneKind {
    Flow,
    Tunnel,
    Stars,
    Aurora,
    Waves,
}

impl SceneKind {
    pub const ALL: [SceneKind; 5] = [
        SceneKind::Flow,
        SceneKind::Tunnel,
        SceneKind::Stars,
        SceneKind::Aurora,
        SceneKind::Waves,
    ];

    pub fn parse(s: &str) -> Option<SceneKind> {
        Some(match s {
            "flow" => SceneKind::Flow,
            "tunnel" => SceneKind::Tunnel,
            "stars" => SceneKind::Stars,
            "aurora" => SceneKind::Aurora,
            "waves" => SceneKind::Waves,
            _ => return None,
        })
    }

    fn shader(self) -> String {
        format!(
            "{COMMON}{}",
            match self {
                SceneKind::Flow => FLOW_FS,
                SceneKind::Tunnel => TUNNEL_FS,
                SceneKind::Stars => STARS_FS,
                SceneKind::Aurora => AURORA_FS,
                SceneKind::Waves => WAVES_FS,
            }
        )
    }

    pub(crate) fn program(self, gl: &Gl) -> Result<Program, String> {
        Program::new(gl, FULLSCREEN_VS, &self.shader(), &[])
    }
}

/// What the animation follows, each from 0 to about 1.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SceneInput {
    pub kind: SceneKind,
    /// Runs on endlessly (seconds, may go faster with the music).
    pub time: f32,
    pub bass: f32,
    pub mid: f32,
    pub high: f32,
    pub energy: f32,
}

const COMMON: &str = r#"
uniform float time;
uniform float bass;
uniform float mid;
uniform float high;
uniform float energy;
uniform float aspect;
in vec2 vUv;
out vec4 fragColor;
const float TAU = 6.28318530718;
vec3 palette(float t) {
    return 0.5 + 0.5 * cos(TAU * (t + vec3(0.0, 0.33, 0.67)));
}
float hash(vec2 p) {
    return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);
}
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    return mix(mix(hash(i), hash(i + vec2(1.0, 0.0)), f.x),
               mix(hash(i + vec2(0.0, 1.0)), hash(i + vec2(1.0, 1.0)), f.x), f.y);
}
"#;

/// Slow colour clouds, bent by the bass.
const FLOW_FS: &str = r#"
void main() {
    vec2 p = (vUv - 0.5) * vec2(aspect, 1.0) * 3.0;
    float t = time * 0.15;
    vec2 q = vec2(sin(p.y * 1.3 + t * 1.7) + sin(p.x * 0.7 - t),
                  cos(p.x * 1.1 - t * 1.3) + sin(p.y * 0.9 + t));
    float v = sin(p.x * 1.2 + q.x * (1.0 + 0.8 * bass) + t)
            + sin(p.y * 1.4 + q.y * (1.0 + 0.4 * mid) + t * 0.7)
            + sin((p.x + p.y) * 0.8 + t * 1.3);
    v = v / 6.0 + 0.5;
    vec3 c = palette(v * 0.6 + time * 0.012);
    fragColor = vec4(c * (0.25 + 0.45 * v + 0.30 * energy), 1.0);
}
"#;

/// Flying through a ring tunnel.
const TUNNEL_FS: &str = r#"
void main() {
    vec2 p = (vUv - 0.5) * vec2(aspect, 1.0);
    p += 0.06 * vec2(sin(time * 0.31), cos(time * 0.23));
    float r = length(p);
    float a = atan(p.y, p.x);
    float z = 0.32 / max(r, 0.015) + time * 0.5;
    float rings = 0.5 + 0.5 * sin(z * 5.0);
    float spokes = 0.5 + 0.5 * sin(a * 6.0 + z * 0.8 + time * 0.1);
    float v = mix(rings, rings * spokes, 0.6);
    float fog = smoothstep(0.0, 0.45, r);
    vec3 c = palette(z * 0.035 + 0.12 * mid);
    fragColor = vec4(c * v * fog * (0.45 + 0.55 * energy) + c * 0.05, 1.0);
}
"#;

/// Stars coming closer; faster with the music.
const STARS_FS: &str = r#"
float layer(vec2 p, float seed) {
    vec2 cell = floor(p);
    vec2 f = fract(p) - 0.5;
    float h = hash(cell + seed);
    if (h < 0.75) return 0.0;
    vec2 o = vec2(hash(cell + seed + 1.3), hash(cell + seed + 2.7)) - 0.5;
    float d = length(f - 0.7 * o);
    return smoothstep(0.09, 0.0, d) * (h - 0.75) * 4.0;
}
void main() {
    vec2 uv = (vUv - 0.5) * vec2(aspect, 1.0);
    float glow = 0.0;
    for (int i = 0; i < 5; i++) {
        float depth = fract(float(i) / 5.0 + time * 0.035);
        float fade = smoothstep(0.0, 0.3, depth) * smoothstep(1.0, 0.75, depth);
        float scale = mix(16.0, 1.5, depth);
        glow += layer(uv * scale, float(i) * 17.0) * fade;
    }
    vec3 night = mix(vec3(0.015, 0.02, 0.05), vec3(0.05, 0.03, 0.10), vUv.y);
    vec3 star = mix(vec3(0.75, 0.85, 1.0), palette(time * 0.01 + mid * 0.2), 0.35);
    fragColor = vec4(night + star * glow * (0.7 + 0.5 * high), 1.0);
}
"#;

/// Curtains of light over a dark sky.
const AURORA_FS: &str = r#"
void main() {
    vec2 p = vec2(vUv.x * aspect, vUv.y);
    float t = time * 0.12;
    vec3 c = mix(vec3(0.01, 0.02, 0.05), vec3(0.02, 0.05, 0.10), vUv.y);
    for (int i = 0; i < 4; i++) {
        float fi = float(i);
        float base = 0.45 + fi * 0.09;
        float wobble = (noise(vec2(p.x * 1.4 + fi * 3.1 + t, t * 0.7 + fi)) - 0.5) * 0.5;
        float d = p.y - base - wobble;
        float curtain = smoothstep(0.32 + 0.12 * bass, 0.0, abs(d)) * smoothstep(-0.12, 0.05, d);
        float rays = 0.6 + 0.4 * noise(vec2(p.x * 14.0 + fi * 5.0, t * 1.6));
        vec3 tint = mix(vec3(0.10, 0.85, 0.55), vec3(0.45, 0.25, 0.95), fi / 3.0 + 0.2 * sin(t * 0.5));
        c += tint * curtain * rays * (0.22 + 0.25 * energy);
    }
    fragColor = vec4(c, 1.0);
}
"#;

/// Glowing lines that swing with the music.
const WAVES_FS: &str = r#"
void main() {
    vec2 p = vec2((vUv.x - 0.5) * aspect, vUv.y - 0.5);
    vec3 c = vec3(0.012, 0.012, 0.03);
    float t = time * 0.35;
    for (int i = 0; i < 7; i++) {
        float fi = float(i);
        float k = 1.2 + fi * 0.55;
        float y = sin(p.x * k + t * (0.7 + fi * 0.13) + fi * 1.7) * (0.06 + 0.16 * bass)
                + sin(p.x * k * 2.3 - t * 0.9 + fi) * (0.02 + 0.07 * mid)
                + (fi - 3.0) * 0.07;
        float d = abs(p.y - y);
        c += palette(fi * 0.12 + time * 0.012) * 0.006 / (d + 0.008);
    }
    fragColor = vec4(c * (0.5 + 0.5 * energy), 1.0);
}
"#;

/// Holds back how fast each point of the picture changes. A flash is a pair of
/// opposing changes of at least a tenth of the maximum brightness; with
/// [`LIMIT_PER_SECOND`] each of those takes at least 0.2 s, so at most 2.5
/// flashes fit in a second (WCAG 2.3.1 allows three). Saturated red, which
/// provokes seizures most easily, is held back as well. 8 bits alone would stop
/// slow changes when they round to nothing, so rounding is left to chance.
pub(crate) const LIMIT_FS: &str = r#"
uniform sampler2D tCur;
uniform sampler2D tPrev;
uniform float maxStep;
uniform float seed;
in vec2 vUv;
out vec4 fragColor;
vec3 toLinear(vec3 c) { return pow(max(c, vec3(0.0)), vec3(2.2)); }
vec3 toDisplay(vec3 c) { return pow(max(c, vec3(0.0)), vec3(1.0 / 2.2)); }
float hash(vec2 p) { return fract(sin(dot(p, vec2(12.9898, 78.233)) + seed) * 43758.5453); }
void main() {
    vec3 cur = texture(tCur, vUv).rgb;
    float sum = cur.r + cur.g + cur.b;
    float redness = sum > 0.001 ? cur.r / sum : 0.0;
    cur.r *= mix(1.0, 0.7, smoothstep(0.6, 0.9, redness));
    vec3 prev = toLinear(texture(tPrev, vUv).rgb);
    vec3 change = clamp(toLinear(cur) - prev, vec3(-maxStep), vec3(maxStep));
    vec3 next = toDisplay(prev + change);
    fragColor = vec4(next + (hash(gl_FragCoord.xy) - 0.5) / 255.0, 1.0);
}
"#;

/// The largest change of linear brightness per second in photosensitive mode.
pub const LIMIT_PER_SECOND: f32 = 0.5;

#[cfg(test)]
mod tests {
    use super::*;

    /// One step of the limiter's arithmetic, as in [`LIMIT_FS`] (linear light).
    fn step(prev: f32, cur: f32, dt: f32) -> f32 {
        prev + (cur - prev).clamp(-LIMIT_PER_SECOND * dt, LIMIT_PER_SECOND * dt)
    }

    /// Flashes per second after WCAG 2.3.1: pairs of opposing changes of at
    /// least 10 % of the maximum brightness, the darker image below 0.8.
    fn flashes(values: &[f32], seconds: f32) -> f32 {
        let (mut changes, mut low, mut high) = (0u32, values[0], values[0]);
        let mut rising: Option<bool> = None;
        for v in values {
            low = low.min(*v);
            high = high.max(*v);
            if rising != Some(true) && v - low >= 0.1 && low < 0.8 {
                changes += 1;
                rising = Some(true);
                high = *v;
            } else if rising != Some(false) && high - v >= 0.1 && *v < 0.8 {
                changes += 1;
                rising = Some(false);
                low = *v;
            }
        }
        changes as f32 / 2.0 / seconds
    }

    fn square(hz: f32, seconds: f32, fps: f32) -> Vec<f32> {
        (0..(seconds * fps) as usize)
            .map(|i| {
                if (i as f32 / fps * hz * 2.0) as u32 % 2 == 0 {
                    0.0
                } else {
                    1.0
                }
            })
            .collect()
    }

    fn limited(input: &[f32], fps: f32) -> Vec<f32> {
        let mut prev = input[0];
        input
            .iter()
            .map(|v| {
                prev = step(prev, *v, 1.0 / fps);
                prev
            })
            .collect()
    }

    #[test]
    fn strobing_stays_below_three_flashes_a_second() {
        for hz in [1.0, 2.0, 3.0, 5.0, 8.0, 10.0, 15.0, 30.0] {
            for fps in [30.0, 60.0, 144.0] {
                let input = square(hz, 20.0, fps);
                let out = limited(&input, fps);
                let f = flashes(&out, 20.0);
                assert!(f <= 3.0, "{hz} Hz at {fps} fps: {f} flashes per second");
            }
        }
        // And the measure sees the strobe without the limiter.
        assert!(flashes(&square(10.0, 20.0, 60.0), 20.0) > 9.0);
    }

    #[test]
    fn slow_changes_pass() {
        // A fade over 4 seconds is slower than the limit.
        let fps = 60.0;
        let input: Vec<f32> = (0..240).map(|i| i as f32 / 240.0).collect();
        let out = limited(&input, fps);
        assert!((out[239] - input[239]).abs() < 0.01);
    }

    #[test]
    fn all_shaders_are_there() {
        for k in SceneKind::ALL {
            let s = k.shader();
            assert!(s.contains("void main()") && s.contains("fragColor"));
        }
        assert_eq!(SceneKind::parse("aurora"), Some(SceneKind::Aurora));
        assert_eq!(SceneKind::parse("none"), None);
    }
}
