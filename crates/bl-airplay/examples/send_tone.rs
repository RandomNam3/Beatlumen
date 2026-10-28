//! Sends a test tone to an AirPlay 1 receiver, or lists the receivers:
//!
//!   cargo run -p bl-airplay --example send_tone -- HOST:PORT [SECONDS] [--encrypt] [--password PW]
//!   cargo run -p bl-airplay --example send_tone -- --list

use bl_airplay::sender::{Options, Sender, SAMPLE_RATE};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--list") {
        for f in bl_airplay::mdns::browse("_raop._tcp", Duration::from_secs(3)).unwrap() {
            println!("{}:{}  {}  {:?}", f.addr, f.port, f.instance, f.txt);
        }
        return;
    }
    let addr = args.first().expect("HOST:PORT").parse().expect("HOST:PORT");
    let secs: f64 = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(3.0);
    let password = args
        .iter()
        .position(|a| a == "--password")
        .and_then(|i| args.get(i + 1).cloned());
    let opts = Options {
        encrypt: args.iter().any(|a| a == "--encrypt"),
        password,
        name: "Beatlumen".into(),
    };
    let mut tx = Sender::connect(addr, "0,1", &opts).expect("connect");
    tx.set_volume(1.0).expect("volume");
    tx.set_metadata("Test tone", "Beatlumen", "")
        .expect("metadata");
    let total = (secs * SAMPLE_RATE as f64) as usize;
    let mut n = 0;
    while n < total {
        let due = tx.frames_due(SAMPLE_RATE / 4).min(total - n);
        let mut pcm = Vec::with_capacity(due * 2);
        for i in n..n + due {
            let t = i as f32 / SAMPLE_RATE as f32;
            pcm.push(((t * 440.0 * std::f32::consts::TAU).sin() * 12000.0) as i16);
            pcm.push(((t * 660.0 * std::f32::consts::TAU).sin() * 8000.0) as i16);
        }
        tx.send(&pcm).expect("send");
        n += due;
        std::thread::sleep(Duration::from_millis(10));
    }
    std::thread::sleep(Duration::from_secs(3));
    println!("sent {n} frames");
}
