//! Image decoding (PNG, JPEG; optionally BMP/GIF/WebP), resizing and simple
//! encoding, std only.

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
    let _ = data;
    Err(Error::Unsupported("not implemented yet"))
}

/// Decodes and downscales so that neither side exceeds `max_dim` (keeping
/// the aspect ratio). Implementations may use cheaper decoding paths (e.g.
/// JPEG DCT scaling) as long as the result quality is comparable.
pub fn decode_max(data: &[u8], max_dim: u32) -> Result<Image, Error> {
    let img = decode(data)?;
    if img.width <= max_dim && img.height <= max_dim {
        return Ok(img);
    }
    let s = max_dim as f64 / img.width.max(img.height) as f64;
    let w = ((img.width as f64 * s).round() as u32).max(1);
    let h = ((img.height as f64 * s).round() as u32).max(1);
    Ok(resize(&img, w, h))
}

/// High quality resize (area averaging when shrinking, bilinear when enlarging).
pub fn resize(img: &Image, width: u32, height: u32) -> Image {
    let _ = (img, width, height);
    unimplemented!()
}

/// Encodes a baseline JPEG (4:2:0, quality 1..=100). Used for server-side
/// thumbnails.
pub fn encode_jpeg(img: &Image, quality: u8) -> Vec<u8> {
    let _ = (img, quality);
    unimplemented!()
}

/// Encodes a PNG (RGBA8, zlib compressed).
pub fn encode_png(img: &Image) -> Vec<u8> {
    let _ = img;
    unimplemented!()
}

/// zlib / raw deflate decompression (also used by other crates, e.g. for
/// compressed ID3 frames).
pub mod inflate {
    /// Decompresses a zlib stream (RFC 1950).
    pub fn zlib_decompress(data: &[u8]) -> Result<Vec<u8>, super::Error> {
        let _ = data;
        Err(super::Error::Unsupported("not implemented yet"))
    }
    /// Decompresses a raw deflate stream (RFC 1951).
    pub fn inflate(data: &[u8]) -> Result<Vec<u8>, super::Error> {
        let _ = data;
        Err(super::Error::Unsupported("not implemented yet"))
    }
}
