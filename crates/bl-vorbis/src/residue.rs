//! Residue decoding, formats 0, 1 and 2 (spec section 8).

use crate::bitreader::BitReader;
use crate::codebook::Codebook;
use crate::Error;

const EOP: Error = Error::InvalidData("truncated residue setup");

pub(crate) struct Residue {
    kind: u8,
    begin: usize,
    end: usize,
    partition_size: usize,
    classifications: usize,
    classbook: u8,
    /// Per classification: book number for each of the 8 passes (-1 = none).
    books: Vec<[i16; 8]>,
    /// Bitmask of passes that use at least one book.
    active_passes: u8,
}

/// Reusable buffers for residue decoding.
#[derive(Default)]
pub(crate) struct ResidueScratch {
    classes: Vec<u8>,
    interleaved: Vec<f32>,
    vq: Vec<f32>,
}

impl Residue {
    pub fn parse(br: &mut BitReader, books: &[Codebook]) -> Result<Self, Error> {
        let kind = br.read(16).ok_or(EOP)?;
        if kind > 2 {
            return Err(Error::InvalidData("bad residue type"));
        }
        let begin = br.read(24).ok_or(EOP)? as usize;
        let end = br.read(24).ok_or(EOP)? as usize;
        let partition_size = br.read(24).ok_or(EOP)? as usize + 1;
        let classifications = br.read(6).ok_or(EOP)? as usize + 1;
        let classbook = br.read(8).ok_or(EOP)? as usize;
        let mut cascade = Vec::with_capacity(classifications);
        for _ in 0..classifications {
            let low = br.read(3).ok_or(EOP)?;
            let high = if br.read_bit().ok_or(EOP)? {
                br.read(5).ok_or(EOP)?
            } else {
                0
            };
            cascade.push((high << 3 | low) as u8);
        }
        let mut list = Vec::with_capacity(classifications);
        let mut active_passes = 0u8;
        for &c in &cascade {
            let mut b = [-1i16; 8];
            for (j, slot) in b.iter_mut().enumerate() {
                if c & (1 << j) != 0 {
                    let book = br.read(8).ok_or(EOP)? as usize;
                    match books.get(book) {
                        Some(cb) if cb.has_vq() => *slot = book as i16,
                        _ => return Err(Error::InvalidData("bad residue codebook")),
                    }
                    active_passes |= 1 << j;
                }
            }
            list.push(b);
        }
        let cb = books
            .get(classbook)
            .ok_or(Error::InvalidData("bad residue classbook"))?;
        if cb.dims == 0 {
            return Err(Error::InvalidData("bad residue classbook"));
        }
        // classifications ^ dims must not exceed the number of entries.
        let mut vals = 1u64;
        for _ in 0..cb.dims {
            vals *= classifications as u64;
            if vals > cb.entries as u64 {
                return Err(Error::InvalidData("bad residue classbook"));
            }
        }
        Ok(Residue {
            kind: kind as u8,
            begin,
            end,
            partition_size,
            classifications,
            classbook: classbook as u8,
            books: list,
            active_passes,
        })
    }

    /// Decodes the residue vectors of channels `chans` (indices into
    /// `spectra`, which holds one vector of `stride` floats per channel; the
    /// first `n` of each are used and must be zeroed by the caller).
    #[allow(clippy::too_many_arguments)]
    pub fn decode(
        &self,
        br: &mut BitReader,
        books: &[Codebook],
        spectra: &mut [f32],
        stride: usize,
        chans: &[usize],
        do_not_decode: &[bool],
        n: usize,
        scratch: &mut ResidueScratch,
    ) {
        let ch = chans.len();
        if ch == 0 {
            return;
        }
        if self.kind == 2 {
            if do_not_decode.iter().all(|&d| d) {
                return;
            }
            let total = n * ch;
            let mut inter = std::mem::take(&mut scratch.interleaved);
            inter.clear();
            inter.resize(total, 0.0);
            self.decode_vectors(br, books, &mut inter, total, &[0], &[false], total, scratch);
            if ch == 2 {
                let (a, b) = (chans[0] * stride, chans[1] * stride);
                for (i, pair) in inter.chunks_exact(2).enumerate() {
                    spectra[a + i] = pair[0];
                    spectra[b + i] = pair[1];
                }
            } else {
                for (j, &c) in chans.iter().enumerate() {
                    let dst = &mut spectra[c * stride..c * stride + n];
                    for (i, d) in dst.iter_mut().enumerate() {
                        *d = inter[i * ch + j];
                    }
                }
            }
            scratch.interleaved = inter;
        } else {
            self.decode_vectors(br, books, spectra, stride, chans, do_not_decode, n, scratch);
        }
    }

    /// Format 0/1 decode of `chans.len()` vectors of size `n`.
    #[allow(clippy::too_many_arguments)]
    fn decode_vectors(
        &self,
        br: &mut BitReader,
        books: &[Codebook],
        spectra: &mut [f32],
        stride: usize,
        chans: &[usize],
        do_not_decode: &[bool],
        n: usize,
        scratch: &mut ResidueScratch,
    ) {
        let ch = chans.len();
        let limit_begin = self.begin.min(n);
        let limit_end = self.end.min(n);
        let n_to_read = limit_end.saturating_sub(limit_begin);
        let psize = self.partition_size;
        let parts = n_to_read / psize;
        if parts == 0 {
            return;
        }
        let classbook = &books[self.classbook as usize];
        let cpc = classbook.dims;
        let cstride = parts + cpc;
        let classes = &mut scratch.classes;
        classes.clear();
        classes.resize(ch * cstride, 0);
        let nclass = self.classifications as u32;
        let vq = &mut scratch.vq;

        for pass in 0..8 {
            if pass > 0 && self.active_passes & (1 << pass) == 0 {
                continue;
            }
            let mut pc = 0;
            while pc < parts {
                if pass == 0 {
                    for j in 0..ch {
                        if do_not_decode[j] {
                            continue;
                        }
                        let Some(mut temp) = classbook.decode_scalar(br) else {
                            return;
                        };
                        let row = &mut classes[j * cstride + pc..j * cstride + pc + cpc];
                        for slot in row.iter_mut().rev() {
                            *slot = (temp % nclass) as u8;
                            temp /= nclass;
                        }
                    }
                }
                let mut i = 0;
                while i < cpc && pc < parts {
                    for j in 0..ch {
                        if do_not_decode[j] {
                            continue;
                        }
                        let class = classes[j * cstride + pc] as usize;
                        let book = self.books[class][pass];
                        if book < 0 {
                            continue;
                        }
                        let book = &books[book as usize];
                        let start = chans[j] * stride + limit_begin + pc * psize;
                        let out = &mut spectra[start..start + psize];
                        let ok = if self.kind == 0 {
                            decode_partition_0(br, book, out, vq)
                        } else {
                            decode_partition_1(br, book, out, vq)
                        };
                        if !ok {
                            return;
                        }
                    }
                    i += 1;
                    pc += 1;
                }
            }
        }
    }
}

/// Format 0: values of each VQ vector are interleaved with stride
/// `partition_size / dims`.
fn decode_partition_0(
    br: &mut BitReader,
    book: &Codebook,
    out: &mut [f32],
    vq: &mut Vec<f32>,
) -> bool {
    let dims = book.dims;
    let step = out.len() / dims;
    for i in 0..step {
        let Some(v) = book.decode_vector(br, vq) else {
            return false;
        };
        for (j, &x) in v.iter().enumerate() {
            out[i + j * step] += x;
        }
    }
    true
}

/// Format 1: VQ vectors are stored consecutively.
#[inline]
fn decode_partition_1(
    br: &mut BitReader,
    book: &Codebook,
    out: &mut [f32],
    vq: &mut Vec<f32>,
) -> bool {
    let dims = book.dims;
    let len = out.len();
    let mut i = 0;
    while i + dims <= len {
        let Some(v) = book.decode_vector(br, vq) else {
            return false;
        };
        for (o, &x) in out[i..i + dims].iter_mut().zip(v) {
            *o += x;
        }
        i += dims;
    }
    if i < len {
        let Some(v) = book.decode_vector(br, vq) else {
            return false;
        };
        for (o, &x) in out[i..].iter_mut().zip(v) {
            *o += x;
        }
    }
    true
}
