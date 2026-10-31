//! Microphones (karaoke): the core asks the platform for its inputs,
//! opens and closes them, keeps their audio with its times and meters it.

mod common;

use bl_core::mic;
use bl_json::Value;
use common::App;

fn cmd(app: &mut App, name: &str, args: Value) -> Value {
    let mut v = args;
    v.set("cmd", name);
    app.cmd(v)
}

fn requests(app: &App) -> Vec<(String, Value)> {
    std::mem::take(&mut *app.host.mic.as_ref().unwrap().lock().unwrap())
        .into_iter()
        .map(|(k, j)| (k, bl_json::parse(&j).unwrap()))
        .collect()
}

#[test]
fn inputs_are_listed_opened_metered_and_closed() {
    let mut app = App::new("mic");
    // Without microphones on the platform.
    let l = cmd(&mut app, "mic.list", Value::object());
    assert!(!l.bool_or("available", true));
    assert!(
        cmd(&mut app, "mic.open", Value::object().with("device", "x"))
            .get("error")
            .as_str()
            .is_some()
    );

    app.host.mic = Some(Default::default());
    let l = cmd(&mut app, "mic.list", Value::object());
    assert!(l.bool_or("available", false));
    assert!(!l.bool_or("listed", true));
    assert_eq!(requests(&app)[0].0, "list");
    // The platform answers with its inputs.
    let devices = Value::Array(vec![
        Value::object()
            .with("id", "usb-mic")
            .with("name", "USB Mic")
            .with("channels", 1)
            .with("rate", 48000)
            .with("default", true),
        Value::object()
            .with("id", "interface")
            .with("name", "Interface")
            .with("channels", 2)
            .with("rate", 44100),
    ]);
    cmd(
        &mut app,
        "mic.devices",
        Value::object().with("devices", devices),
    );
    let l = cmd(&mut app, "mic.list", Value::object());
    assert!(l.bool_or("listed", false));
    assert_eq!(l.get("devices").as_array().unwrap().len(), 2);
    assert_eq!(l.get("devices").at(1).str_or("name", ""), "Interface");
    requests(&app);

    // Two singers on the two channels of the interface, one on the USB mic.
    let a = cmd(
        &mut app,
        "mic.open",
        Value::object().with("device", "usb-mic"),
    )
    .get("stream")
    .as_u64()
    .unwrap() as u32;
    let b = cmd(
        &mut app,
        "mic.open",
        Value::object()
            .with("device", "interface")
            .with("channel", 0),
    )
    .get("stream")
    .as_u64()
    .unwrap() as u32;
    let c = cmd(
        &mut app,
        "mic.open",
        Value::object()
            .with("device", "interface")
            .with("channel", 1),
    )
    .get("stream")
    .as_u64()
    .unwrap() as u32;
    assert!(a != b && b != c);
    // Opening the same again: the same stream.
    let again = cmd(
        &mut app,
        "mic.open",
        Value::object().with("device", "usb-mic"),
    );
    assert_eq!(again.get("stream").as_u64(), Some(a as u64));
    let r = requests(&app);
    assert_eq!(r.len(), 3);
    assert_eq!(r[0].0, "open");
    assert_eq!(r[0].1.str_or("device", ""), "usb-mic");
    assert_eq!(r[0].1.get("stream").as_u64(), Some(a as u64));

    // Audio arrives: the meters move, the channels are kept apart.
    for s in [a, b, c] {
        cmd(
            &mut app,
            "mic.state",
            Value::object().with("stream", s).with("running", true),
        );
    }
    let now = mic::clock_us();
    mic::push_pcm(a, &vec![8000i16; 480], 1, 48000, now);
    let stereo: Vec<i16> = (0..882)
        .map(|i| if i % 2 == 0 { 16000 } else { 0 })
        .collect();
    mic::push_pcm(b, &stereo, 2, 44100, now);
    mic::push_pcm(c, &stereo, 2, 44100, now);
    let levels = cmd(&mut app, "mic.levels", Value::object());
    let streams = levels.get("streams").as_array().unwrap().clone();
    assert_eq!(streams.len(), 3);
    let level = |s: u32| {
        streams
            .iter()
            .find(|v| v.get("stream").as_u64() == Some(s as u64))
            .unwrap()
            .get("level")
            .as_f64()
            .unwrap()
    };
    assert!((level(a) - 8000.0 / 32768.0).abs() < 0.01);
    assert!((level(b) - 16000.0 / 32768.0).abs() < 0.01);
    assert!(level(c) < 0.001);
    assert_eq!(streams[0].str_or("name", ""), "USB Mic");
    assert!(streams[0].bool_or("running", false));
    let block = mic::read(b, 0, 10_000).unwrap();
    assert_eq!((block.rate, block.samples.len()), (44100, 441));

    // A latency offset is stored per input and applies to its streams.
    cmd(
        &mut app,
        "mic.setOffset",
        Value::object().with("device", "usb-mic").with("ms", 30),
    );
    let t = mic::read(a, 0, 1).unwrap().time_us;
    assert!((t - (now as f64 - 30_000.0)).abs() < 1.0);
    let l = cmd(&mut app, "mic.list", Value::object());
    assert_eq!(l.get("devices").at(0).get("offsetMs").as_f64(), Some(30.0));

    // The platform loses one (unplugged): its audio ends, the error stays.
    cmd(
        &mut app,
        "mic.state",
        Value::object()
            .with("stream", c)
            .with("running", false)
            .with("error", "device unplugged"),
    );
    assert!(mic::read(c, 0, 1).is_none());
    let levels = cmd(&mut app, "mic.levels", Value::object());
    let lost = levels.get("streams").at(2).clone();
    assert_eq!(lost.str_or("error", ""), "device unplugged");

    // Closing by stream and by device.
    requests(&app);
    assert!(cmd(&mut app, "mic.close", Value::object().with("stream", a)).bool_or("ok", false));
    assert!(cmd(
        &mut app,
        "mic.close",
        Value::object().with("device", "interface")
    )
    .bool_or("ok", false));
    let r = requests(&app);
    assert_eq!(r.len(), 3);
    assert!(r.iter().all(|(k, _)| k == "close"));
    assert!(mic::read(a, 0, 1).is_none());
    assert!(cmd(&mut app, "mic.levels", Value::object())
        .get("streams")
        .as_array()
        .unwrap()
        .is_empty());

    // The offset survives a restart.
    let dir = app.close();
    let mut app = App::with_dir(dir);
    app.host.mic = Some(Default::default());
    cmd(
        &mut app,
        "mic.devices",
        Value::object().with(
            "devices",
            Value::Array(vec![Value::object()
                .with("id", "usb-mic")
                .with("name", "USB Mic")]),
        ),
    );
    let l = cmd(&mut app, "mic.list", Value::object());
    assert_eq!(l.get("devices").at(0).get("offsetMs").as_f64(), Some(30.0));
}

#[test]
fn closing_the_app_closes_the_microphones() {
    let mut app = App::new("mic-close");
    app.host.mic = Some(Default::default());
    cmd(&mut app, "mic.open", Value::object().with("device", "d"));
    requests(&app);
    let m = app.host.mic.clone().unwrap();
    app.close();
    let r = std::mem::take(&mut *m.lock().unwrap());
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].0, "close");
}
