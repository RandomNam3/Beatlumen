//! Image decoding (PNG, JPEG, BMP, GIF, WebP), resizing and simple encoding
//! (PNG, baseline JPEG), std only.
//!
//! The crate is used for cover art and background images, so the decoders are
//! tuned for photos and screenshots and are lenient with damaged files
//! (truncated images decode partially where possible) but never panic on
//! malformed input.
//!
//! Module overview:
//! * [`inflate`] / [`deflate`]: zlib (RFC 1950) and deflate (RFC 1951),
//! * `png`: PNG decoder (all color types / bit depths, Adam7, APNG default
//!   image) and encoder,
//! * `jpeg`: baseline + progressive Huffman JPEG decoder (DCT scaling for
//!   [`decode_max`], EXIF orientation) and baseline encoder,
//! * `bmp`, `gif`, `webp` (VP8L lossless and VP8 lossy incl. alpha),
//! * `resize`: area averaging / bilinear resampling with premultiplied alpha.
#![forbid(unsafe_code)]
// `is_multiple_of` needs Rust 1.87; the workspace MSRV is 1.80. Index loops
// mirror the specifications' pseudo code (VP8 prediction, transforms).
#![allow(clippy::manual_is_multiple_of, clippy::needless_range_loop)]

mod bmp;
mod crc32;
pub mod deflate;
mod exif;
mod gif;
mod huffman;
pub mod inflate;
mod jpeg;
mod png;
mod resize;
mod webp;

/// Error returned by the decoders.
#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid image data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported image feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Png,
    Jpeg,
    Bmp,
    Gif,
    WebP,
}

/// 8-bit RGBA image, rows top to bottom, no padding. Colours are straight
/// (not premultiplied) alpha.
#[derive(Debug, Clone)]
pub struct Image {
    pub width: u32,
    pub height: u32,
    pub pixels: Vec<u8>,
}

impl Image {
    /// Creates a fully transparent black image.
    pub fn new(width: u32, height: u32) -> Image {
        Image {
            width,
            height,
            pixels: vec![0; width as usize * height as usize * 4],
        }
    }

    /// Returns true when every pixel has alpha 255.
    pub fn is_opaque(&self) -> bool {
        self.pixels.chunks_exact(4).all(|p| p[3] == 255)
    }
}

/// Largest number of pixels a decoder will allocate (256 Mpixel = 1 GiB of
/// RGBA). Larger images are rejected with [`Error::Unsupported`].
pub const MAX_PIXELS: u64 = 1 << 28;

/// Checks the dimensions against [`MAX_PIXELS`] and allocates an image.
pub(crate) fn alloc_image(width: u32, height: u32) -> Result<Image, Error> {
    if width == 0 || height == 0 {
        return Err(Error::InvalidData("zero image dimension"));
    }
    if width as u64 * height as u64 > MAX_PIXELS {
        return Err(Error::Unsupported("image too large"));
    }
    Ok(Image::new(width, height))
}

pub fn probe_format(data: &[u8]) -> Option<Format> {
    if data.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if data.starts_with(&[0xff, 0xd8, 0xff]) {
        Some(Format::Jpeg)
    } else if data.starts_with(b"BM") {
        Some(Format::Bmp)
    } else if data.starts_with(b"GIF87a") || data.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if data.len() >= 12 && &data[..4] == b"RIFF" && &data[8..12] == b"WEBP" {
        Some(Format::WebP)
    } else {
        None
    }
}

/// Decodes an image in any supported format (EXIF orientation applied for JPEG).
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    match probe_format(data) {
        Some(Format::Png) => png::decode(data),
        Some(Format::Jpeg) => jpeg::decode(data, 1),
        Some(Format::Bmp) => bmp::decode(data),
        Some(Format::Gif) => gif::decode(data),
        Some(Format::WebP) => webp::decode(data),
        None => Err(Error::Unsupported("unknown image format")),
    }
}

/// Decodes and downscales so that neither side exceeds `max_dim` (keeping
/// the aspect ratio). Implementations may use cheaper decoding paths (e.g.
/// JPEG DCT scaling) as long as the result quality is comparable.
pub fn decode_max(data: &[u8], max_dim: u32) -> Result<Image, Error> {
    let max_dim = max_dim.max(1);
    let img = match probe_format(data) {
        Some(Format::Jpeg) => {
            // Pick the largest DCT scale (1/2, 1/4, 1/8) that still yields an
            // image at least `max_dim` on its longer side; the final area
            // averaging resize then produces the exact size.
            let (w, h) = jpeg::dimensions(data)?;
            let long = w.max(h);
            let mut scale = 1;
            while scale < 8 && long.div_ceil(scale * 2) >= max_dim {
                scale *= 2;
            }
            let (img, ew, eh) = jpeg::decode_scaled(data, scale)?;
            if scale == 1 {
                img
            } else {
                // Target size from the exact (unscaled) dimensions; the
                // resize maps the exact extent so that partially covered
                // edge pixels do not shift the image.
                let (ow, oh) = (ew * scale as f64, eh * scale as f64);
                let s = max_dim as f64 / ow.max(oh);
                let tw = ((ow * s).round() as u32).clamp(1, max_dim);
                let th = ((oh * s).round() as u32).clamp(1, max_dim);
                return Ok(resize::resize_extent(&img, ew, eh, tw, th));
            }
        }
        _ => decode(data)?,
    };
    Ok(fit_within(img, max_dim))
}

/// Downscales `img` (if needed) so that neither side exceeds `max_dim`.
fn fit_within(img: Image, max_dim: u32) -> Image {
    if img.width <= max_dim && img.height <= max_dim {
        return img;
    }
    let s = max_dim as f64 / img.width.max(img.height) as f64;
    let w = ((img.width as f64 * s).round() as u32).clamp(1, max_dim);
    let h = ((img.height as f64 * s).round() as u32).clamp(1, max_dim);
    resize(&img, w, h)
}

/// High quality resize (area averaging when shrinking, bilinear when enlarging).
/// Filtering is done on premultiplied alpha. A zero target dimension yields an
/// empty image.
pub fn resize(img: &Image, width: u32, height: u32) -> Image {
    resize::resize(img, width, height)
}

/// Encodes a baseline JPEG (4:2:0, quality 1..=100). Used for server-side
/// thumbnails. The alpha channel is ignored.
pub fn encode_jpeg(img: &Image, quality: u8) -> Vec<u8> {
    jpeg::encode::encode(img, quality)
}

/// Encodes a PNG (RGBA8, or RGB8 when the image is fully opaque; zlib
/// compressed with per-row adaptive filtering).
pub fn encode_png(img: &Image) -> Vec<u8> {
    png::encode(img)
}
