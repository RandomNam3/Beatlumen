//! JPEG (ITU T.81) decoding and baseline encoding.
//!
//! * `decode`: sequential and progressive Huffman decoding, restart markers,
//!   any sampling factors, gray / YCbCr / RGB / Adobe CMYK & YCCK, EXIF
//!   orientation and DCT-domain downscaling by 1/2, 1/4, 1/8,
//! * `encode`: baseline 4:2:0 encoder with the standard (Annex K) tables.
//!
//! Arithmetic coding, lossless, hierarchical and 12-bit JPEGs are reported as
//! [`crate::Error::Unsupported`].

pub(crate) mod color;
mod decode;
pub mod encode;
mod huffman;
mod idct;

pub use decode::{decode, decode_scaled, dimensions};

/// Zigzag index -> natural (row-major) index, padded with 63 so that corrupt
/// run lengths cannot index out of bounds.
pub(crate) const ZIGZAG_EXT: [usize; 80] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63, 63,
    63, 63, 63, 63, 63, 63,
];

#[cfg(test)]
mod tests {
    use crate::Image;

    fn test_image(w: u32, h: u32) -> Image {
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                img.pixels[i..i + 4].copy_from_slice(&[
                    (x * 255 / w) as u8,
                    (y * 255 / h) as u8,
                    ((x * y) % 251) as u8,
                    255,
                ]);
            }
        }
        img
    }

    #[test]
    fn restart_markers_decode_identically() {
        let img = test_image(77, 45);
        let plain = super::decode(&super::encode::encode(&img, 85), 1).unwrap();
        for interval in [1u16, 2, 3, 7] {
            let jpg = super::encode::encode_with_restart(&img, 85, interval);
            assert!(jpg.windows(2).any(|w| w == [0xff, 0xd0]));
            let dec = super::decode(&jpg, 1).unwrap();
            assert_eq!(dec.pixels, plain.pixels, "interval {interval}");
        }
    }

    #[test]
    fn unsupported_coding_processes() {
        let jpg = super::encode::encode(&test_image(16, 16), 80);
        let sof = jpg.windows(2).position(|w| w == [0xff, 0xc0]).unwrap();
        for (marker, what) in [
            (0xc9u8, "arithmetic"),
            (0xc3, "lossless"),
            (0xc5, "hierarchical"),
        ] {
            let mut m = jpg.clone();
            m[sof + 1] = marker;
            match super::decode(&m, 1) {
                Err(crate::Error::Unsupported(msg)) => assert!(msg.contains(what), "{msg}"),
                other => panic!(
                    "expected Unsupported for {what}, got {:?}",
                    other.map(|i| i.width)
                ),
            }
        }
        let mut m = jpg.clone();
        m[sof + 4] = 12; // sample precision
        assert!(matches!(
            super::decode(&m, 1),
            Err(crate::Error::Unsupported(_))
        ));
    }

    #[test]
    fn corrupt_restart_segment_is_contained() {
        // Damage the data between two restart markers: the decoder resyncs
        // at the next marker, so the image still decodes.
        let img = test_image(64, 64);
        let mut jpg = super::encode::encode_with_restart(&img, 85, 1);
        let rst = jpg.windows(2).position(|w| w == [0xff, 0xd1]).unwrap();
        for b in &mut jpg[rst - 20..rst - 2] {
            *b = 0x5a;
        }
        let dec = super::decode(&jpg, 1).unwrap();
        assert_eq!((dec.width, dec.height), (64, 64));
    }

    #[test]
    fn zigzag_is_diagonal_walk() {
        let mut zz = Vec::new();
        for s in 0..15usize {
            let cells: Vec<usize> = (0..=s)
                .filter(|&r| r < 8 && s - r < 8)
                .map(|r| r * 8 + (s - r))
                .collect();
            // Even diagonals go up-right (row decreasing), odd ones down-left.
            if s % 2 == 0 {
                zz.extend(cells.iter().rev());
            } else {
                zz.extend(cells.iter());
            }
        }
        assert_eq!(&super::ZIGZAG_EXT[..64], &zz[..]);
    }
}
