//! What happens to the samples on their way out, and whether that may be
//! called bit-perfect. The verdict comes from what actually opened and what
//! is actually in the way, never from the switch that asked for it.

use crate::core::Core;
use crate::host::{DirectOpened, Host};
use crate::player::{PlayState, TrackInfo};
use bl_json::Value;

/// What is known about the moment.
pub(crate) struct Facts<'a> {
    pub track: TrackInfo,
    /// The name of the sound card in direct use.
    pub card: Option<&'a str>,
    /// The name of the speaker in the network that plays instead.
    pub speaker: Option<&'a str>,
    /// Muted counts as a volume of 0.
    pub volume: f32,
    pub equalizer: bool,
    pub normalize: bool,
    pub crossfade: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Verdict {
    /// Every sample reaches the card with the value it has in the file.
    BitPerfect,
    /// Nothing is done to a source without exact samples (MP3, floats).
    Unprocessed,
    /// A card in direct use, but something changes the samples.
    Changed,
    /// The system's mixer decides what happens after us.
    Shared,
    /// A speaker in the network.
    Network,
}

impl Verdict {
    fn name(self) -> &'static str {
        match self {
            Verdict::BitPerfect => "bitperfect",
            Verdict::Unprocessed => "unprocessed",
            Verdict::Changed => "changed",
            Verdict::Shared => "shared",
            Verdict::Network => "network",
        }
    }
}

/// Something that changes the samples.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Change {
    Resampled { from: u32, to: u32 },
    Downmixed { channels: u16 },
    Narrowed { from: u8, to: u8 },
    Volume,
    Equalizer,
    Normalized,
    Crossfade,
}

impl Change {
    fn to_json(self) -> Value {
        match self {
            Change::Resampled { from, to } => Value::object()
                .with("kind", "resampled")
                .with("from", from)
                .with("to", to),
            Change::Downmixed { channels } => Value::object()
                .with("kind", "downmixed")
                .with("channels", channels as u32),
            Change::Narrowed { from, to } => Value::object()
                .with("kind", "narrowed")
                .with("from", from as u32)
                .with("to", to as u32),
            Change::Volume => Value::object().with("kind", "volume"),
            Change::Equalizer => Value::object().with("kind", "equalizer"),
            Change::Normalized => Value::object().with("kind", "normalized"),
            Change::Crossfade => Value::object().with("kind", "crossfade"),
        }
    }
}

/// Bits a float carries exactly on the way through the decoder.
const FLOAT_BITS: u8 = 24;

pub(crate) fn assess(f: &Facts) -> (Verdict, Vec<Change>) {
    let mut changes = Vec::new();
    let t = &f.track;
    if t.out_rate != t.sample_rate {
        changes.push(Change::Resampled {
            from: t.sample_rate,
            to: t.out_rate,
        });
    }
    if t.channels > 2 {
        changes.push(Change::Downmixed {
            channels: t.channels,
        });
    }
    if f.speaker.is_none() && f.volume < 1.0 {
        changes.push(Change::Volume);
    }
    let direct = f.speaker.is_none() && f.card.is_some();
    if direct {
        if let Some(bits) = t.bits {
            let room = FLOAT_BITS.min(t.opened.map_or(FLOAT_BITS, |o| o.format.bits()));
            if bits > room {
                changes.push(Change::Narrowed {
                    from: bits,
                    to: room,
                });
            }
        }
    } else {
        // On a card in direct use none of these run.
        if f.equalizer {
            changes.push(Change::Equalizer);
        }
        if f.normalize {
            changes.push(Change::Normalized);
        }
        if f.crossfade {
            changes.push(Change::Crossfade);
        }
    }
    let verdict = if f.speaker.is_some() {
        Verdict::Network
    } else if !direct {
        Verdict::Shared
    } else if !changes.is_empty() {
        Verdict::Changed
    } else if t.bits.is_some() {
        Verdict::BitPerfect
    } else {
        Verdict::Unprocessed
    };
    (verdict, changes)
}

/// The route for the interface.
pub(crate) fn to_json(f: &Facts) -> Value {
    let (verdict, changes) = assess(f);
    let t = &f.track;
    let mut v = Value::object()
        .with("verdict", verdict.name())
        .with("codec", t.codec)
        .with("rate", t.sample_rate)
        .with("channels", t.channels as u32)
        .with("outRate", t.out_rate)
        .with(
            "changes",
            changes.iter().map(|c| c.to_json()).collect::<Vec<_>>(),
        );
    if let Some(bits) = t.bits {
        v = v.with("bits", bits as u32);
    }
    if let Some(DirectOpened {
        format, channels, ..
    }) = t.opened
    {
        v = v
            .with("format", format.name())
            .with("cardChannels", channels);
    }
    if let Some(card) = f.card {
        v = v.with("card", card);
    }
    if let Some(speaker) = f.speaker {
        v = v.with("speaker", speaker);
    }
    v
}

impl Core {
    /// Follows the settings: a sound card in direct use, unless a speaker in
    /// the network plays. What is playing goes on where it was.
    pub(crate) fn apply_direct(&mut self, host: &mut dyn Host) {
        let target = if self.outputs.speaker_name().is_some() {
            None
        } else {
            self.settings.direct_device()
        };
        self.switch_direct(host, target);
    }

    /// The normal path, for a speaker that is about to take over.
    pub(crate) fn direct_for_speaker(&mut self, host: &mut dyn Host) {
        self.switch_direct(host, None);
    }

    fn switch_direct(&mut self, host: &mut dyn Host, target: Option<String>) {
        if self.player.direct_device() == target.as_deref() {
            return;
        }
        let state = self.player.state();
        let position = self.player.position(host);
        let foreign = self.playing_airplay() || self.playing_dlna() || self.playing_capture();
        self.player.set_direct(host, target);
        if state != PlayState::Stopped && self.current.is_some() && !foreign {
            self.play_current(host, state == PlayState::Playing);
            self.player.seek(host, position);
        }
    }

    /// The card cannot be used (busy, gone, or somebody with a better claim
    /// asked for it). Playback stops rather than go elsewhere unnoticed; play
    /// tries again from the same place.
    pub(crate) fn direct_failed(&mut self, host: &mut dyn Host, error: String) {
        let position = self.player.position(host);
        self.player.stop(host);
        if self.current.is_some() {
            self.resume_at = Some(position);
        }
        let error = self.tr(&error).to_string();
        self.toast(error.clone(), true);
        self.direct_error = Some(error);
    }

    pub(crate) fn audio_json(&self, host: &dyn Host) -> Value {
        let mut v = Value::object()
            .with("directAvailable", host.has_direct())
            .with("directActive", self.player.direct_active());
        if let Some(e) = &self.direct_error {
            v = v.with("directError", e.as_str());
        }
        if let Some(track) = self.player.track_info() {
            let s = &self.settings;
            let facts = Facts {
                track,
                card: self
                    .player
                    .direct_active()
                    .then_some(s.audio_direct_name.as_str()),
                speaker: self.outputs.speaker_name(),
                volume: if s.muted { 0.0 } else { s.volume },
                equalizer: s.eq.active(),
                normalize: s.normalize,
                crossfade: s.crossfade > 0.0,
            };
            v = v.with("route", to_json(&facts));
        }
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bl_media::PcmFormat;

    fn opened(rate: u32, format: PcmFormat) -> Option<DirectOpened> {
        Some(DirectOpened {
            rate,
            format,
            channels: 2,
        })
    }

    fn track(rate: u32, out_rate: u32, bits: Option<u8>, format: PcmFormat) -> TrackInfo {
        TrackInfo {
            duration: None,
            sample_rate: rate,
            channels: 2,
            codec: "flac",
            bits,
            out_rate,
            opened: opened(out_rate, format),
        }
    }

    fn facts(track: TrackInfo) -> Facts<'static> {
        Facts {
            track,
            card: Some("DAC"),
            speaker: None,
            volume: 1.0,
            equalizer: false,
            normalize: false,
            crossfade: false,
        }
    }

    #[test]
    fn only_a_clean_path_is_called_bit_perfect() {
        let good = track(96000, 96000, Some(24), PcmFormat::S24Packed);
        assert_eq!(assess(&facts(good)), (Verdict::BitPerfect, vec![]));
        // 16 bits in a 32-bit word keep every value.
        let wide = track(44100, 44100, Some(16), PcmFormat::S32);
        assert_eq!(assess(&facts(wide)), (Verdict::BitPerfect, vec![]));
    }

    #[test]
    fn a_card_that_refuses_the_rate_is_named() {
        let t = track(44100, 48000, Some(16), PcmFormat::S32);
        assert_eq!(
            assess(&facts(t)),
            (
                Verdict::Changed,
                vec![Change::Resampled {
                    from: 44100,
                    to: 48000
                }]
            )
        );
    }

    #[test]
    fn the_volume_ends_it_and_muting_counts() {
        let mut f = facts(track(44100, 44100, Some(16), PcmFormat::S16));
        f.volume = 0.99;
        assert_eq!(assess(&f), (Verdict::Changed, vec![Change::Volume]));
        f.volume = 0.0;
        assert_eq!(assess(&f).0, Verdict::Changed);
    }

    #[test]
    fn lost_bits_are_named() {
        // A 24-bit file on a card that only takes 16.
        let t = track(44100, 44100, Some(24), PcmFormat::S16);
        assert_eq!(
            assess(&facts(t)),
            (
                Verdict::Changed,
                vec![Change::Narrowed { from: 24, to: 16 }]
            )
        );
        // A 32-bit file: the decoder's floats keep 24.
        let t = track(44100, 44100, Some(32), PcmFormat::S32);
        assert_eq!(
            assess(&facts(t)),
            (
                Verdict::Changed,
                vec![Change::Narrowed { from: 32, to: 24 }]
            )
        );
    }

    #[test]
    fn lossy_files_are_never_bit_perfect() {
        let t = track(44100, 44100, None, PcmFormat::S32);
        assert_eq!(assess(&facts(t)), (Verdict::Unprocessed, vec![]));
    }

    #[test]
    fn more_than_two_channels_are_mixed_down() {
        let mut t = track(48000, 48000, Some(24), PcmFormat::S32);
        t.channels = 6;
        assert_eq!(
            assess(&facts(t)),
            (Verdict::Changed, vec![Change::Downmixed { channels: 6 }])
        );
    }

    #[test]
    fn the_mixer_is_never_exact_and_lists_what_else_is_on() {
        let mut t = track(44100, 48000, Some(16), PcmFormat::S16);
        t.opened = None;
        let mut f = facts(t);
        f.card = None;
        f.equalizer = true;
        f.normalize = true;
        let (verdict, changes) = assess(&f);
        assert_eq!(verdict, Verdict::Shared);
        assert!(changes.contains(&Change::Equalizer));
        assert!(changes.contains(&Change::Normalized));
        assert!(changes.contains(&Change::Resampled {
            from: 44100,
            to: 48000
        }));
        // Same rate, nothing on: still not called exact.
        let mut t = track(48000, 48000, Some(16), PcmFormat::S16);
        t.opened = None;
        let mut f = facts(t);
        f.card = None;
        assert_eq!(assess(&f), (Verdict::Shared, vec![]));
    }

    #[test]
    fn switches_for_other_paths_do_not_count_on_the_card() {
        let mut f = facts(track(44100, 44100, Some(16), PcmFormat::S16));
        f.equalizer = true;
        f.normalize = true;
        f.crossfade = true;
        assert_eq!(assess(&f), (Verdict::BitPerfect, vec![]));
    }

    #[test]
    fn a_speaker_in_the_network_is_its_own_case() {
        let mut f = facts(track(44100, 44100, Some(16), PcmFormat::S16));
        f.speaker = Some("Kitchen");
        assert_eq!(assess(&f).0, Verdict::Network);
    }
}
