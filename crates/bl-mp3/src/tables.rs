//! Layer III constant tables (scale factor bands, partitions, stereo and
//! IMDCT coefficients) and the lazily computed dequantization table.
//!
//! The band tables are those of ISO/IEC 11172-3 table B.8 and ISO/IEC
//! 13818-3 table B.2 in the layout used by minimp3
//! (<https://github.com/lieff/minimp3>, CC0 1.0); the other numeric
//! constants also follow minimp3.

use std::sync::OnceLock;

/// Scale factor band table row for an all-versions sample-rate index
/// (see [`crate::header::FrameHeader::sr_index_all`]).
#[inline]
pub(crate) fn sfb_row(sr_index_all: usize) -> usize {
    sr_index_all - usize::from(sr_index_all != 0)
}

/// Long-block band widths per row: rows are 11.025/12 kHz, 8 kHz,
/// 22.05 kHz, 24 kHz, 16 kHz, 44.1 kHz, 48 kHz, 32 kHz.
const LONG: [[u8; 22]; 8] = [
    [
        6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
    ],
    [
        12, 12, 12, 12, 12, 12, 16, 20, 24, 28, 32, 40, 48, 56, 64, 76, 90, 2, 2, 2, 2, 2,
    ],
    [
        6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
    ],
    [
        6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 18, 22, 26, 32, 38, 46, 54, 62, 70, 76, 36,
    ],
    [
        6, 6, 6, 6, 6, 6, 8, 10, 12, 14, 16, 20, 24, 28, 32, 38, 46, 52, 60, 68, 58, 54,
    ],
    [
        4, 4, 4, 4, 4, 4, 6, 6, 8, 8, 10, 12, 16, 20, 24, 28, 34, 42, 50, 54, 76, 158,
    ],
    [
        4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 10, 12, 16, 18, 22, 28, 34, 40, 46, 54, 54, 192,
    ],
    [
        4, 4, 4, 4, 4, 4, 6, 6, 8, 10, 12, 16, 20, 24, 30, 38, 46, 56, 68, 84, 102, 26,
    ],
];

/// Short-block band widths (each band repeated for the three windows).
const SHORT: [[u8; 39]; 8] = [
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18,
        18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18,
    ],
    [
        8, 8, 8, 8, 8, 8, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20, 24, 24, 24, 28, 28, 28, 36,
        36, 36, 2, 2, 2, 2, 2, 2, 2, 2, 2, 26, 26, 26,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 6, 6, 6, 8, 8, 8, 10, 10, 10, 14, 14, 14, 18, 18, 18,
        26, 26, 26, 32, 32, 32, 42, 42, 42, 18, 18, 18,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18,
        18, 24, 24, 24, 32, 32, 32, 44, 44, 44, 12, 12, 12,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14, 18, 18,
        18, 24, 24, 24, 30, 30, 30, 40, 40, 40, 18, 18, 18,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 10, 10, 10, 12, 12, 12, 14, 14, 14,
        18, 18, 18, 22, 22, 22, 30, 30, 30, 56, 56, 56,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 6, 6, 6, 10, 10, 10, 12, 12, 12, 14, 14, 14,
        16, 16, 16, 20, 20, 20, 26, 26, 26, 66, 66, 66,
    ],
    [
        4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 4, 6, 6, 6, 8, 8, 8, 12, 12, 12, 16, 16, 16, 20, 20, 20,
        26, 26, 26, 34, 34, 34, 42, 42, 42, 12, 12, 12,
    ],
];

pub(crate) static SFB_LONG: [[u8; 22]; 8] = LONG;
pub(crate) static SFB_SHORT: [[u8; 39]; 8] = SHORT;

/// Mixed-block band widths: the first `n_long` long bands (8 for MPEG-1,
/// 6 otherwise, always two or four subbands wide) followed by the short
/// bands from band 3 on. Stored zero-terminated with their length.
const fn mixed_row(row: usize) -> ([u8; 38], usize) {
    let n_long = if row >= 5 { 8 } else { 6 };
    let mut out = [0u8; 38];
    let mut i = 0;
    while i < n_long {
        out[i] = LONG[row][i];
        i += 1;
    }
    let mut j = 9;
    while j < 39 {
        out[i] = SHORT[row][j];
        i += 1;
        j += 1;
    }
    (out, i)
}

pub(crate) static SFB_MIXED: [([u8; 38], usize); 8] = [
    mixed_row(0),
    mixed_row(1),
    mixed_row(2),
    mixed_row(3),
    mixed_row(4),
    mixed_row(5),
    mixed_row(6),
    mixed_row(7),
];

/// Number of scale factors per partition, rows = long / mixed / short,
/// groups of four = MPEG-1, three MPEG-2 rows, three MPEG-2 intensity rows.
pub(crate) const SCF_PARTITIONS: [[u8; 28]; 3] = [
    [
        6, 5, 5, 5, 6, 5, 5, 5, 6, 5, 7, 3, 11, 10, 0, 0, 7, 7, 7, 0, 6, 6, 6, 3, 8, 8, 5, 0,
    ],
    [
        8, 9, 6, 12, 6, 9, 9, 9, 6, 9, 12, 6, 15, 18, 0, 0, 6, 15, 12, 0, 6, 12, 9, 6, 6, 18, 9, 0,
    ],
    [
        9, 9, 6, 12, 9, 9, 9, 9, 9, 9, 12, 6, 18, 18, 0, 0, 12, 12, 12, 0, 12, 9, 9, 6, 15, 12, 9,
        0,
    ],
];

/// MPEG-1 scalefac_compress -> (slen1 << 2) | slen2.
pub(crate) const SCFC_DECODE: [u8; 16] = [0, 1, 2, 3, 12, 5, 6, 7, 9, 10, 11, 13, 14, 15, 18, 19];

/// MPEG-2 scalefac_compress modulus table (rows of four).
pub(crate) const SCF_MOD: [u8; 24] = [
    5, 5, 4, 4, 5, 5, 4, 1, 4, 3, 1, 1, 5, 6, 6, 1, 4, 4, 4, 1, 4, 3, 1, 1,
];

/// Pre-emphasis added to long bands 11..=20 when `preflag` is set.
pub(crate) const PRETAB: [u8; 10] = [1, 1, 1, 1, 2, 2, 3, 3, 3, 2];

/// MPEG-1 intensity stereo (kl, kr) pairs for is_pos 0..=6.
pub(crate) const PAN: [f32; 14] = [
    0., 1., 0.21132487, 0.78867513, 0.36602540, 0.63397460, 0.5, 0.5, 0.63397460, 0.36602540,
    0.78867513, 0.21132487, 1., 0.,
];

/// Antialias butterfly coefficients (cs, ca).
pub(crate) const AA: [[f32; 8]; 2] = [
    [
        0.85749293, 0.88174200, 0.94962865, 0.98331459, 0.99551782, 0.99916056, 0.99989920,
        0.99999316,
    ],
    [
        0.51449576, 0.47173197, 0.31337745, 0.18191320, 0.09457419, 0.04096558, 0.01419856,
        0.00369997,
    ],
];

pub(crate) const TWID9: [f32; 18] = [
    0.73727734, 0.79335334, 0.84339145, 0.88701083, 0.92387953, 0.95371695, 0.97629601, 0.99144486,
    0.99904822, 0.67559021, 0.60876143, 0.53729961, 0.46174861, 0.38268343, 0.30070580, 0.21643961,
    0.13052619, 0.04361938,
];

pub(crate) const TWID3: [f32; 6] = [
    0.79335334, 0.92387953, 0.99144486, 0.60876143, 0.38268343, 0.13052619,
];

/// IMDCT windows (folded): normal long, stop/start transition.
pub(crate) const MDCT_WINDOW: [[f32; 18]; 2] = [
    [
        0.99904822, 0.99144486, 0.97629601, 0.95371695, 0.92387953, 0.88701083, 0.84339145,
        0.79335334, 0.73727734, 0.04361938, 0.13052619, 0.21643961, 0.30070580, 0.38268343,
        0.46174861, 0.53729961, 0.60876143, 0.67559021,
    ],
    [
        1., 1., 1., 1., 1., 1., 0.99144486, 0.92387953, 0.79335334, 0., 0., 0., 0., 0., 0.,
        0.13052619, 0.38268343, 0.60876143,
    ],
];

/// Largest quantized magnitude: 15 + (2^13 - 1).
pub(crate) const POW43_LEN: usize = 15 + 8192;

/// `|x|^(4/3)` for all quantized magnitudes, computed once in f64.
pub(crate) fn pow43() -> &'static [f32; POW43_LEN] {
    static TABLE: OnceLock<Box<[f32; POW43_LEN]>> = OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = Box::new([0f32; POW43_LEN]);
        for (i, v) in t.iter_mut().enumerate() {
            *v = (i as f64).powf(4.0 / 3.0) as f32;
        }
        t
    })
}

/// `2^(q / 4)` rounded once to f32.
#[inline]
pub(crate) fn pow2_quarter(q: i32) -> f32 {
    const FRAC: [f64; 4] = [
        1.0,
        1.189_207_115_002_721,
        std::f64::consts::SQRT_2,
        1.681_792_830_507_429,
    ];
    let e = q.div_euclid(4).clamp(-1022, 1023);
    let f = q.rem_euclid(4) as usize;
    (FRAC[f] * f64::from_bits(((e + 1023) as u64) << 52)) as f32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn band_tables_cover_granule() {
        for row in 0..8 {
            let l: usize = SFB_LONG[row].iter().map(|&w| w as usize).sum();
            let s: usize = SFB_SHORT[row].iter().map(|&w| w as usize).sum();
            let (m, n) = SFB_MIXED[row];
            let ms: usize = m[..n].iter().map(|&w| w as usize).sum();
            assert_eq!((l, s, ms), (576, 576, 576), "row {row}");
            let n_long = if row >= 5 { 8 } else { 6 };
            let long_lines: usize = m[..n_long].iter().map(|&w| w as usize).sum();
            assert_eq!(long_lines, if row == 1 { 72 } else { 36 });
            assert!(SFB_LONG[row]
                .iter()
                .chain(SFB_SHORT[row].iter())
                .all(|w| w % 2 == 0));
        }
    }

    #[test]
    fn quarter_powers() {
        assert_eq!(pow2_quarter(0), 1.0);
        assert_eq!(pow2_quarter(-8), 0.25);
        assert!((pow2_quarter(-1) - 0.840_896_4).abs() < 1e-7);
        assert!((pow43()[8] - 16.0).abs() < 1e-5);
    }
}
