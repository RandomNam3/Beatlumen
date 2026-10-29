// Beatlumen public link page (/share/<id>#<secret>): downloads the
// encrypted file and decrypts it here with vault.wasm (built from
// crates/bl-vault-wasm). The secret in the URL fragment never reaches the
// server; for links with a password the server only hands out the file
// after a proof derived from the password and the secret. See
// docs/ENCRYPTION.md ("Public links").
//
// SPDX-License-Identifier: AGPL-3.0-or-later
'use strict';

(function () {
  const $ = (id) => document.getElementById(id);
  const de = {
    'Shared file': 'Geteilte Datei',
    'This link is protected with a password.': 'Dieser Link ist mit einem Passwort geschützt.',
    'Password': 'Passwort',
    'Open': 'Öffnen',
    'Download': 'Herunterladen',
    'The file is decrypted in this browser; the server cannot read it.': 'Die Datei wird in diesem Browser entschlüsselt; der Server kann sie nicht lesen.',
    'Checking the password…': 'Passwort wird geprüft …',
    'Wrong password.': 'Falsches Passwort.',
    'Too many attempts; try again later.': 'Zu viele Versuche; bitte später erneut versuchen.',
    'Downloading…': 'Wird heruntergeladen …',
    'This link does not exist or has expired.': 'Dieser Link existiert nicht oder ist abgelaufen.',
    'The link is incomplete (the part after # is missing).': 'Der Link ist unvollständig (der Teil nach # fehlt).',
    'The file could not be decrypted. Is the link complete?': 'Die Datei konnte nicht entschlüsselt werden. Ist der Link vollständig?',
    'Available until': 'Verfügbar bis',
  };
  const lang = (navigator.language || 'en').startsWith('de') ? 'de' : 'en';
  const tr = (s) => (lang === 'de' && de[s]) || s;
  document.documentElement.lang = lang;
  document.querySelectorAll('[data-t]').forEach((e) => { e.textContent = tr(e.dataset.t); });
  document.title = 'Beatlumen – ' + tr('Shared file');

  const status = (text, error) => {
    $('status').textContent = text || '';
    $('status').className = error ? 'error' : '';
  };
  const b64 = (buf) => btoa(String.fromCharCode(...new Uint8Array(buf)));
  const unb64 = (s) => Uint8Array.from(atob(s), (c) => c.charCodeAt(0));
  const utf8 = (s) => new TextEncoder().encode(s);

  // ---- vault.wasm (same loader as the sign-in page) ----------------------
  let wasm = null;
  async function loadWasm() {
    const bytes = await (await fetch('../auth/vault.wasm')).arrayBuffer();
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
  // Bytes/strings become (pointer, length); numbers stay; null is a null pointer.
  function call(name, ...args) {
    const allocs = [];
    const flat = [];
    for (const a of args) {
      if (typeof a === 'number') { flat.push(a); continue; }
      if (a === null) { flat.push(0); continue; }
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
  const fixed = (bytes) => Object.assign(new Uint8Array(bytes), { withLen: false });

  // ---- the link ----------------------------------------------------------
  const id = location.pathname.split('/').filter(Boolean).pop() || '';
  const secret = location.hash.replace(/^#/, '');
  const base = '../api/share/' + encodeURIComponent(id);

  const MIME = {
    mp3: 'audio/mpeg', flac: 'audio/flac', ogg: 'audio/ogg', oga: 'audio/ogg', opus: 'audio/ogg',
    m4a: 'audio/mp4', mp4: 'audio/mp4', aac: 'audio/aac', wav: 'audio/wav', webm: 'audio/webm',
    jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', webp: 'image/webp', gif: 'image/gif',
  };
  const fmtSize = (n) => n > 1e6 ? (n / 1e6).toFixed(1) + ' MB' : Math.max(1, Math.round(n / 1e3)) + ' kB';
  const fmtDur = (ms) => { const s = Math.round(ms / 1000); return Math.floor(s / 60) + ':' + String(s % 60).padStart(2, '0'); };

  let info = null;
  async function start() {
    if (!/^[0-9a-f]{64}$/.test(secret)) {
      status(tr('The link is incomplete (the part after # is missing).'), true);
      return;
    }
    const r = await fetch(base, { credentials: 'omit' });
    if (!r.ok) {
      status(tr('This link does not exist or has expired.'), true);
      return;
    }
    info = await r.json();
    await loadWasm();
    if (info.password) {
      $('pwform').hidden = false;
      $('pw').focus();
    } else {
      call('bl_link_derive', secret, null, 0, null, 0, 0, 0);
      await open(info.wrapped, info.meta, '');
    }
  }

  $('pwform').addEventListener('submit', async (e) => {
    e.preventDefault();
    status(tr('Checking the password…'));
    try {
      const p = info.password;
      const auth = call('bl_link_derive', secret, $('pw').value, fixed(unb64(p.salt)), p.params.m, p.params.t, p.params.p);
      const r = await fetch(base + '/unlock', {
        method: 'POST', credentials: 'omit',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ auth: b64(auth) }),
      });
      if (r.status === 429) { status(tr('Too many attempts; try again later.'), true); return; }
      if (!r.ok) { status(tr('Wrong password.'), true); return; }
      const u = await r.json();
      $('pwform').hidden = true;
      await open(u.wrapped, u.meta, u.token);
    } catch (err) {
      status(String(err.message || err), true);
    }
  });

  async function open(wrapped, meta, token) {
    let m;
    try {
      const out = call('bl_link_open', info.file, unb64(wrapped), unb64(info.header), unb64(meta));
      m = JSON.parse(new TextDecoder().decode(out));
    } catch (err) {
      status(tr('The file could not be decrypted. Is the link complete?'), true);
      return;
    }
    const name = m.name || 'file';
    $('file').hidden = false;
    $('name').textContent = m.title ? (m.artist ? m.artist + ' – ' : '') + m.title : name;
    const details = [name !== $('name').textContent ? name : '', m.durationMs ? fmtDur(m.durationMs) : '', fmtSize(m.size || info.size)];
    if (info.expires) details.push(tr('Available until') + ' ' + new Date(info.expires * 1000).toLocaleDateString(lang));
    $('details').textContent = details.filter(Boolean).join(' · ');
    await download(name, token);
  }

  // Streams the file and decrypts it chunk by chunk.
  async function download(name, token) {
    status(tr('Downloading…'));
    $('progress').hidden = false;
    const chunks = Number(wasm.bl_link_chunks());
    const size = Number(wasm.bl_link_chunk_size());
    const header = Number(wasm.bl_link_header_size());
    const r = await fetch(base + '/data' + (token ? '?token=' + encodeURIComponent(token) : ''), { credentials: 'omit' });
    if (!r.ok || !r.body) { status(tr('This link does not exist or has expired.'), true); return; }
    const reader = r.body.getReader();
    const parts = [];
    let buf = new Uint8Array(0);
    let skip = header;
    let index = 0;
    for (;;) {
      const { done, value } = await reader.read();
      if (value) {
        const joined = new Uint8Array(buf.length + value.length);
        joined.set(buf); joined.set(value, buf.length);
        buf = joined;
        if (skip > 0) { const n = Math.min(skip, buf.length); buf = buf.slice(n); skip -= n; }
        while (buf.length >= size && index < chunks) {
          parts.push(call('bl_link_chunk', index, buf.slice(0, size)));
          buf = buf.slice(size);
          index++;
          $('progress').value = index / chunks;
        }
      }
      if (done) break;
    }
    wasm.bl_link_clear();
    if (index !== chunks) { status(tr('The file could not be decrypted. Is the link complete?'), true); return; }
    const ext = (name.split('.').pop() || '').toLowerCase();
    const type = MIME[ext] || 'application/octet-stream';
    const url = URL.createObjectURL(new Blob(parts, { type }));
    $('progress').hidden = true;
    status('');
    $('ready').hidden = false;
    $('save').href = url;
    $('save').download = name;
    if (type.startsWith('audio/')) { $('audio').src = url; $('audio').hidden = false; }
    if (type.startsWith('image/')) { $('image').src = url; $('image').hidden = false; }
  }

  start().catch((err) => status(String(err.message || err), true));
})();
