//! The server's clock as seen from here: everything that plays together goes by
//! it. An answer is as old as half its way, so each sample is corrected by the
//! round trip, and the sample with the shortest round trip counts: the one with
//! the least room for error.

use std::collections::VecDeque;

/// Samples to choose from.
const KEEP: usize = 8;

#[derive(Default)]
pub(crate) struct ServerClock {
    /// (round trip, server clock minus this device's), in milliseconds.
    samples: VecDeque<(f64, f64)>,
    /// A request is on its way.
    pub asking: bool,
    /// When to ask next (the host's clock, seconds).
    pub next_at: f64,
}

impl ServerClock {
    /// `sent` and `received` by this device's clock, `server` from the answer.
    pub fn add(&mut self, sent: f64, received: f64, server: f64) {
        let round_trip = received - sent;
        if round_trip < 0.0 {
            // This device's clock was changed: what we have is worth nothing.
            self.samples.clear();
            return;
        }
        if self.samples.len() == KEEP {
            self.samples.pop_front();
        }
        self.samples
            .push_back((round_trip, server - (sent + received) / 2.0));
    }

    fn best(&self) -> Option<&(f64, f64)> {
        self.samples.iter().min_by(|a, b| a.0.total_cmp(&b.0))
    }

    pub fn offset(&self) -> Option<f64> {
        self.best().map(|s| s.1)
    }

    pub fn round_trip(&self) -> Option<f64> {
        self.best().map(|s| s.0)
    }

    pub fn samples(&self) -> usize {
        self.samples.len()
    }
}

/// What to do about being out of step with the others.
#[derive(Debug, PartialEq)]
pub(crate) enum Correction {
    Nothing,
    /// A step too small to hear (seconds: forwards if positive).
    Nudge(f64),
    Seek,
}

/// Close enough not to be heard.
const IN_STEP: f64 = 0.015;
/// Beyond this a step would be heard anyway, and take too long.
const SEEK_FROM: f64 = 0.25;
/// Where the player cannot take small steps: a seek is heard, so not too soon.
const SEEK_WITHOUT_STEPS: f64 = 0.08;
const MAX_STEP: f64 = 0.02;

/// `ahead`: how far playback is ahead of where it should be (seconds).
pub(crate) fn correction(ahead: f64, can_nudge: bool) -> Correction {
    let off = ahead.abs();
    if off < IN_STEP {
        Correction::Nothing
    } else if off >= SEEK_FROM || (!can_nudge && off >= SEEK_WITHOUT_STEPS) {
        Correction::Seek
    } else if can_nudge {
        Correction::Nudge((-ahead).clamp(-MAX_STEP, MAX_STEP))
    } else {
        Correction::Nothing
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn half_the_round_trip_is_taken_into_account() {
        let mut c = ServerClock::default();
        assert_eq!(c.offset(), None);
        // Sent at 1000, answered at 1100; the server's clock said 5050 half-way.
        c.add(1000.0, 1100.0, 5050.0);
        assert_eq!(c.offset(), Some(4000.0));
        assert_eq!(c.round_trip(), Some(100.0));
    }

    #[test]
    fn the_shortest_round_trip_counts() {
        let mut c = ServerClock::default();
        // Held up on the way back: looks like a much smaller offset.
        c.add(1000.0, 1600.0, 5010.0);
        assert_eq!(c.offset(), Some(3710.0));
        c.add(2000.0, 2020.0, 6010.0);
        assert_eq!(c.offset(), Some(4000.0));
        c.add(3000.0, 3400.0, 7100.0);
        assert_eq!(c.offset(), Some(4000.0), "a slower answer does not count");
    }

    #[test]
    fn only_the_last_samples_are_kept() {
        let mut c = ServerClock::default();
        c.add(0.0, 1.0, 100.5);
        for i in 0..KEEP {
            c.add(
                i as f64 * 1000.0,
                i as f64 * 1000.0 + 50.0,
                i as f64 * 1000.0 + 225.0,
            );
        }
        assert_eq!(c.samples(), KEEP);
        assert_eq!(c.offset(), Some(200.0), "the old best one has gone");
    }

    #[test]
    fn a_clock_set_back_starts_over() {
        let mut c = ServerClock::default();
        c.add(1000.0, 1100.0, 5050.0);
        c.add(2000.0, 1500.0, 5050.0);
        assert_eq!(c.offset(), None);
    }

    #[test]
    fn what_to_do_about_drift() {
        assert_eq!(correction(0.0, true), Correction::Nothing);
        assert_eq!(correction(0.01, true), Correction::Nothing);
        assert_eq!(correction(-0.01, true), Correction::Nothing);
        // Ahead: holds back; behind: goes forwards, in small steps.
        assert_eq!(correction(0.018, true), Correction::Nudge(-0.018));
        assert_eq!(correction(-0.018, true), Correction::Nudge(0.018));
        assert_eq!(correction(0.1, true), Correction::Nudge(-MAX_STEP));
        assert_eq!(correction(-0.2, true), Correction::Nudge(MAX_STEP));
        assert_eq!(correction(0.3, true), Correction::Seek);
        assert_eq!(correction(-5.0, true), Correction::Seek);
        // Without small steps only what is clearly heard.
        assert_eq!(correction(0.05, false), Correction::Nothing);
        assert_eq!(correction(0.1, false), Correction::Seek);
    }
}
