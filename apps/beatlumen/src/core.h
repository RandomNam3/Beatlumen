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
void bl_string_free(char *s);

#ifdef __cplusplus
}
#endif
