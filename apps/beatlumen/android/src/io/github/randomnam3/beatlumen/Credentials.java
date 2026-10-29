// Passkeys and saved passwords through Android's Credential Manager
// (Android 14+, framework API, no library): the user's provider (Google
// Password Manager, Bitwarden, …) shows its own sheet. The core builds the
// WebAuthn requests and checks nothing here; results go back as JSON.
// Android lets the app use passkeys of the server's domain only when the
// server publishes /.well-known/assetlinks.json for this app.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.Activity;
import android.content.Context;
import android.content.pm.PackageManager;
import android.content.pm.Signature;
import android.credentials.CreateCredentialException;
import android.credentials.CreateCredentialRequest;
import android.credentials.CreateCredentialResponse;
import android.credentials.Credential;
import android.credentials.CredentialManager;
import android.credentials.CredentialOption;
import android.credentials.GetCredentialException;
import android.credentials.GetCredentialRequest;
import android.credentials.GetCredentialResponse;
import android.os.Build;
import android.os.Bundle;
import android.os.CancellationSignal;
import android.os.OutcomeReceiver;
import android.util.Log;

import org.json.JSONObject;

import java.security.MessageDigest;

public final class Credentials {
    private Credentials() {}

    private static final String TAG = "Beatlumen";
    // Credential types and bundle keys shared with Jetpack's androidx.credentials,
    // which providers understand.
    private static final String TYPE_PUBLIC_KEY = "androidx.credentials.TYPE_PUBLIC_KEY_CREDENTIAL";
    private static final String TYPE_PASSWORD = "android.credentials.TYPE_PASSWORD_CREDENTIAL";
    private static final String KEY_REQUEST_JSON = "androidx.credentials.BUNDLE_KEY_REQUEST_JSON";
    private static final String KEY_SUBTYPE = "androidx.credentials.BUNDLE_KEY_SUBTYPE";
    private static final String KEY_AUTH_JSON = "androidx.credentials.BUNDLE_KEY_AUTHENTICATION_RESPONSE_JSON";
    private static final String KEY_REG_JSON = "androidx.credentials.BUNDLE_KEY_REGISTRATION_RESPONSE_JSON";
    private static final String KEY_ID = "androidx.credentials.BUNDLE_KEY_ID";
    private static final String KEY_PASSWORD = "androidx.credentials.BUNDLE_KEY_PASSWORD";
    private static final String KEY_DISPLAY_INFO = "androidx.credentials.BUNDLE_KEY_REQUEST_DISPLAY_INFO";
    private static final String KEY_USER_ID = "androidx.credentials.BUNDLE_KEY_USER_ID";
    private static final String KEY_PREFER_IMMEDIATE = "androidx.credentials.BUNDLE_KEY_PREFER_IMMEDIATELY_AVAILABLE_CREDENTIALS";

    static native void nativeResult(String kind, boolean ok, String json);

    /** Whether the system Credential Manager can be used. */
    public static boolean available() {
        return Build.VERSION.SDK_INT >= 34;
    }

    private static void result(String kind, boolean ok, String json) {
        try {
            nativeResult(kind, ok, json);
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "credentials: app not running");
        }
    }

    private static String error(String message, String type) {
        try {
            return new JSONObject().put("error", message == null ? "" : message).put("type", type == null ? "" : type).toString();
        } catch (Exception e) {
            return "{}";
        }
    }

    /**
     * Requests a credential: a passkey for `requestJson` (WebAuthn options in
     * their JSON form; may be empty) and/or a saved password.
     */
    public static void get(Activity activity, String requestJson, boolean password) {
        if (!available()) {
            result("get", false, error("Android 14 or newer is needed", "unsupported"));
            return;
        }
        activity.runOnUiThread(() -> {
            try {
                CredentialManager cm = activity.getSystemService(CredentialManager.class);
                GetCredentialRequest.Builder b = new GetCredentialRequest.Builder(new Bundle());
                if (requestJson != null && !requestJson.isEmpty()) {
                    Bundle data = new Bundle();
                    data.putString(KEY_REQUEST_JSON, requestJson);
                    data.putString(KEY_SUBTYPE, "androidx.credentials.BUNDLE_VALUE_SUBTYPE_GET_PUBLIC_KEY_CREDENTIAL_OPTION");
                    b.addCredentialOption(new CredentialOption.Builder(TYPE_PUBLIC_KEY, data, data).build());
                }
                if (password) {
                    Bundle data = new Bundle();
                    b.addCredentialOption(new CredentialOption.Builder(TYPE_PASSWORD, data, data).build());
                }
                cm.getCredential(activity, b.build(), new CancellationSignal(), activity.getMainExecutor(),
                        new OutcomeReceiver<GetCredentialResponse, GetCredentialException>() {
                            @Override public void onResult(GetCredentialResponse r) {
                                Credential c = r.getCredential();
                                Bundle d = c.getData();
                                try {
                                    JSONObject o = new JSONObject().put("type", c.getType());
                                    if (TYPE_PASSWORD.equals(c.getType())) {
                                        o.put("user", d.getString(KEY_ID, "")).put("password", d.getString(KEY_PASSWORD, ""));
                                    } else {
                                        o.put("response", new JSONObject(d.getString(KEY_AUTH_JSON, "{}")));
                                    }
                                    result("get", true, o.toString());
                                } catch (Exception e) {
                                    result("get", false, error(e.toString(), "parse"));
                                }
                            }
                            @Override public void onError(GetCredentialException e) {
                                result("get", false, error(e.getMessage(), e.getType()));
                            }
                        });
            } catch (Throwable e) {
                result("get", false, error(e.toString(), "exception"));
            }
        });
    }

    /** Creates a passkey (WebAuthn creation options in their JSON form). */
    public static void createPasskey(Activity activity, String requestJson, String userName) {
        if (!available()) {
            result("create", false, error("Android 14 or newer is needed", "unsupported"));
            return;
        }
        activity.runOnUiThread(() -> {
            try {
                CredentialManager cm = activity.getSystemService(CredentialManager.class);
                Bundle display = new Bundle();
                display.putCharSequence(KEY_USER_ID, userName);
                Bundle data = new Bundle();
                data.putString(KEY_REQUEST_JSON, requestJson);
                data.putString(KEY_SUBTYPE, "androidx.credentials.BUNDLE_VALUE_SUBTYPE_CREATE_PUBLIC_KEY_CREDENTIAL_REQUEST");
                data.putBundle(KEY_DISPLAY_INFO, display);
                data.putBoolean(KEY_PREFER_IMMEDIATE, false);
                CreateCredentialRequest req = new CreateCredentialRequest.Builder(TYPE_PUBLIC_KEY, data, data)
                        .setIsSystemProviderRequired(false)
                        .setAlwaysSendAppInfoToProvider(true)
                        .build();
                cm.createCredential(activity, req, new CancellationSignal(), activity.getMainExecutor(),
                        new OutcomeReceiver<CreateCredentialResponse, CreateCredentialException>() {
                            @Override public void onResult(CreateCredentialResponse r) {
                                try {
                                    JSONObject o = new JSONObject().put("response",
                                            new JSONObject(r.getData().getString(KEY_REG_JSON, "{}")));
                                    result("create", true, o.toString());
                                } catch (Exception e) {
                                    result("create", false, error(e.toString(), "parse"));
                                }
                            }
                            @Override public void onError(CreateCredentialException e) {
                                result("create", false, error(e.getMessage(), e.getType()));
                            }
                        });
            } catch (Throwable e) {
                result("create", false, error(e.toString(), "exception"));
            }
        });
    }

    /** Offers to save a password in the user's password manager. */
    public static void savePassword(Activity activity, String user, String password) {
        if (!available())
            return;
        activity.runOnUiThread(() -> {
            try {
                CredentialManager cm = activity.getSystemService(CredentialManager.class);
                Bundle display = new Bundle();
                display.putCharSequence(KEY_USER_ID, user);
                Bundle data = new Bundle();
                data.putString(KEY_ID, user);
                data.putString(KEY_PASSWORD, password);
                data.putBundle(KEY_DISPLAY_INFO, display);
                CreateCredentialRequest req = new CreateCredentialRequest.Builder(TYPE_PASSWORD, data, new Bundle())
                        .setIsSystemProviderRequired(false)
                        .setAlwaysSendAppInfoToProvider(true)
                        .build();
                cm.createCredential(activity, req, new CancellationSignal(), activity.getMainExecutor(),
                        new OutcomeReceiver<CreateCredentialResponse, CreateCredentialException>() {
                            @Override public void onResult(CreateCredentialResponse r) {}
                            @Override public void onError(CreateCredentialException e) {
                                Log.i(TAG, "credentials: password not saved: " + e.getType());
                            }
                        });
            } catch (Throwable e) {
                Log.w(TAG, "credentials: " + e);
            }
        });
    }

    /** SHA-256 of the app's signing certificate (hex), for --android-cert. */
    public static String certificateFingerprint(Context ctx) {
        try {
            Signature[] sigs = ctx.getPackageManager()
                    .getPackageInfo(ctx.getPackageName(), PackageManager.GET_SIGNING_CERTIFICATES)
                    .signingInfo.getApkContentsSigners();
            byte[] d = MessageDigest.getInstance("SHA-256").digest(sigs[0].toByteArray());
            StringBuilder sb = new StringBuilder();
            for (byte b : d)
                sb.append(String.format("%02x", b));
            return sb.toString();
        } catch (Exception e) {
            return "";
        }
    }
}
