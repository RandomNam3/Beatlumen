// Receives the UnifiedPush distributor's broadcasts (endpoint, messages).
// Exported, as the spec requires; forged broadcasts are harmless: messages
// that do not decrypt with the subscription's keys are dropped, and an
// endpoint is only accepted for an instance this app registered.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.BroadcastReceiver;
import android.content.Context;
import android.content.Intent;

public final class PushReceiver extends BroadcastReceiver {
    @Override
    public void onReceive(Context ctx, Intent intent) {
        String action = intent.getAction();
        String instance = intent.getStringExtra("token");
        if (action == null || instance == null)
            return;
        switch (action) {
            case "org.unifiedpush.android.connector.NEW_ENDPOINT": {
                String endpoint = intent.getStringExtra("endpoint");
                if (endpoint != null)
                    UnifiedPush.onEndpoint(ctx, instance, endpoint);
                break;
            }
            case "org.unifiedpush.android.connector.MESSAGE": {
                byte[] body = intent.getByteArrayExtra("bytesMessage");
                if (body != null)
                    UnifiedPush.onMessage(ctx, instance, body, intent.getStringExtra("id"));
                break;
            }
            case "org.unifiedpush.android.connector.REGISTRATION_FAILED":
                UnifiedPush.onFailed(ctx, instance, intent.getStringExtra("reason"));
                break;
            case "org.unifiedpush.android.connector.UNREGISTERED":
                UnifiedPush.onFailed(ctx, instance, "unregistered");
                break;
            default:
                break;
        }
    }
}
