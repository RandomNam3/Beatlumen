//! Synthetic streams exercising features that ffmpeg's and libvorbis'
//! encoders never emit: floor type 0, residue types 0 and 1, VQ lookup
//! type 2, sequence_p, sparse and ordered codebooks, multiple submaps,
//! coupling across submaps, cascade passes >= 3, unused floors and many
//! block-size combinations.
//!
//! A small generator writes valid setup headers and random (but valid)
//! audio packets; the resulting Ogg file is decoded by this crate and by
//! libvorbis (through ffmpeg, 16-bit output). During development the same
//! streams were also checked against libvorbisfile's float output (all
//! configurations within 131-137 dB).

#![allow(
    clippy::needless_range_loop,
    clippy::vec_init_then_push,
    clippy::neg_cmp_op_on_partial_ord,
    clippy::manual_is_multiple_of
)]

mod common;

use common::*;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + self.below((hi - lo + 1) as usize) as i32
    }
}

fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

#[derive(Default)]
struct BitWriter {
    buf: Vec<u8>,
    nbits: usize,
}

impl BitWriter {
    fn write(&mut self, v: u64, bits: u32) {
        for i in 0..bits {
            if self.nbits % 8 == 0 {
                self.buf.push(0);
            }
            if (v >> i) & 1 != 0 {
                *self.buf.last_mut().unwrap() |= 1 << (self.nbits % 8);
            }
            self.nbits += 1;
        }
    }
    fn bytes(&mut self, b: &[u8]) {
        for &x in b {
            self.write(x as u64, 8);
        }
    }
}

fn float32_pack(v: f32) -> u32 {
    if v == 0.0 {
        return 0;
    }
    let sign = if v < 0.0 { 0x8000_0000u32 } else { 0 };
    let a = v.abs() as f64;
    let mut exp = a.log2().floor() as i32;
    let mut mant = (a / 2f64.powi(exp - 20)).round() as u32;
    if mant >= 1 << 21 {
        mant >>= 1;
        exp += 1;
    }
    sign | (((exp - 20 + 788) as u32) << 21) | mant
}

struct Lookup {
    kind: u8,
    min: f32,
    delta: f32,
    bits: u32,
    seq: bool,
    mults: Vec<u32>,
}

struct Book {
    dims: u32,
    lengths: Vec<u8>,
    ordered: bool,
    lookup: Option<Lookup>,
    /// (MSB-first codeword, length) per entry.
    codes: Vec<(u32, u8)>,
    used: Vec<u32>,
}

impl Book {
    fn new(dims: u32, lengths: Vec<u8>, ordered: bool, lookup: Option<Lookup>) -> Book {
        // Codeword assignment of spec section 3.2.1.
        let mut available = [0u64; 33];
        let mut codes = vec![(0u32, 0u8); lengths.len()];
        let mut first = true;
        for (i, &l) in lengths.iter().enumerate() {
            if l == 0 {
                continue;
            }
            let l = l as usize;
            if first {
                first = false;
                codes[i] = (0, l as u8);
                for (j, a) in available.iter_mut().enumerate().take(l + 1).skip(1) {
                    *a = 1u64 << (32 - j);
                }
                continue;
            }
            let mut z = l;
            while available[z] == 0 {
                z -= 1;
                assert!(z > 0, "overspecified test codebook");
            }
            let res = available[z];
            available[z] = 0;
            for y in (z + 1..=l).rev() {
                available[y] = res + (1u64 << (32 - y));
            }
            codes[i] = ((res >> (32 - l)) as u32, l as u8);
        }
        let used: Vec<u32> = (0..lengths.len() as u32)
            .filter(|&i| lengths[i as usize] > 0)
            .collect();
        assert!(
            used.len() == 1 || available.iter().all(|&a| a == 0),
            "incomplete test codebook"
        );
        Book {
            dims,
            lengths,
            ordered,
            lookup,
            codes,
            used,
        }
    }

    fn write_setup(&self, w: &mut BitWriter) {
        let entries = self.lengths.len() as u32;
        w.write(0x56_4342, 24);
        w.write(self.dims as u64, 16);
        w.write(entries as u64, 24);
        if self.ordered {
            w.write(1, 1);
            let mut len = self.lengths[0];
            w.write(len as u64 - 1, 5);
            let mut cur = 0usize;
            while cur < self.lengths.len() {
                let count = self.lengths[cur..]
                    .iter()
                    .take_while(|&&l| l == len)
                    .count();
                w.write(count as u64, ilog(entries - cur as u32));
                cur += count;
                len += 1;
            }
        } else {
            w.write(0, 1);
            let sparse = self.lengths.contains(&0);
            w.write(sparse as u64, 1);
            for &l in &self.lengths {
                if sparse {
                    w.write((l > 0) as u64, 1);
                }
                if l > 0 {
                    w.write(l as u64 - 1, 5);
                }
            }
        }
        match &self.lookup {
            None => w.write(0, 4),
            Some(lk) => {
                w.write(lk.kind as u64, 4);
                w.write(float32_pack(lk.min) as u64, 32);
                w.write(float32_pack(lk.delta) as u64, 32);
                w.write(lk.bits as u64 - 1, 4);
                w.write(lk.seq as u64, 1);
                for &m in &lk.mults {
                    w.write(m as u64, lk.bits);
                }
            }
        }
    }

    fn encode(&self, w: &mut BitWriter, entry: u32) {
        let (code, len) = self.codes[entry as usize];
        assert!(len > 0, "encoding unused entry");
        for i in (0..len).rev() {
            w.write(((code >> i) & 1) as u64, 1);
        }
    }

    fn random_entry(&self, rng: &mut Rng) -> u32 {
        self.used[rng.below(self.used.len())]
    }
}

struct Floor1 {
    classes: Vec<u8>,
    class_dims: Vec<u8>,
    class_sub: Vec<u8>,
    class_master: Vec<u8>,
    sub_books: Vec<Vec<i16>>,
    mult: u8,
    rangebits: u32,
    xs: Vec<u32>,
}

struct Floor0 {
    order: u32,
    rate: u32,
    bark: u32,
    amp_bits: u32,
    amp_offset: u32,
    books: Vec<u8>,
}

enum Floor {
    Zero(Floor0),
    One(Floor1),
}

struct Residue {
    kind: u16,
    begin: u32,
    end: u32,
    psize: u32,
    classbook: u8,
    /// Per classification: book per pass (-1 = none).
    books: Vec<[i16; 8]>,
}

struct Mapping {
    submaps: Vec<(u8, u8)>,
    mux: Vec<u8>,
    coupling: Vec<(u8, u8)>,
}

struct Stream {
    channels: usize,
    rate: u32,
    bs: [u32; 2],
    books: Vec<Book>,
    floors: Vec<Floor>,
    residues: Vec<Residue>,
    mappings: Vec<Mapping>,
    modes: Vec<(bool, u8)>,
}

fn lookup1(min: f32, delta: f32, bits: u32, seq: bool, mults: Vec<u32>) -> Option<Lookup> {
    Some(Lookup {
        kind: 1,
        min,
        delta,
        bits,
        seq,
        mults,
    })
}

fn build_stream(rng: &mut Rng, channels: usize, bs: [u32; 2], lookup2: bool) -> Stream {
    let mut books = Vec::new();
    // 0: floor1 master / residue classbook (4 entries, dims 1).
    books.push(Book::new(1, vec![2, 2, 2, 2], false, None));
    // 1: floor1 Y book, 16 entries.
    books.push(Book::new(1, vec![4; 16], false, None));
    // 2: ordered book with lengths 1..7,7.
    books.push(Book::new(1, vec![1, 2, 3, 4, 5, 6, 7, 7], true, None));
    // 3: residue classbook, dims 2, 9 entries (3 classifications).
    books.push(Book::new(2, vec![3, 3, 3, 3, 3, 3, 3, 4, 4], false, None));
    // 4: unused filler book with a single used entry (sparse).
    books.push(Book::new(1, vec![0, 0, 1, 0], false, None));
    // 5: VQ lookup 1, dims 2, 25 entries (16 used, sparse), values -2..2.
    let mut l5 = vec![0u8; 25];
    for i in 0..16 {
        l5[(i * 3 + 1) % 25] = 4;
    }
    books.push(Book::new(
        2,
        l5,
        false,
        lookup1(-2.0, 1.0, 3, false, vec![0, 1, 2, 3, 4]),
    ));
    // 6: VQ dims 4, 16 entries, ordered, sequence_p; lookup type 2 or 1.
    let lk6 = if lookup2 {
        let mults: Vec<u32> = (0..64).map(|_| rng.below(7) as u32).collect();
        Some(Lookup {
            kind: 2,
            min: -1.5,
            delta: 0.5,
            bits: 3,
            seq: true,
            mults,
        })
    } else {
        lookup1(-1.0, 1.5, 2, true, vec![1, 2])
    };
    books.push(Book::new(4, vec![4; 16], true, lk6));
    // 7: VQ lookup 1, dims 1, 8 entries, values -3.5..3.5.
    books.push(Book::new(
        1,
        vec![3; 8],
        false,
        lookup1(-3.5, 1.0, 3, false, (0..8).collect()),
    ));
    // 8: VQ lookup 1, dims 3, 27 entries.
    let mut l8 = vec![5u8; 27];
    l8[..5].iter_mut().for_each(|l| *l = 4);
    books.push(Book::new(
        3,
        l8,
        false,
        lookup1(-1.0, 1.0, 2, false, vec![0, 1, 2]),
    ));
    // 9: floor0 LSP book: dims 2, sequence_p, increasing values.
    let lk9 = if lookup2 {
        let mults: Vec<u32> = (0..32).map(|_| rng.below(4) as u32).collect();
        Some(Lookup {
            kind: 2,
            min: 0.28,
            delta: 0.03,
            bits: 2,
            seq: true,
            mults,
        })
    } else {
        lookup1(0.28, 0.03, 2, true, vec![2, 0, 3, 1])
    };
    books.push(Book::new(2, vec![4; 16], false, lk9));
    // 10: floor0 LSP book: dims 3, lookup 1 (4 values), sequence_p.
    books.push(Book::new(
        3,
        vec![6; 64],
        false,
        lookup1(0.25, 0.03, 2, true, vec![1, 0, 3, 2]),
    ));

    let n1 = bs[1] / 2;
    let rangebits = ilog(n1) - 1;
    let mut floors = Vec::new();
    // Floor 1 with several classes, subclasses and master books.
    let classes = vec![0u8, 1, 1, 2, 0];
    let class_dims = vec![2u8, 3, 1];
    let mut count = 2;
    for &c in &classes {
        count += class_dims[c as usize] as usize;
    }
    let mut xs = vec![0u32, 1 << rangebits];
    while xs.len() < count {
        let x = 1 + rng.below((1 << rangebits) - 1) as u32;
        if !xs.contains(&x) {
            xs.push(x);
        }
    }
    floors.push(Floor::One(Floor1 {
        classes,
        class_dims,
        class_sub: vec![0, 1, 2],
        class_master: vec![0, 2, 0],
        sub_books: vec![vec![1], vec![1, -1], vec![1, 2, -1, 1]],
        mult: 2,
        rangebits,
        xs,
    }));
    floors.push(Floor::Zero(Floor0 {
        order: if rng.below(2) == 0 { 8 } else { 7 },
        rate: 44100,
        bark: 64,
        amp_bits: 6,
        amp_offset: 100,
        books: vec![9, 10],
    }));
    let mut xs = vec![0u32, 1 << rangebits];
    while xs.len() < 14 {
        let x = 1 + rng.below((1 << rangebits) - 1) as u32;
        if !xs.contains(&x) {
            xs.push(x);
        }
    }
    floors.push(Floor::One(Floor1 {
        classes: vec![0, 0, 0],
        class_dims: vec![4],
        class_sub: vec![0],
        class_master: vec![0],
        sub_books: vec![vec![1]],
        mult: 3,
        rangebits,
        xs,
    }));

    let residues = vec![
        Residue {
            kind: 0,
            begin: 0,
            end: n1 * 3 / 4,
            psize: 12,
            classbook: 3,
            books: vec![
                [-1; 8],
                [7, 5, -1, -1, -1, -1, -1, -1],
                [8, -1, 6, -1, -1, -1, -1, -1],
            ],
        },
        Residue {
            kind: 1,
            begin: 24,
            end: 100_000,
            psize: 8,
            classbook: 0,
            books: vec![
                [-1; 8],
                [5, -1, -1, -1, -1, -1, -1, -1],
                [6, 7, -1, -1, -1, -1, -1, -1],
                [7, -1, -1, 5, -1, -1, 8, -1],
            ],
        },
        Residue {
            kind: 2,
            begin: 0,
            end: (n1 * channels as u32) * 2 / 3,
            // A multiple of the channel count: libvorbis deviates from the
            // spec for interleaved partitions not starting on channel 0.
            psize: 8 * channels as u32,
            classbook: 3,
            books: vec![
                [-1; 8],
                [5, -1, -1, -1, -1, -1, -1, -1],
                [6, 8, -1, -1, -1, -1, -1, -1],
            ],
        },
    ];

    let mut mappings = Vec::new();
    let mut coupling = Vec::new();
    if channels >= 2 {
        coupling.push((0, 1));
    }
    if channels >= 3 {
        coupling.push((2, 0));
    }
    mappings.push(Mapping {
        submaps: vec![(0, 2)],
        mux: vec![0; channels],
        coupling: coupling.clone(),
    });
    if channels >= 2 {
        mappings.push(Mapping {
            submaps: vec![(1, 0), (2, 1)],
            mux: (0..channels).map(|c| (c % 2) as u8).collect(),
            coupling: vec![(0, 1)],
        });
    } else {
        mappings.push(Mapping {
            submaps: vec![(1, 0)],
            mux: vec![0],
            coupling: vec![],
        });
    }
    mappings.push(Mapping {
        submaps: vec![(2, 1)],
        mux: vec![0; channels],
        coupling: vec![],
    });
    let modes = vec![(false, 0), (true, 1), (true, 0), (false, 2)];
    Stream {
        channels,
        rate: 44100,
        bs,
        books,
        floors,
        residues,
        mappings,
        modes,
    }
}

fn ident_header(s: &Stream) -> Vec<u8> {
    let mut h = vec![1u8];
    h.extend_from_slice(b"vorbis");
    h.extend_from_slice(&0u32.to_le_bytes());
    h.push(s.channels as u8);
    h.extend_from_slice(&s.rate.to_le_bytes());
    h.extend_from_slice(&0i32.to_le_bytes());
    h.extend_from_slice(&128000i32.to_le_bytes());
    h.extend_from_slice(&0i32.to_le_bytes());
    h.push((ilog(s.bs[0]) - 1) as u8 | (((ilog(s.bs[1]) - 1) as u8) << 4));
    h.push(1);
    h
}

fn comment_header() -> Vec<u8> {
    let mut h = vec![3u8];
    h.extend_from_slice(b"vorbis");
    let vendor = b"oc-vorbis synthetic";
    h.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    h.extend_from_slice(vendor);
    let comments: [&[u8]; 3] = [b"TITLE=Synthetic", b"artist=Nobody=Really", b"no separator"];
    h.extend_from_slice(&(comments.len() as u32).to_le_bytes());
    for c in comments {
        h.extend_from_slice(&(c.len() as u32).to_le_bytes());
        h.extend_from_slice(c);
    }
    h.push(1);
    h
}

fn setup_header(s: &Stream) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.write(5, 8);
    w.bytes(b"vorbis");
    w.write(s.books.len() as u64 - 1, 8);
    for b in &s.books {
        b.write_setup(&mut w);
    }
    w.write(0, 6);
    w.write(0, 16);
    w.write(s.floors.len() as u64 - 1, 6);
    for f in &s.floors {
        match f {
            Floor::Zero(f) => {
                w.write(0, 16);
                w.write(f.order as u64, 8);
                w.write(f.rate as u64, 16);
                w.write(f.bark as u64, 16);
                w.write(f.amp_bits as u64, 6);
                w.write(f.amp_offset as u64, 8);
                w.write(f.books.len() as u64 - 1, 4);
                for &b in &f.books {
                    w.write(b as u64, 8);
                }
            }
            Floor::One(f) => {
                w.write(1, 16);
                w.write(f.classes.len() as u64, 5);
                for &c in &f.classes {
                    w.write(c as u64, 4);
                }
                for c in 0..f.class_dims.len() {
                    w.write(f.class_dims[c] as u64 - 1, 3);
                    w.write(f.class_sub[c] as u64, 2);
                    if f.class_sub[c] > 0 {
                        w.write(f.class_master[c] as u64, 8);
                    }
                    for j in 0..(1 << f.class_sub[c]) {
                        w.write((f.sub_books[c][j] + 1) as u64, 8);
                    }
                }
                w.write(f.mult as u64 - 1, 2);
                w.write(f.rangebits as u64, 4);
                for &x in &f.xs[2..] {
                    w.write(x as u64, f.rangebits);
                }
            }
        }
    }
    w.write(s.residues.len() as u64 - 1, 6);
    for r in &s.residues {
        w.write(r.kind as u64, 16);
        w.write(r.begin as u64, 24);
        w.write(r.end as u64, 24);
        w.write(r.psize as u64 - 1, 24);
        w.write(r.books.len() as u64 - 1, 6);
        w.write(r.classbook as u64, 8);
        for b in &r.books {
            let cascade: u32 = (0..8).filter(|&j| b[j] >= 0).map(|j| 1 << j).sum();
            w.write((cascade & 7) as u64, 3);
            let high = cascade >> 3;
            w.write((high > 0) as u64, 1);
            if high > 0 {
                w.write(high as u64, 5);
            }
        }
        for b in &r.books {
            for &book in b.iter().filter(|&&x| x >= 0) {
                w.write(book as u64, 8);
            }
        }
    }
    w.write(s.mappings.len() as u64 - 1, 6);
    let chbits = ilog(s.channels as u32 - 1);
    for m in &s.mappings {
        w.write(0, 16);
        if m.submaps.len() > 1 {
            w.write(1, 1);
            w.write(m.submaps.len() as u64 - 1, 4);
        } else {
            w.write(0, 1);
        }
        if m.coupling.is_empty() {
            w.write(0, 1);
        } else {
            w.write(1, 1);
            w.write(m.coupling.len() as u64 - 1, 8);
            for &(a, b) in &m.coupling {
                w.write(a as u64, chbits);
                w.write(b as u64, chbits);
            }
        }
        w.write(0, 2);
        if m.submaps.len() > 1 {
            for &x in &m.mux {
                w.write(x as u64, 4);
            }
        }
        for &(f, r) in &m.submaps {
            w.write(0, 8);
            w.write(f as u64, 8);
            w.write(r as u64, 8);
        }
    }
    w.write(s.modes.len() as u64 - 1, 6);
    for &(long, mapping) in &s.modes {
        w.write(long as u64, 1);
        w.write(0, 16);
        w.write(0, 16);
        w.write(mapping as u64, 8);
    }
    w.write(1, 1);
    w.buf
}

/// Floor-1 Y value giving a floor amplitude that keeps the output well
/// below full scale for block size `n`.
fn floor1_target(n: u32, mult: u8) -> i32 {
    let amp = 0.08 / (1.6 * (n as f64 / 4.0).sqrt());
    let idx = 255.0 + amp.ln() / 0.0630;
    (idx / mult as f64).round() as i32
}

fn write_residue(w: &mut BitWriter, rng: &mut Rng, s: &Stream, r: &Residue, dnd: &[bool], n: u32) {
    let (vectors, actual, dnd): (usize, u32, Vec<bool>) = if r.kind == 2 {
        if dnd.iter().all(|&d| d) {
            return;
        }
        (1, n * dnd.len() as u32, vec![false])
    } else {
        (dnd.len(), n, dnd.to_vec())
    };
    let begin = r.begin.min(actual);
    let end = r.end.min(actual);
    let parts = (end.saturating_sub(begin) / r.psize) as usize;
    if parts == 0 {
        return;
    }
    let cbook = &s.books[r.classbook as usize];
    let cpc = cbook.dims as usize;
    let nclass = r.books.len();
    let mut classes = vec![vec![0usize; parts + cpc]; vectors];
    for pass in 0..8 {
        let mut pc = 0;
        while pc < parts {
            if pass == 0 {
                for v in 0..vectors {
                    if dnd[v] {
                        continue;
                    }
                    let mut temp = 0;
                    for i in 0..cpc {
                        let c = rng.below(nclass);
                        classes[v][pc + i] = c;
                        temp = temp * nclass + c;
                    }
                    cbook.encode(w, temp as u32);
                }
            }
            let mut i = 0;
            while i < cpc && pc < parts {
                for v in 0..vectors {
                    if dnd[v] {
                        continue;
                    }
                    let book = r.books[classes[v][pc]][pass];
                    if book < 0 {
                        continue;
                    }
                    let b = &s.books[book as usize];
                    let count = if r.kind == 0 {
                        r.psize / b.dims
                    } else {
                        r.psize.div_ceil(b.dims)
                    };
                    for _ in 0..count {
                        let e = b.random_entry(rng);
                        b.encode(w, e);
                    }
                }
                i += 1;
                pc += 1;
            }
        }
    }
}

fn audio_packet(
    rng: &mut Rng,
    s: &Stream,
    mode: usize,
    prev_long: bool,
    next_long: bool,
) -> Vec<u8> {
    let mut w = BitWriter::default();
    w.write(0, 1);
    w.write(mode as u64, ilog(s.modes.len() as u32 - 1));
    let (long, mapping) = s.modes[mode];
    if long {
        w.write(prev_long as u64, 1);
        w.write(next_long as u64, 1);
    }
    let n = s.bs[long as usize] / 2;
    let m = &s.mappings[mapping as usize];
    let ch = s.channels;
    let mut no_residue = vec![false; ch];
    for c in 0..ch {
        let (floor, _) = m.submaps[m.mux[c] as usize];
        let used = rng.below(12) != 0;
        no_residue[c] = !used;
        match &s.floors[floor as usize] {
            Floor::One(f) => {
                if !used {
                    w.write(0, 1);
                    continue;
                }
                w.write(1, 1);
                let range = [256, 128, 86, 64][f.mult as usize - 1];
                let target = floor1_target(n, f.mult);
                for _ in 0..2 {
                    let y = (target + rng.range(-4, 4)).clamp(0, range - 1);
                    w.write(y as u64, ilog(range as u32 - 1));
                }
                for &class in &f.classes {
                    let c = class as usize;
                    let cbits = f.class_sub[c] as u32;
                    let mut cval = 0u32;
                    if cbits > 0 {
                        let mb = &s.books[f.class_master[c] as usize];
                        cval = mb.random_entry(rng);
                        mb.encode(&mut w, cval);
                    }
                    for _ in 0..f.class_dims[c] {
                        let book = f.sub_books[c][(cval & ((1 << cbits) - 1)) as usize];
                        cval >>= cbits;
                        if book >= 0 {
                            let b = &s.books[book as usize];
                            let e = rng.below(b.used.len().min(7)) as u32;
                            b.encode(&mut w, e);
                        }
                    }
                }
            }
            Floor::Zero(f) => {
                if !used {
                    w.write(0, f.amp_bits);
                    continue;
                }
                w.write(rng.range(2, 5) as u64, f.amp_bits);
                let bn = rng.below(f.books.len());
                w.write(bn as u64, ilog(f.books.len() as u32));
                let b = &s.books[f.books[bn] as usize];
                for _ in 0..f.order.div_ceil(b.dims) {
                    let e = b.random_entry(rng);
                    b.encode(&mut w, e);
                }
            }
        }
    }
    for &(a, b) in &m.coupling {
        if !no_residue[a as usize] || !no_residue[b as usize] {
            no_residue[a as usize] = false;
            no_residue[b as usize] = false;
        }
    }
    for (si, &(_, res)) in m.submaps.iter().enumerate() {
        let dnd: Vec<bool> = (0..ch)
            .filter(|&c| m.mux[c] as usize == si)
            .map(|c| no_residue[c])
            .collect();
        write_residue(&mut w, rng, s, &s.residues[res as usize], &dnd, n);
    }
    w.buf
}

/// Builds a complete Ogg Vorbis file with `npackets` audio packets.
fn build_ogg(rng: &mut Rng, s: &Stream, npackets: usize) -> Vec<u8> {
    let mut out = Vec::new();
    let serial = 0x5eed;
    oc_ogg::write_page(&mut out, 2, 0, serial, 0, &[&ident_header(s)]);
    oc_ogg::write_page(
        &mut out,
        0,
        0,
        serial,
        1,
        &[&comment_header(), &setup_header(s)],
    );
    let modes: Vec<usize> = (0..npackets).map(|_| rng.below(s.modes.len())).collect();
    let long = |i: usize| s.modes[modes[i]].0;
    let mut granule = 0u64;
    let mut seq = 2;
    let mut page: Vec<Vec<u8>> = Vec::new();
    for i in 0..npackets {
        let prev_long = i > 0 && long(i - 1);
        let next_long = i + 1 < npackets && long(i + 1);
        page.push(audio_packet(rng, s, modes[i], prev_long, next_long));
        if i > 0 {
            granule += (s.bs[long(i - 1) as usize] / 4 + s.bs[long(i) as usize] / 4) as u64;
        }
        if page.len() == 4 || i + 1 == npackets {
            let refs: Vec<&[u8]> = page.iter().map(|p| p.as_slice()).collect();
            let flags = if i + 1 == npackets { 4 } else { 0 };
            oc_ogg::write_page(&mut out, flags, granule as i64, serial, seq, &refs);
            seq += 1;
            page.clear();
        }
    }
    out
}

/// SNR of `test` against a 16-bit reference, ignoring samples where the
/// reference may have been clipped.
fn compare_unclipped(reference: &[f32], test: &[f32]) -> f64 {
    let (mut sig, mut err) = (0f64, 0f64);
    for (&r, &t) in reference.iter().zip(test) {
        if t.abs() < 0.99 {
            sig += (r as f64) * (r as f64);
            err += (r as f64 - t as f64).powi(2);
        }
    }
    if err == 0.0 {
        f64::INFINITY
    } else {
        10.0 * (sig / err).log10()
    }
}

#[test]
fn synthetic_streams() {
    let dir = temp_dir();
    let have_ffmpeg = ffmpeg_available();
    let have_lv = have_ffmpeg && has_decoder("libvorbis");
    if !have_ffmpeg {
        eprintln!("ffmpeg not available; only checking that synthetic streams decode");
    }
    // (channels, block sizes, use VQ lookup type 2 for books 6 and 9)
    let configs: [(usize, [u32; 2], bool); 10] = [
        (1, [256, 2048], false),
        (2, [256, 2048], false),
        (2, [64, 256], true),
        (3, [512, 512], false),
        (2, [128, 8192], true),
        (6, [256, 1024], false),
        (1, [64, 64], true),
        (2, [2048, 4096], false),
        (4, [128, 512], true),
        (1, [1024, 8192], false),
    ];
    let mut failures = Vec::new();
    for (k, &(ch, bs, lookup2)) in configs.iter().enumerate() {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ ((k as u64 + 1) * 0x1234_5678));
        let s = build_stream(&mut rng, ch, bs, lookup2);
        let ogg = build_ogg(&mut rng, &s, 120);
        let dec = decode_ogg(&ogg);
        assert_eq!(dec.channels, ch);
        assert_eq!(dec.decoder.comments().len(), 2);
        assert_eq!(
            dec.decoder.comments()[1],
            ("artist".to_string(), "Nobody=Really".to_string())
        );
        let peak = dec.samples.iter().fold(0f32, |a, &b| a.max(b.abs()));
        assert!(dec.samples.iter().all(|v| v.is_finite()));
        if !have_ffmpeg {
            continue;
        }
        // OC_VORBIS_KEEP=<dir> keeps the generated files for inspection.
        let keep = std::env::var_os("OC_VORBIS_KEEP").map(std::path::PathBuf::from);
        let path = keep
            .as_ref()
            .unwrap_or(&dir)
            .join(format!("synthetic_{k}.ogg"));
        std::fs::write(&path, &ogg).unwrap();
        let lv = if have_lv {
            ffmpeg_decode(&path, "libvorbis").map(|r| vorbis_to_wave(&r, ch))
        } else {
            None
        };
        if keep.is_none() {
            let _ = std::fs::remove_file(&path);
        }
        let snr_lv = lv
            .as_ref()
            .map_or(f64::NAN, |r| compare_unclipped(r, &dec.samples));
        println!(
            "synthetic {k}: {ch} ch, blocks {}/{}, lookup2 {lookup2}: {} frames, peak {peak:.3}; \
             SNR vs libvorbis (16 bit) {snr_lv:.1} dB",
            bs[0],
            bs[1],
            dec.samples.len() / ch,
        );
        // libvorbis is the reference. ffmpeg's native decoder is not used:
        // it rejects lookup type 2 books and residue ranges beyond the block
        // size ("Insufficient output buffer"), both legal per the
        // specification.
        if let Some(r) = &lv {
            if r.len() != dec.samples.len() {
                failures.push(format!(
                    "synthetic {k}: length {} vs libvorbis {}",
                    dec.samples.len(),
                    r.len()
                ));
            }
            if !(snr_lv >= 60.0) {
                failures.push(format!("synthetic {k}: SNR vs libvorbis {snr_lv:.1} dB"));
            }
        }
    }
    assert!(failures.is_empty(), "{failures:#?}");
}
