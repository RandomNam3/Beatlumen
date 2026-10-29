//! The effect runtime: turns a preset, the playback position and audio
//! analysis into a renderable [`FrameView`] every frame.
//!
//! Behaviour mirrors Hyperchroma 0.9: novelty
//! triggers animate filter parameters and camera shake, structural
//! transition peaks schedule camera moves and visual transitions, and
//! background image changes run a transition.
//!
//! Deliberate fixes compared to the original:
//! * smoothing is normalised to 60 fps (`s^(dt*60)`) so the response does
//!   not depend on the display refresh rate;
//! * camera rotation units are consistently degrees in random position
//!   generation;
//! * presets are not forced to a global blur of radius 1;
//! * the glitch re-seed interval is time based.

use crate::camera::{max_distance, CamPos, Camera, CameraView};
use crate::easing::Easing;
use crate::particles::{ParticleConfig, ParticleInstance, Particles};
use crate::preset::{Effect, Preset};
use crate::rng::Rng;
use crate::tween::Tween;
use bl_analysis::{TrackAnalysis, TRANSITION_MAJOR, TRANSITION_MINOR};
use bl_json::Value;

/// A background image known to the renderer.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImageRef {
    pub id: u64,
    /// width / height
    pub aspect: f32,
}

/// A global post-processing filter for this frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FilterView {
    Blur {
        radius: f32,
        passes: u32,
    },
    Bloom {
        opacity: f32,
        radius: f32,
        passes: u32,
    },
    RgbShift {
        amount: f32,
        angle: f32,
    },
    Vignette {
        offset: f32,
        darkness: f32,
    },
    VignetteBlur {
        size: f32,
        radius: f32,
        passes: u32,
    },
    Glitch {
        amount: f32,
        seed: f32,
    },
    /// Hue rotation in degrees.
    HueShift {
        degrees: f32,
    },
    Kaleidoscope {
        amount: f32,
        segments: u32,
        angle: f32,
    },
    Flash {
        amount: f32,
        color: [f32; 3],
    },
    ZoomBlur {
        strength: f32,
    },
    Grain {
        amount: f32,
        scanlines: f32,
        time: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionBlur {
    pub intensity: f32,
    pub samples: u32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TransitionKind {
    Blend,
    Blur {
        intensity: f32,
        samples: u32,
    },
    Wipe {
        direction: i32,
        gradient: f32,
        angle_deg: f32,
    },
    Slide {
        direction: i32,
        slides: i32,
        intensity: f32,
        samples: u32,
    },
    Glitch {
        seed: f32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TransitionView {
    pub kind: TransitionKind,
    /// Eased progress 0..1 (0 = previous scene, 1 = current scene).
    pub amount: f32,
    pub prev_amount: f32,
}

/// One background scene to render.
#[derive(Debug, Clone, Copy)]
pub struct SceneView<'a> {
    /// Stable id for the lifetime of the scene (renderer caches per scene).
    pub id: u64,
    pub image: Option<ImageRef>,
    /// Image plane size in world units (width is always 1).
    pub plane: [f32; 2],
    pub camera: CameraView,
    pub particles: &'a [ParticleInstance],
    pub motion_blur: Option<MotionBlur>,
}

/// Everything the renderer needs for one frame.
#[derive(Debug, Clone)]
pub struct FrameView<'a> {
    pub current: SceneView<'a>,
    /// Present while a visual transition is blending.
    pub previous: Option<SceneView<'a>>,
    pub transition: Option<TransitionView>,
    pub filters: Vec<FilterView>,
}

/// Per frame inputs.
#[derive(Debug, Clone, Copy)]
pub struct FrameInput<'a> {
    /// Real time since the previous frame in seconds.
    pub dt: f64,
    pub playing: bool,
    /// Playback position in seconds.
    pub position: f64,
    pub analysis: Option<&'a TrackAnalysis>,
    /// Fallback novelty (Lows, Mids, Highs) when no analysis is available.
    pub live_novelty: [f32; 3],
    /// RMS energy of the audio just played (fallback without analysis).
    pub live_energy: f32,
    /// Viewport width / height.
    pub view_aspect: f32,
    /// Blur radius requested by the UI (menus open), 0 when hidden.
    pub menu_blur: f32,
}

struct Scene {
    id: u64,
    image: Option<ImageRef>,
    camera: Camera,
    particles: Particles,
    motion_blur: Option<MotionBlur>,
    view: CameraView,
    sway_key: Option<String>,
}

impl Scene {
    fn plane(&self) -> [f32; 2] {
        let aspect = self
            .image
            .map(|i| i.aspect)
            .filter(|a| *a > 0.0)
            .unwrap_or(16.0 / 9.0);
        [1.0, 1.0 / aspect]
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum PosMode {
    None,
    Absolute,
    Relative,
}

/// Random camera target specification (ranges / offsets).
#[derive(Debug, Clone, Copy)]
struct PosSpec {
    mode: PosMode,
    easing: Easing,
    min: [f32; 4],
    max: [f32; 4],
    off: [f32; 4],
}

const BASE_KEYS: [[&str; 3]; 4] = [
    ["xMin", "xMax", "xOffset"],
    ["yMin", "yMax", "yOffset"],
    ["zMin", "zMax", "zOffset"],
    ["zrMin", "zrMax", "zrOffset"],
];
const FINISH_KEYS: [[&str; 3]; 4] = [
    ["finishXMin", "finishXMax", "finishXOffset"],
    ["finishYMin", "finishYMax", "finishYOffset"],
    ["finishZMin", "finishZMax", "finishZOffset"],
    ["finishZRMin", "finishZRMax", "finishZROffset"],
];
const DEFAULT_MIN: [f32; 4] = [0.0, 0.0, 0.8, 0.0];
const DEFAULT_MAX: [f32; 4] = [1.0, 1.0, 1.0, 0.0];

impl PosSpec {
    fn parse(v: &Value, type_key: &str, keys: &[[&str; 3]; 4], easing: Easing) -> PosSpec {
        let mode = match v.get(type_key).as_str() {
            Some("Absolute") => PosMode::Absolute,
            Some("Relative") => PosMode::Relative,
            None if type_key.is_empty() => PosMode::Absolute,
            _ => PosMode::None,
        };
        let mut s = PosSpec {
            mode,
            easing,
            min: DEFAULT_MIN,
            max: DEFAULT_MAX,
            off: [0.0; 4],
        };
        for i in 0..4 {
            s.min[i] = v.f32_or(keys[i][0], DEFAULT_MIN[i]);
            s.max[i] = v.f32_or(keys[i][1], DEFAULT_MAX[i]);
            s.off[i] = v.f32_or(keys[i][2], 0.0);
        }
        s
    }

    /// Generates a target position; `None` for mode None.
    fn generate(&self, rng: &mut Rng, current: CamPos, avoid: Option<CamPos>) -> Option<CamPos> {
        let cur = [current.x, current.y, current.z, current.zr_deg];
        let (lo, hi) = match self.mode {
            PosMode::None => return None,
            PosMode::Absolute => (self.min, self.max),
            PosMode::Relative => {
                let mut lo = [0.0; 4];
                let mut hi = [0.0; 4];
                for i in 0..4 {
                    if i < 3 {
                        lo[i] = (cur[i] - self.off[i]).max(0.0);
                        hi[i] = (cur[i] + self.off[i]).min(1.0);
                    } else {
                        lo[i] = cur[i] - self.off[i];
                        hi[i] = cur[i] + self.off[i];
                    }
                }
                (lo, hi)
            }
        };
        let reference = match self.mode {
            PosMode::Relative => Some(current),
            _ => avoid,
        };
        let mut out = [0.0f32; 4];
        for _ in 0..100 {
            for i in 0..4 {
                let (a, b) = if lo[i] <= hi[i] {
                    (lo[i], hi[i])
                } else {
                    (hi[i], lo[i])
                };
                out[i] = a + (b - a) * rng.f32();
            }
            let Some(r) = reference else { break };
            let rv = [r.x, r.y, r.z, r.zr_deg];
            let too_close = (0..4).any(|i| {
                let width = (hi[i] - lo[i]).abs();
                width > 0.0 && (out[i] - rv[i]).abs() < width / 4.0
            });
            if !too_close {
                break;
            }
        }
        Some(CamPos {
            x: out[0],
            y: out[1],
            z: out[2],
            zr_deg: out[3],
        })
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct FilterState {
    a: f32,
    b: f32,
    passes: u32,
}

struct ActiveTransition {
    kind: TransitionKind,
    tween: Tween<1>,
    prev_amount: f32,
}

struct Scheduled {
    effect_id: String,
    duration: f64,
    camera_delay: f64,
    start: f64,
}

/// A deferred camera switch: jump at `at`, then optionally glide.
struct PendingSwitch {
    at: f64,
    enter: CamPos,
    finish: Option<(PosSpec, f64)>,
}

pub struct Engine {
    rng: Rng,
    now: f64,
    preset: Option<Preset>,
    scene: Scene,
    old: Option<Scene>,
    transition: Option<ActiveTransition>,
    chain: Vec<String>,
    filters: Vec<(String, FilterState)>,
    shake: [f32; 4],
    rgb_angle: f32,
    glitch_seed: f32,
    glitch_timer: f64,
    scheduled: Option<Scheduled>,
    pending_switch: Option<PendingSwitch>,
    last_position: f64,
    next_scene_id: u64,
    view_aspect: f32,
    particle_key: Vec<ParticleConfig>,
    /// Global strength multiplier for audio driven effects.
    intensity: f32,
    /// Scale effect strength with the loudness of the current section.
    adaptive: bool,
    /// Slowly decaying loudness peak used to normalise the energy.
    energy_peak: f32,
    /// Smoothed, normalised loudness of the current section (0..1).
    energy_level: f32,
    /// Per-track novelty scales and the analysis they belong to.
    nov_scale: [f32; 3],
    nov_key: (usize, usize),
    /// Analysis time read last frame (novelty peaks in between count).
    nov_read: Option<f64>,
}

/// Visuals are computed this much ahead of the audible position: the frame
/// reaches the screen about one refresh later.
const DISPLAY_LEAD: f64 = 0.02;

fn easing_of(v: &Value, key: &str, default: Easing) -> Easing {
    v.get(key)
        .as_str()
        .and_then(Easing::from_name)
        .unwrap_or(default)
}

fn weighted_pick<'a>(rng: &mut Rng, candidates: &[&'a Effect]) -> Option<&'a Effect> {
    let total: f32 = candidates.iter().map(|e| e.trig("weight").max(0.0)).sum();
    if total <= 0.0 {
        return None;
    }
    let r = rng.f32() * total;
    let mut acc = 0.0;
    for e in candidates {
        acc += e.trig("weight").max(0.0);
        if r < acc {
            return Some(e);
        }
    }
    None
}

fn lerp(a: f64, b: f64, t: f64) -> f64 {
    a + (b - a) * t
}

impl Engine {
    pub fn new(seed: u64) -> Engine {
        let mut e = Engine {
            rng: Rng::new(seed),
            now: 0.0,
            preset: None,
            scene: Scene {
                id: 0,
                image: None,
                camera: Camera::default(),
                particles: Particles::default(),
                motion_blur: None,
                view: CameraView {
                    position: [0.0, 0.0, 1.0],
                    rotation: 0.0,
                    fov_deg: 35.0,
                },
                sway_key: None,
            },
            old: None,
            transition: None,
            chain: Vec::new(),
            filters: Vec::new(),
            shake: [0.0; 4],
            rgb_angle: 0.0,
            glitch_seed: 0.5,
            glitch_timer: 0.0,
            scheduled: None,
            pending_switch: None,
            last_position: 0.0,
            next_scene_id: 1,
            view_aspect: 16.0 / 9.0,
            particle_key: Vec::new(),
            intensity: 1.0,
            adaptive: false,
            energy_peak: 0.0,
            energy_level: 0.5,
            nov_scale: [1.0; 3],
            nov_key: (0, 0),
            nov_read: None,
        };
        e.scene = e.new_scene(None, CamPos::default());
        e
    }

    /// Sets the global effect intensity (0 = still, 1 = as designed, 2 = double).
    pub fn set_intensity(&mut self, intensity: f32) {
        self.intensity = intensity.clamp(0.0, 2.0);
    }

    /// Enables adapting the effect strength to the loudness of the current
    /// section of the track (quiet parts calmer, drops stronger).
    pub fn set_adaptive(&mut self, on: bool) {
        self.adaptive = on;
    }

    /// Current audio driven filter levels (type, primary amount), for
    /// diagnostics and timing tests.
    pub fn filter_levels(&self) -> Vec<(String, f32)> {
        self.filters.iter().map(|(t, s)| (t.clone(), s.a)).collect()
    }

    /// Smoothed loudness of the current section (0..1), for diagnostics.
    pub fn energy_level(&self) -> f32 {
        self.energy_level
    }

    /// Trigger gain from intensity and (when adaptive) section loudness.
    fn gain(&self) -> f32 {
        let adapt = if self.adaptive {
            0.45 + 0.85 * self.energy_level
        } else {
            1.0
        };
        self.intensity * adapt
    }

    fn update_energy(&mut self, input: &FrameInput, dt: f64) {
        let e = match input.analysis {
            Some(a) if !a.energy.is_empty() => a
                .energy
                .get(a.frame_at(input.position))
                .copied()
                .unwrap_or(0.0),
            _ => input.live_energy,
        };
        let e = if e.is_finite() { e.max(0.0) } else { 0.0 };
        // Peak follower with a ~20 s half-life normalises the loudness.
        self.energy_peak = (self.energy_peak * 0.5f32.powf(dt as f32 / 20.0))
            .max(e)
            .max(1e-4);
        let target = (e / self.energy_peak).clamp(0.0, 1.0);
        let k = 1.0 - (-(dt as f32) / 1.5).exp();
        self.energy_level += (target - self.energy_level) * k;
    }

    pub fn preset(&self) -> Option<&Preset> {
        self.preset.as_ref()
    }

    pub fn current_image(&self) -> Option<ImageRef> {
        self.scene.image
    }

    /// Images referenced by live scenes (renderer may free others).
    pub fn live_images(&self) -> Vec<u64> {
        let mut v: Vec<u64> = self.scene.image.iter().map(|i| i.id).collect();
        if let Some(o) = &self.old {
            if let Some(i) = o.image {
                v.push(i.id);
            }
        }
        v
    }

    pub fn is_transitioning(&self) -> bool {
        self.transition.is_some()
    }

    fn new_scene(&mut self, image: Option<ImageRef>, pos: CamPos) -> Scene {
        let id = self.next_scene_id;
        self.next_scene_id += 1;
        let mut s = Scene {
            id,
            image,
            camera: Camera::new(pos),
            particles: Particles::default(),
            motion_blur: None,
            view: CameraView {
                position: [0.0, 0.0, 1.0],
                rotation: 0.0,
                fov_deg: 35.0,
            },
            sway_key: None,
        };
        let [pw, ph] = s.plane();
        s.particles = Particles::new(
            1.1 * pw,
            1.1 * ph,
            max_distance(pw, ph, self.view_aspect, 0.0),
        );
        s
    }

    fn enabled(&self) -> impl Iterator<Item = &Effect> {
        self.preset
            .iter()
            .flat_map(|p| p.effects.iter())
            .filter(|e| !e.disabled)
    }

    fn last_enabled(&self, effect_type: &str, ty: &str) -> Option<&Effect> {
        self.enabled()
            .filter(|e| e.effect_type == effect_type && e.ty == ty)
            .last()
    }

    fn particle_configs(&self) -> Vec<ParticleConfig> {
        self.enabled()
            .filter(|e| e.effect_type == "Particle")
            .map(|e| ParticleConfig {
                name: e.name.clone(),
                amount: e.cfg("amount").max(0.0) as usize,
                min_size: e.cfg("minSize"),
                max_size: e.cfg("maxSize"),
                min_gradient: e.cfg("minGradient"),
                max_gradient: e.cfg("maxGradient"),
                min_opacity: e.cfg("minOpacity"),
                max_opacity: e.cfg("maxOpacity"),
                color: e.cfg("color").max(0.0) as u32,
                smoothing: e.cfg("smoothing"),
                speed: e.cfg("speed"),
                angle_deg: e.cfg("angle"),
                sway_distance: e.cfg("swayDistance"),
                sway_duration: e.cfg("swayDuration"),
                easing: easing_of(&e.config, "easing", Easing::Linear),
            })
            .collect()
    }

    fn sway_key(&self) -> Option<String> {
        self.last_enabled("Camera", "Sway")
            .map(|e| e.config.to_json())
    }

    /// Applies camera sway, particles and motion blur to a scene.
    fn apply_scene_effects(&mut self, which_old: bool, force: bool) {
        let configs = self.particle_configs();
        let sway = self.last_enabled("Camera", "Sway").map(|e| {
            (
                [e.cfg("x"), e.cfg("y"), e.cfg("z"), e.cfg("zr")],
                e.cfg("duration") as f64,
                easing_of(&e.config, "easing", Easing::Linear),
            )
        });
        let sway_key = self.sway_key();
        let shake_enabled = self.last_enabled("Camera", "Shake").is_some();
        let mb = self
            .last_enabled("Filter", "MotionBlur")
            .map(|e| MotionBlur {
                intensity: e.cfg("intensity"),
                samples: e.cfg("samples").max(0.0) as u32,
            });
        let now = self.now;
        let particles_changed = force || configs != self.particle_key;
        let mut rng = self.rng.clone();
        let scene = if which_old {
            self.old.as_mut()
        } else {
            Some(&mut self.scene)
        };
        if let Some(scene) = scene {
            if force || scene.sway_key != sway_key {
                scene.camera.stop_sway();
                if let Some((amp, duration, easing)) = sway {
                    scene
                        .camera
                        .start_sway(now, &mut rng, amp, duration.max(0.05), easing);
                }
                scene.sway_key = sway_key;
            }
            if !shake_enabled {
                scene.camera.offset = [0.0; 4];
            }
            if particles_changed {
                scene.particles.generate(now, &mut rng, &configs);
            }
            scene.motion_blur = mb;
        }
        self.rng = rng;
        if !which_old {
            self.particle_key = configs;
        }
        if !shake_enabled {
            self.shake = [0.0; 4];
        }
    }

    /// Loads or updates the active preset (call again after edits).
    pub fn set_preset(&mut self, preset: Option<Preset>) {
        self.preset = preset;
        // Global filter chain in order of first occurrence.
        let mut chain: Vec<String> = Vec::new();
        let mut states: Vec<(String, FilterState)> = Vec::new();
        let filters: Vec<Effect> = self
            .enabled()
            .filter(|e| e.effect_type == "Filter")
            .cloned()
            .collect();
        for e in &filters {
            let st = match e.ty.as_str() {
                "Blur" => FilterState {
                    a: e.cfg("radius"),
                    b: 0.0,
                    passes: e.cfg("passes") as u32,
                },
                "Bloom" => FilterState {
                    a: e.cfg("opacity"),
                    b: e.cfg("radius"),
                    passes: e.cfg("passes") as u32,
                },
                "RgbShift" => FilterState {
                    a: e.cfg("amount"),
                    b: 0.0,
                    passes: 0,
                },
                "Vignette" => FilterState {
                    a: e.cfg("darkness"),
                    b: e.cfg("offset"),
                    passes: 0,
                },
                "VignetteBlur" => FilterState {
                    a: e.cfg("size"),
                    b: e.cfg("radius"),
                    passes: e.cfg("passes") as u32,
                },
                "Glitch" => FilterState {
                    a: e.cfg("amount"),
                    b: 0.0,
                    passes: 0,
                },
                "HueShift" => FilterState {
                    a: e.cfg("degrees"),
                    b: 0.0,
                    passes: 0,
                },
                "Kaleidoscope" => FilterState {
                    a: e.cfg("amount"),
                    b: 0.0,
                    passes: e.cfg("segments").max(2.0) as u32,
                },
                "Flash" => FilterState {
                    a: e.cfg("amount"),
                    b: 0.0,
                    passes: 0,
                },
                "ZoomBlur" => FilterState {
                    a: e.cfg("strength"),
                    b: 0.0,
                    passes: 0,
                },
                "Grain" => FilterState {
                    a: e.cfg("amount"),
                    b: e.cfg("scanlines"),
                    passes: 0,
                },
                "MotionBlur" => FilterState {
                    a: e.cfg("intensity"),
                    b: 0.0,
                    passes: e.cfg("samples") as u32,
                },
                _ => continue,
            };
            if e.ty == "RgbShift" {
                self.rgb_angle = e.cfg("angle");
            }
            if e.ty != "MotionBlur" && !chain.contains(&e.ty) {
                chain.push(e.ty.clone());
            }
            if let Some(slot) = states.iter_mut().find(|(t, _)| *t == e.ty) {
                slot.1 = st;
            } else {
                states.push((e.ty.clone(), st));
            }
        }
        self.chain = chain;
        self.filters = states;
        self.glitch_seed = self.rng.f32();
        self.apply_scene_effects(false, false);
        if self.old.is_some() {
            self.apply_scene_effects(true, false);
        }
    }

    /// Changes the background image, running a transition when `animate`
    /// is set and the preset has an eligible Major visual transition.
    pub fn set_background(&mut self, image: Option<ImageRef>, animate: bool) {
        self.finish_transition();
        if animate && self.scene.image.is_some() {
            let candidates: Vec<Effect> = self
                .enabled()
                .filter(|e| {
                    e.effect_type == "Transition"
                        && !e.is_camera_transition()
                        && e.trigger_type() == "Major"
                })
                .cloned()
                .collect();
            let refs: Vec<&Effect> = candidates.iter().collect();
            let mut rng = self.rng.clone();
            let picked = weighted_pick(&mut rng, &refs).cloned();
            self.rng = rng;
            if let Some(e) = picked {
                self.start_visual(&e, 1.1, 0.0, image);
                return;
            }
        }
        let pos = self.scene.camera.position();
        let mut scene = self.new_scene(
            image,
            if self.scene.image.is_some() {
                pos
            } else {
                CamPos::default()
            },
        );
        std::mem::swap(&mut self.scene, &mut scene);
        self.apply_scene_effects(false, true);
    }

    fn finish_transition(&mut self) {
        self.transition = None;
        self.old = None;
    }

    /// Clears any scheduled transition (e.g. when a new track is loaded).
    pub fn reset_schedule(&mut self) {
        self.scheduled = None;
    }

    fn start_visual(&mut self, e: &Effect, duration: f64, delay: f64, image: Option<ImageRef>) {
        let c = &e.config;
        let kind = match e.ty.as_str() {
            "Blend" => TransitionKind::Blend,
            "Blur" => TransitionKind::Blur {
                intensity: e.cfg("intensity"),
                samples: e.cfg("samples").max(0.0) as u32,
            },
            "Wipe" => TransitionKind::Wipe {
                direction: e.cfg("direction") as i32,
                gradient: e.cfg("gradient"),
                angle_deg: e.cfg("angle"),
            },
            "Slide" => TransitionKind::Slide {
                direction: e.cfg("direction") as i32,
                slides: e.cfg("slides") as i32,
                intensity: e.cfg("intensity"),
                samples: e.cfg("samples").max(0.0) as u32,
            },
            "Glitch" => TransitionKind::Glitch {
                seed: self.rng.f32(),
            },
            _ => return,
        };
        let easing = easing_of(c, "easing", Easing::CubicOut);
        let cam = &e.camera;
        let exit = PosSpec::parse(
            cam.get("exit"),
            "type",
            &BASE_KEYS,
            easing_of(cam.get("exit"), "easing", Easing::CubicOut),
        );
        let enter = PosSpec::parse(cam.get("enter"), "", &BASE_KEYS, Easing::Linear);
        let finish = PosSpec::parse(
            cam.get("finish"),
            "type",
            &BASE_KEYS,
            easing_of(cam.get("finish"), "easing", Easing::CubicIn),
        );
        let now = self.now;
        let current = self.scene.camera.position();
        let enter_pos = enter
            .generate(&mut self.rng, current, None)
            .unwrap_or(current);
        let mut next = self.new_scene(image.or(self.scene.image), enter_pos);
        if let Some(t) = exit.generate(&mut self.rng, current, Some(current)) {
            self.scene
                .camera
                .go_to(now, t, delay + duration, 0.0, exit.easing);
        }
        if let Some(t) = finish.generate(&mut self.rng, enter_pos, Some(enter_pos)) {
            next.camera.go_to(now, t, duration, delay, finish.easing);
        }
        std::mem::swap(&mut self.scene, &mut next);
        self.old = Some(next);
        self.apply_scene_effects(false, true);
        self.transition = Some(ActiveTransition {
            kind,
            tween: Tween::new([0.0], [1.0], now, delay, duration, easing),
            prev_amount: 0.0,
        });
    }

    fn execute(&mut self, e: &Effect, duration: f64, camera_delay: f64) {
        let now = self.now;
        let current = self.scene.camera.position();
        let easing = easing_of(&e.config, "easing", Easing::CubicOut);
        match e.ty.as_str() {
            "CameraMove" => {
                let spec = PosSpec::parse(&e.config, "type", &BASE_KEYS, easing);
                let mut rng = self.rng.clone();
                if let Some(t) = spec.generate(&mut rng, current, Some(current)) {
                    self.scene.camera.go_to(now, t, duration, 0.0, easing);
                }
                self.rng = rng;
            }
            "CameraSwitch" => {
                let spec = PosSpec::parse(&e.config, "type", &BASE_KEYS, easing);
                let finish = PosSpec::parse(&e.config, "finishType", &FINISH_KEYS, easing);
                let mut rng = self.rng.clone();
                if let Some(enter) = spec.generate(&mut rng, current, Some(current)) {
                    self.pending_switch = Some(PendingSwitch {
                        at: now + duration / 2.0,
                        enter,
                        finish: (finish.mode != PosMode::None).then_some((finish, duration / 2.0)),
                    });
                }
                self.rng = rng;
            }
            _ => self.start_visual(e, duration, camera_delay, None),
        }
    }

    /// Novelty per band since the last frame: the strongest analysis value
    /// between the previous and the current position (short peaks are not
    /// skipped at low frame rates), scaled to the track.
    fn novelty(&mut self, input: &FrameInput) -> [f32; 3] {
        match input.analysis {
            Some(a) if !a.novelty.is_empty() => {
                let key = (a as *const TrackAnalysis as usize, a.frame_count());
                if key != self.nov_key {
                    self.nov_key = key;
                    let s = a.novelty_scales();
                    self.nov_scale = [0, 1, 2].map(|i| s.get(i).copied().unwrap_or(1.0));
                    self.nov_read = None;
                }
                let t = input.position + DISPLAY_LEAD;
                let f1 = a.novelty_frame_at(t);
                // The current analysis frame plus any skipped since the last
                // render frame (moving forward normally).
                let f0 = match self.nov_read {
                    Some(prev) if prev <= t && t - prev < 0.25 => {
                        (a.novelty_frame_at(prev) + 1).min(f1)
                    }
                    _ => f1,
                };
                self.nov_read = Some(t);
                let mut out = [0.0; 3];
                for (i, o) in out.iter_mut().enumerate() {
                    if let Some(c) = a.novelty.get(i) {
                        let m = (f0..=f1)
                            .filter_map(|f| c.get(f))
                            .fold(0.0f32, |m, v| m.max(*v));
                        *o = (m * self.nov_scale[i]).clamp(0.0, 1.0);
                    }
                }
                out
            }
            _ => input.live_novelty.map(|v| v.clamp(0.0, 1.0)),
        }
    }

    /// Config value of the last enabled filter of type `ty`.
    fn filter_cfg(&self, ty: &str, key: &str) -> f32 {
        self.last_enabled("Filter", ty)
            .map(|e| e.cfg(key))
            .unwrap_or(0.0)
    }

    fn filter_state(&mut self, ty: &str) -> Option<&mut FilterState> {
        self.filters
            .iter_mut()
            .find(|(t, _)| t == ty)
            .map(|(_, s)| s)
    }

    fn run_triggers(&mut self, input: &FrameInput) {
        let nov = self.novelty(input);
        let dt = input.dt.clamp(0.0, 0.25);
        let frames = (dt * 60.0) as f32;
        let gain = self.gain();
        let effects: Vec<Effect> = self
            .enabled()
            .filter(|e| e.effect_type == "Camera" || e.effect_type == "Filter")
            .cloned()
            .collect();
        for e in &effects {
            let band = match e.trigger_type() {
                "Lows" => Some(0),
                "Mids" => Some(1),
                "Highs" => Some(2),
                _ => None,
            };
            let v = band.map(|b| nov[b]).unwrap_or(0.0);
            let threshold = e.trig("threshold");
            let y =
                ((if v >= threshold { v } else { 0.0 }) * e.trig("sensitivity") * gain).min(1.0);
            // Fast attack (a hit shows at once), configured smoothing only
            // for the release.
            let s = e.trig("smoothing").clamp(0.0, 1.0).powf(frames);
            let attack = 0.3f32.powf(frames);
            let smooth = |prev: f32, target: f32| {
                let k = if target.abs() > prev.abs() {
                    attack.min(s)
                } else {
                    s
                };
                prev * k + target * (1.0 - k)
            };
            let lerp_t = |key: &str| e.cfg(key) + (e.trig(key) - e.cfg(key)) * y;
            match (e.effect_type.as_str(), e.ty.as_str()) {
                ("Camera", "Shake") if band.is_some() => {
                    let k = self.intensity.min(1.5);
                    let amp = [
                        e.cfg("x") * k,
                        e.cfg("y") * k,
                        e.cfg("z") * k,
                        e.cfg("zr") * k,
                    ];
                    for (i, a) in amp.iter().enumerate() {
                        let target = if y == 0.0 {
                            0.0
                        } else {
                            self.rng.range(-a, *a) * y
                        };
                        self.shake[i] = smooth(self.shake[i], target);
                    }
                    self.scene.camera.offset = self.shake;
                }
                ("Filter", "Glitch") => {
                    let freq = e.cfg("frequency").clamp(1.0, 60.0) as f64;
                    self.glitch_timer += dt;
                    if self.glitch_timer >= 1.0 / freq {
                        self.glitch_timer = 0.0;
                        self.glitch_seed = self.rng.f32();
                    }
                    if band.is_some() {
                        let t = lerp_t("amount");
                        if let Some(st) = self.filter_state("Glitch") {
                            st.a = smooth(st.a, t);
                        }
                    }
                }
                ("Filter", ty) if band.is_some() => {
                    let (ka, kb) = match ty {
                        "Blur" => ("radius", ""),
                        "Bloom" => ("opacity", "radius"),
                        "RgbShift" => ("amount", ""),
                        "Vignette" => ("darkness", "offset"),
                        "VignetteBlur" => ("size", "radius"),
                        "MotionBlur" => ("intensity", ""),
                        "HueShift" => ("degrees", ""),
                        "Kaleidoscope" => ("amount", ""),
                        "Flash" => ("amount", ""),
                        "ZoomBlur" => ("strength", ""),
                        "Grain" => ("amount", ""),
                        _ => continue,
                    };
                    let ta = lerp_t(ka);
                    let tb = if kb.is_empty() { 0.0 } else { lerp_t(kb) };
                    if ty == "RgbShift" {
                        let target = if y == 0.0 {
                            e.cfg("angle")
                        } else {
                            self.rng.range(0.0, 360.0)
                        };
                        let s_angle = 0.95f32.powf(frames);
                        self.rgb_angle = self.rgb_angle * s_angle + target * (1.0 - s_angle);
                    }
                    if let Some(st) = self.filter_state(ty) {
                        st.a = smooth(st.a, ta);
                        if !kb.is_empty() {
                            st.b = smooth(st.b, tb);
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(st) = self
            .filters
            .iter()
            .find(|(t, _)| t == "MotionBlur")
            .map(|(_, s)| *s)
        {
            if let Some(mb) = &mut self.scene.motion_blur {
                mb.intensity = st.a;
            }
        }
    }

    fn run_scheduler(&mut self, input: &FrameInput) {
        let Some(analysis) = input.analysis else {
            return;
        };
        let p = input.position;
        let busy = self.transition.is_some()
            || self.scene.camera.is_moving(self.now)
            || self.pending_switch.is_some();
        let tr = analysis.transition_time(1).max(1e-3);
        let er = 1.0 / analysis.frame_rate();
        if let Some(s) = &self.scheduled {
            if !busy {
                if s.start < p && p <= s.start + tr {
                    let (id, d, f) = (s.effect_id.clone(), s.duration, s.camera_delay);
                    self.scheduled = None;
                    if let Some(e) = self
                        .preset
                        .as_ref()
                        .and_then(|pr| pr.find_effect(&id))
                        .cloned()
                    {
                        self.execute(&e, d, f);
                    }
                } else if p < self.last_position || p > s.start + tr {
                    self.scheduled = None;
                }
            }
            return;
        }
        if busy {
            return;
        }
        let transitions: Vec<Effect> = self
            .enabled()
            .filter(|e| e.effect_type == "Transition")
            .cloned()
            .collect();
        if transitions.is_empty() || analysis.transitions.is_empty() {
            return;
        }
        let t0 = analysis.transition_frame_at(p);
        let count = (5.0 / tr).ceil() as usize;
        let e0 = analysis.frame_at(p);
        for g in 0..count {
            let Some(&peak) = analysis.transitions.get(t0 + g) else {
                break;
            };
            let h = g as f64 * tr;
            if peak == 0.0 || h < 0.4 {
                continue;
            }
            let kind = if peak == TRANSITION_MAJOR {
                "Major"
            } else if peak == TRANSITION_MINOR {
                "Minor"
            } else {
                continue;
            };
            let energy = analysis
                .energy
                .get(e0 + (g as f64 * tr / er) as usize)
                .copied()
                .unwrap_or(0.0);
            let candidates: Vec<&Effect> = transitions
                .iter()
                .filter(|e| e.trigger_type() == kind && energy >= e.trig("threshold"))
                .collect();
            let mut rng = self.rng.clone();
            let picked = weighted_pick(&mut rng, &candidates).cloned();
            self.rng = rng;
            if let Some(e) = picked {
                let r = lerp((h - 0.2).min(2.0), 0.2, e.cfg("speed") as f64);
                let f = lerp((h - r).min(1.0), 0.2, e.camera.f64_or("speed", 0.0));
                let f = f.max(0.0);
                let start = if e.is_camera_transition() {
                    p + h - r / 2.0
                } else {
                    p + h - r / 2.0 - f
                };
                self.scheduled = Some(Scheduled {
                    effect_id: e.id.clone(),
                    duration: r.max(0.05),
                    camera_delay: f,
                    start,
                });
                break;
            }
        }
    }

    /// Advances the engine and returns what to render.
    pub fn update(&mut self, input: &FrameInput) -> FrameView<'_> {
        let dt = input.dt.clamp(0.0, 0.5);
        self.now += dt;
        if input.view_aspect.is_finite() && input.view_aspect > 0.0 {
            self.view_aspect = input.view_aspect;
        }
        if input.playing {
            self.update_energy(input, dt);
            self.run_triggers(input);
            self.run_scheduler(input);
        }
        self.last_position = input.position;

        // Deferred camera switch.
        if let Some(ps) = &self.pending_switch {
            if self.now >= ps.at {
                let (enter, finish) = (ps.enter, ps.finish);
                self.pending_switch = None;
                let now = self.now;
                self.scene
                    .camera
                    .go_to(now, enter, 0.0, 0.0, Easing::Linear);
                if let Some((spec, d)) = finish {
                    let mut rng = self.rng.clone();
                    if let Some(t) = spec.generate(&mut rng, enter, Some(enter)) {
                        self.scene.camera.go_to(now, t, d, 0.0, spec.easing);
                    }
                    self.rng = rng;
                }
            }
        }

        // Transition progress.
        let mut transition_view = None;
        let mut blending = false;
        if let Some(t) = &mut self.transition {
            if t.tween.is_finished(self.now) {
                self.transition = None;
                self.old = None;
            } else if t.tween.has_started(self.now) {
                let amount = t.tween.value(self.now)[0];
                transition_view = Some(TransitionView {
                    kind: t.kind,
                    amount,
                    prev_amount: t.prev_amount,
                });
                t.prev_amount = amount;
                blending = true;
            }
        }

        let now = self.now;
        let aspect = self.view_aspect;
        let mut rng = self.rng.clone();
        for scene in std::iter::once(&mut self.scene).chain(self.old.iter_mut()) {
            let [pw, ph] = scene.plane();
            scene.view = scene.camera.update(now, &mut rng, pw, ph, aspect);
            scene.particles.update(now, dt, &mut rng);
        }
        self.rng = rng;

        let mut filters = Vec::with_capacity(self.chain.len() + 1);
        let mut has_blur = false;
        for ty in &self.chain {
            let Some(st) = self.filters.iter().find(|(t, _)| t == ty).map(|(_, s)| *s) else {
                continue;
            };
            filters.push(match ty.as_str() {
                "Blur" => {
                    has_blur = true;
                    FilterView::Blur {
                        radius: st.a.max(input.menu_blur),
                        passes: st.passes,
                    }
                }
                "Bloom" => FilterView::Bloom {
                    opacity: st.a,
                    radius: st.b,
                    passes: st.passes,
                },
                "RgbShift" => FilterView::RgbShift {
                    amount: st.a,
                    angle: self.rgb_angle.to_radians(),
                },
                "Vignette" => FilterView::Vignette {
                    darkness: st.a,
                    offset: st.b,
                },
                "VignetteBlur" => FilterView::VignetteBlur {
                    size: st.a,
                    radius: st.b,
                    passes: st.passes,
                },
                "Glitch" => FilterView::Glitch {
                    amount: st.a,
                    seed: self.glitch_seed,
                },
                "HueShift" => {
                    let speed = self.filter_cfg("HueShift", "speed");
                    FilterView::HueShift {
                        degrees: st.a + (self.now as f32 * speed * 60.0) % 360.0,
                    }
                }
                "Kaleidoscope" => {
                    let rot = self.filter_cfg("Kaleidoscope", "rotation");
                    FilterView::Kaleidoscope {
                        amount: st.a.clamp(0.0, 1.0),
                        segments: st.passes.max(2),
                        angle: (self.now as f32 * rot) % std::f32::consts::TAU,
                    }
                }
                "Flash" => {
                    let c = self.filter_cfg("Flash", "color") as u32;
                    let rgb = [
                        ((c >> 16) & 255) as f32 / 255.0,
                        ((c >> 8) & 255) as f32 / 255.0,
                        (c & 255) as f32 / 255.0,
                    ];
                    FilterView::Flash {
                        amount: st.a.clamp(0.0, 0.6),
                        color: rgb,
                    }
                }
                "ZoomBlur" => FilterView::ZoomBlur {
                    strength: st.a.clamp(0.0, 0.5),
                },
                "Grain" => FilterView::Grain {
                    amount: st.a.clamp(0.0, 0.5),
                    scanlines: st.b.clamp(0.0, 1.0),
                    time: (self.now % 1000.0) as f32,
                },
                _ => continue,
            });
        }
        if !has_blur && input.menu_blur > 0.001 {
            filters.push(FilterView::Blur {
                radius: input.menu_blur,
                passes: 1,
            });
        }

        fn view_of(s: &Scene) -> SceneView<'_> {
            SceneView {
                id: s.id,
                image: s.image,
                plane: s.plane(),
                camera: s.view,
                particles: s.particles.instances(),
                motion_blur: s.motion_blur,
            }
        }
        // While a visual transition waits for its camera delay, the old
        // scene is still the one on screen.
        let (current, previous) = match (&self.old, blending) {
            (Some(old), true) => (view_of(&self.scene), Some(view_of(old))),
            (Some(old), false) => (view_of(old), None),
            (None, _) => (view_of(&self.scene), None),
        };
        FrameView {
            current,
            previous,
            transition: transition_view,
            filters,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::preset;

    /// One bass hit per second, each a single analysis frame long.
    fn analysis_with_hits() -> TrackAnalysis {
        let frames = 48000 * 20 / 2048;
        let hit = |f: usize| f % 23 == 11;
        TrackAnalysis {
            sample_rate: 48000,
            hop_size: 2048,
            energy: vec![0.5; frames],
            novelty: vec![
                (0..frames)
                    .map(|f| {
                        if hit(f) {
                            0.5
                        } else if f % 3 == 0 {
                            0.005
                        } else {
                            0.0
                        }
                    })
                    .collect(),
                vec![0.0; frames],
                vec![0.0; frames],
            ],
            transitions: vec![0.0; frames.div_ceil(10)],
            transition_down_sampling: 10,
        }
    }

    /// Bloom level per rendered frame at `fps` with the Balanced preset.
    fn bloom_levels(fps: f64) -> Vec<(f64, f32)> {
        let a = analysis_with_hits();
        let mut rng = Rng::new(3);
        let mut e = Engine::new(3);
        e.set_preset(preset::builtin("Balanced", &mut rng));
        let mut out = Vec::new();
        let mut t = 0.0;
        while t < 19.0 {
            let input = FrameInput {
                dt: 1.0 / fps,
                playing: true,
                position: t,
                analysis: Some(&a),
                live_novelty: [0.0; 3],
                live_energy: 0.0,
                view_aspect: 1.0,
                menu_blur: 0.0,
            };
            e.update(&input);
            let bloom = e
                .filter_levels()
                .iter()
                .find(|(n, _)| n == "Bloom")
                .map_or(0.0, |l| l.1);
            out.push((t, bloom));
            t += 1.0 / fps;
        }
        out
    }

    #[test]
    fn every_hit_shows_promptly_at_any_frame_rate() {
        for fps in [24.0, 30.0, 60.0, 144.0] {
            let levels = bloom_levels(fps);
            let base = levels.iter().map(|l| l.1).fold(f32::MAX, f32::min);
            let peak = levels.iter().map(|l| l.1).fold(base, f32::max);
            assert!(peak > base + 0.2, "{fps} fps: no visible reaction");
            // Each hit (frame 11 + 23k, heard 1.5 hops later) reaches most of
            // the peak within 1.5 frames of being heard.
            for k in 0..15 {
                let heard = ((11 + 23 * k) as f64 + 1.5) * 2048.0 / 48000.0;
                let reached = levels
                    .iter()
                    .find(|(t, v)| *t >= heard - 0.05 && *v >= base + 0.6 * (peak - base));
                let (t, _) =
                    reached.unwrap_or_else(|| panic!("{fps} fps: hit {k} at {heard:.3} s missed"));
                assert!(
                    *t - heard < 1.5 / fps + 0.03,
                    "{fps} fps: hit {k} late by {:.0} ms",
                    (*t - heard) * 1000.0
                );
            }
        }
    }

    fn analysis_with_peaks() -> TrackAnalysis {
        let frames: usize = 44100 * 60 / 2048;
        let mut transitions = vec![0.0; frames.div_ceil(10)];
        for i in (20..transitions.len()).step_by(25) {
            transitions[i] = if i % 50 == 20 { 1.0 } else { 0.5 };
        }
        let novelty: Vec<Vec<f32>> = (0..3)
            .map(|b| {
                (0..frames)
                    .map(|f| if f % (20 + b) == 0 { 1.0 } else { 0.0 })
                    .collect()
            })
            .collect();
        TrackAnalysis {
            sample_rate: 44100,
            hop_size: 2048,
            energy: vec![0.5; frames],
            novelty,
            transitions,
            transition_down_sampling: 10,
        }
    }

    #[test]
    fn runs_full_preset_for_a_minute() {
        let mut rng = Rng::new(1);
        for (name, _) in preset::BUILTIN {
            let p = preset::builtin(name, &mut rng).unwrap();
            let mut eng = Engine::new(42);
            eng.set_preset(Some(p));
            eng.set_background(Some(ImageRef { id: 1, aspect: 1.5 }), false);
            let a = analysis_with_peaks();
            let mut transitions_seen = 0;
            let mut was = false;
            let mut t = 0.0;
            for frame in 0..3600 {
                let input = FrameInput {
                    dt: 1.0 / 60.0,
                    playing: true,
                    position: t,
                    analysis: Some(&a),
                    live_novelty: [0.0; 3],
                    live_energy: 0.0,
                    view_aspect: 16.0 / 9.0,
                    menu_blur: 0.0,
                };
                let view = eng.update(&input);
                assert!(
                    view.current.camera.position.iter().all(|v| v.is_finite()),
                    "{name} frame {frame}"
                );
                for f in &view.filters {
                    if let FilterView::Bloom { opacity, .. } = f {
                        assert!(opacity.is_finite());
                    }
                }
                let is = view.transition.is_some() || view.previous.is_some();
                if is && !was {
                    transitions_seen += 1;
                }
                was = is;
                if frame == 1800 {
                    // Change the image mid-way.
                    eng.set_background(
                        Some(ImageRef {
                            id: 2,
                            aspect: 0.75,
                        }),
                        true,
                    );
                }
                t += 1.0 / 60.0;
            }
            if name != &"Calm" {
                assert!(transitions_seen > 0, "{name}: expected visual transitions");
            }
            assert_eq!(eng.current_image().unwrap().id, 2);
        }
    }

    #[test]
    fn menu_blur_adds_filter() {
        let mut eng = Engine::new(1);
        eng.set_preset(Some(Preset::default()));
        let input = FrameInput {
            dt: 0.016,
            playing: false,
            position: 0.0,
            analysis: None,
            live_novelty: [0.0; 3],
            live_energy: 0.0,
            view_aspect: 1.0,
            menu_blur: 2.0,
        };
        let v = eng.update(&input);
        assert_eq!(
            v.filters,
            vec![FilterView::Blur {
                radius: 2.0,
                passes: 1
            }]
        );
    }

    #[test]
    fn position_generators() {
        let mut rng = Rng::new(3);
        let spec = PosSpec {
            mode: PosMode::Absolute,
            easing: Easing::Linear,
            min: [0.0, 0.0, 0.8, 0.0],
            max: [1.0, 1.0, 1.0, 0.0],
            off: [0.0; 4],
        };
        for _ in 0..100 {
            let p = spec
                .generate(&mut rng, CamPos::default(), Some(CamPos::default()))
                .unwrap();
            assert!((0.0..=1.0).contains(&p.x) && (0.8..=1.0).contains(&p.z));
        }
        let rel = PosSpec {
            mode: PosMode::Relative,
            off: [0.1, 0.1, 0.0, 5.0],
            ..spec
        };
        let cur = CamPos {
            x: 0.05,
            y: 0.5,
            z: 0.9,
            zr_deg: 10.0,
        };
        for _ in 0..100 {
            let p = rel.generate(&mut rng, cur, None).unwrap();
            assert!(p.x >= 0.0 && p.x <= 0.15 && (p.zr_deg - 10.0).abs() <= 5.0);
        }
    }
}
