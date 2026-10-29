// UnifiedPush connector (https://unifiedpush.org/developers/spec/android/),
// implemented directly with Android intents instead of a library: the app
// registers with the distributor the user installed (ntfy, NextPush, …),
// which answers with an endpoint URL; the core registers that endpoint
// with the Beatlumen server. Messages arrive through PushReceiver, are
// decrypted here (WebPushCrypto) and shown as notifications, also while
// the app is closed. No Google services and no phone number or e-mail.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.content.pm.ResolveInfo;
import android.os.Build;
import android.util.Base64;
import android.util.Log;

import org.json.JSONObject;

import java.util.List;
import java.util.Locale;

public final class UnifiedPush {
    private UnifiedPush() {}

    private static final String TAG = "Beatlumen";
    static final String REGISTER = "org.unifiedpush.android.distributor.REGISTER";
    static final String UNREGISTER = "org.unifiedpush.android.distributor.UNREGISTER";
    static final String MESSAGE_ACK = "org.unifiedpush.android.distributor.MESSAGE_ACK";
    private static final String PREFS = "unifiedpush";
    private static final String CHANNEL = "shares";

    /** Set by the app while its native part runs (bridge.cpp). */
    private static volatile boolean sNativeReady;

    static native void nativeEndpoint(String instance, String endpoint, String p256dh, String auth);
    static native void nativeMessage(String json);
    static native void nativeFailed(String instance, String reason);

    public static void setNativeReady(boolean ready) { sNativeReady = ready; }

    private static SharedPreferences prefs(Context ctx) {
        return ctx.getApplicationContext().getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    private static String b64(byte[] b) { return Base64.encodeToString(b, Base64.NO_WRAP); }
    private static byte[] unb64(String s) { return Base64.decode(s, Base64.NO_WRAP); }

    /** The installed distributor to use (the one chosen before, else the first). */
    static String distributor(Context ctx) {
        PackageManager pm = ctx.getPackageManager();
        List<ResolveInfo> list = pm.queryBroadcastReceivers(new Intent(REGISTER), 0);
        String saved = prefs(ctx).getString("distributor", null);
        String first = null;
        for (ResolveInfo ri : list) {
            String pkg = ri.activityInfo.packageName;
            if (pkg.equals(ctx.getPackageName()))
                continue;
            if (pkg.equals(saved))
                return pkg;
            if (first == null)
                first = pkg;
        }
        return first;
    }

    /** The subscription keys of an instance (created on first use). */
    static WebPushCrypto keys(Context ctx, String instance) throws Exception {
        SharedPreferences p = prefs(ctx);
        String priv = p.getString(instance + ".priv", null);
        if (priv != null) {
            return WebPushCrypto.load(unb64(priv), unb64(p.getString(instance + ".pub", "")),
                    unb64(p.getString(instance + ".auth", "")));
        }
        WebPushCrypto k = WebPushCrypto.generate();
        p.edit()
                .putString(instance + ".priv", b64(k.privateKey.getEncoded()))
                .putString(instance + ".pub", b64(k.publicKey.getEncoded()))
                .putString(instance + ".auth", b64(k.auth))
                .apply();
        return k;
    }

    /**
     * Registers an instance (one per server and account). Returns 1 when the
     * request went to a distributor, -1 when none is installed.
     */
    public static synchronized int register(Context ctx, String instance, String vapid) {
        String dist = distributor(ctx);
        if (dist == null)
            return -1;
        try {
            keys(ctx, instance);
        } catch (Exception e) {
            Log.w(TAG, "push: no keys: " + e);
            return 0;
        }
        prefs(ctx).edit().putString("distributor", dist).apply();
        Intent i = new Intent(REGISTER);
        i.setPackage(dist);
        i.putExtra("token", instance);
        i.putExtra("application", ctx.getPackageName());
        i.putExtra("message", "Beatlumen");
        i.putExtra("features", new String[] { "bytes_message" });
        if (vapid != null && !vapid.isEmpty())
            i.putExtra("vapid", vapid);
        // Identifies this app to the distributor (spec AND_2).
        i.putExtra("pi", PendingIntent.getBroadcast(ctx, 0, new Intent("io.github.randomnam3.beatlumen.UNIFIEDPUSH"),
                PendingIntent.FLAG_IMMUTABLE));
        ctx.sendBroadcast(i);
        // A known endpoint is reported right away (the distributor repeats
        // it anyway, but may be slow to start).
        String endpoint = prefs(ctx).getString(instance + ".endpoint", null);
        if (endpoint != null)
            deliverEndpoint(ctx, instance, endpoint);
        return 1;
    }

    public static synchronized void unregister(Context ctx, String instance) {
        String dist = prefs(ctx).getString("distributor", null);
        if (dist != null) {
            Intent i = new Intent(UNREGISTER);
            i.setPackage(dist);
            i.putExtra("token", instance);
            ctx.sendBroadcast(i);
        }
        prefs(ctx).edit()
                .remove(instance + ".priv").remove(instance + ".pub")
                .remove(instance + ".auth").remove(instance + ".endpoint")
                .apply();
    }

    static boolean knows(Context ctx, String instance) {
        return prefs(ctx).contains(instance + ".priv");
    }

    static void onEndpoint(Context ctx, String instance, String endpoint) {
        if (!knows(ctx, instance))
            return;
        prefs(ctx).edit().putString(instance + ".endpoint", endpoint).apply();
        deliverEndpoint(ctx, instance, endpoint);
    }

    private static void deliverEndpoint(Context ctx, String instance, String endpoint) {
        if (!sNativeReady)
            return;
        try {
            WebPushCrypto k = keys(ctx, instance);
            nativeEndpoint(instance, endpoint, b64(WebPushCrypto.uncompressed(k.publicKey)), b64(k.auth));
        } catch (Exception e) {
            Log.w(TAG, "push: " + e);
        }
    }

    static void onFailed(Context ctx, String instance, String reason) {
        prefs(ctx).edit().remove(instance + ".endpoint").apply();
        if (sNativeReady)
            nativeFailed(instance, reason == null ? "" : reason);
    }

    static void onMessage(Context ctx, String instance, byte[] body, String id) {
        if (!knows(ctx, instance))
            return;
        acknowledge(ctx, instance, id);
        String json;
        try {
            json = new String(keys(ctx, instance).decrypt(body), "UTF-8");
        } catch (Exception e) {
            // Not from our server (or changed on the way): ignored.
            Log.w(TAG, "push: message rejected: " + e);
            return;
        }
        notifyUser(ctx, json);
        if (sNativeReady)
            nativeMessage(json);
    }

    private static void acknowledge(Context ctx, String instance, String id) {
        String dist = prefs(ctx).getString("distributor", null);
        if (dist == null || id == null)
            return;
        Intent i = new Intent(MESSAGE_ACK);
        i.setPackage(dist);
        i.putExtra("token", instance);
        i.putExtra("id", id);
        ctx.sendBroadcast(i);
    }

    private static void notifyUser(Context ctx, String json) {
        String from;
        try {
            JSONObject o = new JSONObject(json);
            if (!"share".equals(o.optString("type")))
                return;
            from = o.optString("from", "?");
        } catch (Exception e) {
            return;
        }
        boolean de = Locale.getDefault().getLanguage().equals("de");
        NotificationManager nm = (NotificationManager) ctx.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm == null)
            return;
        if (nm.getNotificationChannel(CHANNEL) == null) {
            nm.createNotificationChannel(new NotificationChannel(CHANNEL, de ? "Freigaben" : "Shared files",
                    NotificationManager.IMPORTANCE_DEFAULT));
        }
        Intent open = ctx.getPackageManager().getLaunchIntentForPackage(ctx.getPackageName());
        PendingIntent pi = open == null ? null
                : PendingIntent.getActivity(ctx, 1, open, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        Notification n = new Notification.Builder(ctx, CHANNEL)
                .setSmallIcon(ctx.getApplicationInfo().icon)
                .setContentTitle(de ? "Neue Freigabe" : "New shared file")
                .setContentText(from + (de ? " hat eine Datei mit dir geteilt" : " shared a file with you"))
                .setContentIntent(pi)
                .setAutoCancel(true)
                .build();
        nm.notify((int) (System.currentTimeMillis() & 0x7fffffff), n);
    }
}
