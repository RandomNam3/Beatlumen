//! Damaged Ogg streams: packets, durations and seeking end cleanly.

#[path = "../../bl-json/tests/fuzz/mod.rs"]
mod fuzz;

use bl_ogg::{read_page, PacketReader, PageWriter};
use std::io::Cursor;

/// Streams as encoders write them: header packets, audio packets of
/// varying size (one spanning pages), two interleaved streams, junk.
fn samples() -> Vec<Vec<u8>> {
    let mut rng = fuzz::Rng(7);
    let mut out = Vec::new();
    // One stream.
    let mut s = Vec::new();
    let mut w = PageWriter::new(0x1234);
    w.packet(&mut s, b"OpusHead\x01\x02\x38\x01\x80\xbb\0\0\0\0\0", 0);
    w.flush(&mut s, false);
    w.packet(&mut s, b"OpusTags\x04\0\0\0test\0\0\0\0", 0);
    w.flush(&mut s, false);
    for i in 0..60u32 {
        let n = if i == 20 { 70_000 } else { 20 + rng.below(400) };
        let p: Vec<u8> = (0..n).map(|_| rng.next() as u8).collect();
        w.packet(&mut s, &p, 960 * (i as i64 + 1));
        if i % 7 == 6 {
            w.flush(&mut s, false);
        }
    }
    w.flush(&mut s, true);
    out.push(s.clone());
    // Two streams, pages interleaved.
    let mut a = PageWriter::new(1);
    let mut b = PageWriter::new(2);
    let mut t = Vec::new();
    for i in 0..30i64 {
        a.packet(&mut t, &[i as u8; 100], 480 * (i + 1));
        a.flush(&mut t, i == 29);
        b.packet(&mut t, &[!(i as u8); 50], 960 * (i + 1));
        b.flush(&mut t, i == 29);
    }
    out.push(t);
    // Junk before, between and after pages.
    let mut j: Vec<u8> = (0..300).map(|_| rng.next() as u8).collect();
    j.extend_from_slice(&s[..s.len() / 2]);
    j.extend_from_slice(b"OggS\0\0garbage that looks like a page");
    j.extend_from_slice(&s[s.len() / 2..]);
    out.push(j);
    out
}

fn exercise(data: &[u8]) {
    // Pages.
    let mut c = Cursor::new(data);
    let mut serial = None;
    for _ in 0..100_000 {
        match read_page(&mut c) {
            Ok(Some(p)) => {
                assert_eq!(p.data.len(), p.lacing.iter().map(|&l| l as usize).sum());
                serial.get_or_insert(p.serial);
            }
            _ => break,
        }
    }
    // Packets, with an error now and then.
    let mut r = PacketReader::new(Cursor::new(data));
    let mut errors = 0;
    for _ in 0..100_000 {
        match r.next_packet() {
            Ok(Some(p)) => assert!(p.data.len() <= bl_ogg::MAX_PACKET),
            Ok(None) => break,
            Err(_) => {
                errors += 1;
                if errors > 100 {
                    break;
                }
            }
        }
    }
    let Some(serial) = serial else { return };
    r.set_serial(Some(serial));
    let total = r.last_granule(serial).ok().flatten().unwrap_or(10_000);
    for target in [0, total / 2, total, total.saturating_add(1 << 40), u64::MAX] {
        if r.seek_granule(serial, target, 0).is_ok() {
            for _ in 0..3 {
                let _ = r.next_packet();
            }
        }
    }
}

#[test]
fn damaged_streams_end_cleanly() {
    let tokens: &[&[u8]] = &[b"OggS", b"OggS\0\0", b"OggS\0\x01", &[0xff; 8], &[0; 27]];
    fuzz::run("ogg", &samples(), tokens, 3000, exercise);
}
