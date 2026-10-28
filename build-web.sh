#!/bin/bash
# Builds the browser version of Beatlumen (Qt for WebAssembly) into dist/web.
# The Beatlumen server serves it with --web-dir dist/web (or embeds it when
# built with BEATLUMEN_EMBED_WEB=dist/web).
#
# Requirements: emsdk 4.0.7 (EMSDK set or EMSDK_DIR pointing to it),
# Qt 6.11 for WebAssembly (single-threaded) and the host Qt
# (QT_WASM / QT_HOST, defaults below), Rust target wasm32-unknown-emscripten.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")" && pwd)"
QT_WASM="${QT_WASM:-/opt/Qt/6.11.3/wasm_singlethread}"
QT_HOST="${QT_HOST:-/opt/Qt/6.11.3/gcc_64}"
if [ -z "${EMSDK:-}" ]; then
    source "${EMSDK_DIR:?set EMSDK_DIR to your emsdk checkout}/emsdk_env.sh" >/dev/null
fi
BUILD="$ROOT/build/wasm"
mkdir -p "$BUILD"
# Some Qt installers drop the executable bit of the wrapper scripts.
chmod +x "$QT_WASM/bin/"* 2>/dev/null || true
"$QT_WASM/bin/qt-cmake" -S "$ROOT/apps/beatlumen" -B "$BUILD" -G Ninja -DCMAKE_BUILD_TYPE=Release -DQT_HOST_PATH="$QT_HOST"
cmake --build "$BUILD"
OUT="$ROOT/dist/web"
rm -rf "$OUT"
mkdir -p "$OUT"
cp "$BUILD/beatlumen.js" "$BUILD/beatlumen.wasm" "$BUILD/qtloader.js" "$OUT/"
cp "$ROOT/apps/beatlumen/web/"{index.html,start.js,beatlumen.css} "$OUT/"
cp "$ROOT/apps/beatlumen/assets/icon.png" "$OUT/icon.png"
"$ROOT/apps/beatlumen/web/finish.sh" "$OUT"
echo "==> $OUT ($(du -sh "$OUT" | cut -f1))"
