// What the system covers of the window: its bars, a notch and the keyboard.
// Qt reports the bars only as they are at the moment and misses changes while
// the window keeps its size; this asks for them whether shown or not, so that
// the layout keeps their room and does not jump.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.app.Activity;
import android.content.Context;
import android.graphics.Insets;
import android.os.Build;
import android.view.View;
import android.view.WindowInsets;

public final class WindowGaps {
    private WindowGaps() {}

    /** Left, top, right, bottom of bars and notch, then the keyboard's height, in pixels (main thread). */
    public static int[] of(Context context)
    {
        int[] out = new int[5];
        if (!(context instanceof Activity) || Build.VERSION.SDK_INT < Build.VERSION_CODES.R)
            return out;
        View decor = ((Activity) context).getWindow().getDecorView();
        WindowInsets w = decor.getRootWindowInsets();
        if (w == null)
            return out;
        Insets bars = w.getInsetsIgnoringVisibility(WindowInsets.Type.systemBars());
        Insets cut = w.getInsets(WindowInsets.Type.displayCutout());
        out[0] = Math.max(bars.left, cut.left);
        out[1] = Math.max(bars.top, cut.top);
        out[2] = Math.max(bars.right, cut.right);
        out[3] = Math.max(bars.bottom, cut.bottom);
        out[4] = w.isVisible(WindowInsets.Type.ime()) ? w.getInsets(WindowInsets.Type.ime()).bottom : 0;
        return out;
    }
}
