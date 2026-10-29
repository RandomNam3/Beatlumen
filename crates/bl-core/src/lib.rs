//! Platform independent Beatlumen core: playback, library, queues,
//! presets, backgrounds, server connection and the visualizer.
//!
//! The user interface (Qt/QML, see `apps/beatlumen`) drives [`Core`]
//! through a JSON command API ([`Core::command`]) and renders the
//! visualizer with [`Core::render`]. Platform services come in through
//! [`host::Host`].

mod airplay;
pub mod capture;
mod commands;
pub mod core;
#[cfg(not(target_arch = "wasm32"))]
mod dlna_receiver;
#[cfg(target_arch = "wasm32")]
#[path = "dlna_receiver_web.rs"]
mod dlna_receiver;
mod extensions;
pub mod host;
pub mod icon;
pub mod jobs;
pub mod library;
#[cfg(not(target_arch = "wasm32"))]
pub mod native;
mod outputs;
pub mod player;
mod push;
pub mod queue;
pub mod server;
pub mod settings;
pub mod strings;
mod vault;
mod visual;
mod webcheck;

pub use crate::core::{Core, LaunchArgs};
