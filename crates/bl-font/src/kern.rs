//! Pair kerning: GPOS pair adjustment (lookup type 2, formats 1 and 2,
//! possibly wrapped in extension lookups) reached through the `kern`
//! feature of the `latn` or `DFLT` script, with variable-font deltas via
//! `GDEF`'s ItemVariationStore; falls back to the legacy `kern` table
//! (format 0 subtables, Microsoft and Apple headers).

use crate::reader::{bsearch_u16, u16_at, u32_at, Reader};
use crate::sfnt::TableRange;
use crate::var::ItemVariationStore;

/// Limits guarding against malicious tables.
const MAX_LOOKUPS: usize = 256;
const MAX_SUBTABLES: usize = 1024;

#[derive(Debug, Clone, Copy)]
struct LegacySub {
    pairs: usize,
    count: usize,
    replace: bool,
}

#[derive(Debug, Default, Clone)]
pub(crate) struct Kerning {
    /// PairPos subtable offsets (absolute), grouped by lookup.
    lookups: Vec<Vec<usize>>,
    legacy: Vec<LegacySub>,
    /// GDEF item variation store (variable device tables).
    pub gdef_store: Option<ItemVariationStore>,
}

impl Kerning {
    pub fn parse(data: &[u8], gpos: Option<TableRange>, kern: Option<TableRange>, gdef: Option<TableRange>) -> Kerning {
        let mut k = Kerning::default();
        if let Some(r) = gpos {
            k.lookups = parse_gpos(data, r.start).unwrap_or_default();
        }
        if k.lookups.is_empty() {
            if let Some(r) = kern {
                k.legacy = parse_kern(data, r).unwrap_or_default();
            }
        }
        if let Some(r) = gdef {
            k.gdef_store = gdef_store(data, r.start);
        }
        k
    }

    pub fn is_empty(&self) -> bool {
        self.lookups.is_empty() && self.legacy.is_empty()
    }

    /// Horizontal pair adjustment in font units.
    pub fn kerning(&self, data: &[u8], left: u16, right: u16, scalars: &[f32]) -> f32 {
        if !self.lookups.is_empty() {
            let mut total = 0.0;
            for lookup in &self.lookups {
                for &st in lookup {
                    if let Some(v) = self.pair_value(data, st, left, right, scalars) {
                        total += v;
                        break;
                    }
                }
            }
            return total;
        }
        let key = (left as u32) << 16 | right as u32;
        let mut total = 0.0;
        for s in &self.legacy {
            if let Some(v) = legacy_lookup(data, s, key) {
                if s.replace {
                    total = v as f32;
                } else {
                    total += v as f32;
                }
            }
        }
        total
    }

    fn pair_value(&self, data: &[u8], st: usize, left: u16, right: u16, scalars: &[f32]) -> Option<f32> {
        let format = u16_at(data, st)?;
        let cov = st + u16_at(data, st + 2)? as usize;
        let vf1 = u16_at(data, st + 4)?;
        let vf2 = u16_at(data, st + 6)?;
        let ci = coverage_index(data, cov, left)?;
        let s1 = value_size(vf1);
        let s2 = value_size(vf2);
        let (rec1, rec2) = match format {
            1 => {
                let count = u16_at(data, st + 8)?;
                if ci >= count as usize {
                    return None;
                }
                let set = st + u16_at(data, st + 10 + 2 * ci)? as usize;
                let n = u16_at(data, set)? as usize;
                let rec_size = 2 + s1 + s2;
                let i = bsearch_u16(data, set + 2, n, rec_size, right)?;
                let rec = set + 2 + i * rec_size + 2;
                (rec, rec + s1)
            }
            2 => {
                let cd1 = st + u16_at(data, st + 8)? as usize;
                let cd2 = st + u16_at(data, st + 10)? as usize;
                let c1n = u16_at(data, st + 12)? as usize;
                let c2n = u16_at(data, st + 14)? as usize;
                let c1 = class_of(data, cd1, left) as usize;
                let c2 = class_of(data, cd2, right) as usize;
                if c1 >= c1n || c2 >= c2n {
                    return None;
                }
                let rec = c1
                    .checked_mul(c2n)
                    .and_then(|v| v.checked_add(c2))
                    .and_then(|v| v.checked_mul(s1 + s2))
                    .and_then(|v| v.checked_add(st + 16))?;
                (rec, rec + s1)
            }
            _ => return None,
        };
        let v1 = self.value_x(data, st, rec1, vf1, true, scalars);
        let v2 = self.value_x(data, st, rec2, vf2, false, scalars);
        Some(v1 + v2)
    }

    /// Horizontal contribution of a ValueRecord: the first glyph's
    /// XAdvance, or the second glyph's XPlacement.
    fn value_x(&self, data: &[u8], base: usize, rec: usize, vf: u16, first: bool, scalars: &[f32]) -> f32 {
        let (value_bit, device_bit) = if first { (0x0004, 0x0040) } else { (0x0001, 0x0010) };
        if vf & value_bit == 0 {
            return 0.0;
        }
        let field = |bit: u16| rec + 2 * (vf & (bit - 1)).count_ones() as usize;
        let mut v = u16_at(data, field(value_bit)).map_or(0.0, |x| x as i16 as f32);
        if vf & device_bit != 0 && !scalars.is_empty() {
            if let (Some(store), Some(off)) = (self.gdef_store, u16_at(data, field(device_bit))) {
                if off != 0 {
                    let dev = base + off as usize;
                    if u16_at(data, dev + 4) == Some(0x8000) {
                        if let (Some(outer), Some(inner)) = (u16_at(data, dev), u16_at(data, dev + 2)) {
                            v += store.delta(data, outer, inner, scalars);
                        }
                    }
                }
            }
        }
        v
    }
}

fn value_size(vf: u16) -> usize {
    2 * (vf & 0xFF).count_ones() as usize
}

pub(crate) fn coverage_index(data: &[u8], cov: usize, g: u16) -> Option<usize> {
    match u16_at(data, cov)? {
        1 => {
            let n = u16_at(data, cov + 2)? as usize;
            bsearch_u16(data, cov + 4, n, 2, g)
        }
        2 => {
            let n = u16_at(data, cov + 2)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let rec = cov + 4 + 6 * mid;
                let start = u16_at(data, rec)?;
                let end = u16_at(data, rec + 2)?;
                if g < start {
                    hi = mid;
                } else if g > end {
                    lo = mid + 1;
                } else {
                    return Some(u16_at(data, rec + 4)? as usize + (g - start) as usize);
                }
            }
            None
        }
        _ => None,
    }
}

pub(crate) fn class_of(data: &[u8], cd: usize, g: u16) -> u16 {
    (|| match u16_at(data, cd)? {
        1 => {
            let start = u16_at(data, cd + 2)?;
            let n = u16_at(data, cd + 4)?;
            let i = g.checked_sub(start)?;
            if i < n {
                u16_at(data, cd + 6 + 2 * i as usize)
            } else {
                None
            }
        }
        2 => {
            let n = u16_at(data, cd + 2)? as usize;
            let (mut lo, mut hi) = (0usize, n);
            while lo < hi {
                let mid = lo + (hi - lo) / 2;
                let rec = cd + 4 + 6 * mid;
                let start = u16_at(data, rec)?;
                let end = u16_at(data, rec + 2)?;
                if g < start {
                    hi = mid;
                } else if g > end {
                    lo = mid + 1;
                } else {
                    return u16_at(data, rec + 4);
                }
            }
            None
        }
        _ => None,
    })()
    .unwrap_or(0)
}

/// Collects the PairPos subtables of the `kern` feature lookups.
fn parse_gpos(data: &[u8], g: usize) -> Option<Vec<Vec<usize>>> {
    let major = u16_at(data, g)?;
    if major != 1 {
        return None;
    }
    let script_list = g + u16_at(data, g + 4)? as usize;
    let feature_list = g + u16_at(data, g + 6)? as usize;
    let lookup_list = g + u16_at(data, g + 8)? as usize;

    let mut lookup_indices: Vec<u16> = Vec::new();
    for script_tag in [*b"latn", *b"DFLT"] {
        let Some(script) = find_tagged(data, script_list, &script_tag) else { continue };
        // Default LangSys, or the first LangSys if there is no default.
        let def = u16_at(data, script)? as usize;
        let langsys = if def != 0 {
            script + def
        } else if u16_at(data, script + 2)? > 0 {
            script + u16_at(data, script + 8)? as usize
        } else {
            continue;
        };
        let nf = u16_at(data, langsys + 4)? as usize;
        let feature_count = u16_at(data, feature_list)?;
        for i in 0..nf {
            let fi = u16_at(data, langsys + 6 + 2 * i)?;
            if fi >= feature_count {
                continue;
            }
            let rec = feature_list + 2 + 6 * fi as usize;
            if data.get(rec..rec + 4) != Some(b"kern") {
                continue;
            }
            let feature = feature_list + u16_at(data, rec + 4)? as usize;
            let nl = u16_at(data, feature + 2)? as usize;
            for j in 0..nl {
                lookup_indices.push(u16_at(data, feature + 4 + 2 * j)?);
            }
        }
        if !lookup_indices.is_empty() {
            break;
        }
    }
    lookup_indices.sort_unstable();
    lookup_indices.dedup();
    lookup_indices.truncate(MAX_LOOKUPS);

    let lookup_count = u16_at(data, lookup_list)?;
    let mut out = Vec::new();
    let mut total = 0usize;
    for li in lookup_indices {
        if li >= lookup_count {
            continue;
        }
        let Some(lookup) = u16_at(data, lookup_list + 2 + 2 * li as usize).map(|o| lookup_list + o as usize)
        else {
            continue;
        };
        let (Some(kind), Some(n)) = (u16_at(data, lookup), u16_at(data, lookup + 4)) else { continue };
        let mut subs = Vec::new();
        for s in 0..n as usize {
            let Some(off) = u16_at(data, lookup + 6 + 2 * s) else { break };
            let mut st = lookup + off as usize;
            let mut t = kind;
            if t == 9 {
                // ExtensionPosFormat1
                if u16_at(data, st) != Some(1) {
                    continue;
                }
                let (Some(et), Some(eo)) = (u16_at(data, st + 2), u32_at(data, st + 4)) else { continue };
                t = et;
                st = st.saturating_add(eo as usize);
            }
            if t == 2 && matches!(u16_at(data, st), Some(1) | Some(2)) {
                subs.push(st);
                total += 1;
            }
        }
        if !subs.is_empty() {
            out.push(subs);
        }
        if total > MAX_SUBTABLES {
            break;
        }
    }
    Some(out)
}

/// Finds a record in a tag list (`count`, then `{tag, offset16}` records).
fn find_tagged(data: &[u8], list: usize, tag: &[u8; 4]) -> Option<usize> {
    let n = u16_at(data, list)? as usize;
    for i in 0..n {
        let rec = list + 2 + 6 * i;
        if data.get(rec..rec + 4)? == tag {
            return Some(list + u16_at(data, rec + 4)? as usize);
        }
    }
    None
}

fn gdef_store(data: &[u8], g: usize) -> Option<ItemVariationStore> {
    let major = u16_at(data, g)?;
    let minor = u16_at(data, g + 2)?;
    if major != 1 || minor < 3 {
        return None;
    }
    let off = u32_at(data, g + 14)? as usize;
    if off == 0 {
        return None;
    }
    ItemVariationStore::new(data, g.checked_add(off)?)
}

fn parse_kern(data: &[u8], range: TableRange) -> Option<Vec<LegacySub>> {
    let t = range.start;
    let end = range.end;
    let mut subs = Vec::new();
    let v0 = u16_at(data, t)?;
    if v0 == 0 {
        // Microsoft format
        let n = u16_at(data, t + 2)?;
        let mut pos = t + 4;
        for _ in 0..n {
            let mut r = Reader::at(data, pos);
            let _version = r.u16()?;
            let length = r.u16()? as usize;
            let coverage = r.u16()?;
            let format = coverage >> 8;
            let mut next = pos + length.max(6);
            if format == 0 {
                let count = r.u16()? as usize;
                let pairs = pos + 14;
                // `length` overflows for big tables; trust the pair count.
                next = pairs + 6 * count;
                let horizontal = coverage & 1 != 0;
                let minimum = coverage & 2 != 0;
                let cross = coverage & 4 != 0;
                if horizontal && !minimum && !cross {
                    let count = count.min(end.saturating_sub(pairs) / 6);
                    subs.push(LegacySub { pairs, count, replace: coverage & 8 != 0 });
                }
            }
            if next >= end {
                break;
            }
            pos = next;
        }
    } else if u32_at(data, t)? == 0x0001_0000 {
        // Apple format
        let n = u32_at(data, t + 4)?.min(64);
        let mut pos = t + 8;
        for _ in 0..n {
            let length = u32_at(data, pos)? as usize;
            let coverage = u16_at(data, pos + 4)?;
            let format = coverage & 0xFF;
            if format == 0 && coverage & 0xE000 == 0 {
                let count = u16_at(data, pos + 8)? as usize;
                let pairs = pos + 16;
                let count = count.min(end.saturating_sub(pairs) / 6);
                subs.push(LegacySub { pairs, count, replace: false });
            }
            if length < 8 {
                break;
            }
            pos = pos.checked_add(length)?;
            if pos >= end {
                break;
            }
        }
    }
    Some(subs)
}

fn legacy_lookup(data: &[u8], s: &LegacySub, key: u32) -> Option<i16> {
    let (mut lo, mut hi) = (0usize, s.count);
    while lo < hi {
        let mid = lo + (hi - lo) / 2;
        let rec = s.pairs + 6 * mid;
        let k = u32_at(data, rec)?;
        if k < key {
            lo = mid + 1;
        } else if k > key {
            hi = mid;
        } else {
            return u16_at(data, rec + 4).map(|v| v as i16);
        }
    }
    None
}
