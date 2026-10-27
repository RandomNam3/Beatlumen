// Beatlumen - Android activity helper.
// SPDX-License-Identifier: GPL-3.0-or-later
//
// A thin subclass of NativeActivity. The application runs in native code
// (libbeatlumen.so); this class only provides services that need Java:
// soft keyboard text input, runtime permissions, the document picker,
// clipboard, keep-screen-on and immersive mode. Native code calls the
// public methods below from its own (attached) thread; everything that
// touches views is posted to the UI thread. The static native methods
// are registered by the library in ANativeActivity_onCreate.

package io.github.randomnam3.beatlumen;

import android.Manifest;
import android.app.NativeActivity;
import android.content.ActivityNotFoundException;
import android.content.ClipData;
import android.content.ClipboardManager;
import android.content.Context;
import android.content.Intent;
import android.content.pm.PackageManager;
import android.database.Cursor;
import android.media.AudioManager;
import android.net.Uri;
import android.os.Build;
import android.os.Bundle;
import android.os.Environment;
import android.provider.OpenableColumns;
import android.provider.Settings;
import android.text.InputType;
import android.util.Log;
import android.view.KeyCharacterMap;
import android.view.KeyEvent;
import android.view.View;
import android.view.ViewGroup;
import android.view.WindowManager;
import android.view.inputmethod.BaseInputConnection;
import android.view.inputmethod.EditorInfo;
import android.view.inputmethod.InputConnection;
import android.view.inputmethod.InputMethodManager;
import android.webkit.MimeTypeMap;

import java.io.File;
import java.io.FileOutputStream;
import java.io.InputStream;
import java.io.OutputStream;
import java.util.ArrayList;
import java.util.LinkedHashSet;

public class MainActivity extends NativeActivity {
    private static final String TAG = "Beatlumen";
    private static final int REQ_PERMISSION = 1;
    private static final int REQ_PICK = 2;

    private TextInputView textView;
    private ClipboardManager clipboard;
    private InputMethodManager imm;
    private volatile boolean immersive = true;
    private boolean permissionDeniedForever = false;

    // ---- native callbacks (registered via JNI RegisterNatives) ---------------

    static native void nativeOnText(String text);
    static native void nativeOnKey(int keyCode);
    static native void nativeOnPermissionResult(boolean granted);
    static native void nativeOnFilesPicked(String[] paths);

    // ---- lifecycle -------------------------------------------------------------

    @Override
    protected void onCreate(Bundle savedInstanceState) {
        super.onCreate(savedInstanceState);
        setVolumeControlStream(AudioManager.STREAM_MUSIC);
        clipboard = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
        imm = (InputMethodManager) getSystemService(Context.INPUT_METHOD_SERVICE);
        textView = new TextInputView(this);
        addContentView(textView, new ViewGroup.LayoutParams(1, 1));
        applyImmersive();
    }

    @Override
    public void onWindowFocusChanged(boolean hasFocus) {
        super.onWindowFocusChanged(hasFocus);
        if (hasFocus) {
            applyImmersive();
        }
    }

    /**
     * The status bar is always hidden (fullscreen theme: the native surface
     * would otherwise extend below it). Immersive mode also hides the
     * navigation bar; a swipe shows the bars transiently over the surface.
     */
    @SuppressWarnings("deprecation")
    private void applyImmersive() {
        View decor = getWindow().getDecorView();
        int flags = View.SYSTEM_UI_FLAG_FULLSCREEN;
        if (immersive) {
            flags = View.SYSTEM_UI_FLAG_LAYOUT_STABLE
                    | View.SYSTEM_UI_FLAG_LAYOUT_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_LAYOUT_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_HIDE_NAVIGATION
                    | View.SYSTEM_UI_FLAG_FULLSCREEN
                    | View.SYSTEM_UI_FLAG_IMMERSIVE_STICKY;
        }
        decor.setSystemUiVisibility(flags);
    }

    // ---- called from native code (any thread) ----------------------------------

    /** Shows or hides the soft keyboard. */
    public void setTextInput(final boolean on) {
        runOnUiThread(() -> {
            if (on) {
                textView.setFocusable(true);
                textView.setFocusableInTouchMode(true);
                textView.requestFocus();
                imm.restartInput(textView);
                imm.showSoftInput(textView, 0);
            } else {
                imm.hideSoftInputFromWindow(textView.getWindowToken(), 0);
                // Drop the editor focus so the IME is not re-shown when the
                // window regains focus.
                textView.setFocusable(false);
            }
        });
    }

    public void setKeepScreenOn(final boolean on) {
        runOnUiThread(() -> {
            if (on) {
                getWindow().addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            } else {
                getWindow().clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON);
            }
        });
    }

    public void setFullscreenMode(final boolean on) {
        immersive = on;
        runOnUiThread(() -> {
            applyImmersive();
        });
    }

    private static String[] mediaPermissions() {
        if (Build.VERSION.SDK_INT >= 33) {
            return new String[] {Manifest.permission.READ_MEDIA_AUDIO, Manifest.permission.READ_MEDIA_IMAGES};
        }
        return new String[] {Manifest.permission.READ_EXTERNAL_STORAGE};
    }

    /** Audio (API 33+) or storage read access. */
    public boolean hasMediaPermission() {
        String p = Build.VERSION.SDK_INT >= 33 ? Manifest.permission.READ_MEDIA_AUDIO : Manifest.permission.READ_EXTERNAL_STORAGE;
        return checkSelfPermission(p) == PackageManager.PERMISSION_GRANTED;
    }

    public void requestMediaPermission() {
        runOnUiThread(() -> {
            if (hasMediaPermission()) {
                nativeOnPermissionResult(true);
            } else if (permissionDeniedForever) {
                // The system no longer shows the dialog: open the app's
                // settings page instead.
                try {
                    Intent i = new Intent(Settings.ACTION_APPLICATION_DETAILS_SETTINGS, Uri.fromParts("package", getPackageName(), null));
                    startActivity(i);
                } catch (ActivityNotFoundException e) {
                    Log.w(TAG, "no settings activity", e);
                }
            } else {
                requestPermissions(mediaPermissions(), REQ_PERMISSION);
            }
        });
    }

    @Override
    public void onRequestPermissionsResult(int requestCode, String[] permissions, int[] grantResults) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults);
        if (requestCode != REQ_PERMISSION) {
            return;
        }
        boolean granted = hasMediaPermission();
        if (!granted && permissions.length > 0 && !shouldShowRequestPermissionRationale(permissions[0])) {
            permissionDeniedForever = true;
        }
        nativeOnPermissionResult(granted);
    }

    /** Opens the system document picker; results arrive as file paths. */
    public void pickFiles(final String accept, final boolean multiple) {
        runOnUiThread(() -> {
            Intent intent = new Intent(Intent.ACTION_OPEN_DOCUMENT);
            intent.addCategory(Intent.CATEGORY_OPENABLE);
            String[] mimes = mimeTypes(accept);
            if (mimes.length == 1) {
                intent.setType(mimes[0]);
            } else {
                intent.setType("*/*");
                if (mimes.length > 1) {
                    intent.putExtra(Intent.EXTRA_MIME_TYPES, mimes);
                }
            }
            intent.putExtra(Intent.EXTRA_ALLOW_MULTIPLE, multiple);
            try {
                startActivityForResult(intent, REQ_PICK);
            } catch (ActivityNotFoundException e) {
                Log.w(TAG, "no document picker", e);
            }
        });
    }

    /** ".mp3,.flac,audio/*" -> MIME types; empty (= any) if an extension is unknown. */
    private static String[] mimeTypes(String accept) {
        LinkedHashSet<String> out = new LinkedHashSet<String>();
        if (accept != null) {
            for (String raw : accept.split(",")) {
                String a = raw.trim().toLowerCase();
                if (a.isEmpty()) {
                    continue;
                }
                if (a.indexOf('/') >= 0) {
                    out.add(a);
                } else {
                    String ext = a.startsWith(".") ? a.substring(1) : a;
                    String m = MimeTypeMap.getSingleton().getMimeTypeFromExtension(ext);
                    if (m == null) {
                        return new String[0];
                    }
                    out.add(m);
                }
            }
        }
        return out.toArray(new String[0]);
    }

    @Override
    protected void onActivityResult(int requestCode, int resultCode, Intent data) {
        super.onActivityResult(requestCode, resultCode, data);
        if (requestCode != REQ_PICK || resultCode != RESULT_OK || data == null) {
            return;
        }
        final ArrayList<Uri> uris = new ArrayList<Uri>();
        ClipData clip = data.getClipData();
        if (clip != null) {
            for (int i = 0; i < clip.getItemCount(); i++) {
                Uri u = clip.getItemAt(i).getUri();
                if (u != null) {
                    uris.add(u);
                }
            }
        } else if (data.getData() != null) {
            uris.add(data.getData());
        }
        if (uris.isEmpty()) {
            return;
        }
        // Copy the documents into the cache so native code gets ordinary
        // seekable files with their real names (the extension selects the
        // decoder).
        new Thread(() -> {
            ArrayList<String> paths = new ArrayList<String>();
            File dir = new File(getCacheDir(), "picked");
            dir.mkdirs();
            for (Uri u : uris) {
                String path = copyToCache(u, dir);
                if (path != null) {
                    paths.add(path);
                }
            }
            if (!paths.isEmpty()) {
                nativeOnFilesPicked(paths.toArray(new String[0]));
            }
        }, "beatlumen-pick").start();
    }

    private String displayName(Uri uri) {
        String name = null;
        Cursor c = null;
        try {
            c = getContentResolver().query(uri, new String[] {OpenableColumns.DISPLAY_NAME}, null, null, null);
            if (c != null && c.moveToFirst()) {
                name = c.getString(0);
            }
        } catch (Exception e) {
            Log.w(TAG, "query " + uri, e);
        } finally {
            if (c != null) {
                c.close();
            }
        }
        if (name == null || name.isEmpty()) {
            name = uri.getLastPathSegment();
        }
        if (name == null || name.isEmpty()) {
            name = "file";
        }
        name = name.replace('/', '_').replace('\\', '_').replace(':', '_');
        if (name.startsWith(".")) {
            name = "_" + name;
        }
        return name;
    }

    private String copyToCache(Uri uri, File dir) {
        File out = new File(dir, displayName(uri));
        InputStream in = null;
        OutputStream os = null;
        try {
            in = getContentResolver().openInputStream(uri);
            if (in == null) {
                return null;
            }
            os = new FileOutputStream(out);
            byte[] buf = new byte[256 * 1024];
            int n;
            while ((n = in.read(buf)) > 0) {
                os.write(buf, 0, n);
            }
            return out.getAbsolutePath();
        } catch (Exception e) {
            Log.w(TAG, "copy " + uri, e);
            return null;
        } finally {
            try {
                if (in != null) {
                    in.close();
                }
                if (os != null) {
                    os.close();
                }
            } catch (Exception ignored) {
            }
        }
    }

    public String getClipboardText() {
        try {
            ClipboardManager cm = (ClipboardManager) getSystemService(Context.CLIPBOARD_SERVICE);
            ClipData clip = cm == null ? null : cm.getPrimaryClip();
            if (clip == null || clip.getItemCount() == 0) {
                return null;
            }
            CharSequence t = clip.getItemAt(0).coerceToText(this);
            return t == null ? null : t.toString();
        } catch (Exception e) {
            Log.w(TAG, "clipboard", e);
            return null;
        }
    }

    public void setClipboardText(final String text) {
        runOnUiThread(() -> {
            try {
                clipboard.setPrimaryClip(ClipData.newPlainText("Beatlumen", text));
            } catch (Exception e) {
                Log.w(TAG, "clipboard", e);
            }
        });
    }

    /** Character produced by a hardware key with the device's layout. */
    public int getUnicodeChar(int deviceId, int keyCode, int metaState) {
        KeyCharacterMap map;
        try {
            map = KeyCharacterMap.load(deviceId);
        } catch (Exception e) {
            map = KeyCharacterMap.load(KeyCharacterMap.VIRTUAL_KEYBOARD);
        }
        return map.get(keyCode, metaState);
    }

    @SuppressWarnings("deprecation")
    public String getExternalStoragePath() {
        File f = Environment.getExternalStorageDirectory();
        return f == null ? null : f.getAbsolutePath();
    }

    // ---- soft keyboard plumbing --------------------------------------------------

    /** Invisible 1x1 view that owns the IME connection. */
    static final class TextInputView extends View {
        TextInputView(Context context) {
            super(context);
            setFocusable(false);
        }

        @Override
        public boolean onCheckIsTextEditor() {
            return true;
        }

        @Override
        public InputConnection onCreateInputConnection(EditorInfo out) {
            out.inputType = InputType.TYPE_CLASS_TEXT | InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS;
            out.imeOptions = EditorInfo.IME_ACTION_DONE | EditorInfo.IME_FLAG_NO_EXTRACT_UI | EditorInfo.IME_FLAG_NO_FULLSCREEN;
            out.initialSelStart = 0;
            out.initialSelEnd = 0;
            return new NativeInputConnection(this);
        }
    }

    /**
     * Forwards IME edits to native code. Composing text is mirrored by
     * deleting/re-typing the changed suffix, so the focused field always
     * shows what the keyboard shows.
     */
    static final class NativeInputConnection extends BaseInputConnection {
        private String composing = "";

        NativeInputConnection(View view) {
            super(view, false);
        }

        private static void typeText(String text) {
            int start = 0;
            for (int i = 0; i < text.length(); i++) {
                if (text.charAt(i) == '\n') {
                    if (i > start) {
                        nativeOnText(text.substring(start, i));
                    }
                    nativeOnKey(KeyEvent.KEYCODE_ENTER);
                    start = i + 1;
                }
            }
            if (start < text.length()) {
                nativeOnText(text.substring(start));
            }
        }

        private void replaceComposing(String text) {
            int common = 0;
            int max = Math.min(composing.length(), text.length());
            while (common < max && composing.charAt(common) == text.charAt(common)) {
                common++;
            }
            if (common > 0 && Character.isHighSurrogate(composing.charAt(common - 1))) {
                common--;
            }
            int del = composing.codePointCount(common, composing.length());
            for (int i = 0; i < del; i++) {
                nativeOnKey(KeyEvent.KEYCODE_DEL);
            }
            if (common < text.length()) {
                typeText(text.substring(common));
            }
        }

        @Override
        public boolean commitText(CharSequence text, int newCursorPosition) {
            replaceComposing(text == null ? "" : text.toString());
            composing = "";
            return true;
        }

        @Override
        public boolean setComposingText(CharSequence text, int newCursorPosition) {
            String t = text == null ? "" : text.toString();
            replaceComposing(t);
            composing = t;
            return true;
        }

        @Override
        public boolean finishComposingText() {
            composing = "";
            return true;
        }

        @Override
        public boolean deleteSurroundingText(int beforeLength, int afterLength) {
            for (int i = 0; i < beforeLength; i++) {
                nativeOnKey(KeyEvent.KEYCODE_DEL);
            }
            for (int i = 0; i < afterLength; i++) {
                nativeOnKey(KeyEvent.KEYCODE_FORWARD_DEL);
            }
            return true;
        }

        @Override
        public boolean deleteSurroundingTextInCodePoints(int beforeLength, int afterLength) {
            return deleteSurroundingText(beforeLength, afterLength);
        }

        @Override
        public boolean performEditorAction(int actionCode) {
            composing = "";
            nativeOnKey(KeyEvent.KEYCODE_ENTER);
            return true;
        }
    }
}
