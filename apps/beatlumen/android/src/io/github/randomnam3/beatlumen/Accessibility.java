// Qt answers every question of an accessibility service on its own thread while
// Android's main thread waits, several round trips for each control, and it asks
// again whenever something moves or appears. A service that reads nothing to
// anybody (a password manager, a remote mouse) then keeps the main thread busy
// for minutes: no keyboard, "not responding", and a crash when the playback
// service cannot start in time. So Qt's bridge is only on for those who need it.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.accessibilityservice.AccessibilityServiceInfo;
import android.content.Context;
import android.content.SharedPreferences;
import android.system.ErrnoException;
import android.system.Os;
import android.util.Log;
import android.view.accessibility.AccessibilityManager;
import android.widget.Toast;

import java.util.Locale;

public final class Accessibility {
    /** Read by Qt's Android plugin before it turns its bridge on. */
    private static final String QT_OFF = "QT_ANDROID_DISABLE_ACCESSIBILITY";
    private static final String PREFS = "accessibility";
    private static final String KEY_ALWAYS = "always";

    private Accessibility() {}

    /** Somebody relies on what the app says about its controls. */
    static boolean assistantRunning(Context context)
    {
        AccessibilityManager am = context.getSystemService(AccessibilityManager.class);
        if (am == null)
            return false;
        if (am.isTouchExplorationEnabled())
            return true;
        for (AccessibilityServiceInfo s :
                am.getEnabledAccessibilityServiceList(AccessibilityServiceInfo.FEEDBACK_ALL_MASK)) {
            final int spoken = AccessibilityServiceInfo.FEEDBACK_SPOKEN | AccessibilityServiceInfo.FEEDBACK_BRAILLE;
            if ((s.feedbackType & spoken) != 0)
                return true;
            // Switch and voice control give no feedback of their own.
            final String pkg = s.getResolveInfo().serviceInfo.packageName.toLowerCase(Locale.ROOT);
            if (pkg.contains("accessibility") || pkg.contains("talkback"))
                return true;
        }
        return false;
    }

    /** At the very start, before Qt is loaded. */
    static void applyAtStart(Context context)
    {
        if (always(context) || assistantRunning(context))
            return;
        try {
            Os.setenv(QT_OFF, "1", true);
        } catch (ErrnoException e) {
            Log.w("Beatlumen", "setenv failed: " + e);
        }
        // Turned on later: Qt only asks when the first service of all starts.
        AccessibilityManager am = context.getSystemService(AccessibilityManager.class);
        if (am != null)
            am.addTouchExplorationStateChangeListener(on -> {
                if (!on)
                    return;
                try {
                    Os.unsetenv(QT_OFF);
                } catch (ErrnoException e) {
                    Log.w("Beatlumen", "unsetenv failed: " + e);
                }
                final boolean german = Locale.getDefault().getLanguage().equals("de");
                Toast.makeText(context, german
                        ? "Bitte Beatlumen neu starten, damit der Screenreader die App lesen kann."
                        : "Please restart Beatlumen so that the screen reader can read the app.",
                        Toast.LENGTH_LONG).show();
            });
    }

    private static SharedPreferences prefs(Context context)
    {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE);
    }

    /** The user wants the bridge whatever runs (from the next start on). */
    public static boolean always(Context context)
    {
        return prefs(context).getBoolean(KEY_ALWAYS, false);
    }

    public static void setAlways(Context context, boolean on)
    {
        prefs(context).edit().putBoolean(KEY_ALWAYS, on).apply();
    }
}
