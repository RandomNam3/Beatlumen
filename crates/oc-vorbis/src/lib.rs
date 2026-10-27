//! Vorbis I decoder (packet level, std only). Container handling (Ogg,
//! Matroska) is done by the caller.
//!
//! Implements the complete Vorbis I specification
//! (<https://xiph.org/vorbis/doc/Vorbis_I_spec.html>): Huffman/VQ codebooks
//! (lookup types 0, 1 and 2, sparse and ordered length lists), floor types 0
//! and 1, residue types 0, 1 and 2, mappings with channel coupling and
//! multiple submaps, both block sizes with overlap-add windowing, and an
//! O(n log n) inverse MDCT.
//!
//! Output is interleaved `f32` in [-1, 1], remapped from Vorbis channel order
//! to WAVE/SMPTE order (L R C LFE BL BR SL SR) for 3..=8 channels.
//!
//! Behaviour notes:
//! - Like libvorbis, the first packet after [`VorbisDecoder::new`] or
//!   [`VorbisDecoder::reset`] only primes the overlap buffer (0 frames);
//!   every later packet yields `prev_blocksize/4 + cur_blocksize/4` frames.
//!   Trimming the end of a stream to its final granule position (and the
//!   start, for streams whose first granule implies a negative offset) is
//!   the container layer's job.
//! - Window shapes are derived from the actual sizes of neighbouring blocks
//!   (the per-packet window flags agree with them in valid streams), so
//!   overlap-add stays consistent after packet loss.
//! - End-of-packet inside floor or residue data is the nominal condition of
//!   the specification (unused floor / remaining residue zero); malformed
//!   headers and packets return [`Error`] and never panic.
//!
//! Module overview:
//! - `bitreader`: LSB-first packet bit reader.
//! - `codebook`: Huffman decoding and VQ value lookup.
//! - `floor`: floor 0 (LSP) and floor 1 (piecewise linear) curves.
//! - `residue`: residue formats 0/1/2.
//! - `mdct`: FFT-based inverse MDCT.
//! - `setup`: header packet parsing.

#![forbid(unsafe_code)]

mod bitreader;
mod codebook;
mod floor;
mod mdct;
mod residue;
mod setup;
mod tables;

use bitreader::BitReader;
use codebook::ilog;
use floor::FloorState;
use mdct::Imdct;
use residue::ResidueScratch;
use setup::Setup;

#[derive(Debug)]
pub enum Error {
    InvalidData(&'static str),
    Unsupported(&'static str),
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Error::InvalidData(s) => write!(f, "invalid vorbis data: {s}"),
            Error::Unsupported(s) => write!(f, "unsupported vorbis feature: {s}"),
        }
    }
}

impl std::error::Error for Error {}

/// Output position (WAVE order) of each Vorbis channel.
fn channel_remap(channels: usize) -> Vec<usize> {
    match channels {
        3 => vec![0, 2, 1],
        5 => vec![0, 2, 1, 3, 4],
        6 => vec![0, 2, 1, 4, 5, 3],
        7 => vec![0, 2, 1, 5, 6, 4, 3],
        8 => vec![0, 2, 1, 6, 7, 4, 5, 3],
        n => (0..n).collect(),
    }
}

/// Disjoint mutable views of channels `i != j` of a buffer holding one
/// vector of `stride` floats per channel.
fn two_channels_mut(
    buf: &mut [f32],
    stride: usize,
    i: usize,
    j: usize,
) -> (&mut [f32], &mut [f32]) {
    if i < j {
        let (a, b) = buf.split_at_mut(j * stride);
        (&mut a[i * stride..(i + 1) * stride], &mut b[..stride])
    } else {
        let (a, b) = buf.split_at_mut(i * stride);
        (&mut b[..stride], &mut a[j * stride..(j + 1) * stride])
    }
}

/// Window slope of length `n`: `sin(π/2 · sin²((k + ½)/n · π/2))`.
fn window_slope(n: usize) -> Vec<f32> {
    (0..n)
        .map(|k| {
            let s = ((k as f64 + 0.5) / n as f64 * std::f64::consts::FRAC_PI_2).sin();
            (std::f64::consts::FRAC_PI_2 * s * s).sin() as f32
        })
        .collect()
}

/// Mutable decoding state and scratch buffers.
struct State {
    imdct: [Imdct; 2],
    /// Overlap slopes for the short and long block sizes, and reversed.
    slopes: [Vec<f32>; 2],
    slopes_rev: [Vec<f32>; 2],
    /// Per channel: spectrum / DCT-IV output (`stride` floats each).
    spectra: Vec<f32>,
    /// Per channel: un-windowed second half of the previous block's IMDCT.
    overlap: Vec<f32>,
    /// First half of the current block's IMDCT (one channel at a time).
    left: Vec<f32>,
    /// Per channel: finished output samples of the current packet.
    pcm: Vec<f32>,
    stride: usize,
    /// Block flag of the previous packet; `None` right after creation/reset.
    prev_blockflag: Option<usize>,
    floors: Vec<FloorState>,
    no_residue: Vec<bool>,
    residue: ResidueScratch,
    vq: Vec<f32>,
    chans: Vec<usize>,
    do_not_decode: Vec<bool>,
}

pub struct VorbisDecoder {
    channels: usize,
    sample_rate: u32,
    blocksizes: [usize; 2],
    bitrate_nominal: i32,
    vendor: String,
    comments: Vec<(String, String)>,
    remap: Vec<usize>,
    setup: Setup,
    st: State,
}

impl VorbisDecoder {
    /// Creates a decoder from the three header packets.
    pub fn new(ident: &[u8], comment: &[u8], setup: &[u8]) -> Result<Self, Error> {
        let id = setup::parse_ident(ident)?;
        let (vendor, comments) = setup::parse_comment(comment)?;
        let setup = setup::parse_setup(setup, &id)?;
        let ch = id.channels;
        let stride = id.blocksizes[1] / 2;
        let st = State {
            imdct: [Imdct::new(id.blocksizes[0]), Imdct::new(id.blocksizes[1])],
            slopes: [
                window_slope(id.blocksizes[0] / 2),
                window_slope(id.blocksizes[1] / 2),
            ],
            slopes_rev: [0, 1].map(|i| {
                let mut v = window_slope(id.blocksizes[i] / 2);
                v.reverse();
                v
            }),
            spectra: vec![0.0; ch * stride],
            overlap: vec![0.0; ch * stride],
            left: vec![0.0; stride],
            pcm: vec![0.0; ch * stride],
            stride,
            prev_blockflag: None,
            floors: (0..ch).map(|_| FloorState::default()).collect(),
            no_residue: vec![false; ch],
            residue: ResidueScratch::default(),
            vq: Vec::new(),
            chans: Vec::with_capacity(ch),
            do_not_decode: Vec::with_capacity(ch),
        };
        Ok(VorbisDecoder {
            channels: ch,
            sample_rate: id.sample_rate,
            blocksizes: id.blocksizes,
            bitrate_nominal: id.bitrate_nominal,
            vendor,
            comments,
            remap: channel_remap(ch),
            setup,
            st,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }

    pub fn channels(&self) -> u16 {
        self.channels as u16
    }

    /// User comments from the comment header as (key, value) pairs; keys are
    /// returned as stored (callers compare case-insensitively).
    pub fn comments(&self) -> &[(String, String)] {
        &self.comments
    }

    /// Vendor string from the comment header.
    pub fn vendor(&self) -> &str {
        &self.vendor
    }

    /// Nominal bitrate from the identification header (bits/s; 0 or
    /// negative if unset).
    pub fn bitrate_nominal(&self) -> i32 {
        self.bitrate_nominal
    }

    /// The short and long block (window) sizes.
    pub fn block_sizes(&self) -> (usize, usize) {
        (self.blocksizes[0], self.blocksizes[1])
    }

    /// Returns the window size (short or long block size) of audio packet
    /// `packet`, or `None` if it is not a decodable audio packet.
    pub fn packet_window_size(&self, packet: &[u8]) -> Option<usize> {
        self.packet_blockflag(packet).map(|bf| self.blocksizes[bf])
    }

    fn packet_blockflag(&self, packet: &[u8]) -> Option<usize> {
        let mut br = BitReader::new(packet);
        if br.read_bit()? {
            return None;
        }
        let modes = &self.setup.modes;
        let mode = br.read(ilog(modes.len() as u32 - 1))? as usize;
        modes.get(mode).map(|m| m.blockflag as usize)
    }

    /// Decodes one audio packet, appending interleaved samples to `out`.
    /// Returns the number of PCM frames appended (0 for the first packet
    /// after creation/reset, as per the Vorbis overlap-add scheme).
    ///
    /// An empty packet is ignored (`Ok(0)`). Non-audio or undecodable
    /// packets return an error and leave the decoder state unchanged.
    pub fn decode_packet(&mut self, packet: &[u8], out: &mut Vec<f32>) -> Result<usize, Error> {
        if packet.is_empty() {
            return Ok(0);
        }
        const TRUNC: Error = Error::InvalidData("truncated audio packet");
        let setup = &self.setup;
        let st = &mut self.st;
        let ch = self.channels;
        let mut br = BitReader::new(packet);
        if br.read_bit().ok_or(TRUNC)? {
            return Err(Error::InvalidData("not an audio packet"));
        }
        let mode_num = br.read(ilog(setup.modes.len() as u32 - 1)).ok_or(TRUNC)? as usize;
        let mode = setup
            .modes
            .get(mode_num)
            .ok_or(Error::InvalidData("bad mode number"))?;
        let bf = mode.blockflag as usize;
        if mode.blockflag {
            // Previous/next window flags. The overlap uses the actual sizes
            // of the neighbouring blocks, which agree for valid streams.
            br.read(2).ok_or(TRUNC)?;
        }
        let n = self.blocksizes[bf];
        let half = n / 2;
        let stride = st.stride;
        let mapping = &setup.mappings[mode.mapping];
        let books = &setup.books;

        // Floor decode.
        for c in 0..ch {
            let (floor, _) = mapping.submaps[mapping.mux[c]];
            setup.floors[floor].decode(&mut br, books, &mut st.floors[c], &mut st.vq);
            st.no_residue[c] = !st.floors[c].used;
        }
        // Nonzero vector propagation.
        for &(m, a) in &mapping.coupling {
            if !st.no_residue[m] || !st.no_residue[a] {
                st.no_residue[m] = false;
                st.no_residue[a] = false;
            }
        }

        // Residue decode.
        for c in 0..ch {
            st.spectra[c * stride..c * stride + half].fill(0.0);
        }
        for (s, &(_, res)) in mapping.submaps.iter().enumerate() {
            st.chans.clear();
            st.do_not_decode.clear();
            for c in 0..ch {
                if mapping.mux[c] == s {
                    st.chans.push(c);
                    st.do_not_decode.push(st.no_residue[c]);
                }
            }
            setup.residues[res].decode(
                &mut br,
                books,
                &mut st.spectra,
                stride,
                &st.chans,
                &st.do_not_decode,
                half,
                &mut st.residue,
            );
        }

        // Inverse coupling.
        for &(m, a) in mapping.coupling.iter().rev() {
            let (mags, angs) = two_channels_mut(&mut st.spectra, stride, m, a);
            for (mr, ar) in mags[..half].iter_mut().zip(&mut angs[..half]) {
                let (mv, av) = (*mr, *ar);
                let (nm, na) = if mv > 0.0 {
                    if av > 0.0 {
                        (mv, mv - av)
                    } else {
                        (mv + av, mv)
                    }
                } else if av > 0.0 {
                    (mv, mv + av)
                } else {
                    (mv - av, mv)
                };
                *mr = nm;
                *ar = na;
            }
        }

        // Floor curve application and inverse MDCT.
        for c in 0..ch {
            let spec = &mut st.spectra[c * stride..c * stride + half];
            if st.floors[c].used {
                let (floor, _) = mapping.submaps[mapping.mux[c]];
                setup.floors[floor].apply(&st.floors[c], bf, spec);
                st.imdct[bf].dct4(spec);
            } else {
                spec.fill(0.0);
            }
        }

        // Overlap-add of the previous block's second half with this block's
        // first half; the output spans from the centre of the previous
        // window to the centre of the current one.
        let mut frames = 0;
        if let Some(pbf) = st.prev_blockflag {
            let mp = self.blocksizes[pbf] / 2;
            let mc = half;
            let w = mp.min(mc);
            let si = if w == self.blocksizes[0] / 2 { 0 } else { 1 };
            let (slope, slope_rev) = (&st.slopes[si][..w], &st.slopes_rev[si][..w]);
            frames = mp / 2 + mc / 2;
            let r0 = mp / 2 - w / 2;
            let l0 = mc / 2 - w / 2;
            for c in 0..ch {
                let u = &st.spectra[c * stride..c * stride + mc];
                let left = &mut st.left[..mc];
                Imdct::left_half(u, left);
                let right = &st.overlap[c * stride..c * stride + mp];
                let pcm = &mut st.pcm[c * stride..c * stride + frames];
                let (pa, rest) = pcm.split_at_mut(r0);
                let (pb, pc) = rest.split_at_mut(w);
                pa.copy_from_slice(&right[..r0]);
                let (rs, ls) = (&right[r0..r0 + w], &left[l0..l0 + w]);
                for ((p, (&r, &l)), (&wr, &wl)) in pb
                    .iter_mut()
                    .zip(rs.iter().zip(ls))
                    .zip(slope_rev.iter().zip(slope))
                {
                    *p = r * wr + l * wl;
                }
                pc.copy_from_slice(&left[l0 + w..]);
            }
            // Interleave into the output in WAVE channel order.
            let base = out.len();
            out.resize(base + frames * ch, 0.0);
            let dst = &mut out[base..];
            match ch {
                1 => dst.copy_from_slice(&st.pcm[..frames]),
                2 => {
                    let (a, b) = (&st.pcm[..frames], &st.pcm[stride..stride + frames]);
                    for (o, (&x, &y)) in dst.chunks_exact_mut(2).zip(a.iter().zip(b)) {
                        o[0] = x;
                        o[1] = y;
                    }
                }
                _ => {
                    for (c, &d) in self.remap.iter().enumerate() {
                        let src = &st.pcm[c * stride..c * stride + frames];
                        for (o, &x) in dst.chunks_exact_mut(ch).zip(src) {
                            o[d] = x;
                        }
                    }
                }
            }
        }
        for c in 0..ch {
            let (u, o) = (
                &st.spectra[c * stride..c * stride + half],
                &mut st.overlap[c * stride..c * stride + half],
            );
            Imdct::right_half(u, o);
        }
        st.prev_blockflag = Some(bf);
        Ok(frames)
    }

    /// Forgets overlap state (call after seeking).
    pub fn reset(&mut self) {
        self.st.prev_blockflag = None;
    }
}

/// Returns the number of PCM frames packet `packet` would produce given the
/// previous packet's blocksize flag (helper for granule bookkeeping); `None`
/// if it is not an audio packet.
///
/// The previous block size is the one of the last packet passed to
/// [`VorbisDecoder::decode_packet`], i.e. this returns exactly what decoding
/// `packet` next would return (0 right after creation or
/// [`VorbisDecoder::reset`]). Use [`VorbisDecoder::packet_window_size`] for
/// stateless bookkeeping (`prev_size / 4 + cur_size / 4`).
pub fn packet_block_size(dec: &VorbisDecoder, packet: &[u8]) -> Option<usize> {
    let bf = dec.packet_blockflag(packet)?;
    Some(match dec.st.prev_blockflag {
        None => 0,
        Some(p) => dec.blocksizes[p] / 4 + dec.blocksizes[bf] / 4,
    })
}
