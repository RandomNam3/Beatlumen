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
    private static boolean sResumeOnFocusGain;
    private static boolean sNoisyRegistered;
    private static boolean sServiceRunning;

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

    /** Tears everything down (app exit). */
    public static void release()
    {
        sMain.post(() -> {
            setService(false);
            abandonFocus();
            setNoisyReceiver(false);
            if (sSession != null) {
                sSession.setActive(false);
                sSession.release();
                sSession = null;
            }
        });
    }

    // ---- called from the service / session callbacks (main thread) ---------------

    static void command(String cmd)
    {
        try {
            nativeCommand(cmd);
        } catch (UnsatisfiedLinkError e) {
            Log.w(TAG, "native media command unavailable", e);
        }
    }

    static MediaSession session()
    {
        return sSession;
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
            requestFocus();
            setNoisyReceiver(true);
        } else {
            setNoisyReceiver(false);
            if (sState == STATE_STOPPED)
                abandonFocus();
        }
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
                if (sServiceRunning)
                    MediaPlaybackService.refresh(sContext);
            });
        });
    }

    private static Bitmap decodeCover(String source)
    {
        try {
            byte[] data;
            if (source.startsWith("http://") || source.startsWith("https://")) {
                HttpURLConnection c = (HttpURLConnection) new URL(source).openConnection();
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
