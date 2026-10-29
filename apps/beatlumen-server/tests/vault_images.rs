//! Images from the vault as backgrounds: listed with thumbnails made from
//! the decrypted image, forgotten when the vault locks.

mod common;

use bl_json::Value;
use common::App;

#[test]
fn vault_images_get_thumbnails() {
    let data = std::env::temp_dir().join(format!("bl-vault-images-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);
    let invite = beatlumen_server::vault::create_invite(&data, 1).unwrap();

    let mut app = App::new("vault-images");
    app.connect(&url);
    app.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite)
            .with("user", "Emil")
            .with("password", "correct horse battery"),
    );
    app.ready();

    // A 64×48 picture.
    let mut img = bl_image::Image::new(64, 48);
    for (i, px) in img.pixels.chunks_mut(4).enumerate() {
        px.copy_from_slice(&[(i % 64 * 4) as u8, 128, 200, 255]);
    }
    std::fs::create_dir_all(&app.dir).unwrap();
    let path = app.dir.join("holiday.png");
    std::fs::write(&path, bl_image::encode_png(&img)).unwrap();
    app.cmd(
        Value::object()
            .with("cmd", "vault.upload")
            .with("paths", vec![path.to_string_lossy().to_string()]),
    );
    app.wait("uploaded", |_| true);
    let t0 = std::time::Instant::now();
    loop {
        app.state();
        let files = app.files();
        if files.iter().any(|f| f.bool_or("image", false)) {
            assert!(!files[0].bool_or("audio", true));
            break;
        }
        assert!(t0.elapsed().as_secs() < 30, "upload");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // The list asks for thumbnails; they arrive decrypted and scaled.
    let mut thumb = String::new();
    let t0 = std::time::Instant::now();
    while thumb.is_empty() {
        app.state();
        let list = app.cmd(Value::object().with("cmd", "vault.images"));
        let list = list.as_array().cloned().unwrap_or_default();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].str_or("name", ""), "holiday.png");
        assert!(list[0].str_or("source", "").starts_with("vault:"));
        thumb = list[0].str_or("thumb", "").to_string();
        assert!(t0.elapsed().as_secs() < 30, "thumbnail");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let jpeg =
        bl_net::base64_decode(thumb.strip_prefix("data:image/jpeg;base64,").unwrap()).unwrap();
    let back = bl_image::decode(&jpeg).unwrap();
    assert_eq!((back.width, back.height), (64, 48));

    // As a background it is listed with its name and thumbnail.
    let source = format!(
        "vault:{}",
        app.files()[0].str_or("id", "").trim_start_matches("vault:")
    );
    app.cmd(
        Value::object()
            .with("cmd", "backgrounds.add")
            .with("sources", vec![source.clone()])
            .with("show", true),
    );
    let bgs = app.cmd(Value::object().with("cmd", "backgrounds.list"));
    let bg = bgs.at(0);
    assert_eq!(bg.str_or("source", ""), source);
    assert_eq!(bg.str_or("label", ""), "holiday.png");
    assert_eq!(bg.str_or("thumb", ""), thumb);

    // Locked: no images, no thumbnails.
    app.cmd(Value::object().with("cmd", "vault.logout"));
    app.state();
    assert_eq!(
        app.cmd(Value::object().with("cmd", "vault.images"))
            .as_array()
            .map(Vec::len),
        Some(0)
    );
    let bg = app
        .cmd(Value::object().with("cmd", "backgrounds.list"))
        .at(0)
        .clone();
    assert_eq!(bg.str_or("thumb", ""), "");

    drop(server);
    let _ = std::fs::remove_dir_all(&data);
}
