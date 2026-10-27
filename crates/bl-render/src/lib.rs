//! GPU rendering for Beatlumen on top of `bl-gl`:
//!
//! * [`visualizer::Visualizer`] renders the effect engine's frames
//!   (image planes, particles, motion blur, transitions, filters).
//! * [`batch2d::Batch2d`] renders the user interface.

pub mod batch2d;
pub mod gpu;
pub mod math;
pub mod shaders;
pub mod visualizer;

pub use batch2d::{Batch2d, Color, Rect};
pub use gpu::{RenderTarget, Target, Texture};
pub use visualizer::Visualizer;
