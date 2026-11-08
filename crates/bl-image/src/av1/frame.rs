//! Frame buffers of the decoder. Samples are kept in rings of rows
//! (absolute row numbers modulo the ring height), so that only the
//! superblock rows the loop filters still need are held. The last row
//! above the superblock row being decoded is also kept before deblocking,
//! as intra prediction uses unfiltered samples.

/// A ring of sample rows.
#[derive(Default)]
pub(crate) struct Ring {
    pub data: Vec<u16>,
    pub stride: usize,
    pub cap: usize,
}

impl Ring {
    pub fn new(stride: usize, cap: usize) -> Ring {
        let cap = cap.max(1);
        Ring {
            data: vec![0; stride * cap],
            stride,
            cap,
        }
    }

    pub fn empty() -> Ring {
        Ring::default()
    }

    #[inline]
    pub fn row(&self, y: usize) -> &[u16] {
        let o = (y % self.cap.max(1)) * self.stride;
        &self.data[o..o + self.stride]
    }

    #[inline]
    pub fn row_mut(&mut self, y: usize) -> &mut [u16] {
        let o = (y % self.cap) * self.stride;
        &mut self.data[o..o + self.stride]
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> u16 {
        self.data[(y % self.cap) * self.stride + x]
    }
}

pub(crate) struct FrameBuf {
    pub planes: usize,
    /// Plane sizes padded to whole 8x8 luma blocks (MiCols * 4 >> ssx).
    pub w: [usize; 3],
    pub h: [usize; 3],
    pub rec: [Ring; 3],
    /// Unfiltered row above the superblock row being decoded.
    pub above: [Vec<u16>; 3],
    /// First row of the superblock row being decoded, per plane.
    pub sb_top: [usize; 3],
}

impl FrameBuf {
    /// `rows`: luma rows to keep.
    pub fn new(planes: usize, w: usize, h: usize, ssx: usize, ssy: usize, rows: usize) -> FrameBuf {
        let mut f = FrameBuf {
            planes,
            w: [0; 3],
            h: [0; 3],
            rec: [Ring::empty(), Ring::empty(), Ring::empty()],
            above: Default::default(),
            sb_top: [0; 3],
        };
        for p in 0..planes {
            let (sx, sy) = if p > 0 { (ssx, ssy) } else { (0, 0) };
            f.w[p] = w >> sx;
            f.h[p] = h >> sy;
            // Blocks reach up to the end of their superblock.
            let stride = (w.div_ceil(128) * 128) >> sx;
            f.rec[p] = Ring::new(stride, rows >> sy);
            f.above[p] = vec![0; stride];
        }
        f
    }

    #[inline]
    pub fn row(&self, p: usize, y: usize) -> &[u16] {
        self.rec[p].row(y)
    }

    #[inline]
    pub fn row_mut(&mut self, p: usize, y: usize) -> &mut [u16] {
        self.rec[p].row_mut(y)
    }

    /// A sample of the current frame before loop filtering.
    #[inline]
    pub fn px(&self, p: usize, x: usize, y: usize) -> u16 {
        if y < self.sb_top[p] {
            self.above[p][x]
        } else {
            self.rec[p].at(x, y)
        }
    }

    /// Keeps the last decoded row before the next superblock row starts
    /// at luma row `next_top`.
    pub fn next_sb_row(&mut self, next_top: usize, ssy: usize) {
        for p in 0..self.planes {
            let sy = if p > 0 { ssy } else { 0 };
            let top = next_top >> sy;
            if top > 0 && top <= self.h[p] {
                let (rec, above) = (&self.rec[p], &mut self.above[p]);
                above.copy_from_slice(rec.row(top - 1));
            }
            self.sb_top[p] = top;
        }
    }
}
