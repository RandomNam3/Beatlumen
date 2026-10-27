//! Vector icons drawn on a 24×24 grid and rasterized into the atlas at the
//! exact pixel size they are displayed at.

use crate::text::{AtlasImage, FontSystem};
use bl_render::{Batch2d, Color, Rect};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Play,
    Pause,
    Stop,
    Next,
    Prev,
    Shuffle,
    Repeat,
    RepeatOne,
    Volume,
    VolumeLow,
    Mute,
    Menu,
    Search,
    Close,
    Plus,
    Minus,
    Check,
    ChevronLeft,
    ChevronRight,
    ChevronUp,
    ChevronDown,
    Folder,
    File,
    Music,
    Album,
    Artist,
    Playlist,
    Queue,
    Image,
    Tune,
    Sparkle,
    Fullscreen,
    FullscreenExit,
    Pin,
    Server,
    Trash,
    Edit,
    More,
    Drag,
    Eye,
    EyeOff,
    Refresh,
    Upload,
    Home,
    Info,
    Warning,
    Library,
    Wave,
    Logout,
    Save,
    Copy,
    Theater,
    Spectrum,
    Clock,
    Disk,
    Undo,
    Redo,
    Sort,
}

/// Path builder producing line segments in pixel space.
struct P {
    s: f32,
    lines: Vec<[f32; 4]>,
    start: (f32, f32),
    cur: (f32, f32),
}

const W: f32 = 2.0; // default stroke width in grid units

impl P {
    fn pt(&self, x: f32, y: f32) -> (f32, f32) {
        (x * self.s, y * self.s)
    }
    fn move_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.start = p;
        self.cur = p;
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let p = self.pt(x, y);
        self.lines.push([self.cur.0, self.cur.1, p.0, p.1]);
        self.cur = p;
    }
    fn close(&mut self) {
        if self.cur != self.start {
            self.lines.push([self.cur.0, self.cur.1, self.start.0, self.start.1]);
        }
        self.cur = self.start;
    }
    /// Filled polygon (grid units).
    fn poly(&mut self, pts: &[(f32, f32)]) {
        if pts.len() < 3 {
            return;
        }
        // Normalise orientation (negative signed area in y-down space).
        let area: f32 = pts.iter().zip(pts.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum();
        let v: Vec<(f32, f32)> = if area > 0.0 { pts.iter().rev().copied().collect() } else { pts.to_vec() };
        self.move_to(v[0].0, v[0].1);
        for p in &v[1..] {
            self.line_to(p.0, p.1);
        }
        self.close();
    }
    /// Polygon with reversed orientation (cuts holes).
    fn hole(&mut self, pts: &[(f32, f32)]) {
        let area: f32 = pts.iter().zip(pts.iter().cycle().skip(1)).map(|(a, b)| a.0 * b.1 - b.0 * a.1).sum();
        let v: Vec<(f32, f32)> = if area < 0.0 { pts.iter().rev().copied().collect() } else { pts.to_vec() };
        self.move_to(v[0].0, v[0].1);
        for p in &v[1..] {
            self.line_to(p.0, p.1);
        }
        self.close();
    }
    fn circle_pts(cx: f32, cy: f32, r: f32, a0: f32, a1: f32) -> Vec<(f32, f32)> {
        let n = ((r * 6.0).ceil() as usize).clamp(8, 64);
        (0..=n)
            .map(|i| {
                let a = a0 + (a1 - a0) * i as f32 / n as f32;
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    }
    fn disc(&mut self, cx: f32, cy: f32, r: f32) {
        let mut p = Self::circle_pts(cx, cy, r, 0.0, std::f32::consts::TAU);
        p.pop();
        self.poly(&p);
    }
    fn ring(&mut self, cx: f32, cy: f32, r: f32, w: f32) {
        let mut outer = Self::circle_pts(cx, cy, r + w / 2.0, 0.0, std::f32::consts::TAU);
        outer.pop();
        let mut inner = Self::circle_pts(cx, cy, (r - w / 2.0).max(0.0), 0.0, std::f32::consts::TAU);
        inner.pop();
        self.poly(&outer);
        self.hole(&inner);
    }
    /// Stroked segment with round caps.
    fn seg(&mut self, x0: f32, y0: f32, x1: f32, y1: f32, w: f32) {
        let (dx, dy) = (x1 - x0, y1 - y0);
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (nx, ny) = (-dy / len * w / 2.0, dx / len * w / 2.0);
        self.poly(&[(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)]);
        self.disc(x0, y0, w / 2.0);
        self.disc(x1, y1, w / 2.0);
    }
    /// Stroked polyline with round joins.
    fn polyline(&mut self, pts: &[(f32, f32)], w: f32) {
        for p in pts.windows(2) {
            self.seg(p[0].0, p[0].1, p[1].0, p[1].1, w);
        }
    }
    /// Stroked arc (angles in degrees, clockwise in screen space).
    fn arc(&mut self, cx: f32, cy: f32, r: f32, a0: f32, a1: f32, w: f32) {
        let pts = Self::circle_pts(cx, cy, r, a0.to_radians(), a1.to_radians());
        self.polyline(&pts, w);
    }
    /// Filled rounded rectangle.
    fn rrect(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32) {
        let r = r.min(w / 2.0).min(h / 2.0);
        let mut pts = Vec::new();
        let corners = [(x + w - r, y + r, -90.0f32), (x + w - r, y + h - r, 0.0), (x + r, y + h - r, 90.0), (x + r, y + r, 180.0)];
        for (cx, cy, a) in corners {
            if r <= 0.0 {
                pts.push((cx, cy));
            } else {
                for i in 0..=6 {
                    let t = (a + 90.0 * i as f32 / 6.0).to_radians();
                    pts.push((cx + r * t.cos(), cy + r * t.sin()));
                }
            }
        }
        self.poly(&pts);
    }
    /// Stroked rounded rectangle outline.
    fn rrect_stroke(&mut self, x: f32, y: f32, w: f32, h: f32, r: f32, sw: f32) {
        self.rrect(x - sw / 2.0, y - sw / 2.0, w + sw, h + sw, r + sw / 2.0);
        let (ix, iy, iw, ih, ir) = (x + sw / 2.0, y + sw / 2.0, w - sw, h - sw, (r - sw / 2.0).max(0.0));
        let ir = ir.min(iw / 2.0).min(ih / 2.0);
        let mut pts = Vec::new();
        let corners = [(ix + iw - ir, iy + ir, -90.0f32), (ix + iw - ir, iy + ih - ir, 0.0), (ix + ir, iy + ih - ir, 90.0), (ix + ir, iy + ir, 180.0)];
        for (cx, cy, a) in corners {
            for i in 0..=6 {
                let t = (a + 90.0 * i as f32 / 6.0).to_radians();
                pts.push((cx + ir * t.cos(), cy + ir * t.sin()));
            }
        }
        self.hole(&pts);
    }
    fn arrow_head(&mut self, x: f32, y: f32, dx: f32, dy: f32, size: f32) {
        let len = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (ux, uy) = (dx / len, dy / len);
        let (px, py) = (-uy, ux);
        self.polyline(&[(x - ux * size + px * size, y - uy * size + py * size), (x, y), (x - ux * size - px * size, y - uy * size - py * size)], W);
    }
}

fn draw_icon(icon: Icon, p: &mut P) {
    use Icon::*;
    match icon {
        Play => p.poly(&[(7.0, 4.5), (19.5, 12.0), (7.0, 19.5)]),
        Pause => {
            p.rrect(6.0, 4.5, 4.0, 15.0, 1.2);
            p.rrect(14.0, 4.5, 4.0, 15.0, 1.2);
        }
        Stop => p.rrect(5.5, 5.5, 13.0, 13.0, 2.0),
        Next => {
            p.poly(&[(5.0, 5.0), (15.5, 12.0), (5.0, 19.0)]);
            p.rrect(16.5, 5.0, 2.8, 14.0, 1.0);
        }
        Prev => {
            p.poly(&[(19.0, 5.0), (8.5, 12.0), (19.0, 19.0)]);
            p.rrect(4.7, 5.0, 2.8, 14.0, 1.0);
        }
        Shuffle => {
            p.polyline(&[(3.0, 7.0), (7.5, 7.0), (14.5, 17.0), (20.0, 17.0)], W);
            p.polyline(&[(3.0, 17.0), (7.5, 17.0), (9.5, 14.3)], W);
            p.polyline(&[(12.6, 9.8), (14.5, 7.0), (20.0, 7.0)], W);
            p.arrow_head(20.5, 7.0, 1.0, 0.0, 3.0);
            p.arrow_head(20.5, 17.0, 1.0, 0.0, 3.0);
        }
        Repeat | RepeatOne => {
            p.polyline(&[(4.0, 12.0), (4.0, 9.5), (6.5, 7.0), (19.0, 7.0)], W);
            p.arrow_head(19.5, 7.0, 1.0, 0.0, 3.0);
            p.polyline(&[(20.0, 12.0), (20.0, 14.5), (17.5, 17.0), (5.0, 17.0)], W);
            p.arrow_head(4.5, 17.0, -1.0, 0.0, 3.0);
            if icon == RepeatOne {
                p.polyline(&[(10.8, 10.6), (12.2, 9.6), (12.2, 14.4)], 1.6);
            }
        }
        Volume | VolumeLow | Mute => {
            p.poly(&[(3.0, 9.0), (7.0, 9.0), (12.0, 4.5), (12.0, 19.5), (7.0, 15.0), (3.0, 15.0)]);
            match icon {
                Mute => {
                    p.seg(15.5, 9.0, 21.0, 15.0, W);
                    p.seg(21.0, 9.0, 15.5, 15.0, W);
                }
                VolumeLow => p.arc(13.0, 12.0, 4.0, -50.0, 50.0, W),
                _ => {
                    p.arc(13.0, 12.0, 4.0, -50.0, 50.0, W);
                    p.arc(13.0, 12.0, 8.0, -50.0, 50.0, W);
                }
            }
        }
        Menu => {
            p.seg(4.0, 6.5, 20.0, 6.5, W);
            p.seg(4.0, 12.0, 20.0, 12.0, W);
            p.seg(4.0, 17.5, 20.0, 17.5, W);
        }
        Search => {
            p.ring(10.5, 10.5, 6.0, W);
            p.seg(15.0, 15.0, 20.0, 20.0, W + 0.4);
        }
        Close => {
            p.seg(6.0, 6.0, 18.0, 18.0, W);
            p.seg(18.0, 6.0, 6.0, 18.0, W);
        }
        Plus => {
            p.seg(12.0, 5.0, 12.0, 19.0, W);
            p.seg(5.0, 12.0, 19.0, 12.0, W);
        }
        Minus => p.seg(5.0, 12.0, 19.0, 12.0, W),
        Check => p.polyline(&[(5.0, 12.5), (10.0, 17.5), (19.0, 7.0)], W),
        ChevronLeft => p.polyline(&[(15.0, 5.0), (8.0, 12.0), (15.0, 19.0)], W),
        ChevronRight => p.polyline(&[(9.0, 5.0), (16.0, 12.0), (9.0, 19.0)], W),
        ChevronUp => p.polyline(&[(5.0, 15.0), (12.0, 8.0), (19.0, 15.0)], W),
        ChevronDown => p.polyline(&[(5.0, 9.0), (12.0, 16.0), (19.0, 9.0)], W),
        Folder => {
            p.poly(&[(3.0, 6.5), (3.5, 5.0), (9.0, 5.0), (11.0, 7.0), (20.5, 7.0), (21.0, 8.0), (21.0, 18.5), (20.0, 19.5), (4.0, 19.5), (3.0, 18.5)]);
            p.hole(&[(5.0, 10.0), (19.0, 10.0), (19.0, 17.5), (5.0, 17.5)]);
        }
        File => {
            p.polyline(&[(14.0, 3.5), (6.0, 3.5), (6.0, 20.5), (18.0, 20.5), (18.0, 7.5), (14.0, 3.5), (14.0, 7.5), (18.0, 7.5)], 1.8);
        }
        Music => {
            p.disc(7.5, 17.5, 3.0);
            p.disc(17.5, 15.5, 3.0);
            p.seg(10.0, 17.0, 10.0, 5.0, W);
            p.seg(20.0, 15.0, 20.0, 3.5, W);
            p.poly(&[(9.0, 4.0), (21.0, 2.0), (21.0, 6.5), (9.0, 8.5)]);
        }
        Album => {
            p.ring(12.0, 12.0, 8.5, W);
            p.disc(12.0, 12.0, 2.2);
            p.arc(12.0, 12.0, 5.0, 200.0, 250.0, 1.4);
        }
        Artist => {
            p.ring(12.0, 8.0, 4.0, W);
            p.arc(12.0, 22.0, 8.0, 200.0, 340.0, W);
        }
        Playlist => {
            p.seg(4.0, 6.0, 15.0, 6.0, W);
            p.seg(4.0, 11.0, 15.0, 11.0, W);
            p.seg(4.0, 16.0, 10.0, 16.0, W);
            p.disc(15.5, 18.0, 2.5);
            p.seg(17.5, 18.0, 17.5, 10.0, W);
            p.seg(17.5, 10.0, 21.0, 11.0, W);
        }
        Queue => {
            p.seg(4.0, 6.0, 20.0, 6.0, W);
            p.seg(4.0, 11.0, 13.0, 11.0, W);
            p.seg(4.0, 16.0, 13.0, 16.0, W);
            p.poly(&[(16.0, 10.0), (21.0, 13.5), (16.0, 17.0)]);
        }
        Image => {
            p.rrect_stroke(3.5, 4.5, 17.0, 15.0, 2.0, W);
            p.disc(9.0, 9.5, 1.8);
            p.polyline(&[(4.5, 18.0), (10.0, 12.5), (13.5, 16.0), (16.0, 13.5), (20.0, 17.5)], W);
        }
        Tune => {
            for (y, x) in [(6.0, 15.0), (12.0, 8.0), (18.0, 13.0)] {
                p.seg(4.0, y, 20.0, y, 1.8);
                p.disc(x, y, 2.8);
            }
        }
        Sparkle => {
            p.poly(&[(10.0, 3.0), (11.8, 8.2), (17.0, 10.0), (11.8, 11.8), (10.0, 17.0), (8.2, 11.8), (3.0, 10.0), (8.2, 8.2)]);
            p.poly(&[(17.5, 13.5), (18.5, 16.5), (21.5, 17.5), (18.5, 18.5), (17.5, 21.5), (16.5, 18.5), (13.5, 17.5), (16.5, 16.5)]);
        }
        Fullscreen => {
            p.polyline(&[(4.0, 9.0), (4.0, 4.0), (9.0, 4.0)], W);
            p.polyline(&[(15.0, 4.0), (20.0, 4.0), (20.0, 9.0)], W);
            p.polyline(&[(20.0, 15.0), (20.0, 20.0), (15.0, 20.0)], W);
            p.polyline(&[(9.0, 20.0), (4.0, 20.0), (4.0, 15.0)], W);
        }
        FullscreenExit => {
            p.polyline(&[(4.0, 9.0), (9.0, 9.0), (9.0, 4.0)], W);
            p.polyline(&[(15.0, 4.0), (15.0, 9.0), (20.0, 9.0)], W);
            p.polyline(&[(20.0, 15.0), (15.0, 15.0), (15.0, 20.0)], W);
            p.polyline(&[(9.0, 20.0), (9.0, 15.0), (4.0, 15.0)], W);
        }
        Pin => {
            p.poly(&[(9.0, 3.5), (15.0, 3.5), (14.0, 10.0), (17.5, 14.0), (6.5, 14.0), (10.0, 10.0)]);
            p.seg(12.0, 14.0, 12.0, 21.0, W);
        }
        Server => {
            p.rrect_stroke(4.0, 4.0, 16.0, 6.5, 1.5, W);
            p.rrect_stroke(4.0, 13.5, 16.0, 6.5, 1.5, W);
            p.disc(7.5, 7.25, 1.1);
            p.disc(7.5, 16.75, 1.1);
        }
        Trash => {
            p.seg(4.0, 6.5, 20.0, 6.5, W);
            p.polyline(&[(9.0, 6.0), (9.5, 3.5), (14.5, 3.5), (15.0, 6.0)], 1.8);
            p.polyline(&[(6.0, 7.0), (7.0, 20.5), (17.0, 20.5), (18.0, 7.0)], W);
            p.seg(10.0, 10.5, 10.0, 17.0, 1.6);
            p.seg(14.0, 10.5, 14.0, 17.0, 1.6);
        }
        Edit => {
            p.poly(&[(15.5, 4.0), (20.0, 8.5), (9.0, 19.5), (4.0, 20.0), (4.5, 15.0)]);
            p.hole(&[(15.5, 6.8), (17.2, 8.5), (8.0, 17.7), (6.0, 18.0), (6.3, 16.0)]);
        }
        More => {
            p.disc(5.5, 12.0, 1.9);
            p.disc(12.0, 12.0, 1.9);
            p.disc(18.5, 12.0, 1.9);
        }
        Drag => {
            for y in [6.0, 12.0, 18.0] {
                p.disc(9.0, y, 1.6);
                p.disc(15.0, y, 1.6);
            }
        }
        Eye | EyeOff => {
            let mut top = P::circle_pts(12.0, 21.0, 13.0, 225.0f32.to_radians(), 315.0f32.to_radians());
            let bottom = P::circle_pts(12.0, 3.0, 13.0, 45.0f32.to_radians(), 135.0f32.to_radians());
            top.extend(bottom);
            p.polyline(&top, 1.8);
            p.ring(12.0, 12.0, 3.0, 1.8);
            if icon == EyeOff {
                p.seg(4.0, 4.0, 20.0, 20.0, W);
            }
        }
        Refresh => {
            p.arc(12.0, 12.0, 7.5, -40.0, 250.0, W);
            p.poly(&[(17.0, 2.5), (20.5, 7.8), (14.2, 8.4)]);
        }
        Upload => {
            p.seg(12.0, 4.0, 12.0, 15.0, W);
            p.polyline(&[(7.0, 8.5), (12.0, 3.5), (17.0, 8.5)], W);
            p.polyline(&[(4.0, 15.0), (4.0, 20.0), (20.0, 20.0), (20.0, 15.0)], W);
        }
        Home => {
            p.polyline(&[(3.5, 11.5), (12.0, 4.0), (20.5, 11.5)], W);
            p.polyline(&[(6.0, 10.0), (6.0, 20.0), (18.0, 20.0), (18.0, 10.0)], W);
        }
        Info => {
            p.ring(12.0, 12.0, 8.5, W);
            p.seg(12.0, 11.0, 12.0, 16.5, W);
            p.disc(12.0, 7.8, 1.3);
        }
        Warning => {
            p.poly(&[(12.0, 3.0), (22.0, 20.5), (2.0, 20.5)]);
            p.hole(&[(12.0, 6.8), (18.8, 18.7), (5.2, 18.7)]);
            p.seg(12.0, 10.0, 12.0, 14.0, 1.8);
            p.disc(12.0, 16.5, 1.1);
        }
        Library => {
            p.rrect(4.0, 4.0, 3.0, 16.0, 0.8);
            p.rrect(9.0, 4.0, 3.0, 16.0, 0.8);
            p.poly(&[(14.0, 5.0), (17.0, 4.3), (20.6, 19.0), (17.6, 19.7)]);
        }
        Wave => {
            for (i, h) in [4.0f32, 9.0, 14.0, 7.0, 11.0, 5.0].iter().enumerate() {
                let x = 4.5 + i as f32 * 3.0;
                p.seg(x, 12.0 - h / 2.0, x, 12.0 + h / 2.0, 1.8);
            }
        }
        Logout => {
            p.polyline(&[(10.0, 4.0), (5.0, 4.0), (5.0, 20.0), (10.0, 20.0)], W);
            p.seg(10.0, 12.0, 20.0, 12.0, W);
            p.polyline(&[(16.0, 8.0), (20.0, 12.0), (16.0, 16.0)], W);
        }
        Save => {
            p.polyline(&[(4.0, 4.0), (16.5, 4.0), (20.0, 7.5), (20.0, 20.0), (4.0, 20.0), (4.0, 4.0)], W);
            p.rrect(8.0, 13.0, 8.0, 7.0, 1.0);
            p.seg(8.0, 4.5, 8.0, 8.0, 1.6);
            p.seg(8.0, 8.0, 15.0, 8.0, 1.6);
        }
        Copy => {
            p.rrect_stroke(8.5, 8.5, 11.5, 11.5, 2.0, W);
            p.polyline(&[(15.0, 5.0), (15.0, 4.0), (4.0, 4.0), (4.0, 15.0), (5.0, 15.0)], W);
        }
        Theater => {
            p.rrect_stroke(3.0, 5.0, 18.0, 14.0, 2.0, W);
            p.poly(&[(10.0, 9.0), (15.0, 12.0), (10.0, 15.0)]);
        }
        Spectrum => {
            for (i, h) in [6.0f32, 12.0, 9.0, 15.0, 7.0].iter().enumerate() {
                let x = 4.0 + i as f32 * 4.0;
                p.rrect(x, 20.0 - h, 2.6, *h, 0.8);
            }
        }
        Clock => {
            p.ring(12.0, 12.0, 8.5, W);
            p.polyline(&[(12.0, 7.0), (12.0, 12.0), (15.5, 14.0)], W);
        }
        Disk => {
            p.rrect_stroke(4.0, 4.0, 16.0, 16.0, 3.0, W);
            p.disc(12.0, 12.0, 2.5);
        }
        Undo => {
            p.arc(12.0, 14.0, 6.5, 180.0, 360.0, W);
            p.seg(18.5, 14.0, 18.5, 18.0, W);
            p.polyline(&[(2.5, 10.0), (5.5, 14.5), (9.5, 11.0)], W);
        }
        Redo => {
            p.arc(12.0, 14.0, 6.5, 180.0, 360.0, W);
            p.seg(5.5, 14.0, 5.5, 18.0, W);
            p.polyline(&[(21.5, 10.0), (18.5, 14.5), (14.5, 11.0)], W);
        }
        Sort => {
            p.seg(7.0, 5.0, 7.0, 19.0, W);
            p.polyline(&[(4.0, 8.0), (7.0, 5.0), (10.0, 8.0)], W);
            p.seg(17.0, 5.0, 17.0, 19.0, W);
            p.polyline(&[(14.0, 16.0), (17.0, 19.0), (20.0, 16.0)], W);
        }
    }
}

/// Rasterizes an icon at `px` pixels (square).
pub fn rasterize(icon: Icon, px: u32) -> (u32, u32, Vec<u8>) {
    let px = px.clamp(4, 512);
    let mut p = P { s: px as f32 / 24.0, lines: Vec::new(), start: (0.0, 0.0), cur: (0.0, 0.0) };
    draw_icon(icon, &mut p);
    let cov = bl_font::rasterize_lines(px as usize, px as usize, &p.lines);
    (px, px, cov)
}

/// Draws an icon centred in `r` with logical size `size`.
pub fn draw(fonts: &mut FontSystem, batch: &mut Batch2d, icon: Icon, r: Rect, size: f32, color: Color) {
    let scale = fonts.scale();
    let px = (size * scale).round().max(4.0) as u32;
    let key = 0x1c0_0000_0000u64 | ((icon as u64) << 16) | px as u64;
    let Some(img): Option<AtlasImage> = fonts.atlas_image(key, || rasterize(icon, px)) else { return };
    let (cx, cy) = r.center();
    let x = ((cx - size / 2.0) * scale).round() / scale;
    let y = ((cy - size / 2.0) * scale).round() / scale;
    batch.glyph(Rect::new(x, y, img.w as f32 / scale, img.h as f32 / scale), img.atlas, color);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_icons_rasterize() {
        let all = [
            Icon::Play,
            Icon::Pause,
            Icon::Shuffle,
            Icon::Repeat,
            Icon::Volume,
            Icon::Search,
            Icon::Folder,
            Icon::Music,
            Icon::Album,
            Icon::Image,
            Icon::Eye,
            Icon::Edit,
            Icon::Warning,
            Icon::Refresh,
            Icon::Save,
        ];
        for icon in all {
            let (w, h, c) = rasterize(icon, 24);
            assert_eq!(c.len(), (w * h) as usize);
            let total: u32 = c.iter().map(|&v| v as u32).sum();
            assert!(total > 255 * 10, "{icon:?} looks empty");
            assert!(total < 255 * 24 * 24 * 9 / 10, "{icon:?} looks filled");
        }
    }
}
