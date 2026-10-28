//! AirPlay audio receiver (RAOP, "AirPlay 1"): Beatlumen shows up as a
//! speaker on iPhone, iPad, Mac and other AirPlay senders and plays and
//! visualises what they send.
//!
//! * [`Receiver`] accepts the senders' RTSP control connections; the audio
//!   arrives over UDP ([`session`]) and is decrypted and decoded into a
//!   [`LiveStream`], which the player reads like a decoder.
//! * [`mdns::Responder`] announces the receiver in the local network
//!   (platforms with their own service registration use
//!   [`Receiver::instance_name`] and [`Receiver::txt`] instead).

pub mod crypto;
pub mod mdns;
pub mod meta;
mod receiver;
pub mod rtsp;
mod session;
pub mod stream;
mod sys;

pub use receiver::{Config, Event, Receiver};
pub use stream::{LiveDecoder, LiveStream};
