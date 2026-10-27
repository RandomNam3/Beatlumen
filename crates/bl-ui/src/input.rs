//! Per-frame input state in logical coordinates.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Escape,
    Enter,
    Tab,
    Backspace,
    Delete,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
    A,
    C,
    V,
    X,
    Z,
    Y,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Mods {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyPress {
    pub key: Key,
    pub mods: Mods,
}

#[derive(Debug, Clone, Default)]
pub struct Input {
    /// Pointer position (None when outside the window).
    pub pointer: Option<(f32, f32)>,
    /// Buttons: 0 left/primary (also touch), 1 right, 2 middle.
    pub down: [bool; 3],
    pub pressed: [bool; 3],
    pub released: [bool; 3],
    /// Position where the primary button was last pressed.
    pub press_pos: (f32, f32),
    pub press_time: f64,
    /// Scroll delta in logical pixels (positive y = content moves down,
    /// i.e. scroll towards the top).
    pub scroll: (f32, f32),
    pub keys: Vec<KeyPress>,
    pub text: String,
    pub mods: Mods,
    /// Clipboard content, provided when a paste shortcut was pressed.
    pub paste: Option<String>,
    /// The last pointer interaction came from a touch screen.
    pub touch: bool,
    pub double_click: bool,
    pub time: f64,
    pub dt: f32,
    /// Pointer movement since the previous frame.
    pub drag_delta: (f32, f32),
}

impl Input {
    pub fn key_pressed(&self, key: Key) -> bool {
        self.keys.iter().any(|k| k.key == key)
    }

    pub fn clicked_at(&self) -> Option<(f32, f32)> {
        if self.released[0] {
            self.pointer
        } else {
            None
        }
    }

    /// Distance the pointer moved since the primary press.
    pub fn drag_distance(&self) -> f32 {
        match self.pointer {
            Some((x, y)) => ((x - self.press_pos.0).powi(2) + (y - self.press_pos.1).powi(2)).sqrt(),
            None => 0.0,
        }
    }

    /// Vertical pointer movement since the previous frame while dragging.
    pub fn scroll_drag_dy(&self) -> f32 {
        self.drag_delta.1
    }

    /// Clears per-frame state (call after the UI frame).
    pub fn end_frame(&mut self) {
        self.pressed = [false; 3];
        self.released = [false; 3];
        self.scroll = (0.0, 0.0);
        self.keys.clear();
        self.text.clear();
        self.paste = None;
        self.double_click = false;
        self.drag_delta = (0.0, 0.0);
    }
}
