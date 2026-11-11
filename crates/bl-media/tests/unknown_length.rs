//! Songs that play while they still arrive: the stream does not know its
//! length yet (`SeekFrom::End` answers `Unsupported`, as bl-net's HTTP stream
//! does for `Content-Range: bytes 0-*/*`) and hands out data in small pieces.
//! FLAC and MP3 must open and decode exactly as from a file.

mod common;

use common::*;
use std::io::{self, Read, Seek, SeekFrom};

/// A file whose end is not known, read in pieces of at most 1000 bytes.
struct Arriving(std::fs::File);

impl Read for Arriving {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = buf.len().min(1000);
        self.0.read(&mut buf[..n])
    }
}

impl Seek for Arriving {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        if let SeekFrom::End(_) = to {
            return Err(io::Error::new(io::ErrorKind::Unsupported, "length not known yet"));
        }
        self.0.seek(to)
    }
}

fn check(name: &str, args: &[&str], ext: &str) {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg");
        return;
    }
    let p = make(name, "stereo44", args).expect("fixture");
    let (spec, _, want, codec) = decode_all(&p).unwrap();
    let mut d = bl_media::open(Box::new(Arriving(std::fs::File::open(&p).unwrap())), Some(ext))
        .unwrap_or_else(|e| panic!("{name}: {e}"));
    assert_eq!(d.spec(), spec, "{name}");
    assert_eq!(d.codec_name(), codec, "{name}");
    let mut got = Vec::new();
    while d.next_chunk(&mut got).unwrap() {}
    assert_eq!(got.len(), want.len(), "{name}: sample count");
    assert!(got == want, "{name}: samples differ");

    // Seeking still works (forward decoding, no bisection to the unknown end).
    let mut d = bl_media::open(Box::new(Arriving(std::fs::File::open(&p).unwrap())), Some(ext)).unwrap();
    let frame = spec.sample_rate as u64; // 1 s in
    let at = d.seek(frame).unwrap_or_else(|e| panic!("{name}: seek: {e}"));
    let mut after = Vec::new();
    while after.len() < 4096 && d.next_chunk(&mut after).unwrap() {}
    assert!(!after.is_empty(), "{name}: nothing after the seek");
    assert!(at <= frame + 1152 && at + 4608 >= frame, "{name}: seek landed at {at} for {frame}");
}

#[test]
fn flac_opens_while_it_arrives() {
    check("s16.flac", &["-c:a", "flac"], "flac");
}

#[test]
fn mp3_opens_while_it_arrives() {
    check("cbr.mp3", &["-c:a", "libmp3lame", "-b:a", "192k"], "mp3");
    check("vbr.mp3", &["-c:a", "libmp3lame", "-q:a", "2"], "mp3");
}
