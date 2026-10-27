//! Polyphase synthesis filterbank (32-band matrixing via a fast DCT-II and
//! the 512-tap ISO window), shared by all layers.
//!
//! Ported from minimp3 (<https://github.com/lieff/minimp3>), which is
//! dedicated to the public domain under CC0 1.0 by its author(s).
//! The window coefficients are the ISO/IEC 11172-3 table 3-B.3 values
//! scaled by 2^16.

const SEC: [f32; 24] = [
    10.19000816,
    0.50060302,
    0.50241929,
    3.40760851,
    0.50547093,
    0.52249861,
    2.05778098,
    0.51544732,
    0.56694406,
    1.48416460,
    0.53104258,
    0.64682180,
    1.16943991,
    0.55310392,
    0.78815460,
    0.97256821,
    0.58293498,
    1.06067765,
    0.83934963,
    0.62250412,
    1.72244716,
    0.74453628,
    0.67480832,
    5.10114861,
];

/// `L` time slots processed in lock step (vectorizes for `L = 4`).
#[derive(Clone, Copy)]
struct V<const L: usize>([f32; L]);

impl<const L: usize> V<L> {
    #[inline(always)]
    fn load(g: &[f32], at: usize) -> Self {
        let mut r = [0.0; L];
        r.copy_from_slice(&g[at..at + L]);
        V(r)
    }
    #[inline(always)]
    fn store(self, g: &mut [f32], at: usize) {
        g[at..at + L].copy_from_slice(&self.0);
    }
}

impl<const L: usize> std::ops::Add for V<L> {
    type Output = Self;
    #[inline(always)]
    fn add(self, o: Self) -> Self {
        let mut r = self.0;
        for (a, b) in r.iter_mut().zip(o.0) {
            *a += b;
        }
        V(r)
    }
}

impl<const L: usize> std::ops::Sub for V<L> {
    type Output = Self;
    #[inline(always)]
    fn sub(self, o: Self) -> Self {
        let mut r = self.0;
        for (a, b) in r.iter_mut().zip(o.0) {
            *a -= b;
        }
        V(r)
    }
}

impl<const L: usize> std::ops::Mul<f32> for V<L> {
    type Output = Self;
    #[inline(always)]
    fn mul(self, s: f32) -> Self {
        let mut r = self.0;
        for a in r.iter_mut() {
            *a *= s;
        }
        V(r)
    }
}

/// 32-point DCT-II of time slots `k..k + L` of one channel's subband
/// samples (`g[sb * 18 + t]`), in place.
#[inline(always)]
fn dct2_lanes<const L: usize>(g: &mut [f32], k: usize) {
    let z = V([0.0; L]);
    let mut t = [[z; 8]; 4];
    for i in 0..8 {
        let x0 = V::<L>::load(g, k + i * 18);
        let x1 = V::<L>::load(g, k + (15 - i) * 18);
        let x2 = V::<L>::load(g, k + (16 + i) * 18);
        let x3 = V::<L>::load(g, k + (31 - i) * 18);
        let t0 = x0 + x3;
        let t1 = x1 + x2;
        let t2 = (x1 - x2) * SEC[3 * i];
        let t3 = (x0 - x3) * SEC[3 * i + 1];
        t[0][i] = t0 + t1;
        t[1][i] = (t0 - t1) * SEC[3 * i + 2];
        t[2][i] = t3 + t2;
        t[3][i] = (t3 - t2) * SEC[3 * i + 2];
    }
    for x in t.iter_mut() {
        let (mut x0, mut x1, mut x2, mut x3) = (x[0], x[1], x[2], x[3]);
        let (mut x4, mut x5, mut x6, mut x7) = (x[4], x[5], x[6], x[7]);
        let mut xt = x0 - x7;
        x0 = x0 + x7;
        x7 = x1 - x6;
        x1 = x1 + x6;
        x6 = x2 - x5;
        x2 = x2 + x5;
        x5 = x3 - x4;
        x3 = x3 + x4;
        x4 = x0 - x3;
        x0 = x0 + x3;
        x3 = x1 - x2;
        x1 = x1 + x2;
        x[0] = x0 + x1;
        x[4] = (x0 - x1) * 0.70710677;
        x5 = x5 + x6;
        x6 = (x6 + x7) * 0.70710677;
        x7 = x7 + xt;
        x3 = (x3 + x4) * 0.70710677;
        x5 = x5 - x7 * 0.198912367; // rotate by PI/8
        x7 = x7 + x5 * 0.382683432;
        x5 = x5 - x7 * 0.198912367;
        x0 = xt - x6;
        xt = xt + x6;
        x[1] = (xt + x7) * 0.50979561;
        x[2] = (x4 + x3) * 0.54119611;
        x[3] = (x0 - x5) * 0.60134488;
        x[5] = (x0 + x5) * 0.89997619;
        x[6] = (x4 - x3) * 1.30656302;
        x[7] = (xt - x7) * 2.56291556;
    }
    let mut y = k;
    for i in 0..7 {
        t[0][i].store(g, y);
        (t[2][i] + t[3][i] + t[3][i + 1]).store(g, y + 18);
        (t[1][i] + t[1][i + 1]).store(g, y + 2 * 18);
        (t[2][i + 1] + t[3][i] + t[3][i + 1]).store(g, y + 3 * 18);
        y += 4 * 18;
    }
    t[0][7].store(g, y);
    (t[2][7] + t[3][7]).store(g, y + 18);
    t[1][7].store(g, y + 2 * 18);
    t[3][7].store(g, y + 3 * 18);
}

/// 32-point DCT-II applied in place to `n` time slots of one channel's
/// subband samples (`g[sb * 18 + t]`).
fn dct2(g: &mut [f32], n: usize) {
    let mut k = 0;
    while k + 4 <= n {
        dct2_lanes::<4>(g, k);
        k += 4;
    }
    while k < n {
        dct2_lanes::<1>(g, k);
        k += 1;
    }
}

const WIN: [[f32; 16]; 15] = [
    [
        -1., 26., -31., 208., 218., 401., -519., 2063., 2000., 4788., -5517., 7134., 5959., 35640.,
        -39336., 74992.,
    ],
    [
        -1., 24., -35., 202., 222., 347., -581., 2080., 1952., 4425., -5879., 7640., 5288., 33791.,
        -41176., 74856.,
    ],
    [
        -1., 21., -38., 196., 225., 294., -645., 2087., 1893., 4063., -6237., 8092., 4561., 31947.,
        -43006., 74630.,
    ],
    [
        -1., 19., -41., 190., 227., 244., -711., 2085., 1822., 3705., -6589., 8492., 3776., 30112.,
        -44821., 74313.,
    ],
    [
        -1., 17., -45., 183., 228., 197., -779., 2075., 1739., 3351., -6935., 8840., 2935., 28289.,
        -46617., 73908.,
    ],
    [
        -1., 16., -49., 176., 228., 153., -848., 2057., 1644., 3004., -7271., 9139., 2037., 26482.,
        -48390., 73415.,
    ],
    [
        -2., 14., -53., 169., 227., 111., -919., 2032., 1535., 2663., -7597., 9389., 1082., 24694.,
        -50137., 72835.,
    ],
    [
        -2., 13., -58., 161., 224., 72., -991., 2001., 1414., 2330., -7910., 9592., 70., 22929.,
        -51853., 72169.,
    ],
    [
        -2., 11., -63., 154., 221., 36., -1064., 1962., 1280., 2006., -8209., 9750., -998., 21189.,
        -53534., 71420.,
    ],
    [
        -2., 10., -68., 147., 215., 2., -1137., 1919., 1131., 1692., -8491., 9863., -2122., 19478.,
        -55178., 70590.,
    ],
    [
        -3., 9., -73., 139., 208., -29., -1210., 1870., 970., 1388., -8755., 9935., -3300., 17799.,
        -56778., 69679.,
    ],
    [
        -3., 8., -79., 132., 200., -57., -1283., 1817., 794., 1095., -8998., 9966., -4533., 16155.,
        -58333., 68692.,
    ],
    [
        -4., 7., -85., 125., 189., -83., -1356., 1759., 605., 814., -9219., 9959., -5818., 14548.,
        -59838., 67629.,
    ],
    [
        -4., 7., -91., 117., 177., -106., -1428., 1698., 402., 545., -9416., 9916., -7154., 12980.,
        -61289., 66494.,
    ],
    [
        -5., 6., -97., 111., 163., -127., -1498., 1634., 185., 288., -9585., 9838., -8540., 11455.,
        -62684., 65290.,
    ],
];

const OUT_SCALE: f32 = 1.0 / 32768.0;

/// Four interleaved filterbank lanes: [left t, right t, left t+1, right t+1].
type Lanes = [f32; 4];
/// One history row: 16 positions of 4 lanes.
type Row = [Lanes; ROW];
/// Positions per history row (16 positions of 4 lanes = 64 floats).
const ROW: usize = 16;
/// History rows kept between granules.
const QMF_ROWS: usize = 15;
/// Rows of scratch for one granule of up to 18 time slots.
const LINS_ROWS: usize = 18 + 15;
/// Rows touched while synthesizing one slot pair.
const WIN_ROWS: usize = 17;

/// Builds a lane vector element-wise (written out so it vectorizes).
#[inline(always)]
fn lanes(f: impl Fn(usize) -> f32) -> Lanes {
    [f(0), f(1), f(2), f(3)]
}

/// Computes the two output samples (offsets 0 and 16) whose window taps
/// are symmetric, from `lane` of position 15 starting at row `r0`.
#[inline(always)]
fn synth_pair(z: &[Row; WIN_ROWS], r0: usize, lane: usize) -> (f32, f32) {
    let p = |r: usize| z[r0 + r][15][lane];
    let q = |r: usize| z[r0 + r][15][lane + 2];
    let mut a = (p(14) - p(0)) * 29.;
    a += (p(1) + p(13)) * 213.;
    a += (p(12) - p(2)) * 459.;
    a += (p(3) + p(11)) * 2037.;
    a += (p(10) - p(4)) * 5153.;
    a += (p(5) + p(9)) * 6574.;
    a += (p(8) - p(6)) * 37489.;
    a += p(7) * 75038.;

    let mut b = q(14) * 104.;
    b += q(12) * 1567.;
    b += q(10) * 9727.;
    b += q(8) * 64019.;
    b += q(6) * -9975.;
    b += q(4) * -45.;
    b += q(2) * 146.;
    b += q(0) * -5.;
    (a * OUT_SCALE, b * OUT_SCALE)
}

/// Synthesizes two consecutive time slots (`t`, `t + 1`) of both channels
/// into 64 output samples per channel (interleaved by `nch` in `pcm`).
/// `z` is the window of history rows belonging to this slot pair.
fn synth_slots(g: &[f32; 1152], t: usize, nch: usize, pcm: &mut [f32], z: &mut [Row; WIN_ROWS]) {
    // Matrixed (DCT) inputs of both slots, by lane.
    let mut x = [[0f32; 32]; 4];
    let r = 576 * (nch - 1);
    for sb in 0..32 {
        x[0][sb] = g[sb * 18 + t];
        x[1][sb] = g[r + sb * 18 + t];
        x[2][sb] = g[sb * 18 + t + 1];
        x[3][sb] = g[r + sb * 18 + t + 1];
    }
    z[15][15] = [x[0][16], x[1][16], x[0][0], x[1][0]];
    z[16][15] = [x[2][16], x[3][16], x[2][0], x[3][0]];

    let mut out = [[0f32; 64]; 2];
    let (a, b) = synth_pair(z, 0, 1);
    out[1][0] = a;
    out[1][16] = b;
    let (a, b) = synth_pair(z, 1, 1);
    out[1][32] = a;
    out[1][48] = b;
    let (a, b) = synth_pair(z, 0, 0);
    out[0][0] = a;
    out[0][16] = b;
    let (a, b) = synth_pair(z, 1, 0);
    out[0][32] = a;
    out[0][48] = b;

    // Each output position `i` only touches column `i` of the rows, so all
    // new inputs are stored first and the windowing is done afterwards.
    for i in 0..15 {
        z[15][i] = [x[0][31 - i], x[1][31 - i], x[2][31 - i], x[3][31 - i]];
        z[16][i][0] = x[2][1 + i];
        z[16][i][1] = x[3][1 + i];
        z[14][i][2] = x[0][1 + i];
        z[14][i][3] = x[1][1 + i];
    }
    window(z, &mut out);
    if nch == 1 {
        pcm[..64].copy_from_slice(&out[0]);
    } else {
        for (d, (l, r)) in pcm[..128]
            .chunks_exact_mut(2)
            .zip(out[0].iter().zip(out[1].iter()))
        {
            d[0] = *l;
            d[1] = *r;
        }
    }
}

/// Windowing of output positions 1..=15 and 17..=31 (of each half) of a
/// slot pair. Kept out of line so the lane arithmetic is vectorized on
/// loads rather than on the values just stored.
#[inline(never)]
fn window(z: &[Row; WIN_ROWS], out: &mut [[f32; 64]; 2]) {
    let mut ab = [[[0f32; 4]; 2]; 15];
    for (i, r) in ab.iter_mut().enumerate() {
        let w = &WIN[14 - i];
        let tap = |k: usize| (z[15 - k][i], z[k][i], w[2 * k], w[2 * k + 1]);
        // b += zz*w1 + yy*w0; a += zz*w0 - yy*w1 (even k) / yy*w1 - zz*w0 (odd k)
        let (zz, yy, w0, w1) = tap(0);
        let mut b = lanes(|j| zz[j] * w1 + yy[j] * w0);
        let mut a = lanes(|j| zz[j] * w0 - yy[j] * w1);
        for k in (1..8).step_by(2) {
            let (zz, yy, w0, w1) = tap(k);
            b = lanes(|j| b[j] + (zz[j] * w1 + yy[j] * w0));
            a = lanes(|j| a[j] + (yy[j] * w1 - zz[j] * w0));
            if k < 7 {
                let (zz, yy, w0, w1) = tap(k + 1);
                b = lanes(|j| b[j] + (zz[j] * w1 + yy[j] * w0));
                a = lanes(|j| a[j] + (zz[j] * w0 - yy[j] * w1));
            }
        }
        *r = [a, b];
    }
    for (i, [a, b]) in ab.into_iter().enumerate() {
        out[1][15 - i] = a[1] * OUT_SCALE;
        out[1][17 + i] = b[1] * OUT_SCALE;
        out[0][15 - i] = a[0] * OUT_SCALE;
        out[0][17 + i] = b[0] * OUT_SCALE;
        out[1][47 - i] = a[3] * OUT_SCALE;
        out[1][49 + i] = b[3] * OUT_SCALE;
        out[0][47 - i] = a[2] * OUT_SCALE;
        out[0][49 + i] = b[2] * OUT_SCALE;
    }
}

/// Runs the synthesis filterbank over `nslots` (even, <= 18) time slots of
/// subband samples in `g` (channel `c` at `g[c * 576 + sb * 18 + t]`),
/// writing `32 * nslots` interleaved samples per channel to `pcm`.
/// `g` is used as scratch.
fn synth_granule(
    qmf: &mut [Row; QMF_ROWS],
    g: &mut [f32; 1152],
    nslots: usize,
    nch: usize,
    pcm: &mut [f32],
    lins: &mut [Row; LINS_ROWS],
) {
    for c in 0..nch {
        dct2(&mut g[c * 576..c * 576 + 576], nslots);
    }
    lins[..QMF_ROWS].copy_from_slice(&qmf[..]);
    for t in (0..nslots).step_by(2) {
        let z: &mut [Row; WIN_ROWS] = (&mut lins[t..t + WIN_ROWS])
            .try_into()
            .expect("window in bounds");
        synth_slots(g, t, nch, &mut pcm[32 * nch * t..32 * nch * (t + 2)], z);
    }
    let tail = &lins[nslots..nslots + QMF_ROWS];
    if nch == 1 {
        // Keep the right-channel lanes untouched for mono frames (as the
        // reference decoder does when a stream switches channel modes).
        for (q, t) in qmf.iter_mut().flatten().zip(tail.iter().flatten()) {
            q[0] = t[0];
            q[2] = t[2];
        }
    } else {
        qmf.copy_from_slice(tail);
    }
}

/// Synthesis filterbank state plus the shared subband sample buffer.
pub(crate) struct SynthState {
    /// Subband samples of the current granule, `[ch * 576 + sb * 18 + t]`.
    pub grbuf: Box<[f32; 1152]>,
    qmf: Box<[Row; QMF_ROWS]>,
    lins: Box<[Row; LINS_ROWS]>,
}

impl SynthState {
    pub fn new() -> Self {
        SynthState {
            grbuf: Box::new([0.0; 1152]),
            qmf: Box::new([[[0.0; 4]; ROW]; QMF_ROWS]),
            lins: Box::new([[[0.0; 4]; ROW]; LINS_ROWS]),
        }
    }

    pub fn reset(&mut self) {
        self.qmf.fill([[0.0; 4]; ROW]);
    }

    /// Synthesizes `nslots` time slots from `grbuf` into `pcm`.
    pub fn run(&mut self, nslots: usize, nch: usize, pcm: &mut [f32]) {
        synth_granule(
            &mut self.qmf,
            &mut self.grbuf,
            nslots,
            nch,
            pcm,
            &mut self.lins,
        );
    }
}
