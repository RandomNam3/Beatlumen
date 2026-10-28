//! The sender against the receiver of this crate: encrypted and plain
//! streams, a password, track information and volume.

use bl_airplay::sender::{Options, Sender, FRAMES_PER_PACKET, SAMPLE_RATE};
use bl_airplay::{Config, Event, Receiver};
use bl_media::Decoder;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

fn receiver(password: Option<&str>) -> Receiver {
    Receiver::start(Config {
        name: "Test".into(),
        id: [1, 2, 3, 4, 5, 6],
        password: password.map(String::from),
    })
    .unwrap()
}

/// Streams `secs` of a 440 Hz tone (left) and 660 Hz (right), paced like
/// the app does, and returns what the receiver decoded.
fn stream(rx: &Receiver, tx: &mut Sender, secs: f64) -> Vec<f32> {
    let total = (secs * SAMPLE_RATE as f64) as usize;
    let mut n = 0usize;
    let mut decoded = Vec::new();
    let mut decoder = None;
    let start = Instant::now();
    while n < total || start.elapsed() < Duration::from_secs_f64(secs + 2.5) {
        if n < total {
            let due = tx.frames_due(SAMPLE_RATE / 4).min(total - n);
            let mut pcm = Vec::with_capacity(due * 2);
            for i in n..n + due {
                let t = i as f32 / SAMPLE_RATE as f32;
                pcm.push(((t * 440.0 * std::f32::consts::TAU).sin() * 12000.0) as i16);
                pcm.push(((t * 660.0 * std::f32::consts::TAU).sin() * 8000.0) as i16);
            }
            tx.send(&pcm).unwrap();
            n += due;
        }
        if decoder.is_none() && rx.stream().is_active() {
            decoder = Some(rx.stream().decoder());
        }
        if let Some(d) = decoder.as_mut() {
            let mut out = Vec::new();
            let _ = d.next_chunk(&mut out);
            decoded.extend(out);
        } else {
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    decoded
}

/// Peak level of the left channel and its zero crossings per second.
fn analyse(decoded: &[f32]) -> (f32, f64) {
    let left: Vec<f32> = decoded.chunks(2).map(|c| c[0]).collect();
    let loud: Vec<f32> = left.iter().copied().filter(|v| v.abs() > 0.01).collect();
    let peak = loud.iter().fold(0.0f32, |m, v| m.max(v.abs()));
    let crossings = loud
        .windows(2)
        .filter(|w| (w[0] < 0.0) != (w[1] < 0.0))
        .count();
    (
        peak,
        crossings as f64 / (loud.len().max(1) as f64 / SAMPLE_RATE as f64) / 2.0,
    )
}

#[test]
fn encrypted_stream_with_track_info() {
    let rx = receiver(None);
    let addr: SocketAddr = format!("127.0.0.1:{}", rx.port()).parse().unwrap();
    let mut tx = Sender::connect(
        addr,
        "0,1",
        &Options {
            encrypt: true,
            name: "Beatlumen test".into(),
            ..Default::default()
        },
    )
    .unwrap();
    tx.set_volume(1.0).unwrap();
    tx.set_metadata("Lied", "Band", "Album").unwrap();
    tx.set_progress(0.0, 3.0).unwrap();
    let decoded = stream(&rx, &mut tx, 3.0);
    let (peak, freq) = analyse(&decoded);
    assert!((peak - 12000.0 / 32768.0).abs() < 0.03, "peak {peak}");
    assert!((freq - 440.0).abs() < 10.0, "frequency {freq}");
    let events = rx.poll();
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Track(t) if t.title == "Lied" && t.artist == "Band")),
        "{events:?}"
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Volume(v) if *v > 0.9)),
        "{events:?}"
    );
}

#[test]
fn plain_stream_after_flush() {
    let rx = receiver(None);
    let addr: SocketAddr = format!("127.0.0.1:{}", rx.port()).parse().unwrap();
    let mut tx = Sender::connect(addr, "0", &Options::default()).unwrap();
    tx.set_volume(1.0).unwrap();
    let _ = stream(&rx, &mut tx, 1.0);
    tx.flush().unwrap();
    // Sending again restarts the clock.
    assert_eq!(tx.frames_due(1000), FRAMES_PER_PACKET * 2);
    let decoded = stream(&rx, &mut tx, 1.5);
    let (_, freq) = analyse(&decoded);
    assert!((freq - 440.0).abs() < 10.0, "frequency {freq}");
}

#[test]
fn password() {
    let rx = receiver(Some("geheim"));
    let addr: SocketAddr = format!("127.0.0.1:{}", rx.port()).parse().unwrap();
    let wrong = Sender::connect(
        addr,
        "0,1",
        &Options {
            password: Some("falsch".into()),
            ..Default::default()
        },
    );
    assert!(
        matches!(wrong, Err(bl_airplay::sender::Error::Auth)),
        "{:?}",
        wrong.err()
    );
    let mut tx = Sender::connect(
        addr,
        "0,1",
        &Options {
            password: Some("geheim".into()),
            encrypt: true,
            ..Default::default()
        },
    )
    .unwrap();
    tx.set_volume(1.0).unwrap();
    let decoded = stream(&rx, &mut tx, 1.0);
    assert!(analyse(&decoded).0 > 0.3);
}
