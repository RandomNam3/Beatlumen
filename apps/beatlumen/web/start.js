// Starts the Qt/WebAssembly build of Beatlumen (no inline scripts, so the
// server can send a strict Content-Security-Policy).
//
// The WebAssembly module is checked against the SHA-256 in the page
// (written by build-web.sh) before it is compiled, like Subresource
// Integrity does for the scripts.
async function verifiedModule() {
  const pinned = document.querySelector('meta[name="beatlumen-wasm"]')?.content || '';
  const response = await fetch('beatlumen.wasm');
  if (!response.ok)
    throw new Error('beatlumen.wasm: HTTP ' + response.status);
  const bytes = await response.arrayBuffer();
  // crypto.subtle exists only on secure pages (https, localhost); over
  // plain http a proxy could change the page itself anyway.
  if (pinned && crypto.subtle) {
    const digest = await crypto.subtle.digest('SHA-256', bytes);
    const actual = 'sha256-' + btoa(String.fromCharCode(...new Uint8Array(digest)));
    if (actual !== pinned)
      throw new Error('beatlumen.wasm was modified (integrity check failed)');
  }
  return WebAssembly.compile(bytes);
}

(async function () {
  const splash = document.getElementById('splash');
  const screen = document.getElementById('screen');
  const status = document.getElementById('status');
  try {
    status.textContent = 'Loading…';
    await qtLoad({
      qt: {
        module: verifiedModule(),
        onLoaded: function () { splash.style.display = 'none'; screen.style.display = 'block'; },
        onExit: function (e) {
          screen.style.display = 'none';
          splash.style.display = 'flex';
          status.textContent = 'Beatlumen stopped' + (e && e.text ? ': ' + e.text : '');
        },
        entryFunction: window.beatlumen_entry,
        containerElements: [screen]
      }
    });
  } catch (e) {
    status.textContent = 'Beatlumen could not start: ' + e;
    console.error(e);
  }
})();
