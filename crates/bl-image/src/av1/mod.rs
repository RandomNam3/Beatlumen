//! AV1 decoder for still images (AVIF), written from the AV1 Bitstream &
//! Decoding Process Specification of the Alliance for Open Media: key
//! frames and intra-only frames with every intra coding tool (all block
//! sizes and partitions, directional / smooth / Paeth / recursive filter
//! intra prediction, chroma from luma, palettes, all transforms including
//! lossless), 8 to 12 bits, 4:0:0 to 4:4:4, tiles, segmentation, delta
//! quantizer and loop filter levels, quantizer matrices, deblocking, CDEF,
//! loop restoration and super-resolution. Inter frames (and intra block
//! copy) are not supported; AVIF still images do not use them.
//!
//! The frame is decoded superblock row by superblock row (the tiles of a
//! tile row side by side) and the loop filters follow one superblock row
//! behind, so a 16K image never exists at full size.

pub(crate) mod bits;
mod cdf;
mod cdf_tables;
mod decoder;
mod frame;
mod intrabc;
pub(crate) mod obu;
mod post;
mod predict;
mod qm_tables;
mod residual;
mod scan_tables;
mod symbol;
mod tables;
mod transform;

use crate::Error;
use decoder::{Decoder, Tile};
use obu::*;

/// Decoded planes at full size (tests and small images).
#[derive(Debug, Clone)]
pub struct Planes {
    pub width: usize,
    pub height: usize,
    pub bit_depth: u32,
    pub ssx: usize,
    pub ssy: usize,
    pub planes: Vec<Vec<u16>>,
}

/// The sequence header and the frame header with its tile data.
pub(crate) struct Stream<'a> {
    pub seq: SequenceHeader,
    pub fh: FrameHeader,
    pub tiles: Vec<&'a [u8]>,
}

/// Finds the first intra frame of a low overhead bitstream. `config` holds
/// configuration OBUs (av1C) used when the data has no sequence header.
pub(crate) fn parse_stream<'a>(data: &'a [u8], config: &[u8]) -> Result<Stream<'a>, Error> {
    let mut seq: Option<SequenceHeader> = None;
    for o in split_obus(config).unwrap_or_default() {
        if o.kind == OBU_SEQUENCE_HEADER {
            seq = Some(parse_sequence_header(o.payload)?);
        }
    }
    let mut fh: Option<FrameHeader> = None;
    let mut tiles: Vec<Option<&'a [u8]>> = Vec::new();
    for o in split_obus(data)? {
        match o.kind {
            OBU_SEQUENCE_HEADER => {
                if fh.is_none() {
                    seq = Some(parse_sequence_header(o.payload)?);
                }
            }
            OBU_FRAME_HEADER | OBU_FRAME | OBU_REDUNDANT_FRAME_HEADER | OBU_TILE_GROUP => {
                let s = seq
                    .as_ref()
                    .ok_or(Error::InvalidData("av1: no sequence header"))?;
                // Operating point 0: drop layers it does not contain.
                let idc = s.operating_point_idc.first().copied().unwrap_or(0);
                if o.has_extension && idc != 0 {
                    let in_t = (idc >> o.temporal_id) & 1;
                    let in_s = (idc >> (o.spatial_id + 8)) & 1;
                    if in_t == 0 || in_s == 0 {
                        continue;
                    }
                }
                let mut payload = o.payload;
                if o.kind != OBU_TILE_GROUP {
                    if fh.is_some() {
                        if o.kind == OBU_REDUNDANT_FRAME_HEADER {
                            continue;
                        }
                        // A second frame: the first one is complete.
                        break;
                    }
                    let (h, len) = parse_frame_header(payload, s, o.temporal_id, o.spatial_id)?;
                    fh = Some(h);
                    if o.kind != OBU_FRAME {
                        continue;
                    }
                    payload = payload
                        .get(len..)
                        .ok_or(Error::InvalidData("av1: truncated frame"))?;
                }
                let h = fh
                    .as_ref()
                    .ok_or(Error::InvalidData("av1: tile group without frame header"))?;
                parse_tile_group(payload, h, &mut tiles)?;
                if tiles.iter().all(|t| t.is_some()) {
                    break;
                }
            }
            _ => {}
        }
    }
    let seq = seq.ok_or(Error::InvalidData("av1: no sequence header"))?;
    let fh = fh.ok_or(Error::InvalidData("av1: no frame"))?;
    if tiles.is_empty() || tiles.iter().any(|t| t.is_none()) {
        return Err(Error::InvalidData("av1: missing tiles"));
    }
    Ok(Stream {
        tiles: tiles.into_iter().map(|t| t.unwrap_or_default()).collect(),
        seq,
        fh,
    })
}

/// Decodes a frame superblock row by superblock row.
pub(crate) struct FrameDecoder<'a> {
    pub d: Decoder,
    tile_data: Vec<&'a [u8]>,
    tiles: Vec<Tile<'a>>,
    tile_row: usize,
    next_row: usize,
    done: bool,
}

impl<'a> FrameDecoder<'a> {
    /// `fast`: skip CDEF and loop restoration (see [`Decoder::new`]).
    pub fn new(s: Stream<'a>, fast: bool) -> FrameDecoder<'a> {
        FrameDecoder {
            d: Decoder::new(s.seq, s.fh, fast),
            tile_data: s.tiles,
            tiles: Vec::new(),
            tile_row: 0,
            next_row: 0,
            done: false,
        }
    }

    pub fn width(&self) -> usize {
        self.d.fh.upscaled_width
    }

    pub fn height(&self) -> usize {
        self.d.fh.frame_height
    }

    /// Decodes the next superblock row and runs the loop filters. Returns
    /// the luma rows [a, b) that became available in the output rings, or
    /// None when the frame is complete.
    pub fn step(&mut self) -> Result<Option<(usize, usize)>, Error> {
        if self.done {
            return Ok(None);
        }
        let ti = self.d.fh.tile.clone();
        let r = self.next_row;
        if self.tiles.is_empty() || r >= ti.row_starts[self.tile_row + 1] {
            if !self.tiles.is_empty() {
                self.tile_row += 1;
            }
            let tr = self.tile_row;
            self.tiles = (0..ti.cols)
                .map(|tc| self.d.new_tile(self.tile_data[tr * ti.cols + tc], tr, tc))
                .collect();
        }
        for t in self.tiles.iter_mut() {
            self.d.decode_sb_row(t, r)?;
        }
        let next = r + self.d.sb4;
        let ssy = self.d.ssy;
        self.d.frame.next_sb_row(next * 4, ssy);
        let last = next >= self.d.mi_rows;
        let rows = self.d.post_sb_row(r, last);
        self.next_row = next;
        self.done = last;
        Ok(Some(rows))
    }

    /// Row `y` of plane `p` of the finished frame (valid for the rows the
    /// last step returned).
    pub fn out_row(&self, p: usize, y: usize) -> &[u16] {
        self.d.post.out[p].row(y)
    }
}

/// Decodes the first frame at full size into planes (cropped to the frame
/// size). For tests and small images.
pub fn decode_planes(data: &[u8], config: &[u8]) -> Result<Planes, Error> {
    let s = parse_stream(data, config)?;
    let planes = s.seq.color.num_planes();
    let (ssx, ssy) = (s.seq.color.ssx as usize, s.seq.color.ssy as usize);
    let bit_depth = s.seq.color.bit_depth;
    let mut f = FrameDecoder::new(s, false);
    let (w, h) = (f.width(), f.height());
    let mut out: Vec<Vec<u16>> = Vec::new();
    for p in 0..planes {
        let (sx, sy) = if p > 0 { (ssx, ssy) } else { (0, 0) };
        out.push(vec![0; ((w + sx) >> sx) * ((h + sy) >> sy)]);
    }
    while let Some((a, b)) = f.step()? {
        for (p, o) in out.iter_mut().enumerate() {
            let (sx, sy) = if p > 0 { (ssx, ssy) } else { (0, 0) };
            let pw = (w + sx) >> sx;
            let ph = (h + sy) >> sy;
            let (pa, pb) = (a >> sy, if b >= h { ph } else { b >> sy });
            for y in pa..pb.min(ph) {
                o[y * pw..(y + 1) * pw].copy_from_slice(&f.out_row(p, y)[..pw]);
            }
        }
    }
    Ok(Planes {
        width: w,
        height: h,
        bit_depth,
        ssx,
        ssy,
        planes: out,
    })
}
