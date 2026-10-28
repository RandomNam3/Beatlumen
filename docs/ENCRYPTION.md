# End-to-end encryption of user files

Status: design (implementation in progress, see the end of this document).

Files that users upload to a Beatlumen server (their own music, backgrounds,
playlists, presets, …) are encrypted **on the user's device** before they
leave it. Only the owner and the users it shared a file with can decrypt
it. The server stores ciphertext and never sees passwords, keys or file
contents.

The music folders the admin configures on the server itself are not
covered: the server has to read those to index and stream them.

## Goals and threat model

Protected against:

* **A malicious server admin** and **an attacker who controls the server**
  (stolen backups, a compromised host, a modified server program): they
  cannot read file contents or names, cannot learn passwords from logins,
  and cannot forge or alter files without the change being detected.
* **Offline attacks on a stolen server database**: every password guess
  costs one Argon2id computation (64 MiB, deliberately slow).
* **A leaked login token**: it gives access to ciphertext only.

Not protected against (inherent limits, stated openly):

* **Metadata** the server needs to work: user names, file count, sizes
  (rounded to 64 KiB chunks), times of uploads and downloads, who shares
  with whom, IP addresses.
* **Deletion, withholding or rolling back** of stored files by the server
  (availability). Clients detect files they cannot verify, but a server can
  always refuse service.
* **A manipulated web client.** The web version is delivered by the server;
  an admin could change it to capture passwords. Integrity checks (below)
  detect manipulation, but only from outside the server: the native apps
  check the web client; their own code never comes from the server.
* **Substituted keys when sharing**: the server could hand out its own
  public key for a user. Safety numbers and key pinning (below) detect it;
  users should compare safety numbers once per contact.
* **Weak passwords**: Argon2id slows guessing, but cannot make a short
  password strong. The apps require at least 12 characters.
* **Compromised devices** (malware on the phone or computer).

## Cryptography

Implemented in `crates/bl-vault` with the RustCrypto crates (pure Rust,
pinned versions): `argon2`, `hkdf`, `sha2`, `chacha20poly1305`,
`x25519-dalek`, `ed25519-dalek`, `zeroize`, `getrandom`.

| Purpose | Algorithm |
|---|---|
| Password → key | Argon2id, 64 MiB, 3 passes, 1 lane, 16-byte random salt |
| Key derivation | HKDF-SHA-256 with fixed labels |
| Encryption | XChaCha20-Poly1305 (24-byte random nonces, no reuse risk) |
| Key exchange (sharing) | X25519 (sealed boxes) |
| Signatures | Ed25519 |
| Fingerprints | SHA-256 |

All secrets are zeroed in memory when dropped. Random numbers come from the
operating system.

## Keys of a user

```
password ──Argon2id(salt)──► password root
                                ├─HKDF "auth"──► auth key ──► server stores SHA-256(auth key ‖ pepper)
                                └─HKDF "kek"───► password key ──wraps──► master key
recovery code (256 bit) ──HKDF "recovery kek"──► recovery key ──wraps──► master key
                         └─HKDF "recovery auth"► recovery auth ──► server stores SHA-256(… ‖ pepper)
master key (random, 256 bit) ──wraps──► identity private keys
identity: Ed25519 (signing) + X25519 (receiving file keys), X25519 public key signed by the Ed25519 key
```

* The **master key** is random, not derived from the password. Changing the
  password only re-wraps it; files stay untouched.
* The server stores per user: name, password salt and Argon2 parameters,
  the two verifier hashes, the wrapped master key (twice: by password and
  by recovery code), the wrapped identity private keys and the public
  keys. Nothing of it decrypts anything without the password or the
  recovery code.
* The **pepper** is a random server secret kept outside the database (file
  `data/vault-pepper`, mode 0600): a database copy alone does not even
  allow checking password guesses.

## Registration, login, password change, recovery

1. **Registration** (the admin creates an invitation code; accounts cannot
   be created by strangers): the app generates the master key, identity
   keys and recovery code, derives the password keys and uploads the
   wrapped keys, public keys and verifiers. The recovery code is shown
   once; the user stores it offline (paper, password manager).
2. **Login**: the app asks the server for the salt and Argon2 parameters
   of the user name (for unknown names the server answers with values
   derived from a server secret, so user names cannot be probed), derives
   the auth key and sends it. The server compares the hash and returns a
   session token and the user's key bundle; the app unwraps the master key
   and identity keys locally. **The password never leaves the device.**
3. **Password change**: logged in, the app sends the current auth key, a
   new salt, the new verifier and the master key wrapped with the new
   password key.
4. **Recovery** (password forgotten): the app derives the recovery auth
   from the code, the server returns the master key wrapped by the
   recovery key, the app unwraps it and sets a new password as in 3. A new
   recovery code is then issued.
5. Login and recovery attempts are rate limited per user and per address,
   like the current password login. Sessions expire; all sessions end on a
   password change or recovery.

## Passkeys

Passkeys unlock the account without a password, on every platform:

* **Unlocking needs a secret, not only a login.** A passkey proves the
  user to the server (WebAuthn signature), and its **PRF extension**
  gives the device a secret that only this passkey produces for this
  server. HKDF turns it into a key that wraps the master key, once per
  passkey (`masterByPasskey`). A passkey without PRF support can not
  unlock the files; the apps say so.
* The PRF input is a fixed value (`SHA-256("beatlumen passkey prf v1")`):
  the output differs per passkey and server anyway, and a fixed input
  allows signing in without typing the user name first.
* The server checks registrations and signatures itself (ES256, EdDSA,
  RS256; user verification required; signature counters) and stores per
  passkey: id, public key, counter, name, the wrapped master key.
  Passkeys need the server's HTTPS address (`--public-url`), whose host
  is the WebAuthn relying party.
* **Password off**: with at least one passkey the user can turn the
  password off; the server then deletes the password verifier and the
  password wrapping of the master key. The recovery code always stays (it
  sets a new password). The last passkey can not be removed while the
  password is off.

Where passkeys and password managers are used:

* **Browser sign-in page** (`/auth/`, part of the signed web client): it
  offers passkeys (including a phone via QR code and security keys) and a
  password form that password managers and browser extensions fill in.
  It runs the same cryptography (`bl-vault` compiled to WebAssembly) and
  also manages passkeys and the password switch.
* **Apps sign in through the browser**: the app creates a one-time
  X25519 key, opens `/auth/?mode=app&key=…` in the system browser and
  shows a pairing code derived from that key; the page shows the same
  code. After unlocking, the page seals the master key and the session to
  the app's one-time key and hands it back to the app (desktop: a local
  callback on 127.0.0.1; Android: the `beatlumen://` link). The server
  only relays ciphertext, and a result can only be opened by the app
  that started the sign-in.
* **Android** additionally uses the system's Credential Manager directly
  (Android 14+): passkeys and saved passwords from the user's provider
  (Google Password Manager, Bitwarden, …) without leaving the app, and
  passkeys can be added there too. The provider signs with the app's
  origin (`android:apk-key-hash:…`), which the server accepts only for the
  signing certificates it knows (`--android-cert`; official builds are
  built in). The server publishes `/.well-known/assetlinks.json` for the
  app, which Android requires before an app may use passkeys of a web
  domain. A password typed into the app is offered to the password
  manager after it was accepted; one that came from the manager is not.
  The app checks everything the provider returns exactly as the sign-in
  page does: the PRF output stays on the device and only unwraps the
  master key there.
* The **web client** opens the sign-in page in a small window and gets
  the result the same way.

## Staying unlocked on a device

By default an app keeps the unlocked account (master key, key bundle and
session token) in the platform's protected store, so the password or
passkey is needed once per device:

* **Android**: encrypted with an AES-256-GCM key in the Android Keystore
  (usually in secure hardware; the key never leaves it). The ciphertext lives
  in the app's private storage, which is excluded from backups.
* **Windows**: DPAPI, bound to the Windows account.
* **Linux**: the Secret Service (GNOME Keyring, KWallet, KeePassXC); the
  keyring may ask to be unlocked.
* **Web**: not offered (a browser has no comparable store).

Whoever can use the device unlocked (or the operating system account) can
then open the files, which is why it can be switched off ("Stay unlocked on
this device"). "Lock" forgets the stored account. Sessions end after 90 days
without use, and a password change ends the sessions of all other devices;
a device whose session the server refuses forgets its stored account and
asks to sign in again.

## Encrypted files

Each file has a random 256-bit **file key**.

```
header:  "BLV1" ‖ version (1) ‖ file id (16) ‖ nonce prefix (16)
         ‖ chunk size (u32) ‖ chunk count (u64)                     – 49 bytes
chunks:  XChaCha20-Poly1305(file key,
                            nonce = prefix ‖ chunk index (7 bytes) ‖ last-chunk flag (1),
                            aad   = header) – 64 KiB plaintext + 16 byte tag each,
         the last chunk padded with 0x80 0x00 … to the full size
meta:    XChaCha20-Poly1305(file key, random nonce, aad = "meta" ‖ header),
         padded to 256 bytes: { name, type, size, created, kind (music/background/playlist/preset/…) }
signature: Ed25519(owner, SHA-256(header ‖ SHA-256(meta) ‖ SHA-256("key commitment" ‖ file key)))
```

* **Random access**: chunk *i* sits at a fixed offset, so audio streams and
  seeking keep working with byte ranges; the app decrypts chunk by chunk.
* **No truncation or reordering**: the chunk index and the last-chunk flag
  are part of every nonce and the whole header is authenticated with every
  chunk; a changed, missing, swapped or appended chunk fails to decrypt.
* **Sizes in whole chunks**: the last chunk is padded, so the server learns
  the size only to 64 KiB. Names and types are encrypted as well; the
  server only sees the file id, the size in chunks and the owner.
* The **signature** covers a commitment to the file key, so it binds the
  file and its key to the owner: the server cannot substitute a file it
  encrypted itself or seal a key of its own to a recipient (it has no key
  the owner signed). The commitment also makes the ciphertext decrypt
  under only one key.
* Every upload, including a new version of a file, gets a fresh file id
  and nonce prefix.

## Sharing

* **Access** = a copy of the file key **sealed to the recipient's X25519
  public key**: ephemeral X25519 key pair, HKDF over the shared secret and
  both public keys, XChaCha20-Poly1305 with the file id and the
  recipient's name as associated data. The owner is a recipient like any
  other.
* **Sharing** a file uploads one more sealed key. The server cannot read
  it and cannot add recipients itself (it has no file key).
* **Public keys** come from the server, signed by the user's Ed25519 key.
  The apps **pin** the keys of each contact on first use and warn loudly
  when they change. A **safety number** (SHA-256 over both users' Ed25519
  public keys, shown as 12 groups of 5 digits and as a QR code) lets two
  users confirm in person that no key was substituted.
* **Revoking** removes the recipient's sealed key. A recipient who already
  downloaded the file key could still decrypt the old ciphertext, so the
  apps offer "revoke and re-encrypt": a new file key, the file is
  re-uploaded and re-sealed for the remaining recipients.
* **Groups** (e.g. a family) are a list of recipients; there is no group
  key the server could hand out.

## Web client integrity

The browser version comes from the server, so it can only be as
trustworthy as the server. Four checks make manipulation visible:

1. **Signed manifest**: the web build (`apps/beatlumen/web/finish.sh`)
   writes `manifest.json` with the SHA-256 of every file and, in official
   builds, `manifest.sig`: the Ed25519 signature of the manifest with the
   project's release key. The private key is a CI secret
   (`BEATLUMEN_RELEASE_SIGNING_KEY`); the public key
   (`BEATLUMEN_RELEASE_KEY`, a repository variable) is compiled into the
   apps and the server.
2. **Server self-check**: on start the server checks the web client it
   serves (folder or embedded) against the manifest, verifies the
   signature when it was built with the release key, checks a file again
   whenever it changes on disk, and refuses to serve modified, added or
   unsigned files (503). `/api/info` reports the result (`webIntegrity`).
   This catches tampering with the files, not a malicious admin (who can
   replace the server too).
3. **In the browser**: `index.html` pins its scripts and style with
   Subresource Integrity hashes, and `start.js` checks the SHA-256 of the
   WebAssembly module before compiling it, so nothing between the page
   and its files (a cache, a proxy, a changed file) can swap them.
4. **Check from outside**: the native apps fetch the manifest and every
   file from the server, verify the signature with their built-in release
   key and compare the hashes (Settings → Server → Check). This also
   catches a server whose admin changed the web client.

The web version shows a short note that encrypted files in the browser
are only as safe as the web app the server delivers, with the advice to
use an app for the best protection.

Setting up the release key (once, by the maintainer):

```sh
beatlumen-server release-keygen
# BEATLUMEN_RELEASE_SIGNING_KEY=…   → GitHub: Settings → Secrets → Actions
# BEATLUMEN_RELEASE_KEY=…           → GitHub: Settings → Variables → Actions
```

## Implementation steps

1. `bl-vault` (done): the cryptography above, with test vectors, pinned
   derivation values (checked against an independent implementation) and
   tests for tampering (changed chunks, swapped chunks, truncation, wrong
   keys, substituted public keys).
2. Server (done, `apps/beatlumen-server/src/vault.rs`, API in the server
   README): user accounts with invitations, the login and recovery flows,
   storage of key bundles, encrypted blobs (with byte ranges), sealed keys
   and signatures. The server never handles plaintext.
3. Apps (done for Linux, Windows, Android and the web: `bl-core/src/vault.rs`,
   `VaultPage.qml`): account setup with recovery code, unlocking, encrypted
   upload, streaming playback that decrypts chunk by chunk (downloaded
   first in the browser), on-device analysis, sharing with safety numbers
   and key pinning, password change and recovery.
4. Web client integrity (done): manifest and signing in CI, server
   self-check, Subresource Integrity and the WebAssembly hash check,
   verification in the apps.
5. Passkeys (done): WebAuthn with PRF on the server, the sign-in page,
   the hand-over to the apps, Android's Credential Manager
   (`bl-core/src/vault/native.rs`, `Credentials.java`), the password
   switch.
