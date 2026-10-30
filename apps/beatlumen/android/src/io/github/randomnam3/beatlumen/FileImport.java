// Copies documents picked with the system file picker (content:// URIs)
// into app storage, because the native core reads plain files.
//
// Names come from the providing app and are untrusted: they are reduced to
// a plain file name (no directories, no "..", no control characters, length
// limited), get an extension from the MIME type when missing and never
// overwrite an existing file. Called from a worker thread (copying can take
// a while); the caller gets the absolute paths of the copies.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.ClipData;
import android.content.ContentResolver;
import android.content.Context;
import android.content.Intent;
import android.database.Cursor;
import android.net.Uri;
import android.provider.OpenableColumns;
import android.util.Log;
import android.webkit.MimeTypeMap;

import java.io.File;
import java.io.FileOutputStream;
import java.io.IOException;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;

public final class FileImport {
    private static final String TAG = "Beatlumen";

    private FileImport() {}

    /** URIs of a picker result (single or multiple selection). */
    public static String[] uris(Intent data) {
        ArrayList<String> out = new ArrayList<>();
        if (data == null)
            return new String[0];
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                Uri u = clip.getItemAt(i).getUri();
                if (u != null)
                    out.add(u.toString());
            }
        } else if (data.getData() != null) {
            out.add(data.getData().toString());
        }
        return out.toArray(new String[0]);
    }

    /**
     * Documents handed over by "Share" (one or several streams) or "Open
     * with" (the intent's data); empty for other intents.
     */
    @SuppressWarnings("deprecation")
    public static String[] shared(Intent intent) {
        ArrayList<String> out = new ArrayList<>();
        if (intent == null || intent.getAction() == null)
            return new String[0];
        String action = intent.getAction();
        if (Intent.ACTION_SEND.equals(action)) {
            Object u = intent.getParcelableExtra(Intent.EXTRA_STREAM);
            if (u instanceof Uri)
                out.add(u.toString());
        } else if (Intent.ACTION_SEND_MULTIPLE.equals(action)) {
            ArrayList<Uri> list = intent.getParcelableArrayListExtra(Intent.EXTRA_STREAM);
            if (list != null)
                for (Uri u : list)
                    if (u != null && out.size() < 200)
                        out.add(u.toString());
        } else if (Intent.ACTION_VIEW.equals(action) && intent.getData() != null
                && "content".equals(intent.getData().getScheme())) {
            out.add(intent.getData().toString());
        }
        return out.toArray(new String[0]);
    }

    /** The text of a shared link ("Share" with text/plain), or null. */
    public static String sharedText(Intent intent) {
        if (intent == null || !Intent.ACTION_SEND.equals(intent.getAction()))
            return null;
        CharSequence t = intent.getCharSequenceExtra(Intent.EXTRA_TEXT);
        return t == null ? null : t.toString();
    }

    /**
     * Copies each URI into {@code destDir} and returns the paths of the
     * copies. Files larger than {@code maxBytes} are skipped.
     */
    public static String[] copy(Context ctx, String[] uris, String destDir, long maxBytes) {
        ArrayList<String> out = new ArrayList<>();
        File dir = new File(destDir);
        if (!dir.isDirectory() && !dir.mkdirs())
            return new String[0];
        ContentResolver cr = ctx.getContentResolver();
        for (String s : uris) {
            Uri uri = Uri.parse(s);
            if (!"content".equals(uri.getScheme()))
                continue; // only documents granted by the picker
            File target = null;
            try {
                String name = safeName(displayName(cr, uri), cr.getType(uri));
                target = unique(dir, name);
                long total = 0;
                try (InputStream in = cr.openInputStream(uri); OutputStream os = new FileOutputStream(target)) {
                    if (in == null)
                        throw new IOException("no data");
                    byte[] buf = new byte[1 << 16];
                    int n;
                    while ((n = in.read(buf)) > 0) {
                        total += n;
                        if (total > maxBytes)
                            throw new IOException("file too large");
                        os.write(buf, 0, n);
                    }
                }
                out.add(target.getAbsolutePath());
            } catch (IOException | RuntimeException e) {
                Log.w(TAG, "cannot import picked file: " + e.getMessage());
                if (target != null)
                    target.delete();
            }
        }
        return out.toArray(new String[0]);
    }

    private static String displayName(ContentResolver cr, Uri uri) {
        try (Cursor c = cr.query(uri, new String[] { OpenableColumns.DISPLAY_NAME }, null, null, null)) {
            if (c != null && c.moveToFirst() && !c.isNull(0))
                return c.getString(0);
        } catch (RuntimeException e) {
            // Fall back to the last path segment.
        }
        String last = uri.getLastPathSegment();
        return last != null ? last : "file";
    }

    /** A plain, harmless file name with an extension. */
    static String safeName(String name, String mime) {
        String n = name == null ? "" : name;
        int slash = Math.max(n.lastIndexOf('/'), n.lastIndexOf('\\'));
        n = n.substring(slash + 1);
        StringBuilder sb = new StringBuilder();
        for (int i = 0; i < n.length(); i++) {
            char ch = n.charAt(i);
            sb.append(ch < 0x20 || ch == 0x7f || ch == ':' || "*?\"<>|".indexOf(ch) >= 0 ? '_' : ch);
        }
        n = sb.toString().trim();
        while (n.startsWith("."))
            n = n.substring(1);
        if (n.isEmpty())
            n = "file";
        String ext = mime == null ? null : MimeTypeMap.getSingleton().getExtensionFromMimeType(mime);
        int dot = n.lastIndexOf('.');
        if (dot < 0 && ext != null)
            n = n + "." + ext;
        if (n.length() > 120) {
            dot = n.lastIndexOf('.');
            String e = dot > 0 && n.length() - dot <= 8 ? n.substring(dot) : "";
            n = n.substring(0, 120 - e.length()) + e;
        }
        return n;
    }

    private static File unique(File dir, String name) {
        File f = new File(dir, name);
        int dot = name.lastIndexOf('.');
        String stem = dot > 0 ? name.substring(0, dot) : name;
        String ext = dot > 0 ? name.substring(dot) : "";
        for (int i = 2; f.exists(); i++)
            f = new File(dir, stem + " (" + i + ")" + ext);
        return f;
    }
}
