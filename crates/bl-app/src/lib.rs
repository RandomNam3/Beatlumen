//! Platform independent Beatlumen application: playback, library,
//! effects, user interface. Platform layers implement [`host::Host`] and
//! drive [`App`].

pub mod app;
pub mod host;
pub mod jobs;
pub mod library;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
pub mod player;
pub mod queue;
mod screens;
pub mod server;
pub mod settings;
pub mod strings;
pub mod view;

pub use app::{icon_rgba, App, LaunchArgs};
