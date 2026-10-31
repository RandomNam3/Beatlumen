// C interface of the Beatlumen core (Rust, crates/bl-ffi).
#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct BlCore BlCore;

typedef struct BlCallbacks {
    void *user;
    uint32_t (*audio_start)(void *user);
    void (*log)(void *user, const char *msg);
    void (*http_request)(void *user, uint64_t id, const char *method, const char *url, const char *headers, const uint8_t *body, size_t len);
    void (*audio_push)(void *user, const float *samples, size_t count);
    uint32_t (*audio_queued)(void *user);
    void (*audio_clear)(void *user);
    void (*audio_set_paused)(void *user, int32_t paused);
    void (*audio_set_gain)(void *user, float gain);
    int64_t (*tls_open)(const char *host, uint16_t port, uint32_t timeout_ms, const char *pin, char *err, size_t err_len);
    // Bytes read, 0 when closed, -2 when nothing arrived within timeout_ms
    // (the connection stays usable), -1 on errors.
    int64_t (*tls_read)(int64_t handle, uint8_t *buf, size_t len, uint32_t timeout_ms);
    int64_t (*tls_write)(int64_t handle, const uint8_t *buf, size_t len, uint32_t timeout_ms);
    void (*tls_close)(int64_t handle);
    // Announces a network service through the platform (Android NsdManager);
    // txt is a JSON array of "key=value" strings. 0 = not available.
    int32_t (*service_register)(void *user, const char *instance, const char *service_type, uint16_t port, const char *txt);
    void (*service_unregister)(void *user);
    // UnifiedPush: 1 registration started, -1 no distributor, 0 not
    // available; the endpoint comes back as the "push.endpoint" command.
    int32_t (*push_register)(void *user, const char *instance, const char *vapid);
    void (*push_unregister)(void *user, const char *instance);
    // Opens a web page in the system browser; non-zero on success.
    int32_t (*open_url)(void *user, const char *url);
    // The platform's credential manager (Android): kind "get", "create" or
    // "save" with JSON; results come back as "vault.credentialResult".
    // Non-zero when the request was started.
    int32_t (*credential_request)(void *user, const char *kind, const char *json);
    // The platform's protected store (Android Keystore, Windows DPAPI,
    // Secret Service): keeps len bytes under key (0 bytes: deletes); load
    // answers with the "vault.secret" command, non-zero when started.
    void (*secret_store)(void *user, const char *key, const uint8_t *data, size_t len);
    int32_t (*secret_load)(void *user, const char *key);
    // Other apps' playback (Android): kind "start", "stop", "access" or
    // "control" with JSON; the state comes back as "capture.state", the
    // other app's media session as "capture.session". Null where the
    // platform cannot record other apps.
    int32_t (*capture_request)(void *user, const char *kind, const char *json);
    // Microphones: kind "list" (the inputs come back as "mic.devices"
    // {devices: [{id, name, channels, rate, default}]}), "open" {stream,
    // device} (records it into bl_mic_push; the state comes back as
    // "mic.state" {stream, running, error}), "close" {stream}. Non-zero when
    // taken; null where the platform has no microphones.
    int32_t (*mic_request)(void *user, const char *kind, const char *json);
} BlCallbacks;

BlCore *bl_core_new(const char *data_dir, const char *args_json, const char *language, const char *platform, BlCallbacks cb);
void bl_core_free(BlCore *c);
char *bl_core_command(BlCore *c, const char *json);
char *bl_core_tick(BlCore *c);
double bl_core_position(BlCore *c);
int32_t bl_core_is_playing(BlCore *c);
int32_t bl_core_gl_init(BlCore *c, const void *(*get_proc)(void *ctx, const char *name), void *ctx, int32_t flavor);
void bl_core_gl_lost(BlCore *c);
void bl_core_render(BlCore *c, uint32_t fbo, uint32_t w, uint32_t h, float dpr, double dt);
void bl_core_http_complete(BlCore *c, uint64_t id, int32_t status, const char *headers_json, const uint8_t *body, size_t len, const char *error);
void bl_core_open_data(BlCore *c, const char *name, const uint8_t *data, size_t len);
void bl_core_save(BlCore *c);
void bl_audio_render(float *out, size_t frames, uint32_t latency_frames);
// Recorded audio of other apps (interleaved 16-bit PCM); from any thread.
void bl_capture_push(const int16_t *pcm, size_t frames, uint32_t channels, uint32_t rate);
// Recorded audio of microphone stream `stream` (interleaved 16-bit PCM);
// capture_time_us: when the first frame was recorded, on bl_clock_us()
// (0: unknown, now). From any thread.
void bl_mic_push(uint32_t stream, const int16_t *pcm, size_t frames, uint32_t channels, uint32_t rate, int64_t capture_time_us);
// The core's monotonic clock in microseconds (never 0 or less).
int64_t bl_clock_us(void);
void bl_string_free(char *s);
// Fetches a URL with the core's HTTP client (TLS included), from any
// thread; returns the body or null. Free with bl_bytes_free.
uint8_t *bl_http_fetch(const char *url, size_t *out_len);
void bl_bytes_free(uint8_t *p, size_t len);

#ifdef __cplusplus
}
#endif
