//! Ogg bitstream (RFC 3533) page and packet reader.
//!
//! Supports reading pages with CRC verification and resynchronisation,
//! reassembling packets that span pages, filtering by logical stream serial,
//! determining the last granule position of a stream (for durations) and
//! bisection seeking by granule position.

#![forbid(unsafe_code)]

use std::io::{self, Read, Seek, SeekFrom};

const CAPTURE: &[u8; 4] = b"OggS";

/// Largest packet reassembled from pages (64 MiB, like the tag limits of
/// the other containers). Audio packets are a few KiB and tag packets with
/// cover art a few MiB; a longer one is dropped with an error.
pub const MAX_PACKET: usize = 64 << 20;

/// Most bytes [`read_page`] checks in pages that turn out damaged before it
/// gives up (32 MiB). Real damage costs one page; a file made of fake page
/// headers with wrong checksums would otherwise cost up to a thousand times
/// its size in checksum work.
pub const MAX_RESYNC: usize = 32 << 20;

/// A single Ogg page.
#[derive(Debug, Clone)]
pub struct Page {
    pub header_type: u8,
    /// Granule position; `-1` means "no packet finishes on this page".
    pub granule: i64,
    pub serial: u32,
    pub sequence: u32,
    pub lacing: Vec<u8>,
    pub data: Vec<u8>,
    /// Byte offset of the page start in the underlying stream.
    pub offset: u64,
}

impl Page {
    pub fn is_continued(&self) -> bool {
        self.header_type & 1 != 0
    }
    pub fn is_bos(&self) -> bool {
        self.header_type & 2 != 0
    }
    pub fn is_eos(&self) -> bool {
        self.header_type & 4 != 0
    }
    /// Total size of the page in bytes (header + data).
    pub fn size(&self) -> usize {
        27 + self.lacing.len() + self.data.len()
    }
}

/// A complete packet.
#[derive(Debug, Clone)]
pub struct Packet {
    pub serial: u32,
    pub data: Vec<u8>,
    /// Granule position of the page on which this packet ended, if this packet
    /// was the last one completed on that page.
    pub granule: Option<u64>,
    pub bos: bool,
    pub eos: bool,
}

fn crc_table() -> &'static [u32; 256] {
    static TABLE: std::sync::OnceLock<[u32; 256]> = std::sync::OnceLock::new();
    TABLE.get_or_init(|| {
        let mut t = [0u32; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut r = (i as u32) << 24;
            for _ in 0..8 {
                r = if r & 0x8000_0000 != 0 {
                    (r << 1) ^ 0x04c1_1db7
                } else {
                    r << 1
                };
            }
            *e = r;
        }
        t
    })
}

fn crc_update(mut crc: u32, data: &[u8]) -> u32 {
    let t = crc_table();
    for &b in data {
        crc = (crc << 8) ^ t[((crc >> 24) as u8 ^ b) as usize];
    }
    crc
}

/// Reads the next valid page starting at the current position. Skips garbage
/// and pages with bad checksums. Returns `Ok(None)` at end of stream, and an
/// `InvalidData` error after [`MAX_RESYNC`] bytes of damaged pages.
pub fn read_page<R: Read + Seek>(r: &mut R) -> io::Result<Option<Page>> {
    let mut window = [0u8; 4];
    let mut filled = 0usize;
    let mut wasted = 0usize;
    loop {
        // Find capture pattern.
        let mut b = [0u8; 1];
        loop {
            if filled == 4 {
                if &window == CAPTURE {
                    break;
                }
                window.copy_within(1..4, 0);
                filled = 3;
            }
            match r.read(&mut b) {
                Ok(0) => return Ok(None),
                Ok(_) => {
                    window[filled] = b[0];
                    filled += 1;
                }
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }
        let offset = r.stream_position()? - 4;
        let mut hdr = [0u8; 23];
        if !read_full(r, &mut hdr)? {
            return Ok(None);
        }
        if hdr[0] != 0 {
            // Unsupported version: resync one byte after the capture pattern.
            r.seek(SeekFrom::Start(offset + 1))?;
            filled = 0;
            continue;
        }
        let header_type = hdr[1];
        let granule = i64::from_le_bytes(hdr[2..10].try_into().unwrap());
        let serial = u32::from_le_bytes(hdr[10..14].try_into().unwrap());
        let sequence = u32::from_le_bytes(hdr[14..18].try_into().unwrap());
        let crc = u32::from_le_bytes(hdr[18..22].try_into().unwrap());
        let nseg = hdr[22] as usize;
        let mut lacing = vec![0u8; nseg];
        if !read_full(r, &mut lacing)? {
            return Ok(None);
        }
        let len: usize = lacing.iter().map(|&v| v as usize).sum();
        let mut data = vec![0u8; len];
        if !read_full(r, &mut data)? {
            return Ok(None);
        }
        let mut c = crc_update(0, CAPTURE);
        let mut h2 = hdr;
        h2[18..22].copy_from_slice(&[0, 0, 0, 0]);
        c = crc_update(c, &h2);
        c = crc_update(c, &lacing);
        c = crc_update(c, &data);
        if c != crc {
            wasted += 27 + nseg + len;
            if wasted > MAX_RESYNC {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "no valid Ogg page among 32 MiB of damaged ones",
                ));
            }
            r.seek(SeekFrom::Start(offset + 1))?;
            filled = 0;
            continue;
        }
        return Ok(Some(Page {
            header_type,
            granule,
            serial,
            sequence,
            lacing,
            data,
            offset,
        }));
    }
}

fn read_full<R: Read>(r: &mut R, buf: &mut [u8]) -> io::Result<bool> {
    let mut pos = 0;
    while pos < buf.len() {
        match r.read(&mut buf[pos..]) {
            Ok(0) => return Ok(false),
            Ok(n) => pos += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
            Err(e) => return Err(e),
        }
    }
    Ok(true)
}

/// Reassembles packets from pages. Optionally restricted to a single
/// logical stream (see [`PacketReader::set_serial`]).
pub struct PacketReader<R: Read + Seek> {
    inner: R,
    serial: Option<u32>,
    partial: Vec<u8>,
    partial_valid: bool,
    pending: std::collections::VecDeque<Packet>,
    eof: bool,
    /// A packet grew beyond [`MAX_PACKET`]; reported by the next call.
    too_long: bool,
}

impl<R: Read + Seek> PacketReader<R> {
    pub fn new(inner: R) -> Self {
        PacketReader {
            inner,
            serial: None,
            partial: Vec::new(),
            partial_valid: false,
            pending: Default::default(),
            eof: false,
            too_long: false,
        }
    }

    /// Restricts packet output to the logical stream with this serial.
    pub fn set_serial(&mut self, serial: Option<u32>) {
        self.serial = serial;
    }

    pub fn get_mut(&mut self) -> &mut R {
        &mut self.inner
    }

    pub fn into_inner(self) -> R {
        self.inner
    }

    /// Drops buffered state; the next packet starts at the next page boundary
    /// from the current stream position.
    pub fn reset(&mut self) {
        self.partial.clear();
        self.partial_valid = false;
        self.pending.clear();
        self.eof = false;
        self.too_long = false;
    }

    /// Returns the next complete packet, or `None` at the end of the stream.
    /// A packet longer than [`MAX_PACKET`] is dropped and reported as an
    /// `InvalidData` error once; reading goes on with the packet after it.
    pub fn next_packet(&mut self) -> io::Result<Option<Packet>> {
        loop {
            if std::mem::take(&mut self.too_long) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Ogg packet longer than 64 MiB",
                ));
            }
            if let Some(p) = self.pending.pop_front() {
                return Ok(Some(p));
            }
            if self.eof {
                return Ok(None);
            }
            let page = match read_page(&mut self.inner) {
                Ok(Some(p)) => p,
                Ok(None) => {
                    self.eof = true;
                    continue;
                }
                Err(e) => {
                    // Endless damage: the stream ends after this error.
                    if e.kind() == io::ErrorKind::InvalidData {
                        self.eof = true;
                    }
                    return Err(e);
                }
            };
            if let Some(s) = self.serial {
                if page.serial != s {
                    continue;
                }
            }
            self.push_page(page);
        }
    }

    fn push_page(&mut self, page: Page) {
        if !page.is_continued() {
            // A fresh packet starts here; drop any unfinished data.
            self.partial.clear();
            self.partial_valid = true;
        }
        let mut pos = 0usize;
        let mut completed: Vec<Vec<u8>> = Vec::new();
        // When we joined mid-packet (e.g. after seeking) the leading
        // continuation data is useless and skipped.
        let mut skipping = page.is_continued() && !self.partial_valid;
        for &lace in &page.lacing {
            let l = lace as usize;
            if !skipping {
                if self.partial.len() + l > MAX_PACKET {
                    // Too long: drop it and skip to its end. A page holds
                    // less than 64 KiB, so this packet began on an earlier
                    // page and is the first on this one: the error comes
                    // in order, before the packets completed after it.
                    self.partial = Vec::new();
                    self.too_long = true;
                    skipping = true;
                } else {
                    self.partial.extend_from_slice(&page.data[pos..pos + l]);
                }
            }
            pos += l;
            if lace < 255 {
                if !skipping {
                    completed.push(std::mem::take(&mut self.partial));
                }
                skipping = false;
                self.partial_valid = true;
            }
        }
        if skipping {
            // Entire page was a continuation we could not use.
            self.partial.clear();
            self.partial_valid = false;
        }
        let n = completed.len();
        for (i, data) in completed.into_iter().enumerate() {
            let last = i + 1 == n;
            self.pending.push_back(Packet {
                serial: page.serial,
                data,
                granule: if last && page.granule >= 0 {
                    Some(page.granule as u64)
                } else {
                    None
                },
                bos: page.is_bos() && i == 0,
                eos: page.is_eos() && last,
            });
        }
    }

    /// Finds the granule position of the last page of the given stream.
    /// Restores the current stream position afterwards.
    pub fn last_granule(&mut self, serial: u32) -> io::Result<Option<u64>> {
        let saved = self.inner.stream_position()?;
        let end = match self.inner.seek(SeekFrom::End(0)) {
            Ok(e) => e,
            // A stream still growing (or of unknown length): no total.
            Err(e) if e.kind() == io::ErrorKind::Unsupported => return Ok(None),
            Err(e) => return Err(e),
        };
        let mut chunk: u64 = 64 * 1024;
        let result;
        loop {
            let start = end.saturating_sub(chunk);
            self.inner.seek(SeekFrom::Start(start))?;
            let mut best: Option<u64> = None;
            while let Some(p) = read_page(&mut self.inner)? {
                if p.serial == serial && p.granule >= 0 {
                    best = Some(p.granule as u64);
                }
            }
            if best.is_some() || start == 0 {
                result = best;
                break;
            }
            chunk *= 4;
        }
        self.inner.seek(SeekFrom::Start(saved))?;
        Ok(result)
    }

    /// Positions the reader so that the next packets come from the first
    /// page (of `serial`) whose granule position is >= `target`, or close
    /// before it. `data_start` is the byte offset of the first audio page
    /// (after the headers). Returns the granule position of the page *before*
    /// the returned position (i.e. the granule at which decoding restarts),
    /// which callers use to compute how many samples to skip.
    pub fn seek_granule(&mut self, serial: u32, target: u64, data_start: u64) -> io::Result<u64> {
        // Unknown length: the linear scan from the start finds the page.
        let end = match self.inner.seek(SeekFrom::End(0)) {
            Ok(e) => e,
            Err(e) if e.kind() == io::ErrorKind::Unsupported => data_start,
            Err(e) => return Err(e),
        };
        let mut lo = data_start;
        let mut hi = end;
        // Best known page start whose granule < target, along with its granule.
        let mut best_off = data_start;
        let mut best_gran = 0u64;
        while hi > lo + 8192 {
            let mid = lo + (hi - lo) / 2;
            self.inner.seek(SeekFrom::Start(mid))?;
            let mut found = None;
            while let Some(p) = read_page(&mut self.inner)? {
                if p.offset >= hi {
                    break;
                }
                if p.serial == serial && p.granule >= 0 {
                    found = Some(p);
                    break;
                }
            }
            match found {
                Some(p) if (p.granule as u64) < target => {
                    best_off = p.offset + p.size() as u64;
                    best_gran = p.granule as u64;
                    lo = best_off;
                }
                Some(p) => hi = p.offset,
                None => hi = mid,
            }
        }
        // Linear scan from the best known position.
        self.inner.seek(SeekFrom::Start(best_off))?;
        loop {
            let pos = self.inner.stream_position()?;
            match read_page(&mut self.inner)? {
                Some(p) if p.serial == serial && p.granule >= 0 && (p.granule as u64) < target => {
                    best_off = p.offset + p.size() as u64;
                    best_gran = p.granule as u64;
                }
                Some(p) if p.serial == serial && p.granule >= 0 => {
                    let _ = pos;
                    break;
                }
                Some(_) => {}
                None => break,
            }
        }
        self.inner.seek(SeekFrom::Start(best_off))?;
        self.reset();
        // The first page after a seek may begin with a continued packet; the
        // packet reader drops such partial data automatically.
        self.partial_valid = false;
        Ok(best_gran)
    }
}

/// Builds an Ogg page (used by tests and tools).
pub fn write_page(
    out: &mut Vec<u8>,
    header_type: u8,
    granule: i64,
    serial: u32,
    sequence: u32,
    packets: &[&[u8]],
) {
    let mut lacing = Vec::new();
    let mut data = Vec::new();
    for p in packets {
        let mut n = p.len();
        loop {
            let l = n.min(255);
            lacing.push(l as u8);
            n -= l;
            if l < 255 {
                break;
            }
        }
        data.extend_from_slice(p);
    }
    let start = out.len();
    out.extend_from_slice(CAPTURE);
    out.push(0);
    out.push(header_type);
    out.extend_from_slice(&granule.to_le_bytes());
    out.extend_from_slice(&serial.to_le_bytes());
    out.extend_from_slice(&sequence.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.push(lacing.len() as u8);
    out.extend_from_slice(&lacing);
    out.extend_from_slice(&data);
    let crc = crc_update(0, &out[start..]);
    out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
}

/// Writes one logical stream as Ogg pages. Packets may span pages; a page
/// is emitted when 255 segments are full or on [`PageWriter::flush`].
pub struct PageWriter {
    serial: u32,
    sequence: u32,
    lacing: Vec<u8>,
    data: Vec<u8>,
    /// Granule of the last packet that ended on the current page.
    granule: Option<i64>,
    /// The current page starts with the rest of a packet.
    continued: bool,
    first: bool,
}

impl PageWriter {
    pub fn new(serial: u32) -> PageWriter {
        PageWriter {
            serial,
            sequence: 0,
            lacing: Vec::new(),
            data: Vec::new(),
            granule: None,
            continued: false,
            first: true,
        }
    }

    /// Bytes of the page being filled.
    pub fn pending(&self) -> usize {
        self.data.len()
    }

    /// Adds a packet that ends at `granule`; full pages go to `out`.
    pub fn packet(&mut self, out: &mut Vec<u8>, packet: &[u8], granule: i64) {
        let mut rest = packet;
        loop {
            if self.lacing.len() == 255 {
                self.emit(out, false);
                self.continued = true;
            }
            let l = rest.len().min(255);
            self.lacing.push(l as u8);
            self.data.extend_from_slice(&rest[..l]);
            rest = &rest[l..];
            if l < 255 {
                break;
            }
        }
        self.granule = Some(granule);
    }

    /// Ends the current page (`eos`: the last page of the stream).
    pub fn flush(&mut self, out: &mut Vec<u8>, eos: bool) {
        if !self.lacing.is_empty() || eos {
            self.emit(out, eos);
            self.continued = false;
        }
    }

    fn emit(&mut self, out: &mut Vec<u8>, eos: bool) {
        // A page on which no packet ends has granule -1.
        let complete = self.lacing.last().is_some_and(|&l| l < 255);
        let granule = if complete {
            self.granule.unwrap_or(0)
        } else {
            -1
        };
        let mut flags = 0u8;
        if self.continued {
            flags |= 1;
        }
        if self.first {
            flags |= 2;
        }
        if eos {
            flags |= 4;
        }
        let start = out.len();
        out.extend_from_slice(CAPTURE);
        out.push(0);
        out.push(flags);
        out.extend_from_slice(&granule.to_le_bytes());
        out.extend_from_slice(&self.serial.to_le_bytes());
        out.extend_from_slice(&self.sequence.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.push(self.lacing.len() as u8);
        out.extend_from_slice(&self.lacing);
        out.extend_from_slice(&self.data);
        let crc = crc_update(0, &out[start..]);
        out[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        self.sequence += 1;
        self.first = false;
        self.lacing.clear();
        self.data.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn fake_pages_cost_bounded_work() {
        // 300-byte blocks that each start a page claiming 65 KB of data
        // with a wrong checksum.
        let mut block = b"OggS\0\0".to_vec();
        block.extend_from_slice(&[0; 20]);
        block.push(255);
        block.extend_from_slice(&[255; 255]);
        block.resize(300, 0x55);
        let data = block.repeat(30_000);
        let t = std::time::Instant::now();
        let e = read_page(&mut Cursor::new(&data)).unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        let mut r = PacketReader::new(Cursor::new(&data));
        assert!(r.next_packet().is_err());
        assert!(r.next_packet().unwrap().is_none());
        assert!(t.elapsed().as_secs() < 10, "{:?}", t.elapsed());
        // Real damage is skipped: a page with a broken checksum, then good ones.
        let mut out = Vec::new();
        let mut w = PageWriter::new(9);
        for i in 0..3 {
            w.packet(&mut out, &[i as u8; 1000], i);
            w.flush(&mut out, i == 2);
        }
        out[40] ^= 1;
        let mut r = PacketReader::new(Cursor::new(out));
        let mut n = 0;
        while let Some(p) = r.next_packet().unwrap() {
            assert_eq!(p.data.len(), 1000);
            n += 1;
        }
        assert_eq!(n, 2);
    }

    #[test]
    fn overlong_packets_are_dropped_with_an_error() {
        let big = vec![7u8; MAX_PACKET + 1];
        let mut out = Vec::new();
        let mut w = PageWriter::new(3);
        w.packet(&mut out, b"head", 0);
        w.flush(&mut out, false);
        w.packet(&mut out, &big, 0);
        w.flush(&mut out, false);
        drop(big);
        w.packet(&mut out, b"tail", 960);
        w.flush(&mut out, true);
        let mut r = PacketReader::new(Cursor::new(out));
        assert_eq!(r.next_packet().unwrap().unwrap().data, b"head");
        let e = r.next_packet().unwrap_err();
        assert_eq!(e.kind(), io::ErrorKind::InvalidData);
        assert!(r.partial.capacity() <= MAX_PACKET);
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.data, b"tail");
        assert!(p.eos);
        assert!(r.next_packet().unwrap().is_none());
        // Exactly the limit still passes.
        let mut out = Vec::new();
        let mut w = PageWriter::new(3);
        w.packet(&mut out, &vec![1u8; MAX_PACKET], 0);
        w.flush(&mut out, true);
        let mut r = PacketReader::new(Cursor::new(out));
        assert_eq!(r.next_packet().unwrap().unwrap().data.len(), MAX_PACKET);
    }

    #[test]
    fn page_writer_spans_pages() {
        let big: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        let small = vec![9u8; 100];
        let mut out = Vec::new();
        let mut w = PageWriter::new(7);
        w.packet(&mut out, b"head", 0);
        w.flush(&mut out, false);
        w.packet(&mut out, &big, 0);
        w.flush(&mut out, false);
        for i in 0..300 {
            w.packet(&mut out, &small, 960 * (i + 1));
        }
        w.flush(&mut out, true);
        let mut r = PacketReader::new(Cursor::new(out));
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.data, b"head");
        assert!(p.bos);
        assert_eq!(r.next_packet().unwrap().unwrap().data, big);
        let mut n = 0;
        let mut last = None;
        while let Some(p) = r.next_packet().unwrap() {
            assert_eq!(p.data, small);
            n += 1;
            last = Some(p);
        }
        assert_eq!(n, 300);
        let last = last.unwrap();
        assert!(last.eos);
        assert_eq!(last.granule, Some(960 * 300));
    }

    #[test]
    fn roundtrip_packets() {
        let mut buf = Vec::new();
        let big = vec![7u8; 600];
        write_page(&mut buf, 2, 0, 42, 0, &[b"head"]);
        write_page(&mut buf, 0, 100, 42, 1, &[b"a", &big, b"bc"]);
        write_page(&mut buf, 4, 200, 42, 2, &[b"last"]);
        let mut r = PacketReader::new(Cursor::new(buf));
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.data, b"head");
        assert!(p.bos);
        assert_eq!(r.next_packet().unwrap().unwrap().data, b"a");
        assert_eq!(r.next_packet().unwrap().unwrap().data, big);
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.data, b"bc");
        assert_eq!(p.granule, Some(100));
        let p = r.next_packet().unwrap().unwrap();
        assert!(p.eos);
        assert!(r.next_packet().unwrap().is_none());
        assert_eq!(r.last_granule(42).unwrap(), Some(200));
    }

    #[test]
    fn packet_spanning_pages_and_garbage() {
        let mut buf = b"garbage".to_vec();
        // Packet of 300 bytes split across two pages manually.
        let pkt: Vec<u8> = (0..300u32).map(|i| i as u8).collect();
        // Page 1 contains first 255 bytes with lacing 255 (unfinished).
        {
            let start = buf.len();
            buf.extend_from_slice(CAPTURE);
            buf.push(0);
            buf.push(0);
            buf.extend_from_slice(&(-1i64).to_le_bytes());
            buf.extend_from_slice(&1u32.to_le_bytes());
            buf.extend_from_slice(&0u32.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0, 0]);
            buf.push(1);
            buf.push(255);
            buf.extend_from_slice(&pkt[..255]);
            let crc = crc_update(0, &buf[start..]);
            buf[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        }
        // Continuation page with remaining 45 bytes.
        {
            let start = buf.len();
            buf.extend_from_slice(CAPTURE);
            buf.push(0);
            buf.push(1);
            buf.extend_from_slice(&(50i64).to_le_bytes());
            buf.extend_from_slice(&1u32.to_le_bytes());
            buf.extend_from_slice(&1u32.to_le_bytes());
            buf.extend_from_slice(&[0, 0, 0, 0]);
            buf.push(1);
            buf.push(45);
            buf.extend_from_slice(&pkt[255..]);
            let crc = crc_update(0, &buf[start..]);
            buf[start + 22..start + 26].copy_from_slice(&crc.to_le_bytes());
        }
        let mut r = PacketReader::new(Cursor::new(buf));
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.data, pkt);
        assert_eq!(p.granule, Some(50));
    }

    #[test]
    fn seek_by_granule() {
        let mut buf = Vec::new();
        write_page(&mut buf, 2, 0, 9, 0, &[b"hdr"]);
        let data_start = buf.len() as u64;
        for i in 0..2000u32 {
            let payload = vec![(i % 251) as u8; 100];
            write_page(&mut buf, 0, (i as i64 + 1) * 1000, 9, i + 1, &[&payload]);
        }
        let mut r = PacketReader::new(Cursor::new(buf));
        let g = r.seek_granule(9, 500_500, data_start).unwrap();
        assert_eq!(g, 500_000);
        let p = r.next_packet().unwrap().unwrap();
        assert_eq!(p.granule, Some(501_000));
    }
}
