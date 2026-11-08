//! OBU layer, sequence header and the frame header of intra frames (AV1
//! specification sections 5.3 to 5.9).

use super::bits::{leb128, BitReader};
use super::tables::{
    REMAP_LR_TYPE, SEGMENTATION_FEATURE_BITS, SEGMENTATION_FEATURE_MAX, SEGMENTATION_FEATURE_SIGNED,
};
use crate::Error;

pub(crate) const OBU_SEQUENCE_HEADER: u8 = 1;
pub(crate) const OBU_FRAME_HEADER: u8 = 3;
pub(crate) const OBU_TILE_GROUP: u8 = 4;
pub(crate) const OBU_FRAME: u8 = 6;
pub(crate) const OBU_REDUNDANT_FRAME_HEADER: u8 = 7;

pub(crate) const KEY_FRAME: u32 = 0;
pub(crate) const INTRA_ONLY_FRAME: u32 = 2;

pub(crate) const RESTORE_NONE: u8 = 0;
pub(crate) const RESTORE_WIENER: u8 = 1;
pub(crate) const RESTORE_SGRPROJ: u8 = 2;
pub(crate) const RESTORE_SWITCHABLE: u8 = 3;

pub(crate) const SEG_LVL_ALT_Q: usize = 0;
pub(crate) const SEG_LVL_ALT_LF_Y_V: usize = 1;
pub(crate) const SEG_LVL_REF_FRAME: usize = 5;
pub(crate) const SEG_LVL_SKIP: usize = 6;

/// One OBU: type, extension ids and payload.
pub(crate) struct Obu<'a> {
    pub kind: u8,
    pub temporal_id: u8,
    pub spatial_id: u8,
    pub has_extension: bool,
    pub payload: &'a [u8],
}

/// Splits a low overhead bitstream (OBUs with size fields, as in AVIF items
/// and av1C) into OBUs.
pub(crate) fn split_obus(mut data: &[u8]) -> Result<Vec<Obu<'_>>, Error> {
    let mut out = Vec::new();
    while !data.is_empty() {
        let h = data[0];
        if h & 0x80 != 0 {
            return Err(Error::InvalidData("av1: forbidden bit set"));
        }
        let kind = (h >> 3) & 15;
        let ext = h & 4 != 0;
        let has_size = h & 2 != 0;
        let mut pos = 1;
        let (mut tid, mut sid) = (0, 0);
        if ext {
            let e = *data
                .get(1)
                .ok_or(Error::InvalidData("av1: truncated OBU"))?;
            tid = e >> 5;
            sid = (e >> 3) & 3;
            pos = 2;
        }
        let size = if has_size {
            let (v, n) =
                leb128(&data[pos..]).ok_or(Error::InvalidData("av1: truncated OBU size"))?;
            pos += n;
            usize::try_from(v).map_err(|_| Error::InvalidData("av1: bad OBU size"))?
        } else {
            data.len() - pos
        };
        let end = pos
            .checked_add(size)
            .filter(|&e| e <= data.len())
            .ok_or(Error::InvalidData("av1: truncated OBU"))?;
        out.push(Obu {
            kind,
            temporal_id: tid,
            spatial_id: sid,
            has_extension: ext,
            payload: &data[pos..end],
        });
        data = &data[end..];
    }
    Ok(out)
}

#[derive(Debug, Clone, Default)]
pub(crate) struct ColorConfig {
    pub bit_depth: u32,
    pub mono: bool,
    pub color_primaries: u32,
    pub transfer: u32,
    pub matrix: u32,
    pub full_range: bool,
    pub ssx: u32,
    pub ssy: u32,
    pub chroma_sample_position: u32,
    pub separate_uv_delta_q: bool,
}

impl ColorConfig {
    pub fn num_planes(&self) -> usize {
        if self.mono {
            1
        } else {
            3
        }
    }
}

#[derive(Debug, Clone, Default)]
pub(crate) struct SequenceHeader {
    pub profile: u32,
    pub reduced_still: bool,
    pub decoder_model_info_present: bool,
    pub equal_picture_interval: bool,
    pub buffer_removal_time_length: u32,
    pub frame_presentation_time_length: u32,
    pub operating_point_idc: Vec<u32>,
    pub decoder_model_present_for_op: Vec<bool>,
    pub frame_width_bits: u32,
    pub frame_height_bits: u32,
    pub max_width: u32,
    pub max_height: u32,
    pub frame_id_numbers_present: bool,
    pub delta_frame_id_length: u32,
    pub additional_frame_id_length: u32,
    pub sb128: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_order_hint: bool,
    pub order_hint_bits: u32,
    /// 0, 1 or 2 (= SELECT_SCREEN_CONTENT_TOOLS).
    pub force_screen_content_tools: u32,
    pub force_integer_mv: u32,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub color: ColorConfig,
    pub film_grain_params_present: bool,
}

pub(crate) fn parse_sequence_header(data: &[u8]) -> Result<SequenceHeader, Error> {
    let mut r = BitReader::new(data);
    let mut s = SequenceHeader {
        profile: r.f(3)?,
        ..Default::default()
    };
    if s.profile > 2 {
        return Err(Error::Unsupported("av1: unknown profile"));
    }
    let _still_picture = r.flag()?;
    s.reduced_still = r.flag()?;
    if s.reduced_still {
        s.operating_point_idc = vec![0];
        s.decoder_model_present_for_op = vec![false];
        let _seq_level_idx = r.f(5)?;
    } else {
        let timing_info_present = r.flag()?;
        if timing_info_present {
            let _num_units_in_display_tick = r.f(32)?;
            let _time_scale = r.f(32)?;
            s.equal_picture_interval = r.flag()?;
            if s.equal_picture_interval {
                let _ = r.uvlc()?;
            }
            s.decoder_model_info_present = r.flag()?;
        }
        let mut buffer_delay_length = 0;
        if s.decoder_model_info_present {
            buffer_delay_length = r.f(5)? + 1;
            let _num_units_in_decoding_tick = r.f(32)?;
            s.buffer_removal_time_length = r.f(5)? + 1;
            s.frame_presentation_time_length = r.f(5)? + 1;
        }
        let initial_display_delay_present = r.flag()?;
        let count = r.f(5)? + 1;
        for _ in 0..count {
            s.operating_point_idc.push(r.f(12)?);
            let level = r.f(5)?;
            if level > 7 {
                let _tier = r.flag()?;
            }
            let mut present = false;
            if s.decoder_model_info_present {
                present = r.flag()?;
                if present {
                    let _decoder_buffer_delay = r.f(buffer_delay_length)?;
                    let _encoder_buffer_delay = r.f(buffer_delay_length)?;
                    let _low_delay_mode = r.flag()?;
                }
            }
            s.decoder_model_present_for_op.push(present);
            if initial_display_delay_present && r.flag()? {
                let _ = r.f(4)?;
            }
        }
    }
    s.frame_width_bits = r.f(4)? + 1;
    s.frame_height_bits = r.f(4)? + 1;
    s.max_width = r.f(s.frame_width_bits)? + 1;
    s.max_height = r.f(s.frame_height_bits)? + 1;
    if !s.reduced_still {
        s.frame_id_numbers_present = r.flag()?;
    }
    if s.frame_id_numbers_present {
        s.delta_frame_id_length = r.f(4)? + 2;
        s.additional_frame_id_length = r.f(3)? + 1;
    }
    s.sb128 = r.flag()?;
    s.enable_filter_intra = r.flag()?;
    s.enable_intra_edge_filter = r.flag()?;
    if s.reduced_still {
        s.force_screen_content_tools = 2;
        s.force_integer_mv = 2;
    } else {
        let _interintra = r.flag()?;
        let _masked = r.flag()?;
        let _warped = r.flag()?;
        let _dual_filter = r.flag()?;
        s.enable_order_hint = r.flag()?;
        if s.enable_order_hint {
            let _jnt_comp = r.flag()?;
            let _ref_frame_mvs = r.flag()?;
        }
        s.force_screen_content_tools = if r.flag()? { 2 } else { r.f(1)? };
        s.force_integer_mv = if s.force_screen_content_tools > 0 {
            if r.flag()? {
                2
            } else {
                r.f(1)?
            }
        } else {
            2
        };
        if s.enable_order_hint {
            s.order_hint_bits = r.f(3)? + 1;
        }
    }
    s.enable_superres = r.flag()?;
    s.enable_cdef = r.flag()?;
    s.enable_restoration = r.flag()?;
    s.color = parse_color_config(&mut r, s.profile)?;
    s.film_grain_params_present = r.flag()?;
    Ok(s)
}

fn parse_color_config(r: &mut BitReader, profile: u32) -> Result<ColorConfig, Error> {
    let mut c = ColorConfig::default();
    let high = r.flag()?;
    c.bit_depth = if profile == 2 && high {
        if r.flag()? {
            12
        } else {
            10
        }
    } else if high {
        10
    } else {
        8
    };
    c.mono = if profile == 1 { false } else { r.flag()? };
    if r.flag()? {
        c.color_primaries = r.f(8)?;
        c.transfer = r.f(8)?;
        c.matrix = r.f(8)?;
    } else {
        c.color_primaries = 2;
        c.transfer = 2;
        c.matrix = 2;
    }
    if c.mono {
        c.full_range = r.flag()?;
        c.ssx = 1;
        c.ssy = 1;
        return Ok(c);
    } else if c.color_primaries == 1 && c.transfer == 13 && c.matrix == 0 {
        c.full_range = true;
    } else {
        c.full_range = r.flag()?;
        match profile {
            0 => {
                c.ssx = 1;
                c.ssy = 1;
            }
            1 => {}
            _ => {
                if c.bit_depth == 12 {
                    c.ssx = r.f(1)?;
                    c.ssy = if c.ssx == 1 { r.f(1)? } else { 0 };
                } else {
                    c.ssx = 1;
                }
            }
        }
        if c.ssx == 1 && c.ssy == 1 {
            c.chroma_sample_position = r.f(2)?;
        }
    }
    c.separate_uv_delta_q = r.flag()?;
    Ok(c)
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FilmGrain {
    pub apply: bool,
    pub seed: u32,
    pub y_points: Vec<(u8, u8)>,
    pub chroma_scaling_from_luma: bool,
    pub cb_points: Vec<(u8, u8)>,
    pub cr_points: Vec<(u8, u8)>,
    pub scaling_shift: u32,
    pub ar_coeff_lag: u32,
    pub ar_coeffs_y: Vec<i32>,
    pub ar_coeffs_cb: Vec<i32>,
    pub ar_coeffs_cr: Vec<i32>,
    pub ar_coeff_shift: u32,
    pub grain_scale_shift: u32,
    pub cb_mult: i32,
    pub cb_luma_mult: i32,
    pub cb_offset: i32,
    pub cr_mult: i32,
    pub cr_luma_mult: i32,
    pub cr_offset: i32,
    pub overlap: bool,
    pub clip_to_restricted_range: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct TileInfo {
    pub cols: usize,
    pub rows: usize,
    pub cols_log2: u32,
    pub rows_log2: u32,
    /// MI column / row starts, with the end appended.
    pub col_starts: Vec<usize>,
    pub row_starts: Vec<usize>,
    pub size_bytes: u32,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct FrameHeader {
    pub frame_type: u32,
    pub show_frame: bool,
    pub showable_frame: bool,
    pub disable_cdf_update: bool,
    pub allow_screen_content_tools: bool,
    pub frame_width: usize,
    pub frame_height: usize,
    pub upscaled_width: usize,
    pub use_superres: bool,
    pub superres_denom: usize,
    pub mi_cols: usize,
    pub mi_rows: usize,
    pub allow_intrabc: bool,
    pub tile: TileInfo,
    pub base_q_idx: i32,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
    pub qm_y: u32,
    pub qm_u: u32,
    pub qm_v: u32,
    pub segmentation_enabled: bool,
    pub feature_enabled: [[bool; 8]; 8],
    pub feature_data: [[i32; 8]; 8],
    pub seg_id_pre_skip: bool,
    pub last_active_seg_id: u32,
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub delta_lf_present: bool,
    pub delta_lf_res: u32,
    pub delta_lf_multi: bool,
    pub coded_lossless: bool,
    pub all_lossless: bool,
    pub lossless: [bool; 8],
    pub seg_qm_level: [[u32; 8]; 3],
    pub lf_level: [i32; 4],
    pub lf_sharpness: i32,
    pub lf_delta_enabled: bool,
    pub lf_ref_deltas: [i32; 8],
    pub lf_mode_deltas: [i32; 2],
    pub cdef_damping: i32,
    pub cdef_bits: u32,
    pub cdef_y_pri: [i32; 8],
    pub cdef_y_sec: [i32; 8],
    pub cdef_uv_pri: [i32; 8],
    pub cdef_uv_sec: [i32; 8],
    pub lr_type: [u8; 3],
    pub lr_unit_size: [usize; 3],
    pub uses_lr: bool,
    /// TX_MODE_SELECT (otherwise largest, or only 4x4 when lossless).
    pub tx_mode_select: bool,
    pub reduced_tx_set: bool,
    pub film_grain: FilmGrain,
}

impl FrameHeader {
    pub fn seg_feature_active_idx(&self, idx: usize, feature: usize) -> bool {
        self.segmentation_enabled && self.feature_enabled[idx][feature]
    }

    /// get_qindex( ignoreDeltaQ, segmentId ) with the current q index of
    /// the superblock.
    pub fn qindex(&self, ignore_delta_q: bool, segment_id: usize, current_q: i32) -> i32 {
        if self.seg_feature_active_idx(segment_id, SEG_LVL_ALT_Q) {
            let data = self.feature_data[segment_id][SEG_LVL_ALT_Q];
            let mut q = self.base_q_idx + data;
            if !ignore_delta_q && self.delta_q_present {
                q = current_q + data;
            }
            q.clamp(0, 255)
        } else if !ignore_delta_q && self.delta_q_present {
            current_q
        } else {
            self.base_q_idx
        }
    }
}

fn tile_log2(blk: usize, target: usize) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

/// Parses the uncompressed header of a frame. Returns the header and the
/// length of the header in bytes (after byte alignment) for OBU_FRAME.
pub(crate) fn parse_frame_header(
    data: &[u8],
    seq: &SequenceHeader,
    temporal_id: u8,
    spatial_id: u8,
) -> Result<(FrameHeader, usize), Error> {
    let mut r = BitReader::new(data);
    let mut h = FrameHeader::default();
    let id_len = seq.additional_frame_id_length + seq.delta_frame_id_length + 1;
    let mut error_resilient = true;
    if seq.reduced_still {
        h.frame_type = KEY_FRAME;
        h.show_frame = true;
    } else {
        if r.flag()? {
            return Err(Error::Unsupported("av1: shown existing frame"));
        }
        h.frame_type = r.f(2)?;
        if h.frame_type != KEY_FRAME && h.frame_type != INTRA_ONLY_FRAME {
            return Err(Error::Unsupported("av1: inter frame"));
        }
        h.show_frame = r.flag()?;
        if h.show_frame && seq.decoder_model_info_present && !seq.equal_picture_interval {
            r.f(seq.frame_presentation_time_length)?;
        }
        if !h.show_frame {
            h.showable_frame = r.flag()?;
        }
        if !(h.frame_type == KEY_FRAME && h.show_frame) {
            error_resilient = r.flag()?;
        }
    }
    h.disable_cdf_update = r.flag()?;
    h.allow_screen_content_tools = if seq.force_screen_content_tools == 2 {
        r.flag()?
    } else {
        seq.force_screen_content_tools == 1
    };
    if h.allow_screen_content_tools && seq.force_integer_mv == 2 {
        let _force_integer_mv = r.flag()?;
    }
    if seq.frame_id_numbers_present {
        let _current_frame_id = r.f(id_len)?;
    }
    let frame_size_override = if seq.reduced_still { false } else { r.flag()? };
    let _order_hint = r.f(seq.order_hint_bits)?;
    // primary_ref_frame is PRIMARY_REF_NONE for intra frames.
    if seq.decoder_model_info_present {
        let present = r.flag()?;
        if present {
            for (op, &idc) in seq.operating_point_idc.iter().enumerate() {
                if seq.decoder_model_present_for_op[op] {
                    let in_t = (idc >> temporal_id) & 1;
                    let in_s = (idc >> (spatial_id + 8)) & 1;
                    if idc == 0 || (in_t == 1 && in_s == 1) {
                        r.f(seq.buffer_removal_time_length)?;
                    }
                }
            }
        }
    }
    let refresh = if h.frame_type == KEY_FRAME && h.show_frame {
        0xff
    } else {
        r.f(8)?
    };
    if h.frame_type == INTRA_ONLY_FRAME && refresh == 0xff {
        return Err(Error::InvalidData(
            "av1: intra-only frame refreshes all frames",
        ));
    }
    if refresh != 0xff && error_resilient && seq.enable_order_hint {
        for _ in 0..8 {
            r.f(seq.order_hint_bits)?;
        }
    }
    // frame_size()
    if frame_size_override {
        h.frame_width = r.f(seq.frame_width_bits)? as usize + 1;
        h.frame_height = r.f(seq.frame_height_bits)? as usize + 1;
    } else {
        h.frame_width = seq.max_width as usize;
        h.frame_height = seq.max_height as usize;
    }
    // superres_params()
    h.use_superres = seq.enable_superres && r.flag()?;
    h.superres_denom = if h.use_superres {
        r.f(3)? as usize + 9
    } else {
        8
    };
    h.upscaled_width = h.frame_width;
    h.frame_width = (h.upscaled_width * 8 + h.superres_denom / 2) / h.superres_denom;
    h.mi_cols = 2 * ((h.frame_width + 7) >> 3);
    h.mi_rows = 2 * ((h.frame_height + 7) >> 3);
    // render_size()
    if r.flag()? {
        r.f(16)?;
        r.f(16)?;
    }
    if h.allow_screen_content_tools && h.upscaled_width == h.frame_width {
        h.allow_intrabc = r.flag()?;
    }
    if !seq.reduced_still && !h.disable_cdf_update {
        let _disable_frame_end_update_cdf = r.flag()?;
    }
    // setup_past_independence()
    h.lf_delta_enabled = true;
    h.lf_ref_deltas = [1, 0, 0, 0, -1, 0, -1, -1];
    h.lf_mode_deltas = [0, 0];
    parse_tile_info(&mut r, seq, &mut h)?;
    // quantization_params()
    let planes = seq.color.num_planes();
    h.base_q_idx = r.f(8)? as i32;
    h.delta_q_y_dc = read_delta_q(&mut r)?;
    if planes > 1 {
        let diff_uv = seq.color.separate_uv_delta_q && r.flag()?;
        h.delta_q_u_dc = read_delta_q(&mut r)?;
        h.delta_q_u_ac = read_delta_q(&mut r)?;
        if diff_uv {
            h.delta_q_v_dc = read_delta_q(&mut r)?;
            h.delta_q_v_ac = read_delta_q(&mut r)?;
        } else {
            h.delta_q_v_dc = h.delta_q_u_dc;
            h.delta_q_v_ac = h.delta_q_u_ac;
        }
    }
    h.using_qmatrix = r.flag()?;
    if h.using_qmatrix {
        h.qm_y = r.f(4)?;
        h.qm_u = r.f(4)?;
        h.qm_v = if seq.color.separate_uv_delta_q {
            r.f(4)?
        } else {
            h.qm_u
        };
    }
    // segmentation_params()
    h.segmentation_enabled = r.flag()?;
    if h.segmentation_enabled {
        // primary_ref_frame is none: the map and the data are updated.
        for i in 0..8 {
            for j in 0..8 {
                let enabled = r.flag()?;
                h.feature_enabled[i][j] = enabled;
                let mut v = 0;
                if enabled {
                    let bits = SEGMENTATION_FEATURE_BITS[j] as u32;
                    let limit = SEGMENTATION_FEATURE_MAX[j] as i32;
                    if SEGMENTATION_FEATURE_SIGNED[j] == 1 {
                        v = r.su(1 + bits)?.clamp(-limit, limit);
                    } else {
                        v = (r.f(bits)? as i32).clamp(0, limit);
                    }
                }
                h.feature_data[i][j] = v;
            }
        }
    }
    for i in 0..8 {
        for j in 0..8 {
            if h.feature_enabled[i][j] {
                h.last_active_seg_id = i as u32;
                if j >= SEG_LVL_REF_FRAME {
                    h.seg_id_pre_skip = true;
                }
            }
        }
    }
    // delta_q_params(), delta_lf_params()
    if h.base_q_idx > 0 {
        h.delta_q_present = r.flag()?;
    }
    if h.delta_q_present {
        h.delta_q_res = r.f(2)?;
        if !h.allow_intrabc {
            h.delta_lf_present = r.flag()?;
        }
        if h.delta_lf_present {
            h.delta_lf_res = r.f(2)?;
            h.delta_lf_multi = r.flag()?;
        }
    }
    h.coded_lossless = true;
    for seg in 0..8 {
        let q = h.qindex(true, seg, h.base_q_idx);
        let lossless = q == 0
            && h.delta_q_y_dc == 0
            && h.delta_q_u_ac == 0
            && h.delta_q_u_dc == 0
            && h.delta_q_v_ac == 0
            && h.delta_q_v_dc == 0;
        h.lossless[seg] = lossless;
        if !lossless {
            h.coded_lossless = false;
        }
        if h.using_qmatrix {
            if lossless {
                h.seg_qm_level[0][seg] = 15;
                h.seg_qm_level[1][seg] = 15;
                h.seg_qm_level[2][seg] = 15;
            } else {
                h.seg_qm_level[0][seg] = h.qm_y;
                h.seg_qm_level[1][seg] = h.qm_u;
                h.seg_qm_level[2][seg] = h.qm_v;
            }
        }
    }
    h.all_lossless = h.coded_lossless && h.frame_width == h.upscaled_width;
    // loop_filter_params()
    if !(h.coded_lossless || h.allow_intrabc) {
        h.lf_level[0] = r.f(6)? as i32;
        h.lf_level[1] = r.f(6)? as i32;
        if planes > 1 && (h.lf_level[0] != 0 || h.lf_level[1] != 0) {
            h.lf_level[2] = r.f(6)? as i32;
            h.lf_level[3] = r.f(6)? as i32;
        }
        h.lf_sharpness = r.f(3)? as i32;
        h.lf_delta_enabled = r.flag()?;
        if h.lf_delta_enabled && r.flag()? {
            for i in 0..8 {
                if r.flag()? {
                    h.lf_ref_deltas[i] = r.su(7)?;
                }
            }
            for i in 0..2 {
                if r.flag()? {
                    h.lf_mode_deltas[i] = r.su(7)?;
                }
            }
        }
    }
    // cdef_params()
    if h.coded_lossless || h.allow_intrabc || !seq.enable_cdef {
        h.cdef_damping = 3;
    } else {
        h.cdef_damping = r.f(2)? as i32 + 3;
        h.cdef_bits = r.f(2)?;
        for i in 0..(1 << h.cdef_bits) {
            h.cdef_y_pri[i] = r.f(4)? as i32;
            h.cdef_y_sec[i] = r.f(2)? as i32;
            if h.cdef_y_sec[i] == 3 {
                h.cdef_y_sec[i] += 1;
            }
            if planes > 1 {
                h.cdef_uv_pri[i] = r.f(4)? as i32;
                h.cdef_uv_sec[i] = r.f(2)? as i32;
                if h.cdef_uv_sec[i] == 3 {
                    h.cdef_uv_sec[i] += 1;
                }
            }
        }
    }
    // lr_params()
    if !(h.all_lossless || h.allow_intrabc || !seq.enable_restoration) {
        let mut uses_chroma_lr = false;
        for i in 0..planes {
            let t = REMAP_LR_TYPE[r.f(2)? as usize];
            h.lr_type[i] = t;
            if t != RESTORE_NONE {
                h.uses_lr = true;
                if i > 0 {
                    uses_chroma_lr = true;
                }
            }
        }
        if h.uses_lr {
            let mut shift;
            if seq.sb128 {
                shift = r.f(1)? + 1;
            } else {
                shift = r.f(1)?;
                if shift == 1 {
                    shift += r.f(1)?;
                }
            }
            h.lr_unit_size[0] = 256 >> (2 - shift);
            let uv_shift = if seq.color.ssx == 1 && seq.color.ssy == 1 && uses_chroma_lr {
                r.f(1)?
            } else {
                0
            };
            h.lr_unit_size[1] = h.lr_unit_size[0] >> uv_shift;
            h.lr_unit_size[2] = h.lr_unit_size[0] >> uv_shift;
        }
    }
    // read_tx_mode()
    if !h.coded_lossless {
        h.tx_mode_select = r.flag()?;
    }
    // frame_reference_mode(), skip_mode_params(): nothing for intra frames;
    // allow_warped_motion = 0.
    h.reduced_tx_set = r.flag()?;
    // global_motion_params(): nothing for intra frames.
    if seq.film_grain_params_present && (h.show_frame || h.showable_frame) {
        h.film_grain = parse_film_grain(&mut r, seq)?;
    }
    r.byte_align();
    Ok((h, r.byte_pos()))
}

fn read_delta_q(r: &mut BitReader) -> Result<i32, Error> {
    if r.flag()? {
        r.su(7)
    } else {
        Ok(0)
    }
}

fn parse_tile_info(
    r: &mut BitReader,
    seq: &SequenceHeader,
    h: &mut FrameHeader,
) -> Result<(), Error> {
    let t = &mut h.tile;
    let (sb_cols, sb_rows, sb_shift) = if seq.sb128 {
        ((h.mi_cols + 31) >> 5, (h.mi_rows + 31) >> 5, 5)
    } else {
        ((h.mi_cols + 15) >> 4, (h.mi_rows + 15) >> 4, 4)
    };
    let sb_size = sb_shift + 2;
    let max_tile_width_sb = 4096 >> sb_size;
    let mut max_tile_area_sb = (4096 * 2304) >> (2 * sb_size);
    let min_log2_cols = tile_log2(max_tile_width_sb, sb_cols);
    let max_log2_cols = tile_log2(1, sb_cols.min(64));
    let max_log2_rows = tile_log2(1, sb_rows.min(64));
    let min_log2_tiles = min_log2_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
    t.col_starts.clear();
    t.row_starts.clear();
    if r.flag()? {
        t.cols_log2 = min_log2_cols;
        while t.cols_log2 < max_log2_cols {
            if r.flag()? {
                t.cols_log2 += 1;
            } else {
                break;
            }
        }
        let w = (sb_cols + (1 << t.cols_log2) - 1) >> t.cols_log2;
        let mut start = 0;
        while start < sb_cols {
            t.col_starts.push(start << sb_shift);
            start += w;
        }
        t.cols = t.col_starts.len();
        t.col_starts.push(h.mi_cols);
        let min_log2_rows = min_log2_tiles.saturating_sub(t.cols_log2);
        t.rows_log2 = min_log2_rows;
        while t.rows_log2 < max_log2_rows {
            if r.flag()? {
                t.rows_log2 += 1;
            } else {
                break;
            }
        }
        let hh = (sb_rows + (1 << t.rows_log2) - 1) >> t.rows_log2;
        let mut start = 0;
        while start < sb_rows {
            t.row_starts.push(start << sb_shift);
            start += hh;
        }
        t.rows = t.row_starts.len();
        t.row_starts.push(h.mi_rows);
    } else {
        let mut widest = 0;
        let mut start = 0;
        while start < sb_cols {
            t.col_starts.push(start << sb_shift);
            let max_w = (sb_cols - start).min(max_tile_width_sb);
            let size = r.ns(max_w as u32)? as usize + 1;
            widest = widest.max(size);
            start += size;
            if t.col_starts.len() > 64 {
                return Err(Error::InvalidData("av1: too many tile columns"));
            }
        }
        t.cols = t.col_starts.len();
        t.col_starts.push(h.mi_cols);
        t.cols_log2 = tile_log2(1, t.cols);
        if min_log2_tiles > 0 {
            max_tile_area_sb = (sb_rows * sb_cols) >> (min_log2_tiles + 1);
        } else {
            max_tile_area_sb = sb_rows * sb_cols;
        }
        let max_h = (max_tile_area_sb / widest).max(1);
        let mut start = 0;
        while start < sb_rows {
            t.row_starts.push(start << sb_shift);
            let mh = (sb_rows - start).min(max_h);
            let size = r.ns(mh as u32)? as usize + 1;
            start += size;
            if t.row_starts.len() > 64 {
                return Err(Error::InvalidData("av1: too many tile rows"));
            }
        }
        t.rows = t.row_starts.len();
        t.row_starts.push(h.mi_rows);
        t.rows_log2 = tile_log2(1, t.rows);
    }
    if t.cols_log2 > 0 || t.rows_log2 > 0 {
        let _context_update_tile_id = r.f(t.rows_log2 + t.cols_log2)?;
        t.size_bytes = r.f(2)? + 1;
    }
    Ok(())
}

fn parse_film_grain(r: &mut BitReader, seq: &SequenceHeader) -> Result<FilmGrain, Error> {
    let mut g = FilmGrain {
        apply: r.flag()?,
        ..Default::default()
    };
    if !g.apply {
        return Ok(FilmGrain::default());
    }
    g.seed = r.f(16)?;
    // update_grain = 1 for intra frames.
    let n = r.f(4)?;
    for _ in 0..n {
        g.y_points.push((r.f(8)? as u8, r.f(8)? as u8));
    }
    let c = &seq.color;
    g.chroma_scaling_from_luma = if c.mono { false } else { r.flag()? };
    if !(c.mono || g.chroma_scaling_from_luma || (c.ssx == 1 && c.ssy == 1 && n == 0)) {
        let ncb = r.f(4)?;
        for _ in 0..ncb {
            g.cb_points.push((r.f(8)? as u8, r.f(8)? as u8));
        }
        let ncr = r.f(4)?;
        for _ in 0..ncr {
            g.cr_points.push((r.f(8)? as u8, r.f(8)? as u8));
        }
    }
    g.scaling_shift = r.f(2)? + 8;
    g.ar_coeff_lag = r.f(2)?;
    let num_pos_luma = 2 * g.ar_coeff_lag * (g.ar_coeff_lag + 1);
    let num_pos_chroma = if !g.y_points.is_empty() {
        for _ in 0..num_pos_luma {
            g.ar_coeffs_y.push(r.f(8)? as i32 - 128);
        }
        num_pos_luma + 1
    } else {
        num_pos_luma
    };
    if g.chroma_scaling_from_luma || !g.cb_points.is_empty() {
        for _ in 0..num_pos_chroma {
            g.ar_coeffs_cb.push(r.f(8)? as i32 - 128);
        }
    }
    if g.chroma_scaling_from_luma || !g.cr_points.is_empty() {
        for _ in 0..num_pos_chroma {
            g.ar_coeffs_cr.push(r.f(8)? as i32 - 128);
        }
    }
    g.ar_coeff_shift = r.f(2)? + 6;
    g.grain_scale_shift = r.f(2)?;
    if !g.cb_points.is_empty() {
        g.cb_mult = r.f(8)? as i32;
        g.cb_luma_mult = r.f(8)? as i32;
        g.cb_offset = r.f(9)? as i32;
    }
    if !g.cr_points.is_empty() {
        g.cr_mult = r.f(8)? as i32;
        g.cr_luma_mult = r.f(8)? as i32;
        g.cr_offset = r.f(9)? as i32;
    }
    g.overlap = r.flag()?;
    g.clip_to_restricted_range = r.flag()?;
    Ok(g)
}

/// The data of each tile of a frame, collected from its tile groups.
pub(crate) fn parse_tile_group<'a>(
    data: &'a [u8],
    h: &FrameHeader,
    tiles: &mut Vec<Option<&'a [u8]>>,
) -> Result<(), Error> {
    let num = h.tile.cols * h.tile.rows;
    if tiles.len() != num {
        tiles.clear();
        tiles.resize(num, None);
    }
    let mut r = BitReader::new(data);
    let mut start = 0;
    let mut end = num - 1;
    if num > 1 && r.flag()? {
        let bits = h.tile.cols_log2 + h.tile.rows_log2;
        start = r.f(bits)? as usize;
        end = r.f(bits)? as usize;
    }
    r.byte_align();
    let mut pos = r.byte_pos();
    if start > end || end >= num {
        return Err(Error::InvalidData("av1: bad tile group"));
    }
    for t in start..=end {
        let size = if t == end {
            data.len().checked_sub(pos)
        } else {
            let n = h.tile.size_bytes as usize;
            let b = data
                .get(pos..pos + n)
                .ok_or(Error::InvalidData("av1: truncated tile size"))?;
            let mut v = 0usize;
            for (i, &x) in b.iter().enumerate() {
                v |= (x as usize) << (8 * i);
            }
            pos += n;
            Some(v + 1)
        }
        .ok_or(Error::InvalidData("av1: truncated tile"))?;
        let d = data
            .get(pos..pos + size)
            .ok_or(Error::InvalidData("av1: truncated tile"))?;
        tiles[t] = Some(d);
        pos += size;
    }
    Ok(())
}
