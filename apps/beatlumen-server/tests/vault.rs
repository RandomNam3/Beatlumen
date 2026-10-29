//! End-to-end encrypted accounts and files against a running server: the
//! client side is done here with `bl-vault`, as the apps do it.

mod common;

use beatlumen_server::{Config, Server};
use bl_json::Value;
use bl_net::{base64_decode, base64_encode, client};
use bl_vault::file::{self, Header};
use bl_vault::keys::{Account, KdfParams, KeyBundle, PasswordKeys};
use bl_vault::recovery::RecoveryCode;
use bl_vault::{identity::PublicIdentity, sealed, Secret};
use std::path::PathBuf;

struct Srv {
    _server: Server,
    base: String,
    data: PathBuf,
}

impl Drop for Srv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn start(name: &str, extra: impl FnOnce(&mut Config)) -> Srv {
    let data = std::env::temp_dir().join(format!("bl-vault-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let mut cfg = Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    };
    extra(&mut cfg);
    let server = beatlumen_server::start(cfg).expect("server starts");
    let base = format!("http://{}", server.addr);
    Srv {
        _server: server,
        base,
        data,
    }
}

impl Srv {
    fn req(&self, method: &str, path: &str, token: Option<&str>, body: &[u8]) -> client::Response {
        let mut h = vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(t) = token {
            h.push(("Authorization".into(), format!("Bearer {t}")));
        }
        client::request(
            method,
            &format!("{}{path}", self.base),
            &h,
            body,
            &client::Options::default(),
        )
        .expect("request")
    }

    fn call(&self, method: &str, path: &str, token: Option<&str>, v: &Value) -> (u16, Value) {
        let r = self.req(method, path, token, v.to_json().as_bytes());
        (
            r.status,
            bl_json::parse_bytes(&r.body).unwrap_or(Value::Null),
        )
    }

    fn invite(&self) -> String {
        beatlumen_server::vault::create_invite(&self.data, 7).unwrap()
    }
}

fn b64(b: &[u8]) -> Value {
    Value::from(base64_encode(b))
}

fn unb64(v: &Value) -> Vec<u8> {
    base64_decode(v.as_str().expect("string")).expect("base64")
}

fn params_json(p: KdfParams) -> Value {
    Value::object()
        .with("m", p.m_kib as u64)
        .with("t", p.t as u64)
        .with("p", p.p as u64)
}

fn bundle_json(b: &KeyBundle) -> Value {
    Value::object()
        .with("salt", b64(&b.salt))
        .with("params", params_json(b.params))
        .with("masterByPassword", b64(&b.master_by_password))
        .with("masterByRecovery", b64(&b.master_by_recovery))
        .with("identity", b64(&b.identity))
        .with("public", b64(&b.public.to_bytes()))
}

fn bundle_from(v: &Value) -> KeyBundle {
    let p = v.get("params");
    KeyBundle {
        salt: unb64(v.get("salt")).try_into().unwrap(),
        params: KdfParams {
            m_kib: p.get("m").as_u64().unwrap() as u32,
            t: p.get("t").as_u64().unwrap() as u32,
            p: p.get("p").as_u64().unwrap() as u32,
        },
        master_by_password: unb64(v.get("masterByPassword")),
        master_by_recovery: unb64(v.get("masterByRecovery")),
        identity: unb64(v.get("identity")),
        public: PublicIdentity::from_bytes(&unb64(v.get("public"))).unwrap(),
    }
}

/// Registers through the API; returns the session token and the code.
fn register(s: &Srv, user: &str, password: &str) -> (String, Account, RecoveryCode) {
    let (acc, reg, code) = Account::register(user, password).unwrap();
    let body = Value::object()
        .with("invite", s.invite())
        .with("user", user)
        .with("auth", b64(reg.auth.as_bytes()))
        .with("recoveryAuth", b64(reg.recovery_auth.as_bytes()))
        .with("bundle", bundle_json(&reg.bundle));
    let (st, v) = s.call("POST", "/api/vault/register", None, &body);
    assert_eq!(st, 201, "{}", v.to_json());
    (v.str_or("token", "").to_string(), acc, code)
}

/// Logs in like an app: prelogin, Argon2 on the device, login, unlock.
fn login(s: &Srv, user: &str, password: &str) -> Result<(String, Account), u16> {
    let (st, pre) = s.call(
        "POST",
        "/api/vault/prelogin",
        None,
        &Value::object().with("user", user),
    );
    assert_eq!(st, 200);
    let salt: [u8; 16] = unb64(pre.get("salt")).try_into().unwrap();
    let p = pre.get("params");
    let params = KdfParams {
        m_kib: p.get("m").as_u64().unwrap() as u32,
        t: p.get("t").as_u64().unwrap() as u32,
        p: p.get("p").as_u64().unwrap() as u32,
    };
    let keys = PasswordKeys::derive(password, &salt, params).unwrap();
    let body = Value::object()
        .with("user", user)
        .with("auth", b64(keys.auth.as_bytes()));
    let (st, v) = s.call("POST", "/api/vault/login", None, &body);
    if st != 200 {
        return Err(st);
    }
    let acc = Account::unlock(user, &keys, &bundle_from(v.get("bundle"))).unwrap();
    Ok((v.str_or("token", "").to_string(), acc))
}

/// Encrypts and uploads `plain` in parts; returns the file id and key.
fn upload(s: &Srv, token: &str, acc: &Account, plain: &[u8], name: &str) -> (String, Secret) {
    let key = Secret::random().unwrap();
    let (h, enc) = file::encrypt(&key, plain).unwrap();
    let meta = file::seal_meta(&key, &h, format!("{{\"name\":\"{name}\"}}").as_bytes()).unwrap();
    let sig = file::sign(&acc.identity, &h, &meta, &key);
    let id: String = h.file_id.iter().map(|b| format!("{b:02x}")).collect();
    let mine = sealed::seal(
        &acc.identity.public().dh,
        &sealed::file_key_aad(&h.file_id, &acc.user),
        key.as_bytes(),
    )
    .unwrap();
    let body = Value::object()
        .with("header", b64(&h.to_bytes()))
        .with("meta", b64(&meta))
        .with("signature", b64(&sig))
        .with("key", b64(&mine));
    let (st, v) = s.call("POST", "/api/vault/files", Some(token), &body);
    assert_eq!(st, 201, "{}", v.to_json());
    assert_eq!(v.str_or("id", ""), id);
    let mut off = file::HEADER_LEN;
    while off < enc.len() {
        let end = (off + 3 * 1024 * 1024).min(enc.len());
        let r = s.req(
            "PUT",
            &format!("/api/vault/files/{id}/data?offset={off}"),
            Some(token),
            &enc[off..end],
        );
        assert_eq!(r.status, 200, "{}", String::from_utf8_lossy(&r.body));
        off = end;
    }
    (id, key)
}

/// Downloads and verifies a file as `acc`, like an app.
fn download(s: &Srv, token: &str, acc: &Account, id: &str) -> Result<(Vec<u8>, String), String> {
    let (st, e) = s.call(
        "GET",
        &format!("/api/vault/files/{id}"),
        Some(token),
        &Value::Null,
    );
    if st != 200 {
        return Err(format!("status {st}"));
    }
    let h = Header::parse(&unb64(e.get("header"))).map_err(|e| e.to_string())?;
    let sealed_key = unb64(e.get("key"));
    let raw = sealed::open(
        &acc.identity,
        &sealed::file_key_aad(&h.file_id, &acc.user),
        &sealed_key,
    )
    .map_err(|e| e.to_string())?;
    let key = Secret::from_bytes(raw.try_into().map_err(|_| "key length")?);
    // The owner's key comes from the server; the apps pin it.
    let owner = e.str_or("owner", "");
    let (_, u) = s.call(
        "GET",
        &format!("/api/vault/users/{owner}"),
        Some(token),
        &Value::Null,
    );
    let owner_pub =
        PublicIdentity::from_bytes(&unb64(u.get("public"))).map_err(|e| e.to_string())?;
    let meta = unb64(e.get("meta"));
    let sig: [u8; 64] = unb64(e.get("signature")).try_into().unwrap();
    file::verify(&owner_pub, &h, &meta, &key, &sig).map_err(|e| e.to_string())?;
    let name =
        String::from_utf8(file::open_meta(&key, &h, &meta).map_err(|e| e.to_string())?).unwrap();
    let r = s.req(
        "GET",
        &format!("/api/vault/files/{id}/data"),
        Some(token),
        &[],
    );
    if r.status != 200 {
        return Err(format!("data status {}", r.status));
    }
    let plain = file::decrypt(&key, &r.body).map_err(|e| e.to_string())?;
    Ok((plain, name))
}

fn share(s: &Srv, token: &str, id: &str, key: &Secret, to: &str) -> u16 {
    let (_, u) = s.call(
        "GET",
        &format!("/api/vault/users/{to}"),
        Some(token),
        &Value::Null,
    );
    let p = PublicIdentity::from_bytes(&unb64(u.get("public"))).unwrap();
    let file_id: [u8; 16] = (0..16)
        .map(|i| u8::from_str_radix(&id[i * 2..i * 2 + 2], 16).unwrap())
        .collect::<Vec<_>>()
        .try_into()
        .unwrap();
    let boxed = sealed::seal(&p.dh, &sealed::file_key_aad(&file_id, to), key.as_bytes()).unwrap();
    let body = Value::object().with("user", to).with("key", b64(&boxed));
    s.call(
        "POST",
        &format!("/api/vault/files/{id}/keys"),
        Some(token),
        &body,
    )
    .0
}

#[test]
fn accounts_files_and_sharing() {
    let s = start("main", |_| {});
    // Registration needs a valid, unused invitation.
    let (acc0, reg0, _) = Account::register("mallory", "pw").unwrap();
    let mut body = Value::object()
        .with("invite", "0000-0000-0000-0000-0000-0000")
        .with("user", "mallory")
        .with("auth", b64(reg0.auth.as_bytes()))
        .with("recoveryAuth", b64(reg0.recovery_auth.as_bytes()))
        .with("bundle", bundle_json(&reg0.bundle));
    assert_eq!(s.call("POST", "/api/vault/register", None, &body).0, 403);
    let invite = s.invite();
    body.set("invite", invite.to_uppercase());
    // A bundle whose public key does not match its binding is refused.
    let mut bad = reg0.bundle.clone();
    bad.public.dh = [9; 32];
    let mut b2 = body.clone();
    b2.set("bundle", bundle_json(&bad));
    assert_eq!(s.call("POST", "/api/vault/register", None, &b2).0, 400);
    assert_eq!(s.call("POST", "/api/vault/register", None, &body).0, 201);
    assert_eq!(
        s.call("POST", "/api/vault/register", None, &body).0,
        403,
        "invitation is single-use"
    );
    drop(acc0);

    let (ta, alice, alice_code) = register(&s, "Alice", "alice password");
    let (_tb, bob, _) = register(&s, "bob", "bob password");
    assert_eq!(alice.user, "alice");
    assert_eq!(
        s.call("POST", "/api/vault/register", None, &Value::object())
            .0,
        400
    );

    // Prelogin answers for unknown names too (same shape, stable salt).
    let p1 = s
        .call(
            "POST",
            "/api/vault/prelogin",
            None,
            &Value::object().with("user", "nobody"),
        )
        .1;
    let p2 = s
        .call(
            "POST",
            "/api/vault/prelogin",
            None,
            &Value::object().with("user", "nobody"),
        )
        .1;
    assert_eq!(p1.to_json(), p2.to_json());
    assert_eq!(unb64(p1.get("salt")).len(), 16);

    // Login, wrong password, unknown user.
    let (ta2, alice2) = login(&s, "alice", "alice password").unwrap();
    assert_eq!(alice2.identity.public(), alice.identity.public());
    assert_eq!(login(&s, "alice", "wrong").err(), Some(401));
    assert_eq!(login(&s, "nobody", "x").err(), Some(401));
    let (tb, bob) = login(&s, "bob", "bob password")
        .map(|(t, _)| (t, bob))
        .unwrap();

    // Users are listed with their public keys; the library is open to them.
    let (st, users) = s.call("GET", "/api/vault/users", Some(&ta), &Value::Null);
    assert_eq!(st, 200);
    assert_eq!(users.as_array().unwrap().len(), 3);
    assert_eq!(s.call("GET", "/api/vault/users", None, &Value::Null).0, 401);

    // Upload, list, download.
    let song: Vec<u8> = (0..300_000u32).map(|i| (i * 7 + i / 300) as u8).collect();
    let (id, key) = upload(&s, &ta, &alice, &song, "song.flac");
    let (plain, meta) = download(&s, &ta2, &alice2, &id).unwrap();
    assert_eq!(plain, song);
    assert_eq!(meta, r#"{"name":"song.flac"}"#);
    let list = s.call("GET", "/api/vault/files", Some(&ta), &Value::Null).1;
    assert_eq!(list.get("files").as_array().unwrap().len(), 1);
    assert!(list.get("used").as_u64().unwrap() > 300_000);

    // On disk there is no plaintext.
    let blob = std::fs::read(s.data.join(format!("vault/files/{id}.blob"))).unwrap();
    assert!(!blob.windows(64).any(|w| song.windows(64).next() == Some(w)));
    assert!(!String::from_utf8_lossy(
        &std::fs::read(s.data.join(format!("vault/files/{id}.json"))).unwrap()
    )
    .contains("song"));

    // Byte ranges for streaming.
    let r = s.req(
        "GET",
        &format!("/api/vault/files/{id}/data"),
        Some(&ta),
        &[],
    );
    let h = Header::parse(&r.body).unwrap();
    let c = 2;
    let (a, len) = (h.chunk_offset(c), h.chunk_len());
    let mut hdr = vec![("Authorization".to_string(), format!("Bearer {ta}"))];
    hdr.push(("Range".into(), format!("bytes={a}-{}", a + len - 1)));
    let rr = client::request(
        "GET",
        &format!("{}/api/vault/files/{id}/data", s.base),
        &hdr,
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(rr.status, 206);
    let chunk = file::decrypt_chunk(&key, &h, c, &rr.body).unwrap();
    assert_eq!(
        chunk[..],
        song[(c * h.chunk_size as u64) as usize..][..chunk.len()]
    );

    // Bob cannot see it until it is shared with him.
    assert_eq!(
        s.call(
            "GET",
            &format!("/api/vault/files/{id}"),
            Some(&tb),
            &Value::Null
        )
        .0,
        404
    );
    assert_eq!(
        s.req(
            "GET",
            &format!("/api/vault/files/{id}/data"),
            Some(&tb),
            &[]
        )
        .status,
        404
    );
    assert_eq!(share(&s, &ta, &id, &key, "bob"), 204);
    let (plain, _) = download(&s, &tb, &bob, &id).unwrap();
    assert_eq!(plain, song);
    // Recipients cannot share on, change or delete.
    assert_eq!(share(&s, &tb, &id, &key, "mallory"), 403);
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/files/{id}"), Some(&tb), &[])
            .status,
        403
    );
    // The owner sees the recipients.
    let e = s
        .call(
            "GET",
            &format!("/api/vault/files/{id}"),
            Some(&ta),
            &Value::Null,
        )
        .1;
    assert_eq!(e.get("recipients").to_json(), r#"["bob"]"#);

    // A key the server made up (it could seal one to bob) does not match
    // the owner's signature.
    let fake = Secret::random().unwrap();
    let e = s
        .call(
            "GET",
            &format!("/api/vault/files/{id}"),
            Some(&tb),
            &Value::Null,
        )
        .1;
    let sig: [u8; 64] = unb64(e.get("signature")).try_into().unwrap();
    assert!(file::verify(
        &alice.identity.public(),
        &h,
        &unb64(e.get("meta")),
        &fake,
        &sig
    )
    .is_err());
    file::verify(
        &alice.identity.public(),
        &h,
        &unb64(e.get("meta")),
        &key,
        &sig,
    )
    .unwrap();

    // Revoke.
    assert_eq!(
        s.req(
            "DELETE",
            &format!("/api/vault/files/{id}/keys/bob"),
            Some(&ta),
            &[]
        )
        .status,
        204
    );
    assert!(download(&s, &tb, &bob, &id).is_err());

    // Upload protocol: wrong offsets, too much data, not finished.
    let key2 = Secret::random().unwrap();
    let (h2, enc2) = file::encrypt(&key2, &vec![1u8; 100_000]).unwrap();
    let meta2 = file::seal_meta(&key2, &h2, b"{}").unwrap();
    let body = Value::object()
        .with("header", b64(&h2.to_bytes()))
        .with("meta", b64(&meta2))
        .with(
            "signature",
            b64(&file::sign(&alice.identity, &h2, &meta2, &key2)),
        )
        .with("key", b64(&[0u8; sealed::SEALED_KEY_LEN]));
    let (st, v) = s.call("POST", "/api/vault/files", Some(&ta), &body);
    assert_eq!(st, 201);
    let id2 = v.str_or("id", "").to_string();
    assert_eq!(s.call("POST", "/api/vault/files", Some(&ta), &body).0, 409);
    assert_eq!(
        s.req(
            "PUT",
            &format!("/api/vault/files/{id2}/data?offset=0"),
            Some(&ta),
            &enc2
        )
        .status,
        409
    );
    assert_eq!(
        s.req(
            "PUT",
            &format!("/api/vault/files/{id2}/data?offset=49"),
            Some(&tb),
            &enc2[49..]
        )
        .status,
        404
    );
    assert_eq!(
        s.req(
            "GET",
            &format!("/api/vault/files/{id2}/data"),
            Some(&ta),
            &[]
        )
        .status,
        409
    );
    let mut too_much = enc2[49..].to_vec();
    too_much.push(0);
    assert_eq!(
        s.req(
            "PUT",
            &format!("/api/vault/files/{id2}/data?offset=49"),
            Some(&ta),
            &too_much
        )
        .status,
        400
    );
    assert_eq!(
        s.req(
            "PUT",
            &format!("/api/vault/files/{id2}/data?offset=49"),
            Some(&ta),
            &enc2[49..]
        )
        .status,
        200
    );
    assert_eq!(
        s.req(
            "PUT",
            &format!("/api/vault/files/{id2}/data?offset={}", enc2.len()),
            Some(&ta),
            b"x"
        )
        .status,
        409
    );
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/files/{id2}"), Some(&ta), &[])
            .status,
        204
    );
    assert_eq!(
        s.req("GET", &format!("/api/vault/files/{id2}"), Some(&ta), &[])
            .status,
        404
    );
    assert_eq!(
        s.req("GET", "/api/vault/files/..%2f..%2fusers", Some(&ta), &[])
            .status,
        404
    );

    // Password change needs the current password and ends other sessions.
    let new = alice.change_password("new alice password").unwrap();
    let pw = Value::object()
        .with("salt", b64(&new.salt))
        .with("params", params_json(new.params))
        .with("auth", b64(new.auth.as_bytes()))
        .with("masterByPassword", b64(&new.master_by_password));
    let wrong = PasswordKeys::derive("wrong", &[0; 16], KdfParams::DEFAULT).unwrap();
    let change = |proof: Value| {
        Value::object()
            .with("proof", proof)
            .with("password", pw.clone())
    };
    assert_eq!(
        s.call(
            "PUT",
            "/api/vault/account/keys",
            Some(&ta),
            &change(Value::object().with("auth", b64(wrong.auth.as_bytes())))
        )
        .0,
        403
    );
    let (st, v) = s.call(
        "PUT",
        "/api/vault/account/keys",
        Some(&ta),
        &change(Value::object().with("auth", b64(alice_auth(&s, "alice password").as_bytes()))),
    );
    assert_eq!(st, 200, "{}", v.to_json());
    let ta_new = v.str_or("token", "").to_string();
    assert_eq!(
        s.call("GET", "/api/vault/account", Some(&ta2), &Value::Null)
            .0,
        401,
        "old sessions end"
    );
    assert_eq!(
        s.call("GET", "/api/vault/account", Some(&ta_new), &Value::Null)
            .0,
        200
    );
    assert!(login(&s, "alice", "alice password").is_err());
    let (ta3, alice3) = login(&s, "alice", "new alice password").unwrap();
    assert_eq!(
        download(&s, &ta3, &alice3, &id).unwrap().0,
        song,
        "files survive a password change"
    );

    // Recovery: forgotten password, the code unlocks; then a new password
    // and a new code.
    let (st, v) = s.call(
        "POST",
        "/api/vault/recover",
        None,
        &Value::object().with("user", "alice").with(
            "recoveryAuth",
            b64(bl_vault::keys::RecoveryKeys::derive(&alice_code)
                .auth
                .as_bytes()),
        ),
    );
    assert_eq!(st, 200);
    let tr = v.str_or("token", "").to_string();
    let rec = Account::recover("alice", &alice_code, &bundle_from(v.get("bundle"))).unwrap();
    let np = rec.change_password("third password").unwrap();
    let (rc, code2) = rec.new_recovery_code().unwrap();
    let body = Value::object()
        .with(
            "proof",
            Value::object().with(
                "recoveryAuth",
                b64(bl_vault::keys::RecoveryKeys::derive(&alice_code)
                    .auth
                    .as_bytes()),
            ),
        )
        .with(
            "password",
            Value::object()
                .with("salt", b64(&np.salt))
                .with("params", params_json(np.params))
                .with("auth", b64(np.auth.as_bytes()))
                .with("masterByPassword", b64(&np.master_by_password)),
        )
        .with(
            "recovery",
            Value::object()
                .with("recoveryAuth", b64(rc.recovery_auth.as_bytes()))
                .with("masterByRecovery", b64(&rc.master_by_recovery)),
        );
    assert_eq!(
        s.call("PUT", "/api/vault/account/keys", Some(&tr), &body).0,
        200
    );
    let (t4, alice4) = login(&s, "alice", "third password").unwrap();
    assert_eq!(download(&s, &t4, &alice4, &id).unwrap().0, song);
    let old = Value::object().with("user", "alice").with(
        "recoveryAuth",
        b64(bl_vault::keys::RecoveryKeys::derive(&alice_code)
            .auth
            .as_bytes()),
    );
    assert_eq!(
        s.call("POST", "/api/vault/recover", None, &old).0,
        401,
        "old code is void"
    );
    let _ = code2;

    let st = s
        .call("POST", "/api/vault/logout", Some(&t4), &Value::Null)
        .0;
    assert_eq!(st, 204);
    assert_eq!(
        s.call("GET", "/api/vault/account", Some(&t4), &Value::Null)
            .0,
        401
    );
}

/// The auth key of a password, derived with the stored salt.
fn alice_auth(s: &Srv, password: &str) -> Secret {
    let pre = s
        .call(
            "POST",
            "/api/vault/prelogin",
            None,
            &Value::object().with("user", "alice"),
        )
        .1;
    let salt: [u8; 16] = unb64(pre.get("salt")).try_into().unwrap();
    PasswordKeys::derive(password, &salt, KdfParams::DEFAULT)
        .unwrap()
        .auth
}

#[test]
fn restart_quota_limits_and_password_protected_server() {
    let s = start("restart", |c| {
        c.password = Some("server password".into());
        c.vault_quota_mb = 1;
    });
    let (t, alice, _) = register(&s, "alice", "pw one");
    // An account opens the library of a password-protected server.
    assert_eq!(s.req("GET", "/api/library", None, &[]).status, 401);
    assert_eq!(s.req("GET", "/api/library", Some(&t), &[]).status, 200);
    let (id, _) = upload(&s, &t, &alice, &vec![5u8; 500_000], "a");
    // Quota: 1 MiB.
    let key = Secret::random().unwrap();
    let (h, _) = file::encrypt(&key, &vec![0u8; 700_000]).unwrap();
    let meta = file::seal_meta(&key, &h, b"{}").unwrap();
    let body = Value::object()
        .with("header", b64(&h.to_bytes()))
        .with("meta", b64(&meta))
        .with("signature", b64(&[0; 64]))
        .with("key", b64(&[0u8; sealed::SEALED_KEY_LEN]));
    assert_eq!(s.call("POST", "/api/vault/files", Some(&t), &body).0, 507);

    // Restart on the same data folder: accounts, sessions and files stay.
    let data = s.data.clone();
    let base_cfg = |c: &mut Config| c.password = Some("server password".into());
    std::mem::forget(s); // keep the folder; the old server keeps running on its port
    let s2 = start_on(&data, base_cfg);
    assert_eq!(
        s2.call("GET", "/api/vault/account", Some(&t), &Value::Null)
            .0,
        200
    );
    let (plain, _) = download(&s2, &t, &alice, &id).unwrap();
    assert_eq!(plain, vec![5u8; 500_000]);
    let pepper = std::fs::read(data.join("vault/pepper")).unwrap();
    assert_eq!(pepper.len(), 32);

    // Registration switched off: even valid invitations are refused.
    let s3 = start("closed", |c| c.registration = false);
    let (_, reg, _) = Account::register("carl", "pw").unwrap();
    let body = Value::object()
        .with("invite", s3.invite())
        .with("user", "carl")
        .with("auth", b64(reg.auth.as_bytes()))
        .with("recoveryAuth", b64(reg.recovery_auth.as_bytes()))
        .with("bundle", bundle_json(&reg.bundle));
    let (st, v) = s3.call("POST", "/api/vault/register", None, &body);
    assert_eq!(st, 403);
    assert!(v.str_or("error", "").contains("disabled"));

    // Brute force: the account locks for a while after 10 wrong passwords.
    for _ in 0..10 {
        assert_eq!(login(&s2, "alice", "guess").err(), Some(401));
    }
    assert_eq!(login(&s2, "alice", "pw one").err(), Some(429));

    // A configured pepper file that is missing stops the start.
    let mut cfg = Config {
        data_dir: data.join("other"),
        bind: "127.0.0.1:0".into(),
        ..Default::default()
    };
    cfg.vault_pepper_file = Some(data.join("missing-pepper"));
    assert!(beatlumen_server::start(cfg).is_err());
}

fn start_on(data: &std::path::Path, extra: impl FnOnce(&mut Config)) -> Srv {
    let mut cfg = Config {
        data_dir: data.to_path_buf(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        ..Default::default()
    };
    extra(&mut cfg);
    let server = beatlumen_server::start(cfg).expect("server starts");
    let base = format!("http://{}", server.addr);
    Srv {
        _server: server,
        base,
        data: data.to_path_buf(),
    }
}

#[test]
fn push_on_share() {
    let s = start("push", |c| c.push_allow_private = true);
    let (ta, alice, _) = register(&s, "alice", "pw one");
    let (tb, _bob, _) = register(&s, "bob", "pw two");
    let (endpoint, rx) = common::fake_push_service();
    let sub = bl_vault::webpush::Subscription::generate().unwrap();
    let body = Value::object()
        .with("endpoint", endpoint.as_str())
        .with("p256dh", b64(&sub.public()))
        .with("auth", b64(&sub.auth))
        .with("name", "Bob's phone");
    let (st, v) = s.call("POST", "/api/vault/push", Some(&tb), &body);
    assert_eq!(st, 201, "{}", v.to_json());
    let (_, list) = s.call("GET", "/api/vault/push", Some(&tb), &Value::Null);
    assert_eq!(list.get("subscriptions").as_array().unwrap().len(), 1);
    let vapid = list.str_or("vapidKey", "").to_string();
    assert_eq!(vapid.len(), 87);

    let (id, key) = upload(&s, &ta, &alice, b"song", "a.mp3");
    assert_eq!(share(&s, &ta, &id, &key, "bob"), 204);
    let (head, msg) = rx
        .recv_timeout(std::time::Duration::from_secs(10))
        .expect("push sent");
    let head = head.to_ascii_lowercase();
    assert!(head.contains("content-encoding: aes128gcm"), "{head}");
    assert!(
        head.contains(&format!("k={}", vapid.to_ascii_lowercase())),
        "VAPID key in header"
    );
    let plain = sub.decrypt(&msg).unwrap();
    let v = bl_json::parse_bytes(&plain).unwrap();
    assert_eq!(v.str_or("type", ""), "share");
    assert_eq!(v.str_or("from", ""), "alice");
    assert_eq!(v.str_or("file", ""), id);
    // The push service saw no names: the body is ciphertext.
    assert!(!msg.windows(5).any(|w| w == b"alice"));

    // Private endpoints are refused without the option, bad keys too.
    let s2 = start("push2", |_| {});
    let (tc, _, _) = register(&s2, "carl", "pw three");
    assert_eq!(s2.call("POST", "/api/vault/push", Some(&tc), &body).0, 400);
    let mut bad = body.clone();
    bad.set("p256dh", b64(&[4; 65]));
    assert_eq!(s.call("POST", "/api/vault/push", Some(&tb), &bad).0, 400);
    let sid = list.get("subscriptions").at(0).str_or("id", "").to_string();
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/push/{sid}"), Some(&tb), &[])
            .status,
        204
    );
}

#[test]
fn accounts_have_their_own_playlists_and_presets() {
    let s = start("userdata", |c| c.password = Some("server password".into()));
    let (ta, _, _) = register(&s, "alice", "pw one");
    let (tb, _, _) = register(&s, "bob", "pw two");
    let (st, v) = s.call(
        "POST",
        "/api/login",
        None,
        &Value::object().with("password", "server password"),
    );
    assert_eq!(st, 200);
    let shared = v.str_or("token", "").to_string();
    let names = |t: &str| -> Vec<String> {
        let (st, v) = s.call("GET", "/api/playlists", Some(t), &Value::Null);
        assert_eq!(st, 200);
        v.as_array()
            .unwrap()
            .iter()
            .map(|p| p.str_or("name", "").to_string())
            .collect()
    };
    let create = |t: &str, name: &str| {
        let body = Value::object().with("name", name).with("kind", "track");
        assert_eq!(s.call("POST", "/api/playlists", Some(t), &body).0, 201);
    };
    create(&ta, "Alice mix");
    create(&tb, "Bob mix");
    create(&shared, "Household");
    assert_eq!(names(&ta), vec!["Alice mix"]);
    assert_eq!(names(&tb), vec!["Bob mix"]);
    assert_eq!(names(&shared), vec!["Household"]);

    // Presets as well.
    let hfx = br#"{"name":"Mine","effects":[]}"#;
    assert!(matches!(
        s.req("PUT", "/api/presets/Mine", Some(&ta), hfx).status,
        200 | 201 | 204
    ));
    let has = |t: &str| {
        s.call("GET", "/api/presets", Some(t), &Value::Null)
            .1
            .to_json()
            .contains("Mine")
    };
    assert!(has(&ta));
    assert!(!has(&tb));
    assert!(!has(&shared));
    assert_eq!(
        s.req("GET", "/api/presets/Mine", Some(&tb), &[]).status,
        404
    );

    // Stored per user, not readable as another user's id.
    assert!(
        s.data
            .join("vault")
            .join("userdata")
            .read_dir()
            .unwrap()
            .count()
            == 2
    );
}

#[test]
fn public_links() {
    use bl_vault::link;
    let s = start("links", |_| {});
    let (ta, alice, _) = register(&s, "alice", "pw one");
    let (tb, _, _) = register(&s, "bob", "pw two");
    let plain = vec![42u8; 200_000];
    let (id, key) = upload(&s, &ta, &alice, &plain, "song.flac");
    let create = |pw: Option<&str>, expires_in: u64| -> (String, link::NewLink) {
        let l = link::create(&id, &key, pw).unwrap();
        let mut body = Value::object()
            .with("wrapped", b64(&l.wrapped))
            .with("expiresIn", expires_in);
        if let Some((salt, params)) = l.password {
            body = body.with(
                "password",
                Value::object()
                    .with("salt", b64(&salt))
                    .with("params", params_json(params))
                    .with("authHash", b64(&l.auth_hash.unwrap())),
            );
        }
        let (st, v) = s.call(
            "POST",
            &format!("/api/vault/files/{id}/links"),
            Some(&ta),
            &body,
        );
        assert_eq!(st, 201, "{}", v.to_json());
        (v.str_or("id", "").to_string(), l)
    };
    let open_meta = |k: &Secret, info: &Value, meta: &Value| -> String {
        let h = Header::parse(&unb64(info.get("header"))).unwrap();
        let m = file::open_meta(k, &h, &unb64(meta)).unwrap();
        String::from_utf8(m).unwrap()
    };

    // Without a password: the link's secret opens everything.
    let (lid, l) = create(None, 0);
    let (st, info) = s.call("GET", &format!("/api/share/{lid}"), None, &Value::Null);
    assert_eq!(st, 200);
    let keys = link::LinkKeys::derive(&l.secret, None).unwrap();
    let k = keys.unwrap(&id, &unb64(info.get("wrapped"))).unwrap();
    assert!(open_meta(&k, &info, info.get("meta")).contains("song.flac"));
    let data = s.req("GET", &format!("/api/share/{lid}/data"), None, &[]);
    assert_eq!(data.status, 200);
    assert_eq!(file::decrypt(&k, &data.body).unwrap(), plain);
    // Only the owner can create links; others cannot list or delete them.
    let (st, _) = s.call(
        "POST",
        &format!("/api/vault/files/{id}/links"),
        Some(&tb),
        &Value::object(),
    );
    assert!(st == 403 || st == 404);
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/links/{lid}"), Some(&tb), &[])
            .status,
        404
    );
    let (_, list) = s.call("GET", "/api/vault/links", Some(&ta), &Value::Null);
    assert_eq!(list.as_array().unwrap().len(), 1);
    assert_eq!(
        s.call("GET", "/api/vault/links", Some(&tb), &Value::Null)
            .1
            .as_array()
            .unwrap()
            .len(),
        0
    );

    // With a password: nothing but the KDF parameters before the proof.
    let (pid, pl) = create(Some("geheim"), 0);
    let (st, info) = s.call("GET", &format!("/api/share/{pid}"), None, &Value::Null);
    assert_eq!(st, 200);
    assert!(info.get("wrapped").is_null() && info.get("meta").is_null());
    assert_eq!(
        s.req("GET", &format!("/api/share/{pid}/data"), None, &[])
            .status,
        403
    );
    let p = info.get("password");
    let salt: [u8; 16] = unb64(p.get("salt")).try_into().unwrap();
    let params = KdfParams {
        m_kib: p.get("params").get("m").as_u64().unwrap() as u32,
        t: p.get("params").get("t").as_u64().unwrap() as u32,
        p: p.get("params").get("p").as_u64().unwrap() as u32,
    };
    let wrong = link::LinkKeys::derive(&pl.secret, Some(("falsch", &salt, params))).unwrap();
    let unlock = |k: &link::LinkKeys| {
        s.call(
            "POST",
            &format!("/api/share/{pid}/unlock"),
            None,
            &Value::object().with("auth", b64(k.auth.as_ref().unwrap().as_bytes())),
        )
    };
    assert_eq!(unlock(&wrong).0, 403);
    let right = link::LinkKeys::derive(&pl.secret, Some(("geheim", &salt, params))).unwrap();
    let (st, u) = unlock(&right);
    assert_eq!(st, 200);
    let k = right.unwrap(&id, &unb64(u.get("wrapped"))).unwrap();
    assert!(open_meta(&k, &info, u.get("meta")).contains("song.flac"));
    let token = u.str_or("token", "");
    let data = s.req(
        "GET",
        &format!("/api/share/{pid}/data?token={token}"),
        None,
        &[],
    );
    assert_eq!(data.status, 200);
    assert_eq!(file::decrypt(&k, &data.body).unwrap(), plain);
    // A token of one link does not open another.
    let (lid2, _) = create(Some("x"), 0);
    assert_eq!(
        s.req(
            "GET",
            &format!("/api/share/{lid2}/data?token={token}"),
            None,
            &[]
        )
        .status,
        403
    );
    // Too many wrong passwords block the link for a while.
    for _ in 0..10 {
        unlock(&wrong);
    }
    assert_eq!(unlock(&right).0, 429);

    // Expiry, revoking, deleting the file.
    let (eid, _) = create(None, 1);
    std::thread::sleep(std::time::Duration::from_millis(2100));
    assert_eq!(
        s.req("GET", &format!("/api/share/{eid}"), None, &[]).status,
        404
    );
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/links/{lid}"), Some(&ta), &[])
            .status,
        204
    );
    assert_eq!(
        s.req("GET", &format!("/api/share/{lid}"), None, &[]).status,
        404
    );
    assert_eq!(
        s.req("GET", &format!("/api/share/{lid2}"), None, &[])
            .status,
        200
    );
    assert_eq!(
        s.req("DELETE", &format!("/api/vault/files/{id}"), Some(&ta), &[])
            .status,
        204
    );
    assert_eq!(
        s.req("GET", &format!("/api/share/{lid2}"), None, &[])
            .status,
        404
    );
    assert_eq!(s.req("GET", "/api/share/not-a-link", None, &[]).status, 404);
}
