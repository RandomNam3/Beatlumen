//! `hmtx` table: horizontal advances and left side bearings.

use crate::reader::{i16_at, u16_at};
use crate::sfnt::TableRange;

#[derive(Debug, Clone)]
pub(crate) struct Hmtx {
    range: TableRange,
    num_hmetrics: u16,
}

impl Hmtx {
    pub fn new(range: TableRange, num_hmetrics: u16) -> Hmtx {
        Hmtx { range, num_hmetrics }
    }

    fn table<'a>(&self, data: &'a [u8]) -> &'a [u8] {
        data.get(self.range.clone()).unwrap_or(&[])
    }

    /// Advance width in font units.
    pub fn advance(&self, data: &[u8], gid: u16) -> u16 {
        if self.num_hmetrics == 0 {
            return 0;
        }
        let i = gid.min(self.num_hmetrics - 1) as usize;
        u16_at(self.table(data), 4 * i).unwrap_or(0)
    }

    /// Left side bearing in font units.
    pub fn lsb(&self, data: &[u8], gid: u16) -> i16 {
        let t = self.table(data);
        let nh = self.num_hmetrics as usize;
        let g = gid as usize;
        if g < nh {
            i16_at(t, 4 * g + 2).unwrap_or(0)
        } else {
            i16_at(t, 4 * nh + 2 * (g - nh)).unwrap_or(0)
        }
    }
}
