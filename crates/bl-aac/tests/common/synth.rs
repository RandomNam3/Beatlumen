//! Generator of synthetic HE-AAC (v1/v2) ADTS streams with implicit SBR
//! signalling: a simple AAC-LC core (long windows, codebook 11 spectra)
//! plus SBR extension payloads (varying headers, grids, envelopes, noise
//! floors, sinusoids, inverse filtering, coupling) and optional parametric
//! stereo data. Used to compare against ffmpeg without committed fixtures.
#![allow(dead_code)]

pub struct BitWriter {
    pub bytes: Vec<u8>,
    bits: usize,
}

impl BitWriter {
    pub fn new() -> Self {
        BitWriter {
            bytes: Vec::new(),
            bits: 0,
        }
    }
    pub fn put(&mut self, v: u32, n: u32) {
        for i in (0..n).rev() {
            if self.bits % 8 == 0 {
                self.bytes.push(0);
            }
            if (v >> i) & 1 != 0 {
                let last = self.bytes.len() - 1;
                self.bytes[last] |= 0x80 >> (self.bits % 8);
            }
            self.bits += 1;
        }
    }
    pub fn code(&mut self, c: (u32, u8)) {
        self.put(c.0, c.1 as u32);
    }
    pub fn align(&mut self) {
        while self.bits % 8 != 0 {
            self.put(0, 1);
        }
    }
    pub fn len_bits(&self) -> usize {
        self.bits
    }
    pub fn append(&mut self, other: &BitWriter) {
        for i in 0..other.bits {
            let b = (other.bytes[i / 8] >> (7 - i % 8)) & 1;
            self.put(b as u32, 1);
        }
    }
}

struct Rng(u32);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 = self.0.wrapping_mul(1103515245).wrapping_add(12345);
        self.0 >> 8
    }
    fn range(&mut self, lo: i32, hi: i32) -> i32 {
        lo + (self.next() % (hi - lo + 1) as u32) as i32
    }
}

/// Codebook 11 codewords for magnitudes (y, z) in 0..=4.
const CB11: [[(u32, u8); 5]; 5] = [
    [(0, 4), (6, 5), (25, 6), (61, 7), (156, 8)],
    [(5, 5), (1, 4), (8, 5), (20, 6), (55, 7)],
    [(23, 6), (7, 5), (9, 5), (24, 6), (57, 7)],
    [(60, 7), (21, 6), (22, 6), (26, 6), (59, 7)],
    [(154, 8), (54, 7), (56, 7), (58, 7), (65, 7)],
];
/// Delta codewords for values -3..=3 (index v + 3).
const ENV_1_5_F: [(u32, u8); 7] = [(12, 4), (5, 3), (1, 2), (0, 2), (4, 3), (13, 4), (29, 5)];
const ENV_3_0_F: [(u32, u8); 7] = [(62, 6), (14, 4), (2, 2), (0, 1), (6, 3), (30, 5), (252, 8)];
const BAL_F: [(u32, u8); 7] = [(126, 7), (14, 4), (2, 2), (0, 1), (6, 3), (30, 5), (62, 6)];
const PS_IID_F: [(u32, u8); 7] = [(29, 5), (13, 4), (5, 3), (0, 1), (4, 3), (12, 4), (28, 5)];
const PS_ICC_F: [(u32, u8); 7] = [(126, 7), (30, 5), (6, 3), (0, 1), (2, 2), (14, 4), (62, 6)];

/// SBR header variants with their band counts at 44.1 kHz output:
/// (amp_res, start, stop, xover, freq_scale, alter, noise_bands,
/// limiter_bands, limiter_gains, interpol, smoothing, n_high, n_low, n_q).
const HEADERS: [(
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    u32,
    usize,
    usize,
    usize,
); 2] = [
    (0, 5, 9, 0, 2, 1, 2, 2, 2, 1, 1, 16, 8, 3),
    (1, 8, 14, 1, 1, 0, 1, 3, 1, 0, 0, 11, 6, 1),
];

pub struct Synth {
    pub stereo: bool,
    pub ps: bool,
    /// PS mixing procedure B (icc_mode 4) instead of A.
    pub ps_type_b: bool,
    pub frames: usize,
}

const MAX_SFB: usize = 40;
/// Number of spectral lines covered by MAX_SFB bands at 22.05 kHz.
const LINES: usize = 600;

fn write_ics(w: &mut BitWriter, rng: &mut Rng, frame: usize, ch: usize) {
    w.put(140, 8); // global_gain
                   // ics_info: reserved, ONLY_LONG, sine, max_sfb, no predictor.
    w.put(0, 1);
    w.put(0, 2);
    w.put(((frame / 7) & 1) as u32, 1);
    w.put(MAX_SFB as u32, 6);
    w.put(0, 1);
    // One section with codebook 11 over all bands (5-bit lengths, escape 31).
    w.put(11, 4);
    w.put(31, 5);
    w.put((MAX_SFB - 31) as u32, 5);
    // Scalefactors: all deltas 0.
    for _ in 0..MAX_SFB {
        w.put(0, 1);
    }
    w.put(0, 1); // pulse
    w.put(0, 1); // tns
    w.put(0, 1); // gain control
                 // Spectral data: a few tonal peaks drifting with the frame + noise.
    let peaks = [
        40 + (frame * 3 + ch * 17) % 60,
        150 + (frame * 5) % 90,
        300 + (frame * 2 + ch * 31) % 200,
        480,
    ];
    let mut k = 0;
    while k < LINES {
        let mut v = [0i32; 2];
        for (j, x) in v.iter_mut().enumerate() {
            let line = k + j;
            *x = if peaks.contains(&line) {
                4
            } else if rng.next() % 5 == 0 {
                rng.range(1, 2)
            } else {
                0
            };
            if *x != 0 && rng.next() & 1 == 1 {
                *x = -*x;
            }
        }
        w.code(CB11[v[0].unsigned_abs() as usize][v[1].unsigned_abs() as usize]);
        for x in v {
            if x != 0 {
                w.put((x < 0) as u32, 1);
            }
        }
        k += 2;
    }
}

struct SbrFrameChoice {
    num_env: usize,
    freq_res: [u32; 2],
    class_fixvar: bool,
}

fn write_grid(w: &mut BitWriter, c: &SbrFrameChoice) {
    if c.class_fixvar {
        w.put(1, 2); // FIXVAR
        w.put(0, 2); // var_bord_1 = 0 (ends at 16)
        w.put(1, 2); // num_rel_1 = 1 -> 2 envelopes
        w.put(1, 2); // rel_bord_1 = 4
        w.put(0, 2); // pointer (2 bits for 2 envelopes)
        w.put(c.freq_res[1], 1); // freq_res written in reverse order
        w.put(c.freq_res[0], 1);
    } else {
        w.put(0, 2); // FIXFIX
        w.put(if c.num_env == 2 { 1 } else { 0 }, 2);
        w.put(c.freq_res[0], 1);
    }
}

/// Picks a delta in -2..=2 keeping the running value within [lo, hi].
fn bounded_delta(rng: &mut Rng, acc: i32, lo: i32, hi: i32) -> i32 {
    (acc + rng.range(-2, 2)).clamp(lo, hi) - acc
}

fn write_env(
    w: &mut BitWriter,
    rng: &mut Rng,
    c: &SbrFrameChoice,
    amp_res: u32,
    nb: [usize; 2],
    balance: bool,
) {
    let eff_amp = if !c.class_fixvar && c.num_env == 1 {
        0
    } else {
        amp_res
    };
    for e in 0..c.num_env {
        let n = nb[c.freq_res[e] as usize];
        let (start_bits, lo, hi, book) = match (balance, eff_amp) {
            (false, 0) => (7, 14, 34, &ENV_1_5_F),
            (false, _) => (6, 7, 17, &ENV_3_0_F),
            (true, 0) => (6, 6, 18, &BAL_F),
            (true, _) => (5, 3, 9, &BAL_F),
        };
        let mut acc = rng.range(lo, hi);
        w.put(acc as u32, start_bits);
        for _ in 1..n {
            let d = bounded_delta(rng, acc, lo, hi);
            acc += d;
            w.code(book[(d + 3) as usize]);
        }
    }
}

fn write_noise(w: &mut BitWriter, rng: &mut Rng, num_noise: usize, n_q: usize, balance: bool) {
    let (lo, hi) = if balance { (3, 9) } else { (5, 14) };
    for _ in 0..num_noise {
        let mut acc = rng.range(lo, hi);
        w.put(acc as u32, 5);
        for _ in 1..n_q {
            let d = bounded_delta(rng, acc, lo, hi);
            acc += d;
            let book = if balance { &BAL_F } else { &ENV_3_0_F };
            w.code(book[(d + 3) as usize]);
        }
    }
}

fn write_ps(w: &mut BitWriter, rng: &mut Rng, frame: usize, type_b: bool) {
    let header = frame % 8 == 0;
    w.put(header as u32, 1);
    if header {
        w.put(1, 1); // enable_iid
        w.put(1, 3); // iid_mode 1: 20 bands, coarse
        w.put(1, 1); // enable_icc
        w.put(if type_b { 4 } else { 1 }, 3);
        w.put(0, 1); // enable_ext
    }
    w.put(0, 1); // frame_class FIX
    let num_env = if frame % 3 == 0 { 2 } else { 1 };
    w.put(if num_env == 2 { 2 } else { 1 }, 2);
    for _ in 0..num_env {
        w.put(0, 1); // iid df
        let mut acc = 0;
        for b in 0..20 {
            let d = if b == 0 {
                rng.range(-3, 3)
            } else {
                rng.range(-1, 1)
            };
            let d = (acc + d).clamp(-6, 6) - acc;
            acc += d;
            w.code(PS_IID_F[(d + 3) as usize]);
        }
    }
    for _ in 0..num_env {
        w.put(0, 1); // icc df
        let mut acc = 0;
        for b in 0..20 {
            let d = if b == 0 {
                rng.range(0, 3)
            } else {
                rng.range(-1, 1)
            };
            let d = (acc + d).clamp(0, 7) - acc;
            acc += d;
            w.code(PS_ICC_F[(d + 3) as usize]);
        }
    }
}

fn write_sbr(rng: &mut Rng, s: &Synth, frame: usize) -> BitWriter {
    let mut w = BitWriter::new();
    w.put(13, 4); // EXT_SBR_DATA
    let hidx = (frame / 40) % HEADERS.len();
    let h = HEADERS[hidx];
    let header = frame % 8 == 0 || frame % 40 == 0;
    w.put(header as u32, 1);
    if header {
        w.put(h.0, 1);
        w.put(h.1, 4);
        w.put(h.2, 4);
        w.put(h.3, 3);
        w.put(0, 2);
        w.put(1, 1);
        w.put(1, 1);
        w.put(h.4, 2);
        w.put(h.5, 1);
        w.put(h.6, 2);
        w.put(h.7, 2);
        w.put(h.8, 2);
        w.put(h.9, 1);
        w.put(h.10, 1);
    }
    let (n_high, n_low, n_q) = (h.11, h.12, h.13);
    let nb = [n_low, n_high];
    let choice = match frame % 3 {
        0 => SbrFrameChoice {
            num_env: 1,
            freq_res: [1, 1],
            class_fixvar: false,
        },
        1 => SbrFrameChoice {
            num_env: 2,
            freq_res: [0, 0],
            class_fixvar: false,
        },
        _ => SbrFrameChoice {
            num_env: 2,
            freq_res: [1, 0],
            class_fixvar: true,
        },
    };
    let num_noise = if choice.num_env > 1 { 2 } else { 1 };
    let nch = if s.stereo { 2 } else { 1 };
    w.put(0, 1); // bs_data_extra
    let coupling = s.stereo && frame % 2 == 1;
    if s.stereo {
        w.put(coupling as u32, 1);
    }
    let grids = if s.stereo && !coupling { 2 } else { 1 };
    for _ in 0..grids {
        write_grid(&mut w, &choice);
    }
    for _ in 0..nch {
        for _ in 0..choice.num_env + num_noise {
            w.put(0, 1); // dtdf: frequency direction
        }
    }
    for _ in 0..grids {
        for i in 0..n_q {
            w.put(((frame + i) % 4) as u32, 2);
        }
    }
    if coupling {
        write_env(&mut w, rng, &choice, h.0, nb, false);
        write_noise(&mut w, rng, num_noise, n_q, false);
        write_env(&mut w, rng, &choice, h.0, nb, true);
        write_noise(&mut w, rng, num_noise, n_q, true);
    } else {
        for _ in 0..nch {
            write_env(&mut w, rng, &choice, h.0, nb, false);
        }
        for _ in 0..nch {
            write_noise(&mut w, rng, num_noise, n_q, false);
        }
    }
    for _ in 0..nch {
        let harm = frame % 4 == 2;
        w.put(harm as u32, 1);
        if harm {
            for i in 0..n_high {
                w.put((i % 5 == 1) as u32, 1);
            }
        }
    }
    if s.ps && !s.stereo {
        let mut ps = BitWriter::new();
        write_ps(&mut ps, rng, frame, s.ps_type_b);
        let cnt = (2 + ps.len_bits() + 7) / 8;
        w.put(1, 1); // bs_extended_data
        if cnt >= 15 {
            w.put(15, 4);
            w.put((cnt - 15) as u32, 8);
        } else {
            w.put(cnt as u32, 4);
        }
        w.put(2, 2); // EXTENSION_ID_PS
        w.append(&ps);
        for _ in 0..(cnt * 8 - 2 - ps.len_bits()) {
            w.put(0, 1);
        }
    } else {
        w.put(0, 1);
    }
    w.align();
    w
}

/// Builds the ADTS stream.
pub fn generate(s: &Synth) -> Vec<u8> {
    let mut rng = Rng(0x1234_5678 ^ (s.stereo as u32) ^ ((s.ps as u32) << 1));
    let mut out = Vec::new();
    for frame in 0..s.frames {
        let mut w = BitWriter::new();
        if s.stereo {
            w.put(1, 3); // CPE
            w.put(0, 4);
            w.put(0, 1); // common_window
            write_ics(&mut w, &mut rng, frame, 0);
            write_ics(&mut w, &mut rng, frame, 1);
        } else {
            w.put(0, 3); // SCE
            w.put(0, 4);
            write_ics(&mut w, &mut rng, frame, 0);
        }
        let sbr = write_sbr(&mut rng, s, frame);
        let cnt = sbr.bytes.len();
        w.put(6, 3); // FIL
        if cnt >= 15 {
            w.put(15, 4);
            w.put((cnt - 14) as u32, 8);
        } else {
            w.put(cnt as u32, 4);
        }
        for &b in &sbr.bytes {
            w.put(b as u32, 8);
        }
        w.put(7, 3); // END
        w.align();
        let len = w.bytes.len() + 7;
        let ch = if s.stereo { 2u8 } else { 1 };
        let sfi = 7u8; // 22050 Hz core
        out.extend_from_slice(&[
            0xff,
            0xf1,
            (1 << 6) | (sfi << 2) | (ch >> 2),
            ((ch & 3) << 6) | ((len >> 11) as u8 & 3),
            (len >> 3) as u8,
            ((len as u8 & 7) << 5) | 0x1f,
            0xfc,
        ]);
        out.extend_from_slice(&w.bytes);
    }
    out
}
