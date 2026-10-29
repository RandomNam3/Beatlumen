// Background playback glue between the Qt app and Android: owns the
// MediaSession (lock screen, headset and Bluetooth controls), audio focus,
// the "becoming noisy" receiver and starts/stops MediaPlaybackService, which
// keeps the process in the foreground while music plays.
//
// C++ (src/android_media.cpp) calls update() and release() from the Qt
// thread; everything here then runs on the Android main thread. User actions
// go back to C++ through the native methods registered by android_media.cpp.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;
import android.content.IntentFilter;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.media.AudioAttributes;
import android.media.AudioFocusRequest;
import android.media.AudioManager;
import android.media.MediaMetadata;
import android.media.session.MediaSession;
import android.media.session.PlaybackState;
import android.os.Build;
import android.os.Handler;
import android.os.Looper;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import java.io.InputStream;
import java.net.HttpURLConnection;
import java.net.URL;
import java.util.concurrent.ExecutorService;
import java.util.concurrent.Executors;

public final class MediaBridge
{
    private static final String TAG = "Beatlumen";

    public static final int STATE_STOPPED = 0;
    public static final int STATE_PAUSED = 1;
    public static final int STATE_PLAYING = 2;
    public static final int STATE_BUFFERING = 3;

    private static final long ACTIONS = PlaybackState.ACTION_PLAY | PlaybackState.ACTION_PAUSE
            | PlaybackState.ACTION_PLAY_PAUSE | PlaybackState.ACTION_SKIP_TO_NEXT
            | PlaybackState.ACTION_SKIP_TO_PREVIOUS | PlaybackState.ACTION_SEEK_TO
            | PlaybackState.ACTION_STOP;

    // Implemented in src/android_media.cpp (registered at runtime).
    static native void nativeCommand(String cmd);
    static native void nativeSeek(long positionMs);

    private static final Handler sMain = new Handler(Looper.getMainLooper());
    private static final ExecutorService sLoader = Executors.newSingleThreadExecutor();

    // Main-thread state.
    private static Context sContext;
    private static MediaSession sSession;
    private static AudioManager sAudio;
    private static AudioFocusRequest sFocusRequest;
    private static boolean sHasFocus;
    private static boolean sWasActive;
    private static boolean sResumeOnFocusGain;
    private static boolean sNoisyRegistered;
    private static boolean sServiceRunning;
    // The app has exited: commands go to resumeInApp().
    private static boolean sNativeGone;
    private static boolean sAppSeen;

    static String sTitle = "";
    static String sArtist = "";
    static String sAlbum = "";
    static Bitmap sCover;
    static int sState = STATE_STOPPED;
    private static String sCoverSource = "";
    private static long sDuration;

    private MediaBridge() {}

    // ---- called from C++ (any thread) -------------------------------------------

    /** Publishes the player state. cover: local file path, http(s) URL or "". */
    public static void update(final Context context, final String title, final String artist, final String album,
                              final String cover, final long durationMs, final long positionMs, final int state)
    {
        final long when = SystemClock.elapsedRealtime();
        sMain.post(() -> {
            try {
                apply(context, title, artist, album, cover, durationMs, positionMs, state, when);
            } catch (RuntimeException e) {
                Log.w(TAG, "media update failed", e);
            }
        });
    }

    /** App exit: playback ends; the session stays for resumption (ResumeService). */
    public static void release()
    {
        sMain.post(() -> {
            setService(false);
            abandonFocus();
            setNoisyReceiver(false);
            sNativeGone = true;
            if (sSession != null) {
                sSession.setActive(false);
                setResumeState();
            }
            // Qt cannot start a second time in the same process: once the
            // app has ended, the process ends too. The system's media
            // controls keep the last track and start a fresh process
            // (ResumeService) when it is played again.
            sMain.postDelayed(() -> {
                if (sNativeGone) {
                    Log.i(TAG, "app closed: ending the process");
                    Process.killProcess(Process.myPid());
                }
            }, 1500);
        });
    }

    // ---- called from the service / session callbacks (main thread) ---------------

    static void command(String cmd)
    {
        if (!sNativeGone) {
            try {
                nativeCommand(cmd);
                return;
            } catch (UnsatisfiedLinkError e) {
                // The app is not running (resumption from the system's
                // media controls): continue there.
            }
        }
        if ("play".equals(cmd) || "toggle".equals(cmd))
            resumeInApp();
    }

    static MediaSession session()
    {
        return sSession;
    }

    /** The media session for ResumeService (main thread), also without the app. */
    static MediaSession sessionFor(Context context)
    {
        if (sContext == null)
            sContext = context.getApplicationContext();
        final boolean fresh = sSession == null;
        ensureSession();
        if (fresh)
            setResumeState();
        return sSession;
    }

    // Shows the last track as paused, ready to play.
    private static void setResumeState()
    {
        if (sContext == null || sSession == null || !ResumeService.hasLast(sContext))
            return;
        sTitle = ResumeService.lastTitle(sContext);
        sArtist = ResumeService.lastArtist(sContext);
        sAlbum = ResumeService.lastAlbum(sContext);
        sCover = ResumeService.lastCover(sContext);
        sCoverSource = "";
        sDuration = 0;
        sState = STATE_PAUSED;
        publishMetadata();
        sSession.setPlaybackState(new PlaybackState.Builder()
                .setActions(ACTIONS)
                .setState(PlaybackState.STATE_PAUSED, 0, 0.0f, SystemClock.elapsedRealtime())
                .build());
    }

    static final String EXTRA_RESUME = "io.github.randomnam3.beatlumen.RESUME";
    private static final int RESUME_NOTIFICATION_ID = 2;

    // Opens the app, which then plays on. Android may refuse to start an
    // activity from the background; a notification to tap is the fallback
    // (removed as soon as the app runs).
    private static void resumeInApp()
    {
        if (sContext == null)
            return;
        Intent i = sContext.getPackageManager().getLaunchIntentForPackage(sContext.getPackageName());
        if (i == null)
            return;
        i.setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        i.putExtra(EXTRA_RESUME, true);
        try {
            sContext.startActivity(i);
        } catch (RuntimeException e) {
            Log.w(TAG, "resume: app not started", e);
        }
        android.app.NotificationManager nm = sContext.getSystemService(android.app.NotificationManager.class);
        if (nm.getNotificationChannel("resume") == null) {
            android.app.NotificationChannel ch = new android.app.NotificationChannel("resume", "Continue playing",
                    android.app.NotificationManager.IMPORTANCE_HIGH);
            ch.setShowBadge(false);
            nm.createNotificationChannel(ch);
        }
        android.app.PendingIntent pi = android.app.PendingIntent.getActivity(sContext, 1, i,
                android.app.PendingIntent.FLAG_IMMUTABLE | android.app.PendingIntent.FLAG_UPDATE_CURRENT);
        android.app.Notification n = new android.app.Notification.Builder(sContext, "resume")
                .setSmallIcon(android.R.drawable.ic_media_play)
                .setContentTitle(sTitle.isEmpty() ? "Beatlumen" : sTitle)
                .setContentText("Tap to continue playing")
                .setContentIntent(pi)
                .setAutoCancel(true)
                .setTimeoutAfter(60_000)
                .build();
        try {
            nm.notify(RESUME_NOTIFICATION_ID, n);
        } catch (RuntimeException e) {
            // No notification permission.
        }
    }

    static void onServiceStopped()
    {
        sServiceRunning = false;
    }

    // ---- implementation (main thread) ------------------------------------------------

    private static void apply(Context context, String title, String artist, String album, String cover,
                              long durationMs, long positionMs, int state, long when)
    {
        if (sContext == null)
            sContext = context.getApplicationContext();
        if (sNativeGone || !sAppSeen) {
            // The app runs (again): the fallback is not needed.
            sNativeGone = false;
            sAppSeen = true;
            sContext.getSystemService(android.app.NotificationManager.class).cancel(RESUME_NOTIFICATION_ID);
        }
        final boolean hasTrack = title != null && !title.isEmpty();
        ensureSession();

        final boolean metaChanged = !eq(title, sTitle) || !eq(artist, sArtist) || !eq(album, sAlbum)
                || durationMs != sDuration;
        sTitle = title == null ? "" : title;
        sArtist = artist == null ? "" : artist;
        sAlbum = album == null ? "" : album;
        sDuration = durationMs;
        final boolean coverChanged = !eq(cover, sCoverSource);
        if (coverChanged) {
            sCoverSource = cover == null ? "" : cover;
            sCover = null;
            loadCover(sCoverSource);
        }
        if (metaChanged || coverChanged)
            publishMetadata();
        if (metaChanged && hasTrack)
            ResumeService.rememberTrack(sContext, sTitle, sArtist, sAlbum);
        sState = hasTrack ? state : STATE_STOPPED;

        int ps;
        switch (sState) {
            case STATE_PLAYING: ps = PlaybackState.STATE_PLAYING; break;
            case STATE_BUFFERING: ps = PlaybackState.STATE_BUFFERING; break;
            case STATE_PAUSED: ps = PlaybackState.STATE_PAUSED; break;
            default: ps = hasTrack ? PlaybackState.STATE_STOPPED : PlaybackState.STATE_NONE; break;
        }
        sSession.setPlaybackState(new PlaybackState.Builder()
                .setActions(ACTIONS)
                .setState(ps, Math.max(0, positionMs), sState == STATE_PLAYING ? 1.0f : 0.0f, when)
                .build());
        sSession.setActive(hasTrack);

        final boolean active = sState == STATE_PLAYING || sState == STATE_BUFFERING;
        if (active) {
            // Asked again whenever playback starts: a focus held earlier may
            // have gone without a callback (another app, a call).
            if (!sWasActive)
                sHasFocus = false;
            requestFocus();
            setNoisyReceiver(true);
        } else {
            setNoisyReceiver(false);
            if (sState == STATE_STOPPED)
                abandonFocus();
        }
        sWasActive = active;
        // The service starts with playback and keeps running while paused; it
        // is in the foreground only while playing (see MediaPlaybackService).
        if (active)
            setService(true);
        else if (sState == STATE_STOPPED)
            setService(false);
        if (sServiceRunning)
            MediaPlaybackService.refresh(sContext);
    }

    private static boolean eq(String a, String b)
    {
        return (a == null ? "" : a).equals(b == null ? "" : b);
    }

    private static void ensureSession()
    {
        if (sSession != null)
            return;
        sSession = new MediaSession(sContext, "Beatlumen");
        sSession.setCallback(new MediaSession.Callback() {
            @Override public void onPlay() { command("play"); }
            @Override public void onPause() { command("pause"); }
            @Override public void onSkipToNext() { command("next"); }
            @Override public void onSkipToPrevious() { command("prev"); }
            @Override public void onStop() { command("pause"); }
            @Override public void onSeekTo(long pos)
            {
                try {
                    nativeSeek(pos);
                } catch (UnsatisfiedLinkError e) {
                    Log.w(TAG, "native seek unavailable", e);
                }
            }
        }, sMain);
        sSession.setSessionActivity(MediaPlaybackService.launchIntent(sContext));
    }

    private static void publishMetadata()
    {
        if (sSession == null)
            return;
        MediaMetadata.Builder b = new MediaMetadata.Builder()
                .putString(MediaMetadata.METADATA_KEY_TITLE, sTitle)
                .putString(MediaMetadata.METADATA_KEY_ARTIST, sArtist)
                .putString(MediaMetadata.METADATA_KEY_ALBUM, sAlbum)
                .putLong(MediaMetadata.METADATA_KEY_DURATION, sDuration);
        if (sCover != null)
            b.putBitmap(MediaMetadata.METADATA_KEY_ALBUM_ART, sCover);
        sSession.setMetadata(b.build());
    }

    // ---- cover art -------------------------------------------------------------------

    private static void loadCover(final String source)
    {
        if (source.isEmpty())
            return;
        sLoader.execute(() -> {
            final Bitmap bmp = decodeCover(source);
            sMain.post(() -> {
                if (!source.equals(sCoverSource))
                    return; // the track changed meanwhile
                sCover = bmp;
                publishMetadata();
                ResumeService.rememberCover(sContext, bmp);
                if (sServiceRunning)
                    MediaPlaybackService.refresh(sContext);
            });
        });
    }

    // Certificates (SHA-256 fingerprints, hex) the user chose to trust, by
    // "host:port" (self-signed certificates of home servers).
    private static final java.util.Map<String, String> sPins = new java.util.concurrent.ConcurrentHashMap<>();

    public static void setCertificatePin(String hostPort, String fingerprint)
    {
        if (fingerprint == null || fingerprint.isEmpty())
            sPins.remove(hostPort.toLowerCase(java.util.Locale.ROOT));
        else
            sPins.put(hostPort.toLowerCase(java.util.Locale.ROOT), fingerprint.toLowerCase(java.util.Locale.ROOT));
    }

    private static String sha256Hex(byte[] der) throws java.security.NoSuchAlgorithmException
    {
        byte[] d = java.security.MessageDigest.getInstance("SHA-256").digest(der);
        StringBuilder sb = new StringBuilder();
        for (byte b : d)
            sb.append(String.format("%02x", b));
        return sb.toString();
    }

    // HTTPS with the system's trust, or else the trusted fingerprint.
    private static void applyPin(javax.net.ssl.HttpsURLConnection c, URL url) throws Exception
    {
        final String pin = sPins.get(url.getHost().toLowerCase(java.util.Locale.ROOT) + ":" + (url.getPort() > 0 ? url.getPort() : 443));
        if (pin == null)
            return;
        javax.net.ssl.TrustManagerFactory tmf = javax.net.ssl.TrustManagerFactory.getInstance(javax.net.ssl.TrustManagerFactory.getDefaultAlgorithm());
        tmf.init((java.security.KeyStore) null);
        final javax.net.ssl.X509TrustManager system = (javax.net.ssl.X509TrustManager) tmf.getTrustManagers()[0];
        javax.net.ssl.X509TrustManager pinned = new javax.net.ssl.X509TrustManager() {
            @Override public void checkClientTrusted(java.security.cert.X509Certificate[] chain, String auth) throws java.security.cert.CertificateException
            {
                throw new java.security.cert.CertificateException("not a server");
            }
            @Override public void checkServerTrusted(java.security.cert.X509Certificate[] chain, String auth) throws java.security.cert.CertificateException
            {
                try {
                    system.checkServerTrusted(chain, auth);
                } catch (java.security.cert.CertificateException e) {
                    try {
                        if (chain.length == 0 || !pin.equals(sha256Hex(chain[0].getEncoded())))
                            throw e;
                    } catch (java.security.NoSuchAlgorithmException n) {
                        throw e;
                    }
                }
            }
            @Override public java.security.cert.X509Certificate[] getAcceptedIssuers()
            {
                return system.getAcceptedIssuers();
            }
        };
        javax.net.ssl.SSLContext ctx = javax.net.ssl.SSLContext.getInstance("TLS");
        ctx.init(null, new javax.net.ssl.TrustManager[] { pinned }, null);
        c.setSSLSocketFactory(ctx.getSocketFactory());
        final javax.net.ssl.HostnameVerifier standard = javax.net.ssl.HttpsURLConnection.getDefaultHostnameVerifier();
        c.setHostnameVerifier((host, session) -> {
            if (standard.verify(host, session))
                return true;
            try {
                java.security.cert.Certificate[] certs = session.getPeerCertificates();
                return certs.length > 0 && pin.equals(sha256Hex(certs[0].getEncoded()));
            } catch (Exception e) {
                return false;
            }
        });
    }

    private static Bitmap decodeCover(String source)
    {
        try {
            byte[] data;
            if (source.startsWith("http://") || source.startsWith("https://")) {
                URL url = new URL(source);
                HttpURLConnection c = (HttpURLConnection) url.openConnection();
                if (c instanceof javax.net.ssl.HttpsURLConnection)
                    applyPin((javax.net.ssl.HttpsURLConnection) c, url);
                c.setConnectTimeout(5000);
                c.setReadTimeout(10000);
                try (InputStream in = c.getInputStream()) {
                    data = readAll(in);
                } finally {
                    c.disconnect();
                }
            } else {
                try (InputStream in = new java.io.FileInputStream(source)) {
                    data = readAll(in);
                }
            }
            BitmapFactory.Options o = new BitmapFactory.Options();
            o.inJustDecodeBounds = true;
            BitmapFactory.decodeByteArray(data, 0, data.length, o);
            int sample = 1;
            while (o.outWidth / (sample * 2) >= 512 && o.outHeight / (sample * 2) >= 512)
                sample *= 2;
            o = new BitmapFactory.Options();
            o.inSampleSize = sample;
            return BitmapFactory.decodeByteArray(data, 0, data.length, o);
        } catch (Exception | OutOfMemoryError e) {
            // Only the error type: messages may contain the URL with the login token.
            Log.w(TAG, "cover not loaded: " + e.getClass().getSimpleName());
            return null;
        }
    }

    private static byte[] readAll(InputStream in) throws java.io.IOException
    {
        java.io.ByteArrayOutputStream out = new java.io.ByteArrayOutputStream();
        byte[] buf = new byte[64 * 1024];
        int n;
        while ((n = in.read(buf)) > 0) {
            out.write(buf, 0, n);
            if (out.size() > 32 * 1024 * 1024)
                throw new java.io.IOException("cover too large");
        }
        return out.toByteArray();
    }

    // ---- foreground service -----------------------------------------------------

    private static void setService(boolean run)
    {
        if (run == sServiceRunning)
            return;
        if (run) {
            try {
                MediaPlaybackService.start(sContext);
                sServiceRunning = true;
            } catch (RuntimeException e) {
                // e.g. ForegroundServiceStartNotAllowedException when the
                // app is in the background without an exemption.
                Log.w(TAG, "playback service not started", e);
            }
        } else {
            sServiceRunning = false;
            MediaPlaybackService.stop(sContext);
        }
    }

    // ---- audio focus and headphones ------------------------------------------

    private static final AudioManager.OnAudioFocusChangeListener sFocusListener = change -> {
        switch (change) {
            case AudioManager.AUDIOFOCUS_GAIN:
                sHasFocus = true;
                if (sResumeOnFocusGain && sState == STATE_PAUSED)
                    command("play");
                sResumeOnFocusGain = false;
                break;
            case AudioManager.AUDIOFOCUS_LOSS:
                // Another player took over for good.
                sHasFocus = false;
                sResumeOnFocusGain = false;
                if (sState == STATE_PLAYING || sState == STATE_BUFFERING)
                    command("pause");
                break;
            case AudioManager.AUDIOFOCUS_LOSS_TRANSIENT:
                // Phone call, navigation prompt...: pause and resume after.
                sResumeOnFocusGain = sState == STATE_PLAYING || sState == STATE_BUFFERING;
                if (sResumeOnFocusGain)
                    command("pause");
                break;
            default:
                // AUDIOFOCUS_LOSS_TRANSIENT_CAN_DUCK: the system ducks us.
                break;
        }
    };

    private static void requestFocus()
    {
        if (sHasFocus)
            return;
        if (sAudio == null)
            sAudio = (AudioManager) sContext.getSystemService(Context.AUDIO_SERVICE);
        if (sFocusRequest == null) {
            sFocusRequest = new AudioFocusRequest.Builder(AudioManager.AUDIOFOCUS_GAIN)
                    .setAudioAttributes(new AudioAttributes.Builder()
                            .setUsage(AudioAttributes.USAGE_MEDIA)
                            .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                            .build())
                    .setOnAudioFocusChangeListener(sFocusListener, sMain)
                    .setWillPauseWhenDucked(false)
                    .build();
        }
        sHasFocus = sAudio.requestAudioFocus(sFocusRequest) == AudioManager.AUDIOFOCUS_REQUEST_GRANTED;
    }

    private static void abandonFocus()
    {
        if (sAudio != null && sFocusRequest != null)
            sAudio.abandonAudioFocusRequest(sFocusRequest);
        sHasFocus = false;
        sResumeOnFocusGain = false;
    }

    private static final BroadcastReceiver sNoisyReceiver = new BroadcastReceiver() {
        @Override
        public void onReceive(Context context, Intent intent)
        {
            // Headphones unplugged / Bluetooth disconnected: do not blast the speaker.
            if (AudioManager.ACTION_AUDIO_BECOMING_NOISY.equals(intent.getAction()))
                command("pause");
        }
    };

    private static void setNoisyReceiver(boolean on)
    {
        if (on == sNoisyRegistered || sContext == null)
            return;
        if (on) {
            IntentFilter f = new IntentFilter(AudioManager.ACTION_AUDIO_BECOMING_NOISY);
            if (Build.VERSION.SDK_INT >= 33)
                sContext.registerReceiver(sNoisyReceiver, f, Context.RECEIVER_NOT_EXPORTED);
            else
                sContext.registerReceiver(sNoisyReceiver, f);
        } else {
            sContext.unregisterReceiver(sNoisyReceiver);
        }
        sNoisyRegistered = on;
    }
}
