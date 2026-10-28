//! Lists the media renderers in the network, or plays a URL on one:
//!
//!   cargo run -p bl-upnp --example renderers
//!   cargo run -p bl-upnp --example renderers -- LOCATION URL [MIME]

use bl_upnp::{ssdp, Renderer};
use std::time::Duration;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.len() >= 2 {
        let r = Renderer::fetch(&args[0]).expect("description");
        let mime = args.get(2).map(String::as_str).unwrap_or("audio/wav");
        println!("{} ({}) sinks: {:?}", r.name, r.model, r.sink_protocols());
        let pi = format!("http-get:*:{mime}:*");
        r.set_uri(&args[1], &bl_upnp::didl("Test", "Beatlumen", &args[1], &pi))
            .expect("set uri");
        r.play().expect("play");
        for _ in 0..8 {
            std::thread::sleep(Duration::from_secs(1));
            println!("{:?} {:?}", r.transport_state(), r.position());
        }
        r.stop().expect("stop");
        return;
    }
    for a in ssdp::discover(ssdp::MEDIA_RENDERER, Duration::from_secs(3)).unwrap() {
        match Renderer::fetch(&a.location) {
            Ok(r) => println!("{}  {} / {}  {}", a.location, r.name, r.model, r.udn),
            Err(e) => println!("{}  ({e})", a.location),
        }
    }
}
