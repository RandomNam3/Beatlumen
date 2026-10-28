// "Stay unlocked on this device" on Android: the vault's account is
// encrypted with an AES key that lives in the Android Keystore (it never
// leaves it, on most devices in secure hardware); only the ciphertext is
// kept in the app's private storage, which is excluded from backups.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.Context;
import android.content.SharedPreferences;
import android.security.keystore.KeyGenParameterSpec;
import android.security.keystore.KeyProperties;
import android.util.Base64;
import android.util.Log;

import java.nio.charset.StandardCharsets;
import java.security.KeyStore;
import java.security.MessageDigest;

import javax.crypto.Cipher;
import javax.crypto.KeyGenerator;
import javax.crypto.SecretKey;
import javax.crypto.spec.GCMParameterSpec;

public final class SecretStore
{
    private SecretStore() {}

    private static final String TAG = "Beatlumen";
    private static final String ALIAS = "beatlumen-vault";
    private static final String PREFS = "secrets";

    private static SecretKey key(boolean create) throws Exception
    {
        KeyStore ks = KeyStore.getInstance("AndroidKeyStore");
        ks.load(null);
        if (ks.containsAlias(ALIAS))
            return ((KeyStore.SecretKeyEntry) ks.getEntry(ALIAS, null)).getSecretKey();
        if (!create)
            return null;
        KeyGenerator g = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore");
        g.init(new KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT | KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build());
        return g.generateKey();
    }

    private static String name(String key) throws Exception
    {
        byte[] d = MessageDigest.getInstance("SHA-256").digest(key.getBytes(StandardCharsets.UTF_8));
        return Base64.encodeToString(d, Base64.NO_WRAP | Base64.URL_SAFE | Base64.NO_PADDING);
    }

    /** Keeps `value` under `key`; an empty value deletes it. */
    public static boolean store(Context c, String key, byte[] value)
    {
        try {
            SharedPreferences p = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
            if (value == null || value.length == 0) {
                p.edit().remove(name(key)).commit();
                return true;
            }
            Cipher ci = Cipher.getInstance("AES/GCM/NoPadding");
            ci.init(Cipher.ENCRYPT_MODE, key(true));
            // The key name binds the ciphertext to its entry.
            ci.updateAAD(key.getBytes(StandardCharsets.UTF_8));
            byte[] iv = ci.getIV();
            byte[] ct = ci.doFinal(value);
            byte[] out = new byte[1 + iv.length + ct.length];
            out[0] = (byte) iv.length;
            System.arraycopy(iv, 0, out, 1, iv.length);
            System.arraycopy(ct, 0, out, 1 + iv.length, ct.length);
            return p.edit().putString(name(key), Base64.encodeToString(out, Base64.NO_WRAP)).commit();
        } catch (Exception e) {
            Log.w(TAG, "secret not stored: " + e.getClass().getSimpleName());
            return false;
        }
    }

    /** The value kept under `key`, or null. */
    public static byte[] load(Context c, String key)
    {
        try {
            String s = c.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(name(key), null);
            SecretKey k = key(false);
            if (s == null || k == null)
                return null;
            byte[] in = Base64.decode(s, Base64.NO_WRAP);
            int n = in[0] & 0xff;
            Cipher ci = Cipher.getInstance("AES/GCM/NoPadding");
            ci.init(Cipher.DECRYPT_MODE, k, new GCMParameterSpec(128, in, 1, n));
            ci.updateAAD(key.getBytes(StandardCharsets.UTF_8));
            return ci.doFinal(in, 1 + n, in.length - 1 - n);
        } catch (Exception e) {
            Log.w(TAG, "secret not read: " + e.getClass().getSimpleName());
            return null;
        }
    }
}
