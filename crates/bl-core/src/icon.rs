//! Procedurally generated application icon: glowing equalizer bars in a
//! rose → violet → cyan gradient on a dark rounded square.

fn smoothstep(e0: f32, e1: f32, x: f32) -> f32 {
    let t = ((x - e0) / (e1 - e0)).clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn mix(a: [f32; 3], b: [f32; 3], t: f32) -> [f32; 3] {
    [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
}

/// Gradient across the bars (0 = left, 1 = right).
fn gradient(t: f32) -> [f32; 3] {
    let rose = [1.0, 0.26, 0.55];
    let violet = [0.62, 0.32, 1.0];
    let cyan = [0.25, 0.85, 1.0];
    if t < 0.5 {
        mix(rose, violet, t * 2.0)
    } else {
        mix(violet, cyan, (t - 0.5) * 2.0)
    }
}

/// Signed distance to a rounded box centred at (cx, cy) with half size (hx, hy).
fn rbox(x: f32, y: f32, cx: f32, cy: f32, hx: f32, hy: f32, r: f32) -> f32 {
    let qx = (x - cx).abs() - hx + r;
    let qy = (y - cy).abs() - hy + r;
    (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt() + qx.max(qy).min(0.0) - r
}

/// RGBA pixels (size × size). `full_bleed` fills the whole square (Android
/// adaptive icon foreground uses `false` with a transparent background).
pub fn icon_rgba(size: u32, background: bool) -> Vec<u8> {
    const BARS: [f32; 5] = [0.38, 0.62, 0.86, 0.56, 0.32];
    let mut out = vec![0u8; (size * size * 4) as usize];
    let ss = 3; // supersampling per axis
    for py in 0..size {
        for px in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..ss {
                for sx in 0..ss {
                    let x = (px as f32 + (sx as f32 + 0.5) / ss as f32) / size as f32;
                    let y = (py as f32 + (sy as f32 + 0.5) / ss as f32) / size as f32;
                    let mut col = [0.0f32; 3];
                    let mut alpha = 0.0f32;
                    if background {
                        let d = rbox(x, y, 0.5, 0.5, 0.5, 0.5, 0.22);
                        let a = smoothstep(0.004, -0.004, d);
                        let bg = mix([0.10, 0.06, 0.16], [0.03, 0.03, 0.07], y);
                        col = bg;
                        alpha = a;
                    }
                    // Bars and glow.
                    let mut glow = [0.0f32; 3];
                    for (i, h) in BARS.iter().enumerate() {
                        let cx = 0.25 + i as f32 * 0.125;
                        let c = gradient(i as f32 / 4.0);
                        let d = rbox(x, y, cx, 0.5, 0.042, h * 0.33, 0.042);
                        let bar = smoothstep(0.004, -0.004, d);
                        let g = (-d.max(0.0) * 18.0).exp() * 0.55;
                        for k in 0..3 {
                            glow[k] += c[k] * g;
                            col[k] = col[k] * (1.0 - bar) + c[k] * bar;
                        }
                        alpha = alpha.max(bar);
                    }
                    for k in 0..3 {
                        col[k] = (col[k] + glow[k] * if background { 1.0 } else { 0.0 }).min(1.0);
                    }
                    acc[0] += col[0] * alpha;
                    acc[1] += col[1] * alpha;
                    acc[2] += col[2] * alpha;
                    acc[3] += alpha;
                }
            }
            let n = (ss * ss) as f32;
            let a = acc[3] / n;
            let i = ((py * size + px) * 4) as usize;
            if a > 0.0 {
                out[i] = ((acc[0] / acc[3]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                out[i + 1] = ((acc[1] / acc[3]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
                out[i + 2] = ((acc[2] / acc[3]).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
            out[i + 3] = (a * 255.0 + 0.5) as u8;
        }
    }
    out
}
