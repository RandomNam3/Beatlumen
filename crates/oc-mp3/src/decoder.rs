//! Frame-level decoder state shared by all layers.

use crate::header::FrameHeader;
use crate::layer3::Layer3;
use crate::synth::SynthState;
use crate::{layer12, Error};

pub(crate) struct FrameDecoder {
    synth: SynthState,
    l3: Layer3,
}

impl FrameDecoder {
    pub fn new() -> Self {
        FrameDecoder {
            synth: SynthState::new(),
            l3: Layer3::new(),
        }
    }

    /// Clears all inter-frame state (filterbank history, bit reservoir).
    pub fn reset(&mut self) {
        self.synth.reset();
        self.l3.reset();
    }

    /// Decodes `frame` (header included) into `pcm`, which must hold
    /// `samples_per_frame * channels` samples. Corrupt frames produce
    /// silence; returns whether the frame decoded cleanly.
    pub fn decode(
        &mut self,
        h: &FrameHeader,
        frame: &[u8],
        pcm: &mut [f32],
    ) -> Result<bool, Error> {
        if pcm.len() < h.samples_per_frame() * h.channels() {
            return Err(Error::InvalidData("pcm buffer too small"));
        }
        Ok(match h.layer {
            3 => self.l3.decode_frame(&mut self.synth, h, frame, pcm),
            _ => layer12::decode_frame(&mut self.synth, h, frame, pcm),
        })
    }
}
