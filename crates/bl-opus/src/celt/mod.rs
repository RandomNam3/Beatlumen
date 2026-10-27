//! CELT layer of the Opus decoder (RFC 6716 section 4.3), float build.
//!
//! Ported from the libopus reference implementation (BSD-3-Clause, see the
//! NOTICE file and the per-file headers).

#![allow(clippy::excessive_precision)]

pub mod bands;
pub mod decoder;
pub mod energy;
pub mod math;
pub mod mdct;
pub mod mode;
pub mod pitch;
pub mod rate;
#[rustfmt::skip]
pub mod static_modes;
pub mod vq;
