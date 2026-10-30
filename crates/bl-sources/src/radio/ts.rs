//! A small MPEG-TS demuxer for HLS audio segments: reads the PAT and PMT,
//! picks the first ADTS AAC (stream type 0x0F) or MPEG audio (0x03, 0x04)
//! stream and reassembles its PES packets into the elementary stream
//! (ADTS or MPEG audio frames, ready for a frame decoder).
//!
//! PAT/PMT sections must fit in one packet (those of audio streams are
//! tiny) and pass their CRC. Scrambled packets, packets with the transport
//! error flag and other streams are skipped; packets are resynchronised on
//! the 0x47 sync byte; a PES packet that misses data (continuity counter)
//! or grows past 1 MiB is dropped.

/// Size of a transport stream packet.
pub const PACKET: usize = 188;
const SYNC: u8 = 0x47;
const MAX_PES: usize = 1 << 20;

#[derive(Debug, Default)]
pub struct TsDemuxer {
    /// The start of a packet split across [`TsDemuxer::push`] calls.
    carry: Vec<u8>,
    pmt_pid: Option<u16>,
    /// PID and stream type of the audio stream.
    audio: Option<(u16, u8)>,
    /// A PMT was read (whether or not it had a supported stream).
    has_pmt: bool,
    pes: Vec<u8>,
    in_pes: bool,
    cc: Option<u8>,
}

/// CRC-32/MPEG-2 (a section including its CRC sums to 0).
fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &b in data {
        crc ^= (b as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// The PSI section starting in this payload (None if it continues in
/// another packet or fails its CRC).
fn section(payload: &[u8], pusi: bool) -> Option<&[u8]> {
    if !pusi {
        return None;
    }
    let pointer = *payload.first()? as usize;
    let s = payload.get(1 + pointer..)?;
    if s.len() < 3 {
        return None;
    }
    let len = (usize::from(s[1] & 0x0f) << 8) | usize::from(s[2]);
    let s = s.get(..3 + len)?;
    (len >= 9 && crc32(s) == 0).then_some(s)
}

impl TsDemuxer {
    pub fn new() -> TsDemuxer {
        TsDemuxer::default()
    }

    /// "aac" or "mp3" once the PMT named a supported audio stream.
    pub fn codec(&self) -> Option<&'static str> {
        match self.audio?.1 {
            0x0f => Some("aac"),
            _ => Some("mp3"),
        }
    }

    /// Whether a PMT was read (a stream without supported audio has one but
    /// no [`TsDemuxer::codec`]).
    pub fn has_pmt(&self) -> bool {
        self.has_pmt
    }

    /// Demuxes `data` (any split of the stream), appending audio to `out`.
    pub fn push(&mut self, data: &[u8], out: &mut Vec<u8>) {
        let joined;
        let d = if self.carry.is_empty() {
            data
        } else {
            let mut v = std::mem::take(&mut self.carry);
            v.extend_from_slice(data);
            joined = v;
            &joined[..]
        };
        let mut i = 0;
        while i + PACKET <= d.len() {
            if d[i] != SYNC {
                // Resynchronise: a sync byte followed by another one a
                // packet later (or the end of the data).
                match (i + 1..d.len())
                    .find(|&j| d[j] == SYNC && d.get(j + PACKET).map_or(true, |&b| b == SYNC))
                {
                    Some(j) => {
                        i = j;
                        continue;
                    }
                    None => {
                        i = d.len();
                        break;
                    }
                }
            }
            self.packet(&d[i..i + PACKET], out);
            i += PACKET;
        }
        self.carry = d[i..].to_vec();
    }

    /// Emits a PES packet still being assembled (at the end of the stream).
    pub fn flush(&mut self, out: &mut Vec<u8>) {
        self.end_pes(out);
    }

    /// Forgets partial packets and PES data (after a discontinuity); the
    /// stream selection stays until the next PMT.
    pub fn reset(&mut self) {
        self.carry.clear();
        self.pes.clear();
        self.in_pes = false;
        self.cc = None;
    }

    fn packet(&mut self, p: &[u8], out: &mut Vec<u8>) {
        if p[1] & 0x80 != 0 || p[3] & 0xc0 != 0 {
            return; // transport error or scrambled
        }
        let pusi = p[1] & 0x40 != 0;
        let pid = (u16::from(p[1] & 0x1f) << 8) | u16::from(p[2]);
        let afc = (p[3] >> 4) & 3;
        let start = if afc & 2 != 0 { 5 + p[4] as usize } else { 4 };
        if afc & 1 == 0 || start >= PACKET {
            return; // no payload
        }
        let payload = &p[start..];
        if pid == 0 {
            self.pat(payload, pusi);
        } else if Some(pid) == self.pmt_pid {
            self.pmt(payload, pusi, out);
        } else if Some(pid) == self.audio.map(|a| a.0) {
            let cc = p[3] & 0x0f;
            let expected = self.cc.map(|c| (c + 1) & 0x0f);
            if self.cc == Some(cc) {
                return; // duplicate packet
            }
            self.cc = Some(cc);
            if expected.is_some_and(|e| e != cc) && !pusi {
                // Data of this PES packet was lost.
                self.pes.clear();
                self.in_pes = false;
                return;
            }
            self.pes_data(payload, pusi, out);
        }
    }

    fn pat(&mut self, payload: &[u8], pusi: bool) {
        let Some(s) = section(payload, pusi) else {
            return;
        };
        if s[0] != 0x00 {
            return;
        }
        for e in s[8..s.len() - 4].chunks_exact(4) {
            let program = u16::from_be_bytes([e[0], e[1]]);
            if program != 0 {
                self.pmt_pid = Some(u16::from_be_bytes([e[2] & 0x1f, e[3]]));
                return;
            }
        }
    }

    fn pmt(&mut self, payload: &[u8], pusi: bool, out: &mut Vec<u8>) {
        let Some(s) = section(payload, pusi) else {
            return;
        };
        if s[0] != 0x02 || s.len() < 16 {
            return;
        }
        let info_len = (usize::from(s[10] & 0x0f) << 8) | usize::from(s[11]);
        let Some(mut es) = s.get(12 + info_len..s.len() - 4) else {
            return;
        };
        let mut found = None;
        while es.len() >= 5 {
            let stream_type = es[0];
            let pid = u16::from_be_bytes([es[1] & 0x1f, es[2]]);
            let info = (usize::from(es[3] & 0x0f) << 8) | usize::from(es[4]);
            if found.is_none() && matches!(stream_type, 0x0f | 0x03 | 0x04) {
                found = Some((pid, stream_type));
            }
            es = es.get(5 + info..).unwrap_or(&[]);
        }
        self.has_pmt = true;
        if found != self.audio {
            self.end_pes(out);
            self.audio = found;
            self.cc = None;
        }
    }

    fn pes_data(&mut self, payload: &[u8], pusi: bool, out: &mut Vec<u8>) {
        if pusi {
            self.end_pes(out);
            self.in_pes = true;
        }
        if !self.in_pes {
            return; // joined in the middle of a PES packet
        }
        if self.pes.len() + payload.len() > MAX_PES {
            self.pes.clear();
            self.in_pes = false;
            return;
        }
        self.pes.extend_from_slice(payload);
        // A PES packet with a length is complete once it has all its bytes.
        if self.pes.len() >= 6 {
            let len = usize::from(u16::from_be_bytes([self.pes[4], self.pes[5]]));
            if len > 0 && self.pes.len() >= 6 + len {
                self.end_pes(out);
            }
        }
    }

    /// Appends the payload of the PES packet being assembled.
    fn end_pes(&mut self, out: &mut Vec<u8>) {
        if !std::mem::take(&mut self.in_pes) {
            self.pes.clear();
            return;
        }
        let p = &self.pes;
        if p.len() >= 9 && p[..3] == [0, 0, 1] {
            let len = usize::from(u16::from_be_bytes([p[4], p[5]]));
            let end = if len == 0 {
                Some(p.len())
            } else {
                Some(6 + len).filter(|&e| e <= p.len())
            };
            // Stream ids without the optional PES header.
            let header = if matches!(p[3], 0xbc | 0xbe | 0xbf | 0xf0 | 0xf1 | 0xf2 | 0xf8 | 0xff) {
                Some(6)
            } else {
                (p[6] & 0xc0 == 0x80).then(|| 9 + p[8] as usize)
            };
            if let (Some(end), Some(header)) = (end, header) {
                if header <= end {
                    out.extend_from_slice(&p[header..end]);
                }
            }
        }
        self.pes.clear();
    }
}

/// Builds transport streams for tests.
#[cfg(test)]
pub(crate) mod testgen {
    use super::*;

    pub const PMT_PID: u16 = 0x100;
    pub const AUDIO_PID: u16 = 0x101;

    pub fn packet(pid: u16, pusi: bool, cc: u8, payload: &[u8]) -> Vec<u8> {
        let payload = &payload[..payload.len().min(184)];
        let mut p = vec![
            0x47,
            ((pusi as u8) << 6) | ((pid >> 8) as u8 & 0x1f),
            pid as u8,
            0,
        ];
        if payload.len() < 184 {
            // Stuffing in an adaptation field.
            let af = 183 - payload.len();
            p[3] = 0x30 | (cc & 0x0f);
            p.push(af as u8);
            if af > 0 {
                p.push(0);
                p.resize(5 + af, 0xff);
            }
        } else {
            p[3] = 0x10 | (cc & 0x0f);
        }
        p.extend_from_slice(payload);
        assert_eq!(p.len(), PACKET);
        p
    }

    pub fn section(table_id: u8, ext: u16, body: &[u8]) -> Vec<u8> {
        let len = 5 + body.len() + 4;
        let mut s = vec![
            table_id,
            0xb0 | ((len >> 8) as u8 & 0x0f),
            len as u8,
            (ext >> 8) as u8,
            ext as u8,
            0xc1,
            0,
            0,
        ];
        s.extend_from_slice(body);
        let crc = crc32(&s);
        s.extend_from_slice(&crc.to_be_bytes());
        s
    }

    pub fn psi(stream_type: u8) -> Vec<u8> {
        let mut pat = vec![0];
        pat.extend(section(
            0,
            1,
            &[0, 1, 0xe0 | (PMT_PID >> 8) as u8, PMT_PID as u8],
        ));
        let mut pmt = vec![0];
        pmt.extend(section(
            2,
            1,
            &[
                0xe0 | (AUDIO_PID >> 8) as u8,
                AUDIO_PID as u8,
                0xf0,
                0,
                // A video stream first: skipped.
                0x1b,
                0xe1,
                0x00,
                0xf0,
                0,
                stream_type,
                0xe0 | (AUDIO_PID >> 8) as u8,
                AUDIO_PID as u8,
                0xf0,
                0,
            ],
        ));
        let mut v = packet(0, true, 0, &pat);
        v.extend(packet(PMT_PID, true, 0, &pmt));
        v
    }

    /// One PES packet per frame (with a length or unbounded), split into
    /// transport packets; `cc` continues the counter.
    pub fn pes(frames: &[Vec<u8>], bounded: bool, cc: &mut u8) -> Vec<u8> {
        let mut v = Vec::new();
        for f in frames {
            let mut pes = vec![0, 0, 1, 0xc0, 0, 0, 0x80, 0x80, 5, 0x21, 0, 1, 0, 1];
            if bounded {
                let len = (8 + f.len()) as u16;
                pes[4..6].copy_from_slice(&len.to_be_bytes());
            }
            pes.extend_from_slice(f);
            for (i, chunk) in pes.chunks(184).enumerate() {
                v.extend(packet(AUDIO_PID, i == 0, *cc, chunk));
                *cc = (*cc + 1) & 0x0f;
            }
        }
        v
    }

    /// A transport stream carrying `frames` as audio of `stream_type`.
    pub fn stream(frames: &[Vec<u8>], stream_type: u8, bounded: bool) -> Vec<u8> {
        let mut v = psi(stream_type);
        let mut cc = 0;
        v.extend(pes(frames, bounded, &mut cc));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::testgen::*;
    use super::*;

    fn frames() -> Vec<Vec<u8>> {
        (0..6u8)
            .map(|i| (0..150 + 97 * i as usize).map(|j| (j as u8) ^ i).collect())
            .collect()
    }

    fn demux_all(ts: &[u8], split: usize) -> (Vec<u8>, TsDemuxer) {
        let mut d = TsDemuxer::new();
        let mut out = Vec::new();
        for chunk in ts.chunks(split) {
            d.push(chunk, &mut out);
        }
        d.flush(&mut out);
        (out, d)
    }

    #[test]
    fn demuxes_aac_and_mpeg_audio() {
        let f = frames();
        let es: Vec<u8> = f.concat();
        for (stream_type, codec) in [(0x0f, "aac"), (0x03, "mp3"), (0x04, "mp3")] {
            for bounded in [true, false] {
                let ts = stream(&f, stream_type, bounded);
                for split in [188, 1000, 7, ts.len()] {
                    let (out, d) = demux_all(&ts, split);
                    assert_eq!(
                        out, es,
                        "type {stream_type} bounded {bounded} split {split}"
                    );
                    assert_eq!(d.codec(), Some(codec));
                }
            }
        }
    }

    #[test]
    fn bounded_pes_is_emitted_without_waiting() {
        let f = frames();
        let ts = stream(&f[..1], 0x0f, true);
        let mut d = TsDemuxer::new();
        let mut out = Vec::new();
        d.push(&ts, &mut out);
        assert_eq!(out, f[0]);
    }

    #[test]
    fn skips_garbage_bad_crc_and_other_streams() {
        let f = frames();
        let mut ts = b"garbage before the stream".to_vec();
        let at = ts.len() + 2 * PACKET; // after the PAT and PMT
        ts.extend(stream(&f, 0x0f, false));
        // Another PID and a scrambled audio packet are ignored.
        let mut scrambled = packet(AUDIO_PID, true, 9, &[0xff; 50]);
        scrambled[3] |= 0x80;
        ts.splice(
            at..at,
            packet(0x200, true, 0, &[1, 2, 3])
                .into_iter()
                .chain(scrambled),
        );
        let (out, _) = demux_all(&ts, 333);
        assert_eq!(out, f.concat());

        // A PMT with a broken CRC selects nothing.
        let mut bad = stream(&f, 0x0f, false);
        bad[2 * PACKET - 1] ^= 0xff; // the PMT's CRC
        let (out, d) = demux_all(&bad, 188);
        assert!(out.is_empty());
        assert_eq!(d.codec(), None);

        // No supported audio stream.
        let (out, d) = demux_all(&stream(&f, 0x81, false), 188);
        assert!(out.is_empty());
        assert!(d.has_pmt() && d.codec().is_none());
    }

    #[test]
    fn lost_packets_drop_their_pes() {
        let f = frames();
        let mut ts = psi(0x0f);
        let mut cc = 0;
        ts.extend(pes(&f[..2], true, &mut cc));
        let lost_from = ts.len() + PACKET; // second packet of the third frame
        ts.extend(pes(&f[2..], true, &mut cc));
        ts.drain(lost_from..lost_from + PACKET);
        let (out, _) = demux_all(&ts, 188);
        let mut expected = f[..2].concat();
        expected.extend(f[3..].concat());
        assert_eq!(out, expected);
    }

    #[test]
    fn hostile_input_does_not_panic() {
        let mut d = TsDemuxer::new();
        let mut out = Vec::new();
        let mut p = vec![0x47u8; PACKET];
        for b in 0..=255u8 {
            p[1] = b & 0x5f;
            p[3] = b;
            p[4] = b;
            p[5] = b;
            d.push(&p, &mut out);
        }
        // A PES packet that never ends is bounded.
        let mut ts = psi(0x0f);
        let mut cc = 0;
        ts.extend(pes(&[vec![0; 3 << 20]], false, &mut cc));
        let (out, _) = demux_all(&ts, 4096);
        assert!(out.is_empty());
    }
}
