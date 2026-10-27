//! SFNT container: table directory and TrueType/OpenType collections.

use crate::reader::{u32_at, Reader};
use crate::Error;
use std::ops::Range;

/// Byte range of a table inside the font file (validated to lie in bounds).
pub(crate) type TableRange = Range<usize>;

/// Parsed table directory.
#[derive(Debug, Default)]
pub(crate) struct Directory {
    tables: Vec<([u8; 4], TableRange)>,
}

impl Directory {
    pub fn get(&self, tag: &[u8; 4]) -> Option<TableRange> {
        self.tables.iter().find(|(t, _)| t == tag).map(|(_, r)| r.clone())
    }
}

const TTCF: u32 = u32::from_be_bytes(*b"ttcf");

/// Number of fonts in a collection (1 for a plain font file, 0 if unknown).
pub(crate) fn collection_count(data: &[u8]) -> u32 {
    match u32_at(data, 0) {
        Some(TTCF) => u32_at(data, 8).unwrap_or(0),
        Some(0x0001_0000) | Some(0x4F54_544F) | Some(0x7472_7565) => 1,
        _ => 0,
    }
}

/// Offset of the table directory for font `index` in `data`.
pub(crate) fn font_offset(data: &[u8], index: u32) -> Result<usize, Error> {
    let tag = u32_at(data, 0).ok_or(Error::InvalidData("file too short"))?;
    if tag == TTCF {
        let count = u32_at(data, 8).ok_or(Error::InvalidData("truncated collection header"))?;
        if index >= count {
            return Err(Error::InvalidData("font index out of range"));
        }
        let off = (index as usize)
            .checked_mul(4)
            .and_then(|o| u32_at(data, o.checked_add(12)?))
            .ok_or(Error::InvalidData("truncated collection header"))?;
        Ok(off as usize)
    } else if index == 0 {
        Ok(0)
    } else {
        Err(Error::InvalidData("font index out of range"))
    }
}

/// Parses the table directory at `offset`.
pub(crate) fn parse_directory(data: &[u8], offset: usize) -> Result<Directory, Error> {
    let mut r = Reader::at(data, offset);
    let version = r.u32().ok_or(Error::InvalidData("truncated table directory"))?;
    match version {
        0x0001_0000 | 0x7472_7565 /* 'true' */ | 0x4F54_544F /* 'OTTO' */ => {}
        0x7479_7031 /* 'typ1' */ => return Err(Error::Unsupported("PostScript Type 1 sfnt")),
        0x774F_4646 | 0x774F_4632 /* 'wOFF' / 'wOF2' */ => {
            return Err(Error::Unsupported("WOFF compressed fonts"))
        }
        _ => return Err(Error::InvalidData("unknown sfnt version")),
    }
    let num_tables = r.u16().ok_or(Error::InvalidData("truncated table directory"))?;
    r.skip(6).ok_or(Error::InvalidData("truncated table directory"))?;
    let mut tables = Vec::with_capacity(num_tables as usize);
    for _ in 0..num_tables {
        let rec = (|| {
            let tag = r.tag()?;
            let _checksum = r.u32()?;
            let off = r.u32()? as usize;
            let len = r.u32()? as usize;
            Some((tag, off, len))
        })();
        let Some((tag, off, len)) = rec else {
            return Err(Error::InvalidData("truncated table directory"));
        };
        // Tables that do not fit in the file are treated as missing.
        if let Some(end) = off.checked_add(len) {
            if end <= data.len() {
                tables.push((tag, off..end));
            }
        }
    }
    Ok(Directory { tables })
}
