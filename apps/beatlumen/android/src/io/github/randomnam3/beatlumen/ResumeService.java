// Playback resumption (Android 11+): after the app was closed, the system's
// media controls keep a card for the last track. The system asks this
// service for "recent" media (EXTRA_RECENT) and sends play to the media
// session; MediaBridge then continues in the app, where the track left off.
//
// Only the system's media controls (MEDIA_CONTENT_CONTROL) and the app
// itself may browse; other apps get nothing.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.Context;
import android.content.SharedPreferences;
import android.content.pm.PackageManager;
import android.graphics.Bitmap;
import android.graphics.BitmapFactory;
import android.media.MediaDescription;
import android.media.browse.MediaBrowser;
import android.os.Bundle;
import android.os.Process;
import android.service.media.MediaBrowserService;

import java.io.File;
import java.io.FileOutputStream;
import java.util.ArrayList;
import java.util.List;

public class ResumeService extends MediaBrowserService
{
    private static final String PREFS = "resume";
    private static final String ROOT_RECENT = "recent";
    private static final String ROOT_EMPTY = "root";
    private static final String COVER_FILE = "resume-cover.jpg";

    @Override
    public void onCreate()
    {
        super.onCreate();
        setSessionToken(MediaBridge.sessionFor(this).getSessionToken());
    }

    @Override
    public BrowserRoot onGetRoot(String clientPackageName, int clientUid, Bundle rootHints)
    {
        final boolean trusted = clientUid == Process.myUid() || clientUid == Process.SYSTEM_UID
                || getPackageManager().checkPermission("android.permission.MEDIA_CONTENT_CONTROL", clientPackageName)
                        == PackageManager.PERMISSION_GRANTED;
        if (!trusted)
            return null;
        if (rootHints != null && rootHints.getBoolean(BrowserRoot.EXTRA_RECENT) && hasLast(this)) {
            Bundle extras = new Bundle();
            extras.putBoolean(BrowserRoot.EXTRA_RECENT, true);
            return new BrowserRoot(ROOT_RECENT, extras);
        }
        return new BrowserRoot(ROOT_EMPTY, null);
    }

    @Override
    public void onLoadChildren(String parentId, Result<List<MediaBrowser.MediaItem>> result)
    {
        List<MediaBrowser.MediaItem> items = new ArrayList<>();
        if (ROOT_RECENT.equals(parentId) && hasLast(this)) {
            SharedPreferences p = getSharedPreferences(PREFS, MODE_PRIVATE);
            MediaDescription.Builder d = new MediaDescription.Builder()
                    .setMediaId("last")
                    .setTitle(p.getString("title", ""))
                    .setSubtitle(p.getString("artist", ""))
                    .setDescription(p.getString("album", ""));
            Bitmap cover = lastCover(this);
            if (cover != null)
                d.setIconBitmap(cover);
            items.add(new MediaBrowser.MediaItem(d.build(), MediaBrowser.MediaItem.FLAG_PLAYABLE));
        }
        result.sendResult(items);
    }

    // ---- the last track (MediaBridge, main thread) ---------------------------------

    static boolean hasLast(Context c)
    {
        return !c.getSharedPreferences(PREFS, MODE_PRIVATE).getString("title", "").isEmpty();
    }

    static void rememberTrack(Context c, String title, String artist, String album)
    {
        c.getSharedPreferences(PREFS, MODE_PRIVATE).edit()
                .putString("title", title).putString("artist", artist).putString("album", album).apply();
        new File(c.getFilesDir(), COVER_FILE).delete();
    }

    static String lastTitle(Context c)
    {
        return c.getSharedPreferences(PREFS, MODE_PRIVATE).getString("title", "");
    }

    static String lastArtist(Context c)
    {
        return c.getSharedPreferences(PREFS, MODE_PRIVATE).getString("artist", "");
    }

    static String lastAlbum(Context c)
    {
        return c.getSharedPreferences(PREFS, MODE_PRIVATE).getString("album", "");
    }

    static void rememberCover(Context c, Bitmap cover)
    {
        if (cover == null)
            return;
        try (FileOutputStream out = new FileOutputStream(new File(c.getFilesDir(), COVER_FILE))) {
            Bitmap small = cover.getWidth() > 256 ? Bitmap.createScaledBitmap(cover, 256, 256 * cover.getHeight() / cover.getWidth(), true) : cover;
            small.compress(Bitmap.CompressFormat.JPEG, 85, out);
        } catch (Exception e) {
            // Without a cover then.
        }
    }

    static Bitmap lastCover(Context c)
    {
        File f = new File(c.getFilesDir(), COVER_FILE);
        return f.exists() ? BitmapFactory.decodeFile(f.getPath()) : null;
    }
}
