//! The encrypted file format.
//!
//! ```text
//! header (49): "BLV1" ‖ version (1) ‖ file id (16) ‖ nonce prefix (16)
//!              ‖ chunk size (u32 BE) ‖ chunk count (u64 BE)
//! chunk i:     XChaCha20-Poly1305(file key,
//!                  nonce = prefix ‖ i (7 bytes BE) ‖ last flag (1),
//!                  aad   = header) – chunk size bytes + 16 byte tag
//! ```
//!
//! Every chunk holds exactly `chunk size` bytes of plaintext; the last one
//! is padded (`0x80` then zeros), so the server learns the size only in
//! whole chunks. Chunk *i* sits at a fixed offset, so byte ranges map to
//! chunks for streaming and seeking. The index, the last flag and the whole
//! header are authenticated with every chunk: changed, swapped, missing or
//! appended chunks fail to decrypt.
//!
//! The metadata (name, type, size, …; the format is up to the app) is
//! encrypted with the same key and padded to 256 bytes. The owner signs the
//! header, the encrypted metadata and a commitment to the file key, so a
//! recipient knows the file is the owner's and was not made up by the
//! server (which could seal a key of its own to anyone).

use crate::identity::{Identity, PublicIdentity};
use crate::{aead, random, sha256, Error, Secret};

pub const MAGIC: &[u8; 4] = b"BLV1";
pub const VERSION: u8 = 1;
pub const CHUNK_SIZE: u32 = 64 * 1024;
pub const HEADER_LEN: usize = 4 + 1 + 16 + 16 + 4 + 8;
const MIN_CHUNK: u32 = 4 * 1024;
const MAX_CHUNK: u32 = 4 * 1024 * 1024;
const META_BLOCK: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Header {
    pub file_id: [u8; 16],
    nonce_prefix: [u8; 16],
    pub chunk_size: u32,
    pub chunks: u64,
}

impl Header {
    /// A header for a new file (or a new version of one: a fresh nonce
    /// prefix and file id every time).
    pub fn new(plain_len: u64) -> Result<Header, Error> {
        let mut file_id = [0u8; 16];
        let mut nonce_prefix = [0u8; 16];
        random(&mut file_id)?;
        random(&mut nonce_prefix)?;
        let h = Header {
            file_id,
            nonce_prefix,
            chunk_size: CHUNK_SIZE,
            // Always room for the padding byte.
            chunks: plain_len / CHUNK_SIZE as u64 + 1,
        };
        h.check()?;
        Ok(h)
    }

    fn check(&self) -> Result<(), Error> {
        if !(MIN_CHUNK..=MAX_CHUNK).contains(&self.chunk_size) {
            return Err(Error::Format("chunk size"));
        }
        if self.chunks == 0 || self.chunks >= 1 << 56 {
            return Err(Error::Format("chunk count"));
        }
        self.chunks
            .checked_mul(self.chunk_size as u64 + aead::TAG_LEN as u64)
            .and_then(|n| n.checked_add(HEADER_LEN as u64))
            .filter(|&n| n < 1 << 62)
            .ok_or(Error::Format("file too large"))?;
        Ok(())
    }

    pub fn to_bytes(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[..4].copy_from_slice(MAGIC);
        b[4] = VERSION;
        b[5..21].copy_from_slice(&self.file_id);
        b[21..37].copy_from_slice(&self.nonce_prefix);
        b[37..41].copy_from_slice(&self.chunk_size.to_be_bytes());
        b[41..49].copy_from_slice(&self.chunks.to_be_bytes());
        b
    }

    /// Parses the first [`HEADER_LEN`] bytes of a file.
    pub fn parse(b: &[u8]) -> Result<Header, Error> {
        if b.len() < HEADER_LEN || &b[..4] != MAGIC {
            return Err(Error::Format("not an encrypted file"));
        }
        if b[4] != VERSION {
            return Err(Error::Format("unsupported version"));
        }
        let h = Header {
            file_id: b[5..21].try_into().expect("16"),
            nonce_prefix: b[21..37].try_into().expect("16"),
            chunk_size: u32::from_be_bytes(b[37..41].try_into().expect("4")),
            chunks: u64::from_be_bytes(b[41..49].try_into().expect("8")),
        };
        h.check()?;
        Ok(h)
    }

    /// Bytes of one encrypted chunk.
    pub fn chunk_len(&self) -> u64 {
        self.chunk_size as u64 + aead::TAG_LEN as u64
    }

    /// Where chunk `index` starts in the encrypted file.
    pub fn chunk_offset(&self, index: u64) -> u64 {
        HEADER_LEN as u64 + index * self.chunk_len()
    }

    /// Size of the whole encrypted file.
    pub fn encrypted_len(&self) -> u64 {
        self.chunk_offset(self.chunks)
    }

    /// The chunk holding plaintext byte `offset`.
    pub fn chunk_for(&self, offset: u64) -> u64 {
        (offset / self.chunk_size as u64).min(self.chunks - 1)
    }

    fn nonce(&self, index: u64) -> [u8; aead::NONCE_LEN] {
        let mut n = [0u8; aead::NONCE_LEN];
        n[..16].copy_from_slice(&self.nonce_prefix);
        n[16..23].copy_from_slice(&index.to_be_bytes()[1..]);
        n[23] = (index + 1 == self.chunks) as u8;
        n
    }
}

fn pad(data: &[u8], to: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(to);
    v.extend_from_slice(data);
    v.push(0x80);
    v.resize(to, 0);
    v
}

fn unpad(mut v: Vec<u8>) -> Result<Vec<u8>, Error> {
    let end = v
        .iter()
        .rposition(|&b| b != 0)
        .ok_or(Error::Format("padding"))?;
    if v[end] != 0x80 {
        return Err(Error::Format("padding"));
    }
    v.truncate(end);
    Ok(v)
}

/// Encrypts chunk `index`: exactly `chunk_size` bytes, the last chunk less.
pub fn encrypt_chunk(key: &Secret, h: &Header, index: u64, plain: &[u8]) -> Result<Vec<u8>, Error> {
    let cs = h.chunk_size as usize;
    let last = index + 1 == h.chunks;
    if index >= h.chunks || (last && plain.len() >= cs) || (!last && plain.len() != cs) {
        return Err(Error::Format("chunk length"));
    }
    let buf = if last { pad(plain, cs) } else { plain.to_vec() };
    Ok(aead::seal_with_nonce(
        key,
        &h.nonce(index),
        &h.to_bytes(),
        &buf,
    ))
}

/// Decrypts chunk `index` (the padding of the last chunk is removed).
pub fn decrypt_chunk(key: &Secret, h: &Header, index: u64, data: &[u8]) -> Result<Vec<u8>, Error> {
    if index >= h.chunks || data.len() as u64 != h.chunk_len() {
        return Err(Error::Decrypt);
    }
    let plain = aead::open_with_nonce(key, &h.nonce(index), &h.to_bytes(), data)?;
    if index + 1 == h.chunks {
        unpad(plain)
    } else {
        Ok(plain)
    }
}

/// Encrypts a whole file held in memory.
pub fn encrypt(key: &Secret, plain: &[u8]) -> Result<(Header, Vec<u8>), Error> {
    let h = Header::new(plain.len() as u64)?;
    let mut out = Vec::with_capacity(h.encrypted_len() as usize);
    out.extend_from_slice(&h.to_bytes());
    let cs = h.chunk_size as usize;
    for i in 0..h.chunks {
        let start = (i as usize * cs).min(plain.len());
        let end = (start + cs).min(plain.len());
        out.extend(encrypt_chunk(key, &h, i, &plain[start..end])?);
    }
    Ok((h, out))
}

/// Decrypts a whole file held in memory.
pub fn decrypt(key: &Secret, data: &[u8]) -> Result<Vec<u8>, Error> {
    let h = Header::parse(data)?;
    if data.len() as u64 != h.encrypted_len() {
        return Err(Error::Decrypt);
    }
    let mut out = Vec::with_capacity(data.len());
    for i in 0..h.chunks {
        let at = h.chunk_offset(i) as usize;
        out.extend(decrypt_chunk(
            key,
            &h,
            i,
            &data[at..at + h.chunk_len() as usize],
        )?);
    }
    Ok(out)
}

fn meta_aad(h: &Header) -> Vec<u8> {
    let mut a = b"beatlumen meta v1\0".to_vec();
    a.extend_from_slice(&h.to_bytes());
    a
}

/// Encrypts the metadata, padded to a multiple of 256 bytes.
pub fn seal_meta(key: &Secret, h: &Header, meta: &[u8]) -> Result<Vec<u8>, Error> {
    let len = (meta.len() / META_BLOCK + 1) * META_BLOCK;
    aead::seal(key, &meta_aad(h), &pad(meta, len))
}

pub fn open_meta(key: &Secret, h: &Header, data: &[u8]) -> Result<Vec<u8>, Error> {
    unpad(aead::open(key, &meta_aad(h), data)?)
}

/// A hash of the file key; signed by the owner, so a recipient can tell
/// that the key it unsealed is the owner's (and AEAD ciphertexts cannot be
/// made to decrypt under two keys).
pub fn key_commitment(key: &Secret) -> [u8; 32] {
    sha256(&[b"beatlumen key commitment v1\0", key.as_bytes()])
}

fn signed_digest(h: &Header, meta: &[u8], key: &Secret) -> [u8; 32] {
    sha256(&[
        b"beatlumen file signature v1\0",
        &h.to_bytes(),
        &sha256(&[meta]),
        &key_commitment(key),
    ])
}

/// The owner's signature over header, encrypted metadata and file key.
pub fn sign(owner: &Identity, h: &Header, meta: &[u8], key: &Secret) -> [u8; 64] {
    owner.sign(&signed_digest(h, meta, key))
}

pub fn verify(
    owner: &PublicIdentity,
    h: &Header,
    meta: &[u8],
    key: &Secret,
    sig: &[u8; 64],
) -> Result<(), Error> {
    owner.verify_signature(&signed_digest(h, meta, key), sig)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sealed;

    fn data(n: usize) -> Vec<u8> {
        (0..n).map(|i| (i * 31 + i / 7) as u8).collect()
    }

    #[test]
    fn roundtrip_sizes() {
        let key = Secret::random().unwrap();
        let cs = CHUNK_SIZE as usize;
        for n in [0, 1, 100, cs - 1, cs, cs + 1, 3 * cs + 17] {
            let plain = data(n);
            let (h, enc) = encrypt(&key, &plain).unwrap();
            assert_eq!(enc.len() as u64, h.encrypted_len());
            assert_eq!(h.chunks, n as u64 / cs as u64 + 1);
            assert_eq!(decrypt(&key, &enc).unwrap(), plain, "size {n}");
            assert_eq!(Header::parse(&enc).unwrap(), h);
        }
    }

    #[test]
    fn random_access() {
        let key = Secret::random().unwrap();
        let cs = CHUNK_SIZE as u64;
        let plain = data(5 * cs as usize + 1000);
        let (h, enc) = encrypt(&key, &plain).unwrap();
        let off = 3 * cs + 12345;
        let i = h.chunk_for(off);
        assert_eq!(i, 3);
        let at = h.chunk_offset(i) as usize;
        let chunk = decrypt_chunk(&key, &h, i, &enc[at..at + h.chunk_len() as usize]).unwrap();
        assert_eq!(chunk[12345], plain[off as usize]);
        assert_eq!(h.chunk_for(u64::MAX), h.chunks - 1);
    }

    #[test]
    fn tampering_is_detected() {
        let key = Secret::random().unwrap();
        let cs = CHUNK_SIZE as usize;
        let plain = data(3 * cs + 5);
        let (h, enc) = encrypt(&key, &plain).unwrap();
        let cl = h.chunk_len() as usize;

        // A flipped bit in a chunk or in the header.
        for pos in [HEADER_LEN + 10, HEADER_LEN + cl + 7, enc.len() - 1, 30, 45] {
            let mut t = enc.clone();
            t[pos] ^= 1;
            assert!(decrypt(&key, &t).is_err(), "byte {pos}");
        }
        // Swapped chunks.
        let mut t = enc.clone();
        let (a, b) = (h.chunk_offset(0) as usize, h.chunk_offset(1) as usize);
        let c0 = t[a..a + cl].to_vec();
        let c1 = t[b..b + cl].to_vec();
        t[a..a + cl].copy_from_slice(&c1);
        t[b..b + cl].copy_from_slice(&c0);
        assert!(decrypt(&key, &t).is_err());
        // Truncated: cut off the last chunk and fix up the count.
        let mut h2 = h.clone();
        h2.chunks -= 1;
        let mut t = h2.to_bytes().to_vec();
        t.extend_from_slice(&enc[HEADER_LEN..h.chunk_offset(3) as usize]);
        assert!(decrypt(&key, &t).is_err());
        assert!(decrypt(&key, &enc[..enc.len() - cl]).is_err());
        // A chunk from the same position of another file with the same key.
        let (_, other) = encrypt(&key, &plain).unwrap();
        let mut t = enc.clone();
        t[a..a + cl].copy_from_slice(&other[a..a + cl]);
        assert!(decrypt(&key, &t).is_err());
        // A wrong key.
        assert_eq!(
            decrypt(&Secret::random().unwrap(), &enc),
            Err(Error::Decrypt)
        );
        // Garbage and bad headers.
        assert!(decrypt(&key, b"BLV1").is_err());
        let mut t = enc.clone();
        t[4] = 2;
        assert_eq!(decrypt(&key, &t), Err(Error::Format("unsupported version")));
        assert!(encrypt_chunk(&key, &h, 0, &[0; 10]).is_err());
        assert!(encrypt_chunk(&key, &h, 3, &vec![0; cs]).is_err());
        assert!(encrypt_chunk(&key, &h, 4, &[]).is_err());
    }

    #[test]
    fn meta_signature_and_sharing() {
        let alice = Identity::generate().unwrap();
        let bob = Identity::generate().unwrap();
        let key = Secret::random().unwrap();
        let (h, _) = encrypt(&key, b"song").unwrap();
        let meta = seal_meta(&key, &h, br#"{"name":"song.flac"}"#).unwrap();
        assert_eq!(meta.len(), META_BLOCK + aead::OVERHEAD);
        assert_eq!(
            open_meta(&key, &h, &meta).unwrap(),
            br#"{"name":"song.flac"}"#
        );
        let (h_other, _) = encrypt(&key, b"song").unwrap();
        assert!(open_meta(&key, &h_other, &meta).is_err());

        let sig = sign(&alice, &h, &meta, &key);
        // Bob receives the key sealed to him and checks Alice's signature.
        let aad = sealed::file_key_aad(&h.file_id, "bob");
        let boxed = sealed::seal(&bob.public().dh, &aad, key.as_bytes()).unwrap();
        let got = Secret::from_bytes(
            sealed::open(&bob, &aad, &boxed)
                .unwrap()
                .try_into()
                .unwrap(),
        );
        verify(&alice.public(), &h, &meta, &got, &sig).unwrap();
        // A key the server made up, other metadata, another signer.
        let fake = Secret::random().unwrap();
        assert!(verify(&alice.public(), &h, &meta, &fake, &sig).is_err());
        let meta2 = seal_meta(&key, &h, b"{}").unwrap();
        assert!(verify(&alice.public(), &h, &meta2, &key, &sig).is_err());
        assert!(verify(&bob.public(), &h, &meta, &key, &sig).is_err());
        assert!(verify(&alice.public(), &h_other, &meta, &key, &sig).is_err());
    }
}
