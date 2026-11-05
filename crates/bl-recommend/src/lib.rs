//! Recommendations without anything external: a song is compared by what
//! is known about it (artist, album, genre, year, labels and, on a server,
//! how it sounds); a listener's taste comes from their favorites and plays.
//!
//! Nothing here reads files or talks to anyone. The server feeds it its
//! library and the login's history, the apps without a server their own
//! (with tags only, no sound).

#![forbid(unsafe_code)]

mod catalog;
mod labels;
mod mixes;
mod radio;
mod rng;
pub mod stats;
mod taste;

pub use catalog::{Catalog, Features, Song};
pub use labels::{auto_labels, key_name};
pub use mixes::{mixes, shelves, Mix, Reason, Shelf, Suggestion};
pub use radio::{radio, Seed};
pub use taste::{Play, Signals, Taste};
