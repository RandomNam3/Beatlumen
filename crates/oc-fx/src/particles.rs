//! Particle groups (drifting soft dots with random size/opacity flicker).
//!
//! Behaviour follows midori's particle system (MIT License, Copyright (c)
//! 2020 Benjamin Pang): particles live in a box slightly larger than the
//! image plane in front of it, drift linearly with wrap-around, sway as a
//! group and randomly walk their size, gradient and opacity.

use crate::easing::Easing;
use crate::rng::Rng;
use crate::tween::Tween;

/// Per-instance data uploaded to the GPU (9 floats).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
#[repr(C)]
pub struct ParticleInstance {
    pub position: [f32; 3],
    pub size: f32,
    pub gradient: f32,
    pub opacity: f32,
    pub color: [f32; 3],
}

#[derive(Debug, Clone, PartialEq)]
pub struct ParticleConfig {
    pub name: String,
    pub amount: usize,
    pub min_size: f32,
    pub max_size: f32,
    pub min_gradient: f32,
    pub max_gradient: f32,
    pub min_opacity: f32,
    pub max_opacity: f32,
    pub color: u32,
    pub smoothing: f32,
    pub speed: f32,
    pub angle_deg: f32,
    pub sway_distance: f32,
    pub sway_duration: f32,
    pub easing: Easing,
}

#[derive(Debug, Clone)]
struct Group {
    cfg: ParticleConfig,
    start: usize,
    color: [f32; 3],
    sway_offset: [f32; 2],
    sway: Tween<2>,
}

/// All particles of one scene.
#[derive(Debug, Clone, Default)]
pub struct Particles {
    width: f32,
    height: f32,
    max_depth: f32,
    groups: Vec<Group>,
    base: Vec<[f32; 3]>,
    size: Vec<f32>,
    gradient: Vec<f32>,
    opacity: Vec<f32>,
    instances: Vec<ParticleInstance>,
    step_acc: f64,
}

/// Attribute random walk rate (midori updates once per rendered frame; we
/// use a fixed 60 Hz step so behaviour does not depend on refresh rate).
const STEP: f64 = 1.0 / 60.0;

fn wrap(v: f32, size: f32) -> f32 {
    let half = size / 2.0;
    if size <= 0.0 {
        return v;
    }
    (v + half).rem_euclid(size) - half
}

fn random_averaged(rng: &mut Rng, cur: f32, min: f32, max: f32, smoothing: f32) -> f32 {
    let n = smoothing.min(0.95);
    let half = (max - min) / 2.0;
    let r = (cur + rng.range(-half, half)).clamp(min.min(max), max.max(min));
    (cur * n + r * (1.0 - n)).clamp(min.min(max), max.max(min))
}

impl Particles {
    /// `width`/`height`: box size (1.1 × plane size); `max_depth`: camera
    /// distance at full zoom-out (particles occupy the first quarter).
    pub fn new(width: f32, height: f32, max_depth: f32) -> Particles {
        Particles { width, height, max_depth, ..Default::default() }
    }

    pub fn configs(&self) -> Vec<ParticleConfig> {
        self.groups.iter().map(|g| g.cfg.clone()).collect()
    }

    /// Replaces all groups. Groups are keyed by name (a later group with the
    /// same name replaces an earlier one).
    pub fn generate(&mut self, now: f64, rng: &mut Rng, configs: &[ParticleConfig]) {
        let mut unique: Vec<ParticleConfig> = Vec::new();
        for c in configs {
            if let Some(slot) = unique.iter_mut().find(|u| u.name == c.name) {
                *slot = c.clone();
            } else {
                unique.push(c.clone());
            }
        }
        self.groups.clear();
        self.base.clear();
        self.size.clear();
        self.gradient.clear();
        self.opacity.clear();
        for cfg in unique {
            let start = self.base.len();
            let amount = cfg.amount.min(100_000);
            for _ in 0..amount {
                self.base.push([
                    -self.width / 2.0 + rng.f32() * self.width,
                    -self.height / 2.0 + rng.f32() * self.height,
                    self.max_depth / 4.0 * rng.f32(),
                ]);
                self.size.push(0.0);
                self.gradient.push(0.0);
                self.opacity.push(0.0);
            }
            let c = cfg.color;
            let color = [((c >> 16) & 255) as f32 / 255.0, ((c >> 8) & 255) as f32 / 255.0, (c & 255) as f32 / 255.0];
            let sway = Self::sway_tween(now, rng, [0.0, 0.0], &cfg);
            self.groups.push(Group { cfg, start, color, sway_offset: [0.0, 0.0], sway });
        }
        self.instances.resize(self.base.len(), ParticleInstance::default());
    }

    fn sway_tween(now: f64, rng: &mut Rng, from: [f32; 2], cfg: &ParticleConfig) -> Tween<2> {
        let d = cfg.sway_distance;
        Tween::new(from, [rng.range(-d, d), rng.range(-d, d)], now, 0.0, cfg.sway_duration.max(0.05) as f64, cfg.easing)
    }

    pub fn clear(&mut self) {
        self.groups.clear();
        self.base.clear();
        self.size.clear();
        self.gradient.clear();
        self.opacity.clear();
        self.instances.clear();
    }

    pub fn len(&self) -> usize {
        self.base.len()
    }

    pub fn is_empty(&self) -> bool {
        self.base.is_empty()
    }

    /// Advances by `dt` seconds and returns the instances to draw.
    pub fn update(&mut self, now: f64, dt: f64, rng: &mut Rng) -> &[ParticleInstance] {
        self.step_acc = (self.step_acc + dt).min(0.25);
        let mut steps = 0;
        while self.step_acc >= STEP {
            self.step_acc -= STEP;
            steps += 1;
        }
        for g in &mut self.groups {
            // Linear drift: `speed` normalised units per second along `angle`.
            let a = g.cfg.angle_deg.to_radians();
            let (vx, vy) = (g.cfg.speed * a.cos() * dt as f32, g.cfg.speed * a.sin() * dt as f32);
            for p in &mut self.base[g.start..g.start + g.cfg.amount.min(100_000)] {
                p[0] = wrap(p[0] + vx, self.width);
                p[1] = wrap(p[1] + vy, self.height);
            }
            g.sway_offset = g.sway.value(now);
            if g.sway.is_finished(now) {
                g.sway = Self::sway_tween(now, rng, g.sway_offset, &g.cfg);
            }
            let end = g.start + g.cfg.amount.min(100_000);
            for _ in 0..steps {
                for i in g.start..end {
                    self.size[i] = random_averaged(rng, self.size[i], g.cfg.min_size, g.cfg.max_size, g.cfg.smoothing);
                    self.gradient[i] = random_averaged(rng, self.gradient[i], g.cfg.min_gradient, g.cfg.max_gradient, g.cfg.smoothing);
                    self.opacity[i] = random_averaged(rng, self.opacity[i], g.cfg.min_opacity, g.cfg.max_opacity, g.cfg.smoothing);
                }
            }
            for i in g.start..end {
                let b = self.base[i];
                self.instances[i] = ParticleInstance {
                    position: [wrap(b[0] + g.sway_offset[0], self.width), wrap(b[1] + g.sway_offset[1], self.height), b[2]],
                    size: self.size[i],
                    gradient: self.gradient[i],
                    opacity: self.opacity[i],
                    color: g.color,
                };
            }
        }
        &self.instances
    }

    pub fn instances(&self) -> &[ParticleInstance] {
        &self.instances
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(name: &str, amount: usize) -> ParticleConfig {
        ParticleConfig {
            name: name.into(),
            amount,
            min_size: 2.0,
            max_size: 10.0,
            min_gradient: 0.0,
            max_gradient: 1.0,
            min_opacity: 0.0,
            max_opacity: 0.8,
            color: 0xff8000,
            smoothing: 0.5,
            speed: 0.1,
            angle_deg: 30.0,
            sway_distance: 0.02,
            sway_duration: 1.0,
            easing: Easing::SineInOut,
        }
    }

    #[test]
    fn particles_stay_in_box() {
        let mut rng = Rng::new(9);
        let mut p = Particles::new(1.1, 0.7, 1.5);
        p.generate(0.0, &mut rng, &[cfg("a", 100), cfg("b", 20), cfg("a", 50)]);
        assert_eq!(p.len(), 70);
        for f in 0..600 {
            let inst = p.update(f as f64 / 60.0, 1.0 / 60.0, &mut rng);
            for i in inst {
                assert!(i.position[0].abs() <= 0.551 && i.position[1].abs() <= 0.351);
                assert!(i.size >= 0.0 && i.size <= 10.0);
                assert!(i.opacity <= 0.8);
            }
        }
        assert!((p.instances()[0].color[0] - 1.0).abs() < 1e-6);
    }
}
