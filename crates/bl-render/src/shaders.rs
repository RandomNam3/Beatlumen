//! GLSL ES 3.00 shader sources (the `#version` header is prepended by
//! `bl_gl::Gl::shader_header`).
//!
//! Several shaders are ports of shaders from:
//! * midori (https://github.com/aeroheim/midori) — MIT License,
//!   Copyright (c) 2020 Benjamin Pang: gaussian blur, vignette blur blend,
//!   glitch, wipe/slide/blur transitions, particles.
//! * three.js (https://github.com/mrdoob/three.js) — MIT License,
//!   Copyright 2010-2021 three.js authors: copy, blend, RGB shift, vignette.
//! * webgl-noise (https://github.com/stegu/webgl-noise) — MIT License,
//!   Copyright (C) 2011 Ashima Arts: 2D simplex noise.
//!
//! See `THIRD_PARTY_NOTICES.md` in the repository root for the full texts.

pub const COPY_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float opacity;
in vec2 vUv;
out vec4 fragColor;
void main() {
    fragColor = texture(tDiffuse, vUv) * opacity;
}
"#;

pub const BLEND_FS: &str = r#"
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform float opacity;
uniform float mixRatio;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 texel1 = texture(tDiffuse1, vUv);
    vec4 texel2 = texture(tDiffuse2, vUv);
    fragColor = opacity * mix(texel1, texel2, mixRatio);
}
"#;

/// 33-tap separable gaussian using bilinear filtering (17 fetches).
pub const BLUR_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float radius;
uniform float resolution;
uniform vec2 direction;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 d = direction * (radius / resolution);
    vec4 sum = vec4(0.0);
    sum += texture(tDiffuse, vUv - 15.0810810809 * d) * 1.13068382e-7;
    sum += texture(tDiffuse, vUv - 13.1351352551 * d) * 0.00000634313;
    sum += texture(tDiffuse, vUv - 11.1891891693 * d) * 0.00014981883;
    sum += texture(tDiffuse, vUv - 9.2432432422 * d) * 0.00181031093;
    sum += texture(tDiffuse, vUv - 7.29729729717 * d) * 0.01244177332;
    sum += texture(tDiffuse, vUv - 5.35135135135 * d) * 0.0518407222;
    sum += texture(tDiffuse, vUv - 3.40540540538 * d) * 0.13626704123;
    sum += texture(tDiffuse, vUv - 1.45945945945 * d) * 0.23145357738;
    sum += texture(tDiffuse, vUv) * 0.13206059971;
    sum += texture(tDiffuse, vUv + 1.45945945945 * d) * 0.23145357738;
    sum += texture(tDiffuse, vUv + 3.40540540538 * d) * 0.13626704123;
    sum += texture(tDiffuse, vUv + 5.35135135135 * d) * 0.0518407222;
    sum += texture(tDiffuse, vUv + 7.29729729717 * d) * 0.01244177332;
    sum += texture(tDiffuse, vUv + 9.2432432422 * d) * 0.00181031093;
    sum += texture(tDiffuse, vUv + 11.1891891693 * d) * 0.00014981883;
    sum += texture(tDiffuse, vUv + 13.1351352551 * d) * 0.00000634313;
    sum += texture(tDiffuse, vUv + 15.0810810809 * d) * 1.13068382e-7;
    fragColor = sum;
}
"#;

pub const RGB_SHIFT_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float amount;
uniform float angle;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 offset = amount * vec2(cos(angle), sin(angle));
    vec4 cr = texture(tDiffuse, vUv + offset);
    vec4 cga = texture(tDiffuse, vUv);
    vec4 cb = texture(tDiffuse, vUv - offset);
    fragColor = vec4(cr.r, cga.g, cb.b, cga.a);
}
"#;

pub const VIGNETTE_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float offset;
uniform float darkness;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 texel = texture(tDiffuse, vUv);
    vec2 uv = (vUv - vec2(0.5)) * vec2(offset);
    fragColor = vec4(mix(texel.rgb, vec3(1.0 - darkness), dot(uv, uv)), texel.a);
}
"#;

/// Blends the sharp image (tDiffuse1) with its blurred version (tDiffuse2)
/// towards the edges.
pub const VIGNETTE_BLEND_FS: &str = r#"
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform float size;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 uv = vUv - vec2(0.5);
    float mixRatio = smoothstep(0.0, 1.0, min(dot(uv, uv) * size, 1.0));
    fragColor = mix(texture(tDiffuse1, vUv), texture(tDiffuse2, vUv), mixRatio);
}
"#;

/// Camera motion blur for the background plane (z = 0). The world position
/// of each pixel is reconstructed by intersecting its view ray with the
/// plane, then reprojected with the previous frame's view-projection matrix.
pub const MOTION_BLUR_FS: &str = r#"
const int MAX_SAMPLES = 64;
uniform sampler2D tDiffuse;
uniform mat4 invViewProj;
uniform mat4 prevViewProj;
uniform float intensity;
uniform int samples;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 ndc = vUv * 2.0 - 1.0;
    vec4 n = invViewProj * vec4(ndc, -1.0, 1.0);
    vec4 f = invViewProj * vec4(ndc, 1.0, 1.0);
    vec3 a = n.xyz / n.w;
    vec3 b = f.xyz / f.w;
    vec3 dir = b - a;
    vec2 velocity = vec2(0.0);
    if (abs(dir.z) > 1e-6) {
        float t = -a.z / dir.z;
        vec3 world = a + dir * t;
        vec4 prev = prevViewProj * vec4(world, 1.0);
        if (prev.w > 1e-6) {
            vec2 prevUv = prev.xy / prev.w * 0.5 + 0.5;
            velocity = (vUv - prevUv) * intensity;
        }
    }
    vec4 texel = texture(tDiffuse, vUv);
    for (int i = 1; i < MAX_SAMPLES; ++i) {
        if (i >= samples) {
            break;
        }
        vec2 o = velocity * (float(i) / float(samples - 1) - 0.5);
        texel += texture(tDiffuse, vUv + o);
    }
    fragColor = texel / max(1.0, float(samples));
}
"#;

const SIMPLEX_NOISE: &str = r#"
vec3 mod289(vec3 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
vec2 mod289(vec2 x) { return x - floor(x * (1.0 / 289.0)) * 289.0; }
vec3 permute(vec3 x) { return mod289(((x * 34.0) + 1.0) * x); }
float snoise(vec2 v) {
    const vec4 C = vec4(0.211324865405187, 0.366025403784439, -0.577350269189626, 0.024390243902439);
    vec2 i = floor(v + dot(v, C.yy));
    vec2 x0 = v - i + dot(i, C.xx);
    vec2 i1 = (x0.x > x0.y) ? vec2(1.0, 0.0) : vec2(0.0, 1.0);
    vec4 x12 = x0.xyxy + C.xxzz;
    x12.xy -= i1;
    i = mod289(i);
    vec3 p = permute(permute(i.y + vec3(0.0, i1.y, 1.0)) + i.x + vec3(0.0, i1.x, 1.0));
    vec3 m = max(0.5 - vec3(dot(x0, x0), dot(x12.xy, x12.xy), dot(x12.zw, x12.zw)), 0.0);
    m = m * m;
    m = m * m;
    vec3 x = 2.0 * fract(p * C.www) - 1.0;
    vec3 h = abs(x) - 0.5;
    vec3 ox = floor(x + 0.5);
    vec3 a0 = x - ox;
    m *= 1.79284291400159 - 0.85373472095314 * (a0 * a0 + h * h);
    vec3 g;
    g.x = a0.x * x0.x + h.x * x0.y;
    g.yz = a0.yz * x12.xz + h.yz * x12.yw;
    return 130.0 * dot(m, g);
}
"#;

const GLITCH_BODY: &str = r#"
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform float amount;
uniform float seed;
uniform vec2 resolution;
in vec2 vUv;
out vec4 fragColor;

vec2 tile(vec2 position, vec2 res, float size, float scale) {
    vec2 tileSize = vec2(size / res.x * scale, size / res.y);
    return tileSize * floor(position / tileSize);
}

float glitchNoise(vec2 position, vec2 res, float amount, float seed) {
    float noise = 0.0;
    noise += max(snoise(tile(position, res, 488.0, 15.0) * (1.0 + amount * seed * 8.0)) * amount - 0.5, 0.0);
    noise += max(snoise(tile(position, res, 100.0, 1.0) * (4.0 + amount * seed * 2.0)) * amount - 0.3, 0.0);
    noise += max(snoise(tile(position, res, 120.0, 8.0) * (4.0 + amount * seed * 4.0)) * amount - 0.2, 0.0);
    noise += max(snoise(tile(position, res, 125.0, 8.0) * (4.0 + amount * seed * 4.0)) * amount - 0.2, 0.0);
    noise += max(snoise(tile(position, res, 29.0, 16.0) * (4.0 + amount * seed * 2.0)) * amount - 0.2, 0.0);
    noise += max(snoise(tile(position, res, 29.0, 1.0) * (8.0 + amount * seed * 2.0)) * amount - 0.7, 0.0);
    if (noise >= 0.6) {
        noise += max(snoise(tile(position, res, 1.1, 1000.0) * 1000.0) * amount, 0.0);
    } else if (noise <= 0.0) {
        float lineNoise = max(snoise(tile(position, res, 1.1, 500.0) * (500.0 + amount * seed * 100.0)) * amount, 0.0);
        lineNoise += min(snoise(tile(position, res, 100.0, 3.0) * (4.0 + amount * seed * 2.0)) * amount, 0.0);
        noise += max(lineNoise, 0.0);
    }
    float glitchCoerceThreshold = 0.9;
    if (amount >= glitchCoerceThreshold) {
        float percent = (amount - glitchCoerceThreshold) / (1.0 - glitchCoerceThreshold);
        return noise + percent;
    }
    return noise;
}

vec4 rgbShift(sampler2D tex, vec2 position, vec3 offset) {
    vec4 r = texture(tex, position + vec2(offset.r, 0.0));
    vec4 g = texture(tex, position + vec2(offset.g, 0.0));
    vec4 b = texture(tex, position + vec2(offset.b, 0.0));
    return vec4(r.r, g.g, b.b, 1.0);
}

void main() {
    float glitch = glitchNoise(vUv, resolution, amount, seed);
    vec3 shift = vec3(0.01, 0.0, -0.01);
    vec4 texel1 = texture(tDiffuse1, vUv);
    vec4 shifted1 = rgbShift(tDiffuse1, vUv, shift);
    vec4 texel2 = texture(tDiffuse2, vUv);
    vec4 shifted2 = rgbShift(tDiffuse2, vUv, shift);
    vec4 color = texel1;
    if (glitch >= 0.95) {
        color = texel2;
    } else if (glitch >= 0.7) {
        color = shifted2;
    } else if (glitch >= 0.6) {
        color = shifted1;
    } else if (glitch >= 0.5) {
        color = texel1 * vec4(1.2, 0.0, 1.2, 0.5);
    } else if (glitch >= 0.4) {
        color = texel1 * vec4(0.0, 1.2, 1.2, 0.5);
    } else if (glitch >= 0.38) {
        color = shifted2 * 1.5;
    } else if (glitch >= 0.2) {
        color = shifted1;
    }
    fragColor = color;
}
"#;

pub fn glitch_fs() -> String {
    format!("{SIMPLEX_NOISE}{GLITCH_BODY}")
}

pub const WIPE_FS: &str = r#"
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform float amount;
uniform float gradient;
uniform int direction;
uniform float angle;
uniform float aspect;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 texel1 = texture(tDiffuse1, vUv);
    vec4 texel2 = texture(tDiffuse2, vUv);
    float position;
    if (direction == 0) {
        position = 1.0 - vUv.x;
    } else if (direction == 1) {
        position = vUv.x;
    } else if (direction == 2) {
        position = vUv.y;
    } else {
        position = 1.0 - vUv.y;
    }
    float rotationOffset;
    float rotatedPosition;
    if (direction < 2) {
        float slope = 1.0 / tan(angle);
        rotationOffset = (1.0 / slope) / aspect;
        rotatedPosition = (vUv.y / slope) / aspect;
    } else {
        float slope = tan(angle);
        rotationOffset = slope / aspect;
        rotatedPosition = (vUv.x * slope) / aspect;
    }
    float wipeOffset = (-max(0.0, rotationOffset) - gradient) + ((1.0 + abs(rotationOffset) + gradient) * amount) + rotatedPosition;
    if (position <= wipeOffset) {
        fragColor = texel2;
    } else if (position <= wipeOffset + gradient) {
        fragColor = mix(texel2, texel1, (position - wipeOffset) / gradient);
    } else {
        fragColor = texel1;
    }
}
"#;

pub const SLIDE_FS: &str = r#"
const int MAX_SAMPLES = 64;
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform int slides;
uniform float amount;
uniform float prevAmount;
uniform float intensity;
uniform int direction;
uniform int samples;
in vec2 vUv;
out vec4 fragColor;

float component(int dir, vec2 uv) { return dir < 2 ? uv.x : uv.y; }
vec2 withComponent(int dir, vec2 uv, float p) { return dir < 2 ? vec2(p, uv.y) : vec2(uv.x, p); }
float offsetPosition(int dir, float uv, float offset) {
    return (dir == 1 || dir == 3) ? mod(uv + offset, 1.0) : mod(uv + (1.0 - offset), 1.0);
}
vec4 sampleAt(float position, float offset) {
    bool first = (direction == 1 || direction == 3) ? position + offset <= 1.0 : position - offset >= 0.0;
    vec2 uv = withComponent(direction, vUv, offsetPosition(direction, position, offset));
    return first ? texture(tDiffuse1, uv) : texture(tDiffuse2, uv);
}
void main() {
    float offset = amount * float(slides);
    float position = component(direction, vUv);
    vec4 texel = sampleAt(position, offset);
    float velocity = (amount - prevAmount) * intensity;
    for (int i = 1; i < MAX_SAMPLES; ++i) {
        if (i >= samples) {
            break;
        }
        float blurOffset = velocity * (float(i) / float(samples - 1) - 0.5);
        texel += sampleAt(position, offset + blurOffset);
    }
    fragColor = texel / max(1.0, float(samples));
}
"#;

pub const BLUR_TRANSITION_FS: &str = r#"
const int MAX_SAMPLES = 64;
uniform sampler2D tDiffuse1;
uniform sampler2D tDiffuse2;
uniform float amount;
uniform float prevAmount;
uniform float intensity;
uniform int samples;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 texel = mix(texture(tDiffuse1, vUv), texture(tDiffuse2, vUv), amount);
    float velocity = (amount - prevAmount) * intensity;
    for (int i = 1; i < MAX_SAMPLES; ++i) {
        if (i >= samples) {
            break;
        }
        float o = velocity * (float(i) / float(samples - 1) - 0.5);
        vec2 uv = vec2(vUv.x + o, vUv.y);
        texel += mix(texture(tDiffuse1, uv), texture(tDiffuse2, uv), amount);
    }
    fragColor = texel / max(1.0, float(samples));
}
"#;

/// Background image plane.
pub const PLANE_VS: &str = r#"
layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;
uniform mat4 viewProj;
uniform vec2 halfSize;
out vec2 vUv;
void main() {
    vUv = uv;
    gl_Position = viewProj * vec4(position * halfSize, 0.0, 1.0);
}
"#;

pub const PLANE_FS: &str = r#"
uniform sampler2D map;
uniform vec3 fallback;
uniform int hasMap;
in vec2 vUv;
out vec4 fragColor;
void main() {
    fragColor = hasMap == 1 ? vec4(texture(map, vUv).rgb, 1.0) : vec4(fallback, 1.0);
}
"#;

/// Particles as instanced, pixel-sized quads.
pub const PARTICLE_VS: &str = r#"
layout(location = 0) in vec2 corner;
layout(location = 1) in vec3 position;
layout(location = 2) in float size;
layout(location = 3) in float gradient;
layout(location = 4) in float opacity;
layout(location = 5) in vec3 color;
uniform mat4 viewProj;
uniform vec2 viewport;
uniform float pixelScale;
out vec2 vCoord;
out float vGradient;
out float vOpacity;
out vec3 vColor;
void main() {
    vec4 clip = viewProj * vec4(position, 1.0);
    vCoord = corner + 0.5;
    vGradient = gradient;
    vOpacity = opacity;
    vColor = color;
    if (clip.w <= 1e-4 || size <= 0.0) {
        gl_Position = vec4(2.0, 2.0, 2.0, 1.0);
        return;
    }
    clip.xy += corner * size * pixelScale * 2.0 / viewport * clip.w;
    gl_Position = clip;
}
"#;

pub const PARTICLE_FS: &str = r#"
in vec2 vCoord;
in float vGradient;
in float vOpacity;
in vec3 vColor;
out vec4 fragColor;
void main() {
    float radius = 0.5;
    float d = distance(vCoord, vec2(0.5));
    if (d > radius) {
        discard;
    }
    float g = max(smoothstep(0.0, 1.0, vGradient * radius), 1e-5);
    fragColor = vec4(vColor, min((radius - d) / g, 1.0) * vOpacity);
}
"#;

/// 2D UI batch shader: rounded rectangles (SDF), glyphs (R8 atlas),
/// images and soft shadows.
pub const UI_VS: &str = r#"
layout(location = 0) in vec2 position;
layout(location = 1) in vec2 uv;
layout(location = 2) in vec4 color;
layout(location = 3) in vec2 local;
layout(location = 4) in vec4 shape;
uniform vec2 screen;
out vec2 vUv;
out vec4 vColor;
out vec2 vLocal;
out vec4 vShape;
void main() {
    vUv = uv;
    vColor = color;
    vLocal = local;
    vShape = shape;
    gl_Position = vec4(position.x / screen.x * 2.0 - 1.0, 1.0 - position.y / screen.y * 2.0, 0.0, 1.0);
}
"#;

/// `shape` = (half width, half height, corner radius, mode) where mode is
/// 0 solid, 1 glyph, 2 image, 3 shadow (radius = blur), 4 outline (uv.x = width).
pub const UI_FS: &str = r#"
uniform sampler2D atlas;
uniform sampler2D image;
uniform float pixelScale;
in vec2 vUv;
in vec4 vColor;
in vec2 vLocal;
in vec4 vShape;
out vec4 fragColor;
float sdRoundBox(vec2 p, vec2 b, float r) {
    vec2 q = abs(p) - b + r;
    return min(max(q.x, q.y), 0.0) + length(max(q, 0.0)) - r;
}
void main() {
    int mode = int(vShape.w + 0.5);
    vec4 c = vColor;
    if (mode == 1) {
        c.a *= texture(atlas, vUv).r;
    } else if (mode == 3) {
        float blur = max(vShape.z, 1.0);
        float d = sdRoundBox(vLocal, vShape.xy - vec2(blur), blur);
        c.a *= 1.0 - smoothstep(-blur, blur, d);
    } else {
        if (mode == 2) {
            c *= texture(image, vUv);
        }
        float d = sdRoundBox(vLocal, vShape.xy, vShape.z);
        if (mode == 4) {
            d = abs(d + vUv.x * 0.5) - vUv.x * 0.5;
        }
        c.a *= clamp(0.5 - d * pixelScale, 0.0, 1.0);
    }
    fragColor = vec4(c.rgb * c.a, c.a);
}
"#;

/// Hue rotation (YIQ space).
pub const HUE_SHIFT_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float angle;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 c = texture(tDiffuse, vUv);
    mat3 toYiq = mat3(0.299, 0.596, 0.211, 0.587, -0.274, -0.523, 0.114, -0.322, 0.312);
    mat3 toRgb = mat3(1.0, 1.0, 1.0, 0.956, -0.272, -1.106, 0.621, -0.647, 1.703);
    vec3 yiq = toYiq * c.rgb;
    float h = atan(yiq.z, yiq.y) + angle;
    float chroma = length(yiq.yz);
    yiq.yz = chroma * vec2(cos(h), sin(h));
    fragColor = vec4(clamp(toRgb * yiq, 0.0, 1.0), c.a);
}
"#;

/// Mirrors the image into `segments` wedges around the centre.
pub const KALEIDOSCOPE_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float amount;
uniform float segments;
uniform float angle;
uniform vec2 resolution;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 p = vUv - 0.5;
    float aspect = resolution.x / max(resolution.y, 1.0);
    p.x *= aspect;
    float r = length(p);
    float a = atan(p.y, p.x) + angle;
    float wedge = 6.28318530718 / segments;
    a = mod(a, wedge);
    a = abs(a - wedge * 0.5);
    vec2 q = r * vec2(cos(a), sin(a));
    q.x /= aspect;
    vec2 uv = q + 0.5;
    vec4 k = texture(tDiffuse, clamp(uv, 0.0, 1.0));
    fragColor = mix(texture(tDiffuse, vUv), k, amount);
}
"#;

/// Additive flash towards a colour (screen blend).
pub const FLASH_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float amount;
uniform vec3 color;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec4 c = texture(tDiffuse, vUv);
    vec3 screen = 1.0 - (1.0 - c.rgb) * (1.0 - color * amount);
    fragColor = vec4(screen, c.a);
}
"#;

/// Radial blur towards the centre.
pub const ZOOM_BLUR_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float strength;
in vec2 vUv;
out vec4 fragColor;
void main() {
    vec2 dir = vUv - 0.5;
    vec4 acc = vec4(0.0);
    const int N = 16;
    for (int i = 0; i < N; i++) {
        float t = float(i) / float(N - 1);
        acc += texture(tDiffuse, 0.5 + dir * (1.0 - strength * t));
    }
    fragColor = acc / float(N);
}
"#;

/// Animated film grain and scanlines.
pub const GRAIN_FS: &str = r#"
uniform sampler2D tDiffuse;
uniform float amount;
uniform float scanlines;
uniform float time;
uniform vec2 resolution;
in vec2 vUv;
out vec4 fragColor;
float hash(vec2 p) {
    p = fract(p * vec2(443.897, 441.423));
    p += dot(p, p.yx + 19.19);
    return fract((p.x + p.y) * p.x);
}
void main() {
    vec4 c = texture(tDiffuse, vUv);
    vec2 px = floor(vUv * resolution);
    float n = hash(px + fract(time * 7.13) * 251.0) - 0.5;
    vec3 col = c.rgb + n * amount;
    float line = 0.5 + 0.5 * sin(vUv.y * resolution.y * 3.14159);
    col *= 1.0 - scanlines * 0.35 * line;
    fragColor = vec4(clamp(col, 0.0, 1.0), c.a);
}
"#;
