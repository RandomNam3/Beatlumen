//! HEIF container (ISO/IEC 23008-12) as used by AVIF and HEIC: items,
//! their locations and properties, grids of tiles, alpha auxiliary images
//! and the transformations (clean aperture, rotation, mirroring). Image
//! sequences ('avis', 'msf1') are read through their primary item, or the
//! first sample of their first video track.

use crate::Error;

const BAD: Error = Error::InvalidData("heif: malformed box");

/// The codec of the coded images.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Codec {
    Av1,
    Hevc,
}

/// True when the data starts with an `ftyp` box of an AVIF or HEIC file.
pub(crate) fn probe(data: &[u8]) -> Option<Codec> {
    if data.len() < 16 || &data[4..8] != b"ftyp" {
        return None;
    }
    let size = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
    let end = size.clamp(16, data.len().min(256));
    let mut brands = vec![&data[8..12]];
    let mut o = 16;
    while o + 4 <= end {
        brands.push(&data[o..o + 4]);
        o += 4;
    }
    if brands.iter().any(|b| *b == b"avif" || *b == b"avis") {
        Some(Codec::Av1)
    } else if brands.iter().any(|b| {
        matches!(
            *b,
            b"heic" | b"heix" | b"heim" | b"heis" | b"hevc" | b"hevx"
        )
    }) {
        Some(Codec::Hevc)
    } else if brands.iter().any(|b| *b == b"mif1" || *b == b"msf1") {
        // Generic HEIF: the item types decide; AV1 is the likelier one
        // to be readable, but parsing tells.
        Some(Codec::Av1)
    } else {
        None
    }
}

/// A box: type, payload range in the file.
#[derive(Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    start: usize,
    end: usize,
}

fn read_u16(d: &[u8], o: usize) -> Result<u16, Error> {
    d.get(o..o + 2)
        .map(|b| u16::from_be_bytes([b[0], b[1]]))
        .ok_or(BAD)
}

fn read_u32(d: &[u8], o: usize) -> Result<u32, Error> {
    d.get(o..o + 4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
        .ok_or(BAD)
}

/// Reads an n-byte big endian unsigned (n = 0, 1, 2, 4 or 8).
fn read_n(d: &[u8], o: usize, n: usize) -> Result<u64, Error> {
    let b = d.get(o..o + n).ok_or(BAD)?;
    let mut v = 0u64;
    for &x in b {
        v = (v << 8) | x as u64;
    }
    Ok(v)
}

/// The child boxes of the range [start, end).
fn children(d: &[u8], start: usize, end: usize) -> Result<Vec<BoxRef>, Error> {
    let mut out = Vec::new();
    let mut o = start;
    while o + 8 <= end {
        let size = read_u32(d, o)? as u64;
        let mut kind = [0u8; 4];
        kind.copy_from_slice(&d[o + 4..o + 8]);
        let mut hdr = 8;
        let size = match size {
            0 => (end - o) as u64,
            1 => {
                hdr = 16;
                read_n(d, o + 8, 8)?
            }
            s => s,
        };
        let size = usize::try_from(size).map_err(|_| BAD)?;
        if size < hdr || o.checked_add(size).map_or(true, |e| e > end) {
            return Err(BAD);
        }
        out.push(BoxRef {
            kind,
            start: o + hdr,
            end: o + size,
        });
        o += size;
        if out.len() > 100_000 {
            return Err(BAD);
        }
    }
    Ok(out)
}

fn find(boxes: &[BoxRef], kind: &[u8; 4]) -> Option<BoxRef> {
    boxes.iter().copied().find(|b| &b.kind == kind)
}

/// A property of the ipco box.
#[derive(Debug, Clone, Default)]
pub(crate) struct Props {
    pub width: u32,
    pub height: u32,
    /// av1C configuration OBUs, or the whole hvcC box (header included).
    pub config: Vec<u8>,
    pub has_config: bool,
    /// colr nclx: primaries, transfer, matrix, full range.
    pub nclx: Option<(u16, u16, u16, bool)>,
    /// Anti-clockwise rotation in quarter turns.
    pub rotation: u8,
    /// Mirror axis: 0 vertical (left and right swap), 1 horizontal.
    pub mirror: Option<u8>,
    /// Clean aperture: (width, height, horizontal offset, vertical
    /// offset) as rationals.
    pub clap: Option<[(i64, i64); 4]>,
    pub alpha: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Item {
    pub id: u32,
    pub kind: [u8; 4],
    pub hidden: bool,
    /// (construction method, extents (offset, length)), base offset
    /// included.
    pub method: u8,
    pub extents: Vec<(u64, u64)>,
    pub props: Props,
}

/// The parsed structure of a HEIF file.
pub(crate) struct Heif<'a> {
    pub data: &'a [u8],
    pub items: Vec<Item>,
    /// iref entries: (type, from, to list).
    pub refs: Vec<([u8; 4], u32, Vec<u32>)>,
    pub primary: u32,
    idat: Option<(usize, usize)>,
    /// First sample of the first video track (sequences without items).
    pub track: Option<(Props, usize, usize)>,
}

/// A coded image to decode: one item or a grid of tiles.
#[derive(Debug, Clone)]
pub(crate) enum Picture {
    Coded(u32),
    Grid {
        rows: usize,
        cols: usize,
        width: u32,
        height: u32,
        tiles: Vec<u32>,
    },
    /// The first sample of a track.
    Sample,
}

impl<'a> Heif<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Heif<'a>, Error> {
        let top = children(data, 0, data.len()).or_else(|_| {
            // Tolerate trailing garbage: parse what is complete.
            let mut v = Vec::new();
            let mut o = 0;
            while o + 8 <= data.len() {
                let s = read_u32(data, o)? as usize;
                if s < 8 || o + s > data.len() {
                    break;
                }
                let mut k = [0u8; 4];
                k.copy_from_slice(&data[o + 4..o + 8]);
                v.push(BoxRef {
                    kind: k,
                    start: o + 8,
                    end: o + s,
                });
                o += s;
            }
            Ok::<_, Error>(v)
        })?;
        let mut h = Heif {
            data,
            items: Vec::new(),
            refs: Vec::new(),
            primary: 0,
            idat: None,
            track: None,
        };
        if let Some(meta) = find(&top, b"meta") {
            h.parse_meta(meta)?;
        }
        if h.items.is_empty() || !h.items.iter().any(|i| i.id == h.primary) {
            if let Some(moov) = find(&top, b"moov") {
                h.track = parse_moov(data, moov).ok().flatten();
            }
            if h.track.is_none() {
                return Err(Error::InvalidData("heif: no image"));
            }
        }
        Ok(h)
    }

    fn parse_meta(&mut self, meta: BoxRef) -> Result<(), Error> {
        let d = self.data;
        // FullBox header.
        let kids = children(d, meta.start + 4, meta.end)?;
        if let Some(b) = find(&kids, b"pitm") {
            let v = *d.get(b.start).ok_or(BAD)?;
            self.primary = if v == 0 {
                read_u16(d, b.start + 4)? as u32
            } else {
                read_u32(d, b.start + 4)?
            };
        }
        if let Some(b) = find(&kids, b"idat") {
            self.idat = Some((b.start, b.end));
        }
        if let Some(b) = find(&kids, b"iinf") {
            let v = *d.get(b.start).ok_or(BAD)?;
            let first = if v == 0 { b.start + 6 } else { b.start + 8 };
            for e in children(d, first, b.end)? {
                if &e.kind != b"infe" {
                    continue;
                }
                let ver = *d.get(e.start).ok_or(BAD)?;
                let flags = read_n(d, e.start + 1, 3)?;
                if ver < 2 {
                    continue;
                }
                let (id, o) = if ver == 2 {
                    (read_u16(d, e.start + 4)? as u32, e.start + 6)
                } else {
                    (read_u32(d, e.start + 4)?, e.start + 8)
                };
                let mut kind = [0u8; 4];
                kind.copy_from_slice(d.get(o + 2..o + 6).ok_or(BAD)?);
                self.items.push(Item {
                    id,
                    kind,
                    hidden: flags & 1 != 0,
                    method: 0,
                    extents: Vec::new(),
                    props: Props::default(),
                });
            }
        }
        if let Some(b) = find(&kids, b"iloc") {
            self.parse_iloc(b)?;
        }
        if let Some(b) = find(&kids, b"iref") {
            let v = *d.get(b.start).ok_or(BAD)?;
            let n = if v == 0 { 2 } else { 4 };
            for r in children(d, b.start + 4, b.end)? {
                let from = read_n(d, r.start, n)? as u32;
                let count = read_u16(d, r.start + n)? as usize;
                let mut to = Vec::with_capacity(count.min(4096));
                for k in 0..count {
                    to.push(read_n(d, r.start + n + 2 + k * n, n)? as u32);
                }
                self.refs.push((r.kind, from, to));
            }
        }
        if let Some(b) = find(&kids, b"iprp") {
            self.parse_iprp(b)?;
        }
        Ok(())
    }

    fn parse_iloc(&mut self, b: BoxRef) -> Result<(), Error> {
        let d = self.data;
        let v = *d.get(b.start).ok_or(BAD)?;
        let mut o = b.start + 4;
        let s1 = *d.get(o).ok_or(BAD)?;
        let s2 = *d.get(o + 1).ok_or(BAD)?;
        let (off_size, len_size, base_size) =
            ((s1 >> 4) as usize, (s1 & 15) as usize, (s2 >> 4) as usize);
        let idx_size = if v >= 1 { (s2 & 15) as usize } else { 0 };
        o += 2;
        let count = if v < 2 {
            let c = read_u16(d, o)? as usize;
            o += 2;
            c
        } else {
            let c = read_u32(d, o)? as usize;
            o += 4;
            c
        };
        for _ in 0..count {
            let id = if v < 2 {
                let x = read_u16(d, o)? as u32;
                o += 2;
                x
            } else {
                let x = read_u32(d, o)?;
                o += 4;
                x
            };
            let mut method = 0u8;
            if v >= 1 {
                method = (read_u16(d, o)? & 15) as u8;
                o += 2;
            }
            o += 2; // data_reference_index
            let base = read_n(d, o, base_size)?;
            o += base_size;
            let n = read_u16(d, o)? as usize;
            o += 2;
            let mut extents = Vec::with_capacity(n.min(1024));
            for _ in 0..n {
                o += idx_size;
                let off = read_n(d, o, off_size)?;
                o += off_size;
                let len = read_n(d, o, len_size)?;
                o += len_size;
                extents.push((base.checked_add(off).ok_or(BAD)?, len));
            }
            if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                it.method = method;
                it.extents = extents;
            }
        }
        Ok(())
    }

    fn parse_iprp(&mut self, b: BoxRef) -> Result<(), Error> {
        let d = self.data;
        let kids = children(d, b.start, b.end)?;
        let ipco = find(&kids, b"ipco").ok_or(BAD)?;
        let props = children(d, ipco.start, ipco.end)?;
        for ipma in kids.iter().filter(|k| &k.kind == b"ipma") {
            let v = *d.get(ipma.start).ok_or(BAD)?;
            let flags = read_n(d, ipma.start + 1, 3)?;
            let mut o = ipma.start + 4;
            let count = read_u32(d, o)? as usize;
            o += 4;
            for _ in 0..count {
                let id = if v < 1 {
                    let x = read_u16(d, o)? as u32;
                    o += 2;
                    x
                } else {
                    let x = read_u32(d, o)?;
                    o += 4;
                    x
                };
                let n = *d.get(o).ok_or(BAD)? as usize;
                o += 1;
                for _ in 0..n {
                    let idx = if flags & 1 != 0 {
                        let x = read_u16(d, o)? as usize & 0x7fff;
                        o += 2;
                        x
                    } else {
                        let x = *d.get(o).ok_or(BAD)? as usize & 0x7f;
                        o += 1;
                        x
                    };
                    if idx == 0 || idx > props.len() {
                        continue;
                    }
                    let p = props[idx - 1];
                    if let Some(it) = self.items.iter_mut().find(|i| i.id == id) {
                        apply_property(d, p, &mut it.props)?;
                    }
                }
            }
        }
        Ok(())
    }

    pub fn item(&self, id: u32) -> Option<&Item> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The bytes of an item (its extents joined).
    pub fn item_data(&self, id: u32) -> Result<std::borrow::Cow<'a, [u8]>, Error> {
        let it = self
            .item(id)
            .ok_or(Error::InvalidData("heif: missing item"))?;
        let base: (&'a [u8], usize) = match it.method {
            0 => (self.data, 0),
            1 => {
                let (s, e) = self.idat.ok_or(BAD)?;
                (&self.data[s..e], 0)
            }
            _ => return Err(Error::Unsupported("heif: item construction method")),
        };
        let src = base.0;
        let get = |off: u64, len: u64| -> Result<&'a [u8], Error> {
            let off = usize::try_from(off).map_err(|_| BAD)?;
            let len = if len == 0 {
                src.len().saturating_sub(off)
            } else {
                usize::try_from(len).map_err(|_| BAD)?
            };
            src.get(off..off.checked_add(len).ok_or(BAD)?)
                .ok_or(Error::InvalidData("heif: item data outside the file"))
        };
        match it.extents.len() {
            0 => Err(BAD),
            1 => Ok(std::borrow::Cow::Borrowed(get(
                it.extents[0].0,
                it.extents[0].1,
            )?)),
            _ => {
                let mut v = Vec::new();
                for &(o, l) in &it.extents {
                    v.extend_from_slice(get(o, l)?);
                }
                Ok(std::borrow::Cow::Owned(v))
            }
        }
    }

    fn refs_from(&self, kind: &[u8; 4], from: u32) -> Option<&Vec<u32>> {
        self.refs
            .iter()
            .find(|(k, f, _)| k == kind && *f == from)
            .map(|(_, _, t)| t)
    }

    /// The picture of an item: a coded image or a grid of coded images.
    pub fn picture(&self, id: u32, codec: &[u8; 4]) -> Result<Picture, Error> {
        let it = self
            .item(id)
            .ok_or(Error::InvalidData("heif: missing item"))?;
        match &it.kind {
            k if k == codec => Ok(Picture::Coded(id)),
            b"grid" => {
                let g = self.item_data(id)?;
                if g.len() < 8 {
                    return Err(BAD);
                }
                let large = g[1] & 1 != 0;
                let rows = g[2] as usize + 1;
                let cols = g[3] as usize + 1;
                let (w, h) = if large {
                    (read_u32(&g, 4)?, read_u32(&g, 8)?)
                } else {
                    (read_u16(&g, 4)? as u32, read_u16(&g, 6)? as u32)
                };
                let tiles = self
                    .refs_from(b"dimg", id)
                    .cloned()
                    .ok_or(Error::InvalidData("heif: grid without tiles"))?;
                if tiles.len() != rows * cols {
                    return Err(Error::InvalidData("heif: grid tile count"));
                }
                for &t in &tiles {
                    let k = self.item(t).ok_or(BAD)?.kind;
                    if &k != codec {
                        return Err(Error::Unsupported("heif: grid of other images"));
                    }
                }
                Ok(Picture::Grid {
                    rows,
                    cols,
                    width: w,
                    height: h,
                    tiles,
                })
            }
            b"iden" => {
                let src = self
                    .refs_from(b"dimg", id)
                    .and_then(|v| v.first().copied())
                    .ok_or(BAD)?;
                self.picture(src, codec)
            }
            b"av01" | b"hvc1" => Err(Error::Unsupported("heif: other codec in a grid")),
            _ => Err(Error::Unsupported("heif: image item type")),
        }
    }

    /// The alpha auxiliary image of an item.
    pub fn alpha_of(&self, id: u32) -> Option<u32> {
        self.refs
            .iter()
            .filter(|(k, _, to)| k == b"auxl" && to.contains(&id))
            .map(|(_, from, _)| *from)
            .find(|&a| self.item(a).is_some_and(|i| i.props.alpha))
    }

    /// True when the color of `id` is premultiplied by its alpha.
    pub fn premultiplied(&self, id: u32, alpha: u32) -> bool {
        self.refs_from(b"prem", id)
            .is_some_and(|t| t.contains(&alpha))
    }

    /// The codec of the primary image.
    pub fn codec(&self) -> Option<Codec> {
        if self.track.is_some() && self.item(self.primary).is_none() {
            return Some(Codec::Av1);
        }
        let it = self.item(self.primary)?;
        let kind = match &it.kind {
            b"grid" | b"iden" => {
                let first = self.refs_from(b"dimg", it.id)?.first()?;
                let k = self.item(*first)?.kind;
                if &k == b"iden" || &k == b"grid" {
                    let f2 = self.refs_from(b"dimg", *first)?.first()?;
                    self.item(*f2)?.kind
                } else {
                    k
                }
            }
            k => *k,
        };
        match &kind {
            b"av01" => Some(Codec::Av1),
            b"hvc1" => Some(Codec::Hevc),
            _ => None,
        }
    }

    /// Properties of the primary item (transformations, colour, size).
    pub fn primary_props(&self) -> Props {
        match self.item(self.primary) {
            Some(i) => i.props.clone(),
            None => self.track.as_ref().map(|t| t.0.clone()).unwrap_or_default(),
        }
    }

    /// The bytes of the first track sample.
    pub fn sample(&self) -> Option<&'a [u8]> {
        let (_, o, l) = self.track.as_ref()?;
        self.data.get(*o..o.checked_add(*l)?)
    }
}

fn apply_property(d: &[u8], p: BoxRef, props: &mut Props) -> Result<(), Error> {
    match &p.kind {
        b"ispe" => {
            props.width = read_u32(d, p.start + 4)?;
            props.height = read_u32(d, p.start + 8)?;
        }
        b"av1C" => {
            if p.end >= p.start + 4 {
                props.config = d[p.start + 4..p.end].to_vec();
            }
            props.has_config = true;
        }
        b"hvcC" => {
            // Kept with its box header for writing a single image file.
            let s = p.start - 8;
            props.config = d[s..p.end].to_vec();
            props.has_config = true;
        }
        b"colr" => {
            if d.get(p.start..p.start + 4) == Some(b"nclx") && p.end >= p.start + 11 {
                props.nclx = Some((
                    read_u16(d, p.start + 4)?,
                    read_u16(d, p.start + 6)?,
                    read_u16(d, p.start + 8)?,
                    d[p.start + 10] & 0x80 != 0,
                ));
            }
        }
        b"irot" => props.rotation = *d.get(p.start).ok_or(BAD)? & 3,
        b"imir" => props.mirror = Some(*d.get(p.start).ok_or(BAD)? & 1),
        b"clap" => {
            let mut v = [(0i64, 1i64); 4];
            for (k, r) in v.iter_mut().enumerate() {
                let n = read_u32(d, p.start + k * 8)?;
                let dd = read_u32(d, p.start + k * 8 + 4)? as i64;
                // Widths are unsigned, offsets signed.
                let n = if k < 2 { n as i64 } else { n as i32 as i64 };
                *r = (n, dd);
            }
            props.clap = Some(v);
        }
        b"auxC" => {
            let s = d.get(p.start + 4..p.end).unwrap_or(&[]);
            let urn = s.split(|&b| b == 0).next().unwrap_or(&[]);
            props.alpha = urn == b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha"
                || urn == b"urn:mpeg:hevc:2015:auxid:1";
        }
        _ => {}
    }
    Ok(())
}

/// The first sample of the first video track with an av01 sample entry.
fn parse_moov(d: &[u8], moov: BoxRef) -> Result<Option<(Props, usize, usize)>, Error> {
    for trak in children(d, moov.start, moov.end)?
        .into_iter()
        .filter(|b| &b.kind == b"trak")
    {
        let mdia = match find(&children(d, trak.start, trak.end)?, b"mdia") {
            Some(b) => b,
            None => continue,
        };
        let mk = children(d, mdia.start, mdia.end)?;
        let minf = match find(&mk, b"minf") {
            Some(b) => b,
            None => continue,
        };
        let stbl = match find(&children(d, minf.start, minf.end)?, b"stbl") {
            Some(b) => b,
            None => continue,
        };
        let sk = children(d, stbl.start, stbl.end)?;
        let stsd = find(&sk, b"stsd").ok_or(BAD)?;
        // FullBox + entry_count, then sample entries.
        let entries = children(d, stsd.start + 8, stsd.end)?;
        let entry = match entries.first() {
            Some(e) if &e.kind == b"av01" => *e,
            _ => continue,
        };
        let mut props = Props::default();
        // VisualSampleEntry: 6 reserved + 2 index + 16 predefined, then
        // width and height, then 50 more bytes before the child boxes.
        props.width = read_u16(d, entry.start + 24)? as u32;
        props.height = read_u16(d, entry.start + 26)? as u32;
        for c in children(d, entry.start + 78, entry.end)? {
            apply_property(d, c, &mut props)?;
        }
        let size = match find(&sk, b"stsz") {
            Some(b) => {
                let fixed = read_u32(d, b.start + 4)?;
                if fixed != 0 {
                    fixed as usize
                } else {
                    read_u32(d, b.start + 12)? as usize
                }
            }
            None => continue,
        };
        let offset = if let Some(b) = find(&sk, b"stco") {
            read_u32(d, b.start + 8)? as usize
        } else if let Some(b) = find(&sk, b"co64") {
            usize::try_from(read_n(d, b.start + 8, 8)?).map_err(|_| BAD)?
        } else {
            continue;
        };
        return Ok(Some((props, offset, size)));
    }
    Ok(None)
}
