//! AirPlay audio (RAOP, "AirPlay 1", and basic AirPlay 2): Beatlumen shows
//! up as a speaker on iPhone, iPad, Mac and other AirPlay senders and plays
//! and visualises what they send, and it sends its own playback to AirPlay
//! speakers ([`sender`], found with [`mdns::browse`]).
//!
//! * [`Receiver`] accepts the senders' RTSP control connections; the audio
//!   arrives over UDP ([`session`]) and is decrypted and decoded into a
//!   [`LiveStream`], which the player reads like a decoder.
//! * AirPlay 2 senders pair first and then use the same connection
//!   encrypted; their audio comes buffered over TCP or realtime over UDP
//!   ([`ap2`]).
//! * [`mdns::Responder`] announces the receiver in the local network
//!   ([`Receiver::services`]: `_raop._tcp` and `_airplay._tcp`; platforms
//!   with their own service registration use [`Receiver::instance_name`]
//!   and [`Receiver::txt`] instead).

pub mod ap2;
pub mod crypto;
pub mod mdns;
pub mod meta;
mod receiver;
pub mod rtsp;
pub mod sender;
mod session;
pub mod stream;

pub use receiver::{Config, Event, Receiver};
pub use stream::{LiveDecoder, LiveStream};
