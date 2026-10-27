//! Background jobs whose results are collected on the main thread.

use std::sync::{Arc, Mutex};

/// A result slot shared between a worker and the main thread.
pub struct Mailbox<T> {
    inner: Arc<Mutex<Vec<T>>>,
}

impl<T> Clone for Mailbox<T> {
    fn clone(&self) -> Self {
        Mailbox { inner: self.inner.clone() }
    }
}

impl<T> Default for Mailbox<T> {
    fn default() -> Self {
        Mailbox { inner: Arc::new(Mutex::new(Vec::new())) }
    }
}

impl<T> Mailbox<T> {
    pub fn post(&self, v: T) {
        if let Ok(mut g) = self.inner.lock() {
            g.push(v);
        }
    }
    pub fn take(&self) -> Vec<T> {
        self.inner.lock().map(|mut g| std::mem::take(&mut *g)).unwrap_or_default()
    }
}

/// Decodes a whole track to mono for analysis.
pub fn decode_mono(src: Box<dyn oc_media::Source>, ext: Option<&str>) -> Result<(Vec<f32>, u32), String> {
    let mut dec = oc_media::open(src, ext).map_err(|e| e.to_string())?;
    let spec = dec.spec();
    let channels = spec.channels.max(1) as usize;
    let mut mono = Vec::new();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let more = dec.next_chunk(&mut buf).map_err(|e| e.to_string())?;
        oc_analysis::to_mono(&buf, channels, &mut mono);
        if !more {
            break;
        }
        if mono.len() > spec.sample_rate as usize * 60 * 60 * 3 {
            // Stop after three hours of audio.
            break;
        }
    }
    Ok((mono, spec.sample_rate))
}

/// Extension of a path or URL (lower case, without dot).
pub fn extension(path: &str) -> Option<String> {
    let name = path.rsplit(['/', '\\']).next()?;
    let name = name.split(['?', '#']).next()?;
    name.rsplit_once('.').map(|(_, e)| e.to_ascii_lowercase()).filter(|e| !e.is_empty() && e.len() <= 5)
}

#[cfg(test)]
mod tests {
    #[test]
    fn ext() {
        assert_eq!(super::extension("/a/b.c/song.FLAC").as_deref(), Some("flac"));
        assert_eq!(super::extension("http://x/api/tracks/1/stream?token=a.b").as_deref(), None);
        assert_eq!(super::extension("C:\\Music\\a.mp3").as_deref(), Some("mp3"));
    }
}
