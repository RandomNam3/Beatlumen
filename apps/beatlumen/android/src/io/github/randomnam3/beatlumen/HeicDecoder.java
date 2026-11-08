// HEIC pictures on Android 9 and newer. HEVC is patent-encumbered, so the
// core has no decoder of its own: it hands each coded image of a HEIF (a
// whole picture or one tile of a grid) as a small HEIC file to
// heic_decode.cpp, which calls decode() and copyPixels() from the core's
// worker threads, several at a time.
//
// SPDX-License-Identifier: AGPL-3.0-or-later

package io.github.randomnam3.beatlumen;

import android.annotation.TargetApi;
import android.graphics.Bitmap;
import android.graphics.ColorSpace;
import android.graphics.ImageDecoder;
import android.os.Build;
import android.util.Log;
import android.util.Size;

import java.nio.ByteBuffer;

public final class HeicDecoder
{
    private HeicDecoder() {}

    private static final String TAG = "Beatlumen";

    /**
     * Decodes one HEIC picture into a software bitmap (ARGB_8888, straight
     * alpha, sRGB), reduced when it is at least twice the needed size on both
     * sides. Null when it cannot be decoded (or before Android 9).
     */
    public static Bitmap decode(byte[] data, int needW, int needH)
    {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.P || data == null)
            return null;
        try {
            return decodeP(data, needW, needH);
        } catch (Throwable t) {
            // Includes OutOfMemoryError: the picture is left out, the app goes on.
            Log.w(TAG, "HEIC not decoded: " + t);
            return null;
        }
    }

    @TargetApi(Build.VERSION_CODES.P)
    private static Bitmap decodeP(byte[] data, int needW, int needH) throws Exception
    {
        ImageDecoder.Source source = ImageDecoder.createSource(ByteBuffer.wrap(data));
        Bitmap bmp = ImageDecoder.decodeBitmap(source, (decoder, info, src) -> {
            // Pixels the native side copies out: no hardware bitmap, and not
            // premultiplied, as the core expects.
            decoder.setAllocator(ImageDecoder.ALLOCATOR_SOFTWARE);
            decoder.setUnpremultipliedRequired(true);
            // The core works in sRGB; wide-gamut photos (Display P3) are converted.
            decoder.setTargetColorSpace(ColorSpace.get(ColorSpace.Named.SRGB));
            Size size = info.getSize();
            int w = size.getWidth(), h = size.getHeight();
            if (needW > 0 && needH > 0 && w >= 2L * needW && h >= 2L * needH) {
                // The smallest size of the same aspect that still covers the need.
                if ((long) w * needH >= (long) h * needW)
                    decoder.setTargetSize((int) (((long) w * needH + h - 1) / h), needH);
                else
                    decoder.setTargetSize(needW, (int) (((long) h * needW + w - 1) / w));
            }
        });
        if (bmp.getConfig() != Bitmap.Config.ARGB_8888) {
            // Pictures of more than 8 bits come as RGBA_F16.
            Bitmap copy = bmp.copy(Bitmap.Config.ARGB_8888, false);
            bmp.recycle();
            bmp = copy;
        }
        return bmp;
    }

    /**
     * Copies the pixels of a bitmap from decode() into `dst` (direct, width x
     * height x 4 bytes): ARGB_8888 is R, G, B, A in memory, rows without gaps
     * in software bitmaps.
     */
    public static boolean copyPixels(Bitmap bmp, ByteBuffer dst)
    {
        try {
            if (bmp.getConfig() != Bitmap.Config.ARGB_8888 || bmp.getRowBytes() != bmp.getWidth() * 4
                    || dst.capacity() != bmp.getByteCount())
                return false;
            bmp.copyPixelsToBuffer(dst);
            return true;
        } catch (Throwable t) {
            Log.w(TAG, "HEIC pixels not copied: " + t);
            return false;
        }
    }
}
