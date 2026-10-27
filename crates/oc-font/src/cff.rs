//! `CFF ` table (Compact Font Format version 1) with Type 2 charstrings.
//!
//! Supports name-keyed and CID-keyed fonts (FDArray/FDSelect formats 0 and
//! 3), local and global subroutines, all path construction operators
//! including flex, and the (deprecated) arithmetic operators. Hints are
//! parsed only as far as needed to skip hint masks. The `seac` accent
//! composition form of `endchar` is not supported.

use crate::outline::OutlineSink;
use crate::reader::{u16_at, u8_at, Reader};
use crate::sfnt::TableRange;

const MAX_STACK: usize = 48;
const MAX_SUBR_DEPTH: u32 = 10;
/// Upper bound of operators executed per glyph (guards against
/// pathological subroutine nesting).
const MAX_OPS: u32 = 200_000;

/// A CFF INDEX structure (offsets absolute into the font data).
#[derive(Debug, Clone, Copy, Default)]
struct Index {
    count: u32,
    off_size: u8,
    offsets: usize,
    /// Position of the byte preceding the object data (offsets are 1-based).
    base: usize,
    end: usize,
}

impl Index {
    /// Parses an INDEX at `pos`; returns the index and the position after it.
    fn parse(data: &[u8], pos: usize, limit: usize) -> Option<Index> {
        let count = u16_at(data, pos)? as u32;
        if count == 0 {
            return Some(Index { count: 0, off_size: 1, offsets: pos + 2, base: pos + 2, end: pos + 2 });
        }
        let off_size = u8_at(data, pos + 2)?;
        if !(1..=4).contains(&off_size) {
            return None;
        }
        let offsets = pos + 3;
        let base = offsets + (count as usize + 1) * off_size as usize - 1;
        let last = Reader::at(data, offsets + count as usize * off_size as usize).uint_n(off_size as usize)?;
        let end = base.checked_add(last as usize)?;
        if end > limit {
            return None;
        }
        Some(Index { count, off_size, offsets, base, end })
    }

    fn get<'a>(&self, data: &'a [u8], i: u32) -> Option<&'a [u8]> {
        if i >= self.count {
            return None;
        }
        let os = self.off_size as usize;
        let mut r = Reader::at(data, self.offsets + i as usize * os);
        let a = r.uint_n(os)? as usize;
        let b = r.uint_n(os)? as usize;
        if a == 0 || b < a {
            return None;
        }
        let s = self.base.checked_add(a)?;
        let e = self.base.checked_add(b)?;
        if e > self.end {
            return None;
        }
        data.get(s..e)
    }
}

#[derive(Debug, Clone)]
enum Fds {
    /// Name-keyed font: one local subroutine INDEX (if any).
    Single(Option<Index>),
    /// CID-keyed font: FDSelect and per-FD local subroutines.
    Cid { select: usize, format: u8, local: Vec<Option<Index>> },
}

/// Parsed `CFF ` table.
#[derive(Debug, Clone)]
pub(crate) struct Cff {
    charstrings: Index,
    global: Index,
    fds: Fds,
}

/// DICT operator encoding: one-byte operators as is, escaped as 1200 + b1.
fn parse_dict(d: &[u8], mut f: impl FnMut(u16, &[f64])) -> Option<()> {
    let mut ops = [0f64; MAX_STACK];
    let mut n = 0usize;
    let mut r = Reader::new(d);
    while let Some(b0) = r.u8() {
        let v = match b0 {
            0..=21 => {
                let op = if b0 == 12 { 1200 + r.u8()? as u16 } else { b0 as u16 };
                f(op, &ops[..n]);
                n = 0;
                continue;
            }
            28 => r.i16()? as f64,
            29 => r.i32()? as f64,
            30 => parse_real(&mut r)?,
            32..=246 => b0 as f64 - 139.0,
            247..=250 => ((b0 as f64 - 247.0) * 256.0) + r.u8()? as f64 + 108.0,
            251..=254 => -((b0 as f64 - 251.0) * 256.0) - r.u8()? as f64 - 108.0,
            _ => return None,
        };
        if n < MAX_STACK {
            ops[n] = v;
            n += 1;
        }
    }
    Some(())
}

fn parse_real(r: &mut Reader) -> Option<f64> {
    let mut s = String::new();
    loop {
        let b = r.u8()?;
        for nib in [b >> 4, b & 0xF] {
            match nib {
                0..=9 => s.push((b'0' + nib) as char),
                0xA => s.push('.'),
                0xB => s.push('E'),
                0xC => s.push_str("E-"),
                0xE => s.push('-'),
                0xF => return Some(s.parse::<f64>().unwrap_or(0.0)),
                _ => {}
            }
            if s.len() > 64 {
                return None;
            }
        }
    }
}

/// Reads the Private DICT at (`size`, `offset`) and returns its local subrs.
fn private_subrs(data: &[u8], cff: &TableRange, size: f64, offset: f64) -> Option<Index> {
    if !(size >= 0.0 && offset > 0.0) {
        return None;
    }
    let start = cff.start.checked_add(offset as usize)?;
    let end = start.checked_add(size as usize)?;
    let dict = data.get(start..end.min(cff.end))?;
    let mut subrs = None;
    parse_dict(dict, |op, a| {
        if op == 19 {
            if let Some(&o) = a.last() {
                subrs = Some(o);
            }
        }
    })?;
    let o = subrs?;
    if o <= 0.0 {
        return None;
    }
    Index::parse(data, start.checked_add(o as usize)?, cff.end)
}

impl Cff {
    pub fn parse(data: &[u8], range: TableRange) -> Option<Cff> {
        let b = range.start;
        let major = u8_at(data, b)?;
        if major != 1 {
            return None;
        }
        let hdr_size = u8_at(data, b + 2)? as usize;
        let lim = range.end;
        let names = Index::parse(data, b + hdr_size, lim)?;
        let top = Index::parse(data, names.end, lim)?;
        let strings = Index::parse(data, top.end, lim)?;
        let global = Index::parse(data, strings.end, lim)?;
        let top_dict = top.get(data, 0)?;

        let mut charstrings_off = None;
        let mut private = None;
        let mut fd_array = None;
        let mut fd_select = None;
        let mut cs_type = 2.0;
        parse_dict(top_dict, |op, a| match op {
            17 => charstrings_off = a.last().copied(),
            18 if a.len() >= 2 => private = Some((a[0], a[1])),
            1206 => cs_type = a.last().copied().unwrap_or(2.0),
            1236 => fd_array = a.last().copied(),
            1237 => fd_select = a.last().copied(),
            _ => {}
        })?;
        if cs_type != 2.0 {
            return None;
        }
        let cs = charstrings_off?;
        if cs <= 0.0 {
            return None;
        }
        let charstrings = Index::parse(data, b.checked_add(cs as usize)?, lim)?;

        let fds = match (fd_array, fd_select) {
            (Some(fa), Some(fs)) if fa > 0.0 && fs > 0.0 => {
                let fd_index = Index::parse(data, b.checked_add(fa as usize)?, lim)?;
                let mut local = Vec::new();
                for i in 0..fd_index.count.min(256) {
                    let mut p = None;
                    if let Some(d) = fd_index.get(data, i) {
                        parse_dict(d, |op, a| {
                            if op == 18 && a.len() >= 2 {
                                p = Some((a[0], a[1]));
                            }
                        });
                    }
                    local.push(p.and_then(|(s, o)| private_subrs(data, &range, s, o)));
                }
                let select = b.checked_add(fs as usize)?;
                let format = u8_at(data, select)?;
                if format != 0 && format != 3 {
                    return None;
                }
                Fds::Cid { select, format, local }
            }
            _ => Fds::Single(private.and_then(|(s, o)| private_subrs(data, &range, s, o))),
        };
        Some(Cff { charstrings, global, fds })
    }

    pub fn num_glyphs(&self) -> u32 {
        self.charstrings.count
    }

    fn fd_index(&self, data: &[u8], select: usize, format: u8, gid: u16) -> Option<usize> {
        match format {
            0 => u8_at(data, select + 1 + gid as usize).map(|v| v as usize),
            3 => {
                let n = u16_at(data, select + 1)? as usize;
                let (mut lo, mut hi) = (0usize, n);
                while lo < hi {
                    let mid = lo + (hi - lo) / 2;
                    let rec = select + 3 + 3 * mid;
                    let first = u16_at(data, rec)?;
                    let next = u16_at(data, rec + 3)?;
                    if gid < first {
                        hi = mid;
                    } else if gid >= next {
                        lo = mid + 1;
                    } else {
                        return u8_at(data, rec + 2).map(|v| v as usize);
                    }
                }
                None
            }
            _ => None,
        }
    }

    /// Emits the outline of glyph `gid`. Returns `None` on malformed data.
    pub fn outline(&self, data: &[u8], gid: u16, sink: &mut dyn OutlineSink) -> Option<()> {
        let cs = self.charstrings.get(data, gid as u32)?;
        let local = match &self.fds {
            Fds::Single(l) => *l,
            Fds::Cid { select, format, local } => {
                let fd = self.fd_index(data, *select, *format, gid)?;
                *local.get(fd)?
            }
        };
        let mut it = Interp {
            data,
            global: self.global,
            local,
            stack: [0.0; MAX_STACK],
            sp: 0,
            x: 0.0,
            y: 0.0,
            nstems: 0,
            open: false,
            ops: 0,
            transient: [0.0; 32],
            sink,
        };
        it.run(cs, 0)?;
        if it.open {
            it.sink.close();
        }
        Some(())
    }
}

fn bias(count: u32) -> i32 {
    if count < 1240 {
        107
    } else if count < 33900 {
        1131
    } else {
        32768
    }
}

struct Interp<'a, 's> {
    data: &'a [u8],
    global: Index,
    local: Option<Index>,
    stack: [f32; MAX_STACK],
    sp: usize,
    x: f32,
    y: f32,
    nstems: usize,
    open: bool,
    ops: u32,
    transient: [f32; 32],
    sink: &'s mut dyn OutlineSink,
}

enum Flow {
    Continue,
    Return,
    End,
}

impl<'a, 's> Interp<'a, 's> {
    #[inline]
    fn push(&mut self, v: f32) -> Option<()> {
        if self.sp >= MAX_STACK {
            return None;
        }
        self.stack[self.sp] = v;
        self.sp += 1;
        Some(())
    }

    #[inline]
    fn pop(&mut self) -> Option<f32> {
        if self.sp == 0 {
            return None;
        }
        self.sp -= 1;
        Some(self.stack[self.sp])
    }

    fn move_to(&mut self, dx: f32, dy: f32) {
        if self.open {
            self.sink.close();
        }
        self.x += dx;
        self.y += dy;
        self.sink.move_to(self.x, self.y);
        self.open = true;
    }

    fn ensure_open(&mut self) {
        if !self.open {
            self.sink.move_to(self.x, self.y);
            self.open = true;
        }
    }

    fn line(&mut self, dx: f32, dy: f32) {
        self.ensure_open();
        self.x += dx;
        self.y += dy;
        self.sink.line_to(self.x, self.y);
    }

    fn curve(&mut self, dx1: f32, dy1: f32, dx2: f32, dy2: f32, dx3: f32, dy3: f32) {
        self.ensure_open();
        let x1 = self.x + dx1;
        let y1 = self.y + dy1;
        let x2 = x1 + dx2;
        let y2 = y1 + dy2;
        self.x = x2 + dx3;
        self.y = y2 + dy3;
        self.sink.curve_to(x1, y1, x2, y2, self.x, self.y);
    }

    fn run(&mut self, code: &[u8], depth: u32) -> Option<Flow> {
        if depth > MAX_SUBR_DEPTH {
            return None;
        }
        let mut r = Reader::new(code);
        while let Some(b0) = r.u8() {
            self.ops += 1;
            if self.ops > MAX_OPS {
                return None;
            }
            match b0 {
                32..=246 => self.push(b0 as f32 - 139.0)?,
                247..=250 => {
                    let b1 = r.u8()? as f32;
                    self.push((b0 as f32 - 247.0) * 256.0 + b1 + 108.0)?
                }
                251..=254 => {
                    let b1 = r.u8()? as f32;
                    self.push(-(b0 as f32 - 251.0) * 256.0 - b1 - 108.0)?
                }
                28 => {
                    let v = r.i16()? as f32;
                    self.push(v)?
                }
                255 => {
                    let v = r.i32()? as f32 / 65536.0;
                    self.push(v)?
                }
                10 | 29 => {
                    let idx = self.pop()? as i32;
                    let (index, local) = if b0 == 10 { (self.local?, true) } else { (self.global, false) };
                    let _ = local;
                    let i = idx.checked_add(bias(index.count))?;
                    if i < 0 {
                        return None;
                    }
                    let sub = index.get(self.data, i as u32)?;
                    match self.run(sub, depth + 1)? {
                        Flow::End => return Some(Flow::End),
                        Flow::Return | Flow::Continue => {}
                    }
                }
                11 => return Some(Flow::Return),
                14 => {
                    // endchar (the seac form with 4 extra args is ignored)
                    if self.open {
                        self.sink.close();
                        self.open = false;
                    }
                    self.sp = 0;
                    return Some(Flow::End);
                }
                12 => {
                    let b1 = r.u8()?;
                    self.escape(b1)?;
                }
                _ => self.operator(b0, &mut r)?,
            }
        }
        Some(Flow::Continue)
    }

    fn operator(&mut self, op: u8, r: &mut Reader) -> Option<()> {
        let n = self.sp;
        let a = self.stack;
        match op {
            // hstem, vstem, hstemhm, vstemhm
            1 | 3 | 18 | 23 => {
                self.nstems += n / 2;
            }
            // hintmask, cntrmask
            19 | 20 => {
                self.nstems += n / 2;
                r.skip(self.nstems.div_ceil(8))?;
            }
            21 => {
                if n < 2 {
                    return None;
                }
                self.move_to(a[n - 2], a[n - 1]);
            }
            22 => {
                if n < 1 {
                    return None;
                }
                self.move_to(a[n - 1], 0.0);
            }
            4 => {
                if n < 1 {
                    return None;
                }
                self.move_to(0.0, a[n - 1]);
            }
            5 => {
                let mut i = 0;
                while i + 2 <= n {
                    self.line(a[i], a[i + 1]);
                    i += 2;
                }
            }
            6 | 7 => {
                let mut horizontal = op == 6;
                for &v in &a[..n] {
                    if horizontal {
                        self.line(v, 0.0);
                    } else {
                        self.line(0.0, v);
                    }
                    horizontal = !horizontal;
                }
            }
            8 => {
                let mut i = 0;
                while i + 6 <= n {
                    self.curve(a[i], a[i + 1], a[i + 2], a[i + 3], a[i + 4], a[i + 5]);
                    i += 6;
                }
            }
            24 => {
                // rcurveline
                let mut i = 0;
                while n - i >= 8 {
                    self.curve(a[i], a[i + 1], a[i + 2], a[i + 3], a[i + 4], a[i + 5]);
                    i += 6;
                }
                if n - i >= 2 {
                    self.line(a[i], a[i + 1]);
                }
            }
            25 => {
                // rlinecurve
                let mut i = 0;
                while n - i >= 8 {
                    self.line(a[i], a[i + 1]);
                    i += 2;
                }
                if n - i >= 6 {
                    self.curve(a[i], a[i + 1], a[i + 2], a[i + 3], a[i + 4], a[i + 5]);
                }
            }
            26 => {
                // vvcurveto
                let mut i = 0;
                let mut dx1 = 0.0;
                if n % 2 == 1 {
                    dx1 = a[0];
                    i = 1;
                }
                while i + 4 <= n {
                    self.curve(dx1, a[i], a[i + 1], a[i + 2], 0.0, a[i + 3]);
                    dx1 = 0.0;
                    i += 4;
                }
            }
            27 => {
                // hhcurveto
                let mut i = 0;
                let mut dy1 = 0.0;
                if n % 2 == 1 {
                    dy1 = a[0];
                    i = 1;
                }
                while i + 4 <= n {
                    self.curve(a[i], dy1, a[i + 1], a[i + 2], a[i + 3], 0.0);
                    dy1 = 0.0;
                    i += 4;
                }
            }
            30 | 31 => {
                // vhcurveto / hvcurveto
                let mut horizontal = op == 31;
                let mut i = 0;
                while i + 4 <= n {
                    let last = if n - i == 5 { a[i + 4] } else { 0.0 };
                    if horizontal {
                        self.curve(a[i], 0.0, a[i + 1], a[i + 2], last, a[i + 3]);
                    } else {
                        self.curve(0.0, a[i], a[i + 1], a[i + 2], a[i + 3], last);
                    }
                    horizontal = !horizontal;
                    i += 4;
                }
            }
            // Reserved operators: ignore.
            _ => {}
        }
        self.sp = 0;
        Some(())
    }

    fn escape(&mut self, op: u8) -> Option<()> {
        let n = self.sp;
        let a = self.stack;
        match op {
            35 => {
                // flex
                if n < 13 {
                    return None;
                }
                self.curve(a[0], a[1], a[2], a[3], a[4], a[5]);
                self.curve(a[6], a[7], a[8], a[9], a[10], a[11]);
                self.sp = 0;
            }
            34 => {
                // hflex
                if n < 7 {
                    return None;
                }
                self.curve(a[0], 0.0, a[1], a[2], a[3], 0.0);
                self.curve(a[4], 0.0, a[5], -a[2], a[6], 0.0);
                self.sp = 0;
            }
            36 => {
                // hflex1
                if n < 9 {
                    return None;
                }
                self.curve(a[0], a[1], a[2], a[3], a[4], 0.0);
                self.curve(a[5], 0.0, a[6], a[7], a[8], -(a[1] + a[3] + a[7]));
                self.sp = 0;
            }
            37 => {
                // flex1
                if n < 11 {
                    return None;
                }
                let dx = a[0] + a[2] + a[4] + a[6] + a[8];
                let dy = a[1] + a[3] + a[5] + a[7] + a[9];
                let (dx6, dy6) = if dx.abs() > dy.abs() { (a[10], -dy) } else { (-dx, a[10]) };
                self.curve(a[0], a[1], a[2], a[3], a[4], a[5]);
                self.curve(a[6], a[7], a[8], a[9], dx6, dy6);
                self.sp = 0;
            }
            // Arithmetic and storage operators (Type 2, deprecated).
            3 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(((a != 0.0) && (b != 0.0)) as i32 as f32)?;
            }
            4 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(((a != 0.0) || (b != 0.0)) as i32 as f32)?;
            }
            5 => {
                let a = self.pop()?;
                self.push((a == 0.0) as i32 as f32)?;
            }
            9 => {
                let a = self.pop()?;
                self.push(a.abs())?;
            }
            10 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(a + b)?;
            }
            11 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(a - b)?;
            }
            12 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(if b == 0.0 { 0.0 } else { a / b })?;
            }
            14 => {
                let a = self.pop()?;
                self.push(-a)?;
            }
            15 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push((a == b) as i32 as f32)?;
            }
            18 => {
                self.pop()?;
            }
            20 => {
                let i = self.pop()?;
                let v = self.pop()?;
                if let Some(slot) = self.transient.get_mut(i as usize) {
                    *slot = v;
                }
            }
            21 => {
                let i = self.pop()?;
                let v = self.transient.get(i as usize).copied().unwrap_or(0.0);
                self.push(v)?;
            }
            22 => {
                let v2 = self.pop()?;
                let v1 = self.pop()?;
                let s2 = self.pop()?;
                let s1 = self.pop()?;
                self.push(if v1 <= v2 { s1 } else { s2 })?;
            }
            23 => self.push(0.5)?,
            24 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(a * b)?;
            }
            26 => {
                let a = self.pop()?;
                self.push(a.max(0.0).sqrt())?;
            }
            27 => {
                let a = self.pop()?;
                self.push(a)?;
                self.push(a)?;
            }
            28 => {
                let (b, a) = (self.pop()?, self.pop()?);
                self.push(b)?;
                self.push(a)?;
            }
            29 => {
                let i = self.pop()?;
                let i = if i < 0.0 { 0 } else { i as usize };
                if i >= self.sp {
                    return None;
                }
                let v = self.stack[self.sp - 1 - i];
                self.push(v)?;
            }
            30 => {
                let j = self.pop()? as i32;
                let nn = self.pop()?;
                if nn < 0.0 || nn as usize > self.sp {
                    return None;
                }
                let nn = nn as usize;
                if nn > 0 {
                    let s = &mut self.stack[self.sp - nn..self.sp];
                    let k = j.rem_euclid(nn as i32) as usize;
                    s.rotate_right(k);
                }
            }
            // dotsection and reserved: clear the stack.
            _ => self.sp = 0,
        }
        Some(())
    }
}
