// Quick settings tile "Visualize other apps": opens Beatlumen and starts
// recording other apps' playback (the system asks for permission in the
// app); while it runs, the tile stops it.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.PendingIntent;
import android.content.Intent;
import android.os.Build;
import android.service.quicksettings.Tile;
import android.service.quicksettings.TileService;

public class CaptureTile extends TileService
{
    static final String EXTRA_CAPTURE = "io.github.randomnam3.beatlumen.CAPTURE";

    @Override
    public void onStartListening()
    {
        Tile t = getQsTile();
        if (t == null)
            return;
        t.setState(CaptureService.isRunning() ? Tile.STATE_ACTIVE : Tile.STATE_INACTIVE);
        t.updateTile();
    }

    @Override
    public void onClick()
    {
        if (CaptureService.isRunning()) {
            CaptureService.stop();
            Tile t = getQsTile();
            if (t != null) {
                t.setState(Tile.STATE_INACTIVE);
                t.updateTile();
            }
            return;
        }
        Intent i = getPackageManager().getLaunchIntentForPackage(getPackageName());
        if (i == null)
            return;
        i.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        i.putExtra(EXTRA_CAPTURE, true);
        if (Build.VERSION.SDK_INT >= 34) {
            startActivityAndCollapse(PendingIntent.getActivity(this, 20, i,
                    PendingIntent.FLAG_IMMUTABLE | PendingIntent.FLAG_UPDATE_CURRENT));
        } else {
            startActivityAndCollapse(i);
        }
    }
}
