// Starts the Qt/WebAssembly build of Beatlumen (no inline scripts, so the
// server can send a strict Content-Security-Policy).
(async function () {
  const splash = document.getElementById('splash');
  const screen = document.getElementById('screen');
  const status = document.getElementById('status');
  try {
    status.textContent = 'Loading…';
    await qtLoad({
      qt: {
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
