//! Header packet parsing: identification (spec 4.2.2), comment (spec 5)
//! and setup (spec 4.2.4) headers.

use crate::bitreader::BitReader;
use crate::codebook::{ilog, Codebook};
use crate::floor::Floor;
use crate::residue::Residue;
use crate::Error;

const EOP: Error = Error::InvalidData("truncated setup header");

fn check_signature(data: &[u8], packet_type: u8) -> Result<(), Error> {
    if data.len() < 7 || data[0] != packet_type || &data[1..7] != b"vorbis" {
        return Err(Error::InvalidData("bad header signature"));
    }
    Ok(())
}

pub(crate) struct Ident {
    pub channels: usize,
    pub sample_rate: u32,
    pub blocksizes: [usize; 2],
    pub bitrate_nominal: i32,
}

pub(crate) fn parse_ident(data: &[u8]) -> Result<Ident, Error> {
    check_signature(data, 1)?;
    if data.len() < 30 {
        return Err(Error::InvalidData("identification header too short"));
    }
    let u32_at = |o: usize| u32::from_le_bytes([data[o], data[o + 1], data[o + 2], data[o + 3]]);
    if u32_at(7) != 0 {
        return Err(Error::Unsupported("vorbis version"));
    }
    let channels = data[11] as usize;
    let sample_rate = u32_at(12);
    let bitrate_nominal = u32_at(20) as i32;
    let bs0 = data[28] & 15;
    let bs1 = data[28] >> 4;
    if channels == 0 || sample_rate == 0 {
        return Err(Error::InvalidData("zero channels or sample rate"));
    }
    if !(6..=13).contains(&bs0) || !(6..=13).contains(&bs1) || bs0 > bs1 {
        return Err(Error::InvalidData("bad block sizes"));
    }
    if data[29] & 1 == 0 {
        return Err(Error::InvalidData("identification header framing bit"));
    }
    Ok(Ident {
        channels,
        sample_rate,
        blocksizes: [1 << bs0, 1 << bs1],
        bitrate_nominal,
    })
}

/// Parses the comment header. Truncated contents are tolerated (the
/// comments read so far are kept); a bad signature is an error.
pub(crate) fn parse_comment(data: &[u8]) -> Result<(String, Vec<(String, String)>), Error> {
    check_signature(data, 3)?;
    let mut pos = 7;
    let next_str = |pos: &mut usize| -> Option<&[u8]> {
        let len_bytes = data.get(*pos..*pos + 4)?;
        let len =
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]) as usize;
        let start = *pos + 4;
        let s = data.get(start..start.checked_add(len)?)?;
        *pos = start + len;
        Some(s)
    };
    let vendor = next_str(&mut pos)
        .map(|v| String::from_utf8_lossy(v).into_owned())
        .unwrap_or_default();
    let mut comments = Vec::new();
    if let Some(cnt) = data.get(pos..pos + 4) {
        let count = u32::from_le_bytes([cnt[0], cnt[1], cnt[2], cnt[3]]);
        pos += 4;
        for _ in 0..count {
            let Some(s) = next_str(&mut pos) else { break };
            let s = String::from_utf8_lossy(s);
            if let Some((k, v)) = s.split_once('=') {
                comments.push((k.to_string(), v.to_string()));
            }
        }
    }
    Ok((vendor, comments))
}

pub(crate) struct Mapping {
    /// (magnitude, angle) channel pairs.
    pub coupling: Vec<(usize, usize)>,
    /// Submap number of each channel.
    pub mux: Vec<usize>,
    /// (floor, residue) of each submap.
    pub submaps: Vec<(usize, usize)>,
}

pub(crate) struct Mode {
    pub blockflag: bool,
    pub mapping: usize,
}

pub(crate) struct Setup {
    pub books: Vec<Codebook>,
    pub floors: Vec<Floor>,
    pub residues: Vec<Residue>,
    pub mappings: Vec<Mapping>,
    pub modes: Vec<Mode>,
}

pub(crate) fn parse_setup(data: &[u8], ident: &Ident) -> Result<Setup, Error> {
    check_signature(data, 5)?;
    let mut br = BitReader::new(&data[7..]);
    let br = &mut br;
    let channels = ident.channels;

    let nbooks = br.read(8).ok_or(EOP)? as usize + 1;
    let mut books = Vec::with_capacity(nbooks);
    for _ in 0..nbooks {
        books.push(Codebook::parse(br)?);
    }

    let ntimes = br.read(6).ok_or(EOP)? + 1;
    for _ in 0..ntimes {
        if br.read(16).ok_or(EOP)? != 0 {
            return Err(Error::InvalidData("bad time domain transform"));
        }
    }

    let nfloors = br.read(6).ok_or(EOP)? as usize + 1;
    let mut floors = Vec::with_capacity(nfloors);
    for _ in 0..nfloors {
        floors.push(Floor::parse(br, &books, ident.blocksizes)?);
    }

    let nresidues = br.read(6).ok_or(EOP)? as usize + 1;
    let mut residues = Vec::with_capacity(nresidues);
    for _ in 0..nresidues {
        residues.push(Residue::parse(br, &books)?);
    }

    let nmappings = br.read(6).ok_or(EOP)? as usize + 1;
    let mut mappings = Vec::with_capacity(nmappings);
    for _ in 0..nmappings {
        if br.read(16).ok_or(EOP)? != 0 {
            return Err(Error::InvalidData("bad mapping type"));
        }
        let nsubmaps = if br.read_bit().ok_or(EOP)? {
            br.read(4).ok_or(EOP)? as usize + 1
        } else {
            1
        };
        let mut coupling = Vec::new();
        if br.read_bit().ok_or(EOP)? {
            let steps = br.read(8).ok_or(EOP)? as usize + 1;
            let bits = ilog(channels as u32 - 1);
            for _ in 0..steps {
                let mag = br.read(bits).ok_or(EOP)? as usize;
                let ang = br.read(bits).ok_or(EOP)? as usize;
                if mag == ang || mag >= channels || ang >= channels {
                    return Err(Error::InvalidData("bad channel coupling"));
                }
                coupling.push((mag, ang));
            }
        }
        if br.read(2).ok_or(EOP)? != 0 {
            return Err(Error::InvalidData("bad mapping reserved field"));
        }
        let mut mux = vec![0usize; channels];
        if nsubmaps > 1 {
            for m in mux.iter_mut() {
                *m = br.read(4).ok_or(EOP)? as usize;
                if *m >= nsubmaps {
                    return Err(Error::InvalidData("bad mapping mux"));
                }
            }
        }
        let mut submaps = Vec::with_capacity(nsubmaps);
        for _ in 0..nsubmaps {
            br.read(8).ok_or(EOP)?; // unused time configuration
            let floor = br.read(8).ok_or(EOP)? as usize;
            let residue = br.read(8).ok_or(EOP)? as usize;
            if floor >= floors.len() || residue >= residues.len() {
                return Err(Error::InvalidData("bad submap"));
            }
            submaps.push((floor, residue));
        }
        mappings.push(Mapping {
            coupling,
            mux,
            submaps,
        });
    }

    let nmodes = br.read(6).ok_or(EOP)? as usize + 1;
    let mut modes = Vec::with_capacity(nmodes);
    for _ in 0..nmodes {
        let blockflag = br.read_bit().ok_or(EOP)?;
        let window = br.read(16).ok_or(EOP)?;
        let transform = br.read(16).ok_or(EOP)?;
        let mapping = br.read(8).ok_or(EOP)? as usize;
        if window != 0 || transform != 0 || mapping >= mappings.len() {
            return Err(Error::InvalidData("bad mode"));
        }
        modes.push(Mode { blockflag, mapping });
    }
    if !br.read_bit().ok_or(EOP)? {
        return Err(Error::InvalidData("setup header framing bit"));
    }

    // Precompute VQ tables (largest benefit for residue books).
    let mut budget = Codebook::MAX_VQ_TOTAL;
    for book in books.iter_mut() {
        if book.has_vq() {
            book.precompute(&mut budget);
        }
    }

    Ok(Setup {
        books,
        floors,
        residues,
        mappings,
        modes,
    })
}
