//! Test receiver: accepts one AirPlay stream and writes the decoded audio
//! (interleaved f32) to a file; events go to stdout.
//!
//!   cargo run -p bl-airplay --example airplay_receive -- out.f32 [password] [--mdns]

use bl_media::Decoder;
use std::io::Write;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let out_path = args.get(1).cloned().unwrap_or_else(|| "airplay.f32".into());
    let password = args.get(2).filter(|a| !a.starts_with("--")).cloned();
    let rx = bl_airplay::Receiver::start(bl_airplay::Config {
        name: "Beatlumen Test".into(),
        id: [0x0a, 0x1b, 0x2c, 0x3d, 0x4e, 0x5f],
        password,
    })
    .expect("start receiver");
    let _mdns = args.iter().any(|a| a == "--mdns").then(|| {
        // AirPlay 1 (_raop._tcp) and AirPlay 2 (_airplay._tcp).
        bl_airplay::mdns::Responder::start_all(rx.services(), "beatlumen-test".into())
            .expect("mdns")
    });
    println!("PORT {}", rx.port());
    std::io::stdout().flush().ok();
    rx.stream().set_output_latency(0.0);
    let mut file = std::fs::File::create(&out_path).expect("output");
    loop {
        for e in rx.poll() {
            println!("EVENT {e:?}");
            std::io::stdout().flush().ok();
            if let bl_airplay::Event::Started { .. } = e {
                let mut dec = rx.stream().decoder();
                let mut buf = Vec::new();
                loop {
                    buf.clear();
                    match dec.next_chunk(&mut buf) {
                        Ok(true) => {
                            for s in &buf {
                                file.write_all(&s.to_le_bytes()).ok();
                            }
                        }
                        _ => break,
                    }
                }
                file.flush().ok();
                println!("STREAM-END");
                std::io::stdout().flush().ok();
                for e in rx.poll() {
                    println!("EVENT {e:?}");
                }
                std::io::stdout().flush().ok();
                return;
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
