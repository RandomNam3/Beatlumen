//! Spectral Band Replication (HE-AAC v1, ISO/IEC 14496-3 4.6.18): bitstream
//! parsing (sbr_extension_data), envelope/noise decoding, and per-element
//! state. The signal processing lives in [`process`].

mod freq;
mod process;
pub(crate) mod qmf;
mod tables;

use crate::bits::BitReader;
use crate::fft::Complex;
use crate::huffman::HuffTable;
use crate::ps::Ps;
use crate::sbr_huffman_tables::*;
use crate::Error;
use freq::FreqTables;
use qmf::{Analysis, Synthesis, SynthesisDs};
use std::sync::OnceLock;

pub(crate) const MAX_ENV: usize = 5;
const MAX_NOISE_ENV: usize = 2;
const MAX_BANDS: usize = 64;
/// QMF slots per frame (16 time slots x RATE 2).
pub(crate) const SLOTS: usize = 32;
/// HF generator history (t_HFGen) and adjustment offset (t_HFAdj).
const T_HFGEN: usize = 8;
const T_HFADJ: usize = 2;

const FIXFIX: u8 = 0;
const FIXVAR: u8 = 1;
const VARFIX: u8 = 2;
#[allow(dead_code)]
const VARVAR: u8 = 3;

const EXTENSION_ID_PS: u32 = 2;

/// One QMF slot of complex subband samples.
pub(crate) type Slot = [Complex; 64];

#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct SbrHeader {
    pub amp_res: bool,
    pub start_freq: u8,
    pub stop_freq: u8,
    pub xover_band: u8,
    pub freq_scale: u8,
    pub alter_scale: bool,
    pub noise_bands: u8,
    pub limiter_bands: u8,
    pub limiter_gains: u8,
    pub interpol_freq: bool,
    pub smoothing_mode: bool,
}

impl SbrHeader {
    fn parse(r: &mut BitReader) -> SbrHeader {
        let amp_res = r.read_bit();
        let start_freq = r.read(4) as u8;
        let stop_freq = r.read(4) as u8;
        let xover_band = r.read(3) as u8;
        r.skip(2);
        let extra1 = r.read_bit();
        let extra2 = r.read_bit();
        let mut h = SbrHeader {
            amp_res,
            start_freq,
            stop_freq,
            xover_band,
            freq_scale: 2,
            alter_scale: true,
            noise_bands: 2,
            limiter_bands: 2,
            limiter_gains: 2,
            interpol_freq: true,
            smoothing_mode: true,
        };
        if extra1 {
            h.freq_scale = r.read(2) as u8;
            h.alter_scale = r.read_bit();
            h.noise_bands = r.read(2) as u8;
        }
        if extra2 {
            h.limiter_bands = r.read(2) as u8;
            h.limiter_gains = r.read(2) as u8;
            h.interpol_freq = r.read_bit();
            h.smoothing_mode = r.read_bit();
        }
        h
    }

    /// Fields whose change requires recomputing the frequency tables.
    fn table_key(&self) -> (u8, u8, u8, u8, bool, u8, u8) {
        (self.start_freq, self.stop_freq, self.xover_band, self.freq_scale, self.alter_scale, self.noise_bands, self.limiter_bands)
    }
}

/// Time/frequency grid of one channel (sbr_grid and derived borders).
#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Grid {
    pub class: u8,
    pub num_env: usize,
    pub t_e: [usize; MAX_ENV + 1],
    pub freq_res: [u8; MAX_ENV],
    pub pointer: usize,
    /// Transient envelope index (l_A), -1 if none.
    pub l_a: i32,
    pub num_noise: usize,
    pub t_q: [usize; MAX_NOISE_ENV + 1],
}

/// Parsed per-channel SBR data of the current frame.
#[derive(Clone)]
pub(crate) struct ChannelFrame {
    pub grid: Grid,
    pub amp_res: bool,
    df_env: [bool; MAX_ENV],
    df_noise: [bool; MAX_NOISE_ENV],
    pub invf: [u8; 5],
    env: [[i32; MAX_BANDS]; MAX_ENV],
    noise: [[i32; 5]; MAX_NOISE_ENV],
    pub add_harmonic_flag: bool,
    pub add_harmonic: [bool; MAX_BANDS],
}

impl Default for ChannelFrame {
    fn default() -> Self {
        ChannelFrame {
            grid: Grid::default(),
            amp_res: false,
            df_env: [false; MAX_ENV],
            df_noise: [false; MAX_NOISE_ENV],
            invf: [0; 5],
            env: [[0; MAX_BANDS]; MAX_ENV],
            noise: [[0; 5]; MAX_NOISE_ENV],
            add_harmonic_flag: false,
            add_harmonic: [false; MAX_BANDS],
        }
    }
}

/// Per-channel SBR state.
pub(crate) struct SbrChannel {
    pub ana: Analysis,
    pub syn: Synthesis,
    pub syn_ds: SynthesisDs,
    /// Low band QMF samples: T_HFGEN history slots + 32 new slots.
    pub x_low: Vec<[Complex; 32]>,
    /// Envelope-adjusted high band, indexed by HF slot (0..38).
    pub y: Vec<Slot>,
    pub frame: ChannelFrame,
    /// Dequantised envelope / noise floor of the current frame.
    pub e_orig: [[f32; MAX_BANDS]; MAX_ENV],
    pub q_orig: [[f32; 5]; MAX_NOISE_ENV],
    /// Delta-decoding state: last envelope (high resolution) and noise.
    prev_env: [i32; MAX_BANDS],
    prev_noise: [i32; 5],
    /// Absolute (delta-decoded) values of the current frame.
    env_abs: [[i32; MAX_BANDS]; MAX_ENV],
    noise_abs: [[i32; 5]; MAX_NOISE_ENV],
    // Processing state carried between frames.
    pub prev_l_a_last: bool,
    pub bw_prev: [f32; 5],
    pub invf_prev: [u8; 5],
    pub s_index_prev: [bool; MAX_BANDS],
    pub g_hist: [[f32; MAX_BANDS]; 4],
    pub q_hist: [[f32; MAX_BANDS]; 4],
    pub hist_valid: bool,
    pub index_noise: usize,
    pub index_sine: usize,
    pub kx_prev: usize,
    pub m_prev: usize,
}

impl SbrChannel {
    fn new() -> Self {
        SbrChannel {
            ana: Analysis::new(),
            syn: Synthesis::new(),
            syn_ds: SynthesisDs::new(),
            x_low: vec![[Complex::default(); 32]; T_HFGEN + SLOTS],
            y: vec![[Complex::default(); 64]; SLOTS + 8],
            frame: ChannelFrame::default(),
            e_orig: [[0.0; MAX_BANDS]; MAX_ENV],
            q_orig: [[0.0; 5]; MAX_NOISE_ENV],
            prev_env: [0; MAX_BANDS],
            prev_noise: [0; 5],
            env_abs: [[0; MAX_BANDS]; MAX_ENV],
            noise_abs: [[0; 5]; MAX_NOISE_ENV],
            prev_l_a_last: false,
            bw_prev: [0.0; 5],
            invf_prev: [0; 5],
            s_index_prev: [false; MAX_BANDS],
            g_hist: [[0.0; MAX_BANDS]; 4],
            q_hist: [[0.0; MAX_BANDS]; 4],
            hist_valid: false,
            index_noise: 0,
            index_sine: 0,
            kx_prev: 32,
            m_prev: 0,
        }
    }

    fn reset_tables(&mut self) {
        self.prev_env = [0; MAX_BANDS];
        self.prev_noise = [0; 5];
        self.hist_valid = false;
        self.index_noise = 0;
    }

    fn reset(&mut self) {
        self.ana.reset();
        self.syn.reset();
        self.syn_ds.reset();
        for s in &mut self.x_low {
            *s = [Complex::default(); 32];
        }
        for s in &mut self.y {
            *s = [Complex::default(); 64];
        }
        self.reset_tables();
        self.kx_prev = 32;
        self.m_prev = 0;
    }
}

struct SbrBooks {
    env_1_5_t: HuffTable,
    env_1_5_f: HuffTable,
    bal_1_5_t: HuffTable,
    bal_1_5_f: HuffTable,
    env_3_0_t: HuffTable,
    env_3_0_f: HuffTable,
    bal_3_0_t: HuffTable,
    bal_3_0_f: HuffTable,
    noise_t: HuffTable,
    noise_bal_t: HuffTable,
}

fn books() -> &'static SbrBooks {
    static B: OnceLock<SbrBooks> = OnceLock::new();
    B.get_or_init(|| SbrBooks {
        env_1_5_t: HuffTable::new(&SBR_ENV_1_5DB_T, 9),
        env_1_5_f: HuffTable::new(&SBR_ENV_1_5DB_F, 9),
        bal_1_5_t: HuffTable::new(&SBR_ENV_BAL_1_5DB_T, 9),
        bal_1_5_f: HuffTable::new(&SBR_ENV_BAL_1_5DB_F, 9),
        env_3_0_t: HuffTable::new(&SBR_ENV_3_0DB_T, 9),
        env_3_0_f: HuffTable::new(&SBR_ENV_3_0DB_F, 9),
        bal_3_0_t: HuffTable::new(&SBR_ENV_BAL_3_0DB_T, 9),
        bal_3_0_f: HuffTable::new(&SBR_ENV_BAL_3_0DB_F, 9),
        noise_t: HuffTable::new(&SBR_NOISE_3_0DB_T, 9),
        noise_bal_t: HuffTable::new(&SBR_NOISE_BAL_3_0DB_T, 9),
    })
}

#[inline]
fn huff(r: &mut BitReader, t: &HuffTable, offset: i32) -> Result<i32, Error> {
    t.decode(r).map(|v| v as i32 - offset).ok_or(Error::InvalidData("invalid SBR huffman code"))
}

/// SBR state of one SCE/CPE element.
pub(crate) struct SbrElement {
    /// SBR (output) sampling rate.
    rate: u32,
    header: Option<SbrHeader>,
    pub(crate) freq: Option<FreqTables>,
    /// The frequency tables changed since the last processed frame.
    reset: bool,
    coupling: bool,
    pub channels: Vec<SbrChannel>,
    /// Valid SBR data was parsed for the current frame.
    data_valid: bool,
    pub ps: Option<Box<Ps>>,
    /// PS data was seen in this element.
    pub ps_seen: bool,
    /// Synthesis filterbank of the PS right channel.
    ps_syn: Synthesis,
    ps_syn_ds: SynthesisDs,
    /// Downsampled SBR: output at the core rate (32-band synthesis).
    pub downsampled: bool,
}

impl SbrElement {
    pub fn new(rate: u32, nch: usize) -> Self {
        SbrElement {
            rate,
            header: None,
            freq: None,
            reset: true,
            coupling: false,
            channels: (0..nch).map(|_| SbrChannel::new()).collect(),
            data_valid: false,
            ps: None,
            ps_seen: false,
            ps_syn: Synthesis::new(),
            ps_syn_ds: SynthesisDs::new(),
            downsampled: false,
        }
    }

    pub fn reset(&mut self) {
        for c in &mut self.channels {
            c.reset();
        }
        self.reset = true;
        self.data_valid = false;
        if let Some(ps) = &mut self.ps {
            ps.reset();
        }
        self.ps_syn.reset();
        self.ps_syn_ds.reset();
    }

    /// Marks the start of a new frame (no SBR data parsed yet).
    pub fn begin_frame(&mut self) {
        self.data_valid = false;
    }

    /// Parses sbr_extension_data (the payload after the 4-bit extension
    /// type) for this element. On error the frame is treated as having no
    /// SBR data.
    pub fn parse(&mut self, r: &mut BitReader, crc: bool, with_ps: bool) -> Result<(), Error> {
        self.data_valid = false;
        if crc {
            r.skip(10);
        }
        if r.read_bit() {
            let h = SbrHeader::parse(r);
            r.check()?;
            let changed = self.header.map_or(true, |old| old.table_key() != h.table_key());
            self.header = Some(h);
            if changed || self.freq.is_none() {
                self.freq = freq::compute(&h, self.rate);
                self.reset = true;
                for c in &mut self.channels {
                    c.reset_tables();
                }
            }
        }
        let (Some(h), Some(ft)) = (self.header, self.freq.clone()) else {
            return Ok(());
        };
        let res = if self.channels.len() == 2 { self.parse_cpe(r, &h, &ft) } else { self.parse_sce(r, &h, &ft, with_ps) };
        res?;
        r.check()?;
        self.decode_envelopes(&ft);
        self.data_valid = true;
        Ok(())
    }

    fn parse_sce(&mut self, r: &mut BitReader, h: &SbrHeader, ft: &FreqTables, with_ps: bool) -> Result<(), Error> {
        if r.read_bit() {
            r.skip(4);
        }
        self.coupling = false;
        let mut f = ChannelFrame::default();
        parse_grid(r, &mut f, h)?;
        parse_dtdf(r, &mut f);
        parse_invf(r, &mut f, ft);
        parse_envelope(r, &mut f, ft, false)?;
        parse_noise(r, &mut f, ft, false)?;
        parse_harmonics(r, &mut f, ft);
        self.channels[0].frame = f;
        self.parse_extended(r, with_ps)
    }

    fn parse_cpe(&mut self, r: &mut BitReader, h: &SbrHeader, ft: &FreqTables) -> Result<(), Error> {
        if r.read_bit() {
            r.skip(8);
        }
        self.coupling = r.read_bit();
        let mut f0 = ChannelFrame::default();
        let mut f1 = ChannelFrame::default();
        if self.coupling {
            parse_grid(r, &mut f0, h)?;
            f1.grid = f0.grid;
            f1.amp_res = f0.amp_res;
            parse_dtdf(r, &mut f0);
            parse_dtdf(r, &mut f1);
            parse_invf(r, &mut f0, ft);
            f1.invf = f0.invf;
            parse_envelope(r, &mut f0, ft, false)?;
            parse_noise(r, &mut f0, ft, false)?;
            parse_envelope(r, &mut f1, ft, true)?;
            parse_noise(r, &mut f1, ft, true)?;
        } else {
            parse_grid(r, &mut f0, h)?;
            parse_grid(r, &mut f1, h)?;
            parse_dtdf(r, &mut f0);
            parse_dtdf(r, &mut f1);
            parse_invf(r, &mut f0, ft);
            parse_invf(r, &mut f1, ft);
            parse_envelope(r, &mut f0, ft, false)?;
            parse_envelope(r, &mut f1, ft, false)?;
            parse_noise(r, &mut f0, ft, false)?;
            parse_noise(r, &mut f1, ft, false)?;
        }
        parse_harmonics(r, &mut f0, ft);
        parse_harmonics(r, &mut f1, ft);
        self.channels[0].frame = f0;
        self.channels[1].frame = f1;
        self.parse_extended(r, false)
    }

    fn parse_extended(&mut self, r: &mut BitReader, with_ps: bool) -> Result<(), Error> {
        if !r.read_bit() {
            return Ok(());
        }
        let mut cnt = r.read(4) as usize;
        if cnt == 15 {
            cnt += r.read(8) as usize;
        }
        let mut bits_left = 8 * cnt as isize;
        let end = r.position() + 8 * cnt;
        while bits_left > 7 {
            let id = r.read(2);
            bits_left -= 2;
            let start = r.position();
            if id == EXTENSION_ID_PS && with_ps {
                let mut sub = r.sub(bits_left.max(0) as usize);
                let ps = self.ps.get_or_insert_with(|| Box::new(Ps::new()));
                if ps.parse(&mut sub).is_ok() && !sub.overrun() {
                    self.ps_seen = true;
                    r.set_position(sub.position());
                } else {
                    ps.invalidate();
                    r.set_position(end);
                }
            } else {
                r.set_position(end);
            }
            bits_left -= (r.position() - start) as isize;
        }
        r.set_position(end);
        Ok(())
    }

    /// Delta decoding and dequantisation of envelopes and noise floors.
    fn decode_envelopes(&mut self, ft: &FreqTables) {
        for ch in &mut self.channels {
            delta_decode(ch, ft);
        }
        if self.coupling && self.channels.len() == 2 {
            let (a, b) = self.channels.split_at_mut(1);
            dequant_coupled(&mut a[0], &mut b[0], ft);
        } else {
            for ch in &mut self.channels {
                dequant(ch, ft);
            }
        }
    }

    /// Runs SBR on the core output of channel `ch` (1024 samples), writing
    /// the QMF-domain signal for synthesis into `x` (32 slots).
    pub fn process_channel(&mut self, ch: usize, core: &[f32], x: &mut [Slot]) {
        let qt = qmf::tables();
        let active = self.data_valid && self.freq.is_some() && self.header.is_some();
        let h = self.header.unwrap_or_default();
        let ft = self.freq.as_ref();
        let reset = self.reset;
        let chan = &mut self.channels[ch];
        for s in 0..SLOTS {
            chan.ana.process(qt, &core[s * 32..s * 32 + 32], &mut chan.x_low[T_HFGEN + s]);
        }
        process::process(chan, ft.filter(|_| active), &h, reset, x);
    }

    /// Synthesises a QMF-domain signal into 2048 output samples using
    /// channel `ch`'s synthesis state.
    pub fn synthesize(&mut self, ch: usize, x: &[Slot], out: &mut [f32]) {
        let qt = qmf::tables();
        let c = &mut self.channels[ch];
        for (s, slot) in x.iter().enumerate().take(SLOTS) {
            if self.downsampled {
                c.syn_ds.process(qt, slot, &mut out[s * 32..s * 32 + 32]);
            } else {
                c.syn.process(qt, slot, &mut out[s * 64..s * 64 + 64]);
            }
        }
    }

    /// Parametric stereo: splits the mono QMF signal into left and right.
    pub fn process_ps(&mut self, x: &[Slot], left: &mut [Slot], right: &mut [Slot]) {
        let usb = match (&self.freq, self.data_valid) {
            (Some(f), true) => f.kx + f.m,
            _ => 32,
        };
        match &mut self.ps {
            Some(ps) if ps.started() => ps.process(x, left, right, usb),
            _ => {
                // No PS data yet: plain mono upmix.
                left[..SLOTS].copy_from_slice(&x[..SLOTS]);
                right[..SLOTS].copy_from_slice(&x[..SLOTS]);
            }
        }
    }

    /// Synthesises the PS right channel.
    pub fn synthesize_ps_right(&mut self, x: &[Slot], out: &mut [f32]) {
        let qt = qmf::tables();
        for (s, slot) in x.iter().enumerate().take(SLOTS) {
            if self.downsampled {
                self.ps_syn_ds.process(qt, slot, &mut out[s * 32..s * 32 + 32]);
            } else {
                self.ps_syn.process(qt, slot, &mut out[s * 64..s * 64 + 64]);
            }
        }
    }

    /// Called once all channels of the element were processed.
    pub fn end_frame(&mut self) {
        if self.data_valid {
            self.reset = false;
        }
    }
}

fn parse_grid(r: &mut BitReader, f: &mut ChannelFrame, h: &SbrHeader) -> Result<(), Error> {
    let g = &mut f.grid;
    g.class = r.read(2) as u8;
    let mut rel0 = [0usize; 4];
    let mut rel1 = [0usize; 4];
    let (abs_lead, abs_trail, n_rel0);
    match g.class {
        FIXFIX => {
            g.num_env = 1 << r.read(2);
            if g.num_env > 4 {
                return Err(Error::InvalidData("SBR: too many envelopes"));
            }
            let res = r.read(1) as u8;
            g.freq_res = [res; MAX_ENV];
            abs_lead = 0;
            abs_trail = 16;
            n_rel0 = g.num_env - 1;
            for v in rel0.iter_mut() {
                *v = 16 / g.num_env;
            }
            g.pointer = 0;
        }
        FIXVAR => {
            abs_lead = 0;
            abs_trail = r.read(2) as usize + 16;
            n_rel0 = 0;
            let n_rel1 = r.read(2) as usize;
            g.num_env = n_rel1 + 1;
            for v in rel1.iter_mut().take(n_rel1) {
                *v = 2 * r.read(2) as usize + 2;
            }
            g.pointer = r.read(pointer_bits(g.num_env)) as usize;
            for e in 0..g.num_env {
                g.freq_res[g.num_env - 1 - e] = r.read(1) as u8;
            }
        }
        VARFIX => {
            abs_lead = r.read(2) as usize;
            abs_trail = 16;
            n_rel0 = r.read(2) as usize;
            g.num_env = n_rel0 + 1;
            for v in rel0.iter_mut().take(n_rel0) {
                *v = 2 * r.read(2) as usize + 2;
            }
            g.pointer = r.read(pointer_bits(g.num_env)) as usize;
            for e in 0..g.num_env {
                g.freq_res[e] = r.read(1) as u8;
            }
        }
        _ => {
            abs_lead = r.read(2) as usize;
            abs_trail = r.read(2) as usize + 16;
            n_rel0 = r.read(2) as usize;
            let n_rel1 = r.read(2) as usize;
            g.num_env = n_rel0 + n_rel1 + 1;
            if g.num_env > MAX_ENV {
                return Err(Error::InvalidData("SBR: too many envelopes"));
            }
            for v in rel0.iter_mut().take(n_rel0) {
                *v = 2 * r.read(2) as usize + 2;
            }
            for v in rel1.iter_mut().take(n_rel1) {
                *v = 2 * r.read(2) as usize + 2;
            }
            g.pointer = r.read(pointer_bits(g.num_env)) as usize;
            for e in 0..g.num_env {
                g.freq_res[e] = r.read(1) as u8;
            }
        }
    }
    let l = g.num_env;
    if g.pointer > l + 1 {
        return Err(Error::InvalidData("SBR: bad pointer"));
    }
    // Envelope time borders.
    g.t_e = [0; MAX_ENV + 1];
    g.t_e[0] = abs_lead;
    g.t_e[l] = abs_trail;
    for i in 1..=n_rel0.min(l - 1) {
        g.t_e[i] = g.t_e[i - 1] + rel0[i - 1];
    }
    let mut i = l - 1;
    while i > n_rel0 {
        g.t_e[i] = g.t_e[i + 1].checked_sub(rel1[l - 1 - i]).ok_or(Error::InvalidData("SBR: bad borders"))?;
        i -= 1;
    }
    for i in 0..l {
        if g.t_e[i] >= g.t_e[i + 1] {
            return Err(Error::InvalidData("SBR: non-increasing borders"));
        }
    }
    if g.t_e[l] > 19 {
        return Err(Error::InvalidData("SBR: border out of range"));
    }
    // Transient envelope.
    g.l_a = match g.class {
        FIXFIX => -1,
        VARFIX => {
            if g.pointer > 1 {
                g.pointer as i32 - 1
            } else {
                -1
            }
        }
        _ => {
            if g.pointer > 0 {
                (l + 1 - g.pointer) as i32
            } else {
                -1
            }
        }
    };
    // Noise floor borders.
    g.num_noise = if l > 1 { 2 } else { 1 };
    g.t_q = [0; MAX_NOISE_ENV + 1];
    g.t_q[0] = g.t_e[0];
    g.t_q[g.num_noise] = g.t_e[l];
    if g.num_noise == 2 {
        let idx = match g.class {
            FIXFIX => l / 2,
            VARFIX => match g.pointer {
                0 => 1,
                1 => l - 1,
                p => p - 1,
            },
            _ => {
                if g.pointer > 1 {
                    l + 1 - g.pointer
                } else {
                    l - 1
                }
            }
        };
        if idx == 0 || idx >= l {
            return Err(Error::InvalidData("SBR: bad noise border"));
        }
        g.t_q[1] = g.t_e[idx];
    }
    // Amplitude resolution (forced to 1.5 dB for FIXFIX with one envelope).
    f.amp_res = if g.class == FIXFIX && l == 1 { false } else { h.amp_res };
    r.check()
}

fn pointer_bits(num_env: usize) -> u32 {
    // ceil(log2(num_env + 1))
    match num_env {
        0 | 1 => 1,
        2 | 3 => 2,
        _ => 3,
    }
}

fn parse_dtdf(r: &mut BitReader, f: &mut ChannelFrame) {
    for e in 0..f.grid.num_env {
        f.df_env[e] = r.read_bit();
    }
    for q in 0..f.grid.num_noise {
        f.df_noise[q] = r.read_bit();
    }
}

fn parse_invf(r: &mut BitReader, f: &mut ChannelFrame, ft: &FreqTables) {
    for i in 0..ft.n_q() {
        f.invf[i] = r.read(2) as u8;
    }
}

fn parse_envelope(r: &mut BitReader, f: &mut ChannelFrame, ft: &FreqTables, balance: bool) -> Result<(), Error> {
    let b = books();
    let (t_book, f_book, t_off, f_off, start_bits) = match (balance, f.amp_res) {
        (false, false) => (&b.env_1_5_t, &b.env_1_5_f, SBR_ENV_1_5DB_T_OFFSET, SBR_ENV_1_5DB_F_OFFSET, 7),
        (false, true) => (&b.env_3_0_t, &b.env_3_0_f, SBR_ENV_3_0DB_T_OFFSET, SBR_ENV_3_0DB_F_OFFSET, 6),
        (true, false) => (&b.bal_1_5_t, &b.bal_1_5_f, SBR_ENV_BAL_1_5DB_T_OFFSET, SBR_ENV_BAL_1_5DB_F_OFFSET, 6),
        (true, true) => (&b.bal_3_0_t, &b.bal_3_0_f, SBR_ENV_BAL_3_0DB_T_OFFSET, SBR_ENV_BAL_3_0DB_F_OFFSET, 5),
    };
    let shift = balance as u32;
    for e in 0..f.grid.num_env {
        let n = ft.table(f.grid.freq_res[e]).len() - 1;
        if !f.df_env[e] {
            f.env[e][0] = (r.read(start_bits) as i32) << shift;
            for k in 1..n {
                f.env[e][k] = huff(r, f_book, f_off)? << shift;
            }
        } else {
            for k in 0..n {
                f.env[e][k] = huff(r, t_book, t_off)? << shift;
            }
        }
    }
    r.check()
}

fn parse_noise(r: &mut BitReader, f: &mut ChannelFrame, ft: &FreqTables, balance: bool) -> Result<(), Error> {
    let b = books();
    let (t_book, f_book, t_off, f_off) = if balance {
        (&b.noise_bal_t, &b.bal_3_0_f, SBR_NOISE_BAL_3_0DB_T_OFFSET, SBR_ENV_BAL_3_0DB_F_OFFSET)
    } else {
        (&b.noise_t, &b.env_3_0_f, SBR_NOISE_3_0DB_T_OFFSET, SBR_ENV_3_0DB_F_OFFSET)
    };
    let shift = balance as u32;
    let n = ft.n_q();
    for q in 0..f.grid.num_noise {
        if !f.df_noise[q] {
            f.noise[q][0] = (r.read(5) as i32) << shift;
            for k in 1..n {
                f.noise[q][k] = huff(r, f_book, f_off)? << shift;
            }
        } else {
            for k in 0..n {
                f.noise[q][k] = huff(r, t_book, t_off)? << shift;
            }
        }
    }
    r.check()
}

fn parse_harmonics(r: &mut BitReader, f: &mut ChannelFrame, ft: &FreqTables) {
    f.add_harmonic_flag = r.read_bit();
    if f.add_harmonic_flag {
        for i in 0..ft.n_high() {
            f.add_harmonic[i] = r.read_bit();
        }
    }
}

/// Delta decoding (time and frequency direction) into absolute values.
fn delta_decode(ch: &mut SbrChannel, ft: &FreqTables) {
    let f = &ch.frame;
    let n_high = ft.n_high();
    for e in 0..f.grid.num_env {
        let res = f.grid.freq_res[e];
        let table = ft.table(res);
        let n = table.len() - 1;
        let mut vals = [0i32; MAX_BANDS];
        if !f.df_env[e] {
            let mut acc = 0;
            for k in 0..n {
                acc += f.env[e][k];
                vals[k] = acc;
            }
        } else {
            for k in 0..n {
                let i = if res != 0 { k } else { ft.f_high.iter().position(|&b| b == table[k]).unwrap_or(0) };
                vals[k] = ch.prev_env[i] + f.env[e][k];
            }
        }
        for v in vals.iter_mut().take(n) {
            *v = (*v).clamp(-64, 160);
        }
        ch.env_abs[e] = vals;
        // Remember in the high resolution domain.
        if res != 0 {
            ch.prev_env[..n_high].copy_from_slice(&vals[..n_high]);
        } else {
            for k in 0..n {
                for i in 0..n_high {
                    if ft.f_high[i] >= table[k] && ft.f_high[i] < table[k + 1] {
                        ch.prev_env[i] = vals[k];
                    }
                }
            }
        }
    }
    let nq = ft.n_q();
    for q in 0..f.grid.num_noise {
        let mut vals = [0i32; 5];
        if !f.df_noise[q] {
            let mut acc = 0;
            for k in 0..nq {
                acc += f.noise[q][k];
                vals[k] = acc;
            }
        } else {
            for k in 0..nq {
                vals[k] = ch.prev_noise[k] + f.noise[q][k];
            }
        }
        for v in vals.iter_mut().take(nq) {
            *v = (*v).clamp(-64, 128);
        }
        ch.noise_abs[q] = vals;
        ch.prev_noise = vals;
    }
}

fn dequant(ch: &mut SbrChannel, ft: &FreqTables) {
    let f = &ch.frame;
    let a = if f.amp_res { 1.0 } else { 0.5 };
    for e in 0..f.grid.num_env {
        let n = ft.table(f.grid.freq_res[e]).len() - 1;
        for k in 0..n {
            ch.e_orig[e][k] = 64.0 * 2f32.powf(ch.env_abs[e][k] as f32 * a);
        }
    }
    for q in 0..f.grid.num_noise {
        for k in 0..ft.n_q() {
            ch.q_orig[q][k] = 2f32.powf(6.0 - ch.noise_abs[q][k] as f32);
        }
    }
}

fn dequant_coupled(l: &mut SbrChannel, r: &mut SbrChannel, ft: &FreqTables) {
    let f = &l.frame;
    let a = if f.amp_res { 1.0 } else { 0.5 };
    for e in 0..f.grid.num_env {
        let n = ft.table(f.grid.freq_res[e]).len() - 1;
        for k in 0..n {
            let level = 64.0 * 2f32.powf(l.env_abs[e][k] as f32 * a + 1.0);
            let pan = r.env_abs[e][k] as f32 * a - 12.0;
            l.e_orig[e][k] = level / (1.0 + 2f32.powf(-pan));
            r.e_orig[e][k] = level / (1.0 + 2f32.powf(pan));
        }
    }
    for q in 0..f.grid.num_noise {
        for k in 0..ft.n_q() {
            let level = 2f32.powf(7.0 - l.noise_abs[q][k] as f32);
            let pan = r.noise_abs[q][k] as f32 - 12.0;
            l.q_orig[q][k] = level / (1.0 + 2f32.powf(-pan));
            r.q_orig[q][k] = level / (1.0 + 2f32.powf(pan));
        }
    }
}
