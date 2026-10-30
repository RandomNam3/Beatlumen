//! SRP-6a (RFC 5054 / RFC 2945) as AirPlay 2 pair-setup uses it: the
//! 3072-bit group with SHA-512, the user "Pair-Setup" and the device PIN as
//! password. Parameterised over group and hash so it can be checked
//! against the RFC 5054 test vectors (1024-bit group, SHA-1).
//!
//! As in the reference implementations, `s`, `A`, `B` and `S` enter the
//! proofs without leading zero bytes, `k` and `u` hash padded values.

use super::bigint::{self, strip};

/// A group: prime modulus and generator (big-endian).
pub struct Group {
    pub n: &'static [u8],
    pub g: &'static [u8],
}

/// Hash function used by the protocol.
pub type HashFn = fn(&[&[u8]]) -> Vec<u8>;

/// The 3072-bit group of RFC 5054 Appendix A (generator 5).
pub const GROUP_3072: Group = Group { n: &N3072, g: &[5] };

const N3072: [u8; 384] = {
    const HEX: &[u8] = b"\
        FFFFFFFFFFFFFFFFC90FDAA22168C234C4C6628B80DC1CD129024E088A67CC74020BBEA63B\
        139B22514A08798E3404DDEF9519B3CD3A431B302B0A6DF25F14374FE1356D6D51C245E485\
        B576625E7EC6F44C42E9A637ED6B0BFF5CB6F406B7EDEE386BFB5A899FA5AE9F24117C4B1F\
        E649286651ECE45B3DC2007CB8A163BF0598DA48361C55D39A69163FA8FD24CF5F83655D23\
        DCA3AD961C62F356208552BB9ED529077096966D670C354E4ABC9804F1746C08CA18217C32\
        905E462E36CE3BE39E772C180E86039B2783A2EC07A28FB5C55DF06F4C52C9DE2BCBF69558\
        17183995497CEA956AE515D2261898FA051015728E5A8AAAC42DAD33170D04507A33A85521\
        ABDF1CBA64ECFB850458DBEF0A8AEA71575D060C7DB3970F85A6E1E4C7ABF5AE8CDB0933D7\
        1E8C94E04A25619DCEE3D2261AD2EE6BF12FFA06D98A0864D87602733EC86A64521F2B1817\
        7B200CBBE117577A615D6C770988C0BAD946E208E24FA074E5AB3143DB5BFCE0FD108E4B82\
        D120A93AD2CAFFFFFFFFFFFFFFFF";
    const fn nibble(c: u8) -> u8 {
        match c {
            b'0'..=b'9' => c - b'0',
            b'A'..=b'F' => c - b'A' + 10,
            _ => panic!("bad hex"),
        }
    }
    let mut out = [0u8; 384];
    let mut i = 0;
    while i < 384 {
        out[i] = nibble(HEX[2 * i]) << 4 | nibble(HEX[2 * i + 1]);
        i += 1;
    }
    out
};

/// SHA-512 as a [`HashFn`].
pub fn sha512(parts: &[&[u8]]) -> Vec<u8> {
    super::sha512::sha512(parts).to_vec()
}

/// Left-pads `x` with zeros to the length of the modulus.
fn pad(x: &[u8], len: usize) -> Vec<u8> {
    let x = strip(x);
    let mut out = vec![0u8; len.saturating_sub(x.len())];
    out.extend_from_slice(x);
    out
}

/// `x = H(s | H(I ":" P))`.
fn private_x(hash: HashFn, salt: &[u8], user: &str, pass: &[u8]) -> Vec<u8> {
    let inner = hash(&[user.as_bytes(), b":", pass]);
    hash(&[strip(salt), &inner])
}

/// `k = H(N | PAD(g))`.
fn multiplier(group: &Group, hash: HashFn) -> Vec<u8> {
    hash(&[group.n, &pad(group.g, group.n.len())])
}

/// `u = H(PAD(A) | PAD(B))`.
fn scrambler(group: &Group, hash: HashFn, a: &[u8], b: &[u8]) -> Vec<u8> {
    let len = group.n.len();
    hash(&[&pad(a, len), &pad(b, len)])
}

/// The verifier `v = g^x` of a password.
pub fn verifier(group: &Group, hash: HashFn, salt: &[u8], user: &str, pass: &[u8]) -> Vec<u8> {
    let x = private_x(hash, salt, user, pass);
    bigint::mod_pow(group.g, &x, group.n)
}

/// The server side of one authentication.
pub struct Server {
    group: &'static Group,
    hash: HashFn,
    user: String,
    salt: Vec<u8>,
    v: Vec<u8>,
    b: Vec<u8>,
    big_b: Vec<u8>,
}

/// Result of a successful authentication.
pub struct Session {
    /// Shared session key `K = H(S)`.
    pub key: Vec<u8>,
    /// Server proof `H(A | M | K)` for the client.
    pub proof: Vec<u8>,
}

impl Server {
    /// Starts an authentication with password `pass`; `salt` should not
    /// start with a zero byte, `b` is the secret ephemeral value.
    pub fn new(
        group: &'static Group,
        hash: HashFn,
        user: &str,
        pass: &[u8],
        salt: &[u8],
        b: &[u8],
    ) -> Server {
        let v = verifier(group, hash, salt, user, pass);
        let k = multiplier(group, hash);
        let kv = bigint::mod_mul(&k, &v, group.n);
        let gb = bigint::mod_pow(group.g, b, group.n);
        let big_b = bigint::mod_add(&kv, &gb, group.n);
        Server {
            group,
            hash,
            user: user.to_string(),
            salt: salt.to_vec(),
            v,
            b: b.to_vec(),
            big_b,
        }
    }

    pub fn salt(&self) -> &[u8] {
        &self.salt
    }

    /// The public value `B` (without leading zeros).
    pub fn public_key(&self) -> &[u8] {
        strip(&self.big_b)
    }

    /// The premaster secret `S = (A * v^u)^b`; `None` when `A` is invalid
    /// (zero modulo N or not reduced).
    pub fn premaster(&self, a: &[u8]) -> Option<Vec<u8>> {
        let n = self.group.n;
        if strip(a).is_empty() || !bigint::less_than(a, n) {
            return None;
        }
        let u = scrambler(self.group, self.hash, a, &self.big_b);
        let vu = bigint::mod_pow(&self.v, &u, n);
        let avu = bigint::mod_mul(a, &vu, n);
        Some(bigint::mod_pow(&avu, &self.b, n))
    }

    /// Checks the client's proof `M`; on success the session key and the
    /// server's proof.
    pub fn verify(&self, a: &[u8], client_proof: &[u8]) -> Option<Session> {
        let s = self.premaster(a)?;
        let key = (self.hash)(&[strip(&s)]);
        let m = proof(
            self.group,
            self.hash,
            &self.user,
            &self.salt,
            a,
            &self.big_b,
            &key,
        );
        let equal = m.len() == client_proof.len()
            && m.iter()
                .zip(client_proof)
                .fold(0u8, |acc, (x, y)| acc | (x ^ y))
                == 0;
        if !equal {
            return None;
        }
        let server_proof = (self.hash)(&[strip(a), &m, &key]);
        Some(Session {
            key,
            proof: server_proof,
        })
    }
}

/// The client proof `M = H(H(N) xor H(g) | H(I) | s | A | B | K)`.
fn proof(
    group: &Group,
    hash: HashFn,
    user: &str,
    salt: &[u8],
    a: &[u8],
    b: &[u8],
    key: &[u8],
) -> Vec<u8> {
    let hn = hash(&[group.n]);
    let hg = hash(&[strip(group.g)]);
    let x: Vec<u8> = hn.iter().zip(&hg).map(|(p, q)| p ^ q).collect();
    let hi = hash(&[user.as_bytes()]);
    hash(&[&x, &hi, strip(salt), strip(a), strip(b), key])
}

/// The client side (used by the tests' sender).
#[cfg(test)]
pub struct Client {
    pub a_pub: Vec<u8>,
    pub key: Vec<u8>,
    pub proof: Vec<u8>,
    pub server_proof: Vec<u8>,
}

#[cfg(test)]
impl Client {
    /// Computes `A`, the session key and the proofs from the server's salt
    /// and `B`; `None` when `B` is invalid.
    pub fn new(
        group: &Group,
        hash: HashFn,
        user: &str,
        pass: &[u8],
        salt: &[u8],
        big_b: &[u8],
        a: &[u8],
    ) -> Option<Client> {
        let n = group.n;
        if strip(big_b).is_empty() || !bigint::less_than(big_b, n) {
            return None;
        }
        let a_pub = bigint::mod_pow(group.g, a, n);
        let s = client_premaster(group, hash, user, pass, salt, big_b, a, &a_pub);
        let key = hash(&[strip(&s)]);
        let m = proof(group, hash, user, salt, &a_pub, big_b, &key);
        let server_proof = hash(&[strip(&a_pub), &m, &key]);
        Some(Client {
            a_pub: strip(&a_pub).to_vec(),
            key,
            proof: m,
            server_proof,
        })
    }
}

/// `S = (B - k * g^x)^(a + u * x)`.
#[cfg(test)]
#[allow(clippy::too_many_arguments)]
fn client_premaster(
    group: &Group,
    hash: HashFn,
    user: &str,
    pass: &[u8],
    salt: &[u8],
    big_b: &[u8],
    a: &[u8],
    a_pub: &[u8],
) -> Vec<u8> {
    let n = group.n;
    let x = private_x(hash, salt, user, pass);
    let u = scrambler(group, hash, a_pub, big_b);
    let k = multiplier(group, hash);
    let gx = bigint::mod_pow(group.g, &x, n);
    let kgx = bigint::mod_mul(&k, &gx, n);
    let base = bigint::mod_sub(big_b, &kgx, n);
    let exp = bigint::add(a, &bigint::mul(&u, &x));
    bigint::mod_pow(&base, &exp, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::hash::hex;
    use crate::crypto::test_util::unhex;

    fn sha1(parts: &[&[u8]]) -> Vec<u8> {
        crate::crypto::hash::sha1(&parts.concat()).to_vec()
    }

    fn n1024() -> &'static [u8] {
        static N: std::sync::OnceLock<Vec<u8>> = std::sync::OnceLock::new();
        N.get_or_init(|| {
            unhex(
                "EEAF0AB9ADB38DD69C33F80AFA8FC5E86072618775FF3C0B9EA2314C\
                 9C256576D674DF7496EA81D3383B4813D692C6E0E0D5D8E250B98BE4\
                 8E495C1D6089DAD15DC7D7B46154D6B6CE8EF4AD69B15D4982559B29\
                 7BCF1885C529F566660E57EC68EDBC3C05726CC02FD4CBF4976EAA9A\
                 FD5138FE8376435B9FC61D2FC0EB06E3",
            )
        })
    }

    fn short(h: &str) -> String {
        h.trim_start_matches('0').to_lowercase()
    }

    #[test]
    fn rfc5054_vectors() {
        let group: &'static Group = Box::leak(Box::new(Group {
            n: n1024(),
            g: &[2],
        }));
        let salt = unhex("BEB25379D1A8581EB5A727673A2441EE");
        let (user, pass) = ("alice", b"password123");
        assert_eq!(
            hex(&multiplier(group, sha1)),
            "7556aa045aef2cdd07abaf0f665c3e818913186f"
        );
        assert_eq!(
            hex(&private_x(sha1, &salt, user, pass)),
            "94b7555aabe9127cc58ccf4993db6cf84d16c124"
        );
        assert_eq!(
            short(&hex(&verifier(group, sha1, &salt, user, pass))),
            "7e273de8696ffc4f4e337d05b4b375beb0dde1569e8fa00a9886d812\
             9bada1f1822223ca1a605b530e379ba4729fdc59f105b4787e5186f5\
             c671085a1447b52a48cf1970b4fb6f8400bbf4cebfbb168152e08ab5\
             ea53d15c1aff87b2b9da6e04e058ad51cc72bfc9033b564e26480d78\
             e955a5e29e7ab245db2be315e2099afb"
        );
        let a = unhex("60975527035CF2AD1989806F0407210BC81EDC04E2762A56AFD529DDDA2D4393");
        let b = unhex("E487CB59D31AC550471E81F00F6928E01DDA08E974A004F49E61F5D105284D20");
        let server = Server::new(group, sha1, user, pass, &salt, &b);
        assert_eq!(
            hex(server.public_key()),
            "bd0c61512c692c0cb6d041fa01bb152d4916a1e77af46ae105393011\
             baf38964dc46a0670dd125b95a981652236f99d9b681cbf87837ec99\
             6c6da04453728610d0c6ddb58b318885d7d82c7f8deb75ce7bd4fbaa\
             37089e6f9c6059f388838e7a00030b331eb76840910440b1b27aaeae\
             eb4012b7d7665238a8e3fb004b117b58"
        );
        let a_pub = bigint::mod_pow(group.g, &a, group.n);
        assert_eq!(
            short(&hex(&a_pub)),
            "61d5e490f6f1b79547b0704c436f523dd0e560f0c64115bb72557ec4\
             4352e8903211c04692272d8b2d1a5358a2cf1b6e0bfcf99f921530ec\
             8e39356179eae45e42ba92aeaced825171e1e8b9af6d9c03e1327f44\
             be087ef06530e69f66615261eef54073ca11cf5858f0edfdfe15efea\
             b349ef5d76988a3672fac47b0769447b"
        );
        assert_eq!(
            hex(&scrambler(group, sha1, &a_pub, server.public_key())),
            "ce38b9593487da98554ed47d70a7ae5f462ef019"
        );
        let premaster = "b0dc82babcf30674ae450c0287745e7990a3381f63b387aaf271a10d\
                         233861e359b48220f7c4693c9ae12b0a6f67809f0876e2d013800d6c\
                         41bb59b6d5979b5c00a172b4a2a5903a0bdcaf8a709585eb2afafa8f\
                         3499b200210dcc1f10eb33943cd67fc88a2f39a4be5bec4ec0a3212d\
                         c346d7e474b29ede8a469ffeca686e5a";
        assert_eq!(short(&hex(&server.premaster(&a_pub).unwrap())), premaster);
        let c = Client::new(group, sha1, user, pass, &salt, server.public_key(), &a).unwrap();
        assert_eq!(
            short(&hex(&client_premaster(
                group,
                sha1,
                user,
                pass,
                &salt,
                server.public_key(),
                &a,
                &a_pub
            ))),
            premaster
        );
        let s = server.verify(&c.a_pub, &c.proof).unwrap();
        assert_eq!(s.key, c.key);
        assert_eq!(s.proof, c.server_proof);
    }

    #[test]
    fn airplay_group_round_trip() {
        let salt = unhex("BEB25379D1A8581EB5A727673A2441EE");
        let b = [0x42u8; 32];
        let server = Server::new(&GROUP_3072, sha512, "Pair-Setup", b"3939", &salt, &b);
        let c = Client::new(
            &GROUP_3072,
            sha512,
            "Pair-Setup",
            b"3939",
            &salt,
            server.public_key(),
            &[0x17; 32],
        )
        .unwrap();
        let s = server.verify(&c.a_pub, &c.proof).unwrap();
        assert_eq!(s.key.len(), 64);
        assert_eq!(s.key, c.key);
        assert_eq!(s.proof, c.server_proof);
        // A wrong PIN fails, as do A = 0 and A = N.
        let wrong = Client::new(
            &GROUP_3072,
            sha512,
            "Pair-Setup",
            b"1234",
            &salt,
            server.public_key(),
            &[0x17; 32],
        )
        .unwrap();
        assert!(server.verify(&wrong.a_pub, &wrong.proof).is_none());
        assert!(server.verify(&[0], &c.proof).is_none());
        assert!(server.verify(GROUP_3072.n, &c.proof).is_none());
    }
}
