//! Translation of Android input events (`AInputEvent`) into application
//! [`Event`]s: touch with pointer ids, mouse (buttons, hover, wheel),
//! stylus (treated as touch) and key codes.

use crate::ffi::*;
use oc_app::host::{Button, Event, Key, Mods, TouchPhase};

/// Android key codes used below (android.view.KeyEvent).
pub mod keycode {
    pub const BACK: i32 = 4;
    pub const DPAD_UP: i32 = 19;
    pub const DPAD_DOWN: i32 = 20;
    pub const DPAD_LEFT: i32 = 21;
    pub const DPAD_RIGHT: i32 = 22;
    pub const DPAD_CENTER: i32 = 23;
    pub const VOLUME_UP: i32 = 24;
    pub const VOLUME_DOWN: i32 = 25;
    pub const NUM_0: i32 = 7;
    pub const NUM_9: i32 = 16;
    pub const A: i32 = 29;
    pub const Z: i32 = 54;
    pub const TAB: i32 = 61;
    pub const SPACE: i32 = 62;
    pub const ENTER: i32 = 66;
    pub const DEL: i32 = 67;
    pub const MINUS: i32 = 69;
    pub const EQUALS: i32 = 70;
    pub const HEADSETHOOK: i32 = 79;
    pub const PLUS: i32 = 81;
    pub const MEDIA_PLAY_PAUSE: i32 = 85;
    pub const MEDIA_STOP: i32 = 86;
    pub const MEDIA_NEXT: i32 = 87;
    pub const MEDIA_PREVIOUS: i32 = 88;
    pub const PAGE_UP: i32 = 92;
    pub const PAGE_DOWN: i32 = 93;
    pub const ESCAPE: i32 = 111;
    pub const FORWARD_DEL: i32 = 112;
    pub const MOVE_HOME: i32 = 122;
    pub const MOVE_END: i32 = 123;
    pub const INSERT: i32 = 124;
    pub const MEDIA_PLAY: i32 = 126;
    pub const MEDIA_PAUSE: i32 = 127;
    pub const F1: i32 = 131;
    pub const F12: i32 = 142;
    pub const NUMPAD_SUBTRACT: i32 = 156;
    pub const NUMPAD_ADD: i32 = 157;
    pub const NUMPAD_ENTER: i32 = 160;
    pub const VOLUME_MUTE: i32 = 164;
}

/// Maps an Android key code to the application's key. `MEDIA_PLAY` and
/// `MEDIA_PAUSE` map to `MediaPlayPause`; the caller filters them by the
/// current playback state.
pub fn map_keycode(code: i32) -> Key {
    use keycode::*;
    match code {
        BACK => Key::Back,
        ESCAPE => Key::Escape,
        ENTER | NUMPAD_ENTER | DPAD_CENTER => Key::Enter,
        TAB => Key::Tab,
        DEL => Key::Backspace,
        FORWARD_DEL => Key::Delete,
        INSERT => Key::Insert,
        DPAD_LEFT => Key::Left,
        DPAD_RIGHT => Key::Right,
        DPAD_UP => Key::Up,
        DPAD_DOWN => Key::Down,
        MOVE_HOME => Key::Home,
        MOVE_END => Key::End,
        PAGE_UP => Key::PageUp,
        PAGE_DOWN => Key::PageDown,
        SPACE => Key::Space,
        MINUS | NUMPAD_SUBTRACT => Key::Minus,
        EQUALS | PLUS | NUMPAD_ADD => Key::Equals,
        MEDIA_PLAY_PAUSE | HEADSETHOOK | MEDIA_PLAY | MEDIA_PAUSE => Key::MediaPlayPause,
        MEDIA_NEXT => Key::MediaNext,
        MEDIA_PREVIOUS => Key::MediaPrev,
        MEDIA_STOP => Key::MediaStop,
        VOLUME_UP => Key::VolumeUp,
        VOLUME_DOWN => Key::VolumeDown,
        VOLUME_MUTE => Key::VolumeMute,
        c if (F1..=F12).contains(&c) => Key::F((c - F1 + 1) as u8),
        c if (NUM_0..=NUM_9).contains(&c) => Key::Char((b'0' + (c - NUM_0) as u8) as char),
        c if (A..=Z).contains(&c) => Key::Char((b'A' + (c - A) as u8) as char),
        _ => Key::Unknown,
    }
}

pub fn mods(meta: i32) -> Mods {
    Mods { shift: meta & AMETA_SHIFT_ON != 0, ctrl: meta & AMETA_CTRL_ON != 0, alt: meta & AMETA_ALT_ON != 0, meta: meta & AMETA_META_ON != 0 }
}

/// Mouse button state carried between motion events.
#[derive(Default)]
pub struct PointerState {
    buttons: i32,
}

/// Result of translating one motion event.
#[derive(Default)]
pub struct Motion {
    pub events: Vec<Event>,
    /// A finger/stylus touched down (used to re-show the soft keyboard).
    pub touch_started: bool,
}

const BUTTONS: [(i32, Button); 3] =
    [(AMOTION_EVENT_BUTTON_PRIMARY, Button::Left), (AMOTION_EVENT_BUTTON_SECONDARY, Button::Right), (AMOTION_EVENT_BUTTON_TERTIARY, Button::Middle)];

/// Translates a motion event.
///
/// # Safety
/// `ev` must be a valid motion event obtained from the input queue and not
/// yet finished.
pub unsafe fn translate_motion(ev: *const AInputEvent, state: &mut PointerState) -> Motion {
    let mut out = Motion::default();
    let raw_action = AMotionEvent_getAction(ev);
    let action = raw_action & AMOTION_EVENT_ACTION_MASK;
    let index = ((raw_action & AMOTION_EVENT_ACTION_POINTER_INDEX_MASK) >> AMOTION_EVENT_ACTION_POINTER_INDEX_SHIFT) as usize;
    let count = AMotionEvent_getPointerCount(ev);
    if count == 0 {
        return out;
    }
    let index = index.min(count - 1);
    let source = AInputEvent_getSource(ev);
    let tool = AMotionEvent_getToolType(ev, index);
    let is_mouse = tool == AMOTION_EVENT_TOOL_TYPE_MOUSE || (source & AINPUT_SOURCE_MOUSE) == AINPUT_SOURCE_MOUSE;
    if is_mouse {
        let (x, y) = (AMotionEvent_getX(ev, 0), AMotionEvent_getY(ev, 0));
        match action {
            AMOTION_EVENT_ACTION_SCROLL => {
                let v = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_VSCROLL, 0);
                let h = AMotionEvent_getAxisValue(ev, AMOTION_EVENT_AXIS_HSCROLL, 0);
                if v != 0.0 || h != 0.0 {
                    // Android: +v = wheel away (up), +h = right. The app
                    // uses +dx for "scroll left" like X11 button 6.
                    out.events.push(Event::Wheel { dx: -h, dy: v });
                }
                return out;
            }
            AMOTION_EVENT_ACTION_HOVER_EXIT => {
                if state.buttons == 0 {
                    out.events.push(Event::PointerLeave);
                }
                return out;
            }
            _ => {}
        }
        out.events.push(Event::PointerMove { x, y });
        let mut buttons = AMotionEvent_getButtonState(ev);
        match action {
            // Some devices report no button state on DOWN/UP.
            AMOTION_EVENT_ACTION_DOWN if buttons == 0 => buttons = AMOTION_EVENT_BUTTON_PRIMARY,
            AMOTION_EVENT_ACTION_UP | AMOTION_EVENT_ACTION_CANCEL => buttons = 0,
            _ => {}
        }
        for (bit, button) in BUTTONS {
            let was = state.buttons & bit != 0;
            let now = buttons & bit != 0;
            if now && !was {
                out.events.push(Event::PointerDown { x, y, button });
            } else if was && !now {
                out.events.push(Event::PointerUp { x, y, button });
            }
        }
        state.buttons = buttons;
        return out;
    }
    // Touch screen, stylus, eraser: touches with pointer ids.
    let touch = |i: usize, phase: TouchPhase| Event::Touch {
        id: AMotionEvent_getPointerId(ev, i).max(0) as u64,
        phase,
        x: AMotionEvent_getX(ev, i),
        y: AMotionEvent_getY(ev, i),
    };
    match action {
        AMOTION_EVENT_ACTION_DOWN | AMOTION_EVENT_ACTION_POINTER_DOWN => {
            out.events.push(touch(index, TouchPhase::Start));
            out.touch_started = true;
        }
        AMOTION_EVENT_ACTION_UP | AMOTION_EVENT_ACTION_POINTER_UP => out.events.push(touch(index, TouchPhase::End)),
        AMOTION_EVENT_ACTION_MOVE => {
            for i in 0..count {
                out.events.push(touch(i, TouchPhase::Move));
            }
        }
        AMOTION_EVENT_ACTION_CANCEL => {
            for i in 0..count {
                out.events.push(touch(i, TouchPhase::Cancel));
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keycodes() {
        assert_eq!(map_keycode(keycode::A), Key::Char('A'));
        assert_eq!(map_keycode(keycode::Z), Key::Char('Z'));
        assert_eq!(map_keycode(keycode::NUM_0 + 5), Key::Char('5'));
        assert_eq!(map_keycode(keycode::F1 + 10), Key::F(11));
        assert_eq!(map_keycode(keycode::BACK), Key::Back);
        assert_eq!(map_keycode(keycode::DEL), Key::Backspace);
        assert_eq!(map_keycode(9999), Key::Unknown);
    }
}
