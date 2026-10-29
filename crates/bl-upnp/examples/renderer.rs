//! Runs a media renderer that only prints what control points ask for:
//!
//!   cargo run -p bl-upnp --example renderer -- [NAME]

use bl_upnp::renderer::{Command, Config, Renderer, State, Status};
use std::time::Duration;

fn main() {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Beatlumen test".into());
    let r = Renderer::start(Config {
        name,
        udn: "uuid:5e1f0c2a-0000-4000-8000-0123456789ab".into(),
        sink_protocols: vec![
            "http-get:*:audio/mpeg:*".into(),
            "http-get:*:audio/flac:*".into(),
        ],
    })
    .expect("start");
    println!("http://127.0.0.1:{}/description.xml", r.port());
    let mut status = Status {
        volume: 40,
        ..Default::default()
    };
    loop {
        for c in r.poll() {
            println!("{c:?}");
            match c {
                Command::SetUri { .. } => status.state = State::Stopped,
                Command::Play => status.state = State::Playing,
                Command::Pause => status.state = State::Paused,
                Command::Stop => status.state = State::Stopped,
                Command::Seek(t) => status.position = t,
                Command::Volume(v) => status.volume = v,
                Command::Mute(m) => status.mute = m,
            }
        }
        if status.state == State::Playing {
            status.position += 0.1;
        }
        r.set_status(status.clone());
        std::thread::sleep(Duration::from_millis(100));
    }
}
