//! Scalefactor band and TNS tables of ISO/IEC 14496-3 (subpart 4) for
//! 1024/128-line frames.

const SWB_LONG_96: [u16; 42] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 156, 172, 188, 212,
    240, 276, 320, 384, 448, 512, 576, 640, 704, 768, 832, 896, 960, 1024,
];
const SWB_LONG_64: [u16; 48] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64, 72, 80, 88, 100, 112, 124, 140, 156, 172, 192, 216, 240,
    268, 304, 344, 384, 424, 464, 504, 544, 584, 624, 664, 704, 744, 784, 824, 864, 904, 944, 984, 1024,
];
const SWB_LONG_48: [u16; 50] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160, 176, 196, 216, 240, 264,
    292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704, 736, 768, 800, 832, 864, 896, 928, 1024,
];
const SWB_LONG_32: [u16; 52] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 48, 56, 64, 72, 80, 88, 96, 108, 120, 132, 144, 160, 176, 196, 216, 240, 264,
    292, 320, 352, 384, 416, 448, 480, 512, 544, 576, 608, 640, 672, 704, 736, 768, 800, 832, 864, 896, 928, 960, 992,
    1024,
];
const SWB_LONG_24: [u16; 48] = [
    0, 4, 8, 12, 16, 20, 24, 28, 32, 36, 40, 44, 52, 60, 68, 76, 84, 92, 100, 108, 116, 124, 136, 148, 160, 172, 188,
    204, 220, 240, 260, 284, 308, 336, 364, 396, 432, 468, 508, 552, 600, 652, 704, 768, 832, 896, 960, 1024,
];
const SWB_LONG_16: [u16; 44] = [
    0, 8, 16, 24, 32, 40, 48, 56, 64, 72, 80, 88, 100, 112, 124, 136, 148, 160, 172, 184, 196, 212, 228, 244, 260, 280,
    300, 320, 344, 368, 396, 424, 456, 492, 532, 572, 616, 664, 716, 772, 832, 896, 960, 1024,
];
const SWB_LONG_8: [u16; 41] = [
    0, 12, 24, 36, 48, 60, 72, 84, 96, 108, 120, 132, 144, 156, 172, 188, 204, 220, 236, 252, 268, 288, 308, 328, 348,
    372, 396, 420, 448, 476, 508, 544, 580, 620, 664, 712, 764, 820, 880, 944, 1024,
];

const SWB_SHORT_96: [u16; 13] = [0, 4, 8, 12, 16, 20, 24, 32, 40, 48, 64, 92, 128];
const SWB_SHORT_48: [u16; 15] = [0, 4, 8, 12, 16, 20, 28, 36, 44, 56, 68, 80, 96, 112, 128];
const SWB_SHORT_24: [u16; 16] = [0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 64, 76, 92, 108, 128];
const SWB_SHORT_16: [u16; 16] = [0, 4, 8, 12, 16, 20, 24, 28, 32, 40, 48, 60, 72, 88, 108, 128];
const SWB_SHORT_8: [u16; 16] = [0, 4, 8, 12, 16, 20, 24, 28, 36, 44, 52, 60, 72, 88, 108, 128];

/// Long-window scalefactor band offsets (num_swb + 1 entries) for a
/// sampling frequency index (0..=12).
pub(crate) fn swb_offset_long(sf_index: usize) -> &'static [u16] {
    match sf_index {
        0 | 1 => &SWB_LONG_96,
        2 => &SWB_LONG_64,
        3 | 4 => &SWB_LONG_48,
        5 => &SWB_LONG_32,
        6 | 7 => &SWB_LONG_24,
        8..=10 => &SWB_LONG_16,
        _ => &SWB_LONG_8,
    }
}

/// Short-window scalefactor band offsets.
pub(crate) fn swb_offset_short(sf_index: usize) -> &'static [u16] {
    match sf_index {
        0..=2 => &SWB_SHORT_96,
        3..=5 => &SWB_SHORT_48,
        6 | 7 => &SWB_SHORT_24,
        8..=10 => &SWB_SHORT_16,
        _ => &SWB_SHORT_8,
    }
}

/// Maximum number of TNS bands (AAC LC) for long / short windows.
pub(crate) fn tns_max_bands(sf_index: usize, short: bool) -> usize {
    const LONG: [u8; 13] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39, 39];
    const SHORT: [u8; 13] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14];
    let i = sf_index.min(12);
    if short {
        SHORT[i] as usize
    } else {
        LONG[i] as usize
    }
}

/// Maps an arbitrary sampling frequency to the sampling frequency index whose
/// tables are used (ISO/IEC 14496-3 Table 4.82).
pub(crate) fn sf_index_for_rate(rate: u32) -> usize {
    const LIMITS: [u32; 11] = [92017, 75132, 55426, 46009, 37566, 27713, 23004, 18783, 13856, 11502, 9391];
    LIMITS.iter().position(|&l| rate >= l).unwrap_or(11)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_tables_are_consistent() {
        let expect_long = [41, 41, 47, 49, 49, 51, 47, 47, 43, 43, 43, 40, 40];
        let expect_short = [12, 12, 12, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15];
        for i in 0..13 {
            let l = swb_offset_long(i);
            let s = swb_offset_short(i);
            assert_eq!(l.len() - 1, expect_long[i], "long {i}");
            assert_eq!(s.len() - 1, expect_short[i], "short {i}");
            assert_eq!(*l.last().unwrap(), 1024);
            assert_eq!(*s.last().unwrap(), 128);
            assert!(l.windows(2).all(|w| w[0] < w[1] && (w[1] - w[0]) % 4 == 0));
            assert!(s.windows(2).all(|w| w[0] < w[1] && (w[1] - w[0]) % 4 == 0));
        }
        assert_eq!(sf_index_for_rate(44100), 4);
        assert_eq!(sf_index_for_rate(48000), 3);
        assert_eq!(sf_index_for_rate(8000), 11);
        assert_eq!(sf_index_for_rate(96000), 0);
    }
}
