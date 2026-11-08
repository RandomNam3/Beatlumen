//! A minimal HEIF writer for tests: AVIF / HEIC files with grids, alpha
//! auxiliary images and transformation properties, built from coded
//! image data (AV1 OBUs from ffmpeg, or stand-in bytes for HEVC).

#![allow(dead_code)]

fn put_box(out: &mut Vec<u8>, kind: &[u8; 4], body: &[u8]) {
    out.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(body);
}

fn boxed(kind: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut v = Vec::new();
    put_box(&mut v, kind, body);
    v
}

/// The data of an item: its own bytes, or the bytes of an earlier item.
pub enum Data {
    Own(Vec<u8>),
    SameAs(usize),
}

pub struct Item {
    pub kind: [u8; 4],
    pub data: Data,
    /// Property boxes (whole boxes), associated in this order.
    pub props: Vec<Vec<u8>>,
    pub hidden: bool,
}

impl Item {
    pub fn new(kind: &[u8; 4], data: Vec<u8>, props: Vec<Vec<u8>>) -> Item {
        Item {
            kind: *kind,
            data: Data::Own(data),
            props,
            hidden: false,
        }
    }
}

pub fn ispe(w: u32, h: u32) -> Vec<u8> {
    let mut b = vec![0u8; 4];
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    boxed(b"ispe", &b)
}

/// av1C with the sequence header OBU taken from the stream.
pub fn av1c(obus: &[u8]) -> Vec<u8> {
    let mut b = vec![0x81, 0, 0, 0];
    b.extend_from_slice(&sequence_header_obu(obus));
    boxed(b"av1C", &b)
}

/// A stand-in hvcC box (the system decoder of the tests ignores it).
pub fn hvcc_stub() -> Vec<u8> {
    let mut b = vec![1u8];
    b.extend_from_slice(&[0u8; 22]);
    boxed(b"hvcC", &b)
}

pub fn irot(quarter_turns: u8) -> Vec<u8> {
    boxed(b"irot", &[quarter_turns & 3])
}

pub fn imir(axis: u8) -> Vec<u8> {
    boxed(b"imir", &[axis & 1])
}

/// clap with integer width / height and a centre offset in samples
/// (rationals with denominator 2 allow half samples).
pub fn clap(w: u32, h: u32, off_x2: i32, off_y2: i32) -> Vec<u8> {
    let mut b = Vec::new();
    for (n, d) in [(w as i32, 1i32), (h as i32, 1), (off_x2, 2), (off_y2, 2)] {
        b.extend_from_slice(&n.to_be_bytes());
        b.extend_from_slice(&(d as u32).to_be_bytes());
    }
    boxed(b"clap", &b)
}

pub fn nclx(primaries: u16, transfer: u16, matrix: u16, full: bool) -> Vec<u8> {
    let mut b = b"nclx".to_vec();
    b.extend_from_slice(&primaries.to_be_bytes());
    b.extend_from_slice(&transfer.to_be_bytes());
    b.extend_from_slice(&matrix.to_be_bytes());
    b.push(if full { 0x80 } else { 0 });
    boxed(b"colr", &b)
}

pub fn auxc_alpha() -> Vec<u8> {
    let mut b = vec![0u8; 4];
    b.extend_from_slice(b"urn:mpeg:mpegB:cicp:systems:auxiliary:alpha\0");
    boxed(b"auxC", &b)
}

/// The data of a grid item.
pub fn grid(rows: u8, cols: u8, w: u32, h: u32) -> Vec<u8> {
    let mut b = vec![0u8, 1, rows - 1, cols - 1];
    b.extend_from_slice(&w.to_be_bytes());
    b.extend_from_slice(&h.to_be_bytes());
    b
}

/// The OBU stream without temporal delimiters.
pub fn strip_td(obus: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    for (kind, whole) in split(obus) {
        if kind != 2 {
            out.extend_from_slice(whole);
        }
    }
    out
}

fn sequence_header_obu(obus: &[u8]) -> Vec<u8> {
    split(obus)
        .into_iter()
        .find(|(k, _)| *k == 1)
        .map(|(_, w)| w.to_vec())
        .unwrap_or_default()
}

/// (type, whole OBU) of a low overhead bitstream with size fields.
fn split(mut d: &[u8]) -> Vec<(u8, &[u8])> {
    let mut v = Vec::new();
    while !d.is_empty() {
        let h = d[0];
        let kind = (h >> 3) & 15;
        let mut pos = 1 + ((h >> 2) & 1) as usize;
        let mut size = 0usize;
        for i in 0..8 {
            let b = d[pos] as usize;
            pos += 1;
            size |= (b & 0x7f) << (7 * i);
            if b & 0x80 == 0 {
                break;
            }
        }
        let end = pos + size;
        v.push((kind, &d[..end]));
        d = &d[end..];
    }
    v
}

/// A HEIF file: `items` get the ids 1, 2, ..., `primary` is an id,
/// `refs` are (type, from, to) references.
pub fn file(
    brand: &[u8; 4],
    items: &[Item],
    primary: u32,
    refs: &[(&[u8; 4], u32, Vec<u32>)],
) -> Vec<u8> {
    let mut ftyp = brand.to_vec();
    ftyp.extend_from_slice(&0u32.to_be_bytes());
    ftyp.extend_from_slice(b"mif1miaf");
    ftyp.extend_from_slice(brand);
    let ftyp = boxed(b"ftyp", &ftyp);
    // mdat first: offsets are known before the meta box is written.
    let mut mdat = Vec::new();
    let mut extents: Vec<(u32, u32)> = Vec::new();
    let data_start = ftyp.len() as u32 + 8;
    for it in items {
        match &it.data {
            Data::Own(d) => {
                extents.push((data_start + mdat.len() as u32, d.len() as u32));
                mdat.extend_from_slice(d);
            }
            Data::SameAs(i) => extents.push(extents[*i]),
        }
    }
    let mut meta = vec![0u8; 4];
    let mut hdlr = vec![0u8; 8];
    hdlr[4..8].copy_from_slice(b"pict");
    hdlr.extend_from_slice(&[0u8; 13]);
    put_box(&mut meta, b"hdlr", &hdlr);
    let mut pitm = vec![0u8; 4];
    pitm.extend_from_slice(&(primary as u16).to_be_bytes());
    put_box(&mut meta, b"pitm", &pitm);
    let mut iinf = vec![0u8; 4];
    iinf.extend_from_slice(&(items.len() as u16).to_be_bytes());
    for (i, it) in items.iter().enumerate() {
        let mut infe = vec![2u8, 0, 0, it.hidden as u8];
        infe.extend_from_slice(&((i + 1) as u16).to_be_bytes());
        infe.extend_from_slice(&[0, 0]);
        infe.extend_from_slice(&it.kind);
        infe.push(0);
        put_box(&mut iinf, b"infe", &infe);
    }
    put_box(&mut meta, b"iinf", &iinf);
    let mut iloc = vec![1u8, 0, 0, 0, 0x44, 0x00];
    iloc.extend_from_slice(&(items.len() as u16).to_be_bytes());
    for (i, &(o, l)) in extents.iter().enumerate() {
        iloc.extend_from_slice(&((i + 1) as u16).to_be_bytes());
        iloc.extend_from_slice(&[0, 0]); // construction method 0
        iloc.extend_from_slice(&[0, 0]); // data reference index
        iloc.extend_from_slice(&[0, 1]); // one extent
        iloc.extend_from_slice(&o.to_be_bytes());
        iloc.extend_from_slice(&l.to_be_bytes());
    }
    put_box(&mut meta, b"iloc", &iloc);
    if !refs.is_empty() {
        let mut iref = vec![0u8; 4];
        for (k, from, to) in refs {
            let mut r = Vec::new();
            r.extend_from_slice(&(*from as u16).to_be_bytes());
            r.extend_from_slice(&(to.len() as u16).to_be_bytes());
            for t in to {
                r.extend_from_slice(&(*t as u16).to_be_bytes());
            }
            put_box(&mut iref, k, &r);
        }
        put_box(&mut meta, b"iref", &iref);
    }
    // Identical properties are stored once (grids share them); 16-bit
    // indices when there are many.
    let mut uniq: Vec<&Vec<u8>> = Vec::new();
    for it in items {
        for p in &it.props {
            if !uniq.contains(&p) {
                uniq.push(p);
            }
        }
    }
    let wide = uniq.len() > 127;
    let mut ipco = Vec::new();
    for p in &uniq {
        ipco.extend_from_slice(p);
    }
    let mut ipma = vec![0u8, 0, 0, wide as u8];
    ipma.extend_from_slice(&(items.len() as u32).to_be_bytes());
    for (i, it) in items.iter().enumerate() {
        ipma.extend_from_slice(&((i + 1) as u16).to_be_bytes());
        ipma.push(it.props.len() as u8);
        for p in &it.props {
            let idx = uniq.iter().position(|u| *u == p).unwrap() + 1;
            if wide {
                ipma.extend_from_slice(&(0x8000 | idx as u16).to_be_bytes());
            } else {
                ipma.push(0x80 | idx as u8);
            }
        }
    }
    let mut iprp = Vec::new();
    put_box(&mut iprp, b"ipco", &ipco);
    put_box(&mut iprp, b"ipma", &ipma);
    put_box(&mut meta, b"iprp", &iprp);
    let mut out = ftyp;
    put_box(&mut out, b"mdat", &mdat);
    put_box(&mut out, b"meta", &meta);
    out
}
