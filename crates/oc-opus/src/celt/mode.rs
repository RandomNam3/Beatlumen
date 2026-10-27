//! The (only) CELT mode used by Opus: 48 kHz, 960-sample frames, 120-sample
//! overlap, 21 bands. Static tables and small helpers.
//!
//! Ported from libopus `celt/modes.c`, `celt/celt.c`, `celt/celt.h`,
//! `celt/quant_bands.c` and `celt/rate.c`:
//!
//! Copyright (c) 2007-2008 CSIRO
//! Copyright (c) 2007-2010 Xiph.Org Foundation
//! Copyright (c) 2008 Gregory Maxwell
//! Written by Jean-Marc Valin and Gregory Maxwell
//!
//! Redistribution and use in source and binary forms, with or without
//! modification, are permitted provided that the conditions in the NOTICE
//! file (BSD-3-Clause) are met. THIS SOFTWARE IS PROVIDED "AS IS" AND ANY
//! EXPRESS OR IMPLIED WARRANTIES ARE DISCLAIMED.

pub use super::static_modes::{CACHE_BITS50, CACHE_CAPS50, CACHE_INDEX50, LOGN400, WINDOW120};

pub const OVERLAP: usize = 120;
pub const NB_EBANDS: usize = 21;
pub const EFF_EBANDS: usize = 21;
pub const MAX_LM: usize = 3;
pub const SHORT_MDCT_SIZE: usize = 120;
pub const NB_ALLOC_VECTORS: usize = 11;
pub const MAX_PERIOD: usize = 1024;
pub const PREEMPH0: f32 = 0.850_006_1;
pub const COMBFILTER_MINPERIOD: i32 = 15;
pub const DECODE_BUFFER_SIZE: usize = 2048;
pub const CELT_LPC_ORDER: usize = 24;

pub const SPREAD_NONE: i32 = 0;
pub const SPREAD_NORMAL: i32 = 2;
pub const SPREAD_AGGRESSIVE: i32 = 3;

pub const MAX_FINE_BITS: i32 = 8;
pub const FINE_OFFSET: i32 = 21;
pub const QTHETA_OFFSET: i32 = 4;
pub const QTHETA_OFFSET_TWOPHASE: i32 = 16;
pub const LOG_MAX_PSEUDO: i32 = 6;

pub static EBANDS: [i16; 22] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 20, 24, 28, 34, 40, 48, 60, 78, 100,
];

/// Bit allocation table in units of 1/32 bit/sample.
pub static BAND_ALLOCATION: [u8; 11 * 21] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, //
    90, 80, 75, 69, 63, 56, 49, 40, 34, 29, 20, 18, 10, 0, 0, 0, 0, 0, 0, 0, 0, //
    110, 100, 90, 84, 78, 71, 65, 58, 51, 45, 39, 32, 26, 20, 12, 0, 0, 0, 0, 0, 0, //
    118, 110, 103, 93, 86, 80, 75, 70, 65, 59, 53, 47, 40, 31, 23, 15, 4, 0, 0, 0, 0, //
    126, 119, 112, 104, 95, 89, 83, 78, 72, 66, 60, 54, 47, 39, 32, 25, 17, 12, 1, 0, 0, //
    134, 127, 120, 114, 103, 97, 91, 85, 78, 72, 66, 60, 54, 47, 41, 35, 29, 23, 16, 10, 1, //
    144, 137, 130, 124, 113, 107, 101, 95, 88, 82, 76, 70, 64, 57, 51, 45, 39, 33, 26, 15,
    1, //
    152, 145, 138, 132, 123, 117, 111, 105, 98, 92, 86, 80, 74, 67, 61, 55, 49, 43, 36, 20,
    1, //
    162, 155, 148, 142, 133, 127, 121, 115, 108, 102, 96, 90, 84, 77, 71, 65, 59, 53, 46, 30,
    1, //
    172, 165, 158, 152, 143, 137, 131, 125, 118, 112, 106, 100, 94, 87, 81, 75, 69, 63, 56, 45,
    20, //
    200, 200, 200, 200, 200, 200, 200, 200, 198, 193, 188, 183, 178, 173, 168, 163, 158, 153, 148,
    129, 104, //
];

/// Mean energy in each band (Q4 values converted to float).
pub static E_MEANS: [f32; 25] = [
    6.437500, 6.250000, 5.750000, 5.312500, 5.062500, 4.812500, 4.500000, 4.375000, 4.875000,
    4.687500, 4.562500, 4.437500, 4.875000, 4.625000, 4.312500, 4.500000, 4.375000, 4.625000,
    4.750000, 4.437500, 3.750000, 3.750000, 3.750000, 3.750000, 3.750000,
];

pub static PRED_COEF: [f32; 4] = [
    29440.0 / 32768.0,
    26112.0 / 32768.0,
    21248.0 / 32768.0,
    16384.0 / 32768.0,
];
pub static BETA_COEF: [f32; 4] = [
    30147.0 / 32768.0,
    22282.0 / 32768.0,
    12124.0 / 32768.0,
    6554.0 / 32768.0,
];
pub const BETA_INTRA: f32 = 4915.0 / 32768.0;

/// Laplace model parameters for coarse energy: [LM][intra][2*band + {p0, decay}].
pub static E_PROB_MODEL: [[[u8; 42]; 2]; 4] = [
    [
        [
            72, 127, 65, 129, 66, 128, 65, 128, 64, 128, 62, 128, 64, 128, 64, 128, 92, 78, 92, 79,
            92, 78, 90, 79, 116, 41, 115, 40, 114, 40, 132, 26, 132, 26, 145, 17, 161, 12, 176, 10,
            177, 11,
        ],
        [
            24, 179, 48, 138, 54, 135, 54, 132, 53, 134, 56, 133, 55, 132, 55, 132, 61, 114, 70,
            96, 74, 88, 75, 88, 87, 74, 89, 66, 91, 67, 100, 59, 108, 50, 120, 40, 122, 37, 97, 43,
            78, 50,
        ],
    ],
    [
        [
            83, 78, 84, 81, 88, 75, 86, 74, 87, 71, 90, 73, 93, 74, 93, 74, 109, 40, 114, 36, 117,
            34, 117, 34, 143, 17, 145, 18, 146, 19, 162, 12, 165, 10, 178, 7, 189, 6, 190, 8, 177,
            9,
        ],
        [
            23, 178, 54, 115, 63, 102, 66, 98, 69, 99, 74, 89, 71, 91, 73, 91, 78, 89, 86, 80, 92,
            66, 93, 64, 102, 59, 103, 60, 104, 60, 117, 52, 123, 44, 138, 35, 133, 31, 97, 38, 77,
            45,
        ],
    ],
    [
        [
            61, 90, 93, 60, 105, 42, 107, 41, 110, 45, 116, 38, 113, 38, 112, 38, 124, 26, 132, 27,
            136, 19, 140, 20, 155, 14, 159, 16, 158, 18, 170, 13, 177, 10, 187, 8, 192, 6, 175, 9,
            159, 10,
        ],
        [
            21, 178, 59, 110, 71, 86, 75, 85, 84, 83, 91, 66, 88, 73, 87, 72, 92, 75, 98, 72, 105,
            58, 107, 54, 115, 52, 114, 55, 112, 56, 129, 51, 132, 40, 150, 33, 140, 29, 98, 35, 77,
            42,
        ],
    ],
    [
        [
            42, 121, 96, 66, 108, 43, 111, 40, 117, 44, 123, 32, 120, 36, 119, 33, 127, 33, 134,
            34, 139, 21, 147, 23, 152, 20, 158, 25, 154, 26, 166, 21, 173, 16, 184, 13, 184, 10,
            150, 13, 139, 15,
        ],
        [
            22, 178, 63, 114, 74, 82, 84, 83, 92, 82, 103, 62, 96, 72, 96, 67, 101, 73, 107, 72,
            113, 55, 118, 52, 125, 52, 118, 52, 117, 55, 135, 49, 137, 39, 157, 32, 145, 29, 97,
            33, 77, 40,
        ],
    ],
];

pub static SMALL_ENERGY_ICDF: [u8; 3] = [2, 1, 0];

/// TF change table, indexed `[LM][4*isTransient + 2*tf_select + per_band_flag]`.
pub static TF_SELECT_TABLE: [[i8; 8]; 4] = [
    [0, -1, 0, -1, 0, -1, 0, -1],
    [0, -1, 0, -2, 1, 0, 1, -1],
    [0, -2, 0, -3, 2, 0, 1, -1],
    [0, -2, 0, -3, 3, 0, 1, -1],
];

pub static TRIM_ICDF: [u8; 11] = [126, 124, 119, 109, 87, 41, 19, 9, 4, 2, 0];
pub static SPREAD_ICDF: [u8; 4] = [25, 23, 2, 0];
pub static TAPSET_ICDF: [u8; 3] = [2, 1, 0];

pub static LOG2_FRAC_TABLE: [u8; 24] = [
    0, 8, 13, 16, 19, 21, 23, 24, 26, 27, 28, 29, 30, 31, 32, 32, 33, 34, 34, 35, 36, 36, 37, 37,
];

/// Ordery Hadamard indexing for N=2, 4, 8, 16 (offset `stride-2`).
pub static ORDERY_TABLE: [usize; 30] = [
    1, 0, 3, 0, 2, 1, 7, 0, 4, 3, 6, 1, 5, 2, 15, 0, 8, 7, 12, 3, 11, 4, 14, 1, 9, 6, 13, 2, 10, 5,
];

/// Pulse cache bits for a band at a given LM (`cache.bits + cache.index[...]`).
#[inline]
pub fn cache_bits(lm1: usize, band: usize) -> &'static [u8] {
    let idx = CACHE_INDEX50[lm1 * NB_EBANDS + band];
    if idx < 0 {
        &[0]
    } else {
        &CACHE_BITS50[idx as usize..]
    }
}

/// `init_caps`
pub fn init_caps(cap: &mut [i32; NB_EBANDS], lm: usize, c: usize) {
    for (i, cap) in cap.iter_mut().enumerate() {
        let n = ((EBANDS[i + 1] - EBANDS[i]) as i32) << lm;
        *cap = (CACHE_CAPS50[NB_EBANDS * (2 * lm + c - 1) + i] as i32 + 64) * c as i32 * n >> 2;
    }
}

/// Decimation factor for an API sample rate.
pub fn resampling_factor(rate: u32) -> usize {
    match rate {
        48000 => 1,
        24000 => 2,
        16000 => 3,
        12000 => 4,
        8000 => 6,
        _ => 0,
    }
}
