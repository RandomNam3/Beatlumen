// HEIC pictures decoded by the operating system. The core gives each coded
// image (a whole picture or one tile of a grid, usually 512x512) as a small
// HEIC file of its own; the decoder fills the buffer the core allocates with
// straight RGBA rows. It is called from the core's worker threads, several
// at a time, and never touches the GUI thread.
#include "heic_decode.h"
#include "core.h"

#include <QtGlobal>

#include <cstdint>
#include <cstring>

#if defined(Q_OS_LINUX) && !defined(Q_OS_ANDROID)

// ---- Linux: the system's libheif ------------------------------------------------
// Loaded at run time and never shipped: distributions build it with their
// HEVC decoder (libde265) where they may. Without it HEIC is left out.

#include <QLibrary>
#include <QString>

#include <mutex>
#include <type_traits>

namespace {

// The declarations it needs, as in libheif 1.x's heif.h. Enums are passed
// as int, the error struct by value.
struct HeifError {
    int code; // heif_error_Ok = 0
    int subcode;
    const char *message;
};
struct HeifContext;
struct HeifImageHandle;
struct HeifImage;

constexpr int kCompressionHevc = 1; // heif_compression_HEVC
constexpr int kColorspaceRgb = 1; // heif_colorspace_RGB
constexpr int kChromaRgba = 11; // heif_chroma_interleaved_RGBA (always 8 bits)
constexpr int kChannelInterleaved = 10; // heif_channel_interleaved

struct Heif {
    HeifContext *(*context_alloc)();
    void (*context_free)(HeifContext *);
    HeifError (*read_from_memory_without_copy)(HeifContext *, const void *, size_t, const void *);
    HeifError (*get_primary_image_handle)(HeifContext *, HeifImageHandle **);
    void (*image_handle_release)(const HeifImageHandle *);
    HeifError (*decode_image)(const HeifImageHandle *, HeifImage **, int, int, const void *);
    const uint8_t *(*get_plane_readonly)(const HeifImage *, int, int *);
    int (*get_width)(const HeifImage *, int);
    int (*get_height)(const HeifImage *, int);
    void (*image_release)(const HeifImage *);
};

const Heif *heif()
{
    static std::once_flag once;
    static const Heif *loaded = nullptr;
    std::call_once(once, []() {
        // libheif.so.1: every libheif 1.x. Never unloaded, not even by Qt's
        // clean-up at exit, while worker threads may still decode.
        static QLibrary lib(QStringLiteral("heif"), 1);
        lib.setLoadHints(QLibrary::PreventUnloadHint);
        if (!lib.load())
            return;
        static Heif f;
        bool ok = true;
        auto load = [&](auto &fn, const char *name) {
            fn = reinterpret_cast<std::remove_reference_t<decltype(fn)>>(lib.resolve(name));
            ok = ok && fn;
        };
        load(f.context_alloc, "heif_context_alloc");
        load(f.context_free, "heif_context_free");
        load(f.read_from_memory_without_copy, "heif_context_read_from_memory_without_copy");
        load(f.get_primary_image_handle, "heif_context_get_primary_image_handle");
        load(f.image_handle_release, "heif_image_handle_release");
        load(f.decode_image, "heif_decode_image");
        load(f.get_plane_readonly, "heif_image_get_plane_readonly");
        load(f.get_width, "heif_image_get_width");
        load(f.get_height, "heif_image_get_height");
        load(f.image_release, "heif_image_release");
        if (!ok)
            return;
        // Since 1.13 the decoders may be plugins, which heif_init loads
        // (older versions have them built in and no heif_init). It is
        // reference counted; the matching heif_deinit is never called.
        using Init = HeifError (*)(void *);
        if (auto init = reinterpret_cast<Init>(lib.resolve("heif_init"))) {
            if (init(nullptr).code != 0)
                return;
        }
        // Some distributions build libheif without an HEVC decoder.
        using HaveDecoder = int (*)(int);
        if (auto have = reinterpret_cast<HaveDecoder>(lib.resolve("heif_have_decoder_for_format"))) {
            if (!have(kCompressionHevc))
                return;
        }
        loaded = &f;
    });
    return loaded;
}

// libheif decodes at full size only; the core scales the result.
int decodeHeic(const uint8_t *data, size_t len, uint32_t, uint32_t, void *ctx, bl_image_alloc_fn alloc)
{
    const Heif *h = heif();
    if (!h || !data || !len)
        return 0;
    HeifContext *c = h->context_alloc();
    if (!c)
        return 0;
    HeifImageHandle *handle = nullptr;
    HeifImage *img = nullptr;
    int ok = 0;
    if (h->read_from_memory_without_copy(c, data, len, nullptr).code == 0
        && h->get_primary_image_handle(c, &handle).code == 0 && handle
        && h->decode_image(handle, &img, kColorspaceRgb, kChromaRgba, nullptr).code == 0 && img) {
        int stride = 0;
        const uint8_t *src = h->get_plane_readonly(img, kChannelInterleaved, &stride);
        const int w = h->get_width(img, kChannelInterleaved);
        const int ht = h->get_height(img, kChannelInterleaved);
        const size_t row = size_t(w > 0 ? w : 0) * 4;
        if (src && w > 0 && ht > 0 && stride > 0 && size_t(stride) >= row) {
            // libheif owns its image (rows may be padded): one copy into the
            // core's buffer.
            if (uint8_t *dst = alloc(ctx, uint32_t(w), uint32_t(ht))) {
                for (int y = 0; y < ht; ++y)
                    std::memcpy(dst + size_t(y) * row, src + size_t(y) * size_t(stride), row);
                ok = 1;
            }
        }
    }
    if (img)
        h->image_release(img);
    if (handle)
        h->image_handle_release(handle);
    h->context_free(c);
    return ok;
}

} // namespace

void registerHeicDecoder()
{
    if (heif())
        bl_set_heic_decoder(decodeHeic);
}

#elif defined(Q_OS_WIN)

// ---- Windows: Windows Imaging Component ------------------------------------------------
// Decodes HEIC when the HEIF Image Extensions and HEVC Video Extensions
// (Microsoft Store) are installed; without them creating the decoder fails
// and the picture is left out.

#include <windows.h>
#include <wincodec.h>
#include <wrl/client.h>

using Microsoft::WRL::ComPtr;

namespace {

int copyFrame(IWICImagingFactory *factory, IWICBitmapFrameDecode *frame, uint32_t needW, uint32_t needH, void *ctx,
              bl_image_alloc_fn alloc)
{
    UINT w = 0, h = 0;
    if (FAILED(frame->GetSize(&w, &h)) || w == 0 || h == 0)
        return 0;
    ComPtr<IWICBitmapSource> source = frame;
    // At least twice the needed size on both sides: scaled while it is read,
    // to the smallest size of the same aspect that still covers the need
    // (one full-size buffer less). If the scaler refuses, full size.
    if (needW > 0 && needH > 0 && uint64_t(w) >= 2 * uint64_t(needW) && uint64_t(h) >= 2 * uint64_t(needH)) {
        UINT tw, th;
        if (uint64_t(w) * needH >= uint64_t(h) * needW) {
            th = needH;
            tw = UINT((uint64_t(w) * needH + h - 1) / h);
        } else {
            tw = needW;
            th = UINT((uint64_t(h) * needW + w - 1) / w);
        }
        ComPtr<IWICBitmapScaler> scaler;
        if (SUCCEEDED(factory->CreateBitmapScaler(&scaler))
            && SUCCEEDED(scaler->Initialize(source.Get(), tw, th, WICBitmapInterpolationModeFant))) {
            source = scaler;
            w = tw;
            h = th;
        }
    }
    // Straight (not premultiplied) RGBA, the core's layout.
    ComPtr<IWICFormatConverter> converter;
    if (FAILED(factory->CreateFormatConverter(&converter))
        || FAILED(converter->Initialize(source.Get(), GUID_WICPixelFormat32bppRGBA, WICBitmapDitherTypeNone, nullptr, 0.0,
                                        WICBitmapPaletteTypeCustom)))
        return 0;
    const uint64_t bytes = uint64_t(w) * h * 4;
    if (bytes > 0xFFFFFFFFu)
        return 0;
    uint8_t *dst = alloc(ctx, w, h);
    if (!dst)
        return 0;
    return SUCCEEDED(converter->CopyPixels(nullptr, w * 4, UINT(bytes), dst)) ? 1 : 0;
}

int decodeHeic(const uint8_t *data, size_t len, uint32_t needW, uint32_t needH, void *ctx, bl_image_alloc_fn alloc)
{
    if (!data || !len || len > 0xFFFFFFFFu)
        return 0;
    // The core's worker threads are in no apartment; WIC works in either
    // kind, so a thread already in one (RPC_E_CHANGED_MODE) keeps it.
    const HRESULT init = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    int ok = 0;
    {
        ComPtr<IWICImagingFactory> factory;
        ComPtr<IWICStream> stream;
        ComPtr<IWICBitmapDecoder> decoder;
        ComPtr<IWICBitmapFrameDecode> frame;
        if (SUCCEEDED(CoCreateInstance(CLSID_WICImagingFactory, nullptr, CLSCTX_INPROC_SERVER, IID_PPV_ARGS(&factory)))
            && SUCCEEDED(factory->CreateStream(&stream))
            // Read in place; the stream does not write to it.
            && SUCCEEDED(stream->InitializeFromMemory(const_cast<BYTE *>(data), DWORD(len)))
            && SUCCEEDED(factory->CreateDecoderFromStream(stream.Get(), nullptr, WICDecodeMetadataCacheOnDemand, &decoder))
            && SUCCEEDED(decoder->GetFrame(0, &frame)))
            ok = copyFrame(factory.Get(), frame.Get(), needW, needH, ctx, alloc);
    } // all COM objects released before CoUninitialize
    if (SUCCEEDED(init)) // S_OK or S_FALSE
        CoUninitialize();
    return ok;
}

} // namespace

void registerHeicDecoder()
{
    bl_set_heic_decoder(decodeHeic);
}

#elif defined(Q_OS_ANDROID)

// ---- Android 9 and newer: ImageDecoder (HeicDecoder.java) ---------------------------------

#include <QCoreApplication>
#include <QJniEnvironment>
#include <QJniObject>

namespace {

constexpr const char *kClass = "io/github/randomnam3/beatlumen/HeicDecoder";

int decodeHeic(const uint8_t *data, size_t len, uint32_t needW, uint32_t needH, void *ctx, bl_image_alloc_fn alloc)
{
    if (!data || !len || len > 0x7FFFFFFF || needW > 0x7FFFFFFF || needH > 0x7FFFFFFF)
        return 0;
    // Attaches the worker thread to the VM (Qt detaches it when it ends).
    QJniEnvironment env;
    if (!env.isValid())
        return 0;
    jbyteArray bytes = env->NewByteArray(jsize(len));
    if (!bytes) {
        env.checkAndClearExceptions();
        return 0;
    }
    env->SetByteArrayRegion(bytes, 0, jsize(len), reinterpret_cast<const jbyte *>(data));
    const QJniObject bitmap = QJniObject::callStaticObjectMethod(kClass, "decode", "([BII)Landroid/graphics/Bitmap;", bytes,
                                                                 jint(needW), jint(needH));
    env->DeleteLocalRef(bytes);
    if (env.checkAndClearExceptions() || !bitmap.isValid())
        return 0;
    const jint w = bitmap.callMethod<jint>("getWidth", "()I");
    const jint h = bitmap.callMethod<jint>("getHeight", "()I");
    int ok = 0;
    if (!env.checkAndClearExceptions() && w > 0 && h > 0) {
        if (uint8_t *dst = alloc(ctx, uint32_t(w), uint32_t(h))) {
            // Java copies the pixels straight into the core's buffer (no
            // full-size Java array: those count against the Java heap).
            if (jobject buffer = env->NewDirectByteBuffer(dst, jlong(w) * jlong(h) * 4)) {
                const jboolean copied = QJniObject::callStaticMethod<jboolean>(
                    kClass, "copyPixels", "(Landroid/graphics/Bitmap;Ljava/nio/ByteBuffer;)Z", bitmap.object(), buffer);
                env->DeleteLocalRef(buffer);
                ok = copied ? 1 : 0;
            }
            if (env.checkAndClearExceptions())
                ok = 0;
        }
    }
    // The pixels are native memory that only the garbage collector would
    // free otherwise.
    bitmap.callMethod<void>("recycle", "()V");
    env.checkAndClearExceptions();
    return ok;
}

} // namespace

void registerHeicDecoder()
{
    // ImageDecoder (and its HEIF support) came with Android 9.
    if (QNativeInterface::QAndroidApplication::sdkVersion() >= 28)
        bl_set_heic_decoder(decodeHeic);
}

#else

// The browser, iOS and others: no system decoder is used.
void registerHeicDecoder() {}

#endif
