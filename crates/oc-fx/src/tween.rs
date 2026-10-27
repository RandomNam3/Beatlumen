//! Time based tweens of fixed-size float vectors.

use crate::easing::Easing;

#[derive(Debug, Clone)]
pub struct Tween<const N: usize> {
    pub from: [f32; N],
    pub to: [f32; N],
    /// Engine time at which the tween starts moving (after its delay).
    pub start: f64,
    pub duration: f64,
    pub easing: Easing,
}

impl<const N: usize> Tween<N> {
    pub fn new(from: [f32; N], to: [f32; N], now: f64, delay: f64, duration: f64, easing: Easing) -> Tween<N> {
        Tween { from, to, start: now + delay.max(0.0), duration: duration.max(0.0), easing }
    }

    /// Eased progress in [0, 1] (0 before the start).
    pub fn progress(&self, now: f64) -> f32 {
        if now < self.start {
            0.0
        } else if self.duration <= 0.0 {
            1.0
        } else {
            self.easing.apply(((now - self.start) / self.duration).min(1.0) as f32)
        }
    }

    pub fn has_started(&self, now: f64) -> bool {
        now >= self.start
    }

    pub fn is_finished(&self, now: f64) -> bool {
        now >= self.start + self.duration
    }

    pub fn value(&self, now: f64) -> [f32; N] {
        let t = self.progress(now);
        let mut out = self.from;
        for (o, (a, b)) in out.iter_mut().zip(self.from.iter().zip(&self.to)) {
            *o = a + (b - a) * t;
        }
        out
    }

    pub fn end_time(&self) -> f64 {
        self.start + self.duration
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delay_and_progress() {
        let t = Tween::new([0.0, 10.0], [1.0, 20.0], 1.0, 0.5, 2.0, Easing::Linear);
        assert_eq!(t.value(1.2), [0.0, 10.0]);
        assert_eq!(t.value(2.5), [0.5, 15.0]);
        assert_eq!(t.value(9.0), [1.0, 20.0]);
        assert!(t.is_finished(3.5));
        let z = Tween::new([0.0], [1.0], 0.0, 1.0, 0.0, Easing::Linear);
        assert_eq!(z.value(0.5), [0.0]);
        assert_eq!(z.value(1.0), [1.0]);
    }
}
