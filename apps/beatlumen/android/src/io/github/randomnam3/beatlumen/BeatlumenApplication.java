// Application class: disables Qt's JIT compilers before any Qt library is
// loaded. Hardened Android builds (GrapheneOS) report writable+executable
// memory as dynamic code loading; Qt maps such memory for the QML JIT and
// for the PCRE2 JIT behind QRegularExpression, which Qt's Android plugin
// already uses before main() runs, so setting these in main() is too late.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.system.ErrnoException;
import android.system.Os;
import android.util.Log;

import org.qtproject.qt.android.bindings.QtApplication;

public class BeatlumenApplication extends QtApplication {
    @Override
    public void onCreate()
    {
        try {
            // QML: ahead-of-time compiled code and the interpreter only.
            Os.setenv("QV4_FORCE_INTERPRETER", "1", true);
            // QRegularExpression: interpreted matching.
            Os.setenv("QT_ENABLE_REGEXP_JIT", "0", true);
        } catch (ErrnoException e) {
            Log.w("Beatlumen", "setenv failed: " + e);
        }
        super.onCreate();
    }
}
