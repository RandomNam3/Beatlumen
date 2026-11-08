//! Panics caught at the core's edges: the C interface, worker jobs, the
//! decoder and the network listeners. A bug triggered by crafted data (a
//! file, a server answer, another device) then ends that piece of work with
//! an error instead of taking the app down or leaving it waiting forever.
//!
//! Caught panics are counted process wide; [`Reporter`] tells the user once
//! in a while that something went wrong.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

static COUNT: AtomicU64 = AtomicU64::new(0);
static LAST: Mutex<String> = Mutex::new(String::new());

/// The text of a panic payload.
pub fn message(payload: &(dyn Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

/// Counts a caught panic in `place` (e.g. "decoder").
pub fn record(place: &str, msg: &str) {
    let text = format!("{place}: {msg}");
    eprintln!("Beatlumen: caught a panic in {text}");
    *LAST.lock().unwrap_or_else(|e| e.into_inner()) = text;
    COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Panics caught so far in this process.
pub fn count() -> u64 {
    COUNT.load(Ordering::SeqCst)
}

/// Where and why the latest caught panic happened.
pub fn last() -> String {
    LAST.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Runs `f`; a panic is recorded and gives `None`.
pub fn catch<T>(place: &str, f: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(v) => Some(v),
        Err(p) => {
            record(place, &message(&*p));
            None
        }
    }
}

/// Runs a job that ends in a result; a panic becomes an error result, so
/// whoever waits for the job hears about the end.
pub fn guarded<T>(place: &str, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch(place, f).unwrap_or_else(|| Err(format!("internal error in {place}")))
}

/// Tells the user about caught panics: the first at once, later ones at
/// most every [`Reporter::QUIET`] seconds.
#[derive(Debug, Default)]
pub struct Reporter {
    seen: u64,
    shown_at: Option<f64>,
}

impl Reporter {
    pub const QUIET: f64 = 60.0;

    /// The latest panic to tell about at `now` (seconds), if any.
    pub fn due(&mut self, now: f64) -> Option<String> {
        let n = count();
        if n == self.seen {
            return None;
        }
        if self.shown_at.is_some_and(|t| now - t < Self::QUIET && now >= t) {
            return None;
        }
        self.seen = n;
        self.shown_at = Some(now);
        Some(last())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caught_panics_are_counted_and_reported() {
        let before = count();
        let r: Option<u32> = catch("test", || panic!("boom {}", 7));
        assert!(r.is_none());
        assert!(count() > before);
        assert!(last().contains("boom 7"), "{}", last());
        let e: Result<u8, String> = guarded("job", || panic!("bad"));
        assert_eq!(e.unwrap_err(), "internal error in job");
        assert_eq!(catch("test", || 5), Some(5));

        let mut rep = Reporter::default();
        assert!(rep.due(10.0).is_some());
        assert!(rep.due(11.0).is_none());
        let _ = catch("test", || panic!("again"));
        assert!(rep.due(20.0).is_none(), "quiet for a minute");
        assert!(rep.due(80.0).is_some());
    }
}
