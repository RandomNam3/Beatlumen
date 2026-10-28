// Web Push message decryption (RFC 8291, "aes128gcm" of RFC 8188) with the
// platform's own cryptography (no libraries). Used for UnifiedPush
// messages, which are decrypted here so that notifications also work while
// the app is closed.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import java.math.BigInteger;
import java.nio.charset.StandardCharsets;
import java.security.KeyFactory;
import java.security.KeyPair;
import java.security.KeyPairGenerator;
import java.security.PrivateKey;
import java.security.PublicKey;
import java.security.SecureRandom;
import java.security.interfaces.ECPublicKey;
import java.security.spec.ECGenParameterSpec;
import java.security.spec.ECParameterSpec;
import java.security.spec.ECPoint;
import java.security.spec.ECPublicKeySpec;
import java.security.spec.PKCS8EncodedKeySpec;
import java.security.spec.X509EncodedKeySpec;
import java.util.Arrays;

import javax.crypto.Cipher;
import javax.crypto.KeyAgreement;
import javax.crypto.Mac;
import javax.crypto.spec.GCMParameterSpec;
import javax.crypto.spec.SecretKeySpec;

public final class WebPushCrypto {
    /** The subscription's keys: P-256 key pair and auth secret. */
    public final PrivateKey privateKey;
    public final ECPublicKey publicKey;
    public final byte[] auth;

    public WebPushCrypto(PrivateKey privateKey, ECPublicKey publicKey, byte[] auth) {
        this.privateKey = privateKey;
        this.publicKey = publicKey;
        this.auth = auth;
    }

    public static WebPushCrypto generate() throws Exception {
        KeyPairGenerator g = KeyPairGenerator.getInstance("EC");
        g.initialize(new ECGenParameterSpec("secp256r1"));
        KeyPair kp = g.generateKeyPair();
        byte[] auth = new byte[16];
        new SecureRandom().nextBytes(auth);
        return new WebPushCrypto(kp.getPrivate(), (ECPublicKey) kp.getPublic(), auth);
    }

    /** From stored encodings (PKCS#8 private key, X.509 public key). */
    public static WebPushCrypto load(byte[] pkcs8, byte[] x509, byte[] auth) throws Exception {
        KeyFactory f = KeyFactory.getInstance("EC");
        return new WebPushCrypto(f.generatePrivate(new PKCS8EncodedKeySpec(pkcs8)),
                (ECPublicKey) f.generatePublic(new X509EncodedKeySpec(x509)), auth);
    }

    private static byte[] fixed32(BigInteger v) {
        byte[] b = v.toByteArray();
        byte[] out = new byte[32];
        int n = Math.min(b.length, 32);
        System.arraycopy(b, b.length - n, out, 32 - n, n);
        return out;
    }

    /** Uncompressed point (0x04 || x || y), the "p256dh" key. */
    public static byte[] uncompressed(ECPublicKey key) {
        byte[] out = new byte[65];
        out[0] = 4;
        System.arraycopy(fixed32(key.getW().getAffineX()), 0, out, 1, 32);
        System.arraycopy(fixed32(key.getW().getAffineY()), 0, out, 33, 32);
        return out;
    }

    private static PublicKey fromUncompressed(byte[] p, ECParameterSpec params) throws Exception {
        if (p.length != 65 || p[0] != 4)
            throw new IllegalArgumentException("bad public key");
        ECPoint w = new ECPoint(new BigInteger(1, Arrays.copyOfRange(p, 1, 33)),
                new BigInteger(1, Arrays.copyOfRange(p, 33, 65)));
        return KeyFactory.getInstance("EC").generatePublic(new ECPublicKeySpec(w, params));
    }

    private static byte[] hmac(byte[] key, byte[]... parts) throws Exception {
        Mac m = Mac.getInstance("HmacSHA256");
        m.init(new SecretKeySpec(key, "HmacSHA256"));
        for (byte[] p : parts)
            m.update(p);
        return m.doFinal();
    }

    /** HKDF-SHA-256 with output of at most 32 bytes. */
    private static byte[] hkdf(byte[] salt, byte[] ikm, byte[] info, int len) throws Exception {
        byte[] prk = hmac(salt, ikm);
        return Arrays.copyOf(hmac(prk, info, new byte[] { 1 }), len);
    }

    private static byte[] concat(byte[]... parts) {
        int n = 0;
        for (byte[] p : parts)
            n += p.length;
        byte[] out = new byte[n];
        int at = 0;
        for (byte[] p : parts) {
            System.arraycopy(p, 0, out, at, p.length);
            at += p.length;
        }
        return out;
    }

    /** Decrypts a message body; throws on any manipulation. */
    public byte[] decrypt(byte[] body) throws Exception {
        if (body.length < 21 + 65 + 17 || (body[20] & 0xff) != 65)
            throw new IllegalArgumentException("bad header");
        byte[] salt = Arrays.copyOfRange(body, 0, 16);
        byte[] asPublic = Arrays.copyOfRange(body, 21, 86);
        byte[] ct = Arrays.copyOfRange(body, 86, body.length);
        KeyAgreement ka = KeyAgreement.getInstance("ECDH");
        ka.init(privateKey);
        ka.doPhase(fromUncompressed(asPublic, publicKey.getParams()), true);
        byte[] shared = ka.generateSecret();
        byte[] info = concat("WebPush: info\0".getBytes(StandardCharsets.US_ASCII), uncompressed(publicKey), asPublic);
        byte[] ikm = hkdf(auth, shared, info, 32);
        byte[] cek = hkdf(salt, ikm, "Content-Encoding: aes128gcm\0".getBytes(StandardCharsets.US_ASCII), 16);
        byte[] nonce = hkdf(salt, ikm, "Content-Encoding: nonce\0".getBytes(StandardCharsets.US_ASCII), 12);
        Cipher c = Cipher.getInstance("AES/GCM/NoPadding");
        c.init(Cipher.DECRYPT_MODE, new SecretKeySpec(cek, "AES"), new GCMParameterSpec(128, nonce));
        byte[] plain = c.doFinal(ct);
        int end = plain.length - 1;
        while (end >= 0 && plain[end] == 0)
            end--;
        if (end < 0 || plain[end] != 2)
            throw new IllegalArgumentException("bad padding");
        return Arrays.copyOf(plain, end);
    }
}
