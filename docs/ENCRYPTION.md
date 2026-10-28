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
trustworthy as the server. Three checks make manipulation visible:

1. **Signed manifest**: the official build writes the SHA-256 of every web
   client file into `manifest.json`, signed with the project's release
   key (Ed25519; the private key is a CI secret, the public key is part of
   the apps and the server).
2. **Server self-check**: on start the server checks the web client it
   would serve against the manifest and refuses to serve changed files.
   This catches tampering with the files on disk, not a malicious admin
   (who can replace the server too).
3. **Check from outside**: the native apps fetch the web client from the
   server, verify it against the signed manifest and show the result
   ("web version verified" or a warning). Script and WebAssembly files are
   referenced with Subresource Integrity hashes, so a proxy cannot alter
   them either.

The web version shows a short note that encrypted files are only as safe
as the server's admin when opened in the browser, with a link to verify it
with an app.

## Implementation steps

1. `bl-vault` (done): the cryptography above, with test vectors, pinned
   derivation values (checked against an independent implementation) and
   tests for tampering (changed chunks, swapped chunks, truncation, wrong
   keys, substituted public keys).
2. Server (done, `apps/beatlumen-server/src/vault.rs`, API in the server
   README): user accounts with invitations, the login and recovery flows,
   storage of key bundles, encrypted blobs (with byte ranges), sealed keys
   and signatures. The server never handles plaintext.
3. Apps: account setup with recovery code, unlocking, encrypted upload and
   download, sharing with safety numbers and key pinning, password change
   and recovery.
4. Web client integrity: manifest and signing in CI, server self-check,
   Subresource Integrity, verification in the apps.
