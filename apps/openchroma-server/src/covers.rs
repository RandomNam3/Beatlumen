//! Cover art lookup and JPEG thumbnail generation (also used for scaled
//! background images).
//!
//! Lookup order for a track: its own embedded picture (front cover
//! preferred), the folder image of its directory, the embedded picture of
//! the directory's first track.

use crate::library::{Entry, Index};
use crate::util;
use std::path::Path;

/// Largest image file read for thumbnails.
pub const MAX_IMAGE_FILE: u64 = 64 * 1024 * 1024;

/// JPEG quality of generated thumbnails.
pub const JPEG_QUALITY: u8 = 88;

/// Decodes an image, scales it to fit `max_dim` and encodes a JPEG.
/// Transparent images are composited onto black. Decoder panics are caught.
pub fn make_thumbnail(data: &[u8], max_dim: u32) -> Result<Vec<u8>, String> {
    util::catch("image decoder", || -> Result<Vec<u8>, String> {
        let mut img = oc_image::decode_max(data, max_dim).map_err(|e| e.to_string())?;
        if !img.is_opaque() {
            for p in img.pixels.chunks_exact_mut(4) {
                let a = p[3] as u32;
                for c in &mut p[..3] {
                    *c = ((*c as u32 * a + 127) / 255) as u8;
                }
                p[3] = 255;
            }
        }
        Ok(oc_image::encode_jpeg(&img, JPEG_QUALITY))
    })?
}

/// The embedded picture of an audio file (front cover preferred), if it is
/// in a decodable image format.
pub fn embedded_picture(path: &Path, ext: &str) -> Option<Vec<u8>> {
    let mut f = std::fs::File::open(path).ok()?;
    let info = util::catch("tag reader", || oc_media::read_info(&mut f, Some(ext), true)).ok()?.ok()?;
    let mut pics = info.pictures;
    let usable = |p: &oc_media::Picture| oc_image::probe_format(&p.data).is_some();
    let i = pics.iter().position(|p| p.kind == 3 && usable(p)).or_else(|| pics.iter().position(usable))?;
    Some(pics.swap_remove(i).data)
}

/// Reads an image file (size limited).
pub fn read_image_file(path: &Path) -> Option<Vec<u8>> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_IMAGE_FILE {
        return None;
    }
    std::fs::read(path).ok()
}

/// Finds the source image bytes for a track's cover.
pub fn cover_source(idx: &Index, e: &Entry) -> Option<Vec<u8>> {
    if let Some(d) = embedded_picture(&e.abs, &e.ext()) {
        return Some(d);
    }
    if let Some(d) = idx.dir_cover_path(e).and_then(|p| read_image_file(&p)) {
        return Some(d);
    }
    let other = e.dir_cover_track.as_deref().and_then(|id| idx.track(id)).filter(|o| o.track.id != e.track.id)?;
    embedded_picture(&other.abs, &other.ext())
}

/// A stamp that changes whenever any possible cover source of the track
/// changes (track file, folder image, fallback track).
pub fn cover_stamp(idx: &Index, e: &Entry) -> u64 {
    let mut s = format!("{}:{}", e.mtime_ns, e.track.size);
    if let Some(m) = idx.dir_cover_path(e).and_then(|p| std::fs::metadata(p).ok()) {
        s.push_str(&format!(":{}:{}", util::mtime_ns(&m), m.len()));
    }
    if let Some(o) = e.dir_cover_track.as_deref().and_then(|id| idx.track(id)) {
        s.push_str(&format!(":{}:{}", o.mtime_ns, o.track.size));
    }
    util::fnv1a64(s.as_bytes())
}

/// Parses a cache file name `<id>_<size>_<stamp>.jpg`.
pub fn parse_cache_name(name: &str) -> Option<(&str, u32, u64)> {
    let base = name.strip_suffix(".jpg")?;
    let mut it = base.split('_');
    let (id, size, stamp) = (it.next()?, it.next()?, it.next()?);
    if it.next().is_some() || !util::is_id(id) {
        return None;
    }
    Some((id, size.parse().ok()?, u64::from_str_radix(stamp, 16).ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_names() {
        assert_eq!(parse_cache_name("0123456789abcdef_300_00000000000000ff.jpg"), Some(("0123456789abcdef", 300, 255)));
        assert!(parse_cache_name("x_300_ff.jpg").is_none());
        assert!(parse_cache_name("0123456789abcdef_300.jpg").is_none());
    }

    #[test]
    fn thumbnail_of_garbage_is_an_error() {
        assert!(make_thumbnail(b"not an image", 100).is_err());
        assert!(make_thumbnail(&[0x89, b'P', b'N', b'G', b'\r', b'\n', 0x1a, b'\n', 0, 0], 100).is_err());
    }
}
