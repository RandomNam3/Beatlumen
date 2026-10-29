// Gives MediaSessions.java access to other apps' media sessions: Android
// shares them with enabled notification listeners only. Notifications are
// not read (no onNotificationPosted).
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.service.notification.NotificationListenerService;

public class SessionListener extends NotificationListenerService
{
    @Override
    public void onListenerConnected()
    {
        MediaSessions.accessChanged();
    }

    @Override
    public void onListenerDisconnected()
    {
        MediaSessions.accessChanged();
    }
}
