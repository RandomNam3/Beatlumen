//! Image decoding (PNG/APNG, JPEG, BMP, GIF, WebP, TIFF), animation,
//! resizing and simple encoding (PNG, baseline JPEG), std only.
//!
//! The crate is used for cover art and background images, so the decoders are
//! tuned for photos and screenshots and are lenient with damaged files
//! (truncated images decode partially where possible) but never panic on
//! malformed input.
//!
//! Large images are scaled while they are decoded: every decoder hands its
//! rows to a streaming area-averaging scaler ([`decode_fit`]), so an image
//! up to [`MAX_SOURCE_PIXELS`] (e.g. 16K, 15360 × 8640) never exists at
//! full size in memory. Animations ([`Animation`]) compose their frames at
//! the target size.
//!
//! Module overview:
//! * [`inflate`] / [`deflate`]: zlib (RFC 1950) and deflate (RFC 1951),
//!   also incremental,
//! * `scale`: streaming scaler, target sizes ([`Fit`]), frame compositing,
//! * `png`: PNG decoder (all color types / bit depths, Adam7, APNG) and
//!   encoder,
//! * `jpeg`: baseline + progressive Huffman JPEG decoder (DCT scaling,
//!   EXIF orientation) and baseline encoder,
//! * `bmp`, `gif`, `webp` (VP8L lossless and VP8 lossy incl. alpha and
//!   animation), `tiff`,
//! * `anim`: frame sequencing and disposal for GIF, APNG and WebP,
//! * `resize`: area averaging / bilinear resampling with premultiplied alpha.
#![forbid(unsafe_code)]
// `is_multiple_of` needs Rust 1.87; the workspace MSRV is 1.80. Index loops
// mirror the specifications' pseudo code (VP8 prediction, transforms).
#![allow(clippy::manual_is_multiple_of, clippy::needless_range_loop)]

mod anim;
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
mod scale;
mod tiff;
mod webp;

pub use anim::{Animation, Frame};
pub use scale::Fit;

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
    Tiff,
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

/// Largest number of pixels a decoder output may have (32 Mpixel = 128 MiB
/// of RGBA, e.g. 8192 × 4096). [`decode`] rejects larger images; scaled
/// decoding ([`decode_fit`]) never produces more.
pub const MAX_PIXELS: u64 = 1 << 25;

/// Largest source image the scaling decoders accept (2^28 pixels, e.g.
/// 16384 × 16384). Such images are never held at full size; together with
/// the per-format plausibility checks (a tiny file cannot claim a huge
/// image) this keeps "decompression bombs" from exhausting memory.
pub const MAX_SOURCE_PIXELS: u64 = 1 << 28;

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
    } else if data.starts_with(b"II\x2a\x00")
        || data.starts_with(b"MM\x00\x2a")
        || data.starts_with(b"II\x2b\x00")
        || data.starts_with(b"MM\x00\x2b")
    {
        Some(Format::Tiff)
    } else {
        None
    }
}

/// What an image file contains, read from its headers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Info {
    pub format: Format,
    /// Upright size (JPEG / TIFF orientation applied).
    pub width: u32,
    pub height: u32,
    /// Number of animation frames (1 for still images).
    pub frames: usize,
}

impl Info {
    pub fn is_animated(&self) -> bool {
        self.frames > 1
    }
}

/// Reads format, size and frame count without decoding the pixels.
pub fn info(data: &[u8]) -> Result<Info, Error> {
    let format = probe_format(data).ok_or(Error::Unsupported("unknown image format"))?;
    let (w, h, frames) = match format {
        Format::Png => {
            let p = png::parse(data)?;
            let (w, h) = p.size();
            (w, h, p.frames.len().max(1))
        }
        Format::Jpeg => {
            let (w, h) = jpeg::dimensions(data)?;
            let img_o = jpeg_orientation(data);
            if img_o >= 5 {
                (h, w, 1)
            } else {
                (w, h, 1)
            }
        }
        Format::Bmp => {
            let (w, h) = bmp::dimensions(data)?;
            (w, h, 1)
        }
        Format::Gif => {
            let p = gif::parse(data)?;
            (p.width as u32, p.height as u32, p.frames.len().max(1))
        }
        Format::WebP => {
            let p = webp::parse(data)?;
            (p.width as u32, p.height as u32, p.frames.len().max(1))
        }
        Format::Tiff => {
            let p = tiff::parse(data)?;
            let (w, h) = (p.width as u32, p.height as u32);
            if p.orientation >= 5 {
                (h, w, 1)
            } else {
                (w, h, 1)
            }
        }
    };
    Ok(Info {
        format,
        width: w,
        height: h,
        frames,
    })
}

fn jpeg_orientation(data: &[u8]) -> u16 {
    // Decoding a 1x1 thumbnail would do the same; read the header instead.
    jpeg::orientation_of(data)
}

/// Upright image size, read from the headers.
pub fn dimensions(data: &[u8]) -> Result<(u32, u32), Error> {
    info(data).map(|i| (i.width, i.height))
}

/// True for animations with more than one frame (GIF, APNG, WebP).
pub fn is_animated(data: &[u8]) -> bool {
    info(data).is_ok_and(|i| i.is_animated())
}

/// Decodes an image in any supported format at full size (orientation
/// applied; first frame of animations). Images larger than [`MAX_PIXELS`]
/// are rejected; use [`decode_fit`] for those.
pub fn decode(data: &[u8]) -> Result<Image, Error> {
    match probe_format(data) {
        Some(Format::Png) => png::decode(data),
        Some(Format::Jpeg) => jpeg::decode(data),
        Some(Format::Bmp) => {
            let (w, h) = bmp::dimensions(data)?;
            if w as u64 * h as u64 > MAX_PIXELS {
                return Err(Error::Unsupported("image too large"));
            }
            bmp::decode_fit(data, &Fit::full())
        }
        Some(Format::Gif) => gif::decode(data),
        Some(Format::WebP) => webp::decode(data),
        Some(Format::Tiff) => tiff::decode(data),
        None => Err(Error::Unsupported("unknown image format")),
    }
}

/// Decodes and downscales so that neither side exceeds `max_dim` (keeping
/// the aspect ratio).
pub fn decode_max(data: &[u8], max_dim: u32) -> Result<Image, Error> {
    decode_fit(data, &Fit::within(max_dim.max(1)))
}

/// Like [`decode_fit`], for a file the caller hands over: PNG data is
/// rearranged in place instead of being copied (saves the size of the file
/// for large PNGs).
pub fn decode_fit_owned(data: Vec<u8>, fit: &Fit) -> Result<Image, Error> {
    if probe_format(&data) == Some(Format::Png) {
        let p = png::parse(&data)?;
        let (w, h) = p.size();
        let (tw, th) = fit.target(w, h);
        return p.decode_fit_owned(data, tw, th);
    }
    decode_fit(&data, fit)
}

/// Decodes an image scaled to the size `fit` gives for it (first frame of
/// animations). The image is scaled while it is decoded (JPEG mostly in
/// the DCT domain), so memory stays near the size of the result.
pub fn decode_fit(data: &[u8], fit: &Fit) -> Result<Image, Error> {
    match probe_format(data) {
        Some(Format::Png) => {
            let p = png::parse(data)?;
            let (w, h) = p.size();
            let (tw, th) = fit.target(w, h);
            p.decode_fit(data, tw, th)
        }
        Some(Format::Jpeg) => jpeg::decode_fit(data, fit),
        Some(Format::Bmp) => bmp::decode_fit(data, fit),
        Some(Format::Gif) => gif::parse(data)?.decode_fit(data, fit),
        Some(Format::WebP) => webp::parse(data)?.decode_fit(data, fit),
        Some(Format::Tiff) => tiff::parse(data)?.decode_fit(data, fit),
        None => Err(Error::Unsupported("unknown image format")),
    }
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
