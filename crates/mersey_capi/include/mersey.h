/* Mersey embedding API (docs/architecture/embedding-api.md).
 *
 * This is the boundary the Chromium fork's //components/mersey wraps, and it
 * mirrors — function for function — the WASM boundary the browser loader
 * already drives (crates/mersey_wasm). Same loader payloads, same tagged-JSON
 * bridge, same return codes: an embedder that has read one has read both, and
 * both run the same loader implementation (mersey_interp::embed).
 *
 * Design rules (from the architecture doc):
 *  - the host owns all I/O and the event loop; the engine only computes;
 *  - strings cross the boundary as (pointer, length) UTF-8, never
 *    NUL-terminated by contract;
 *  - errors are reported through the host's `error` callback, never unwound
 *    across the ABI;
 *  - a context is confined to its creating thread. Calls may RE-ENTER: a
 *    bridge call the engine makes (web_call, web_new) may synchronously call
 *    msy_context_invoke* back on the same context — `new Promise(executor)`
 *    runs its executor before returning — and the engine supports that. What
 *    it does not support is two threads.
 *
 * Buffer ownership, both directions, one rule: a returned pointer is valid
 * until the NEXT call across the boundary in the same direction on the same
 * context/table. The receiver copies immediately.
 *
 * The universal web bridge crosses as tagged JSON (webjson wire format):
 *   primitives   -> JSON scalars
 *   host object  -> {"__ref__": handle}   (handle 0 = the global object)
 *   Mersey fn    -> {"__cb__": id}        (invoke via msy_context_invoke_args)
 *   reply        -> {"ok": value} | {"err": "message"}
 */
#ifndef MERSEY_H
#define MERSEY_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

/* Bumped whenever the table layout or a contract below changes. Check it
 * before installing a table; a mismatch means "do not use this engine". */
#define MSY_ABI_VERSION 2u
uint32_t msy_abi_version(void);

typedef struct msy_context msy_context;

/* Context-creation flags. */
#define MSY_FLAG_NO_JIT 0x1u /* Tier 0 only: never map executable pages. */

/* Host function table. Any pointer may be NULL — that capability is then
 * denied or a no-op, never an error. `data` is passed back verbatim. */
typedef struct {
    void *data;

    /* ---- console + diagnostics ---------------------------------------- */
    void (*print)(void *data, const char *utf8, size_t len);
    /* level is "log"/"warn"/"error"/"debug"; when NULL, print is used. */
    void (*print_level)(void *data, const char *level, size_t level_len,
                        const char *utf8, size_t len);
    void (*error)(void *data, const char *utf8, size_t len);

    /* ---- capabilities (spec §5.4) -------------------------------------- */
    /* JSON array of capability names this context is granted, e.g.
     * "[\"web\",\"random\",\"time\"]". Read once at context creation.
     * NULL grants nothing: deny-by-default is the point. */
    const char *(*caps)(void *data, size_t *out_len);

    /* ---- the universal web bridge -------------------------------------- */
    /* Resolve an ambient global to a handle; return -1 for "no such global"
     * (importing it then fails, which is how feature detection works). */
    int64_t (*web_global)(void *data, const char *name, size_t len);
    /* Each returns a tagged-JSON reply ({"ok":…}|{"err":…}) valid until the
     * next host call; *out_len receives its length. NULL reply = "{}". */
    const char *(*web_get)(void *data, int64_t target, const char *prop,
                           size_t prop_len, size_t *out_len);
    const char *(*web_set)(void *data, int64_t target, const char *prop,
                           size_t prop_len, const char *value_json,
                           size_t value_len, size_t *out_len);
    const char *(*web_call)(void *data, int64_t target, const char *method,
                            size_t method_len, const char *args_json,
                            size_t args_len, size_t *out_len);
    const char *(*web_new)(void *data, const char *ctor, size_t ctor_len,
                           const char *args_json, size_t args_len,
                           size_t *out_len);
    /* Snapshot an iterable host object: {"ok":[…]}. */
    const char *(*web_iterate)(void *data, int64_t target, size_t *out_len);
    /* `x instanceof Ctor` on the host side; returns 0 or 1. */
    int32_t (*web_instanceof)(void *data, int64_t target, int64_t ctor);
    /* The engine no longer references this handle (its proxy died). */
    void (*web_release)(void *data, int64_t target);
    /* Byte buffers cross packed, not as JSON (pixels, audio, files).
     * web_bytes_read: host returns the bytes and sets *out_len (same
     * lifetime rule); NULL = target is not a byte source. web_bytes_write:
     * host copies and returns a handle to the buffer it created. */
    const uint8_t *(*web_bytes_read)(void *data, int64_t target,
                                     size_t *out_len);
    int64_t (*web_bytes_write)(void *data, const uint8_t *bytes, size_t len);

    /* ---- time + entropy (capability-gated on the engine side) ---------- */
    /* epoch != 0: ms since the Unix epoch; else a monotonic ms reading. */
    double (*time_ms)(void *data, int32_t epoch);
    /* Fill buf with n cryptographically random bytes; 0 = success. */
    int32_t (*random_bytes)(void *data, uint8_t *buf, size_t n);

    /* ---- legacy fake-DOM hooks (native demos and tests; a real browser
     * embedder uses the web bridge above and leaves these NULL) ---------- */
    void (*dom_set_text)(void *data, const char *id, size_t id_len,
                         const char *text, size_t text_len);
    const char *(*dom_get_text)(void *data, const char *id, size_t id_len,
                                size_t *out_len);
    void (*dom_add_listener)(void *data, const char *id, size_t id_len,
                             const char *event, size_t event_len, uint32_t cb);
} msy_host_table;

/* Create a context backed by `host` (the table is copied). Check
 * msy_abi_version() first; installing a mismatched table is undefined. */
msy_context *msy_context_new(const msy_host_table *host);
msy_context *msy_context_new_ex(const msy_host_table *host, uint32_t flags);
void msy_context_free(msy_context *ctx);

/* ---- loading and running ------------------------------------------------ */

/* Scan one module's imports without running it. The HOST fetches (CORS, CSP,
 * SRI are its jurisdiction); this tells it what to fetch next. Returns JSON
 * {"static":[…],"dynamic":[…]}, valid until the next msy_* call on `ctx`. */
const char *msy_context_scan_imports(msy_context *ctx, const char *src_utf8,
                                     size_t len, size_t *out_len);

/* Run a whole module graph. The payload is the loader contract (identical to
 * the WASM loader's):
 *   {"entry":"a.mersey",
 *    "modules":[{"spec":"b.mersey","source":"…"}, …],   dependency-first
 *    "lazy":["c.mersey"]}                               dynamic-import targets
 * Returns 0 = ran, 1 = did not start (diagnostics via error), 2 = threw. */
uint32_t msy_context_run_graph(msy_context *ctx, const char *payload_json,
                               size_t len);

/* Compile and execute one self-contained module (no relative imports). */
uint32_t msy_context_run(msy_context *ctx, const char *src_utf8, size_t len);

/* ---- callbacks: how the host's event loop drives the engine ------------- */

/* Fire callback `cb` (a {"__cb__":id} the engine handed out) with a JSON
 * array of arguments — an event object, a promise's settled value. May be
 * called re-entrantly from inside a web_* host hook. */
uint32_t msy_context_invoke_args(msy_context *ctx, uint32_t cb,
                                 const char *args_json, size_t len);
/* As above, with no arguments. */
uint32_t msy_context_invoke(msy_context *ctx, uint32_t cb);
/* The host is done with a callback (a listener was removed, a promise
 * settled): release its slot so the table doesn't grow for a page lifetime. */
void msy_context_release_callback(msy_context *ctx, uint32_t cb);

#ifdef __cplusplus
}
#endif
#endif /* MERSEY_H */
