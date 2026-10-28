#!/bin/bash
# Finishes a web client build in DIR: Subresource Integrity hashes in
# index.html, the SHA-256 of the WebAssembly module (checked by start.js),
# the pre-compressed module and the integrity manifest (signed when
# BEATLUMEN_RELEASE_SIGNING_KEY is set). Called by build-web.sh.
set -euo pipefail
OUT="$1"
ROOT="$(cd "$(dirname "$0")/../../.." && pwd)"
# Subresource Integrity: the page pins the hashes of its scripts and style,
# and start.js checks the WebAssembly module before running it.
sri() { echo "sha384-$(openssl dgst -sha384 -binary "$OUT/$1" | openssl base64 -A)"; }
WASM_SHA256="$(openssl dgst -sha256 -binary "$OUT/beatlumen.wasm" | openssl base64 -A)"
sed -i \
    -e "s|<script src=\"qtloader.js\"></script>|<script src=\"qtloader.js\" integrity=\"$(sri qtloader.js)\"></script>|" \
    -e "s|<script src=\"beatlumen.js\"></script>|<script src=\"beatlumen.js\" integrity=\"$(sri beatlumen.js)\"></script>|" \
    -e "s|<script src=\"start.js\"></script>|<script src=\"start.js\" integrity=\"$(sri start.js)\"></script>|" \
    -e "s|<link rel=\"stylesheet\" href=\"beatlumen.css\">|<link rel=\"stylesheet\" href=\"beatlumen.css\" integrity=\"$(sri beatlumen.css)\">|" \
    -e "s|<meta name=\"beatlumen-wasm\" content=\"\">|<meta name=\"beatlumen-wasm\" content=\"sha256-$WASM_SHA256\">|" \
    "$OUT/index.html"
grep -q 'integrity="sha384-' "$OUT/index.html" || { echo "SRI injection failed" >&2; exit 1; }
# Pre-compressed copies for servers that support them.
gzip -9 -k -f "$OUT/beatlumen.wasm"
# Integrity manifest (hashes of all files), signed when
# BEATLUMEN_RELEASE_SIGNING_KEY is set (official builds).
cargo run -q --release -p beatlumen-server --manifest-path "$ROOT/Cargo.toml" -- web-manifest "$OUT"
