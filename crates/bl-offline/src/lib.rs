//! Offline copies of songs from the server: one file per song (`.blo`)
//! with everything the app needs to play it without a connection.
//!
//! ```text
//! 0      8  magic "BLOFFLN" + format version (1)
//! 8      4  header capacity (u32 LE)
//! 12     4  header length (u32 LE)
//! 16  cap   header: JSON, padded with spaces
//! ...       sections: cover, analysis, then the audio (aligned to 4 KiB)
//! ```
//!
//! The header holds the track's description, the song text, the karaoke
//! data and where each section is (offset, length, CRC-32). The audio is the original file as
//! the server has it, not re-encoded; it is read through a window on the
//! container, so playback seeks within the file directly. Files are written
//! under a temporary name of their own (`.<name>.<process>-<n>.part`) and
//! renamed when complete, so an interrupted download never looks like a
//! finished one and two writers (threads, or two running apps) never write
//! into the same file. A finished container is never changed in place:
//! readers (playback, the app's list) always see a whole one.

#![forbid(unsafe_code)]

use bl_json::Value;
use std::fs::File;
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

pub const MAGIC: &[u8; 8] = b"BLOFFLN\x01";
/// File name extension of offline copies.
pub const EXTENSION: &str = "blo";
const PREFIX_LEN: u64 = 16;
const ALIGN: u64 = 4096;
/// Spare header room for later song text changes.
const HEADER_SLACK: usize = 8 * 1024;
const MAX_HEADER: u32 = 4 << 20;

/// A section of the container.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Section {
    pub offset: u64,
    pub len: u64,
    pub crc32: u32,
}

impl Section {
    fn to_json(self) -> Value {
        Value::object()
            .with("offset", self.offset)
            .with("len", self.len)
            .with("crc32", self.crc32 as u64)
    }

    fn from_json(v: &Value) -> Option<Section> {
        Some(Section {
            offset: v.get("offset").as_u64()?,
            len: v.get("len").as_u64()?,
            crc32: u32::try_from(v.get("crc32").as_u64()?).ok()?,
        })
    }
}

/// What a container holds besides the sections' bytes.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    /// The track as the server describes it (`bl_net::api::Track` JSON).
    pub track: Value,
    /// File name extension of the audio (decoder hint), e.g. "flac".
    pub ext: String,
    /// When the copy was made (seconds since the Unix epoch).
    pub saved: u64,
    /// Song text and its source, if the track has one.
    pub lyrics: Option<(String, String)>,
    /// Karaoke data (`bl_pitch::KaraokeTrack` JSON as the server stores
    /// it), if the track has some.
    pub karaoke: Option<Value>,
    /// Cache key of the stored analysis (it depends on the analysis settings).
    pub analysis_key: String,
    pub audio: Section,
    pub cover: Option<Section>,
    pub analysis: Option<Section>,
}

impl Header {
    fn to_json(&self) -> Value {
        let mut v = Value::object()
            .with("track", self.track.clone())
            .with("ext", self.ext.as_str())
            .with("saved", self.saved)
            .with("audio", self.audio.to_json());
        if let Some((text, source)) = &self.lyrics {
            v = v.with(
                "lyrics",
                Value::object()
                    .with("text", text.as_str())
                    .with("source", source.as_str()),
            );
        }
        if let Some(k) = &self.karaoke {
            v = v.with("karaoke", k.clone());
        }
        if let Some(c) = self.cover {
            v = v.with("cover", c.to_json());
        }
        if let Some(a) = self.analysis {
            v = v
                .with("analysis", a.to_json())
                .with("analysisKey", self.analysis_key.as_str());
        }
        v
    }

    fn from_json(v: &Value) -> Option<Header> {
        let lyrics = v.get("lyrics");
        Some(Header {
            track: v.get("track").clone(),
            ext: v.str_or("ext", "").to_string(),
            saved: v.get("saved").as_u64().unwrap_or(0),
            lyrics: lyrics
                .get("text")
                .as_str()
                .map(|t| (t.to_string(), lyrics.str_or("source", "").to_string())),
            karaoke: v
                .get("karaoke")
                .as_object()
                .map(|_| v.get("karaoke").clone()),
            analysis_key: v.str_or("analysisKey", "").to_string(),
            audio: Section::from_json(v.get("audio"))?,
            cover: Section::from_json(v.get("cover")),
            analysis: Section::from_json(v.get("analysis")),
        })
    }

    /// The track's id from its description.
    pub fn id(&self) -> &str {
        self.track.str_or("id", "")
    }
}

/// CRC-32 (IEEE), computed incrementally.
#[derive(Clone, Copy)]
pub struct Crc32(u32);

impl Default for Crc32 {
    fn default() -> Self {
        Crc32(0xffff_ffff)
    }
}

impl Crc32 {
    pub fn update(&mut self, data: &[u8]) {
        static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
        let t = TABLE.get_or_init(|| {
            let mut t = [0u32; 256];
            for (i, e) in t.iter_mut().enumerate() {
                let mut c = i as u32;
                for _ in 0..8 {
                    c = if c & 1 != 0 {
                        0xedb8_8320 ^ (c >> 1)
                    } else {
                        c >> 1
                    };
                }
                *e = c;
            }
            t
        });
        for &b in data {
            self.0 = t[((self.0 ^ b as u32) & 0xff) as usize] ^ (self.0 >> 8);
        }
    }

    pub fn finish(self) -> u32 {
        !self.0
    }
}

fn header_bytes(h: &Header, cap: Option<usize>) -> io::Result<(Vec<u8>, usize)> {
    let mut json = h.to_json().to_json().into_bytes();
    let cap = cap.unwrap_or(json.len() + HEADER_SLACK);
    if json.len() > cap || cap > MAX_HEADER as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "header does not fit",
        ));
    }
    let len = json.len();
    json.resize(cap, b' ');
    let mut out = Vec::with_capacity(PREFIX_LEN as usize + cap);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&(cap as u32).to_le_bytes());
    out.extend_from_slice(&(len as u32).to_le_bytes());
    out.extend_from_slice(&json);
    Ok((out, cap))
}

/// Writes an offline copy at `path`: cover and analysis from memory, the
/// audio streamed from `audio` (e.g. the download). `progress` gets the
/// audio bytes written so far and may return false to cancel.
pub fn write(
    path: &Path,
    header: Header,
    cover: Option<&[u8]>,
    analysis: Option<&[u8]>,
    audio: &mut dyn Read,
    progress: &mut dyn FnMut(u64) -> bool,
) -> io::Result<Header> {
    write_with(path, header, cover, analysis, audio, progress, None)
}

/// [`write`] with the header room `cap` when the header fits into it (the
/// sections then keep the offsets of a container written with that room).
fn write_with(
    path: &Path,
    mut header: Header,
    cover: Option<&[u8]>,
    analysis: Option<&[u8]>,
    audio: &mut dyn Read,
    progress: &mut dyn FnMut(u64) -> bool,
    cap: Option<usize>,
) -> io::Result<Header> {
    let part = temp_path(path, "part");
    let result = (|| {
        let mut f = BufWriter::with_capacity(256 * 1024, File::create(&part)?);
        // Placeholder header with room for the sections' entries; written
        // again at the end.
        header.audio = Section {
            offset: u64::MAX,
            len: u64::MAX,
            crc32: u32::MAX,
        };
        header.cover = cover.map(|_| Section {
            offset: u64::MAX,
            len: u64::MAX,
            crc32: u32::MAX,
        });
        header.analysis = analysis.map(|_| Section {
            offset: u64::MAX,
            len: u64::MAX,
            crc32: u32::MAX,
        });
        let (bytes, cap) = cap
            .and_then(|c| header_bytes(&header, Some(c)).ok())
            .map_or_else(|| header_bytes(&header, None), Ok)?;
        f.write_all(&bytes)?;
        let mut pos = bytes.len() as u64;
        let put = |f: &mut BufWriter<File>, pos: &mut u64, data: &[u8]| -> io::Result<Section> {
            let mut c = Crc32::default();
            c.update(data);
            f.write_all(data)?;
            let s = Section {
                offset: *pos,
                len: data.len() as u64,
                crc32: c.finish(),
            };
            *pos += data.len() as u64;
            Ok(s)
        };
        header.cover = cover.map(|c| put(&mut f, &mut pos, c)).transpose()?;
        header.analysis = analysis.map(|a| put(&mut f, &mut pos, a)).transpose()?;
        let start = pos.div_ceil(ALIGN) * ALIGN;
        f.write_all(&vec![0u8; (start - pos) as usize])?;
        let mut crc = Crc32::default();
        let mut len = 0u64;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            let n = match audio.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            };
            crc.update(&buf[..n]);
            f.write_all(&buf[..n])?;
            len += n as u64;
            if !progress(len) {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
            }
        }
        if len == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "no audio"));
        }
        header.audio = Section {
            offset: start,
            len,
            crc32: crc.finish(),
        };
        let (bytes, _) = header_bytes(&header, Some(cap))?;
        let mut f = f.into_inner().map_err(|e| e.into_error())?;
        f.seek(SeekFrom::Start(0))?;
        f.write_all(&bytes)?;
        f.sync_all()?;
        Ok(())
    })();
    match result {
        Ok(()) => {
            std::fs::rename(&part, path)?;
            Ok(header)
        }
        Err(e) => {
            let _ = std::fs::remove_file(&part);
            Err(e)
        }
    }
}

/// A temporary file next to `path` that no other writer uses:
/// `.<name>.<process id>-<counter>.<kind>`.
fn temp_path(path: &Path, kind: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    path.with_file_name(format!(
        ".{name}.{}-{}.{kind}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Whether `name` is a temporary file of this crate (or of older versions,
/// which wrote `<name>.part` and `<name>.new`).
pub fn is_temp_name(name: &str) -> bool {
    name.ends_with(".part") || name.ends_with(".blo.new") || (name.starts_with('.') && name.ends_with(".new"))
}

/// Removes temporary files in `dir` left by an interrupted write (a crash,
/// a killed app) that were last written more than `age` ago.
pub fn remove_stale_temp(dir: &Path, age: Duration) {
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let name = e.file_name().to_string_lossy().into_owned();
        let old = e
            .metadata()
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .is_some_and(|d| d > age);
        if is_temp_name(&name) && old {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

fn bad(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Reads the header of an offline copy.
pub fn read_header(path: &Path) -> io::Result<Header> {
    read_header_from(&mut File::open(path)?).map(|(h, _)| h)
}

/// The header and its room, from an open container.
fn read_header_from(f: &mut File) -> io::Result<(Header, usize)> {
    let file_len = f.metadata()?.len();
    let mut prefix = [0u8; PREFIX_LEN as usize];
    f.read_exact(&mut prefix)?;
    if &prefix[..8] != MAGIC {
        return Err(bad("not an offline copy"));
    }
    let cap = u32::from_le_bytes(prefix[8..12].try_into().expect("4 bytes"));
    let len = u32::from_le_bytes(prefix[12..16].try_into().expect("4 bytes"));
    if cap > MAX_HEADER || len > cap {
        return Err(bad("bad header size"));
    }
    let mut json = vec![0u8; len as usize];
    f.read_exact(&mut json)?;
    let v = bl_json::parse_bytes(&json).map_err(|_| bad("bad header"))?;
    let h = Header::from_json(&v).ok_or_else(|| bad("bad header"))?;
    let data_start = PREFIX_LEN + cap as u64;
    for s in [Some(h.audio), h.cover, h.analysis].into_iter().flatten() {
        if s.offset < data_start || s.offset.checked_add(s.len).is_none_or(|e| e > file_len) {
            return Err(bad("section outside the file"));
        }
    }
    Ok((h, cap as usize))
}

/// Opens an offline copy for playback: its header and its audio, both from
/// the same open file, so they match even when the container is replaced
/// meanwhile (e.g. by a song text change).
pub fn open(path: &Path) -> io::Result<(Header, Window)> {
    let mut f = File::open(path)?;
    let (h, _) = read_header_from(&mut f)?;
    let w = Window::new(f, h.audio.offset, h.audio.len)?;
    Ok((h, w))
}

/// Reads the section `pick` chooses from the current container (header and
/// section from the same open file), checking its CRC-32.
pub fn read_current_section(
    path: &Path,
    pick: impl FnOnce(&Header) -> Option<Section>,
) -> io::Result<Option<Vec<u8>>> {
    let mut f = File::open(path)?;
    let (h, _) = read_header_from(&mut f)?;
    pick(&h).map(|s| read_section_from(&mut f, s)).transpose()
}

/// Reads a whole (small) section, checking its CRC-32.
pub fn read_section(path: &Path, s: Section) -> io::Result<Vec<u8>> {
    read_section_from(&mut File::open(path)?, s)
}

fn read_section_from(f: &mut File, s: Section) -> io::Result<Vec<u8>> {
    if s.len > 64 << 20 {
        return Err(bad("section too large"));
    }
    f.seek(SeekFrom::Start(s.offset))?;
    let mut data = vec![0u8; s.len as usize];
    f.read_exact(&mut data)?;
    let mut c = Crc32::default();
    c.update(&data);
    if c.finish() != s.crc32 {
        return Err(bad("section damaged"));
    }
    Ok(data)
}

/// The audio of an offline copy as a seekable stream.
pub fn audio(path: &Path, h: &Header) -> io::Result<Window> {
    Window::new(File::open(path)?, h.audio.offset, h.audio.len)
}

/// Checks the audio against its CRC-32 (reads the whole file).
pub fn verify(path: &Path) -> io::Result<bool> {
    let h = read_header(path)?;
    let mut w = audio(path, &h)?;
    let mut c = Crc32::default();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        let n = w.read(&mut buf)?;
        if n == 0 {
            break;
        }
        c.update(&buf[..n]);
    }
    Ok(c.finish() == h.audio.crc32)
}

/// Replaces the song text (see [`update_header`]).
pub fn set_lyrics(path: &Path, lyrics: Option<(String, String)>) -> io::Result<()> {
    update_header(path, |h| h.lyrics = lyrics)
}

/// Replaces the karaoke data (as [`set_lyrics`]).
pub fn set_karaoke(path: &Path, karaoke: Option<Value>) -> io::Result<()> {
    update_header(path, |h| h.karaoke = karaoke)
}

/// Changes the header. The container is written anew under a temporary
/// name and renamed over the old one, never changed in place: playback
/// may be reading the file, the app may read its header, and a crash in
/// the middle must not leave a header that looks damaged (damaged copies
/// are removed). Changes in this process run one after another.
pub fn update_header(path: &Path, change: impl FnOnce(&mut Header)) -> io::Result<()> {
    static UPDATES: Mutex<()> = Mutex::new(());
    let _one_at_a_time = UPDATES.lock().unwrap_or_else(|e| e.into_inner());
    let mut f = File::open(path)?;
    let (mut h, cap) = read_header_from(&mut f)?;
    change(&mut h);
    let audio_crc = h.audio.crc32;
    let cover = h.cover.map(|s| read_section_from(&mut f, s)).transpose()?;
    let analysis = h.analysis.map(|s| read_section_from(&mut f, s)).transpose()?;
    let mut src = Window::new(f, h.audio.offset, h.audio.len)?;
    let tmp = temp_path(path, "new");
    // The same room when the header still fits: every section stays where
    // it was, so a header read before (kept by the app) still points right.
    let result = write_with(
        &tmp,
        h,
        cover.as_deref(),
        analysis.as_deref(),
        &mut src,
        &mut |_| true,
        Some(cap),
    )
    .and_then(|written| {
        // A copy whose audio was damaged stays as it is (and is found
        // out by `verify`) instead of getting a fresh checksum.
        if written.audio.crc32 != audio_crc {
            return Err(bad("audio damaged"));
        }
        std::fs::rename(&tmp, path)
    });
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A window `[offset, offset + len)` of a file, read and seeked as if it
/// were the whole file.
pub struct Window {
    f: io::BufReader<File>,
    offset: u64,
    len: u64,
    pos: u64,
}

impl Window {
    pub fn new(f: File, offset: u64, len: u64) -> io::Result<Window> {
        let mut f = io::BufReader::with_capacity(64 * 1024, f);
        f.seek(SeekFrom::Start(offset))?;
        Ok(Window {
            f,
            offset,
            len,
            pos: 0,
        })
    }
}

impl Read for Window {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let left = self.len.saturating_sub(self.pos);
        if left == 0 {
            return Ok(0);
        }
        let n = buf.len().min(left as usize);
        let n = self.f.read(&mut buf[..n])?;
        self.pos += n as u64;
        Ok(n)
    }
}

impl Seek for Window {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let target = match to {
            SeekFrom::Start(p) => p as i128,
            SeekFrom::Current(d) => self.pos as i128 + d as i128,
            SeekFrom::End(d) => self.len as i128 + d as i128,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before start",
            ));
        }
        let target = (target as u64).min(self.len);
        self.f.seek(SeekFrom::Start(self.offset + target))?;
        self.pos = target;
        Ok(target)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("bl-offline-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d.join("song.blo")
    }

    /// Files other than `path` in its folder.
    fn temp_files(path: &Path) -> Vec<String> {
        std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| Some(n.as_str()) != path.file_name().and_then(|f| f.to_str()))
            .collect()
    }

    /// Lyrics change on several threads while others read the header and
    /// the audio, as when a lyrics sync and playback meet: every read sees
    /// a whole container, nothing is left behind.
    #[test]
    fn header_changes_while_reading() {
        let path = tmp("race");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i * 7 % 251) as u8).collect();
        let h = Header {
            track: Value::object().with("id", "race1"),
            ext: "flac".into(),
            ..Default::default()
        };
        write(&path, h, Some(b"cover"), None, &mut &data[..], &mut |_| true).unwrap();
        let texts: Vec<String> = (0..6).map(|i| format!("{i}").repeat(1 + i * 5000)).collect();
        let stop = std::sync::atomic::AtomicBool::new(false);
        std::thread::scope(|s| {
            let readers: Vec<_> = (0..2)
                .map(|_| {
                    s.spawn(|| {
                        let mut n = 0;
                        while !stop.load(Ordering::Relaxed) {
                            match open(&path) {
                                Ok((h, mut w)) => {
                                    let mut a = Vec::new();
                                    w.read_to_end(&mut a).unwrap();
                                    // Replaced meanwhile: the open file is still whole.
                                    assert_eq!(a, data);
                                    assert!(h.lyrics.is_none_or(|(t, _)| texts.contains(&t)));
                                    let c = read_current_section(&path, |h| h.cover).unwrap();
                                    assert_eq!(c.as_deref(), Some(&b"cover"[..]));
                                    n += 1;
                                }
                                Err(_) if cfg!(windows) => {}
                                Err(e) => panic!("a reader saw a broken container: {e}"),
                            }
                        }
                        n
                    })
                })
                .collect();
            let writers: Vec<_> = texts
                .iter()
                .map(|t| {
                    let path = &path;
                    s.spawn(move || {
                        for _ in 0..3 {
                            set_lyrics(path, Some((t.clone(), "test".into()))).unwrap();
                        }
                    })
                })
                .collect();
            for w in writers {
                w.join().unwrap();
            }
            stop.store(true, Ordering::Relaxed);
            for r in readers {
                let n: i32 = r.join().unwrap();
                assert!(n > 0 || cfg!(windows));
            }
        });
        let h = read_header(&path).unwrap();
        assert!(texts.contains(&h.lyrics.unwrap().0));
        assert!(verify(&path).unwrap());
        assert_eq!(read_section(&path, h.cover.unwrap()).unwrap(), b"cover");
        assert_eq!(temp_files(&path), Vec::<String>::new());
    }

    #[test]
    fn stale_temporary_files_are_removed() {
        let path = tmp("stale");
        let dir = path.parent().unwrap().to_path_buf();
        for n in [".song.blo.1-2.part", "song.blo.part", "song.blo.new", ".x.blo.3-4.new"] {
            std::fs::write(dir.join(n), b"x").unwrap();
        }
        std::fs::write(dir.join("keep.blo"), b"x").unwrap();
        remove_stale_temp(&dir, Duration::from_secs(3600));
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 5, "fresh files stay");
        std::thread::sleep(Duration::from_millis(20));
        remove_stale_temp(&dir, Duration::from_millis(1));
        let left: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(left, vec!["keep.blo".to_string()]);
    }

    #[test]
    fn crc32_matches_reference() {
        let mut c = Crc32::default();
        c.update(b"123456789");
        assert_eq!(c.finish(), 0xcbf4_3926);
    }

    #[test]
    fn write_read_seek_and_update() {
        let path = tmp("rw");
        let data: Vec<u8> = (0..300_001u32).map(|i| (i * 31 % 253) as u8).collect();
        let h = Header {
            track: Value::object().with("id", "abc123").with("title", "Song"),
            ext: "flac".into(),
            saved: 1,
            lyrics: Some(("[00:01.00]Hi".into(), "lrc".into())),
            analysis_key: "k1".into(),
            ..Default::default()
        };
        let mut seen = 0;
        let written = write(
            &path,
            h,
            Some(b"cover"),
            Some(b"analysis"),
            &mut &data[..],
            &mut |n| {
                seen = n;
                true
            },
        )
        .unwrap();
        assert_eq!(seen, data.len() as u64);
        assert_eq!(temp_files(&path), Vec::<String>::new());
        let h = read_header(&path).unwrap();
        assert_eq!(h, written);
        assert_eq!(h.id(), "abc123");
        assert_eq!(h.audio.offset % ALIGN, 0);
        assert_eq!(read_section(&path, h.cover.unwrap()).unwrap(), b"cover");
        assert_eq!(
            read_section(&path, h.analysis.unwrap()).unwrap(),
            b"analysis"
        );
        assert!(verify(&path).unwrap());

        // The audio window reads and seeks like the original file.
        let mut w = audio(&path, &h).unwrap();
        let mut all = Vec::new();
        w.read_to_end(&mut all).unwrap();
        assert_eq!(all, data);
        assert_eq!(w.seek(SeekFrom::End(-10)).unwrap(), data.len() as u64 - 10);
        let mut tail = Vec::new();
        w.read_to_end(&mut tail).unwrap();
        assert_eq!(tail, data[data.len() - 10..]);
        w.seek(SeekFrom::Start(1000)).unwrap();
        let mut b = [0u8; 5];
        w.read_exact(&mut b).unwrap();
        assert_eq!(b, data[1000..1005]);
        assert!(w.seek(SeekFrom::Current(-2000)).is_err());

        // Song text: within the header room (every section stays where it
        // was), then too large for it.
        set_lyrics(&path, Some(("new text".into(), "imported".into()))).unwrap();
        let after = read_header(&path).unwrap();
        assert_eq!(after.lyrics.as_ref().unwrap().0, "new text");
        assert_eq!(
            (after.audio, after.cover, after.analysis),
            (h.audio, h.cover, h.analysis)
        );
        let big = "x".repeat(20_000);
        set_lyrics(&path, Some((big.clone(), "imported".into()))).unwrap();
        let h = read_header(&path).unwrap();
        assert_eq!(h.lyrics.unwrap().0, big);
        assert!(verify(&path).unwrap());
        assert_eq!(read_section(&path, h.cover.unwrap()).unwrap(), b"cover");

        // Karaoke data: kept next to the song text, removed again.
        let notes: Vec<Value> = (0..2000)
            .map(|i| Value::object().with("t", i as f64 * 0.25).with("p", 60))
            .collect();
        let k = Value::object()
            .with("format", "beatlumen-karaoke")
            .with("lines", vec![Value::object().with("notes", notes)]);
        set_karaoke(&path, Some(k.clone())).unwrap();
        let h = read_header(&path).unwrap();
        assert_eq!(h.karaoke.as_ref(), Some(&k));
        assert_eq!(h.lyrics.unwrap().0, big);
        assert!(verify(&path).unwrap());
        set_karaoke(&path, None).unwrap();
        assert!(read_header(&path).unwrap().karaoke.is_none());
    }

    #[test]
    fn damaged_and_cancelled() {
        let path = tmp("bad");
        let h = Header {
            track: Value::object().with("id", "x"),
            ext: "mp3".into(),
            ..Default::default()
        };
        let err = write(
            &path,
            h.clone(),
            None,
            None,
            &mut &[1u8; 200_000][..],
            &mut |n| n < 100_000,
        );
        assert!(err.is_err());
        assert!(!path.exists());
        assert_eq!(temp_files(&path), Vec::<String>::new());
        write(&path, h, None, None, &mut &[1u8; 10_000][..], &mut |_| true).unwrap();
        // Flip a byte of the audio.
        let mut d = std::fs::read(&path).unwrap();
        let last = d.len() - 1;
        d[last] ^= 0xff;
        std::fs::write(&path, &d).unwrap();
        assert!(!verify(&path).unwrap());
        std::fs::write(&path, b"garbage that is not a container").unwrap();
        assert!(read_header(&path).is_err());
    }
}
