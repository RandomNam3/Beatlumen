// Beatlumen sign-in page: passkeys (WebAuthn with the PRF extension) and
// passwords (filled in by the browser or a password manager) unlock the
// account here, with the same cryptography as the apps (vault.wasm, built
// from crates/bl-vault-wasm). The result goes to the app that asked for
// it, sealed to its one-time key, or to the web client; the server only
// ever sees what it sees from the apps. Also manages passkeys and the
// password switch. See docs/ENCRYPTION.md.
//
// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';

(function () {
  const $ = (id) => document.getElementById(id);
  const params = new URLSearchParams(location.search);
  const mode = params.get('mode') || 'manage';         // app | web | manage
  const state = params.get('state') || '';
  const redirect = params.get('redirect') || '';
  const appKeyText = params.get('key') || '';

  // ---- translations ------------------------------------------------------
  const de = {
    'Sign in': 'Anmelden',
    'Check that the app shows this code:': 'Prüfe, dass die App diesen Code zeigt:',
    'Sign in with a passkey': 'Mit Passkey anmelden',
    'or with your password': 'oder mit deinem Passwort',
    'User name': 'Benutzername',
    'Password': 'Passwort',
    'Passkeys': 'Passkeys',
    'A passkey unlocks your encrypted files without a password, on this device or your phone.': 'Ein Passkey entsperrt deine verschlüsselten Dateien ohne Passwort, auf diesem Gerät oder deinem Handy.',
    'Name of the new passkey': 'Name des neuen Passkeys',
    'Add passkey': 'Passkey hinzufügen',
    'Turn the password off': 'Passwort ausschalten',
    'New password': 'Neues Passwort',
    'Repeat password': 'Passwort wiederholen',
    'Set password': 'Passwort festlegen',
    'Sign out': 'Abmelden',
    'Remove': 'Entfernen',
    'Unlocking…': 'Entsperren…',
    'Wrong user name or password.': 'Falscher Benutzername oder falsches Passwort.',
    'Too many attempts; try again later.': 'Zu viele Versuche; bitte später erneut versuchen.',
    'This passkey cannot unlock encrypted files (its provider does not support the PRF extension). Use another passkey or your password.': 'Dieser Passkey kann verschlüsselte Dateien nicht entsperren (sein Anbieter unterstützt die PRF-Erweiterung nicht). Nutze einen anderen Passkey oder dein Passwort.',
    'Passkeys are not set up on this server.': 'Passkeys sind auf diesem Server nicht eingerichtet.',
    'Signed in. You can return to the app now.': 'Angemeldet. Du kannst jetzt zur App zurückkehren.',
    'Signed in.': 'Angemeldet.',
    'This sign-in link is not valid.': 'Dieser Anmelde-Link ist ungültig.',
    'The password is on.': 'Das Passwort ist eingeschaltet.',
    'The password is off: only passkeys and the recovery code unlock the account.': 'Das Passwort ist ausgeschaltet: nur Passkeys und der Wiederherstellungscode entsperren das Konto.',
    'Add a passkey before turning the password off.': 'Füge einen Passkey hinzu, bevor du das Passwort ausschaltest.',
    'Passkey added.': 'Passkey hinzugefügt.',
    'Passkey removed.': 'Passkey entfernt.',
    'Password turned off.': 'Passwort ausgeschaltet.',
    'Password set.': 'Passwort festgelegt.',
    'The passwords do not match.': 'Die Passwörter stimmen nicht überein.',
    'last used': 'zuletzt benutzt',
    'Signed out.': 'Abgemeldet.',
    'Turn the password off? Only your passkeys and the recovery code will unlock the account.': 'Passwort ausschalten? Dann entsperren nur deine Passkeys und der Wiederherstellungscode das Konto.',
  };
  const lang = (navigator.language || 'en').toLowerCase().startsWith('de') ? 'de' : 'en';
  const tr = (s) => (lang === 'de' && de[s]) || s;
  document.documentElement.lang = lang;
  document.querySelectorAll('[data-t]').forEach((e) => { e.textContent = tr(e.dataset.t); });
  document.title = 'Beatlumen – ' + tr('Sign in');

  const status = (text, error) => {
    $('status').textContent = text || '';
    $('status').className = error ? 'error' : '';
  };

  // ---- encodings ---------------------------------------------------------
  const b64u = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf))).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
  const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf)));
  const unb64 = (s) => {
    const t = s.replace(/-/g, '+').replace(/_/g, '/');
    const bin = atob(t + '==='.slice((t.length + 3) % 4));
    return Uint8Array.from(bin, (c) => c.charCodeAt(0));
  };
  const utf8 = (s) => new TextEncoder().encode(s);

  // ---- vault.wasm --------------------------------------------------------
  let wasm = null;
  async function loadWasm() {
    const bytes = await (await fetch('vault.wasm')).arrayBuffer();
    const pinned = document.querySelector('meta[name="beatlumen-vault-wasm"]')?.content || '';
    if (pinned && crypto.subtle) {
      const d = 'sha256-' + b64(await crypto.subtle.digest('SHA-256', bytes));
      if (d !== pinned)
        throw new Error('vault.wasm was modified (integrity check failed)');
    }
    const env = {
      host_random: (ptr, len) => crypto.getRandomValues(new Uint8Array(wasm.memory.buffer, ptr, len)),
    };
    const { instance } = await WebAssembly.instantiate(bytes, { env });
    wasm = instance.exports;
  }

  // Calls an exported function with byte/string arguments ({b: bytes},
  // numbers as they are); returns the result bytes or throws.
  function call(name, ...args) {
    const allocs = [];
    const flat = [];
    for (const a of args) {
      if (typeof a === 'number') { flat.push(a); continue; }
      const bytes = typeof a === 'string' ? utf8(a) : new Uint8Array(a);
      const ptr = wasm.bl_alloc(bytes.length);
      new Uint8Array(wasm.memory.buffer, ptr, bytes.length).set(bytes);
      allocs.push([ptr, bytes.length]);
      flat.push(ptr);
      if (a.withLen !== false) flat.push(bytes.length);
    }
    const rc = wasm[name](...flat);
    const out = new Uint8Array(wasm.memory.buffer, wasm.bl_out_ptr(), wasm.bl_out_len()).slice();
    for (const [p, n] of allocs) wasm.bl_free(p, n);
    if (rc !== 0) throw new Error(new TextDecoder().decode(out));
    return out;
  }
  // A fixed-size argument passed without its length.
  const fixed = (bytes) => Object.assign(new Uint8Array(bytes), { withLen: false });

  // ---- server ------------------------------------------------------------
  let token = '';
  async function api(method, path, body) {
    const headers = { 'Content-Type': 'application/json' };
    if (token) headers.Authorization = 'Bearer ' + token;
    const r = await fetch(path, { method, headers, body: body === undefined ? undefined : JSON.stringify(body), credentials: 'omit' });
    let v = null;
    try { v = await r.json(); } catch (e) { /* empty body */ }
    if (!r.ok) {
      const e = new Error((v && v.error) || ('HTTP ' + r.status));
      e.status = r.status;
      throw e;
    }
    return v;
  }

  // ---- WebAuthn JSON ↔ binary ---------------------------------------------
  function requestOptions(o) {
    const pk = { ...o, challenge: unb64(o.challenge) };
    pk.allowCredentials = (o.allowCredentials || []).map((c) => ({ ...c, id: unb64(c.id) }));
    pk.extensions = { prf: { eval: { first: unb64(o.extensions.prf.eval.first) } } };
    return pk;
  }
  function creationOptions(o) {
    const pk = { ...o, challenge: unb64(o.challenge), user: { ...o.user, id: unb64(o.user.id) } };
    pk.excludeCredentials = (o.excludeCredentials || []).map((c) => ({ ...c, id: unb64(c.id) }));
    pk.extensions = { prf: { eval: { first: unb64(o.extensions.prf.eval.first) } } };
    return pk;
  }
  const prfOf = (cred) => cred.getClientExtensionResults().prf?.results?.first;

  // ---- signed-in state -----------------------------------------------------
  let session = null; // { user, master, bundle, proof }

  async function passwordSignIn(user, pw) {
    status(tr('Unlocking…'));
    const pre = await api('POST', '/api/vault/prelogin', { user });
    // Weak parameters are refused inside (a malicious server could send them).
    const auth = call('bl_pw_derive', pw, fixed(unb64(pre.salt)), pre.params.m, pre.params.t, pre.params.p);
    let r;
    try {
      r = await api('POST', '/api/vault/login', { user, auth: b64(auth) });
    } catch (e) {
      throw new Error(e.status === 401 ? tr('Wrong user name or password.') : e.status === 429 ? tr('Too many attempts; try again later.') : e.message);
    }
    const master = call('bl_pw_unwrap', r.user, unb64(r.bundle.masterByPassword));
    return { user: r.user, token: r.token, master, bundle: r.bundle, proof: { auth: b64(auth) } };
  }

  async function passkeySignIn(mediation, signal) {
    let o;
    try {
      o = await api('POST', '/api/vault/passkeys/options', { purpose: 'login' });
    } catch (e) {
      throw new Error(e.status === 404 ? tr('Passkeys are not set up on this server.') : e.message);
    }
    const cred = await navigator.credentials.get({ publicKey: requestOptions(o.publicKey), mediation, signal });
    const prf = prfOf(cred);
    if (!prf)
      throw new Error(tr('This passkey cannot unlock encrypted files (its provider does not support the PRF extension). Use another passkey or your password.'));
    status(tr('Unlocking…'));
    const r = await api('POST', '/api/vault/passkeys/login', {
      challengeId: o.challengeId,
      credentialId: b64u(cred.rawId),
      clientDataJSON: b64u(cred.response.clientDataJSON),
      authenticatorData: b64u(cred.response.authenticatorData),
      signature: b64u(cred.response.signature),
      userHandle: cred.response.userHandle ? b64u(cred.response.userHandle) : '',
    });
    const master = call('bl_passkey_unwrap', r.user, new Uint8Array(cred.rawId), new Uint8Array(prf), unb64(r.masterByPasskey));
    return { user: r.user, token: r.token, master, bundle: r.bundle, proof: { session: true } };
  }

  async function signedIn(s) {
    session = s;
    token = s.token;
    status('');
    if (mode === 'app' || mode === 'web') {
      const payload = JSON.stringify({ user: s.user, token: s.token, master: b64(s.master), bundle: s.bundle });
      const sealed = call('bl_handoff_seal', fixed(appKey), state, payload);
      if (mode === 'app') {
        const sep = redirect.includes('?') ? '&' : '?';
        showFinished(tr('Signed in. You can return to the app now.'));
        location.replace(redirect + sep + 'state=' + encodeURIComponent(state) + '&result=' + b64u(sealed));
      } else {
        window.opener.postMessage({ type: 'beatlumen-auth', state, result: b64u(sealed) }, location.origin);
        showFinished(tr('Signed in.'));
        window.close();
      }
      wipe();
      return;
    }
    await showManage();
  }

  function wipe() {
    if (session) session.master.fill(0);
    session = null;
    wasm.bl_clear();
  }

  function showFinished(text) {
    $('signin').hidden = true;
    $('manage').hidden = true;
    $('pairing').hidden = true;
    $('finished').hidden = false;
    $('finishedText').textContent = text;
  }

  // ---- management ----------------------------------------------------------
  async function showManage() {
    $('signin').hidden = true;
    $('manage').hidden = false;
    const v = await api('GET', '/api/vault/passkeys');
    const list = $('keys');
    list.textContent = '';
    for (const k of v.passkeys) {
      const li = document.createElement('li');
      const label = document.createElement('div');
      label.textContent = k.name;
      const small = document.createElement('small');
      small.textContent = tr('last used') + ' ' + new Date(k.lastUsed * 1000).toLocaleDateString(lang);
      label.appendChild(small);
      const remove = document.createElement('button');
      remove.className = 'small';
      remove.type = 'button';
      remove.textContent = tr('Remove');
      remove.onclick = () => run(async () => {
        await api('DELETE', '/api/vault/passkeys/' + k.id, { proof: session.proof });
        status(tr('Passkey removed.'));
        await showManage();
      });
      li.append(label, remove);
      list.appendChild(li);
    }
    $('pwstate').textContent = v.passwordOff ? tr('The password is off: only passkeys and the recovery code unlock the account.') : tr('The password is on.');
    $('pwoff').hidden = v.passwordOff || v.passkeys.length === 0;
    $('pwon').hidden = !v.passwordOff;
    $('pwon').elements.username.value = session.user;
  }

  async function addPasskey(name) {
    const o = await api('POST', '/api/vault/passkeys/options', { purpose: 'register' });
    const cred = await navigator.credentials.create({ publicKey: creationOptions(o.publicKey) });
    let prf = prfOf(cred);
    if (!prf) {
      // Most providers give the PRF output only when signing in: ask once.
      const lo = await api('POST', '/api/vault/passkeys/options', { purpose: 'login' });
      const req = requestOptions(lo.publicKey);
      req.allowCredentials = [{ type: 'public-key', id: cred.rawId }];
      prf = prfOf(await navigator.credentials.get({ publicKey: req }));
    }
    if (!prf)
      throw new Error(tr('This passkey cannot unlock encrypted files (its provider does not support the PRF extension). Use another passkey or your password.'));
    const wrapped = call('bl_passkey_wrap', session.user, fixed(session.master), new Uint8Array(cred.rawId), new Uint8Array(prf));
    await api('POST', '/api/vault/passkeys', {
      challengeId: o.challengeId,
      clientDataJSON: b64u(cred.response.clientDataJSON),
      attestationObject: b64u(cred.response.attestationObject),
      name: name || navigator.platform || 'Passkey',
      masterByPasskey: b64(wrapped),
      proof: session.proof,
    });
    status(tr('Passkey added.'));
    await showManage();
  }

  // Runs an action, showing errors.
  async function run(f) {
    try {
      await f();
    } catch (e) {
      if (e.name === 'NotAllowedError' || e.name === 'AbortError')
        status('');
      else
        status(e.message || String(e), true);
    }
  }

  // ---- start ---------------------------------------------------------------
  let appKey = null;
  let conditional = null;

  async function start() {
    await loadWasm();
    if (mode === 'app' || mode === 'web') {
      const key = appKeyText ? unb64(appKeyText) : null;
      // Results go only to the app on this device (local callback or the
      // app's own link) or to the web client that opened this page.
      const okRedirect = mode === 'web' ? !!window.opener
        : /^http:\/\/(127\.0\.0\.1|localhost|\[::1\]):\d+\//.test(redirect) || redirect.startsWith('beatlumen://');
      if (!key || key.length !== 32 || !state || !okRedirect) {
        status(tr('This sign-in link is not valid.'), true);
        return;
      }
      appKey = key;
      $('code').textContent = new TextDecoder().decode(call('bl_pairing_code', fixed(appKey)));
      $('pairing').hidden = mode !== 'app';
    }
    $('signin').hidden = false;
    $('pwform').onsubmit = (e) => {
      e.preventDefault();
      conditional?.abort();
      run(async () => signedIn(await passwordSignIn($('user').value.trim().toLowerCase(), $('pw').value)));
    };
    $('passkey').onclick = () => {
      conditional?.abort();
      run(async () => signedIn(await passkeySignIn('optional')));
    };
    $('addform').onsubmit = (e) => {
      e.preventDefault();
      run(() => addPasskey($('keyname').value.trim()));
    };
    $('pwoff').onclick = () => run(async () => {
      if (!confirm(tr('Turn the password off? Only your passkeys and the recovery code will unlock the account.')))
        return;
      await api('PUT', '/api/vault/account/keys', { proof: session.proof, passwordOff: true });
      // The password proof is void now.
      session.proof = { session: true };
      status(tr('Password turned off.'));
      await showManage();
    });
    $('pwon').onsubmit = (e) => {
      e.preventDefault();
      run(async () => {
        if ($('newpw').value !== $('newpw2').value)
          throw new Error(tr('The passwords do not match.'));
        const pw = JSON.parse(new TextDecoder().decode(call('bl_password_set', session.user, fixed(session.master), $('newpw').value)));
        const r = await api('PUT', '/api/vault/account/keys', { proof: session.proof, password: pw });
        if (r && r.token) token = r.token;
        session.proof = { auth: pw.auth };
        $('newpw').value = $('newpw2').value = '';
        status(tr('Password set.'));
        await showManage();
      });
    };
    $('done').onclick = () => run(async () => {
      await api('POST', '/api/vault/logout');
      token = '';
      wipe();
      showFinished(tr('Signed out.'));
    });
    // Passkeys offered in the user name field's autofill, where supported.
    if (window.PublicKeyCredential?.isConditionalMediationAvailable && await PublicKeyCredential.isConditionalMediationAvailable()) {
      conditional = new AbortController();
      run(async () => signedIn(await passkeySignIn('conditional', conditional.signal)));
    }
  }

  start().catch((e) => status(e.message || String(e), true));
})();
