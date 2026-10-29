//! Stream description (SDP of the ANNOUNCE request) and track metadata
//! (DMAP items of SET_PARAMETER).

use crate::crypto;

/// Audio format and encryption of an announced stream.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamFormat {
    pub codec: Codec,
    pub sample_rate: u32,
    pub channels: u16,
    /// Frames per packet.
    pub frames_per_packet: u32,
    /// AES key and IV, when the stream is encrypted.
    pub aes: Option<([u8; 16], [u8; 16])>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Codec {
    /// Apple Lossless with its `ALACSpecificConfig`.
    Alac(Vec<u8>),
    /// 16-bit big-endian PCM.
    L16,
}

/// Parses the SDP of an ANNOUNCE request.
pub fn parse_sdp(sdp: &str) -> Result<StreamFormat, &'static str> {
    let mut rtpmap = None;
    let mut fmtp = None;
    let mut rsakey = None;
    let mut iv = None;
    for line in sdp.lines() {
        let line = line.trim();
        if let Some(v) = line.strip_prefix("a=rtpmap:") {
            rtpmap = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("a=fmtp:") {
            fmtp = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("a=rsaaeskey:") {
            rsakey = Some(v.to_string());
        } else if let Some(v) = line.strip_prefix("a=aesiv:") {
            iv = Some(v.to_string());
        }
    }
    let rtpmap = rtpmap.ok_or("no rtpmap")?;
    // "96 AppleLossless" or "96 L16/44100/2"
    let encoding = rtpmap.split_whitespace().nth(1).ok_or("bad rtpmap")?;
    let aes = match (rsakey, iv) {
        (Some(k), Some(v)) => {
            let wrapped = crypto::base64_decode(&k).ok_or("bad rsaaeskey")?;
            let key = crypto::rsa_decrypt_oaep(&wrapped).ok_or("cannot unwrap the AES key")?;
            let iv = crypto::base64_decode(&v).ok_or("bad aesiv")?;
            let key: [u8; 16] = key.try_into().map_err(|_| "bad AES key length")?;
            let iv: [u8; 16] = iv.try_into().map_err(|_| "bad AES IV length")?;
            Some((key, iv))
        }
        _ => None,
    };
    if encoding.eq_ignore_ascii_case("AppleLossless") {
        let fmtp = fmtp.ok_or("no fmtp")?;
        let f: Vec<u32> = fmtp
            .split_whitespace()
            .skip(1)
            .map(|v| v.parse().map_err(|_| "bad fmtp"))
            .collect::<Result<_, _>>()?;
        if f.len() < 11 {
            return Err("short fmtp");
        }
        // frameLength compatibleVersion bitDepth pb mb kb numChannels maxRun
        // maxFrameBytes avgBitRate sampleRate
        let mut cookie = Vec::with_capacity(24);
        cookie.extend_from_slice(&f[0].to_be_bytes());
        for &v in &f[1..7] {
            cookie.push(v as u8);
        }
        cookie.extend_from_slice(&(f[7] as u16).to_be_bytes());
        for &v in &f[8..11] {
            cookie.extend_from_slice(&v.to_be_bytes());
        }
        let cfg = bl_media::AlacConfig::parse(&cookie).map_err(|_| "unsupported ALAC format")?;
        Ok(StreamFormat {
            codec: Codec::Alac(cookie),
            sample_rate: cfg.sample_rate,
            channels: cfg.num_channels as u16,
            frames_per_packet: cfg.frame_length,
            aes,
        })
    } else if let Some(rest) = encoding.strip_prefix("L16/") {
        let mut it = rest.split('/');
        let rate: u32 = it
            .next()
            .and_then(|v| v.parse().ok())
            .ok_or("bad L16 rate")?;
        let channels: u16 = it.next().and_then(|v| v.parse().ok()).unwrap_or(2);
        if !(8000..=192000).contains(&rate) || !(1..=2).contains(&channels) {
            return Err("unsupported L16 format");
        }
        Ok(StreamFormat {
            codec: Codec::L16,
            sample_rate: rate,
            channels,
            frames_per_packet: 352,
            aes,
        })
    } else {
        Err("unsupported codec")
    }
}

/// Title, artist and album from a DMAP listing (`mlit` container).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TrackInfo {
    pub title: String,
    pub artist: String,
    pub album: String,
}

pub fn parse_dmap(data: &[u8]) -> TrackInfo {
    let mut info = TrackInfo::default();
    walk_dmap(data, 0, &mut info);
    info
}

fn walk_dmap(mut data: &[u8], depth: u32, info: &mut TrackInfo) {
    while data.len() >= 8 && depth < 4 {
        let tag = &data[..4];
        let len = u32::from_be_bytes([data[4], data[5], data[6], data[7]]) as usize;
        let Some(value) = data.get(8..8 + len) else {
            return;
        };
        let text = || {
            String::from_utf8_lossy(value)
                .trim()
                .chars()
                .take(512)
                .collect()
        };
        match tag {
            b"mlit" | b"cmst" | b"mdcl" => walk_dmap(value, depth + 1, info),
            b"minm" => info.title = text(),
            b"asar" => info.artist = text(),
            b"asal" => info.album = text(),
            _ => {}
        }
        data = &data[8 + len..];
    }
}

/// `progress: start/current/end` (RTP timestamps) as elapsed and total
/// seconds.
pub fn parse_progress(text: &str, rate: u32) -> Option<(f64, f64)> {
    let v = text.trim().strip_prefix("progress:")?.trim();
    let mut it = v.split('/').map(|x| x.trim().parse::<u32>().ok());
    let (start, cur, end) = (it.next()??, it.next()??, it.next()??);
    let r = rate.max(1) as f64;
    Some((
        cur.wrapping_sub(start) as f64 / r,
        end.wrapping_sub(start) as f64 / r,
    ))
}

/// AirPlay volume (-30 dB … 0 dB, -144 = mute) as a linear gain.
pub fn volume_gain(text: &str) -> Option<f32> {
    let db: f32 = text.trim().strip_prefix("volume:")?.trim().parse().ok()?;
    if !db.is_finite() || db <= -30.0 {
        return Some(0.0);
    }
    Some(10f32.powf(db.min(0.0) / 20.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn alac_sdp() {
        let sdp = "v=0\r\no=iTunes 3413821438 0 IN IP4 192.168.1.2\r\ns=iTunes\r\nc=IN IP4 192.168.1.3\r\n\
            t=0 0\r\nm=audio 0 RTP/AVP 96\r\na=rtpmap:96 AppleLossless\r\n\
            a=fmtp:96 352 0 16 40 10 14 2 255 0 0 44100\r\n";
        let f = parse_sdp(sdp).unwrap();
        assert_eq!(f.sample_rate, 44100);
        assert_eq!(f.channels, 2);
        assert_eq!(f.frames_per_packet, 352);
        assert!(f.aes.is_none());
        let Codec::Alac(cookie) = f.codec else {
            panic!()
        };
        assert_eq!(cookie.len(), 24);
        assert!(parse_sdp("a=rtpmap:96 mpeg4-generic/44100/2\r\n").is_err());
        assert!(parse_sdp("a=rtpmap:96 AppleLossless\r\na=fmtp:96 352 0 16\r\n").is_err());
    }

    #[test]
    fn dmap_and_parameters() {
        fn item(tag: &[u8], v: &[u8]) -> Vec<u8> {
            let mut o = tag.to_vec();
            o.extend_from_slice(&(v.len() as u32).to_be_bytes());
            o.extend_from_slice(v);
            o
        }
        let mut inner = item(b"minm", b"Song");
        inner.extend(item(b"asar", b"Artist"));
        inner.extend(item(b"asal", b"Album"));
        inner.extend(item(b"astm", &[0, 0, 1, 0]));
        let data = item(b"mlit", &inner);
        let info = parse_dmap(&data);
        assert_eq!(info.title, "Song");
        assert_eq!(info.artist, "Artist");
        assert_eq!(info.album, "Album");
        // Truncated data is ignored, not read past.
        assert_eq!(parse_dmap(&data[..data.len() - 3]), TrackInfo::default());

        let (e, t) = parse_progress("progress: 1000/45100/442000\r\n", 44100).unwrap();
        assert!((e - 1.0).abs() < 1e-9 && (t - 10.0).abs() < 1e-9);
        assert_eq!(volume_gain("volume: -144.0"), Some(0.0));
        assert_eq!(volume_gain("volume: 0.000000"), Some(1.0));
        assert!((volume_gain("volume: -20").unwrap() - 0.1).abs() < 1e-6);
    }
}
