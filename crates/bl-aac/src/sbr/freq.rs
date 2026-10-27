//! SBR frequency band tables (ISO/IEC 14496-3 4.6.18.3.2): master table,
//! high/low resolution tables, noise floor bands, patches (4.6.18.6.3) and
//! limiter bands.

use super::SbrHeader;

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FreqTables {
    pub k0: usize,
    pub k2: usize,
    /// First SBR band (crossover).
    pub kx: usize,
    /// Number of SBR bands.
    pub m: usize,
    pub f_master: Vec<usize>,
    /// f_tableHigh / f_tableLow (band borders, absolute QMF band indices).
    pub f_high: Vec<usize>,
    pub f_low: Vec<usize>,
    pub f_noise: Vec<usize>,
    /// Patches: (source start band, number of bands).
    pub patches: Vec<(usize, usize)>,
    /// Limiter band borders (absolute QMF band indices).
    pub f_lim: Vec<usize>,
}

impl FreqTables {
    pub fn n_high(&self) -> usize {
        self.f_high.len() - 1
    }
    pub fn n_q(&self) -> usize {
        self.f_noise.len() - 1
    }
    /// Band table for frequency resolution `res` (0 = low, 1 = high).
    pub fn table(&self, res: u8) -> &[usize] {
        if res != 0 {
            &self.f_high
        } else {
            &self.f_low
        }
    }
}

fn round(x: f64) -> i64 {
    x.round() as i64
}

/// Builds all frequency tables for a header at SBR (output) rate `fs`.
pub(crate) fn compute(h: &SbrHeader, fs: u32) -> Option<FreqTables> {
    let fs_f = fs as f64;
    // Start frequency k0.
    let row: [i32; 16] = match fs {
        0..=16000 => [-8, -7, -6, -5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7],
        16001..=22050 => [-5, -4, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13],
        22051..=24000 => [-5, -3, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
        24001..=32000 => [-6, -4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16],
        32001..=64000 => [-4, -2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20],
        _ => [-2, -1, 0, 1, 2, 3, 4, 5, 6, 7, 9, 11, 13, 16, 20, 24],
    };
    let base = if fs < 32000 {
        3000.0
    } else if fs < 64000 {
        4000.0
    } else {
        5000.0
    };
    let start_min = round(base * 128.0 / fs_f);
    let stop_min = round(base * 2.0 * 128.0 / fs_f);
    let k0 = start_min + row[h.start_freq as usize] as i64;
    let k2 = match h.stop_freq {
        14 => (2 * k0).min(64),
        15 => (3 * k0).min(64),
        s => {
            let mut dk: Vec<i64> = (0..13)
                .map(|i| {
                    let a =
                        (stop_min as f64) * (64.0 / stop_min as f64).powf((i + 1) as f64 / 13.0);
                    let b = (stop_min as f64) * (64.0 / stop_min as f64).powf(i as f64 / 13.0);
                    round(a) - round(b)
                })
                .collect();
            dk.sort_unstable();
            (stop_min + dk[..s as usize].iter().sum::<i64>()).min(64)
        }
    };
    if k0 < 1 || k2 <= k0 || k2 > 64 || k0 >= 64 {
        return None;
    }
    let max_bands = if fs <= 32000 {
        48
    } else if fs <= 44100 {
        35
    } else {
        32
    };
    if k2 - k0 > max_bands {
        return None;
    }
    let (k0, k2) = (k0 as usize, k2 as usize);
    let f_master = master_table(h, k0, k2)?;
    let n_master = f_master.len() - 1;
    if h.xover_band as usize >= n_master {
        return None;
    }
    let f_high: Vec<usize> = f_master[h.xover_band as usize..].to_vec();
    let n_high = f_high.len() - 1;
    let kx = f_high[0];
    let m = f_high[n_high] - kx;
    if kx > 32 || kx + m > 64 || m == 0 || n_high == 0 {
        return None;
    }
    let n_low = (n_high + 1) / 2;
    let mut f_low = vec![f_high[0]];
    for i in 1..=n_low {
        f_low.push(f_high[2 * i - (n_high & 1)]);
    }
    let n_q = if h.noise_bands == 0 {
        1
    } else {
        (round(h.noise_bands as f64 * (k2 as f64 / kx as f64).log2()) as usize).max(1)
    };
    if n_q > 5 {
        return None;
    }
    let mut f_noise = vec![f_low[0]];
    let mut i = 0usize;
    for k in 1..=n_q {
        i += (n_low - i) / (n_q + 1 - k);
        f_noise.push(f_low[i]);
    }
    let patches = make_patches(&f_master, k0, kx, m, fs)?;
    let f_lim = limiter_table(h.limiter_bands, &f_low, &patches, kx);
    Some(FreqTables {
        k0,
        k2,
        kx,
        m,
        f_master,
        f_high,
        f_low,
        f_noise,
        patches,
        f_lim,
    })
}

fn master_table(h: &SbrHeader, k0: usize, k2: usize) -> Option<Vec<usize>> {
    let mut f = vec![k0];
    if h.freq_scale == 0 {
        let dk = if h.alter_scale { 2 } else { 1 };
        let num_bands = if dk == 1 {
            2 * ((k2 - k0) / 2)
        } else {
            2 * ((k2 - k0 + 2) / 4)
        };
        if num_bands == 0 {
            return None;
        }
        let mut vdk = vec![dk as i64; num_bands];
        let mut diff = (k2 - k0) as i64 - (num_bands * dk) as i64;
        if diff < 0 {
            let mut k = 0;
            while diff != 0 && k < num_bands {
                vdk[k] -= 1;
                k += 1;
                diff += 1;
            }
        } else if diff > 0 {
            let mut k = num_bands;
            while diff != 0 && k > 0 {
                k -= 1;
                vdk[k] += 1;
                diff -= 1;
            }
        }
        for d in vdk {
            if d <= 0 {
                return None;
            }
            let next = *f.last().unwrap() + d as usize;
            f.push(next);
        }
    } else {
        let bands = match h.freq_scale {
            1 => 12.0,
            2 => 10.0,
            _ => 8.0,
        };
        let warp = if h.alter_scale { 1.3 } else { 1.0 };
        let two_regions = k2 as f64 / k0 as f64 > 2.2449;
        let k1 = if two_regions { 2 * k0 } else { k2 };
        let nb0 = 2 * round(bands * (k1 as f64 / k0 as f64).log2() / 2.0) as usize;
        if nb0 == 0 {
            return None;
        }
        let mut vdk0 = geometric_bands(k0, k1, nb0);
        vdk0.sort_unstable();
        if vdk0[0] <= 0 {
            return None;
        }
        for d in &vdk0 {
            let next = *f.last().unwrap() + *d as usize;
            f.push(next);
        }
        if two_regions {
            let nb1 = 2 * round(bands * (k2 as f64 / k1 as f64).log2() / (2.0 * warp)) as usize;
            if nb1 == 0 {
                return None;
            }
            let mut vdk1 = geometric_bands(k1, k2, nb1);
            let max0 = *vdk0.iter().max().unwrap();
            let min1 = *vdk1.iter().min().unwrap();
            if min1 < max0 {
                vdk1.sort_unstable();
                let change = max0 - vdk1[0];
                let lim = (vdk1[nb1 - 1] - vdk1[0]) / 2;
                let change = change.min(lim);
                vdk1[0] += change;
                vdk1[nb1 - 1] -= change;
            }
            vdk1.sort_unstable();
            for d in vdk1 {
                if d <= 0 {
                    return None;
                }
                let next = *f.last().unwrap() + d as usize;
                f.push(next);
            }
        }
    }
    if *f.last().unwrap() != k2 || f.len() < 2 {
        return None;
    }
    Some(f)
}

/// Band widths of a geometric division of [start, stop] into `n` bands.
fn geometric_bands(start: usize, stop: usize, n: usize) -> Vec<i64> {
    let r = stop as f64 / start as f64;
    (0..n)
        .map(|k| {
            let a = round(start as f64 * r.powf((k + 1) as f64 / n as f64));
            let b = round(start as f64 * r.powf(k as f64 / n as f64));
            a - b
        })
        .collect()
}

fn make_patches(
    f_master: &[usize],
    k0: usize,
    kx: usize,
    m: usize,
    fs: u32,
) -> Option<Vec<(usize, usize)>> {
    let n_master = f_master.len() - 1;
    let goal_sb = round(2.048e6 / fs as f64) as usize;
    let mut k = if goal_sb < kx + m {
        let mut k = 0;
        let mut i = 0;
        while i <= n_master && f_master[i] < goal_sb {
            k = i + 1;
            i += 1;
        }
        k.min(n_master)
    } else {
        n_master
    };
    let mut patches: Vec<(usize, usize)> = Vec::new();
    let mut msb = k0;
    let mut usb = kx;
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 64 {
            return None;
        }
        // Highest master band that fits below the source range.
        let mut j = k + 1;
        let (sb, odd) = loop {
            if j == 0 {
                return None;
            }
            j -= 1;
            let sb = f_master[j];
            let odd = (sb + k0) % 2; // (sb - 2 + k0) mod 2
            if sb + 1 + odd <= k0 + msb {
                break (sb, odd);
            }
        };
        let num = sb.saturating_sub(usb);
        if num > 0 {
            let start = (k0 - odd).checked_sub(num)?;
            patches.push((start, num));
            usb = sb;
            msb = sb;
        } else {
            msb = kx;
        }
        if f_master[k] < sb + 3 {
            k = n_master;
        }
        if sb == kx + m {
            break;
        }
        if patches.len() > 5 {
            return None;
        }
    }
    if patches.len() > 1 && patches.last().map_or(false, |p| p.1 < 3) {
        patches.pop();
    }
    if patches.is_empty() || patches.len() > 5 {
        return None;
    }
    Some(patches)
}

fn limiter_table(
    limiter_bands: u8,
    f_low: &[usize],
    patches: &[(usize, usize)],
    kx: usize,
) -> Vec<usize> {
    let n_low = f_low.len() - 1;
    if limiter_bands == 0 {
        return vec![f_low[0], f_low[n_low]];
    }
    let per_octave = [1.2, 2.0, 3.0][limiter_bands as usize - 1];
    // Patch borders (absolute band indices).
    let mut borders = vec![kx];
    for p in patches {
        let last = *borders.last().unwrap();
        borders.push(last + p.1);
    }
    let mut table: Vec<usize> = f_low.to_vec();
    table.extend_from_slice(&borders[1..borders.len() - 1]);
    table.sort_unstable();
    let is_border = |v: usize| borders.contains(&v);
    let mut k = 1;
    while k < table.len() {
        let n_oct = (table[k] as f64 / table[k - 1] as f64).log2();
        if n_oct * per_octave < 0.49 {
            if table[k] == table[k - 1] {
                table.remove(k);
                continue;
            }
            if is_border(table[k]) {
                if is_border(table[k - 1]) {
                    k += 1;
                } else {
                    table.remove(k - 1);
                }
            } else {
                table.remove(k);
            }
            continue;
        }
        k += 1;
    }
    table
}
