//! Ogg bitstream (RFC 3533) page and packet reader.
//!
//! Supports reading pages with CRC verification and resynchronisation,
//! reassembling packets that span pages, filtering by logical stream serial,
//! determining the last granule position of a stream (for durations) and
//! bisection seeking by granule position.

use std::io::{self, Read, Seek, SeekFrom};

const CAPTURE: &[u8; 4] = b"OggS";

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
                r = if r & 0x8000_0000 != 0 { (r << 1) ^ 0x04c1_1db7 } else { r << 1 };
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
/// and pages with bad checksums. Returns `Ok(None)` at end of stream.
pub fn read_page<R: Read + Seek>(r: &mut R) -> io::Result<Option<Page>> {
    let mut window = [0u8; 4];
    let mut filled = 0usize;
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
            r.seek(SeekFrom::Start(offset + 1))?;
            filled = 0;
            continue;
        }
        return Ok(Some(Page { header_type, granule, serial, sequence, lacing, data, offset }));
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
    }

    /// Returns the next complete packet, or `None` at the end of the stream.
    pub fn next_packet(&mut self) -> io::Result<Option<Packet>> {
        loop {
            if let Some(p) = self.pending.pop_front() {
                return Ok(Some(p));
            }
            if self.eof {
                return Ok(None);
            }
            let page = match read_page(&mut self.inner)? {
                Some(p) => p,
                None => {
                    self.eof = true;
                    continue;
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
                self.partial.extend_from_slice(&page.data[pos..pos + l]);
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
                granule: if last && page.granule >= 0 { Some(page.granule as u64) } else { None },
                bos: page.is_bos() && i == 0,
                eos: page.is_eos() && last,
            });
        }
    }

    /// Finds the granule position of the last page of the given stream.
    /// Restores the current stream position afterwards.
    pub fn last_granule(&mut self, serial: u32) -> io::Result<Option<u64>> {
        let saved = self.inner.stream_position()?;
        let end = self.inner.seek(SeekFrom::End(0))?;
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
        let end = self.inner.seek(SeekFrom::End(0))?;
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
pub fn write_page(out: &mut Vec<u8>, header_type: u8, granule: i64, serial: u32, sequence: u32, packets: &[&[u8]]) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

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
