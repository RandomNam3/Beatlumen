//! Helpers for the app's own small network listeners (the live stream for
//! speakers, the sign-in callback): a cap on open connections, a deadline
//! for reading a request and a comparison of secrets that takes the same
//! time wherever they differ.

use std::io::Read;
use std::net::TcpStream;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Places for open connections.
pub(crate) struct Slots {
    open: AtomicUsize,
    max: usize,
}

/// A taken place, given back when dropped (also when its thread panics).
pub(crate) struct Slot(Arc<Slots>);

impl Slots {
    pub(crate) fn new(max: usize) -> Arc<Slots> {
        Arc::new(Slots {
            open: AtomicUsize::new(0),
            max,
        })
    }

    /// A place, or `None` when all are taken.
    pub(crate) fn take(self: &Arc<Self>) -> Option<Slot> {
        if self.open.fetch_add(1, Ordering::SeqCst) >= self.max {
            self.open.fetch_sub(1, Ordering::SeqCst);
            return None;
        }
        Some(Slot(self.clone()))
    }

    #[cfg(test)]
    pub(crate) fn open(&self) -> usize {
        self.open.load(Ordering::SeqCst)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Reads from a connection until a deadline: each read waits at most for
/// what is left, so a client sending a byte now and then cannot hold a
/// connection (and its thread) open.
pub(crate) struct DeadlineReader {
    conn: TcpStream,
    deadline: Instant,
}

impl DeadlineReader {
    pub(crate) fn new(conn: &TcpStream, within: Duration) -> std::io::Result<DeadlineReader> {
        conn.set_nonblocking(false)?;
        Ok(DeadlineReader {
            conn: conn.try_clone()?,
            deadline: Instant::now() + within,
        })
    }
}

impl Read for DeadlineReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(std::io::ErrorKind::TimedOut.into());
        }
        self.conn.set_read_timeout(Some(left))?;
        self.conn.read(buf)
    }
}

/// Compares a received secret with the expected one without stopping at
/// the first difference.
pub(crate) fn same_secret(got: &str, expected: &str) -> bool {
    got.len() == expected.len()
        && got
            .bytes()
            .zip(expected.bytes())
            .fold(0u8, |d, (a, b)| d | (a ^ b))
            == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_given_back() {
        let s = Slots::new(2);
        let a = s.take().unwrap();
        let b = s.take().unwrap();
        assert!(s.take().is_none());
        assert_eq!(s.open(), 2);
        drop(a);
        let c = s.take().unwrap();
        // Also when the thread holding one panics.
        let _ = std::thread::spawn(move || {
            let _c = c;
            panic!("handler bug");
        })
        .join();
        drop(b);
        assert_eq!(s.open(), 0);
    }

    #[test]
    fn secrets() {
        assert!(same_secret("abc", "abc"));
        assert!(!same_secret("abd", "abc"));
        assert!(!same_secret("ab", "abc"));
        assert!(!same_secret("", "abc"));
    }
}
