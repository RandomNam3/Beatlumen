// Bit-perfect playback on Android 14 and newer: a USB sound card whose preferred
// mixer is set to MIXER_BEHAVIOR_BIT_PERFECT, and one AudioTrack in exactly that
// format, filled from the core on a thread of its own (src/audio_direct_android.cpp).
// What the system refuses is reported and nothing goes elsewhere: a track that
// ends up on another device stops.
//
// Android has no public way to ask whether a track really is on that mixer. What
// can be seen is checked: the system took the preference and still has it, the
// track has the mixer's format, and it plays on this device.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.annotation.TargetApi;
import android.content.Context;
import android.media.AudioAttributes;
import android.media.AudioDeviceInfo;
import android.media.AudioFormat;
import android.media.AudioManager;
import android.media.AudioMixerAttributes;
import android.media.AudioTimestamp;
import android.media.AudioTrack;
import android.os.Build;
import android.os.Process;
import android.os.SystemClock;
import android.util.Log;

import org.json.JSONArray;
import org.json.JSONException;
import org.json.JSONObject;

import java.nio.ByteBuffer;
import java.util.ArrayList;
import java.util.Arrays;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

@TargetApi(Build.VERSION_CODES.UPSIDE_DOWN_CAKE)
public final class DirectOutput
{
    private static final String TAG = "Beatlumen";

    // Texts the core knows and translates (strings.rs).
    private static final String NOT_AVAILABLE = "The sound card is not available";
    private static final String IN_USE = "The sound card is in use by another program";
    private static final String NOT_TAKEN = "The sound card does not take this music";
    private static final String ASKED_FOR = "Another program asked for the sound card";

    // The core's PcmFormat. Its fourth format (24 bits in the low bytes of four)
    // has no encoding in AudioFormat.
    private static final int S16 = 0;
    private static final int S24_PACKED = 1;
    private static final int S32 = 3;

    private static final int PERIOD_MS = 20;
    // Under 100 ms: from there on AudioTrack.Builder asks for a deep buffer by
    // itself, and the track should ask for nothing but the mixer's format.
    private static final int BUFFER_MS = 80;
    private static final int ROUTE_WAIT_MS = 1500;
    private static final int ROUTE_CHECK_MS = 250;
    private static final int PLAY_OUT_MAX_MS = 1000;

    static native void nativeRender(ByteBuffer out, int frames, int format, int channels, int latency);
    static native void nativeFailed(String error);

    private static Context sContext;
    private static String sDevice;
    private static int sRate;
    private static int[] sFormats;
    private static boolean sIdle;
    private static String sOpened;
    private static Stream sStream;

    private DirectOutput()
    {
    }

    // ---- the cards ------------------------------------------------------------

    private static AudioManager audio(Context context)
    {
        return (AudioManager) context.getSystemService(Context.AUDIO_SERVICE);
    }

    private static boolean isUsb(int type)
    {
        return type == AudioDeviceInfo.TYPE_USB_DEVICE
                || type == AudioDeviceInfo.TYPE_USB_HEADSET
                || type == AudioDeviceInfo.TYPE_USB_ACCESSORY;
    }

    private static String name(AudioDeviceInfo device)
    {
        CharSequence product = device.getProductName();
        return product != null && product.length() > 0 ? product.toString() : "USB";
    }

    /**
     * The USB outputs by an id that outlives pulling the plug (the system's own
     * changes every time): the product's name, numbered when there are several.
     */
    private static Map<String, AudioDeviceInfo> usbDevices(AudioManager am)
    {
        AudioDeviceInfo[] devices = am.getDevices(AudioManager.GET_DEVICES_OUTPUTS);
        Arrays.sort(devices, Comparator.comparingInt(AudioDeviceInfo::getId));
        Map<String, AudioDeviceInfo> out = new LinkedHashMap<>();
        for (AudioDeviceInfo d : devices) {
            if (!isUsb(d.getType()))
                continue;
            String id = "usb:" + name(d);
            String free = id;
            for (int n = 2; out.containsKey(free); ++n)
                free = id + "#" + n;
            out.put(free, d);
        }
        return out;
    }

    private static List<AudioMixerAttributes> bitPerfect(AudioManager am, AudioDeviceInfo device)
    {
        List<AudioMixerAttributes> out = new ArrayList<>();
        for (AudioMixerAttributes m : am.getSupportedMixerAttributes(device)) {
            if (m.getMixerBehavior() == AudioMixerAttributes.MIXER_BEHAVIOR_BIT_PERFECT)
                out.add(m);
        }
        return out;
    }

    /** The USB sound cards with a bit-perfect mode as JSON: [{id, name}]. Opens none. */
    static String list(Context context)
    {
        JSONArray out = new JSONArray();
        AudioManager am = audio(context);
        try {
            for (Map.Entry<String, AudioDeviceInfo> e : usbDevices(am).entrySet()) {
                if (!bitPerfect(am, e.getValue()).isEmpty())
                    out.put(new JSONObject().put("id", e.getKey()).put("name", name(e.getValue())));
            }
        } catch (JSONException e) {
            Log.w(TAG, "direct output: " + e);
        }
        return out.toString();
    }

    // ---- what a card takes ------------------------------------------------------

    private static int pcmFormat(int encoding)
    {
        switch (encoding) {
        case AudioFormat.ENCODING_PCM_16BIT: return S16;
        case AudioFormat.ENCODING_PCM_24BIT_PACKED: return S24_PACKED;
        case AudioFormat.ENCODING_PCM_32BIT: return S32;
        default: return -1;
        }
    }

    private static int bytesOf(int format)
    {
        return format == S16 ? 2 : format == S24_PACKED ? 3 : 4;
    }

    private static int rank(int[] formats, int format)
    {
        for (int i = 0; i < formats.length; ++i) {
            if (formats[i] == format)
                return i;
        }
        return -1;
    }

    /**
     * The best of the card's bit-perfect mixers: nearest to the rate, then the
     * first of the formats, then stereo rather than all of its channels (the
     * music is on the first two).
     */
    static AudioMixerAttributes choose(List<AudioMixerAttributes> mixers, int rate, int[] formats)
    {
        AudioMixerAttributes best = null;
        long bestScore = Long.MAX_VALUE;
        for (AudioMixerAttributes m : mixers) {
            AudioFormat f = m.getFormat();
            int format = pcmFormat(f.getEncoding());
            int place = format < 0 ? -1 : rank(formats, format);
            int channels = f.getChannelCount();
            if (place < 0 || channels < 2 || f.getSampleRate() <= 0)
                continue;
            long score = Math.abs((long) f.getSampleRate() - rate) * 100000 + place * 1000L + (channels == 2 ? 0 : channels);
            if (score < bestScore) {
                bestScore = score;
                best = m;
            }
        }
        return best;
    }

    private static boolean same(AudioMixerAttributes a, AudioMixerAttributes b)
    {
        if (a == null || b == null || a.getMixerBehavior() != b.getMixerBehavior())
            return false;
        AudioFormat x = a.getFormat();
        AudioFormat y = b.getFormat();
        return x.getEncoding() == y.getEncoding()
                && x.getSampleRate() == y.getSampleRate()
                && x.getChannelMask() == y.getChannelMask()
                && x.getChannelIndexMask() == y.getChannelIndexMask();
    }

    // ---- from the core ------------------------------------------------------------

    /** Returns {rate, format, channels} as JSON, or {error}. */
    static synchronized String open(Context context, String device, int rate, int[] formats)
    {
        if (sStream != null && !sStream.failed() && !sIdle && device.equals(sDevice) && rate == sRate
                && Arrays.equals(formats, sFormats))
            return sOpened;
        finish(true);
        sContext = context.getApplicationContext();
        sDevice = device;
        sRate = rate;
        sFormats = formats;
        sIdle = false;
        String error = start();
        if (error == null)
            return sOpened;
        sDevice = null;
        try {
            return new JSONObject().put("error", error).toString();
        } catch (JSONException e) {
            return "{}";
        }
    }

    static synchronized void close()
    {
        finish(false);
        sDevice = null;
        sIdle = false;
    }

    /** Gives the card back for now (a long pause) or takes it again. */
    static synchronized void setIdle(boolean idle)
    {
        if (idle == sIdle || sDevice == null)
            return;
        sIdle = idle;
        finish(false);
        // Asking for the card can take a while: not on the caller's thread.
        if (!idle)
            new Thread(DirectOutput::wake, "DirectOutput wake").start();
    }

    /** The core goes on from what it knows: anything else than the same is a failure. */
    private static synchronized void wake()
    {
        if (sIdle || sDevice == null || sStream != null)
            return;
        String before = sOpened;
        String error = start();
        if (error == null && !before.equals(sOpened)) {
            finish(false);
            error = NOT_TAKEN;
        }
        if (error != null)
            nativeFailed(error);
    }

    private static void finish(boolean playOut)
    {
        if (sStream != null) {
            sStream.finish(playOut);
            sStream = null;
        }
    }

    /** Null when it plays, or what the core can show. */
    private static String start()
    {
        AudioManager am = audio(sContext);
        AudioDeviceInfo device = usbDevices(am).get(sDevice);
        if (device == null)
            return NOT_AVAILABLE;
        AudioMixerAttributes mixer = choose(bitPerfect(am, device), sRate, sFormats);
        if (mixer == null)
            return NOT_TAKEN;
        AudioAttributes media = new AudioAttributes.Builder()
                .setUsage(AudioAttributes.USAGE_MEDIA)
                .setContentType(AudioAttributes.CONTENT_TYPE_MUSIC)
                .build();
        if (!am.setPreferredMixerAttributes(media, device, mixer)) {
            Log.w(TAG, "direct output: the system refused the mixer for " + sDevice);
            return am.getPreferredMixerAttributes(media, device) != null ? IN_USE : NOT_TAKEN;
        }
        Stream stream = new Stream(am, media, device, mixer);
        String error = stream.begin();
        if (error != null) {
            am.clearPreferredMixerAttributes(media, device);
            return error;
        }
        sStream = stream;
        try {
            sOpened = new JSONObject()
                    .put("rate", stream.mRate)
                    .put("format", stream.mFormat)
                    .put("channels", stream.mChannels)
                    .toString();
        } catch (JSONException e) {
            finish(false);
            return NOT_AVAILABLE;
        }
        Log.i(TAG, "direct output: " + sDevice + ", " + stream.mRate + " Hz (asked for " + sRate + "), format "
                + stream.mFormat + ", " + stream.mChannels + " channels");
        return null;
    }

    // ---- the output --------------------------------------------------------------

    private static final class Stream implements Runnable
    {
        final AudioManager mAudio;
        final AudioAttributes mMedia;
        final AudioDeviceInfo mDevice;
        final AudioMixerAttributes mMixer;
        final int mRate;
        final int mFormat;
        final int mChannels;
        final int mFrameBytes;
        final int mPeriodFrames;
        final Thread mThread = new Thread(this, "DirectOutput");
        final AudioTimestamp mStamp = new AudioTimestamp();
        AudioTrack mTrack;
        long mWritten;
        long mStampedAt;
        boolean mStamped;
        boolean mReleased;
        volatile boolean mRun = true;
        volatile boolean mRouted;
        volatile boolean mFailed;
        // Somebody changed the mixer under the track.
        volatile String mLost;

        final AudioManager.OnPreferredMixerAttributesChangedListener mListener = this::mixerChanged;

        Stream(AudioManager am, AudioAttributes media, AudioDeviceInfo device, AudioMixerAttributes mixer)
        {
            mAudio = am;
            mMedia = media;
            mDevice = device;
            mMixer = mixer;
            AudioFormat f = mixer.getFormat();
            mRate = f.getSampleRate();
            mFormat = pcmFormat(f.getEncoding());
            mChannels = f.getChannelCount();
            mFrameBytes = bytesOf(mFormat) * mChannels;
            mPeriodFrames = Math.max(mRate * PERIOD_MS / 1000, 1);
        }

        private void mixerChanged(AudioAttributes attributes, AudioDeviceInfo device, AudioMixerAttributes mixer)
        {
            if (device.getId() != mDevice.getId() || attributes.getUsage() != AudioAttributes.USAGE_MEDIA)
                return;
            // What the system has now, rather than this message, which may be
            // about an earlier change.
            AudioMixerAttributes now = mAudio.getPreferredMixerAttributes(mMedia, mDevice);
            if (!same(now, mMixer))
                mLost = now == null ? NOT_AVAILABLE : ASKED_FOR;
        }

        /** Starts it and looks at what came of it. Null when all is as asked for. */
        String begin()
        {
            if (!same(mAudio.getPreferredMixerAttributes(mMedia, mDevice), mMixer))
                return NOT_TAKEN;
            AudioFormat format = mMixer.getFormat();
            try {
                mTrack = new AudioTrack.Builder()
                        .setAudioAttributes(mMedia)
                        .setAudioFormat(format)
                        .setTransferMode(AudioTrack.MODE_STREAM)
                        .setBufferSizeInBytes(mRate * BUFFER_MS / 1000 * mFrameBytes)
                        .build();
            } catch (UnsupportedOperationException | IllegalArgumentException e) {
                Log.w(TAG, "direct output: " + e);
                return NOT_TAKEN;
            }
            if (mTrack.getState() != AudioTrack.STATE_INITIALIZED || !mTrack.setPreferredDevice(mDevice)
                    || mTrack.getSampleRate() != mRate || mTrack.getAudioFormat() != format.getEncoding()) {
                mTrack.release();
                return NOT_TAKEN;
            }
            mAudio.addOnPreferredMixerAttributesChangedListener(Runnable::run, mListener);
            try {
                mTrack.play();
            } catch (IllegalStateException e) {
                Log.w(TAG, "direct output: " + e);
                release();
                return NOT_AVAILABLE;
            }
            mThread.start();
            // Where a track plays can only be told while it plays.
            long until = SystemClock.uptimeMillis() + ROUTE_WAIT_MS;
            while (SystemClock.uptimeMillis() < until && !mFailed) {
                AudioDeviceInfo routed = mTrack.getRoutedDevice();
                if (routed != null && routed.getId() == mDevice.getId()) {
                    mRouted = true;
                    return null;
                }
                sleep(20);
            }
            Log.w(TAG, "direct output: the track did not reach the device");
            finish(false);
            return NOT_AVAILABLE;
        }

        boolean failed()
        {
            return mFailed;
        }

        void finish(boolean playOut)
        {
            mRun = false;
            try {
                mThread.join();
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
            if (playOut && !mFailed) {
                long left = latency() * 1000L / mRate;
                try {
                    mTrack.stop(); // What was written still plays.
                    sleep(Math.min(left, PLAY_OUT_MAX_MS));
                } catch (IllegalStateException e) {
                    Log.w(TAG, "direct output: " + e);
                }
            }
            release();
        }

        private synchronized void release()
        {
            if (mReleased)
                return;
            mReleased = true;
            mAudio.removeOnPreferredMixerAttributesChangedListener(mListener);
            try {
                mTrack.pause();
                mTrack.flush();
            } catch (IllegalStateException e) {
                // Stopped already.
            }
            mTrack.release();
            mAudio.clearPreferredMixerAttributes(mMedia, mDevice);
        }

        private static void sleep(long ms)
        {
            try {
                Thread.sleep(ms);
            } catch (InterruptedException e) {
                Thread.currentThread().interrupt();
            }
        }

        /** Frames written and not heard yet. Asking for a timestamp is not cheap: once a second. */
        private int latency()
        {
            long now = SystemClock.uptimeMillis();
            if (!mStamped || now - mStampedAt >= 1000) {
                mStampedAt = now;
                mStamped = mTrack.getTimestamp(mStamp);
            }
            long queued = mWritten;
            if (mStamped)
                queued -= mStamp.framePosition + (System.nanoTime() - mStamp.nanoTime) * mRate / 1000000000L;
            return (int) Math.max(0, Math.min(queued, mRate * 2L));
        }

        @Override
        public void run()
        {
            Process.setThreadPriority(Process.THREAD_PRIORITY_URGENT_AUDIO);
            final int bytes = mPeriodFrames * mFrameBytes;
            ByteBuffer out = ByteBuffer.allocateDirect(bytes);
            long nextCheck = 0;
            String error = null;
            while (mRun && error == null) {
                nativeRender(out, mPeriodFrames, mFormat, mChannels, latency());
                out.position(0);
                out.limit(bytes);
                // Never blocked for good: a device that hangs must not keep this from stopping.
                while (out.hasRemaining() && mRun && error == null) {
                    int n = mTrack.write(out, out.remaining(), AudioTrack.WRITE_NON_BLOCKING);
                    if (n < 0) {
                        Log.w(TAG, "direct output: write: " + n);
                        error = NOT_AVAILABLE;
                    } else if (out.hasRemaining()) {
                        sleep(PERIOD_MS / 4);
                    }
                }
                mWritten += out.position() / mFrameBytes;
                error = error != null ? error : mLost;
                long now = SystemClock.uptimeMillis();
                if (error == null && mRouted && now >= nextCheck) {
                    nextCheck = now + ROUTE_CHECK_MS;
                    AudioDeviceInfo routed = mTrack.getRoutedDevice();
                    if (routed != null && routed.getId() != mDevice.getId()) {
                        Log.w(TAG, "direct output: the track moved to " + routed.getType());
                        error = NOT_AVAILABLE;
                    }
                }
            }
            if (error != null) {
                mFailed = true;
                release();
                nativeFailed(error);
            }
        }
    }
}
