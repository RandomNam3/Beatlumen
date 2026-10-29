// Other apps' media sessions: title, artist, cover and position of the app
// that plays, and its transport controls (play, pause, next, previous,
// seek). Reading them needs notification access for SessionListener (the
// user grants it in the system settings); notifications themselves are
// not read.
//
// C++ (src/android_capture.cpp) calls watch(), control() and
// requestAccess(); everything here runs on the Android main thread. The
// session goes back as JSON through nativeSession().
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.Activity;
import android.app.NotificationManager;
import android.content.ComponentName;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ApplicationInfo;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.media.MediaMetadata;
import android.media.session.MediaController;
import android.media.session.MediaSessionManager;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.provider.Settings;
import android.util.Log;

import org.json.JSONException;
import org.json.JSONObject;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.util.List;

public final class MediaSessions
{
    private static final String TAG = "Beatlumen";

    // Implemented in src/android_capture.cpp (registered at runtime).
    static native void nativeSession(String json);
    static native void nativeAccess(boolean granted);

    private static final Handler sMain = new Handler(Looper.getMainLooper());
    private static volatile boolean sNativeReady;

    // Main-thread state.
    private static Context sContext;
    private static boolean sWatching;
    private static boolean sListening;
    private static MediaSessionManager sManager;
    private static MediaController sController;
    private static String sLastJson = "";
    private static long sCoverKey;
    private static String sCoverPath = "";
    private static int sCoverSerial;

    private MediaSessions() {}

    static void setNativeReady(boolean ready)
    {
        sNativeReady = ready;
    }

    private static ComponentName component(Context c)
    {
        return new ComponentName(c, SessionListener.class);
    }

    static boolean hasAccess(Context c)
    {
        if (Build.VERSION.SDK_INT >= 27)
            return c.getSystemService(NotificationManager.class).isNotificationListenerAccessGranted(component(c));
        String enabled = Settings.Secure.getString(c.getContentResolver(), "enabled_notification_listeners");
        return enabled != null && enabled.contains(component(c).flattenToString());
    }

    /** Opens the system setting where the user allows the access. */
    static void requestAccess(final Activity activity)
    {
        sMain.post(() -> {
            Intent i;
            if (Build.VERSION.SDK_INT >= 30) {
                i = new Intent(Settings.ACTION_NOTIFICATION_LISTENER_DETAIL_SETTINGS)
                        .putExtra(Settings.EXTRA_NOTIFICATION_LISTENER_COMPONENT_NAME,
                                  component(activity).flattenToString());
            } else {
                i = new Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS);
            }
            try {
                activity.startActivity(i);
            } catch (RuntimeException e) {
                try {
                    activity.startActivity(new Intent(Settings.ACTION_NOTIFICATION_LISTENER_SETTINGS));
                } catch (RuntimeException e2) {
                    Log.w(TAG, "sessions: no settings page", e2);
                }
            }
        });
    }

    /** Follows the playing app's session while on (and access is granted). */
    static void watch(final Context context, final boolean on)
    {
        sMain.post(() -> {
            sContext = context.getApplicationContext();
            sWatching = on;
            update();
        });
    }

    /** The access was granted or withdrawn (SessionListener). */
    static void accessChanged()
    {
        sMain.post(MediaSessions::update);
    }

    private static void update()
    {
        if (sContext == null)
            return;
        final boolean access = hasAccess(sContext);
        if (sNativeReady)
            nativeAccess(access);
        final boolean listen = sWatching && access;
        if (listen == sListening)
            return;
        if (listen) {
            sManager = sContext.getSystemService(MediaSessionManager.class);
            try {
                sManager.addOnActiveSessionsChangedListener(sSessionsChanged, component(sContext), sMain);
                sListening = true;
                pick(sManager.getActiveSessions(component(sContext)));
            } catch (SecurityException e) {
                Log.w(TAG, "sessions: no access", e);
                sListening = false;
            }
        } else {
            if (sManager != null)
                sManager.removeOnActiveSessionsChangedListener(sSessionsChanged);
            sListening = false;
            setController(null);
        }
    }

    private static final MediaSessionManager.OnActiveSessionsChangedListener sSessionsChanged = MediaSessions::pick;

    private static final MediaController.Callback sCallback = new MediaController.Callback() {
        @Override
        public void onPlaybackStateChanged(PlaybackState state)
        {
            report();
        }

        @Override
        public void onMetadataChanged(MediaMetadata metadata)
        {
            report();
        }

        @Override
        public void onSessionDestroyed()
        {
            refresh();
        }
    };

    private static void refresh()
    {
        if (!sListening || sManager == null)
            return;
        try {
            pick(sManager.getActiveSessions(component(sContext)));
        } catch (SecurityException e) {
            setController(null);
        }
    }

    private static boolean isPlaying(MediaController c)
    {
        PlaybackState s = c.getPlaybackState();
        return s != null && (s.getState() == PlaybackState.STATE_PLAYING || s.getState() == PlaybackState.STATE_BUFFERING);
    }

    /** The app that plays (else the one shown so far, else the most recent). */
    private static void pick(List<MediaController> controllers)
    {
        if (!sListening)
            return;
        MediaController best = null;
        if (controllers != null) {
            final String own = sContext.getPackageName();
            for (MediaController c : controllers) {
                if (own.equals(c.getPackageName()))
                    continue;
                if (isPlaying(c)) {
                    best = c;
                    break;
                }
                if (best == null || (sController != null && c.getSessionToken().equals(sController.getSessionToken())))
                    best = c;
            }
        }
        setController(best);
    }

    private static void setController(MediaController c)
    {
        if (sController != null && c != null && sController.getSessionToken().equals(c.getSessionToken())) {
            report();
            return;
        }
        if (sController != null)
            sController.unregisterCallback(sCallback);
        sController = c;
        if (c != null)
            c.registerCallback(sCallback, sMain);
        report();
    }

    // ---- reporting -------------------------------------------------------------------

    private static String text(MediaMetadata m, String... keys)
    {
        for (String k : keys) {
            CharSequence v = m.getText(k);
            if (v != null && v.length() > 0)
                return v.toString();
        }
        return "";
    }

    private static String appLabel(String pkg)
    {
        try {
            PackageManager pm = sContext.getPackageManager();
            ApplicationInfo info = pm.getApplicationInfo(pkg, 0);
            return pm.getApplicationLabel(info).toString();
        } catch (PackageManager.NameNotFoundException e) {
            return pkg;
        }
    }

    private static void report()
    {
        String json = "";
        if (sController != null) {
            try {
                json = sessionJson(sController);
            } catch (JSONException | RuntimeException e) {
                Log.w(TAG, "sessions: could not read", e);
            }
        }
        if (json.equals(sLastJson))
            return;
        sLastJson = json;
        if (sNativeReady)
            nativeSession(json);
    }

    private static String sessionJson(MediaController c) throws JSONException
    {
        JSONObject o = new JSONObject();
        o.put("app", appLabel(c.getPackageName()));
        MediaMetadata m = c.getMetadata();
        double duration = 0;
        if (m != null) {
            o.put("title", text(m, MediaMetadata.METADATA_KEY_TITLE, MediaMetadata.METADATA_KEY_DISPLAY_TITLE));
            o.put("artist", text(m, MediaMetadata.METADATA_KEY_ARTIST, MediaMetadata.METADATA_KEY_ALBUM_ARTIST,
                                 MediaMetadata.METADATA_KEY_DISPLAY_SUBTITLE));
            o.put("album", text(m, MediaMetadata.METADATA_KEY_ALBUM));
            duration = Math.max(0, m.getLong(MediaMetadata.METADATA_KEY_DURATION)) / 1000.0;
            o.put("cover", cover(m));
        } else {
            o.put("cover", cover(null));
        }
        o.put("duration", duration);
        PlaybackState s = c.getPlaybackState();
        boolean playing = false;
        double position = 0;
        boolean canSeek = false;
        if (s != null) {
            playing = s.getState() == PlaybackState.STATE_PLAYING || s.getState() == PlaybackState.STATE_BUFFERING;
            long pos = s.getPosition();
            if (pos >= 0) {
                if (s.getState() == PlaybackState.STATE_PLAYING && s.getLastPositionUpdateTime() > 0)
                    pos += (long) ((SystemClock.elapsedRealtime() - s.getLastPositionUpdateTime()) * s.getPlaybackSpeed());
                position = pos / 1000.0;
            }
            canSeek = (s.getActions() & PlaybackState.ACTION_SEEK_TO) != 0;
        }
        o.put("position", Math.round(position * 10) / 10.0);
        o.put("playing", playing);
        o.put("canSeek", canSeek);
        return o.toString();
    }

    /** The cover as a file for the core (rewritten only when it changes). */
    private static String cover(MediaMetadata m)
    {
        Bitmap b = null;
        if (m != null) {
            b = m.getBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART);
            if (b == null)
                b = m.getBitmap(MediaMetadata.METADATA_KEY_ART);
            if (b == null)
                b = m.getBitmap(MediaMetadata.METADATA_KEY_DISPLAY_ICON);
        }
        final long key = b == null ? 0 : fingerprint(b);
        if (key == sCoverKey)
            return sCoverPath;
        sCoverKey = key;
        if (!sCoverPath.isEmpty())
            new File(sCoverPath).delete();
        sCoverPath = "";
        if (b == null)
            return "";
        File dir = new File(sContext.getCacheDir(), "capture");
        dir.mkdirs();
        File f = new File(dir, "cover-" + (++sCoverSerial) + ".jpg");
        Bitmap scaled = b;
        final int max = 1024;
        if (b.getWidth() > max || b.getHeight() > max) {
            float k = (float) max / Math.max(b.getWidth(), b.getHeight());
            scaled = Bitmap.createScaledBitmap(b, Math.max(1, Math.round(b.getWidth() * k)),
                                               Math.max(1, Math.round(b.getHeight() * k)), true);
        }
        try (FileOutputStream out = new FileOutputStream(f)) {
            scaled.compress(Bitmap.CompressFormat.JPEG, 90, out);
            sCoverPath = f.getAbsolutePath();
        } catch (IOException e) {
            Log.w(TAG, "sessions: could not save the cover", e);
        }
        return sCoverPath;
    }

    /** Size and a few pixels: tells pictures apart without comparing all. */
    private static long fingerprint(Bitmap b)
    {
        final int w = b.getWidth(), h = b.getHeight();
        long k = 1469598103934665603L ^ ((long) w << 32 | h);
        try {
            for (int i = 1; i <= 8; i++) {
                for (int j = 1; j <= 8; j++) {
                    k = (k ^ b.getPixel(w * i / 9, h * j / 9)) * 1099511628211L;
                }
            }
        } catch (IllegalStateException e) {
            // Hardware bitmap: no pixel access.
            k ^= System.identityHashCode(b);
        }
        return k == 0 ? 1 : k;
    }

    // ---- control ---------------------------------------------------------------------

    static void control(final String action, final double position)
    {
        sMain.post(() -> {
            if (sController == null)
                return;
            MediaController.TransportControls t = sController.getTransportControls();
            switch (action) {
            case "play": t.play(); break;
            case "pause": t.pause(); break;
            case "next": t.skipToNext(); break;
            case "prev": t.skipToPrevious(); break;
            case "seek": t.seekTo((long) (Math.max(0, position) * 1000)); break;
            default: break;
            }
        });
    }
}
