//! `cmap` table: character to glyph mapping.
//!
//! Supported subtable formats: 0, 4, 6, 10, 12 and 13. The best subtable is
//! chosen once at load time: full Unicode repertoire subtables first
//! ((3,10), (0,4), (0,6)), then BMP subtables ((3,1), (0,3), other (0,x)),
//! then the Windows symbol encoding (3,0) and finally Mac Roman (1,0) for
//! ASCII only.

use crate::reader::{u16_at, u32_at, Reader};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Unicode,
    Symbol,
    MacRoman,
}

/// The selected cmap subtable (absolute offset into the font data).
#[derive(Debug, Clone, Copy)]
pub(crate) struct Cmap {
    offset: usize,
    format: u16,
    kind: Kind,
}

fn supported(format: u16) -> bool {
    matches!(format, 0 | 4 | 6 | 10 | 12 | 13)
}

/// Picks the best subtable from the `cmap` table located at `table_off`.
pub(crate) fn parse(data: &[u8], table_off: usize, table_end: usize) -> Option<Cmap> {
    let table = data.get(table_off..table_end)?;
    let mut r = Reader::new(table);
    let _version = r.u16()?;
    let count = r.u16()?;
    let mut best: Option<(u32, Cmap)> = None;
    for _ in 0..count {
        let Some((pid, eid, off)) = (|| Some((r.u16()?, r.u16()?, r.u32()? as usize)))() else {
            break;
        };
        let Some(format) = u16_at(table, off) else { continue };
        if !supported(format) {
            continue;
        }
        let wide = matches!(format, 10 | 12 | 13);
        let (score, kind) = match (pid, eid) {
            (3, 10) | (0, 4) | (0, 6) if wide => (if format == 13 { 3 } else { 0 }, Kind::Unicode),
            (3, 1) | (3, 10) | (0, _) => (1, Kind::Unicode),
            (3, 0) => (4, Kind::Symbol),
            (1, 0) => (5, Kind::MacRoman),
            _ => continue,
        };
        // Within the same class prefer the Windows table (3,x).
        let score = score * 2 + u32::from(pid != 3);
        if !matches!(best, Some((s, _)) if s <= score) {
            let Some(offset) = table_off.checked_add(off) else { continue };
            best = Some((score, Cmap { offset, format, kind }));
        }
    }
    best.map(|(_, c)| c)
}

impl Cmap {
    /// Maps a code point to a glyph id (0 = missing).
    pub fn lookup(&self, data: &[u8], cp: u32) -> u16 {
        let Some(sub) = data.get(self.offset..) else { return 0 };
        match self.kind {
            Kind::Unicode => lookup_format(sub, self.format, cp).unwrap_or(0),
            Kind::Symbol => {
                let g = lookup_format(sub, self.format, cp).unwrap_or(0);
                if g == 0 && cp < 0x100 {
                    lookup_format(sub, self.format, 0xF000 + cp).unwrap_or(0)
                } else {
                    g
                }
            }
            Kind::MacRoman => {
                if cp < 0x80 {
                    lookup_format(sub, self.format, cp).unwrap_or(0)
                } else {
                    0
                }
            }
        }
    }
}

fn lookup_format(sub: &[u8], format: u16, cp: u32) -> Option<u16> {
    match format {
        0 => {
            if cp < 256 {
                sub.get(6 + cp as usize).map(|&g| g as u16)
            } else {
                None
            }
        }
        4 => format4(sub, cp),
        6 => {
            let first = u16_at(sub, 6)? as u32;
            let count = u16_at(sub, 8)? as u32;
            let i = cp.checked_sub(first)?;
            if i < count {
                u16_at(sub, 10 + 2 * i as usize)
            } else {
                None
            }
        }
        10 => {
            let first = u32_at(sub, 12)?;
            let count = u32_at(sub, 16)?;
            let i = cp.checked_sub(first)?;
            if i < count {
                u16_at(sub, (i as usize).checked_mul(2)?.checked_add(20)?)
            } else {
                None
            }
        }
        12 | 13 => {
            let groups = u32_at(sub, 12)? as usize;
            let (mut lo, mut hi) = (0usize, groups);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let rec = 16usize.checked_add(mid.checked_mul(12)?)?;
                let start = u32_at(sub, rec)?;
                let end = u32_at(sub, rec + 4)?;
                if cp < start {
                    hi = mid;
                } else if cp > end {
                    lo = mid + 1;
                } else {
                    let g = u32_at(sub, rec + 8)?;
                    let g = if format == 12 { g.checked_add(cp - start)? } else { g };
                    return u16::try_from(g).ok();
                }
            }
            None
        }
        _ => None,
    }
}

fn format4(sub: &[u8], cp: u32) -> Option<u16> {
    if cp > 0xFFFF {
        return None;
    }
    let cp = cp as u16;
    let seg_count = (u16_at(sub, 6)? / 2) as usize;
    let ends = 14usize;
    let starts = ends + 2 * seg_count + 2;
    let deltas = starts + 2 * seg_count;
    let range_offs = deltas + 2 * seg_count;
    // Binary search for the first segment with endCode >= cp.
    let (mut lo, mut hi) = (0usize, seg_count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        if u16_at(sub, ends + 2 * mid)? < cp {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    if lo >= seg_count {
        return None;
    }
    let i = lo;
    let start = u16_at(sub, starts + 2 * i)?;
    if cp < start {
        return None;
    }
    let delta = u16_at(sub, deltas + 2 * i)?;
    let ro_pos = range_offs + 2 * i;
    let ro = u16_at(sub, ro_pos)? as usize;
    if ro == 0 {
        Some(cp.wrapping_add(delta))
    } else {
        let addr = ro_pos + ro + 2 * (cp - start) as usize;
        let g = u16_at(sub, addr)?;
        if g == 0 {
            None
        } else {
            Some(g.wrapping_add(delta))
        }
    }
}
