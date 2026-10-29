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
# The sign-in page (passkeys, password managers) with its WebAssembly
# cryptography from crates/bl-vault-wasm.
if [ -d "$OUT/auth" ]; then
    cargo build -q --release -p bl-vault-wasm --target wasm32-unknown-unknown --manifest-path "$ROOT/Cargo.toml"
    cp "$ROOT/target/wasm32-unknown-unknown/release/bl_vault_wasm.wasm" "$OUT/auth/vault.wasm"
    VAULT_SHA256="$(openssl dgst -sha256 -binary "$OUT/auth/vault.wasm" | openssl base64 -A)"
    sed -i \
        -e "s|<script src=\"auth.js\"></script>|<script src=\"auth.js\" integrity=\"$(sri auth/auth.js)\"></script>|" \
        -e "s|<link rel=\"stylesheet\" href=\"auth.css\">|<link rel=\"stylesheet\" href=\"auth.css\" integrity=\"$(sri auth/auth.css)\">|" \
        -e "s|<meta name=\"beatlumen-vault-wasm\" content=\"\">|<meta name=\"beatlumen-vault-wasm\" content=\"sha256-$VAULT_SHA256\">|" \
        "$OUT/auth/index.html"
    grep -q 'integrity="sha384-' "$OUT/auth/index.html" || { echo "SRI injection (auth) failed" >&2; exit 1; }
fi
# The page of public links to encrypted files (same vault.wasm).
if [ -d "$OUT/share" ]; then
    sed -i \
        -e "s|<script src=\"share.js\"></script>|<script src=\"share.js\" integrity=\"$(sri share/share.js)\"></script>|" \
        -e "s|<link rel=\"stylesheet\" href=\"share.css\">|<link rel=\"stylesheet\" href=\"share.css\" integrity=\"$(sri share/share.css)\">|" \
        -e "s|<link rel=\"stylesheet\" href=\"../auth/auth.css\">|<link rel=\"stylesheet\" href=\"../auth/auth.css\" integrity=\"$(sri auth/auth.css)\">|" \
        -e "s|<meta name=\"beatlumen-vault-wasm\" content=\"\">|<meta name=\"beatlumen-vault-wasm\" content=\"sha256-$VAULT_SHA256\">|" \
        "$OUT/share/index.html"
    grep -q 'integrity="sha384-' "$OUT/share/index.html" || { echo "SRI injection (share) failed" >&2; exit 1; }
fi
# Pre-compressed copies for servers that support them.
gzip -9 -k -f "$OUT/beatlumen.wasm"
# Integrity manifest (hashes of all files), signed when
# BEATLUMEN_RELEASE_SIGNING_KEY is set (official builds).
cargo run -q --release -p beatlumen-server --manifest-path "$ROOT/Cargo.toml" -- web-manifest "$OUT"
