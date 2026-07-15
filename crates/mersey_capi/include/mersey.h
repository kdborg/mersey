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
#define MSY_ABI_VERSION 6u
uint32_t msy_abi_version(void);

typedef struct msy_context msy_context;

/* A call/constructor argument that crosses without JSON: a number (is_num != 0)
 * or a UTF-8 string (is_num == 0; str/str_len valid). */
typedef struct {
    int32_t is_num;
    double num;
    const char *str;
    size_t str_len;
} msy_scalar;

/* Wide-string fast path (ABI v5). The engine's strings are UTF-16, and a UTF-16
 * host (Gecko's nsString, V8's String) is too — so strings cross with no
 * encoding conversion at all: arguments are UTF-16 (borrowed straight from the
 * engine buffer), replies carry UTF-16 the engine copies into a new string.
 * No UTF-8 intermediary, no JSON. */

/* An argument. `kind` selects the payload:
 *   0 str16 (UTF-16 code units; str16_len in units)
 *   1 number (num)
 *   2 host-object handle (num) — the bridge resolves it back to the object
 *   3 bool (num is 0/1)      4 null
 * Kinds 0/1 keep the old is_num semantics; 2–4 are ABI v6. */
typedef struct {
    int32_t kind;
    double num;
    const uint16_t *str16;
    size_t str16_len;
} msy_arg16;

/* A typed reply, filled by the host. `str16` (when present) is UTF-16, valid
 * until the next call across the boundary on this context. */
typedef struct {
    int32_t tag;           /* 0 null  1 num  2 str16  3 ref  4 bool  5 err(str16)
                            * 7 json(str16, tagged JSON — caller parses it) */
    double num;            /* tag 1: number; tag 3: handle; tag 4: 0/1 */
    const uint16_t *str16; /* tag 2 / 5 / 7: UTF-16 text */
    size_t str16_len;      /* in UTF-16 code units */
} msy_reply;

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

    /* ---- interned bridge fast paths (ABI v3) --------------------------- */
    /* A member/method name crosses the ABI once; afterwards only its id
     * does, and scalar values skip JSON entirely. A host that leaves these
     * NULL (or returns UINT32_MAX from web_intern) is transparently served
     * by the reflective web_get/web_set/web_call above — same replies.
     *
     * web_intern: map a name to a stable id, or UINT32_MAX to decline. */
    uint32_t (*web_intern)(void *data, const char *name, size_t len);
    /* Read target[name_id]. */
    const char *(*web_get_id)(void *data, int64_t target, uint32_t name_id,
                              size_t *out_len);
    /* target[name_id] = string / number, no JSON. */
    const char *(*web_set_str)(void *data, int64_t target, uint32_t name_id,
                               const char *v, size_t v_len, size_t *out_len);
    const char *(*web_set_num)(void *data, int64_t target, uint32_t name_id,
                               double v, size_t *out_len);
    /* target[name_id](arg) with a single string argument. */
    const char *(*web_call_str)(void *data, int64_t target, uint32_t name_id,
                                const char *arg, size_t arg_len,
                                size_t *out_len);
    /* target[name_id](args…) and new Ctor(args…) with all-scalar arguments,
     * no JSON. An empty reply (out_len 0, non-NULL) means "not implemented,
     * use the reflective web_call/web_new instead". */
    const char *(*web_call_scalars)(void *data, int64_t target, uint32_t name_id,
                                    const msy_scalar *args, size_t argc,
                                    size_t *out_len);
    const char *(*web_new_scalars)(void *data, uint32_t ctor_id,
                                   const msy_scalar *args, size_t argc,
                                   size_t *out_len);

    /* ---- wide-string fast paths (ABI v5) ------------------------------- */
    /* target[name_id], target[name_id] = value, target[name_id](args…), with
     * UTF-32 arguments and a typed UTF-16 reply. NULL declines (the engine uses
     * the UTF-8 ops above; identical results). */
    void (*web_get_u16)(void *data, int64_t target, uint32_t name_id,
                        msy_reply *out);
    void (*web_set_u16)(void *data, int64_t target, uint32_t name_id,
                        const msy_arg16 *value, msy_reply *out);
    void (*web_call_u16)(void *data, int64_t target, uint32_t name_id,
                         const msy_arg16 *args, size_t argc, msy_reply *out);
    /* new Ctor(args…) with UTF-32 args and a typed reply (the handle of the new
     * object comes back as tag 3). */
    void (*web_new_u16)(void *data, uint32_t ctor_id, const msy_arg16 *args,
                        size_t argc, msy_reply *out);
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
