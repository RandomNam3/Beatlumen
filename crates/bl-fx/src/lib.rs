//! Effect engine of Beatlumen.
//!
//! * [`preset`]: `.hfx` preset model (compatible with Hyperchroma 0.9.x).
//! * [`schema`]: declarative effect type descriptions (defaults, ranges).
//! * [`engine`]: per-frame runtime producing a [`engine::FrameView`].
//! * [`camera`], [`particles`], [`easing`], [`tween`], [`rng`]: building blocks.

#![forbid(unsafe_code)]

pub mod camera;
pub mod easing;
pub mod engine;
pub mod particles;
pub mod preset;
pub mod rng;
pub mod schema;
pub mod tween;

pub use engine::{Engine, FilterView, FrameInput, FrameView, ImageRef, MotionBlur, SceneView, TransitionKind, TransitionView};
pub use particles::ParticleInstance;
pub use preset::{Effect, Preset};
pub use rng::Rng;
