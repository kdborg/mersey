/* Mersey embedding API (docs/architecture/embedding-api.md, v0 subset).
 *
 * Design rules honored from the architecture doc:
 *  - the host owns all I/O and the event loop; the engine only computes;
 *  - strings cross the boundary as (pointer, length) UTF-8;
 *  - errors are reported through the host's `error` callback, never
 *    unwound across the ABI;
 *  - a context is single-threaded and confined to its creating thread.
 */
#ifndef MERSEY_H
#define MERSEY_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct msy_context msy_context;

/* Host function table. Any pointer may be NULL (the capability is then
 * denied / a no-op). `data` is passed back verbatim. */
typedef struct {
    void *data;
    void (*print)(void *data, const char *utf8, size_t len);
    void (*error)(void *data, const char *utf8, size_t len);
    void (*dom_set_text)(void *data, const char *id, size_t id_len,
                         const char *text, size_t text_len);
    /* Return a pointer valid until the next host call; *out_len = length.
     * Return NULL for "no such element". */
    const char *(*dom_get_text)(void *data, const char *id, size_t id_len,
                                size_t *out_len);
    /* Register `cb` as a listener for `event` on element `id`. The engine has
     * no list of which events exist — the host owns the event loop, so the host
     * is what knows. */
    void (*dom_add_listener)(void *data, const char *id, size_t id_len,
                             const char *event, size_t event_len, uint32_t cb);
} msy_host_table;

/* Create a context backed by `host` (table is copied). */
msy_context *msy_context_new(const msy_host_table *host);
void msy_context_free(msy_context *ctx);

/* Compile and execute one module.
 * Returns 0 = ok, 1 = compile diagnostics, 2 = runtime error
 * (details via host->error). */
uint32_t msy_context_run(msy_context *ctx, const char *src_utf8, size_t len);

/* Fire an event callback previously registered through dom_add_listener. */
uint32_t msy_context_invoke(msy_context *ctx, uint32_t cb);

#ifdef __cplusplus
}
#endif
#endif /* MERSEY_H */
