//! Streaming area-averaging downscaler and target sizes.
//!
//! Decoders push the rows of an image (straight RGBA, top to bottom) into a
//! [`Scaler`], which accumulates them directly into the target grid: a
//! large image is never held at full size, only one horizontally scaled row
//! and the few target rows it contributes to. A frame may cover only a
//! rectangle of its canvas (animations): its target pixels then carry the
//! exact fraction of each pixel the rectangle covers, and the rows are
//! composited onto a premultiplied [`Canvas`] ("over" or "replace").
//!
//! Filtering happens on premultiplied alpha in fixed point: colours are
//! `c * a` (0..=65025) and alpha `a * 255`, weights have 14 fraction bits.

use crate::Image;

const WBITS: u32 = 14;
const ONE: u32 = 1 << WBITS;
/// Premultiplied full scale (255 * 255).
pub(crate) const FULL: u32 = 65025;

/// Receives the rows of a decoded image or frame.
pub(crate) trait RowSink {
    /// Row `y` (0 = top) of straight RGBA, at least `width * 4` bytes.
    /// Rows arrive in increasing order; skipped rows stay empty.
    fn row(&mut self, y: usize, rgba: &[u8]);
}

/// How large a decoded image should be.
///
/// The image keeps its aspect ratio and is never enlarged. It is scaled
/// down as far as it still covers `cover_w` × `cover_h` (e.g. the screen),
/// but no side exceeds `max_side` (e.g. the GPU's texture limit) and the
/// area stays within `max_pixels`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fit {
    pub cover_w: u32,
    pub cover_h: u32,
    pub max_side: u32,
    pub max_pixels: u64,
}

impl Fit {
    /// Full size (limited to [`crate::MAX_PIXELS`]).
    pub const fn full() -> Fit {
        Fit {
            cover_w: u32::MAX,
            cover_h: u32::MAX,
            max_side: u32::MAX,
            max_pixels: crate::MAX_PIXELS,
        }
    }

    /// Neither side larger than `max_dim` (the behaviour of
    /// [`crate::decode_max`]).
    pub const fn within(max_dim: u32) -> Fit {
        Fit {
            cover_w: u32::MAX,
            cover_h: u32::MAX,
            max_side: max_dim,
            max_pixels: crate::MAX_PIXELS,
        }
    }

    /// Covers `w` × `h`, at most `max_side` per side and `max_pixels`.
    pub const fn cover(w: u32, h: u32, max_side: u32, max_pixels: u64) -> Fit {
        Fit {
            cover_w: w,
            cover_h: h,
            max_side,
            max_pixels,
        }
    }

    /// The target size for a `w` × `h` image (each side at least 1).
    pub fn target(&self, w: u32, h: u32) -> (u32, u32) {
        let (w, h) = (w.max(1), h.max(1));
        let (wf, hf) = (w as f64, h as f64);
        let mut s = (self.cover_w as f64 / wf)
            .max(self.cover_h as f64 / hf)
            .min(1.0);
        let max_side = self.max_side.max(1);
        s = s.min(max_side as f64 / wf.max(hf));
        let max_px = self.max_pixels.max(1).min(crate::MAX_PIXELS) as f64;
        if wf * hf * s * s > max_px {
            s = (max_px / (wf * hf)).sqrt();
        }
        if s >= 1.0 {
            return (w, h);
        }
        let limit = max_side.min(w.max(h));
        let tw = ((wf * s).round() as u32).clamp(1, limit.min(w));
        let th = ((hf * s).round() as u32).clamp(1, limit.min(h));
        // Rounding up must not break the pixel budget.
        if tw as u64 * th as u64 > max_px as u64 {
            return (
                (wf * s).floor().max(1.0) as u32,
                (hf * s).floor().max(1.0) as u32,
            );
        }
        (tw, th)
    }
}

/// Filter taps for one axis of a frame inside its canvas.
struct Axis {
    /// First target index the frame touches.
    t0: usize,
    /// Per target index (from `t0`): first source index, tap count, offset
    /// of the weights, coverage (`ONE` = fully covered).
    first: Vec<usize>,
    count: Vec<usize>,
    offs: Vec<usize>,
    cov: Vec<u32>,
    w: Vec<u32>,
    identity: bool,
}

impl Axis {
    /// `n_src` source pixels start at canvas position `off`; the canvas is
    /// `extent` units long (source pixels beyond it are cut off) and maps
    /// onto `n_target` target pixels.
    fn new(n_src: usize, off: f64, extent: f64, n_target: usize) -> Axis {
        let identity = off == 0.0 && extent == n_src as f64 && n_target == n_src;
        let mut a = Axis {
            t0: 0,
            first: Vec::new(),
            count: Vec::new(),
            offs: Vec::new(),
            cov: Vec::new(),
            w: Vec::new(),
            identity,
        };
        if n_src == 0 || n_target == 0 || extent <= 0.0 {
            return a;
        }
        let scale = extent / n_target as f64;
        let lo = off.max(0.0);
        let hi = (off + n_src as f64).min(extent);
        if hi <= lo {
            return a;
        }
        let t0 = ((lo / scale).floor() as usize).min(n_target - 1);
        let t1 = ((hi / scale).ceil() as usize).clamp(t0 + 1, n_target);
        a.t0 = t0;
        let mut fw: Vec<f64> = Vec::new();
        for t in t0..t1 {
            let (c0, c1) = (t as f64 * scale, ((t + 1) as f64 * scale).min(extent));
            let (s0, s1) = (c0.max(lo), c1.min(hi));
            fw.clear();
            let first = (((s0 - off).floor()).max(0.0) as usize).min(n_src - 1);
            let last = (((s1 - off).ceil()) as usize).clamp(first + 1, n_src);
            for i in first..last {
                let p0 = off + i as f64;
                let ov = s1.min(p0 + 1.0) - s0.max(p0);
                fw.push(ov.max(0.0) / scale);
            }
            let cover: f64 = fw.iter().sum::<f64>().min(1.0);
            // Interior cells must sum to exactly ONE, edge cells to their
            // coverage: quantize, then put the rounding error on the
            // largest weight.
            let target_sum = (cover * ONE as f64).round() as i64;
            let start = a.w.len();
            let total: f64 = fw.iter().sum();
            let mut sum = 0i64;
            for &x in &fw {
                let q = if total > 0.0 {
                    (x / total * target_sum as f64).round() as i64
                } else {
                    0
                };
                a.w.push(q as u32);
                sum += q;
            }
            if let Some((imax, _)) = fw
                .iter()
                .enumerate()
                .max_by(|x, y| x.1.partial_cmp(y.1).unwrap_or(std::cmp::Ordering::Equal))
            {
                let adj = a.w[start + imax] as i64 + (target_sum - sum);
                a.w[start + imax] = adj.max(0) as u32;
            }
            a.first.push(first);
            a.count.push(fw.len());
            a.offs.push(start);
            a.cov.push(target_sum.clamp(0, ONE as i64) as u32);
        }
        a
    }

    fn len(&self) -> usize {
        self.first.len()
    }

    #[inline]
    fn weights(&self, i: usize) -> &[u32] {
        &self.w[self.offs[i]..self.offs[i] + self.count[i]]
    }
}

/// How a frame's pixels combine with the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Blend {
    /// Alpha compositing (source over canvas).
    Over,
    /// The frame replaces the covered canvas area.
    Replace,
}

/// A premultiplied RGBA canvas at target size (animation compositing).
#[derive(Clone)]
pub(crate) struct Canvas {
    pub width: usize,
    pub height: usize,
    /// `[c * a, c * a, c * a, a * 255]` per pixel.
    pub px: Vec<u16>,
}

impl Canvas {
    pub fn new(width: usize, height: usize) -> Canvas {
        Canvas {
            width,
            height,
            px: vec![0; width * height * 4],
        }
    }

    /// Straight 8-bit RGBA copy of the canvas.
    pub fn to_image(&self) -> Image {
        let mut out = vec![0u8; self.width * self.height * 4];
        for (o, p) in out.chunks_exact_mut(4).zip(self.px.chunks_exact(4)) {
            unpremultiply([p[0] as u32, p[1] as u32, p[2] as u32, p[3] as u32], o);
        }
        Image {
            width: self.width as u32,
            height: self.height as u32,
            pixels: out,
        }
    }

    /// The target pixels a `w` × `h` rectangle at (`left`, `top`) of a
    /// `cw` × `ch` canvas touches: (x0, y0, width, height).
    fn rect_bounds(
        &self,
        cw: usize,
        ch: usize,
        rect: (usize, usize, usize, usize),
    ) -> (Axis, Axis) {
        let (left, top, w, h) = rect;
        (
            Axis::new(w, left as f64, cw as f64, self.width),
            Axis::new(h, top as f64, ch as f64, self.height),
        )
    }

    /// Copies the canvas pixels a frame rectangle touches (before the frame
    /// is drawn), for [`Canvas::clear_rect`].
    pub fn save_rect(&self, cw: usize, ch: usize, rect: (usize, usize, usize, usize)) -> Vec<u16> {
        let (ax, ay) = self.rect_bounds(cw, ch, rect);
        let mut v = Vec::with_capacity(ax.len() * ay.len() * 4);
        for j in 0..ay.len() {
            let o = ((ay.t0 + j) * self.width + ax.t0) * 4;
            v.extend_from_slice(&self.px[o..o + ax.len() * 4]);
        }
        v
    }

    /// Clears the area of a frame rectangle (disposal to background).
    /// Pixels only partly covered keep the uncovered part of what was
    /// there before the frame was drawn (`before`, from
    /// [`Canvas::save_rect`]), so no trace of the frame remains at the
    /// edges.
    pub fn clear_rect(
        &mut self,
        cw: usize,
        ch: usize,
        rect: (usize, usize, usize, usize),
        before: Option<&[u16]>,
    ) {
        let (ax, ay) = self.rect_bounds(cw, ch, rect);
        let n = ax.len();
        for j in 0..ay.len() {
            let ty = ay.t0 + j;
            for i in 0..n {
                let tx = ax.t0 + i;
                let cov = (ax.cov[i] * ay.cov[j]) >> WBITS;
                let keep = ONE - cov.min(ONE);
                let o = (ty * self.width + tx) * 4;
                let b = (j * n + i) * 4;
                for c in 0..4 {
                    let base = before
                        .and_then(|v| v.get(b + c))
                        .copied()
                        .unwrap_or(self.px[o + c]);
                    self.px[o + c] = ((base as u32 * keep + (ONE >> 1)) >> WBITS) as u16;
                }
            }
        }
    }
}

/// Converts premultiplied values (see [`Canvas`]) to straight RGBA8.
#[inline]
fn unpremultiply(a: [u32; 4], o: &mut [u8]) {
    let al = a[3];
    if al == 0 {
        o[..4].copy_from_slice(&[0, 0, 0, 0]);
    } else if al >= FULL {
        for c in 0..3 {
            o[c] = ((a[c] + 127) / 255).min(255) as u8;
        }
        o[3] = 255;
    } else {
        for c in 0..3 {
            o[c] = ((a[c] * 255 + al / 2) / al).min(255) as u8;
        }
        o[3] = ((al + 127) / 255) as u8;
    }
}

/// Where the scaled rows go.
pub(crate) enum Dest<'a> {
    /// Straight RGBA at target size, initially transparent (a single image).
    Image(&'a mut Image),
    /// Composited onto an animation canvas.
    Canvas(&'a mut Canvas, Blend),
}

/// Streaming area-averaging scaler for one image or frame (see the module
/// documentation).
pub(crate) struct Scaler<'a> {
    dest: Dest<'a>,
    src_w: usize,
    x: Axis,
    y: Axis,
    /// Accumulators for the target rows still open (ring of `slots.len()`).
    slots: Vec<Vec<u32>>,
    /// Next target row (index into `y`) to emit, and rows opened so far.
    emit: usize,
    opened: usize,
    last_src: Option<usize>,
    hrow: Vec<u32>,
}

impl<'a> Scaler<'a> {
    /// Scales a full `src_w` × `src_h` image onto `dest`.
    pub fn new(dest: Dest<'a>, src_w: usize, src_h: usize) -> Scaler<'a> {
        Self::frame(dest, (src_w, src_h), (0, 0, src_w, src_h), None)
    }

    /// Scales a frame covering `rect` = (left, top, w, h) of a canvas of
    /// size `canvas` (in source pixels); `extent` optionally narrows the
    /// canvas to a fractional size (DCT-scaled JPEG: the last row / column
    /// is only partially part of the image).
    pub fn frame(
        dest: Dest<'a>,
        canvas: (usize, usize),
        rect: (usize, usize, usize, usize),
        extent: Option<(f64, f64)>,
    ) -> Scaler<'a> {
        let (tw, th) = match &dest {
            Dest::Image(img) => (img.width as usize, img.height as usize),
            Dest::Canvas(c, _) => (c.width, c.height),
        };
        let (ew, eh) = extent.unwrap_or((canvas.0 as f64, canvas.1 as f64));
        let (left, top, w, h) = rect;
        let x = Axis::new(w, left as f64, ew, tw);
        let y = Axis::new(h, top as f64, eh, th);
        // Target rows one source row can touch at once (2 when shrinking).
        let mut k = 1;
        let mut lo = 0;
        for j in 0..y.len() {
            while lo < j && y.first[lo] + y.count[lo] <= y.first[j] {
                lo += 1;
            }
            k = k.max(j - lo + 1);
        }
        let slots = vec![vec![0u32; x.len() * 4]; k + 1];
        Scaler {
            dest,
            src_w: w,
            hrow: vec![0; x.len() * 4],
            x,
            y,
            slots,
            emit: 0,
            opened: 0,
            last_src: None,
        }
    }

    /// Emits all target rows that are still open (call after the last row;
    /// missing source rows count as transparent).
    pub fn finish(&mut self) {
        while self.emit < self.y.len() {
            if self.emit >= self.opened {
                self.open(self.emit);
            }
            self.emit_row(self.emit);
            self.emit += 1;
        }
    }

    fn open(&mut self, j: usize) {
        while self.opened <= j {
            let k = self.slots.len();
            self.slots[self.opened % k].iter_mut().for_each(|v| *v = 0);
            self.opened += 1;
        }
    }

    fn emit_row(&mut self, j: usize) {
        let k = self.slots.len();
        let acc = &self.slots[j % k];
        let ty = self.y.t0 + j;
        let x0 = self.x.t0;
        let cov_y = self.y.cov[j];
        match &mut self.dest {
            Dest::Image(img) => {
                let tw = img.width as usize;
                let row = &mut img.pixels[(ty * tw + x0) * 4..(ty * tw + x0 + self.x.len()) * 4];
                for (o, a) in row.chunks_exact_mut(4).zip(acc.chunks_exact(4)) {
                    let r = |v: u32| (v + (ONE >> 1)) >> WBITS;
                    unpremultiply([r(a[0]), r(a[1]), r(a[2]), r(a[3])], o);
                }
            }
            Dest::Canvas(c, blend) => {
                let tw = c.width;
                let row = &mut c.px[(ty * tw + x0) * 4..(ty * tw + x0 + self.x.len()) * 4];
                for (i, (o, a)) in row.chunks_exact_mut(4).zip(acc.chunks_exact(4)).enumerate() {
                    let r = |v: u32| ((v + (ONE >> 1)) >> WBITS).min(FULL);
                    let src = [r(a[0]), r(a[1]), r(a[2]), r(a[3])];
                    let keep = match blend {
                        // dst * (1 - src alpha)
                        Blend::Over => ((FULL - src[3].min(FULL)) << WBITS) / FULL,
                        // dst * (1 - coverage)
                        Blend::Replace => ONE - ((self.x.cov[i] * cov_y) >> WBITS).min(ONE),
                    };
                    for c in 0..4 {
                        let v = src[c] + ((o[c] as u32 * keep + (ONE >> 1)) >> WBITS);
                        o[c] = v.min(FULL) as u16;
                    }
                }
            }
        }
    }
}

impl RowSink for Scaler<'_> {
    fn row(&mut self, y: usize, rgba: &[u8]) {
        if self.last_src.is_some_and(|l| y <= l) || self.x.len() == 0 {
            return;
        }
        self.last_src = Some(y);
        // Identity: copy straight into an image.
        if self.x.identity && self.y.identity {
            if let Dest::Image(img) = &mut self.dest {
                let n = self.src_w * 4;
                if let (Some(dst), Some(src)) =
                    (img.pixels.get_mut(y * n..(y + 1) * n), rgba.get(..n))
                {
                    dst.copy_from_slice(src);
                }
                self.emit = self.y.len();
                return;
            }
        }
        // Emit the target rows that end before this source row.
        while self.emit < self.y.len() && self.y.first[self.emit] + self.y.count[self.emit] <= y {
            if self.emit >= self.opened {
                self.open(self.emit);
            }
            self.emit_row(self.emit);
            self.emit += 1;
        }
        if self.emit >= self.y.len() || self.y.first[self.emit] > y {
            return;
        }
        // Horizontal pass on premultiplied values.
        let x = &self.x;
        let n = self.src_w.min(rgba.len() / 4);
        for (i, d) in self.hrow.chunks_exact_mut(4).enumerate() {
            let first = x.first[i];
            let mut acc = [0u32; 4];
            for (j, &w) in x.weights(i).iter().enumerate() {
                let sx = first + j;
                if sx >= n {
                    break;
                }
                let p = &rgba[sx * 4..sx * 4 + 4];
                let a = p[3] as u32;
                acc[0] += w * (p[0] as u32 * a);
                acc[1] += w * (p[1] as u32 * a);
                acc[2] += w * (p[2] as u32 * a);
                acc[3] += w * (a * 255);
            }
            for c in 0..4 {
                d[c] = (acc[c] + (ONE >> 1)) >> WBITS;
            }
        }
        // Vertical: add into every open target row that contains `y`.
        let k = self.slots.len();
        let mut j = self.emit;
        while j < self.y.len() && self.y.first[j] <= y {
            let rel = y - self.y.first[j];
            if rel < self.y.count[j] {
                if j >= self.opened {
                    self.open(j);
                }
                let w = self.y.weights(j)[rel];
                if w > 0 {
                    for (a, &v) in self.slots[j % k].iter_mut().zip(&self.hrow) {
                        *a += w * v;
                    }
                }
            }
            j += 1;
        }
        // The last source row of the current target row may be this one.
        while self.emit < self.y.len() && self.y.first[self.emit] + self.y.count[self.emit] <= y + 1
        {
            if self.emit >= self.opened {
                self.open(self.emit);
            }
            self.emit_row(self.emit);
            self.emit += 1;
        }
    }
}

/// Feeds all rows of `img` into `sink`.
pub(crate) fn push_image(img: &Image, sink: &mut dyn RowSink) {
    let n = img.width as usize * 4;
    for (y, row) in img.pixels.chunks_exact(n.max(1)).enumerate() {
        sink.row(y, row);
    }
}

/// Scales a full image onto a new target-size image.
pub(crate) fn scale_image(img: &Image, tw: u32, th: u32) -> Image {
    if (tw, th) == (img.width, img.height) {
        return img.clone();
    }
    let mut out = Image::new(tw, th);
    {
        let mut s = Scaler::new(
            Dest::Image(&mut out),
            img.width as usize,
            img.height as usize,
        );
        push_image(img, &mut s);
        s.finish();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, px: [u8; 4]) -> Image {
        Image {
            width: w,
            height: h,
            pixels: px.repeat((w * h) as usize),
        }
    }

    #[test]
    fn fit_targets() {
        // Cover 1920x1080 with a 16K image: 1920x1080.
        let f = Fit::cover(1920, 1080, 8192, 1 << 25);
        assert_eq!(f.target(15360, 8640), (1920, 1080));
        // Portrait screen: the landscape image must cover the height.
        let f = Fit::cover(1080, 1920, 8192, 1 << 25);
        assert_eq!(f.target(15360, 8640), (3413, 1920));
        // Never enlarged.
        assert_eq!(f.target(640, 360), (640, 360));
        // Side limit.
        let f = Fit::cover(10000, 10000, 4096, 1 << 30);
        assert_eq!(f.target(15360, 8640), (4096, 2304));
        // Pixel budget.
        let f = Fit::cover(10000, 10000, 1 << 20, 1_000_000);
        let (w, h) = f.target(16000, 9000);
        assert!(w as u64 * h as u64 <= 1_000_000, "{w}x{h}");
        assert!(w > 1300 && h > 740);
        // Within: decode_max semantics.
        assert_eq!(Fit::within(2560).target(5120, 2880), (2560, 1440));
        assert_eq!(Fit::within(2560).target(100, 50), (100, 50));
    }

    #[test]
    fn streaming_box_average() {
        // 4x4 -> 2x2 of a checker of black and white: all gray 128.
        let mut img = Image::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                let v = if (x + y) % 2 == 0 { 255 } else { 0 };
                let i = (y * 4 + x) * 4;
                img.pixels[i..i + 4].copy_from_slice(&[v, v, v, 255]);
            }
        }
        let out = scale_image(&img, 2, 2);
        for p in out.pixels.chunks_exact(4) {
            assert!((p[0] as i32 - 128).abs() <= 1 && p[3] == 255, "{p:?}");
        }
        // Matches the non-streaming resize on a gradient.
        let mut g = Image::new(37, 23);
        for (i, p) in g.pixels.chunks_exact_mut(4).enumerate() {
            p.copy_from_slice(&[(i * 7) as u8, (i * 3) as u8, (i / 5) as u8, 255]);
        }
        let a = scale_image(&g, 11, 7);
        let b = crate::resize::resize(&g, 11, 7);
        for (x, y) in a.pixels.iter().zip(&b.pixels) {
            assert!((*x as i32 - *y as i32).abs() <= 1);
        }
    }

    #[test]
    fn identity_and_alpha() {
        let img = solid(5, 3, [10, 20, 30, 40]);
        assert_eq!(scale_image(&img, 5, 3).pixels, img.pixels);
        let out = scale_image(&solid(10, 6, [200, 100, 50, 128]), 3, 2);
        for p in out.pixels.chunks_exact(4) {
            assert!((p[0] as i32 - 200).abs() <= 1 && (p[3] as i32 - 128).abs() <= 1);
        }
    }

    #[test]
    fn frame_rect_coverage() {
        // A 2x2 opaque white frame at (1,1) of a 4x4 canvas, scaled to 2x2:
        // each target pixel is a quarter covered.
        let mut c = Canvas::new(2, 2);
        {
            let mut s = Scaler::frame(
                Dest::Canvas(&mut c, Blend::Over),
                (4, 4),
                (1, 1, 2, 2),
                None,
            );
            let row = [255u8; 8];
            s.row(0, &row);
            s.row(1, &row);
            s.finish();
        }
        let img = c.to_image();
        for p in img.pixels.chunks_exact(4) {
            assert_eq!(p, [255, 255, 255, 64], "{:?}", img.pixels);
        }
        // Clearing the same rectangle restores what was there before.
        let before = Canvas::new(2, 2).save_rect(4, 4, (1, 1, 2, 2));
        c.clear_rect(4, 4, (1, 1, 2, 2), Some(&before));
        assert!(c.px.iter().all(|&v| v == 0), "{:?}", c.px);
        // Replace: a transparent frame over an opaque canvas punches a hole
        // proportional to coverage; "over" keeps the canvas.
        let mut c = Canvas::new(2, 2);
        c.px.chunks_exact_mut(4)
            .for_each(|p| p.copy_from_slice(&[65025, 0, 0, 65025]));
        let mut over = c.clone();
        for (canvas, blend, alpha) in [(&mut c, Blend::Replace, 191), (&mut over, Blend::Over, 255)]
        {
            let mut s = Scaler::frame(Dest::Canvas(canvas, blend), (4, 4), (1, 1, 2, 2), None);
            s.row(0, &[0u8; 8]);
            s.row(1, &[0u8; 8]);
            s.finish();
            for p in canvas.to_image().pixels.chunks_exact(4) {
                assert_eq!(p, [255, 0, 0, alpha]);
            }
        }
    }
}
