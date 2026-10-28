//! Passkeys against a running server, with a software authenticator
//! (P-256, "none" attestation, PRF simulated by a fixed secret).

mod common;

use beatlumen_server::{Config, Server};
use bl_json::Value;
use bl_net::{base64_decode, base64_encode, client};
use bl_vault::keys::{self, Account};
use bl_vault::webpush::base64url;
use p256::ecdsa::{signature::Signer, Signature, SigningKey};

const ORIGIN: &str = "http://localhost:7878";

struct Srv {
    _server: Server,
    base: String,
    data: std::path::PathBuf,
}

impl Drop for Srv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.data);
    }
}

fn start(name: &str) -> Srv {
    let data = std::env::temp_dir().join(format!("bl-passkey-it-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&data);
    let server = beatlumen_server::start(Config {
        data_dir: data.clone(),
        bind: "127.0.0.1:0".into(),
        rescan_interval: 0,
        public_url: Some(ORIGIN.into()),
        android_certs: vec!["ab".repeat(32)],
        ..Default::default()
    })
    .unwrap();
    Srv {
        base: format!("http://{}", server.addr),
        _server: server,
        data,
    }
}

impl Srv {
    fn call(&self, method: &str, path: &str, token: Option<&str>, v: &Value) -> (u16, Value) {
        let mut h = vec![("Content-Type".to_string(), "application/json".to_string())];
        if let Some(t) = token {
            h.push(("Authorization".into(), format!("Bearer {t}")));
        }
        let r = client::request(
            method,
            &format!("{}{path}", self.base),
            &h,
            v.to_json().as_bytes(),
            &client::Options::default(),
        )
        .unwrap();
        (
            r.status,
            bl_json::parse_bytes(&r.body).unwrap_or(Value::Null),
        )
    }
}

/// Minimal CBOR encoder for the test authenticator.
fn cbor_head(major: u8, n: u64, out: &mut Vec<u8>) {
    match n {
        0..=23 => out.push(major << 5 | n as u8),
        24..=0xff => out.extend([major << 5 | 24, n as u8]),
        _ => {
            out.push(major << 5 | 25);
            out.extend((n as u16).to_be_bytes());
        }
    }
}
fn cbor_int(i: i64, out: &mut Vec<u8>) {
    if i >= 0 {
        cbor_head(0, i as u64, out)
    } else {
        cbor_head(1, (-1 - i) as u64, out)
    }
}
fn cbor_bytes(b: &[u8], out: &mut Vec<u8>) {
    cbor_head(2, b.len() as u64, out);
    out.extend_from_slice(b);
}
fn cbor_text(t: &str, out: &mut Vec<u8>) {
    cbor_head(3, t.len() as u64, out);
    out.extend_from_slice(t.as_bytes());
}

struct Authenticator {
    key: SigningKey,
    id: Vec<u8>,
    prf: [u8; 32],
    count: u32,
}

impl Authenticator {
    fn new() -> Authenticator {
        let mut seed = [0u8; 32];
        bl_vault::random(&mut seed).unwrap();
        let mut id = vec![0u8; 16];
        bl_vault::random(&mut id).unwrap();
        let mut prf = [0u8; 32];
        bl_vault::random(&mut prf).unwrap();
        Authenticator {
            key: SigningKey::from_slice(&seed).unwrap(),
            id,
            prf,
            count: 0,
        }
    }

    fn rp_hash() -> [u8; 32] {
        bl_vault::sha256(&[b"localhost"])
    }

    fn create(&self, challenge: &str, origin: &str) -> (Vec<u8>, Vec<u8>) {
        let client = format!(
            r#"{{"type":"webauthn.create","challenge":"{challenge}","origin":"{origin}","crossOrigin":false}}"#
        );
        let point = self.key.verifying_key().to_encoded_point(false);
        let mut cose = Vec::new();
        cbor_head(5, 5, &mut cose);
        cbor_int(1, &mut cose);
        cbor_int(2, &mut cose);
        cbor_int(3, &mut cose);
        cbor_int(-7, &mut cose);
        cbor_int(-1, &mut cose);
        cbor_int(1, &mut cose);
        cbor_int(-2, &mut cose);
        cbor_bytes(point.x().unwrap(), &mut cose);
        cbor_int(-3, &mut cose);
        cbor_bytes(point.y().unwrap(), &mut cose);
        let mut auth = Self::rp_hash().to_vec();
        auth.push(0x45); // UP | UV | AT
        auth.extend(0u32.to_be_bytes());
        auth.extend([0u8; 16]);
        auth.extend((self.id.len() as u16).to_be_bytes());
        auth.extend(&self.id);
        auth.extend(cose);
        let mut att = Vec::new();
        cbor_head(5, 3, &mut att);
        cbor_text("fmt", &mut att);
        cbor_text("none", &mut att);
        cbor_text("attStmt", &mut att);
        cbor_head(5, 0, &mut att);
        cbor_text("authData", &mut att);
        cbor_bytes(&auth, &mut att);
        (client.into_bytes(), att)
    }

    fn get(&mut self, challenge: &str, origin: &str, flags: u8) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        self.count += 1;
        let client =
            format!(r#"{{"type":"webauthn.get","challenge":"{challenge}","origin":"{origin}"}}"#);
        let mut auth = Self::rp_hash().to_vec();
        auth.push(flags);
        auth.extend(self.count.to_be_bytes());
        let mut msg = auth.clone();
        msg.extend(bl_vault::sha256(&[client.as_bytes()]));
        let sig: Signature = self.key.sign(&msg);
        (client.into_bytes(), auth, sig.to_der().as_bytes().to_vec())
    }
}

fn options(s: &Srv, purpose: &str, token: Option<&str>) -> (String, String) {
    let (st, v) = s.call(
        "POST",
        "/api/vault/passkeys/options",
        token,
        &Value::object().with("purpose", purpose),
    );
    assert_eq!(st, 200, "{}", v.to_json());
    let pk = v.get("publicKey");
    if purpose == "login" {
        assert_eq!(pk.str_or("rpId", ""), "localhost");
        assert_eq!(
            pk.get("extensions")
                .get("prf")
                .get("eval")
                .str_or("first", ""),
            base64url(&keys::passkey_prf_input())
        );
    }
    (
        v.str_or("challengeId", "").to_string(),
        pk.str_or("challenge", "").to_string(),
    )
}

fn login(s: &Srv, a: &mut Authenticator, origin: &str, flags: u8) -> (u16, Value) {
    let (cid, ch) = options(s, "login", None);
    let (client, auth, sig) = a.get(&ch, origin, flags);
    let body = Value::object()
        .with("challengeId", cid)
        .with("credentialId", base64url(&a.id))
        .with("clientDataJSON", base64url(&client))
        .with("authenticatorData", base64url(&auth))
        .with("signature", base64url(&sig));
    s.call("POST", "/api/vault/passkeys/login", None, &body)
}

#[test]
fn passkey_register_login_and_password_off() {
    let s = start("main");
    // An account with a password first.
    let (acc, reg, code) = Account::register("alice", "alice password").unwrap();
    let invite = beatlumen_server::vault::create_invite(&s.data, 1).unwrap();
    let b = &reg.bundle;
    let bundle = Value::object()
        .with("salt", base64_encode(&b.salt))
        .with(
            "params",
            Value::object()
                .with("m", b.params.m_kib as u64)
                .with("t", b.params.t as u64)
                .with("p", b.params.p as u64),
        )
        .with("masterByPassword", base64_encode(&b.master_by_password))
        .with("masterByRecovery", base64_encode(&b.master_by_recovery))
        .with("identity", base64_encode(&b.identity))
        .with("public", base64_encode(&b.public.to_bytes()));
    let (st, v) = s.call(
        "POST",
        "/api/vault/register",
        None,
        &Value::object()
            .with("invite", invite)
            .with("user", "alice")
            .with("auth", base64_encode(reg.auth.as_bytes()))
            .with("recoveryAuth", base64_encode(reg.recovery_auth.as_bytes()))
            .with("bundle", bundle),
    );
    assert_eq!(st, 201);
    let token = v.str_or("token", "").to_string();
    let info = s.call("GET", "/api/info", None, &Value::Null).1;
    assert!(info.bool_or("passkeys", false));

    // Register a passkey: proof with the password, master key wrapped with
    // the PRF output.
    let mut a = Authenticator::new();
    let (cid, ch) = options(&s, "register", Some(&token));
    let (client, att) = a.create(&ch, ORIGIN);
    let wrapped = keys::wrap_master_for_passkey("alice", acc.master(), &a.id, &a.prf).unwrap();
    let reg_body = |cid: &str, client: &[u8], att: &[u8], proof: Value| {
        Value::object()
            .with("challengeId", cid)
            .with("clientDataJSON", base64url(client))
            .with("attestationObject", base64url(att))
            .with("name", "Test key")
            .with("masterByPasskey", base64_encode(&wrapped))
            .with("proof", proof)
    };
    let proof = Value::object().with("auth", base64_encode(reg.auth.as_bytes()));
    let wrong = Value::object().with("auth", base64_encode(&[1; 32]));
    assert_eq!(
        s.call(
            "POST",
            "/api/vault/passkeys",
            Some(&token),
            &reg_body(&cid, &client, &att, wrong)
        )
        .0,
        403
    );
    let (cid, ch) = options(&s, "register", Some(&token));
    // Wrong origin: refused.
    let (bad_client, att2) = a.create(&ch, "https://evil.example");
    assert_eq!(
        s.call(
            "POST",
            "/api/vault/passkeys",
            Some(&token),
            &reg_body(&cid, &bad_client, &att2, proof.clone())
        )
        .0,
        400
    );
    let (cid, ch) = options(&s, "register", Some(&token));
    let (client, att) = a.create(&ch, ORIGIN);
    let (st, v) = s.call(
        "POST",
        "/api/vault/passkeys",
        Some(&token),
        &reg_body(&cid, &client, &att, proof.clone()),
    );
    assert_eq!(st, 201, "{}", v.to_json());
    // The same challenge answers once.
    assert_eq!(
        s.call(
            "POST",
            "/api/vault/passkeys",
            Some(&token),
            &reg_body(&cid, &client, &att, proof.clone())
        )
        .0,
        400
    );
    let list = s
        .call("GET", "/api/vault/passkeys", Some(&token), &Value::Null)
        .1;
    assert_eq!(list.get("passkeys").as_array().unwrap().len(), 1);

    // Sign in with the passkey and unlock with its PRF output.
    let (st, v) = login(&s, &mut a, ORIGIN, 0x05);
    assert_eq!(st, 200, "{}", v.to_json());
    let ptoken = v.str_or("token", "").to_string();
    let w = base64_decode(v.str_or("masterByPasskey", "")).unwrap();
    let cred = base64_decode(v.str_or("credentialId", "")).unwrap();
    let master = keys::unwrap_master_with_passkey("alice", &cred, &a.prf, &w).unwrap();
    let back = Account::from_master("alice", master, b).unwrap();
    assert_eq!(back.identity.public(), acc.identity.public());
    // Wrong origin, no user verification, replayed counter, forged signature.
    assert_eq!(login(&s, &mut a, "https://evil.example", 0x05).0, 401);
    assert_eq!(login(&s, &mut a, ORIGIN, 0x01).0, 401);
    a.count = 0;
    assert_eq!(login(&s, &mut a, ORIGIN, 0x05).0, 401, "counter must grow");
    a.count = 100;
    let mut other = Authenticator::new();
    other.id = a.id.clone();
    assert_eq!(
        login(&s, &mut other, ORIGIN, 0x05).0,
        401,
        "signature of another key"
    );

    // Password off, proven by the fresh passkey session.
    let off = Value::object()
        .with("proof", Value::object().with("session", true))
        .with("passwordOff", true);
    assert_eq!(
        s.call("PUT", "/api/vault/account/keys", Some(&token), &off)
            .0,
        403,
        "old password session is not fresh"
    );
    let (st, v) = s.call("PUT", "/api/vault/account/keys", Some(&ptoken), &off);
    assert_eq!(st, 200, "{}", v.to_json());
    // The password opens nothing now; prelogin looks like an unknown user.
    let pre = s
        .call(
            "POST",
            "/api/vault/prelogin",
            None,
            &Value::object().with("user", "alice"),
        )
        .1;
    assert_ne!(
        base64_decode(pre.str_or("salt", "")).unwrap(),
        b.salt.to_vec()
    );
    let st = s
        .call(
            "POST",
            "/api/vault/login",
            None,
            &Value::object()
                .with("user", "alice")
                .with("auth", base64_encode(reg.auth.as_bytes())),
        )
        .0;
    assert_eq!(st, 401);
    let acct = s
        .call("GET", "/api/vault/account", Some(&ptoken), &Value::Null)
        .1;
    assert!(acct.get("bundle").bool_or("passwordOff", false));
    // The last passkey stays while the password is off.
    let id = base64url(&a.id);
    let del = Value::object().with("proof", Value::object().with("session", true));
    assert_eq!(
        s.call(
            "DELETE",
            &format!("/api/vault/passkeys/{id}"),
            Some(&ptoken),
            &del
        )
        .0,
        409
    );
    // The recovery code still works.
    let rauth = bl_vault::keys::RecoveryKeys::derive(&code).auth;
    let st = s
        .call(
            "POST",
            "/api/vault/recover",
            None,
            &Value::object()
                .with("user", "alice")
                .with("recoveryAuth", base64_encode(rauth.as_bytes())),
        )
        .0;
    assert_eq!(st, 200);

    // Android: the app's certificate is published for the domain.
    let r = client::request(
        "GET",
        &format!("{}/.well-known/assetlinks.json", s.base),
        &[],
        &[],
        &client::Options::default(),
    )
    .unwrap();
    assert_eq!(r.status, 200);
    let text = String::from_utf8_lossy(&r.body);
    assert!(
        text.contains("io.github.randomnam3.beatlumen") && text.contains("AB:AB:AB"),
        "{text}"
    );
    assert!(text.contains("get_login_creds"));
    let _ = common::fake_push_service;
}
