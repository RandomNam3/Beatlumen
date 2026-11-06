//! Laying decoded samples out as the integers a sound card takes.
//!
//! The decoders hand out `f32` scaled by a power of two. A float holds every
//! integer up to 24 bits exactly, so samples of up to 24 bits come back here
//! with the very value they had in the file, also in a wider word. Only 32-bit
//! files lose their lowest bits on the way, and they say so through
//! [`crate::Decoder::bits_per_sample`].

/// A sample format of a sound card (little endian).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u32)]
pub enum PcmFormat {
    S16 = 0,
    /// 24 bits in three bytes.
    S24Packed = 1,
    /// 24 bits in the low bytes of four.
    S24 = 2,
    /// Also how most USB DACs take 24 bits: in the high bytes of four.
    S32 = 3,
}

impl PcmFormat {
    pub const ALL: [PcmFormat; 4] = [
        PcmFormat::S16,
        PcmFormat::S24Packed,
        PcmFormat::S24,
        PcmFormat::S32,
    ];

    pub fn from_index(i: u32) -> Option<PcmFormat> {
        PcmFormat::ALL.get(i as usize).copied()
    }

    pub fn bytes(self) -> usize {
        match self {
            PcmFormat::S16 => 2,
            PcmFormat::S24Packed => 3,
            PcmFormat::S24 | PcmFormat::S32 => 4,
        }
    }

    /// Bits a value can have.
    pub fn bits(self) -> u8 {
        match self {
            PcmFormat::S16 => 16,
            PcmFormat::S24Packed | PcmFormat::S24 => 24,
            PcmFormat::S32 => 32,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            PcmFormat::S16 => "S16_LE",
            PcmFormat::S24Packed => "S24_3LE",
            PcmFormat::S24 => "S24_LE",
            PcmFormat::S32 => "S32_LE",
        }
    }
}

/// The formats to ask a card for, best first: the narrowest that holds the
/// source's bits, then wider ones (which keep every bit), and what would drop
/// bits last. Without integer samples in the source (lossy, float) the widest
/// comes first.
pub fn pcm_preference(source_bits: Option<u8>) -> [PcmFormat; 4] {
    use PcmFormat::*;
    match source_bits {
        Some(b) if b <= 16 => [S16, S24Packed, S32, S24],
        Some(b) if b <= 24 => [S24Packed, S32, S24, S16],
        _ => [S32, S24Packed, S24, S16],
    }
}

/// Writes interleaved stereo as `format` with `channels` channels per frame;
/// channels past the second stay silent (a card that only takes all of its
/// channels at once). `out` holds `stereo.len() / 2` frames.
pub fn pack_pcm(stereo: &[f32], format: PcmFormat, channels: usize, out: &mut [u8]) {
    let channels = channels.max(2);
    let frame_bytes = format.bytes() * channels;
    out.fill(0);
    for (frame, chunk) in stereo
        .chunks_exact(2)
        .zip(out.chunks_exact_mut(frame_bytes))
    {
        let (left, right) = chunk.split_at_mut(format.bytes());
        write_sample(frame[0], format, left);
        write_sample(frame[1], format, right);
    }
}

#[inline]
fn write_sample(sample: f32, format: PcmFormat, out: &mut [u8]) {
    let scale = (1u64 << (format.bits() - 1)) as f64;
    let value = (sample as f64 * scale).round().clamp(-scale, scale - 1.0) as i32;
    let bytes = value.to_le_bytes();
    match format {
        PcmFormat::S16 => out[..2].copy_from_slice(&bytes[..2]),
        PcmFormat::S24Packed => out[..3].copy_from_slice(&bytes[..3]),
        PcmFormat::S24 | PcmFormat::S32 => out[..4].copy_from_slice(&bytes),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(sample: f32, format: PcmFormat) -> Vec<u8> {
        let mut out = vec![0; format.bytes() * 2];
        pack_pcm(&[sample, sample], format, 2, &mut out);
        out[..format.bytes()].to_vec()
    }

    #[test]
    fn every_16_bit_value_survives_in_every_format() {
        for v in i16::MIN..=i16::MAX {
            // As the decoders scale them.
            let s = v as f32 * (1.0 / 32768.0);
            assert_eq!(one(s, PcmFormat::S16), v.to_le_bytes());
            assert_eq!(one(s, PcmFormat::S24Packed)[1..], v.to_le_bytes());
            assert_eq!(one(s, PcmFormat::S24), ((v as i32) << 8).to_le_bytes());
            assert_eq!(one(s, PcmFormat::S32), ((v as i32) << 16).to_le_bytes());
        }
    }

    #[test]
    fn every_24_bit_value_survives_in_the_formats_that_hold_it() {
        let scale = 1.0 / (1u64 << 23) as f32;
        for v in (-(1i32 << 23)..1 << 23).step_by(7).chain([(1 << 23) - 1]) {
            let s = v as f32 * scale;
            assert_eq!(one(s, PcmFormat::S24Packed), v.to_le_bytes()[..3]);
            assert_eq!(one(s, PcmFormat::S24), v.to_le_bytes());
            assert_eq!(one(s, PcmFormat::S32), (v << 8).to_le_bytes());
        }
    }

    #[test]
    fn loud_floats_stop_at_the_end_of_the_range() {
        assert_eq!(one(2.0, PcmFormat::S16), i16::MAX.to_le_bytes());
        assert_eq!(one(-2.0, PcmFormat::S16), i16::MIN.to_le_bytes());
        assert_eq!(one(1.0, PcmFormat::S32), i32::MAX.to_le_bytes());
        assert_eq!(one(-1.0, PcmFormat::S32), i32::MIN.to_le_bytes());
    }

    #[test]
    fn extra_channels_stay_silent() {
        let mut out = vec![0xAA; 4 * 10 * 2];
        pack_pcm(&[0.5, -0.5, 0.25, -0.25], PcmFormat::S32, 10, &mut out);
        let word = |i: usize| i32::from_le_bytes(out[i * 4..i * 4 + 4].try_into().unwrap());
        assert_eq!(word(0), 1 << 30);
        assert_eq!(word(1), -(1 << 30));
        assert!((2..10).all(|i| word(i) == 0));
        assert_eq!(word(10), 1 << 29);
        assert_eq!(word(11), -(1 << 29));
        assert!((12..20).all(|i| word(i) == 0));
    }

    #[test]
    fn the_narrowest_format_that_holds_the_source_comes_first() {
        use PcmFormat::*;
        assert_eq!(pcm_preference(Some(16))[0], S16);
        assert_eq!(pcm_preference(Some(8))[0], S16);
        assert_eq!(pcm_preference(Some(24))[0], S24Packed);
        assert_eq!(pcm_preference(Some(20))[0], S24Packed);
        // A 24-bit source never starts with what would drop bits.
        assert_eq!(pcm_preference(Some(24))[3], S16);
        assert_eq!(pcm_preference(None)[0], S32);
        assert_eq!(pcm_preference(Some(32))[0], S32);
    }
}
