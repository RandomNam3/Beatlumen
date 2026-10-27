//! Background camera: framing math and animated state.
//!
//! The framing math follows midori (MIT License, Copyright (c) 2020 Benjamin
//! Pang): the image is a plane of width 1 and height 1/aspect at z = 0,
//! viewed by a perspective camera (35° vertical FOV) looking down -Z.
//! Normalised camera positions use x, y in [0, 1] to pan across the range in
//! which no image edge is visible, z in [0, 1] as fraction of the largest
//! distance at which the view still fits the image, and a z rotation.

use crate::easing::Easing;
use crate::rng::Rng;
use crate::tween::Tween;

pub const FOV_DEG: f32 = 35.0;

/// Visible height at distance `d`.
fn visible_height(d: f32) -> f32 {
    2.0 * (FOV_DEG.to_radians() / 2.0).tan() * d
}

/// Largest axis aligned rectangle inside a `w`×`h` rectangle rotated by
/// `angle`.
pub fn inner_rect(w: f32, h: f32, angle: f32) -> (f32, f32) {
    let wide = w >= h;
    let (long, short) = if wide { (w, h) } else { (h, w) };
    let s = angle.sin().abs();
    let c = angle.cos().abs();
    if short <= 2.0 * s * c * long || (s - c).abs() < 1e-10 {
        let x = 0.5 * short;
        if wide {
            (x / s.max(1e-12), x / c.max(1e-12))
        } else {
            (x / c.max(1e-12), x / s.max(1e-12))
        }
    } else {
        let cos2 = c * c - s * s;
        ((w * c - h * s) / cos2, (h * c - w * s) / cos2)
    }
}

/// Camera distance at which the (rotated) view exactly fits inside the plane.
pub fn max_distance(plane_w: f32, plane_h: f32, view_aspect: f32, angle: f32) -> f32 {
    let (iw, ih) = inner_rect(plane_w, plane_h, angle);
    let u = 2.0 * (FOV_DEG.to_radians() / 2.0).tan();
    (iw / (u * view_aspect)).min(ih / u)
}

/// World space camera placement for a normalised position.
pub fn world_position(plane_w: f32, plane_h: f32, view_aspect: f32, x: f32, y: f32, z: f32, rot: f32) -> [f32; 3] {
    let dist_max = max_distance(plane_w, plane_h, view_aspect, rot);
    let (iw, ih) = inner_rect(plane_w, plane_h, rot);
    let d = z * dist_max;
    let vis_h = visible_height(d);
    let vis_w = vis_h * view_aspect;
    let range_w = iw - vis_w;
    let range_h = ih - vis_h;
    let fx = -range_w / 2.0 + x * range_w;
    let fy = range_h / 2.0 - y * range_h;
    let (s, c) = rot.sin_cos();
    [fx * c - fy * s, fx * s + fy * c, d]
}

/// A normalised camera position (rotation in degrees).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CamPos {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub zr_deg: f32,
}

impl Default for CamPos {
    fn default() -> Self {
        CamPos { x: 0.5, y: 0.5, z: 1.0, zr_deg: 0.0 }
    }
}

/// Camera output for rendering.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CameraView {
    /// World position.
    pub position: [f32; 3],
    /// Rotation around the view axis in radians.
    pub rotation: f32,
    pub fov_deg: f32,
}

#[derive(Debug, Clone)]
struct Sway {
    amplitude: [f32; 4],
    duration: f64,
    easing: Easing,
    tween: Tween<4>,
}

/// Animated camera of one background scene.
#[derive(Debug, Clone)]
pub struct Camera {
    /// x, y, z normalised; rotation in radians.
    position: [f32; 4],
    /// Shake offset: x, y, z normalised; rotation in degrees.
    pub offset: [f32; 4],
    sway_offset: [f32; 4],
    sway: Option<Sway>,
    move_tween: Option<Tween<3>>,
    rotate_tween: Option<Tween<1>>,
    with_offset: [f32; 4],
}

impl Default for Camera {
    fn default() -> Self {
        Camera::new(CamPos::default())
    }
}

impl Camera {
    pub fn new(p: CamPos) -> Camera {
        let position = [p.x, p.y, p.z, p.zr_deg.to_radians()];
        Camera { position, offset: [0.0; 4], sway_offset: [0.0; 4], sway: None, move_tween: None, rotate_tween: None, with_offset: position }
    }

    /// Current base position (without shake/sway), rotation in degrees.
    pub fn position(&self) -> CamPos {
        CamPos { x: self.position[0], y: self.position[1], z: self.position[2], zr_deg: self.position[3].to_degrees() }
    }

    pub fn is_moving(&self, now: f64) -> bool {
        self.move_tween.as_ref().is_some_and(|t| !t.is_finished(now))
    }

    pub fn is_rotating(&self, now: f64) -> bool {
        self.rotate_tween.as_ref().is_some_and(|t| !t.is_finished(now))
    }

    /// Moves (instantly when duration and delay are 0) to a position.
    pub fn move_to(&mut self, now: f64, x: f32, y: f32, z: f32, duration: f64, delay: f64, easing: Easing) {
        if duration > 0.0 || delay > 0.0 {
            let from = [self.position[0], self.position[1], self.position[2]];
            self.move_tween = Some(Tween::new(from, [x, y, z], now, delay, duration, easing));
        } else {
            self.move_tween = None;
            self.position[0] = x;
            self.position[1] = y;
            self.position[2] = z;
        }
    }

    /// Rotates (instantly when duration and delay are 0) to `deg` degrees.
    pub fn rotate_to(&mut self, now: f64, deg: f32, duration: f64, delay: f64, easing: Easing) {
        let target = deg.to_radians();
        if duration > 0.0 || delay > 0.0 {
            self.rotate_tween = Some(Tween::new([self.position[3]], [target], now, delay, duration, easing));
        } else {
            self.rotate_tween = None;
            self.position[3] = target;
        }
    }

    /// Moves and rotates to `p` with the same timing.
    pub fn go_to(&mut self, now: f64, p: CamPos, duration: f64, delay: f64, easing: Easing) {
        self.move_to(now, p.x, p.y, p.z, duration, delay, easing);
        self.rotate_to(now, p.zr_deg, duration, delay, easing);
    }

    /// Starts a looping sway (amplitudes x, y, z normalised; zr degrees).
    pub fn start_sway(&mut self, now: f64, rng: &mut Rng, amplitude: [f32; 4], duration: f64, easing: Easing) {
        let tween = self.next_sway_tween(now, rng, amplitude, duration, easing);
        self.sway = Some(Sway { amplitude, duration, easing, tween });
    }

    pub fn stop_sway(&mut self) {
        self.sway = None;
    }

    pub fn is_swaying(&self) -> bool {
        self.sway.is_some()
    }

    fn next_sway_tween(&self, now: f64, rng: &mut Rng, a: [f32; 4], duration: f64, easing: Easing) -> Tween<4> {
        let mut target = [0.0f32; 4];
        for i in 0..3 {
            let cur = (self.position[i] + self.offset[i]).clamp(0.0, 1.0);
            let lo = (cur - a[i]).max(0.0);
            let hi = (cur + a[i]).min(1.0);
            target[i] = lo + (hi - lo) * rng.f32() - cur;
        }
        let zr = a[3].to_radians();
        target[3] = -zr + rng.f32() * zr * 2.0;
        Tween::new(self.sway_offset, target, now, 0.0, duration, easing)
    }

    /// Advances animations and returns the world camera for a plane of the
    /// given size seen through a viewport with `view_aspect`.
    pub fn update(&mut self, now: f64, rng: &mut Rng, plane_w: f32, plane_h: f32, view_aspect: f32) -> CameraView {
        if let Some(t) = &self.move_tween {
            if t.has_started(now) {
                let v = t.value(now);
                self.position[0] = v[0];
                self.position[1] = v[1];
                self.position[2] = v[2];
            }
            if t.is_finished(now) {
                self.move_tween = None;
            }
        }
        if let Some(t) = &self.rotate_tween {
            if t.has_started(now) {
                self.position[3] = t.value(now)[0];
            }
            if t.is_finished(now) {
                self.rotate_tween = None;
            }
        }
        if let Some(s) = &self.sway {
            self.sway_offset = s.tween.value(now);
            if s.tween.is_finished(now) {
                let (a, d, e) = (s.amplitude, s.duration, s.easing);
                let next = self.next_sway_tween(now, rng, a, d.max(0.05), e);
                if let Some(s) = &mut self.sway {
                    s.tween = next;
                }
            }
        }
        let rot_now = self.with_offset[3];
        // Sway in x/y is scaled by the current zoom relative to the
        // maximum camera distance (closer camera -> smaller pan sway).
        let scale = self.with_offset[2] / max_distance(plane_w, plane_h, view_aspect, rot_now).max(1e-6);
        let p = &self.position;
        let o = &self.offset;
        let s = &self.sway_offset;
        self.with_offset = [
            (p[0] + o[0] + s[0] * scale).clamp(0.0, 1.0),
            (p[1] + o[1] + s[1] * scale).clamp(0.0, 1.0),
            (0.9 * (p[2] + o[2] + s[2]) + 0.1).clamp(0.0, 1.0),
            p[3] + o[3].to_radians() + s[3],
        ];
        let w = &self.with_offset;
        let position = world_position(plane_w, plane_h, view_aspect, w[0], w[1], w[2], w[3]);
        CameraView { position, rotation: w[3], fov_deg: FOV_DEG }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inner_rect_unrotated() {
        let (w, h) = inner_rect(1.0, 0.5, 0.0);
        assert!((w - 1.0).abs() < 1e-6 && (h - 0.5).abs() < 1e-6);
        let (w, h) = inner_rect(1.0, 1.0, std::f32::consts::FRAC_PI_4);
        assert!((w - h).abs() < 1e-4 && w < 1.0);
    }

    #[test]
    fn full_zoom_out_fits() {
        // 16:9 image in a 16:9 viewport at z = 1 shows the full image.
        let aspect = 16.0 / 9.0;
        let (pw, ph) = (1.0, 1.0 / aspect);
        let d = max_distance(pw, ph, aspect, 0.0);
        assert!((visible_height(d) - ph).abs() < 1e-4);
        let p = world_position(pw, ph, aspect, 0.5, 0.5, 1.0, 0.0);
        assert!(p[0].abs() < 1e-5 && p[1].abs() < 1e-5);
    }

    #[test]
    fn move_and_sway() {
        let mut rng = Rng::new(5);
        let mut c = Camera::default();
        c.move_to(0.0, 0.0, 0.0, 0.5, 1.0, 0.0, Easing::Linear);
        assert!(c.is_moving(0.5));
        c.update(0.5, &mut rng, 1.0, 0.6, 1.6);
        assert!((c.position().x - 0.25).abs() < 1e-5);
        c.update(2.0, &mut rng, 1.0, 0.6, 1.6);
        assert!(!c.is_moving(2.0));
        assert_eq!(c.position().z, 0.5);
        c.start_sway(2.0, &mut rng, [0.1, 0.1, 0.05, 2.0], 1.0, Easing::SineInOut);
        for i in 0..100 {
            let v = c.update(2.0 + i as f64 * 0.05, &mut rng, 1.0, 0.6, 1.6);
            assert!(v.position.iter().all(|x| x.is_finite()));
        }
    }
}
