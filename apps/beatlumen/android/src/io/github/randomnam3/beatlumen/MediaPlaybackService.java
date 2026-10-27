// Foreground service of type mediaPlayback: keeps Beatlumen running while
// music plays with the screen off or the app in the background, and shows
// the media-style notification (cover, title, previous / play-pause / next).
//
// It is started by MediaBridge when playback starts and stays started while
// paused (notification dismissable, not in the foreground) so that resuming
// from the notification or a headset works without a new background start.
//
// SPDX-License-Identifier: GPL-3.0-or-later

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
import android.media.session.MediaSession;
import android.os.Build;
import android.os.IBinder;
import android.os.PowerManager;
import android.util.Log;

public class MediaPlaybackService extends Service
{
    private static final String TAG = "Beatlumen";
    private static final String CHANNEL = "playback";
    private static final int NOTIFICATION_ID = 1;

    static final String ACTION_REFRESH = "io.github.randomnam3.beatlumen.REFRESH";
    static final String ACTION_TOGGLE = "io.github.randomnam3.beatlumen.TOGGLE";
    static final String ACTION_NEXT = "io.github.randomnam3.beatlumen.NEXT";
    static final String ACTION_PREV = "io.github.randomnam3.beatlumen.PREV";
    static final String ACTION_DISMISS = "io.github.randomnam3.beatlumen.DISMISS";

    private static MediaPlaybackService sInstance;

    private boolean mForeground;
    private PowerManager.WakeLock mWakeLock;

    // ---- control (main thread) ----------------------------------------------------

    static void start(Context context)
    {
        Intent i = new Intent(context, MediaPlaybackService.class).setAction(ACTION_REFRESH);
        if (sInstance != null)
            sInstance.refreshNotification();
        else
            context.startForegroundService(i);
    }

    static void stop(Context context)
    {
        // A start still on its way stops itself in onStartCommand (stopping
        // it before startForeground() would crash the app on some versions).
        if (sInstance != null)
            sInstance.shutdown();
    }

    static void refresh(Context context)
    {
        if (sInstance != null)
            sInstance.refreshNotification();
    }

    static PendingIntent launchIntent(Context context)
    {
        Intent i = context.getPackageManager().getLaunchIntentForPackage(context.getPackageName());
        if (i == null)
            i = new Intent();
        i.setFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        return PendingIntent.getActivity(context, 0, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    // ---- service ---------------------------------------------------------------------

    @Override
    public void onCreate()
    {
        super.onCreate();
        sInstance = this;
        NotificationManager nm = getSystemService(NotificationManager.class);
        if (nm.getNotificationChannel(CHANNEL) == null) {
            NotificationChannel ch = new NotificationChannel(CHANNEL, "Playback", NotificationManager.IMPORTANCE_LOW);
            ch.setDescription("Media controls while music is playing");
            ch.setShowBadge(false);
            nm.createNotificationChannel(ch);
        }
        // Started with startForegroundService(): go to the foreground right away.
        goForeground(buildNotification());
    }

    @Override
    public int onStartCommand(Intent intent, int flags, int startId)
    {
        final String action = intent != null ? intent.getAction() : null;
        if (ACTION_TOGGLE.equals(action)) {
            MediaBridge.command("toggle");
        } else if (ACTION_NEXT.equals(action)) {
            MediaBridge.command("next");
        } else if (ACTION_PREV.equals(action)) {
            MediaBridge.command("prev");
        } else if (ACTION_DISMISS.equals(action)) {
            if (!isPlaying()) {
                shutdown();
                return START_NOT_STICKY;
            }
        }
        if (MediaBridge.sState == MediaBridge.STATE_STOPPED) {
            shutdown();
            return START_NOT_STICKY;
        }
        refreshNotification();
        return START_NOT_STICKY;
    }

    @Override
    public void onTaskRemoved(Intent rootIntent)
    {
        // Swiped away from recents: the Qt activity is destroyed and the
        // process exits with it; do not leave a stale notification behind.
        shutdown();
        super.onTaskRemoved(rootIntent);
    }

    @Override
    public void onDestroy()
    {
        setWakeLock(false);
        if (sInstance == this) {
            sInstance = null;
            MediaBridge.onServiceStopped();
        }
        super.onDestroy();
    }

    @Override
    public IBinder onBind(Intent intent)
    {
        return null;
    }

    // ---- notification ----------------------------------------------------------------

    private static boolean isPlaying()
    {
        return MediaBridge.sState == MediaBridge.STATE_PLAYING || MediaBridge.sState == MediaBridge.STATE_BUFFERING;
    }

    private void shutdown()
    {
        if (sInstance == this)
            sInstance = null;
        MediaBridge.onServiceStopped();
        setWakeLock(false);
        stopForeground(STOP_FOREGROUND_REMOVE);
        mForeground = false;
        getSystemService(NotificationManager.class).cancel(NOTIFICATION_ID);
        stopSelf();
    }

    private void refreshNotification()
    {
        final Notification n = buildNotification();
        final boolean playing = isPlaying();
        setWakeLock(playing);
        if (playing) {
            goForeground(n);
        } else {
            if (mForeground) {
                // Paused: leave the foreground but keep the notification,
                // which can now be swiped away (ACTION_DISMISS).
                stopForeground(STOP_FOREGROUND_DETACH);
                mForeground = false;
            }
            getSystemService(NotificationManager.class).notify(NOTIFICATION_ID, n);
        }
    }

    private void goForeground(Notification n)
    {
        try {
            if (Build.VERSION.SDK_INT >= 29)
                startForeground(NOTIFICATION_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK);
            else
                startForeground(NOTIFICATION_ID, n);
            mForeground = true;
        } catch (RuntimeException e) {
            // ForegroundServiceStartNotAllowedException (API 31+) when resumed
            // from the background without an exemption: still show controls.
            Log.w(TAG, "startForeground failed", e);
            getSystemService(NotificationManager.class).notify(NOTIFICATION_ID, n);
        }
    }

    private PendingIntent serviceIntent(String action, int code)
    {
        Intent i = new Intent(this, MediaPlaybackService.class).setAction(action);
        return PendingIntent.getService(this, code, i, PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT);
    }

    private Notification.Action action(int icon, String title, String action, int code)
    {
        return new Notification.Action.Builder(Icon.createWithResource(this, icon), title, serviceIntent(action, code)).build();
    }

    private Notification buildNotification()
    {
        final boolean playing = isPlaying();
        int small = getResources().getIdentifier("ic_stat_beatlumen", "drawable", getPackageName());
        if (small == 0)
            small = android.R.drawable.ic_media_play;
        Notification.Builder b = new Notification.Builder(this, CHANNEL)
                .setSmallIcon(small)
                .setContentTitle(MediaBridge.sTitle)
                .setContentText(MediaBridge.sArtist)
                .setContentIntent(launchIntent(this))
                .setDeleteIntent(serviceIntent(ACTION_DISMISS, 4))
                .setVisibility(Notification.VISIBILITY_PUBLIC)
                .setCategory(Notification.CATEGORY_TRANSPORT)
                .setShowWhen(false)
                .setOnlyAlertOnce(true)
                .setOngoing(playing)
                .addAction(action(android.R.drawable.ic_media_previous, "Previous", ACTION_PREV, 1))
                .addAction(playing ? action(android.R.drawable.ic_media_pause, "Pause", ACTION_TOGGLE, 2)
                                   : action(android.R.drawable.ic_media_play, "Play", ACTION_TOGGLE, 2))
                .addAction(action(android.R.drawable.ic_media_next, "Next", ACTION_NEXT, 3));
        if (!MediaBridge.sAlbum.isEmpty())
            b.setSubText(MediaBridge.sAlbum);
        if (MediaBridge.sCover != null)
            b.setLargeIcon(MediaBridge.sCover);
        Notification.MediaStyle style = new Notification.MediaStyle().setShowActionsInCompactView(0, 1, 2);
        MediaSession session = MediaBridge.session();
        if (session != null)
            style.setMediaSession(session.getSessionToken());
        b.setStyle(style);
        if (Build.VERSION.SDK_INT >= 31)
            b.setForegroundServiceBehavior(Notification.FOREGROUND_SERVICE_IMMEDIATE);
        return b.build();
    }

    // ---- wake lock -------------------------------------------------------------------

    // Keeps the CPU (Qt event loop, decoder thread) running between tracks
    // and while the next track loads; released as soon as playback pauses.
    private void setWakeLock(boolean on)
    {
        if (mWakeLock == null) {
            if (!on)
                return;
            PowerManager pm = getSystemService(PowerManager.class);
            mWakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "Beatlumen:playback");
            mWakeLock.setReferenceCounted(false);
        }
        if (on && !mWakeLock.isHeld())
            mWakeLock.acquire();
        else if (!on && mWakeLock.isHeld())
            mWakeLock.release();
    }
}
