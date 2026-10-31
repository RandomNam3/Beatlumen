// Microphones (karaoke): lists the device's audio inputs and records the
// ones the user opens with AudioRecord, each on its own thread, into the
// core (src/android_mic.cpp). Recording only runs while an input is open;
// the RECORD_AUDIO permission is asked for when the first one is opened.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.content.Context;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioRecord;
import android.media.AudioTimestamp;
import android.media.MediaRecorder;
import android.os.Build;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.nio.ByteBuffer;
import java.util.HashMap;
import java.util.Map;

public final class MicRecorder
{
    private static final String TAG = "Beatlumen";
    /** The device's own choice of microphone. */
    private static final String DEFAULT_ID = "default";

    static native void nativeMicState(int stream, boolean running, String error);
    static native void nativeMicPcm(int stream, ByteBuffer pcm, int bytes, int channels, int rate, long ageNs);

    private static volatile boolean sNativeReady;
    private static final Map<Integer, MicRecorder> sOpen = new HashMap<>();

    static void setNativeReady(boolean ready)
    {
        sNativeReady = ready;
    }

    private static boolean isInput(int type)
    {
        switch (type) {
        case AudioDeviceInfo.TYPE_BUILTIN_MIC:
        case AudioDeviceInfo.TYPE_WIRED_HEADSET:
        case AudioDeviceInfo.TYPE_USB_DEVICE:
        case AudioDeviceInfo.TYPE_USB_HEADSET:
        case AudioDeviceInfo.TYPE_USB_ACCESSORY:
        case AudioDeviceInfo.TYPE_LINE_ANALOG:
        case AudioDeviceInfo.TYPE_LINE_DIGITAL:
            return true;
        default:
            return false;
        }
    }

    private static String typeName(int type)
    {
        switch (type) {
        case AudioDeviceInfo.TYPE_BUILTIN_MIC: return "Built-in microphone";
        case AudioDeviceInfo.TYPE_WIRED_HEADSET: return "Headset";
        case AudioDeviceInfo.TYPE_USB_DEVICE:
        case AudioDeviceInfo.TYPE_USB_HEADSET:
        case AudioDeviceInfo.TYPE_USB_ACCESSORY: return "USB";
        default: return "Line in";
        }
    }

    /** The inputs as JSON: [{id, name, channels, rate, default}]. */
    static String list(Context context)
    {
        JSONArray out = new JSONArray();
        try {
            out.put(new JSONObject()
                    .put("id", DEFAULT_ID)
                    .put("name", "Microphone")
                    .put("channels", 1)
                    .put("rate", 48000)
                    .put("default", true));
            AudioManager am = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
            for (AudioDeviceInfo d : am.getDevices(AudioManager.GET_DEVICES_INPUTS)) {
                if (!isInput(d.getType()))
                    continue;
                CharSequence product = d.getProductName();
                String name = typeName(d.getType());
                if (d.getType() != AudioDeviceInfo.TYPE_BUILTIN_MIC && product != null && product.length() > 0)
                    name += " (" + product + ")";
                String address = d.getAddress();
                if (d.getType() == AudioDeviceInfo.TYPE_BUILTIN_MIC && address != null && !address.isEmpty())
                    name += " " + address;
                out.put(new JSONObject()
                        .put("id", Integer.toString(d.getId()))
                        .put("name", name)
                        .put("channels", 1)
                        .put("rate", 48000)
                        .put("default", false));
            }
        } catch (JSONException | RuntimeException e) {
            Log.w(TAG, "mic: cannot list the inputs", e);
        }
        return out.toString();
    }

    /** Starts recording `device` as stream `stream` (permission granted). */
    static void open(Context context, int stream, String device)
    {
        close(stream);
        MicRecorder r = new MicRecorder(stream);
        String error = r.start(context, device);
        if (error != null) {
            report(stream, false, error);
            return;
        }
        synchronized (sOpen) {
            sOpen.put(stream, r);
        }
    }

    static void close(int stream)
    {
        MicRecorder r;
        synchronized (sOpen) {
            r = sOpen.remove(stream);
        }
        if (r != null)
            r.stop();
    }

    private static void report(int stream, boolean running, String error)
    {
        if (sNativeReady)
            nativeMicState(stream, running, error);
    }

    // ---- one opened input ------------------------------------------------------------

    private final int mStream;
    private AudioRecord mRecord;
    private Thread mThread;
    private volatile boolean mRunning;

    private MicRecorder(int stream)
    {
        mStream = stream;
    }

    /** Null on success, else what went wrong. */
    private String start(Context context, String device)
    {
        AudioManager am = (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
        // Unprocessed where the device offers it (no gain control or noise
        // suppression that would bend the pitch), else the voice recognition
        // source, which is tuned for little processing too.
        int source = MediaRecorder.AudioSource.VOICE_RECOGNITION;
        if ("true".equals(am.getProperty(AudioManager.PROPERTY_SUPPORT_AUDIO_SOURCE_UNPROCESSED)))
            source = MediaRecorder.AudioSource.UNPROCESSED;
        AudioDeviceInfo preferred = null;
        if (!DEFAULT_ID.equals(device)) {
            for (AudioDeviceInfo d : am.getDevices(AudioManager.GET_DEVICES_INPUTS)) {
                if (Integer.toString(d.getId()).equals(device))
                    preferred = d;
            }
            if (preferred == null)
                return "the microphone is not connected";
        }
        AudioRecord record = null;
        int rate = 0;
        for (int r : new int[] { 48000, 44100, 16000 }) {
            int min = AudioRecord.getMinBufferSize(r, AudioFormat.CHANNEL_IN_MONO, AudioFormat.ENCODING_PCM_16BIT);
            if (min <= 0)
                continue;
            try {
                AudioRecord a = new AudioRecord.Builder()
                        .setAudioSource(source)
                        .setAudioFormat(new AudioFormat.Builder()
                                .setEncoding(AudioFormat.ENCODING_PCM_16BIT)
                                .setSampleRate(r)
                                .setChannelMask(AudioFormat.CHANNEL_IN_MONO)
                                .build())
                        .setBufferSizeInBytes(Math.max(min * 2, r / 10 * 2))
                        .build();
                if (a.getState() == AudioRecord.STATE_INITIALIZED) {
                    record = a;
                    rate = r;
                    break;
                }
                a.release();
            } catch (UnsupportedOperationException | SecurityException e) {
                Log.w(TAG, "mic: " + r + " Hz not available", e);
            }
        }
        if (record == null)
            return "the microphone cannot be recorded";
        if (preferred != null)
            record.setPreferredDevice(preferred);
        try {
            record.startRecording();
        } catch (IllegalStateException e) {
            record.release();
            return "the microphone cannot be recorded";
        }
        if (record.getRecordingState() != AudioRecord.RECORDSTATE_RECORDING) {
            record.release();
            return "the microphone is in use";
        }
        mRecord = record;
        mRunning = true;
        final AudioRecord rec = record;
        final int sampleRate = rate;
        report(mStream, true, null);
        mThread = new Thread(() -> {
            // 10 ms per read: singing is scored against the song closely.
            ByteBuffer buf = ByteBuffer.allocateDirect(sampleRate / 100 * 2);
            AudioTimestamp ts = new AudioTimestamp();
            long frames = 0;
            String error = null;
            while (mRunning) {
                buf.clear();
                int n = rec.read(buf, buf.capacity(), AudioRecord.READ_BLOCKING);
                if (n < 0) {
                    error = "recording stopped (" + n + ")";
                    break;
                }
                if (n == 0)
                    continue;
                // How long ago the first of these frames was recorded, from
                // the recorder's timestamp (-1: unknown).
                long age = -1;
                if (rec.getTimestamp(ts, AudioTimestamp.TIMEBASE_MONOTONIC) == AudioRecord.SUCCESS) {
                    long at = ts.nanoTime + (frames - ts.framePosition) * 1000000000L / sampleRate;
                    long a = System.nanoTime() - at;
                    if (a >= 0 && a < 2000000000L)
                        age = a;
                }
                frames += n / 2;
                if (sNativeReady)
                    nativeMicPcm(mStream, buf, n, 1, sampleRate, age);
            }
            if (mRunning) {
                mRunning = false;
                synchronized (sOpen) {
                    sOpen.remove(mStream);
                }
                rec.stop();
                rec.release();
                report(mStream, false, error);
            }
        }, "bl-mic-" + mStream);
        mThread.start();
        return null;
    }

    private void stop()
    {
        if (!mRunning)
            return;
        mRunning = false;
        if (mThread != null) {
            try {
                mThread.join(500);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            mThread = null;
        }
        if (mRecord != null) {
            mRecord.stop();
            mRecord.release();
            mRecord = null;
        }
    }
}
