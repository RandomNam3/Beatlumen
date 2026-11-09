//! Phones as wireless microphones: one app (the phone) sends its
//! microphone through the server to another (the device that plays), which
//! hears it as an input of its own, with the times it was recorded.

mod common;

use bl_core::mic;
use bl_json::Value;
use common::App;
use std::time::{Duration, Instant};

/// On or off for 20 ms steps, at random (the same each run).
fn keyed(n: usize) -> f64 {
    let step = (n / 960) as u32;
    let x = step.wrapping_mul(2_654_435_761).rotate_left(13) ^ 0x9e37_79b9;
    ((x >> 7) & 1) as f64
}

fn named(app: &mut App, name: &str) {
    app.cmd(
        Value::object()
            .with("cmd", "settings.set")
            .with("key", "deviceName")
            .with("value", name),
    );
}

#[test]
fn a_phone_sings_into_another_device() {
    let root = std::env::temp_dir().join(format!("bl-phone-mic-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("music")).unwrap();
    let server = beatlumen_server::start(beatlumen_server::Config {
        music_dirs: vec![root.join("music")],
        data_dir: root.join("data"),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    assert!(server.wait_for_scan(Duration::from_secs(30)));
    let url = format!("http://{}", server.addr);
    let mut phone = App::new("phone-mic-phone");
    let mut tv = App::new("phone-mic-tv");
    named(&mut phone, "Phone");
    named(&mut tv, "TV");
    phone.connect(&url);
    tv.connect(&url);
    // The phone sees the TV among the devices.
    let t0 = Instant::now();
    let tv_id = loop {
        phone.state();
        tv.state();
        let list = phone.cmd(Value::object().with("cmd", "devices.list"));
        if let Some(d) = list
            .as_array()
            .and_then(|l| l.iter().find(|d| d.str_or("name", "") == "TV"))
        {
            break d.str_or("id", "").to_string();
        }
        phone.cmd(Value::object().with("cmd", "devices.refresh"));
        assert!(t0.elapsed() < Duration::from_secs(20), "{}", list.to_json());
        std::thread::sleep(Duration::from_millis(50));
    };
    // The phone's own microphone.
    phone.host.mic = Some(Default::default());
    phone.cmd(Value::object().with("cmd", "mic.devices").with(
        "devices",
        Value::Array(vec![Value::object()
            .with("id", "builtin")
            .with("name", "Built-in")
            .with("rate", 48000)]),
    ));
    let ps = phone
        .cmd(
            Value::object()
                .with("cmd", "mic.open")
                .with("device", "builtin"),
        )
        .get("stream")
        .as_u64()
        .unwrap() as u32;
    let r = phone.cmd(
        Value::object()
            .with("cmd", "karaoke.send")
            .with("target", tv_id.as_str())
            .with("stream", ps),
    );
    assert!(r.bool_or("ok", false), "{}", r.to_json());
    tv.cmd(
        Value::object()
            .with("cmd", "karaoke.receive")
            .with("on", true),
    );

    // The phone sings A4 in real time; the TV opens the phone's input once
    // it shows up and reads what arrives.
    let mut phase = 0f64;
    let t0 = Instant::now();
    let mut sung = 0usize;
    let mut tv_stream: Option<u32> = None;
    let mut next = 0u64;
    let mut heard: Vec<f32> = Vec::new();
    // When each sample heard was sung, as the TV's input says (µs after
    // the phone's first sample).
    let mut times: Vec<f64> = Vec::new();
    // (clock µs, sample number) of what the phone pushed.
    let start_us = mic::clock_us();
    while t0.elapsed() < Duration::from_secs(6) {
        let due = (t0.elapsed().as_secs_f64() * 48000.0) as usize;
        if due > sung {
            let pcm: Vec<i16> = (sung..due)
                .map(|n| {
                    phase += std::f64::consts::TAU * 440.0 / 48000.0;
                    (phase.sin() * 12000.0 * keyed(n)) as i16
                })
                .collect();
            let at = start_us + (sung as i64 * 1_000_000 / 48000);
            mic::push_pcm(ps, &pcm, 1, 48000, at);
            sung = due;
        }
        phone.state();
        tv.state();
        if tv_stream.is_none() {
            let l = tv.cmd(Value::object().with("cmd", "mic.list"));
            if let Some(d) = l
                .get("devices")
                .as_array()
                .and_then(|a| a.iter().find(|d| d.bool_or("phone", false)))
            {
                assert!(d.str_or("name", "").contains("Phone"), "{}", d.to_json());
                let id = d.str_or("id", "").to_string();
                tv_stream = tv
                    .cmd(Value::object().with("cmd", "mic.open").with("device", id))
                    .get("stream")
                    .as_u64()
                    .map(|s| s as u32);
            }
        } else if let Some(b) = mic::read(tv_stream.unwrap(), next, 48000) {
            if !b.samples.is_empty() {
                for (j, v) in b.samples.iter().enumerate() {
                    times.push(b.time_us + j as f64 * 1e6 / 48000.0 - start_us as f64);
                    heard.push(*v);
                }
                next = b.index + b.samples.len() as u64;
            }
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    let s = phone.cmd(Value::object().with("cmd", "karaoke.sending"));
    assert!(
        s.get("packets").as_u64().unwrap_or(0) > 100,
        "{}",
        s.to_json()
    );
    assert!(tv_stream.is_some(), "the phone never showed up on the TV");
    assert!(
        heard.len() > 48000 * 2,
        "only {} samples arrived",
        heard.len()
    );
    // A4 arrived: the period of the loud parts of the last second.
    let last = &heard[heard.len() - 48000..];
    let loud: Vec<usize> = (1..last.len())
        .filter(|&i| {
            last[i - 1] < 0.0
                && last[i] >= 0.0
                && last[i.saturating_sub(30)..i].iter().any(|v| v.abs() > 0.2)
        })
        .collect();
    let periods: Vec<usize> = loud
        .windows(2)
        .map(|w| w[1] - w[0])
        .filter(|p| *p < 200)
        .collect();
    let mut sorted = periods.clone();
    sorted.sort_unstable();
    let period = sorted[sorted.len() / 2] as f64;
    assert!(
        (48000.0 / period - 440.0).abs() < 15.0,
        "{} Hz",
        48000.0 / period
    );
    // The times say when it was sung: the keyed pattern heard against the
    // pattern sung, whatever the network took.
    let score = |d_us: f64| -> f64 {
        heard
            .iter()
            .zip(&times)
            .map(|(v, t)| {
                let n = ((t - d_us) * 48000.0 / 1e6).round();
                if n < 0.0 {
                    0.0
                } else {
                    v.abs() as f64 * (keyed(n as usize) * 2.0 - 1.0)
                }
            })
            .sum()
    };
    let best = (-40..=40)
        .map(|ms| ms as f64 * 1000.0)
        .max_by(|a, b| score(*a).total_cmp(&score(*b)))
        .unwrap();
    eprintln!("time stamps off by {:.1} ms", best / 1000.0);
    assert!(best.abs() <= 5000.0, "{} ms", best / 1000.0);
    drop(phone);
    drop(tv);
    drop(server);
    let _ = std::fs::remove_dir_all(&root);
}
