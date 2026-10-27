//! Minimal logging to stderr with UTC timestamps.
//!
//! Lines look like `2026-10-27 17:45:01.123Z INFO  message`. Debug lines are
//! only printed when verbose logging is enabled (`--verbose`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enables or disables debug output.
pub fn set_verbose(on: bool) {
    VERBOSE.store(on, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Converts days since 1970-01-01 to a (year, month, day) civil date
/// (proleptic Gregorian calendar, H. Hinnant's algorithm).
pub fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Current UTC time as `YYYY-MM-DD hh:mm:ss.mmmZ`.
pub fn timestamp() -> String {
    let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
    let secs = d.as_secs();
    let (y, m, day) = civil_from_days((secs / 86_400) as i64);
    let rem = secs % 86_400;
    format!("{y:04}-{m:02}-{day:02} {:02}:{:02}:{:02}.{:03}Z", rem / 3600, rem / 60 % 60, rem % 60, d.subsec_millis())
}

/// Writes one log line (a single `eprintln!` call so lines do not interleave).
pub fn write(level: &str, msg: &str) {
    eprintln!("{} {level:<5} {msg}", timestamp());
}

#[macro_export]
macro_rules! info {
    ($($a:tt)*) => { $crate::log::write("INFO", &format!($($a)*)) };
}

#[macro_export]
macro_rules! warn {
    ($($a:tt)*) => { $crate::log::write("WARN", &format!($($a)*)) };
}

#[macro_export]
macro_rules! error {
    ($($a:tt)*) => { $crate::log::write("ERROR", &format!($($a)*)) };
}

#[macro_export]
macro_rules! debug {
    ($($a:tt)*) => {
        if $crate::log::verbose() {
            $crate::log::write("DEBUG", &format!($($a)*))
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29));
        assert_eq!(civil_from_days(20_753), (2026, 10, 27));
        assert_eq!(civil_from_days(-1), (1969, 12, 31));
        assert_eq!(timestamp().len(), 24);
    }
}
