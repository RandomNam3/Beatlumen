// Other apps' playback: records what they play (Android 10+ playback
// capture, permitted by the user through MediaProjection) and hands it to
// the core for the visuals (src/android_capture.cpp). Foreground service
// of type mediaProjection with a notification that can stop it.
//
// Apps can opt out of being recorded; those stay silent here (their title
// and controls still work through MediaSessions.java).
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.app.Service;
import android.content.Context;
import android.content.Intent;
import android.content.pm.ServiceInfo;
import android.graphics.drawable.Icon;
import android.media.AudioAttributes;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioPlaybackCaptureConfiguration;
import android.media.AudioRecord;
import android.media.projection.MediaProjection;
import android.media.projection.MediaProjectionConfig;
import android.media.projection.MediaProjectionManager;
import android.os.Build;
import android.os.Handler;
import android.os.IBinder;
import android.os.Looper;
import android.os.Process;
import android.util.Log;

import java.nio.ByteBuffer;
import java.util.Locale;

public class CaptureService extends Service
{
    private static final String TAG = "Beatlumen";
    private static final String CHANNEL = "capture";
    private static final int NOTIFICATION_ID = 2;

    static final String ACTION_START = "io.github.randomnam3.beatlumen.CAPTURE_START";
    static final String ACTION_STOP = "io.github.randomnam3.beatlumen.CAPTURE_STOP";
    private static final String EXTRA_CODE = "code";
    private static final String EXTRA_DATA = "data";

    // Implemented in src/android_capture.cpp (registered at runtime).
    static native void nativeState(boolean running, String error);
    static native void nativePcm(ByteBuffer pcm, int bytes, int channels, int rate);

    private static final Handler sMain = new Handler(Looper.getMainLooper());
    private static volatile boolean sNativeReady;
    private static CaptureService sInstance; // main thread

    private MediaProjection mProjection;
    private AudioRecord mRecord;
    private Thread mThread;
    private volatile boolean mRunning;
    private boolean mStopping;

    static void setNativeReady(boolean ready)
    {
        sNativeReady = ready;
    }

    static boolean isRunning()
    {
        return sInstance != null && sInstance.mRunning;
    }

    private static boolean german()
    {
        return "de".equals(Locale.getDefault().getLanguage());
    }

    // ---- control -------------------------------------------------------------------

    /** The system's consent screen; only the whole screen can be chosen
     *  (the recording is of all apps, not one window). */
    static Intent consentIntent(Context context)
    {
        MediaProjectionManager mpm = context.getSystemService(MediaProjectionManager.class);
        if (Build.VERSION.SDK_INT >= 34)
            return mpm.createScreenCaptureIntent(MediaProjectionConfig.createConfigForDefaultDisplay());
        return mpm.createScreenCaptureIntent();
    }

    /** After the user agreed (result of consentIntent()). */
    static void start(Context context, int resultCode, Intent data)
    {
        Intent i = new Intent(context, CaptureService.class)
                .setAction(ACTION_START)
                .putExtra(EXTRA_CODE, resultCode)
                .putExtra(EXTRA_DATA, data);
        try {
            context.startForegroundService(i);
        } catch (RuntimeException e) {
            Log.w(TAG, "capture: could not start", e);
            report(false, e.getMessage());
        }
    }

    static void stop()
    {
        sMain.post(() -> {
            if (sInstance != null)
                sInstance.stopCapture(null);
        });
    }

    private static void report(boolean running, String error)
    {
        if (sNativeReady)
            nativeState(running, error);
    }

    // ---- service ---------------------------------------------------------------------

    @Override
    public void onCreate()
    {
        super.onCreate();
        sInstance = this;
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel ch = new NotificationChannel(CHANNEL,
                    german() ? "Andere Apps" : "Other apps", NotificationManager.IMPORTANCE_LOW);
            ch.setShowBadge(false);
            nm.createNotificationChannel(ch);
        }
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId)
    {
        final String action = intent != null ? intent.getAction() : null;
        if (ACTION_START.equals(action)) {
            // Android 14+: in the foreground before the projection is taken.
            try {
                startForeground(NOTIFICATION_ID, buildNotification(), ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION);
            } catch (RuntimeException e) {
                Log.w(TAG, "capture: startForeground failed", e);
                finish(e.getMessage());
                return START_NOT_STICKY;
            }
            Intent data;
            if (Build.VERSION.SDK_INT >= 33)
                data = intent.getParcelableExtra(EXTRA_DATA, Intent.class);
            else
                data = intent.getParcelableExtra(EXTRA_DATA);
            try {
                startCapture(intent.getIntExtra(EXTRA_CODE, 0), data);
            } catch (Exception e) {
                Log.w(TAG, "capture: could not record", e);
                stopCapture(e.getMessage() != null ? e.getMessage() : e.toString());
            }
        } else {
            // ACTION_STOP (notification) or restarted without a projection.
            stopCapture(null);
        }
        return START_NOT_STICKY;
    }

    @Override
    public void onDestroy()
    {
        stopCapture(null);
        if (sInstance == this)
            sInstance = null;
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent)
    {
        return null;
    }

    // ---- recording ---------------------------------------------------------------------

    private void startCapture(int resultCode, Intent data) throws Exception
    {
        if (mRecord != null)
            return;
        if (data == null)
            throw new IllegalStateException("no permission to record");
        MediaProjectionManager mpm = getSystemService(MediaProjectionManager.class);
        mProjection = mpm.getMediaProjection(resultCode, data);
        if (mProjection == null)
            throw new IllegalStateException("no permission to record");
        mProjection.registerCallback(new MediaProjection.Callback() {
            @Override
            public void onStop()
            {
                // Ended by the system (or the user in the status bar).
                stopCapture(null);
            }
        }, sMain);

        AudioPlaybackCaptureConfiguration config = new AudioPlaybackCaptureConfiguration.Builder(mProjection)
                .addMatchingUsage(AudioAttributes.USAGE_MEDIA)
                .addMatchingUsage(AudioAttributes.USAGE_GAME)
                .addMatchingUsage(AudioAttributes.USAGE_UNKNOWN)
                .excludeUid(Process.myUid())
                .build();
        int rate = 48000;
        AudioManager am = getSystemService(AudioManager.class);
        try {
            String r = am.getProperty(AudioManager.PROPERTY_OUTPUT_SAMPLE_RATE);
            if (r != null)
                rate = Integer.parseInt(r);
        } catch (NumberFormatException e) {
            // keep 48 kHz
        }
        AudioFormat format = new AudioFormat.Builder()
                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                .setSampleRate(rate)
                .setChannelMask(AudioFormat.CHANNEL_IN_STEREO)
                .build();
        int min = AudioRecord.getMinBufferSize(rate, AudioFormat.CHANNEL_IN_STEREO, AudioFormat.ENCODING_PCM_16BIT);
        mRecord = new AudioRecord.Builder()
                .setAudioFormat(format)
                .setBufferSizeInBytes(Math.max(min * 2, rate / 5 * 4))
                .setAudioPlaybackCaptureConfig(config)
                .build();
        if (mRecord.getState() != AudioRecord.STATE_INITIALIZED)
            throw new IllegalStateException("recording is not available");
        mRecord.startRecording();
        mRunning = true;
        final AudioRecord record = mRecord;
        final int sampleRate = rate;
        mThread = new Thread(() -> {
            // 10 ms per read: the visuals follow closely.
            ByteBuffer buf = ByteBuffer.allocateDirect(sampleRate / 100 * 4);
            while (mRunning) {
                buf.clear();
                int n = record.read(buf, buf.capacity(), AudioRecord.READ_BLOCKING);
                if (n < 0) {
                    Log.w(TAG, "capture: read failed " + n);
                    break;
                }
                if (n > 0 && sNativeReady)
                    nativePcm(buf, n, 2, sampleRate);
            }
        }, "bl-capture");
        mThread.start();
        report(true, null);
    }

    private void stopCapture(String error)
    {
        if (mStopping)
            return;
        mStopping = true;
        final boolean wasRunning = mRunning || mRecord != null;
        mRunning = false;
        if (mRecord != null) {
            try {
                mRecord.stop();
            } catch (IllegalStateException e) {
                // not recording
            }
        }
        if (mThread != null) {
            try {
                mThread.join(500);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            mThread = null;
        }
        if (mRecord != null) {
            mRecord.release();
            mRecord = null;
        }
        if (mProjection != null) {
            mProjection.stop();
            mProjection = null;
        }
        if (wasRunning || error != null)
            report(false, error);
        finish(null);
        mStopping = false;
    }

    private void finish(String error)
    {
        if (error != null)
            report(false, error);
        stopForeground(STOP_FOREGROUND_REMOVE);
        stopSelf();
    }

    private Notification buildNotification()
    {
        int small = getResources().getIdentifier("ic_stat_beatlumen", "drawable", getPackageName());
        if (small == 0)
            small = android.R.drawable.ic_media_play;
        Intent stop = new Intent(this, CaptureService.class).setAction(ACTION_STOP);
        PendingIntent stopIntent = PendingIntent.getService(this, 10, stop,
                PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
        final boolean de = german();
        return new Notification.Builder(this, CHANNEL)
                .setSmallIcon(small)
                .setContentTitle(de ? "Visualisiert andere Apps" : "Visualizing other apps")
                .setContentText(de ? "Beatlumen zeigt Effekte zur Wiedergabe anderer Apps."
                                   : "Beatlumen shows visuals for what other apps play.")
                .setContentIntent(MediaPlaybackService.launchIntent(this))
                .setOngoing(true)
                .setShowWhen(false)
                .setCategory(Notification.CATEGORY_SERVICE)
                .addAction(new Notification.Action.Builder(
                        Icon.createWithResource(this, android.R.drawable.ic_menu_close_clear_cancel),
                        de ? "Beenden" : "Stop", stopIntent).build())
                .build();
    }
}
