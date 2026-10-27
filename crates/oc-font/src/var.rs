//! Font variations common code: `fvar` axes, `avar` mapping, coordinate
//! normalization, region scalars, `ItemVariationStore` and
//! `DeltaSetIndexMap` (used by `HVAR` and `GDEF`/`GPOS`).

use crate::reader::{u16_at, u32_at, u8_at, Reader};
use crate::sfnt::TableRange;

/// A variation axis from the `fvar` table (user-space values).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VariationAxis {
    /// Axis tag such as `*b"wght"`.
    pub tag: [u8; 4],
    pub min: f32,
    pub default: f32,
    pub max: f32,
    /// The axis is flagged hidden in `fvar` (not meant for user interfaces).
    pub hidden: bool,
}

/// Parses the `fvar` axis records.
pub(crate) fn parse_fvar(data: &[u8], range: TableRange) -> Vec<VariationAxis> {
    let Some(t) = data.get(range) else { return Vec::new() };
    let mut axes = Vec::new();
    let (Some(axes_off), Some(count), Some(size)) = (u16_at(t, 4), u16_at(t, 8), u16_at(t, 10))
    else {
        return axes;
    };
    if size < 20 {
        return axes;
    }
    for i in 0..count as usize {
        let mut r = Reader::at(t, axes_off as usize + i * size as usize);
        let rec = (|| {
            let tag = r.tag()?;
            let min = r.fixed()?;
            let default = r.fixed()?;
            let max = r.fixed()?;
            let flags = r.u16()?;
            Some(VariationAxis { tag, min, default, max, hidden: flags & 1 != 0 })
        })();
        match rec {
            Some(mut a) => {
                // Repair inconsistent records instead of failing.
                if !(a.min <= a.default && a.default <= a.max) {
                    a.min = a.min.min(a.default);
                    a.max = a.max.max(a.default);
                }
                axes.push(a)
            }
            None => break,
        }
    }
    // Keep the axis count small and sane (coordinates arrays are indexed
    // by axis).
    axes.truncate(64);
    axes
}

#[inline]
fn quantize(v: f32) -> f32 {
    (v * 16384.0).round() / 16384.0
}

/// Normalizes a user-space coordinate to [-1, 1] (default 0).
pub(crate) fn normalize(axis: &VariationAxis, value: f32) -> f32 {
    let v = if value.is_nan() { axis.default } else { value.clamp(axis.min, axis.max) };
    let n = if v < axis.default {
        let d = axis.default - axis.min;
        if d > 0.0 {
            (v - axis.default) / d
        } else {
            0.0
        }
    } else if v > axis.default {
        let d = axis.max - axis.default;
        if d > 0.0 {
            (v - axis.default) / d
        } else {
            0.0
        }
    } else {
        0.0
    };
    quantize(n.clamp(-1.0, 1.0))
}

/// Applies the `avar` (version 1) segment maps to normalized coordinates.
pub(crate) fn apply_avar(data: &[u8], range: Option<TableRange>, coords: &mut [f32]) {
    let Some(t) = range.and_then(|r| data.get(r)) else { return };
    let Some(axis_count) = u16_at(t, 6) else { return };
    let mut pos = 8usize;
    for (i, c) in coords.iter_mut().enumerate() {
        if i >= axis_count as usize {
            break;
        }
        let Some(n) = u16_at(t, pos) else { return };
        let maps = pos + 2;
        pos = maps + 4 * n as usize;
        if n < 2 {
            continue;
        }
        let get = |k: usize| -> Option<(f32, f32)> {
            let from = u16_at(t, maps + 4 * k)? as i16 as f32 / 16384.0;
            let to = u16_at(t, maps + 4 * k + 2)? as i16 as f32 / 16384.0;
            Some((from, to))
        };
        let v = *c;
        let mut mapped = None;
        let mut prev: Option<(f32, f32)> = None;
        for k in 0..n as usize {
            let Some((from, to)) = get(k) else { break };
            if v == from {
                mapped = Some(to);
                break;
            }
            if v < from {
                mapped = Some(match prev {
                    Some((pf, pt)) if from > pf => pt + (to - pt) * (v - pf) / (from - pf),
                    Some((_, pt)) => pt,
                    None => v + (to - from),
                });
                break;
            }
            prev = Some((from, to));
        }
        let v2 = match (mapped, prev) {
            (Some(m), _) => m,
            (None, Some((pf, pt))) => v + (pt - pf),
            (None, None) => v,
        };
        *c = quantize(v2.clamp(-1.0, 1.0));
    }
}

/// Scalar of one axis of a variation region (start, peak, end) at
/// normalized coordinate `v`, following the OpenType algorithm.
#[inline]
pub(crate) fn axis_scalar(start: f32, peak: f32, end: f32, v: f32) -> f32 {
    if peak == 0.0 || start > peak || peak > end || (start < 0.0 && end > 0.0) {
        return 1.0;
    }
    if v < start || v > end {
        return 0.0;
    }
    if v == peak {
        1.0
    } else if v < peak {
        if peak == start {
            1.0
        } else {
            (v - start) / (peak - start)
        }
    } else if end == peak {
        1.0
    } else {
        (end - v) / (end - peak)
    }
}

/// An `ItemVariationStore` (absolute offset into the font data).
#[derive(Debug, Clone, Copy)]
pub(crate) struct ItemVariationStore {
    offset: usize,
}

impl ItemVariationStore {
    pub fn new(data: &[u8], offset: usize) -> Option<Self> {
        let format = u16_at(data, offset)?;
        if format != 1 {
            return None;
        }
        Some(ItemVariationStore { offset })
    }

    /// Computes the scalar of every region of the store for `coords`.
    pub fn region_scalars(&self, data: &[u8], coords: &[f32]) -> Vec<f32> {
        let mut out = Vec::new();
        let Some(rl) = u32_at(data, self.offset + 2) else { return out };
        let Some(rl) = self.offset.checked_add(rl as usize) else { return out };
        let (Some(axis_count), Some(region_count)) = (u16_at(data, rl), u16_at(data, rl + 2)) else {
            return out;
        };
        let axis_count = axis_count as usize;
        let mut r = Reader::at(data, rl + 4);
        for _ in 0..region_count {
            let mut s = 1.0f32;
            for a in 0..axis_count {
                let (Some(start), Some(peak), Some(end)) = (r.f2dot14(), r.f2dot14(), r.f2dot14())
                else {
                    out.push(0.0);
                    return out;
                };
                if s != 0.0 {
                    let v = coords.get(a).copied().unwrap_or(0.0);
                    s *= axis_scalar(start, peak, end, v);
                }
            }
            out.push(s);
        }
        out
    }

    /// Interpolated delta of item (`outer`, `inner`) given precomputed region
    /// scalars.
    pub fn delta(&self, data: &[u8], outer: u16, inner: u16, scalars: &[f32]) -> f32 {
        self.delta_opt(data, outer, inner, scalars).unwrap_or(0.0)
    }

    fn delta_opt(&self, data: &[u8], outer: u16, inner: u16, scalars: &[f32]) -> Option<f32> {
        let count = u16_at(data, self.offset + 6)?;
        if outer >= count {
            return None;
        }
        let ivd = self.offset.checked_add(u32_at(data, self.offset + 8 + 4 * outer as usize)? as usize)?;
        let item_count = u16_at(data, ivd)?;
        let word_field = u16_at(data, ivd + 2)?;
        let region_index_count = u16_at(data, ivd + 4)? as usize;
        if inner >= item_count {
            return None;
        }
        let long_words = word_field & 0x8000 != 0;
        let word_count = ((word_field & 0x7FFF) as usize).min(region_index_count);
        let (big, small) = if long_words { (4usize, 2usize) } else { (2, 1) };
        let row_size = word_count * big + (region_index_count - word_count) * small;
        let indices = ivd + 6;
        let rows = indices + 2 * region_index_count;
        let mut r = Reader::at(data, rows.checked_add(row_size.checked_mul(inner as usize)?)?);
        let mut sum = 0.0f32;
        for k in 0..region_index_count {
            let d = if k < word_count {
                if long_words {
                    r.i32()? as f32
                } else {
                    r.i16()? as f32
                }
            } else if long_words {
                r.i16()? as f32
            } else {
                r.i8()? as f32
            };
            if d != 0.0 {
                let region = u16_at(data, indices + 2 * k)? as usize;
                sum += d * scalars.get(region).copied().unwrap_or(0.0);
            }
        }
        Some(sum)
    }
}

/// Looks up `(outer, inner)` for `index` in a `DeltaSetIndexMap` at
/// absolute offset `map`.
pub(crate) fn delta_set_index(data: &[u8], map: usize, index: u32) -> Option<(u16, u16)> {
    let format = u8_at(data, map)?;
    let entry_format = u8_at(data, map + 1)?;
    let (count, start) = match format {
        0 => (u16_at(data, map + 2)? as u32, map + 4),
        1 => (u32_at(data, map + 2)?, map + 6),
        _ => return None,
    };
    if count == 0 {
        return None;
    }
    let idx = index.min(count - 1) as usize;
    let size = (((entry_format & 0x30) >> 4) + 1) as usize;
    let inner_bits = ((entry_format & 0x0F) + 1) as u32;
    let entry = Reader::at(data, start.checked_add(idx.checked_mul(size)?)?).uint_n(size)?;
    let outer = (entry >> inner_bits) as u16;
    let inner = (entry & ((1u32 << inner_bits) - 1)) as u16;
    Some((outer, inner))
}

/// `HVAR` table: advance width variations.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Hvar {
    pub store: ItemVariationStore,
    advance_map: Option<usize>,
}

impl Hvar {
    pub fn parse(data: &[u8], range: TableRange) -> Option<Hvar> {
        let base = range.start;
        let store_off = u32_at(data, base + 4)? as usize;
        let adv_map = u32_at(data, base + 8)? as usize;
        if store_off == 0 {
            return None;
        }
        let store = ItemVariationStore::new(data, base.checked_add(store_off)?)?;
        let advance_map = if adv_map != 0 { Some(base.checked_add(adv_map)?) } else { None };
        Some(Hvar { store, advance_map })
    }

    pub fn advance_delta(&self, data: &[u8], glyph: u16, scalars: &[f32]) -> f32 {
        let (outer, inner) = match self.advance_map {
            Some(m) => match delta_set_index(data, m, glyph as u32) {
                Some(v) => v,
                None => return 0.0,
            },
            None => (0, glyph),
        };
        self.store.delta(data, outer, inner, scalars)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalars() {
        assert_eq!(axis_scalar(0.0, 1.0, 1.0, 0.5), 0.5);
        assert_eq!(axis_scalar(0.0, 1.0, 1.0, -0.5), 0.0);
        assert_eq!(axis_scalar(-1.0, -1.0, 0.0, -0.25), 0.25);
        assert_eq!(axis_scalar(0.2, 0.5, 1.0, 0.75), 0.5);
        assert_eq!(axis_scalar(0.0, 0.0, 0.0, 0.3), 1.0);
        // invalid region spanning zero: ignored axis
        assert_eq!(axis_scalar(-0.5, 0.5, 1.0, 0.0), 1.0);
    }

    #[test]
    fn avar_mapping() {
        let f = |v: f32| ((v * 16384.0) as i16).to_be_bytes();
        let mut t = vec![0, 1, 0, 0, 0, 0, 0, 2];
        // axis 0: -1->-1, 0->0, 0.5->0.8, 1->1
        t.extend_from_slice(&[0, 4]);
        for (a, b) in [(-1.0, -1.0), (0.0, 0.0), (0.5, 0.8), (1.0, 1.0)] {
            t.extend_from_slice(&f(a));
            t.extend_from_slice(&f(b));
        }
        // axis 1: identity (only 3 maps)
        t.extend_from_slice(&[0, 3]);
        for v in [-1.0, 0.0, 1.0] {
            t.extend_from_slice(&f(v));
            t.extend_from_slice(&f(v));
        }
        let mut c = [0.25, 0.3];
        apply_avar(&t, Some(0..t.len()), &mut c);
        assert!((c[0] - 0.4).abs() < 1e-3 && (c[1] - 0.3).abs() < 1e-3, "{c:?}");
        let mut c = [0.75, -0.5];
        apply_avar(&t, Some(0..t.len()), &mut c);
        assert!((c[0] - 0.9).abs() < 1e-3 && (c[1] + 0.5).abs() < 1e-3, "{c:?}");
        // truncated table: no panic, coordinates stay finite
        let mut c = [0.75, 0.5];
        apply_avar(&t[..13], Some(0..13), &mut c);
        assert!(c.iter().all(|v| v.is_finite()));
    }

    #[test]
    fn normalization() {
        let a = VariationAxis { tag: *b"wght", min: 100.0, default: 400.0, max: 900.0, hidden: false };
        assert_eq!(normalize(&a, 400.0), 0.0);
        assert_eq!(normalize(&a, 900.0), 1.0);
        assert_eq!(normalize(&a, 100.0), -1.0);
        assert_eq!(normalize(&a, 2000.0), 1.0);
        assert!((normalize(&a, 650.0) - 0.5).abs() < 1e-4);
        assert_eq!(normalize(&a, f32::NAN), 0.0);
    }
}
