//! Passkeys and saved passwords through the platform's credential manager
//! (Android): the test host plays the provider with a software
//! authenticator and answers as `Credentials.java` does.

mod common;

use bl_json::Value;
use bl_vault::webpush::base64url;
use common::authenticator::Authenticator;
use common::App;
use std::time::{Duration, Instant};

const ORIGIN_URL: &str = "http://localhost:7878";

/// The origin Android's Credential Manager reports for the app.
fn android_origin() -> String {
    format!("android:apk-key-hash:{}", base64url(&[0xab; 32]))
}

/// Waits for the next request to the credential manager.
fn next_request(app: &mut App, kind: &str) -> Value {
    let t0 = Instant::now();
    loop {
        app.state();
        let r = {
            let mut c = app.host.credentials.lock().unwrap();
            if c.is_empty() {
                None
            } else {
                Some(c.remove(0))
            }
        };
        if let Some((k, json)) = r {
            assert_eq!(k, kind, "{json}");
            return bl_json::parse(&json).unwrap();
        }
        assert!(
            t0.elapsed() < Duration::from_secs(60),
            "no {kind} request: {}",
            app.state().get("vault").to_json()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn result(app: &mut App, kind: &str, ok: bool, json: Value) {
    app.cmd(
        Value::object()
            .with("cmd", "vault.credentialResult")
            .with("kind", kind)
            .with("ok", ok)
            .with("json", json.to_json()),
    );
}

fn public_key(req: &Value) -> Value {
    bl_json::parse(req.str_or("publicKey", "")).unwrap()
}

fn prf_results(a: &Authenticator) -> Value {
    Value::object().with(
        "prf",
        Value::object().with("results", Value::object().with("first", base64url(&a.prf))),
    )
}

/// An assertion as the provider returns it (WebAuthn JSON).
fn assertion(a: &mut Authenticator, pk: &Value, prf: bool) -> Value {
    let (client, auth, sig) = a.get(pk.str_or("challenge", ""), &android_origin(), 0x05);
    Value::object()
        .with("type", "androidx.credentials.TYPE_PUBLIC_KEY_CREDENTIAL")
        .with(
            "response",
            Value::object()
                .with("id", base64url(&a.id))
                .with("rawId", base64url(&a.id))
                .with("type", "public-key")
                .with(
                    "response",
                    Value::object()
                        .with("clientDataJSON", base64url(&client))
                        .with("authenticatorData", base64url(&auth))
                        .with("signature", base64url(&sig)),
                )
                .with(
                    "clientExtensionResults",
                    if prf { prf_results(a) } else { Value::object() },
                ),
        )
}

fn locked(app: &mut App) -> Value {
    app.wait("settled", |s| {
        let v = s.get("vault");
        v.str_or("state", "") != "busy"
    })
    .get("vault")
    .clone()
}

#[test]
fn passkeys_and_passwords_from_the_platform() {
    let data = std::env::temp_dir().join(format!("bl-native-cred-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let server = beatlumen_server::start(beatlumen_server::Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        public_url: Some(ORIGIN_URL.into()),
        android_certs: vec!["ab".repeat(32)],
        ..Default::default()
    })
    .unwrap();
    let url = format!("http://{}", server.addr);
    let invite = beatlumen_server::vault::create_invite(&data, 1).unwrap();

    let mut app = App::new("native-cred");
    app.host.android = true;
    app.connect(&url);
    let v = app.state().get("vault").clone();
    assert!(v.bool_or("platformCredentials", false));
    assert!(v.bool_or("passkeys", false));

    // Registering offers the password to the password manager.
    let password = "correct horse battery";
    app.cmd(
        Value::object()
            .with("cmd", "vault.register")
            .with("invite", invite)
            .with("user", "Carol")
            .with("password", password),
    );
    let s = app.ready();
    assert_eq!(s.get("vault").str_or("error", ""), "");
    let fingerprint = s.get("vault").str_or("fingerprint", "").to_string();
    let save = next_request(&mut app, "save");
    assert_eq!(save.str_or("user", ""), "carol");
    assert_eq!(save.str_or("password", ""), password);

    // A wrong password is refused before anything is created.
    app.cmd(
        Value::object()
            .with("cmd", "vault.addPasskey")
            .with("password", "wrong password!")
            .with("name", "Phone"),
    );
    let v = locked(&mut app);
    assert_eq!(v.str_or("error", ""), "Wrong password");
    assert!(app.host.credentials.lock().unwrap().is_empty());
    app.cmd(Value::object().with("cmd", "vault.clearError"));

    // Adding a passkey: creation, then one sign-in for the PRF output.
    let mut auth = Authenticator::new();
    app.cmd(
        Value::object()
            .with("cmd", "vault.addPasskey")
            .with("password", password)
            .with("name", "Phone"),
    );
    let create = next_request(&mut app, "create");
    assert_eq!(create.str_or("user", ""), "carol");
    let pk = public_key(&create);
    assert_eq!(pk.get("rp").str_or("id", ""), "localhost");
    assert!(pk.get("extensions").get("prf").get("eval").get("first").as_str().is_some());
    let (client, att) = auth.create(pk.str_or("challenge", ""), &android_origin());
    result(
        &mut app,
        "create",
        true,
        Value::object().with(
            "response",
            Value::object()
                .with("id", base64url(&auth.id))
                .with("rawId", base64url(&auth.id))
                .with(
                    "response",
                    Value::object()
                        .with("clientDataJSON", base64url(&client))
                        .with("attestationObject", base64url(&att)),
                )
                .with(
                    "clientExtensionResults",
                    Value::object().with("prf", Value::object().with("enabled", true)),
                ),
        ),
    );
    let get = next_request(&mut app, "get");
    assert!(!get.bool_or("password", true));
    let pk = public_key(&get);
    assert_eq!(
        pk.get("allowCredentials").at(0).str_or("id", ""),
        base64url(&auth.id)
    );
    let a = assertion(&mut auth, &pk, true);
    result(&mut app, "get", true, a);
    let v = locked(&mut app);
    assert_eq!(v.str_or("error", ""), "", "{}", v.to_json());
    assert_eq!(v.str_or("state", ""), "ready");

    // Signing in with the passkey unlocks the same account.
    app.cmd(Value::object().with("cmd", "vault.logout"));
    app.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    let get = next_request(&mut app, "get");
    assert!(get.bool_or("password", false), "saved passwords offered too");
    let a = assertion(&mut auth, &public_key(&get), true);
    result(&mut app, "get", true, a);
    let s = app.ready();
    assert_eq!(s.get("vault").str_or("error", ""), "");
    assert_eq!(s.get("vault").str_or("fingerprint", ""), fingerprint);
    assert_eq!(s.get("vault").str_or("user", ""), "carol");

    // Without PRF output the passkey cannot unlock.
    app.cmd(Value::object().with("cmd", "vault.logout"));
    app.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    let get = next_request(&mut app, "get");
    let a = assertion(&mut auth, &public_key(&get), false);
    result(&mut app, "get", true, a);
    let v = locked(&mut app);
    assert!(v.str_or("error", "").contains("PRF"), "{}", v.to_json());
    app.cmd(Value::object().with("cmd", "vault.clearError"));

    // A saved password signs in as typed, and is not offered again.
    app.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    next_request(&mut app, "get");
    result(
        &mut app,
        "get",
        true,
        Value::object()
            .with("type", "android.credentials.TYPE_PASSWORD_CREDENTIAL")
            .with("user", "carol")
            .with("password", password),
    );
    let s = app.ready();
    assert_eq!(s.get("vault").str_or("error", ""), "");
    assert_eq!(s.get("vault").str_or("fingerprint", ""), fingerprint);
    assert!(app.host.credentials.lock().unwrap().is_empty());

    // Cancelling the sheet leaves the account locked without an error.
    app.cmd(Value::object().with("cmd", "vault.logout"));
    app.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    next_request(&mut app, "get");
    result(
        &mut app,
        "get",
        false,
        Value::object()
            .with("type", "android.credentials.GetCredentialException.TYPE_USER_CANCELED")
            .with("error", "cancelled"),
    );
    let v = locked(&mut app);
    assert_eq!(v.str_or("state", ""), "locked");
    assert_eq!(v.str_or("error", ""), "");

    // Older Android: the browser takes over.
    app.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    next_request(&mut app, "get");
    result(
        &mut app,
        "get",
        false,
        Value::object().with("type", "unsupported"),
    );
    app.state();
    let opened = app.host.opened.lock().unwrap().clone().unwrap_or_default();
    assert!(opened.contains("/auth/?mode=app"), "{opened}");
    assert!(app.state().get("vault").get("pairingCode").as_str().is_some());

    // Other platforms go to the browser directly.
    let mut desktop = App::new("native-cred-desktop");
    desktop.connect(&url);
    assert!(!desktop.state().get("vault").bool_or("platformCredentials", true));
    desktop.cmd(Value::object().with("cmd", "vault.nativeSignIn"));
    assert!(desktop.host.opened.lock().unwrap().is_some());

    drop(server);
    let _ = std::fs::remove_dir_all(&data);
}
