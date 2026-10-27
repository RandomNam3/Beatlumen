//! Immediate-mode user interface toolkit for Beatlumen, rendered with
//! `bl-render`'s 2D batcher and `bl-font` text.

pub mod icons;
pub mod input;
pub mod text;
pub mod theme;
pub mod ui;

pub use icons::Icon;
pub use input::{Input, Key, KeyPress, Mods};
pub use bl_render::{Color, Rect};
pub use text::{Align, FontSystem, Weight};
pub use theme::Theme;
pub use ui::{child, id, Context, Id, MenuItem, Response, Ui};

/// Rectangle layout helpers.
pub trait RectExt {
    fn split_left(&self, w: f32) -> (Rect, Rect);
    fn split_right(&self, w: f32) -> (Rect, Rect);
    fn split_top(&self, h: f32) -> (Rect, Rect);
    fn split_bottom(&self, h: f32) -> (Rect, Rect);
    fn pad(&self, x: f32, y: f32) -> Rect;
    fn centered(&self, w: f32, h: f32) -> Rect;
    fn row(&self, i: usize, h: f32) -> Rect;
    fn columns(&self, n: usize, gap: f32) -> Vec<Rect>;
}

impl RectExt for Rect {
    fn split_left(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w).max(0.0);
        (Rect::new(self.x, self.y, w, self.h), Rect::new(self.x + w, self.y, self.w - w, self.h))
    }
    fn split_right(&self, w: f32) -> (Rect, Rect) {
        let w = w.min(self.w).max(0.0);
        (Rect::new(self.x, self.y, self.w - w, self.h), Rect::new(self.right() - w, self.y, w, self.h))
    }
    fn split_top(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h).max(0.0);
        (Rect::new(self.x, self.y, self.w, h), Rect::new(self.x, self.y + h, self.w, self.h - h))
    }
    fn split_bottom(&self, h: f32) -> (Rect, Rect) {
        let h = h.min(self.h).max(0.0);
        (Rect::new(self.x, self.y, self.w, self.h - h), Rect::new(self.x, self.bottom() - h, self.w, h))
    }
    fn pad(&self, x: f32, y: f32) -> Rect {
        Rect::new(self.x + x, self.y + y, (self.w - 2.0 * x).max(0.0), (self.h - 2.0 * y).max(0.0))
    }
    fn centered(&self, w: f32, h: f32) -> Rect {
        Rect::new(self.x + (self.w - w) / 2.0, self.y + (self.h - h) / 2.0, w, h)
    }
    fn row(&self, i: usize, h: f32) -> Rect {
        Rect::new(self.x, self.y + i as f32 * h, self.w, h)
    }
    fn columns(&self, n: usize, gap: f32) -> Vec<Rect> {
        let n = n.max(1);
        let w = (self.w - gap * (n - 1) as f32) / n as f32;
        (0..n).map(|i| Rect::new(self.x + i as f32 * (w + gap), self.y, w, self.h)).collect()
    }
}
