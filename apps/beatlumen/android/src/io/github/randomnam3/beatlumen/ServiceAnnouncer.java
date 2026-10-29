// Announces the AirPlay receiver in the local network through Android's
// own service discovery (NsdManager), which answers the mDNS queries of
// AirPlay senders for the app.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.Context;
import android.net.nsd.NsdManager;
import android.net.nsd.NsdServiceInfo;
import android.util.Log;

import org.json.JSONArray;

public final class ServiceAnnouncer {
    private ServiceAnnouncer() {}

    private static final String TAG = "Beatlumen";
    private static NsdManager sManager;
    private static NsdManager.RegistrationListener sListener;

    /** Registers the service; returns false if the system service is missing. */
    public static synchronized boolean register(Context ctx, String instance, String type, int port, String txtJson)
    {
        unregister();
        NsdManager manager = (NsdManager) ctx.getApplicationContext().getSystemService(Context.NSD_SERVICE);
        if (manager == null)
            return false;
        NsdServiceInfo info = new NsdServiceInfo();
        info.setServiceName(instance);
        info.setServiceType(type);
        info.setPort(port);
        try {
            JSONArray txt = new JSONArray(txtJson);
            for (int i = 0; i < txt.length(); i++) {
                String entry = txt.getString(i);
                int eq = entry.indexOf('=');
                if (eq > 0)
                    info.setAttribute(entry.substring(0, eq), entry.substring(eq + 1));
            }
        } catch (Exception e) {
            Log.w(TAG, "AirPlay: bad TXT record: " + e);
        }
        NsdManager.RegistrationListener listener = new NsdManager.RegistrationListener() {
            @Override public void onServiceRegistered(NsdServiceInfo s)
            {
                Log.i(TAG, "AirPlay: announced as " + s.getServiceName());
            }
            @Override public void onRegistrationFailed(NsdServiceInfo s, int error)
            {
                Log.w(TAG, "AirPlay: announcement failed (" + error + ")");
            }
            @Override public void onServiceUnregistered(NsdServiceInfo s) {}
            @Override public void onUnregistrationFailed(NsdServiceInfo s, int error) {}
        };
        try {
            manager.registerService(info, NsdManager.PROTOCOL_DNS_SD, listener);
        } catch (RuntimeException e) {
            Log.w(TAG, "AirPlay: " + e);
            return false;
        }
        sManager = manager;
        sListener = listener;
        return true;
    }

    public static synchronized void unregister()
    {
        if (sManager != null && sListener != null) {
            try {
                sManager.unregisterService(sListener);
            } catch (RuntimeException ignored) {
            }
        }
        sManager = null;
        sListener = null;
    }
}
