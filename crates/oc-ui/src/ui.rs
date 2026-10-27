//! Immediate-mode context and widgets.

use crate::icons::{self, Icon};
use crate::input::{Input, Key, KeyPress};
use crate::text::{Align, FontSystem, Weight};
use crate::theme::Theme;
use oc_render::{Batch2d, Color, Rect};
use std::collections::HashMap;

pub type Id = u64;

/// Hashes a string into a widget id.
pub fn id(s: &str) -> Id {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    h
}

/// Derives a child id.
pub fn child(parent: Id, n: u64) -> Id {
    let mut h = parent ^ n.wrapping_mul(0x9e37_79b9_7f4a_7c15);
    h ^= h >> 29;
    h.wrapping_mul(0xbf58_476d_1ce4_e5b9)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Response {
    pub hovered: bool,
    pub pressed: bool,
    /// Held down (active) this frame.
    pub held: bool,
    pub clicked: bool,
    pub secondary_clicked: bool,
    pub double_clicked: bool,
    /// Pointer delta while held.
    pub drag: (f32, f32),
}

#[derive(Debug, Clone, Copy, Default)]
struct Scroll {
    offset: f32,
    velocity: f32,
    content: f32,
    view: f32,
    dragging_bar: bool,
}

#[derive(Debug, Clone, Default)]
struct TextEdit {
    id: Id,
    cursor: usize,
    anchor: usize,
    scroll_x: f32,
    blink: f64,
}

/// A popup (dropdown or context menu) owned by the context.
#[derive(Debug, Clone)]
pub struct MenuItem {
    pub label: String,
    pub icon: Option<Icon>,
    pub enabled: bool,
    pub checked: bool,
    pub danger: bool,
}

impl MenuItem {
    pub fn new(label: impl Into<String>) -> MenuItem {
        MenuItem { label: label.into(), icon: None, enabled: true, checked: false, danger: false }
    }
    pub fn icon(mut self, i: Icon) -> MenuItem {
        self.icon = Some(i);
        self
    }
    pub fn enabled(mut self, e: bool) -> MenuItem {
        self.enabled = e;
        self
    }
    pub fn checked(mut self, c: bool) -> MenuItem {
        self.checked = c;
        self
    }
    pub fn danger(mut self) -> MenuItem {
        self.danger = true;
        self
    }
}

#[derive(Debug, Clone)]
struct Popup {
    id: Id,
    anchor: Rect,
    items: Vec<MenuItem>,
    width: f32,
    scroll: f32,
    opened_at: f64,
}

pub struct Context {
    pub fonts: FontSystem,
    pub theme: Theme,
    active: Id,
    active_frame_seen: bool,
    focus: Id,
    focus_seen: bool,
    scrolls: HashMap<Id, Scroll>,
    drag_scroll: Option<Id>,
    text: TextEdit,
    anims: HashMap<Id, f32>,
    popup: Option<Popup>,
    popup_result: Option<(Id, usize)>,
    blockers: Vec<(Rect, u32)>,
    next_blockers: Vec<(Rect, u32)>,
    layer: u32,
    tooltip: Option<(Rect, String)>,
    hover_since: (Id, f64),
    /// Text to copy to the system clipboard (taken by the app).
    pub copy_out: Option<String>,
    /// Whether a text field has keyboard focus (show the soft keyboard).
    pub text_input_active: bool,
    pub wants_pointer_cursor: bool,
    pub wants_text_cursor: bool,
    /// Whether the UI consumed pointer input this frame (hovering a
    /// widget or panel).
    pub pointer_over_ui: bool,
    pub animating: bool,
    time: f64,
}

impl Context {
    pub fn new(fonts: FontSystem, theme: Theme) -> Context {
        Context {
            fonts,
            theme,
            active: 0,
            active_frame_seen: false,
            focus: 0,
            focus_seen: false,
            scrolls: HashMap::new(),
            drag_scroll: None,
            text: TextEdit::default(),
            anims: HashMap::new(),
            popup: None,
            popup_result: None,
            blockers: Vec::new(),
            next_blockers: Vec::new(),
            layer: 0,
            tooltip: None,
            hover_since: (0, 0.0),
            copy_out: None,
            text_input_active: false,
            wants_pointer_cursor: false,
            wants_text_cursor: false,
            pointer_over_ui: false,
            animating: false,
            time: 0.0,
        }
    }

    pub fn focused(&self) -> Id {
        self.focus
    }

    pub fn set_focus(&mut self, id: Id) {
        self.focus = id;
    }

    pub fn has_popup(&self) -> bool {
        self.popup.is_some()
    }

    pub fn close_popup(&mut self) {
        self.popup = None;
    }

    /// Starts a UI frame.
    pub fn begin<'a>(&'a mut self, batch: &'a mut Batch2d, input: &'a Input) -> Ui<'a> {
        self.time = input.time;
        self.blockers = std::mem::take(&mut self.next_blockers);
        self.layer = 0;
        self.tooltip = None;
        self.wants_pointer_cursor = false;
        self.wants_text_cursor = false;
        self.pointer_over_ui = false;
        self.animating = false;
        self.active_frame_seen = false;
        self.focus_seen = false;
        if !input.down[0] && !input.released[0] {
            self.active = 0;
        }
        if input.released[0] || !input.down[0] {
            if let Some(id) = self.drag_scroll {
                if !input.down[0] {
                    self.drag_scroll = None;
                    let _ = id;
                }
            }
        }
        if let Some(p) = &self.popup {
            // Popup blocks everything below it.
            let r = popup_rect(p, batch.screen(), &self.theme);
            self.blockers.push((r, 10));
        }
        Ui { ctx: self, batch, input, clip: Vec::new() }
    }
}

fn popup_rect(p: &Popup, screen: (f32, f32), theme: &Theme) -> Rect {
    let item_h = theme.control_height;
    let h = (p.items.len() as f32 * item_h + 8.0).min(screen.1 * 0.7);
    let w = p.width;
    let mut x = p.anchor.x;
    let mut y = p.anchor.bottom() + 4.0;
    if x + w > screen.0 - 4.0 {
        x = (screen.0 - 4.0 - w).max(4.0);
    }
    if y + h > screen.1 - 4.0 {
        y = (p.anchor.y - h - 4.0).max(4.0);
    }
    Rect::new(x, y, w, h)
}

/// A UI frame.
pub struct Ui<'a> {
    pub ctx: &'a mut Context,
    pub batch: &'a mut Batch2d,
    pub input: &'a Input,
    clip: Vec<Rect>,
}

impl Ui<'_> {
    pub fn theme(&self) -> &Theme {
        &self.ctx.theme
    }

    pub fn screen(&self) -> Rect {
        let (w, h) = self.batch.screen();
        Rect::new(0.0, 0.0, w, h)
    }

    pub fn time(&self) -> f64 {
        self.ctx.time
    }

    // ---- layering, clipping, blocking ------------------------------------

    /// Raises the current layer (popups/modals); widgets on lower layers
    /// do not receive pointer input inside rectangles blocked on higher
    /// layers.
    pub fn set_layer(&mut self, layer: u32) {
        self.ctx.layer = layer;
    }

    /// Marks `r` as opaque for pointer input from the next frame on.
    pub fn block(&mut self, r: Rect) {
        let layer = self.ctx.layer;
        self.ctx.next_blockers.push((r, layer));
        if let Some(p) = self.input.pointer {
            if r.contains(p.0, p.1) {
                self.ctx.pointer_over_ui = true;
            }
        }
    }

    pub fn push_clip(&mut self, r: Rect) {
        let r = match self.clip.last() {
            Some(c) => c.intersect(&r),
            None => r,
        };
        self.clip.push(r);
        self.batch.push_clip(r);
    }

    pub fn pop_clip(&mut self) {
        self.clip.pop();
        self.batch.pop_clip();
    }

    fn pointer_in(&self, r: Rect) -> bool {
        let Some((x, y)) = self.input.pointer else { return false };
        if !r.contains(x, y) {
            return false;
        }
        if let Some(c) = self.clip.last() {
            if !c.contains(x, y) {
                return false;
            }
        }
        let layer = self.ctx.layer;
        !self.ctx.blockers.iter().any(|(b, l)| *l > layer && b.contains(x, y))
    }

    /// Hover test respecting clipping and blockers.
    pub fn hovered(&self, r: Rect) -> bool {
        self.pointer_in(r)
    }

    /// Generic interaction for a widget rectangle.
    pub fn interact(&mut self, id: Id, r: Rect) -> Response {
        let hovered = self.pointer_in(r) && self.ctx.drag_scroll.is_none();
        let mut resp = Response { hovered, ..Default::default() };
        if hovered {
            self.ctx.pointer_over_ui = true;
        }
        if hovered && self.input.pressed[0] {
            self.ctx.active = id;
            resp.pressed = true;
        }
        if self.ctx.active == id {
            self.ctx.active_frame_seen = true;
            if self.ctx.drag_scroll.is_some() {
                self.ctx.active = 0;
            } else {
                resp.held = self.input.down[0];
                if self.input.released[0] {
                    if hovered {
                        resp.clicked = true;
                        resp.double_clicked = self.input.double_click;
                    }
                    self.ctx.active = 0;
                }
            }
        }
        if hovered && self.input.pressed[1] {
            resp.secondary_clicked = true;
        }
        if hovered && !self.input.touch {
            if self.ctx.hover_since.0 != id {
                self.ctx.hover_since = (id, self.ctx.time);
            }
        }
        resp
    }

    /// Smoothly animated value towards `target` (0..1) keyed by id.
    pub fn anim(&mut self, id: Id, target: f32, speed: f32) -> f32 {
        let dt = self.input.dt.clamp(0.0, 0.1);
        let v = self.ctx.anims.entry(id).or_insert(target);
        let k = 1.0 - (-speed * dt).exp();
        *v += (target - *v) * k;
        if (target - *v).abs() < 0.002 {
            *v = target;
        } else {
            self.ctx.animating = true;
        }
        *v
    }

    pub fn tooltip(&mut self, id: Id, r: Rect, text: &str) {
        if self.ctx.hover_since.0 == id && self.pointer_in(r) && self.ctx.time - self.ctx.hover_since.1 > 0.6 && !self.input.down[0] {
            self.ctx.tooltip = Some((r, text.to_string()));
        } else if self.ctx.hover_since.0 == id && self.pointer_in(r) {
            self.ctx.animating = true;
        }
    }

    // ---- drawing primitives ------------------------------------------------

    pub fn rect(&mut self, r: Rect, color: Color, radius: f32) {
        self.batch.rect(r, color, radius);
    }

    pub fn outline(&mut self, r: Rect, color: Color, radius: f32, width: f32) {
        self.batch.outline(r, color, radius, width);
    }

    pub fn shadow(&mut self, r: Rect, color: Color, blur: f32) {
        self.batch.shadow(r, color, blur);
    }

    pub fn panel(&mut self, r: Rect) {
        let t = &self.ctx.theme;
        let (panel, border, radius) = (t.panel, t.border, t.radius);
        self.batch.rect(r, panel, radius);
        self.batch.outline(r, border, radius, 1.0);
        self.block(r);
    }

    pub fn text(&mut self, r: Rect, s: &str, size: f32, weight: Weight, color: Color, align: Align) -> f32 {
        self.ctx.fonts.draw_in(self.batch, r, s, size, weight, color, align)
    }

    /// Text at a position (top-left), no clipping/ellipsis.
    pub fn text_at(&mut self, x: f32, y: f32, s: &str, size: f32, weight: Weight, color: Color) -> f32 {
        self.ctx.fonts.draw(self.batch, x, y, s, size, weight, color)
    }

    /// Wrapped multi-line text; returns the used height.
    pub fn paragraph(&mut self, r: Rect, s: &str, size: f32, weight: Weight, color: Color) -> f32 {
        let lines = self.ctx.fonts.wrap(s, size, weight, r.w);
        let lh = self.ctx.fonts.line_height(size);
        let mut y = r.y;
        for l in &lines {
            self.ctx.fonts.draw(self.batch, r.x, y, l, size, weight, color);
            y += lh;
        }
        y - r.y
    }

    pub fn measure(&mut self, s: &str, size: f32, weight: Weight) -> f32 {
        self.ctx.fonts.measure(s, size, weight)
    }

    pub fn icon(&mut self, r: Rect, icon: Icon, size: f32, color: Color) {
        icons::draw(&mut self.ctx.fonts, self.batch, icon, r, size, color);
    }

    pub fn image(&mut self, r: Rect, texture: u32, uv: [f32; 4], tint: Color, radius: f32) {
        self.batch.image(r, texture, uv, tint, radius);
    }

    /// Image scaled to cover `r` (cropping), keeping the aspect ratio.
    pub fn image_cover(&mut self, r: Rect, texture: u32, tex_w: u32, tex_h: u32, tint: Color, radius: f32) {
        let ta = tex_w as f32 / tex_h.max(1) as f32;
        let ra = r.w / r.h.max(1.0);
        let (mut u0, mut u1, mut v0, mut v1) = (0.0, 1.0, 1.0, 0.0);
        if ta > ra {
            let f = ra / ta;
            u0 = (1.0 - f) / 2.0;
            u1 = 1.0 - u0;
        } else {
            let f = ta / ra;
            let m = (1.0 - f) / 2.0;
            v0 = 1.0 - m;
            v1 = m;
        }
        self.batch.image(r, texture, [u0, v0, u1, v1], tint, radius);
    }

    // ---- widgets -----------------------------------------------------------

    /// Flat button with label (and optional icon).
    pub fn button(&mut self, id: Id, r: Rect, label: &str, icon: Option<Icon>, primary: bool) -> Response {
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let hov = self.anim(id, if resp.hovered { 1.0 } else { 0.0 }, 18.0);
        let bg = if primary {
            t.primary.lerp(t.primary_hover, hov)
        } else if resp.held {
            t.surface_active
        } else {
            t.surface.lerp(t.surface_hover, hov)
        };
        self.rect(r, bg, t.radius_small);
        let fg = if primary { t.primary_text } else { t.text };
        let tw = if label.is_empty() { 0.0 } else { self.measure(label, t.font, Weight::Medium) };
        let iw = if icon.is_some() { 18.0 } else { 0.0 };
        let gap = if icon.is_some() && !label.is_empty() { 8.0 } else { 0.0 };
        let total = (tw + iw + gap).min(r.w - 16.0);
        let mut x = r.x + (r.w - total) / 2.0;
        if let Some(i) = icon {
            self.icon(Rect::new(x, r.y, iw, r.h), i, 18.0, fg);
            x += iw + gap;
        }
        if !label.is_empty() {
            self.text(Rect::new(x, r.y, (r.right() - 8.0 - x).max(0.0), r.h), label, t.font, Weight::Medium, fg, Align::Left);
        }
        if resp.hovered {
            self.ctx.wants_pointer_cursor = true;
        }
        resp
    }

    /// Round icon button; `active` highlights it.
    pub fn icon_button(&mut self, id: Id, r: Rect, icon: Icon, size: f32, active: bool, tooltip: &str) -> Response {
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let hov = self.anim(id, if resp.hovered { 1.0 } else { 0.0 }, 18.0);
        if hov > 0.0 || resp.held {
            let c = if resp.held { t.surface_active } else { t.surface_hover.mul_alpha(hov) };
            self.rect(r, c, r.w.min(r.h) / 2.0);
        }
        let color = if active { t.primary_hover } else { t.text.lerp(t.text, hov) };
        self.icon(r, icon, size, color);
        if resp.hovered {
            self.ctx.wants_pointer_cursor = true;
        }
        if !tooltip.is_empty() {
            self.tooltip(id, r, tooltip);
        }
        resp
    }

    /// Switch toggle; returns true when changed.
    pub fn toggle(&mut self, id: Id, r: Rect, value: &mut bool) -> bool {
        let resp = self.interact(id, r);
        if resp.clicked {
            *value = !*value;
        }
        let t = self.ctx.theme.clone();
        let k = self.anim(id, if *value { 1.0 } else { 0.0 }, 14.0);
        let w = 38.0;
        let h = 22.0;
        let tr = Rect::new(r.right() - w, r.y + (r.h - h) / 2.0, w, h);
        let bg = Color(1.0, 1.0, 1.0, 0.18).lerp(t.primary, k);
        self.rect(tr, bg, h / 2.0);
        let knob = h - 6.0;
        let kx = tr.x + 3.0 + (w - knob - 6.0) * k;
        self.rect(Rect::new(kx, tr.y + 3.0, knob, knob), Color::WHITE, knob / 2.0);
        if resp.hovered {
            self.ctx.wants_pointer_cursor = true;
        }
        resp.clicked
    }

    pub fn checkbox(&mut self, id: Id, r: Rect, value: &mut bool, label: &str) -> bool {
        let resp = self.interact(id, r);
        if resp.clicked {
            *value = !*value;
        }
        let t = self.ctx.theme.clone();
        let b = Rect::new(r.x, r.y + (r.h - 18.0) / 2.0, 18.0, 18.0);
        if *value {
            self.rect(b, t.primary, 4.0);
            self.icon(b, Icon::Check, 14.0, Color::WHITE);
        } else {
            self.outline(b, t.text_dim, 4.0, 1.5);
        }
        self.text(Rect::new(r.x + 28.0, r.y, r.w - 28.0, r.h), label, t.font, Weight::Regular, t.text, Align::Left);
        resp.clicked
    }

    /// Horizontal slider; returns true when the value changed.
    pub fn slider(&mut self, id: Id, r: Rect, value: &mut f32, min: f32, max: f32, step: f32) -> bool {
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let mut changed = false;
        if (resp.held || resp.pressed) && max > min {
            if let Some((px, _)) = self.input.pointer {
                let f = ((px - r.x) / r.w.max(1.0)).clamp(0.0, 1.0);
                let mut v = min + (max - min) * f;
                if step > 0.0 {
                    v = (v / step).round() * step;
                }
                let v = v.clamp(min, max);
                if (v - *value).abs() > f32::EPSILON {
                    *value = v;
                    changed = true;
                }
            }
        }
        if resp.hovered && self.input.scroll.1 != 0.0 && !self.input.touch {
            let s = if step > 0.0 { step } else { (max - min) / 100.0 };
            let v = (*value + self.input.scroll.1.signum() * s).clamp(min, max);
            if v != *value {
                *value = v;
                changed = true;
            }
        }
        let f = if max > min { ((*value - min) / (max - min)).clamp(0.0, 1.0) } else { 0.0 };
        let hov = self.anim(id, if resp.hovered || resp.held { 1.0 } else { 0.0 }, 18.0);
        let th = 4.0 + 2.0 * hov;
        let track = Rect::new(r.x, r.y + (r.h - th) / 2.0, r.w, th);
        self.rect(track, Color(1.0, 1.0, 1.0, 0.18), th / 2.0);
        self.rect(Rect::new(track.x, track.y, track.w * f, th), t.text, th / 2.0);
        let kr = 6.0 + 2.0 * hov;
        let (kx, ky) = (r.x + r.w * f, r.y + r.h / 2.0);
        if hov > 0.01 {
            self.rect(Rect::new(kx - kr, ky - kr, kr * 2.0, kr * 2.0), Color::WHITE, kr);
        }
        if resp.hovered || resp.held {
            self.ctx.wants_pointer_cursor = true;
        }
        changed
    }

    /// Segmented tabs; returns true when the selection changed.
    pub fn tabs(&mut self, id: Id, r: Rect, selected: &mut usize, labels: &[&str]) -> bool {
        let t = self.ctx.theme.clone();
        self.rect(r, t.surface, t.radius_small + 2.0);
        let n = labels.len().max(1);
        let w = r.w / n as f32;
        let mut changed = false;
        let sel = self.anim(id, *selected as f32, 16.0);
        self.rect(Rect::new(r.x + 3.0 + sel * w, r.y + 3.0, w - 6.0, r.h - 6.0), t.surface_active, t.radius_small);
        for (i, l) in labels.iter().enumerate() {
            let cr = Rect::new(r.x + i as f32 * w, r.y, w, r.h);
            let resp = self.interact(child(id, i as u64), cr);
            if resp.clicked && *selected != i {
                *selected = i;
                changed = true;
            }
            let color = if *selected == i { t.text } else { t.text_dim };
            self.text(cr.shrink(4.0), l, t.font, Weight::Medium, color, Align::Center);
            if resp.hovered {
                self.ctx.wants_pointer_cursor = true;
            }
        }
        changed
    }

    // ---- text input --------------------------------------------------------

    /// Single-line text field. Returns (changed, submitted with Enter).
    pub fn text_input(&mut self, id: Id, r: Rect, value: &mut String, placeholder: &str, password: bool) -> (bool, bool) {
        let t = self.ctx.theme.clone();
        let resp = self.interact(id, r);
        let size = t.font;
        let pad = 10.0;
        let focused_before = self.ctx.focus == id;
        if resp.pressed {
            if !focused_before {
                self.ctx.focus = id;
                self.ctx.text = TextEdit { id, cursor: value.len(), anchor: value.len(), scroll_x: 0.0, blink: self.ctx.time };
            }
            let display = if password { "•".repeat(value.chars().count()) } else { value.clone() };
            if let Some((px, _)) = self.input.pointer {
                let local = px - r.x - pad + self.ctx.text.scroll_x;
                let b = self.ctx.fonts.hit_test(&display, size, Weight::Regular, local);
                let b = map_display_offset(value, &display, b, password);
                self.ctx.text.cursor = b;
                if !self.input.mods.shift {
                    self.ctx.text.anchor = b;
                }
                self.ctx.text.blink = self.ctx.time;
            }
        } else if resp.held && self.ctx.focus == id {
            if let Some((px, _)) = self.input.pointer {
                let display = if password { "•".repeat(value.chars().count()) } else { value.clone() };
                let local = px - r.x - pad + self.ctx.text.scroll_x;
                let b = self.ctx.fonts.hit_test(&display, size, Weight::Regular, local);
                self.ctx.text.cursor = map_display_offset(value, &display, b, password);
            }
        } else if self.input.pressed[0] && self.ctx.focus == id && !resp.hovered {
            self.ctx.focus = 0;
        }
        if resp.hovered {
            self.ctx.wants_text_cursor = true;
        }
        let focused = self.ctx.focus == id;
        let mut changed = false;
        let mut submitted = false;
        if focused {
            self.ctx.focus_seen = true;
            self.ctx.text_input_active = true;
            if self.ctx.text.id != id {
                self.ctx.text = TextEdit { id, cursor: value.len(), anchor: value.len(), scroll_x: 0.0, blink: self.ctx.time };
            }
            let te = &mut self.ctx.text;
            te.cursor = te.cursor.min(value.len());
            te.anchor = te.anchor.min(value.len());
            for kp in &self.input.keys {
                let KeyPress { key, mods } = *kp;
                let (lo, hi) = (te.cursor.min(te.anchor), te.cursor.max(te.anchor));
                match key {
                    Key::Enter => submitted = true,
                    Key::Escape => {
                        self.ctx.focus = 0;
                    }
                    Key::Backspace => {
                        if lo != hi {
                            value.replace_range(lo..hi, "");
                            te.cursor = lo;
                        } else if te.cursor > 0 {
                            let p = if mods.ctrl { word_left(value, te.cursor) } else { prev_char(value, te.cursor) };
                            value.replace_range(p..te.cursor, "");
                            te.cursor = p;
                        }
                        te.anchor = te.cursor;
                        changed = true;
                    }
                    Key::Delete => {
                        if lo != hi {
                            value.replace_range(lo..hi, "");
                            te.cursor = lo;
                        } else if te.cursor < value.len() {
                            let n = if mods.ctrl { word_right(value, te.cursor) } else { next_char(value, te.cursor) };
                            value.replace_range(te.cursor..n, "");
                        }
                        te.anchor = te.cursor;
                        changed = true;
                    }
                    Key::Left => {
                        te.cursor = if !mods.shift && lo != hi {
                            lo
                        } else if mods.ctrl {
                            word_left(value, te.cursor)
                        } else {
                            prev_char(value, te.cursor)
                        };
                        if !mods.shift {
                            te.anchor = te.cursor;
                        }
                    }
                    Key::Right => {
                        te.cursor = if !mods.shift && lo != hi {
                            hi
                        } else if mods.ctrl {
                            word_right(value, te.cursor)
                        } else {
                            next_char(value, te.cursor)
                        };
                        if !mods.shift {
                            te.anchor = te.cursor;
                        }
                    }
                    Key::Home => {
                        te.cursor = 0;
                        if !mods.shift {
                            te.anchor = 0;
                        }
                    }
                    Key::End => {
                        te.cursor = value.len();
                        if !mods.shift {
                            te.anchor = te.cursor;
                        }
                    }
                    Key::A if mods.ctrl => {
                        te.anchor = 0;
                        te.cursor = value.len();
                    }
                    Key::C | Key::X if mods.ctrl && lo != hi && !password => {
                        self.ctx.copy_out = Some(value[lo..hi].to_string());
                        if key == Key::X {
                            value.replace_range(lo..hi, "");
                            te.cursor = lo;
                            te.anchor = lo;
                            changed = true;
                        }
                    }
                    _ => {}
                }
                te.blink = self.ctx.time;
            }
            let te = &mut self.ctx.text;
            let mut insert = self.input.text.clone();
            if let Some(p) = &self.input.paste {
                insert.push_str(&p.replace(['\n', '\r'], " "));
            }
            if !insert.is_empty() {
                let (lo, hi) = (te.cursor.min(te.anchor), te.cursor.max(te.anchor));
                value.replace_range(lo..hi, &insert);
                te.cursor = lo + insert.len();
                te.anchor = te.cursor;
                te.blink = self.ctx.time;
                changed = true;
            }
        }
        // Drawing.
        let focus_k = self.anim(child(id, 1), if focused { 1.0 } else { 0.0 }, 20.0);
        self.rect(r, Color(0.0, 0.0, 0.0, 0.25), t.radius_small);
        let border = t.border.lerp(t.primary, focus_k);
        self.outline(r, border, t.radius_small, 1.0 + focus_k * 0.5);
        let inner = Rect::new(r.x + pad, r.y, (r.w - 2.0 * pad).max(0.0), r.h);
        let display = if password { "•".repeat(value.chars().count()) } else { value.clone() };
        let text_h = self.ctx.fonts.ascent(size) + self.ctx.fonts.descent(size);
        let ty = r.y + (r.h - text_h) / 2.0;
        self.push_clip(inner);
        if value.is_empty() && !focused {
            let ph = placeholder.to_string();
            self.text_at(inner.x, ty, &ph, size, Weight::Regular, t.text_faint);
        } else {
            let to_disp = |b: usize| if password { value[..b.min(value.len())].chars().count() * "•".len() } else { b };
            let cur_x = self.ctx.fonts.caret_x(&display, size, Weight::Regular, to_disp(self.ctx.text.cursor));
            if focused {
                let te = &mut self.ctx.text;
                if cur_x - te.scroll_x > inner.w - 2.0 {
                    te.scroll_x = cur_x - inner.w + 2.0;
                }
                if cur_x - te.scroll_x < 0.0 {
                    te.scroll_x = cur_x;
                }
            } else {
                self.ctx.text.scroll_x = if self.ctx.text.id == id { self.ctx.text.scroll_x } else { 0.0 };
            }
            let sx = if focused { self.ctx.text.scroll_x } else { 0.0 };
            if focused && self.ctx.text.cursor != self.ctx.text.anchor {
                let a = self.ctx.fonts.caret_x(&display, size, Weight::Regular, to_disp(self.ctx.text.anchor));
                let (l, rr) = (a.min(cur_x), a.max(cur_x));
                self.rect(Rect::new(inner.x + l - sx, ty - 2.0, rr - l, text_h + 4.0), t.primary.with_alpha(0.45), 2.0);
            }
            self.text_at(inner.x - sx, ty, &display, size, Weight::Regular, t.text);
            if focused {
                let blink = ((self.ctx.time - self.ctx.text.blink) * 2.0) as i64 % 2 == 0;
                if blink {
                    self.rect(Rect::new(inner.x + cur_x - sx, ty - 1.0, 1.5, text_h + 2.0), t.text, 0.0);
                }
                self.ctx.animating = true;
            }
        }
        self.pop_clip();
        (changed, submitted)
    }

    // ---- scrolling ---------------------------------------------------------

    /// Begins a vertical scroll area; returns the current offset. Content
    /// drawn between `begin_scroll` and `end_scroll` is clipped to `r`.
    pub fn begin_scroll(&mut self, id: Id, r: Rect, content_height: f32) -> f32 {
        let dt = self.input.dt.clamp(0.0, 0.1);
        let hovered = self.pointer_in(r);
        let pointer = self.input.pointer;
        let touch = self.input.touch;
        let mut s = *self.ctx.scrolls.get(&id).unwrap_or(&Scroll::default());
        s.content = content_height;
        s.view = r.h;
        let max = (content_height - r.h).max(0.0);
        if hovered && self.input.scroll.1 != 0.0 {
            s.offset -= self.input.scroll.1;
            s.velocity = 0.0;
        }
        // Touch/drag scrolling: once the pointer moved beyond a threshold
        // inside this area, the area captures the drag.
        if self.ctx.drag_scroll.is_none() && self.input.down[0] && touch && self.input.drag_distance() > 8.0 && r.contains(self.input.press_pos.0, self.input.press_pos.1) {
            let pressed_inside = !self.ctx.blockers.iter().any(|(b, l)| *l > self.ctx.layer && b.contains(self.input.press_pos.0, self.input.press_pos.1));
            if pressed_inside {
                self.ctx.drag_scroll = Some(id);
            }
        }
        if self.ctx.drag_scroll == Some(id) {
            if let (Some(_), true) = (pointer, self.input.down[0]) {
                let dy = self.input.scroll_drag_dy();
                s.offset -= dy;
                s.velocity = if dt > 0.0 { -dy / dt } else { 0.0 };
            }
        } else if s.velocity.abs() > 1.0 {
            s.offset += s.velocity * dt;
            s.velocity *= (-4.0 * dt).exp();
            self.ctx.animating = true;
        } else {
            s.velocity = 0.0;
        }
        // Scrollbar dragging (mouse).
        let bar = scrollbar_rect(r, &s);
        let bar_hit = Rect::new(r.right() - 12.0, r.y, 12.0, r.h);
        if !touch && max > 0.0 && self.input.pressed[0] && self.pointer_in(bar_hit) {
            s.dragging_bar = true;
        }
        if s.dragging_bar {
            if self.input.down[0] {
                if let Some((_, py)) = pointer {
                    let track = r.h - bar.h;
                    if track > 0.0 {
                        s.offset = ((py - r.y - bar.h / 2.0) / track * max).clamp(0.0, max);
                    }
                }
            } else {
                s.dragging_bar = false;
            }
        }
        s.offset = s.offset.clamp(0.0, max);
        self.ctx.scrolls.insert(id, s);
        if hovered {
            self.ctx.pointer_over_ui = true;
        }
        self.push_clip(r);
        s.offset
    }

    pub fn end_scroll(&mut self, id: Id, r: Rect) {
        self.pop_clip();
        let s = *self.ctx.scrolls.get(&id).unwrap_or(&Scroll::default());
        if s.content > s.view + 1.0 {
            let hov = self.pointer_in(r) || s.dragging_bar;
            let k = self.anim(child(id, 7), if hov { 1.0 } else { 0.35 }, 10.0);
            let bar = scrollbar_rect(r, &s);
            self.rect(bar, Color(1.0, 1.0, 1.0, 0.35 * k), bar.w / 2.0);
        }
    }

    pub fn scroll_offset(&self, id: Id) -> f32 {
        self.ctx.scrolls.get(&id).map(|s| s.offset).unwrap_or(0.0)
    }

    pub fn set_scroll_offset(&mut self, id: Id, offset: f32) {
        let s = self.ctx.scrolls.entry(id).or_default();
        s.offset = offset;
        s.velocity = 0.0;
    }

    /// Makes sure the row range [y, y+h) (content coordinates) is visible.
    pub fn scroll_into_view(&mut self, id: Id, y: f32, h: f32) {
        if let Some(s) = self.ctx.scrolls.get_mut(&id) {
            if y < s.offset {
                s.offset = y;
            } else if y + h > s.offset + s.view {
                s.offset = y + h - s.view;
            }
        }
    }

    /// Whether a drag on a touch screen is currently scrolling (so taps in
    /// lists should be ignored).
    pub fn is_drag_scrolling(&self) -> bool {
        self.ctx.drag_scroll.is_some()
    }

    // ---- popups ------------------------------------------------------------

    /// Opens a menu anchored at `anchor` (e.g. a button or the pointer).
    pub fn open_menu(&mut self, id: Id, anchor: Rect, items: Vec<MenuItem>, width: f32) {
        self.ctx.popup = Some(Popup { id, anchor, items, width, scroll: 0.0, opened_at: self.ctx.time });
    }

    /// Opens a context menu at the pointer.
    pub fn open_context_menu(&mut self, id: Id, items: Vec<MenuItem>) {
        let (x, y) = self.input.pointer.unwrap_or((0.0, 0.0));
        self.open_menu(id, Rect::new(x, y, 0.0, 0.0), items, 220.0);
    }

    pub fn menu_open(&self, id: Id) -> bool {
        self.ctx.popup.as_ref().is_some_and(|p| p.id == id)
    }

    /// Result of a menu opened earlier with `id` (consumed).
    pub fn menu_result(&mut self, id: Id) -> Option<usize> {
        match self.ctx.popup_result {
            Some((pid, idx)) if pid == id => {
                self.ctx.popup_result = None;
                Some(idx)
            }
            _ => None,
        }
    }

    /// Dropdown select; returns true when the selection changed.
    pub fn select(&mut self, id: Id, r: Rect, selected: &mut usize, options: &[String]) -> bool {
        let mut changed = false;
        if let Some(i) = self.menu_result(id) {
            if i < options.len() && i != *selected {
                *selected = i;
                changed = true;
            }
        }
        let resp = self.interact(id, r);
        let t = self.ctx.theme.clone();
        let open = self.menu_open(id);
        let hov = self.anim(id, if resp.hovered || open { 1.0 } else { 0.0 }, 18.0);
        self.rect(r, t.surface.lerp(t.surface_hover, hov), t.radius_small);
        let label = options.get(*selected).cloned().unwrap_or_default();
        self.text(Rect::new(r.x + 10.0, r.y, r.w - 36.0, r.h), &label, t.font, Weight::Regular, t.text, Align::Left);
        self.icon(Rect::new(r.right() - 28.0, r.y, 20.0, r.h), if open { Icon::ChevronUp } else { Icon::ChevronDown }, 16.0, t.text_dim);
        if resp.clicked {
            if open {
                self.ctx.popup = None;
            } else {
                let items = options.iter().enumerate().map(|(i, o)| MenuItem::new(o.clone()).checked(i == *selected)).collect();
                self.open_menu(id, r, items, r.w.max(160.0));
            }
        }
        if resp.hovered {
            self.ctx.wants_pointer_cursor = true;
        }
        changed
    }

    /// Draws popups and tooltips; call at the very end of the frame.
    pub fn finish(&mut self) {
        let t = self.ctx.theme.clone();
        if let Some(mut p) = self.ctx.popup.clone() {
            self.set_layer(10);
            let r = popup_rect(&p, self.batch.screen(), &t);
            self.block(r);
            self.shadow(r, t.shadow, 18.0);
            self.rect(r, t.panel_strong, t.radius_small + 2.0);
            self.outline(r, t.border, t.radius_small + 2.0, 1.0);
            let item_h = t.control_height;
            let content = p.items.len() as f32 * item_h + 8.0;
            let sid = child(p.id, 0x5c);
            let off = self.begin_scroll(sid, r, content);
            p.scroll = off;
            let mut chosen = None;
            for (i, item) in p.items.iter().enumerate() {
                let ir = Rect::new(r.x + 4.0, r.y + 4.0 + i as f32 * item_h - off, r.w - 8.0, item_h);
                if ir.bottom() < r.y || ir.y > r.bottom() {
                    continue;
                }
                let resp = self.interact(child(p.id, 1000 + i as u64), ir);
                if item.enabled && resp.hovered {
                    self.rect(ir, t.surface_hover, t.radius_small);
                    self.ctx.wants_pointer_cursor = true;
                }
                let color = if !item.enabled {
                    t.text_faint
                } else if item.danger {
                    t.danger
                } else {
                    t.text
                };
                let mut x = ir.x + 10.0;
                if let Some(ic) = item.icon {
                    self.icon(Rect::new(x, ir.y, 18.0, ir.h), ic, 16.0, color);
                    x += 28.0;
                }
                self.text(Rect::new(x, ir.y, ir.right() - x - 30.0, ir.h), &item.label, t.font, Weight::Regular, color, Align::Left);
                if item.checked {
                    self.icon(Rect::new(ir.right() - 26.0, ir.y, 18.0, ir.h), Icon::Check, 16.0, t.primary_hover);
                }
                if resp.clicked && item.enabled && self.ctx.time - p.opened_at > 0.05 {
                    chosen = Some(i);
                }
            }
            self.end_scroll(sid, r);
            if let Some(i) = chosen {
                self.ctx.popup_result = Some((p.id, i));
                self.ctx.popup = None;
            } else {
                // Close on outside press or Escape.
                let outside = self.input.pressed.iter().any(|&b| b) && self.input.pointer.is_some_and(|(x, y)| !r.contains(x, y) && !p.anchor.contains(x, y));
                if outside || self.input.key_pressed(Key::Escape) {
                    self.ctx.popup = None;
                } else if let Some(pp) = &mut self.ctx.popup {
                    pp.scroll = p.scroll;
                }
            }
            self.set_layer(0);
        }
        if let Some((r, text)) = self.ctx.tooltip.clone() {
            let size = t.font_small;
            let w = self.measure(&text, size, Weight::Regular) + 16.0;
            let (sw, sh) = self.batch.screen();
            let mut x = r.x + r.w / 2.0 - w / 2.0;
            x = x.clamp(4.0, (sw - w - 4.0).max(4.0));
            let mut y = r.bottom() + 6.0;
            if y + 28.0 > sh {
                y = r.y - 34.0;
            }
            let tr = Rect::new(x, y, w, 28.0);
            self.rect(tr, t.panel_strong, 6.0);
            self.text(tr, &text, size, Weight::Regular, t.text, Align::Center);
        }
        if !self.ctx.focus_seen && self.ctx.focus != 0 {
            // The focused widget disappeared.
            self.ctx.focus = 0;
        }
        self.ctx.text_input_active = self.ctx.focus != 0 && self.ctx.focus_seen;
        if !self.ctx.active_frame_seen && !self.input.down[0] {
            self.ctx.active = 0;
        }
    }
}

fn scrollbar_rect(r: Rect, s: &Scroll) -> Rect {
    let max = (s.content - s.view).max(1.0);
    let h = (r.h * r.h / s.content.max(1.0)).clamp(24.0, r.h);
    let y = r.y + (r.h - h) * (s.offset / max).clamp(0.0, 1.0);
    Rect::new(r.right() - 6.0, y, 4.0, h)
}

fn prev_char(s: &str, i: usize) -> usize {
    let mut j = i.saturating_sub(1);
    while j > 0 && !s.is_char_boundary(j) {
        j -= 1;
    }
    j
}

fn next_char(s: &str, i: usize) -> usize {
    let mut j = (i + 1).min(s.len());
    while j < s.len() && !s.is_char_boundary(j) {
        j += 1;
    }
    j
}

fn word_left(s: &str, i: usize) -> usize {
    let b = &s[..i];
    let trimmed = b.trim_end();
    trimmed.rfind(|c: char| c.is_whitespace()).map(|p| p + 1).unwrap_or(0)
}

fn word_right(s: &str, i: usize) -> usize {
    let rest = &s[i..];
    let start = rest.len() - rest.trim_start().len();
    let after = &rest[start..];
    i + start + after.find(|c: char| c.is_whitespace()).unwrap_or(after.len())
}

/// Maps a byte offset in the (possibly masked) display string back to the
/// value string.
fn map_display_offset(value: &str, display: &str, b: usize, password: bool) -> usize {
    if !password {
        return b.min(value.len());
    }
    let chars = display[..b.min(display.len())].chars().count();
    value.char_indices().nth(chars).map(|(i, _)| i).unwrap_or(value.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn word_motion() {
        let s = "hello big world";
        assert_eq!(word_left(s, s.len()), 10);
        assert_eq!(word_left(s, 10), 6);
        assert_eq!(word_right(s, 0), 5);
        assert_eq!(word_right(s, 5), 9);
        assert_eq!(prev_char("aä", 3), 1);
        assert_eq!(next_char("äa", 0), 2);
    }
}
