//! The tempo of the music as it plays and the sync measurement: the user
//! taps along to what they hear; the difference to the predicted beats is
//! the output latency (e.g. Bluetooth), which becomes the sync offset.

use crate::core::Core;
use crate::host::Host;
use bl_json::Value;

/// Taps kept for the measurement.
const MAX_TAPS: usize = 16;
/// Taps needed for a result.
const MIN_TAPS: usize = 6;

impl Core {
    /// The median of the taps' offsets in seconds (heard after predicted).
    fn sync_measured(&self) -> Option<f64> {
        if self.sync_taps.len() < MIN_TAPS {
            return None;
        }
        let mut v = self.sync_taps.clone();
        v.sort_by(|a, b| a.total_cmp(b));
        Some(v[v.len() / 2])
    }

    pub(crate) fn tempo_json(&self) -> Value {
        Value::object()
            .with("bpm", self.beats.bpm().map(|b| (b * 10.0).round() / 10.0))
            .with("confidence", self.beats.confidence())
            .with("taps", self.sync_taps.len() as u64)
            .with("needed", MIN_TAPS as u64)
            .with(
                "measuredMs",
                self.sync_measured().map(|s| (s * 1000.0).round()),
            )
    }

    /// `sync.*` commands.
    pub(crate) fn sync_command(&mut self, host: &mut dyn Host, name: &str) -> Value {
        match name {
            "sync.tap" => {
                let pos = self.player.position(host);
                let Some(bpm) = self.beats.bpm() else {
                    return Value::object().with("error", self.tr("No steady beat yet"));
                };
                let period = 60.0 / bpm;
                let Some(beat) = self.beats.next_beat(pos - period / 2.0) else {
                    return Value::object().with("error", self.tr("No steady beat yet"));
                };
                // Heard minus predicted, within half a beat.
                let off = pos - beat;
                if self.sync_taps.len() >= MAX_TAPS {
                    self.sync_taps.remove(0);
                }
                self.sync_taps.push(off);
                self.revs.settings += 1;
                Value::object().with("taps", self.sync_taps.len() as u64)
            }
            "sync.apply" => {
                let Some(off) = self.sync_measured() else {
                    return Value::object().with("error", self.tr("Tap along a few more beats"));
                };
                // Effects drawn this much later land on the heard beat.
                let ms = (-off * 1000.0).clamp(-500.0, 500.0).round();
                self.set_setting(host, "syncOffsetMs", Value::from(ms));
                self.sync_taps.clear();
                Value::object().with("syncOffsetMs", ms)
            }
            "sync.reset" => {
                self.sync_taps.clear();
                self.revs.settings += 1;
                Value::object().with("ok", true)
            }
            _ => Value::object().with("error", "unknown command"),
        }
    }
}
