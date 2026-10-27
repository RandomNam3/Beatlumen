//! Outline sinks: the public [`OutlineSink`] trait, a bounding box sink and
//! the curve flattener feeding the rasterizer.

/// Receives a glyph outline in font units (y axis pointing up).
///
/// Contours always start with `move_to` and end with `close`.
pub trait OutlineSink {
    fn move_to(&mut self, x: f32, y: f32);
    fn line_to(&mut self, x: f32, y: f32);
    /// Quadratic Bézier with control point (`x1`, `y1`).
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32);
    /// Cubic Bézier with control points (`x1`, `y1`) and (`x2`, `y2`).
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32);
    fn close(&mut self);
}

/// A line segment in pixel space (y down).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Line {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

/// Flattening tolerance in pixels.
const TOLERANCE: f32 = 0.025;
/// Upper bound of segments per curve.
const MAX_SEGMENTS: f32 = 512.0;
/// Upper bound of line segments per glyph (guards memory use on malicious
/// outlines).
const MAX_LINES: usize = 1 << 22;

/// Converts an outline in font units into line segments in pixel space
/// (scaled, y flipped, shifted by the sub-pixel offset) and tracks their
/// bounding box.
pub(crate) struct Flattener {
    scale: f32,
    dx: f32,
    pub lines: Vec<Line>,
    start: (f32, f32),
    cur: (f32, f32),
    open: bool,
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
    /// Set when the outline produced more than `MAX_LINES` segments.
    pub overflow: bool,
}

impl Flattener {
    pub fn new(scale: f32, dx: f32) -> Self {
        Flattener {
            scale,
            dx,
            lines: Vec::with_capacity(128),
            start: (0.0, 0.0),
            cur: (0.0, 0.0),
            open: false,
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
            overflow: false,
        }
    }

    #[inline]
    fn map(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.scale + self.dx, -y * self.scale)
    }

    #[inline]
    fn push(&mut self, p: (f32, f32)) {
        let (x0, y0) = self.cur;
        let (x1, y1) = p;
        if x0 == x1 && y0 == y1 {
            return;
        }
        if self.lines.len() >= MAX_LINES {
            self.overflow = true;
            return;
        }
        self.min_x = self.min_x.min(x0.min(x1));
        self.max_x = self.max_x.max(x0.max(x1));
        self.min_y = self.min_y.min(y0.min(y1));
        self.max_y = self.max_y.max(y0.max(y1));
        self.lines.push(Line { x0, y0, x1, y1 });
        self.cur = p;
    }

    #[inline]
    fn ensure_open(&mut self) {
        if !self.open {
            self.start = self.cur;
            self.open = true;
        }
    }

    pub fn finish(&mut self) {
        self.close();
    }
}

impl OutlineSink for Flattener {
    fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        let p = self.map(x, y);
        self.start = p;
        self.cur = p;
        self.open = true;
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.ensure_open();
        let p = self.map(x, y);
        self.push(p);
    }

    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.ensure_open();
        let p0 = self.cur;
        let p1 = self.map(x1, y1);
        let p2 = self.map(x, y);
        let ddx = p0.0 - 2.0 * p1.0 + p2.0;
        let ddy = p0.1 - 2.0 * p1.1 + p2.1;
        let dd = (ddx * ddx + ddy * ddy).sqrt();
        // Max deviation of n uniform chords: |dd| / (4 n^2).
        let n = (dd / (4.0 * TOLERANCE)).sqrt().ceil().clamp(1.0, MAX_SEGMENTS);
        let n = if n.is_nan() { 1 } else { n as u32 };
        let step = 1.0 / n as f32;
        for i in 1..n {
            let t = i as f32 * step;
            let mt = 1.0 - t;
            let a = mt * mt;
            let b = 2.0 * mt * t;
            let c = t * t;
            self.push((a * p0.0 + b * p1.0 + c * p2.0, a * p0.1 + b * p1.1 + c * p2.1));
        }
        self.push(p2);
    }

    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.ensure_open();
        let p0 = self.cur;
        let p1 = self.map(x1, y1);
        let p2 = self.map(x2, y2);
        let p3 = self.map(x, y);
        let d1x = p0.0 - 2.0 * p1.0 + p2.0;
        let d1y = p0.1 - 2.0 * p1.1 + p2.1;
        let d2x = p1.0 - 2.0 * p2.0 + p3.0;
        let d2y = p1.1 - 2.0 * p2.1 + p3.1;
        let m = (d1x * d1x + d1y * d1y).max(d2x * d2x + d2y * d2y).sqrt();
        // |B''| <= 6 m, deviation <= |B''| / (8 n^2).
        let n = (3.0 * m / (4.0 * TOLERANCE)).sqrt().ceil().clamp(1.0, MAX_SEGMENTS);
        let n = if n.is_nan() { 1 } else { n as u32 };
        let step = 1.0 / n as f32;
        for i in 1..n {
            let t = i as f32 * step;
            let mt = 1.0 - t;
            let a = mt * mt * mt;
            let b = 3.0 * mt * mt * t;
            let c = 3.0 * mt * t * t;
            let d = t * t * t;
            self.push((
                a * p0.0 + b * p1.0 + c * p2.0 + d * p3.0,
                a * p0.1 + b * p1.1 + c * p2.1 + d * p3.1,
            ));
        }
        self.push(p3);
    }

    fn close(&mut self) {
        if self.open {
            let s = self.start;
            self.push(s);
            self.open = false;
        }
    }
}

/// Emits TrueType quadratic contours (points with on-curve flags) to a sink.
pub(crate) fn emit_quadratic(pts: &[[f32; 2]], on: &[bool], ends: &[usize], sink: &mut dyn OutlineSink) {
    let n = pts.len().min(on.len());
    let mut start = 0usize;
    for &end in ends {
        if end < start || end >= n {
            break;
        }
        emit_contour(&pts[start..=end], &on[start..=end], sink);
        start = end + 1;
    }
}

#[inline]
fn mid(a: [f32; 2], b: [f32; 2]) -> [f32; 2] {
    [(a[0] + b[0]) * 0.5, (a[1] + b[1]) * 0.5]
}

fn emit_contour(pts: &[[f32; 2]], on: &[bool], sink: &mut dyn OutlineSink) {
    let n = pts.len();
    if n == 0 {
        return;
    }
    let first = pts[0];
    let last = pts[n - 1];
    let (start, range) = if on[0] {
        (first, 1..n)
    } else if on[n - 1] {
        (last, 0..n - 1)
    } else {
        (mid(first, last), 0..n)
    };
    sink.move_to(start[0], start[1]);
    let mut ctrl: Option<[f32; 2]> = None;
    for i in range {
        let p = pts[i];
        if on[i] {
            match ctrl.take() {
                Some(c) => sink.quad_to(c[0], c[1], p[0], p[1]),
                None => sink.line_to(p[0], p[1]),
            }
        } else {
            if let Some(c) = ctrl {
                let m = mid(c, p);
                sink.quad_to(c[0], c[1], m[0], m[1]);
            }
            ctrl = Some(p);
        }
    }
    match ctrl {
        Some(c) => sink.quad_to(c[0], c[1], start[0], start[1]),
        None => sink.line_to(start[0], start[1]),
    }
    sink.close();
}

/// Sink computing the exact control-box of an outline.
#[derive(Debug, Clone, Copy)]
pub(crate) struct BoundsSink {
    pub min_x: f32,
    pub min_y: f32,
    pub max_x: f32,
    pub max_y: f32,
}

impl Default for BoundsSink {
    fn default() -> Self {
        BoundsSink {
            min_x: f32::INFINITY,
            min_y: f32::INFINITY,
            max_x: f32::NEG_INFINITY,
            max_y: f32::NEG_INFINITY,
        }
    }
}

impl BoundsSink {
    fn add(&mut self, x: f32, y: f32) {
        self.min_x = self.min_x.min(x);
        self.min_y = self.min_y.min(y);
        self.max_x = self.max_x.max(x);
        self.max_y = self.max_y.max(y);
    }
}

impl OutlineSink for BoundsSink {
    fn move_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.add(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        self.add(x1, y1);
        self.add(x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        self.add(x1, y1);
        self.add(x2, y2);
        self.add(x, y);
    }
    fn close(&mut self) {}
}
