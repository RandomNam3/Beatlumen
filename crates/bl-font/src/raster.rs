//! Exact-area anti-aliasing rasterizer (signed area / cover accumulation).
//!
//! Each line segment deposits, for every pixel row it crosses, the signed
//! area it covers to its right into an accumulation buffer; a running prefix
//! sum along each row then yields the exact coverage of every pixel. The
//! non-zero winding rule is approximated by taking the absolute value of the
//! accumulated winding and clamping it to 1.
//!
//! The line drawing routine is a port of the accumulation rasterizer of
//! font-rs (<https://github.com/raphlinus/font-rs>):
//!
//! ```text
//! Copyright 2015 Google Inc. All rights reserved.
//!
//! Licensed under the Apache License, Version 2.0 (the "License");
//! you may not use this file except in compliance with the License.
//! You may obtain a copy of the License at
//!
//!     http://www.apache.org/licenses/LICENSE-2.0
//!
//! Unless required by applicable law or agreed to in writing, software
//! distributed under the License is distributed on an "AS IS" BASIS,
//! WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
//! See the License for the specific language governing permissions and
//! limitations under the License.
//! ```
//!
//! Changes: per-row accumulation with a padded row stride, coordinate
//! clamping so that no input can index out of bounds, and integration with
//! this crate's flattener.

use crate::outline::Line;

/// Accumulation buffer for a `width` x `height` bitmap.
pub(crate) struct Raster {
    w: usize,
    h: usize,
    stride: usize,
    acc: Vec<f32>,
}

impl Raster {
    pub fn new(w: usize, h: usize) -> Raster {
        let stride = w + 2;
        Raster { w, h, stride, acc: vec![0.0; stride * (h + 1)] }
    }

    /// Draws a line in bitmap coordinates (pixel (0,0) is the top-left
    /// pixel). Coordinates are clamped to the bitmap rectangle.
    pub fn draw_line(&mut self, l: &Line) {
        let wf = self.w as f32;
        let hf = self.h as f32;
        // `max`/`min` also turn NaN into a finite value.
        let cx = |v: f32| v.max(0.0).min(wf);
        let cy = |v: f32| v.max(0.0).min(hf);
        let (x0, y0, x1, y1) = (cx(l.x0), cy(l.y0), cx(l.x1), cy(l.y1));
        if (y0 - y1).abs() <= f32::EPSILON {
            return;
        }
        let (dir, x0, y0, x1, y1) = if y0 < y1 { (1.0, x0, y0, x1, y1) } else { (-1.0, x1, y1, x0, y0) };
        let dxdy = (x1 - x0) / (y1 - y0);
        let mut x = x0;
        let ystart = y0 as usize;
        let yend = (y1.ceil() as usize).min(self.h);
        let stride = self.stride;
        for y in ystart..yend {
            let row = y * stride;
            let line = &mut self.acc[row..row + stride];
            let dy = ((y + 1) as f32).min(y1) - (y as f32).max(y0);
            let xnext = (x + dxdy * dy).max(0.0).min(wf);
            let d = dy * dir;
            let (xa, xb) = if x < xnext { (x, xnext) } else { (xnext, x) };
            let xa_floor = xa.floor();
            let xai = xa_floor as usize;
            let xb_ceil = xb.ceil();
            let xbi = xb_ceil as usize;
            if xbi <= xai + 1 {
                // Within a single pixel column.
                let xmf = 0.5 * (x + xnext) - xa_floor;
                line[xai] += d - d * xmf;
                line[xai + 1] += d * xmf;
            } else {
                let s = (xb - xa).recip();
                let xaf = xa - xa_floor;
                let a0 = 0.5 * s * (1.0 - xaf) * (1.0 - xaf);
                let xbf = xb - xb_ceil + 1.0;
                let am = 0.5 * s * xbf * xbf;
                line[xai] += d * a0;
                if xbi == xai + 2 {
                    line[xai + 1] += d * (1.0 - a0 - am);
                } else {
                    let a1 = s * (1.5 - xaf);
                    line[xai + 1] += d * (a1 - a0);
                    let ds = d * s;
                    for v in &mut line[xai + 2..xbi - 1] {
                        *v += ds;
                    }
                    let a2 = a1 + (xbi - xai - 3) as f32 * s;
                    line[xbi - 1] += d * (1.0 - a2 - am);
                }
                line[xbi] += d * am;
            }
            x = xnext;
        }
    }

    /// Integrates the accumulation buffer into 8-bit coverage values.
    pub fn coverage(&self) -> Vec<u8> {
        let mut out = vec![0u8; self.w * self.h];
        if self.w == 0 {
            return out;
        }
        for (src, dst) in self.acc.chunks_exact(self.stride).zip(out.chunks_exact_mut(self.w)) {
            let mut acc = 0.0f32;
            for (s, d) in src.iter().zip(dst.iter_mut()) {
                acc += *s;
                let c = acc.abs().min(1.0);
                *d = (c * 255.0 + 0.5) as u8;
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn poly(r: &mut Raster, pts: &[(f32, f32)]) {
        for i in 0..pts.len() {
            let a = pts[i];
            let b = pts[(i + 1) % pts.len()];
            r.draw_line(&Line { x0: a.0, y0: a.1, x1: b.0, y1: b.1 });
        }
    }

    fn total(c: &[u8]) -> f32 {
        c.iter().map(|&v| v as f32 / 255.0).sum()
    }

    #[test]
    fn square_area() {
        let mut r = Raster::new(12, 8);
        poly(&mut r, &[(0.3, 0.3), (10.7, 0.3), (10.7, 5.2), (0.3, 5.2)]);
        let c = r.coverage();
        let area = 10.4 * 4.9;
        assert!((total(&c) - area).abs() < 0.05, "{} vs {}", total(&c), area);
        // interior pixel fully covered, outside empty
        assert_eq!(c[2 * 12 + 5], 255);
        assert_eq!(c[7 * 12 + 11], 0);
        // partial edge pixel: 0.7 coverage
        assert!((c[2 * 12] as f32 / 255.0 - 0.7).abs() < 0.01);
    }

    #[test]
    fn triangle_area_both_orientations() {
        for pts in [[(1.0, 1.0), (9.5, 2.0), (4.0, 7.5)], [(4.0, 7.5), (9.5, 2.0), (1.0, 1.0)]] {
            let mut r = Raster::new(10, 8);
            poly(&mut r, &pts);
            let c = r.coverage();
            let (a, b, cc) = (pts[0], pts[1], pts[2]);
            let area = ((b.0 - a.0) * (cc.1 - a.1) - (cc.0 - a.0) * (b.1 - a.1)).abs() * 0.5;
            assert!((total(&c) - area).abs() < 0.05, "{} vs {}", total(&c), area);
        }
    }

    #[test]
    fn out_of_range_and_nan_do_not_panic() {
        let mut r = Raster::new(4, 4);
        poly(&mut r, &[(-10.0, -3.0), (100.0, 2.0), (f32::NAN, 9.0), (2.0, f32::INFINITY)]);
        poly(&mut r, &[(4.0, 0.0), (4.0, 4.0), (0.0, 4.0)]);
        let _ = r.coverage();
        let r = Raster::new(0, 0);
        assert!(r.coverage().is_empty());
    }
}
